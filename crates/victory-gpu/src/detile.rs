//! Texture detiling on the card (`VICTORY_GPU_DETILE=1`; off by default).
//!
//! Without it every title texture a list samples has a staging buffer of its
//! own in upload memory (system memory the size of the texture, kept while
//! the texture is in use), and the submitting thread detiles title memory
//! into it on the CPU (`crate::texture`). With it an upload is: title memory
//! read once (and hashed, as the refresh hashes it), copied as it is into a
//! slice of a ring of upload memory, and detiled by a compute shader into a
//! slice of a ring in the card's memory laid out as the texture's copyable
//! footprints; the list's `CopyTextureRegion`s read that slice. Nothing is
//! kept per texture but the staging state's bookkeeping, so the system
//! memory of the staging buffers becomes two rings of fixed size.
//!
//! The shader copies by table (`victory_formats::texture::Layout::copy_table`):
//! the runs the CPU's detile copies, the column runs once and the rows once,
//! cached by layout, mip and pitch in a buffer of upload memory. An array
//! slice of a thin layout is slice 0's table moved by whole slices
//! (`shift`, a root constant), so one table serves every slice.
//!
//! Safety of the card: both rings are fixed in size
//! (`VICTORY_GPU_DETILE_RAW_MB`, 256; `VICTORY_GPU_DETILE_OUT_MB`, 192, made
//! only while the card's budget has room for it); a slice is reserved when
//! the list records the upload and given back only when the list has let it
//! go and the card has run every submission that used it. Whatever does not
//! fit (a ring full, the table buffer full, a table not in whole 32-bit
//! words, a table reaching outside its slices) goes the CPU's way at once:
//! nothing waits, nothing is overrun. Every table is checked against its
//! slices on the CPU, and the shader checks every word again.
//!
//! `VICTORY_GPU_DETILE_VERIFY=<n>`: up to `n` uploads a second are read back
//! and compared with the CPU's detile of the same bytes; a difference is
//! logged with the texture.
//!
//! The switch is read once (the rings are made with the first texture), so
//! it cannot alternate inside a run.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};

use victory_formats::texture::{CopyTable, Dimension, Layout};
use windows::Win32::Graphics::Direct3D12::*;
use windows::core::Interface;

/// Whether textures are detiled on the card (`VICTORY_GPU_DETILE=1`).
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_GPU_DETILE").is_some_and(|v| v == "1"))
}

/// Uploads a second read back and compared (`VICTORY_GPU_DETILE_VERIFY`;
/// 0, unset, none).
fn verify_per_second() -> u64 {
    static N: OnceLock<u64> = OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("VICTORY_GPU_DETILE_VERIFY")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    })
}

/// Megabytes from `var` (`default` if unset or not a number), clamped to
/// `[least, most]`, in bytes.
fn mb(var: &str, default: u64, least: u64, most: u64) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
        .clamp(least, most)
        << 20
}

/// Why an upload went the CPU's way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Why {
    /// The rings or the shader could not be made.
    NotReady,
    /// A table not in whole 32-bit words (narrow levels of 1- and 2-byte
    /// formats).
    Unaligned,
    /// The layout has no table (`copy_table` refused it).
    NoTable,
    /// The buffer of tables is full.
    Tables,
    /// A table reaches outside the slices it would be given (never
    /// expected).
    Bounds,
    /// The upload is larger than a ring, or than 32-bit offsets reach.
    TooBig,
    /// The ring of upload memory has no room now.
    RawFull,
    /// The ring in the card's memory has no room now.
    OutFull,
}

/// Since the start: uploads detiled on the card and their bytes of title
/// memory; uploads that went the CPU's way, by [`Why`] (8 places); refills
/// left out because the slices were still in use (a list submitted again);
/// uploads verified and those that differed; dispatches.
pub(crate) static STATS: [AtomicU64; 14] = [const { AtomicU64::new(0) }; 14];
const DONE: usize = 0;
const DONE_BYTES: usize = 1;
const WHY_AT: usize = 2;
const REUSED: usize = 10;
const CHECKED: usize = 11;
const DIFFERED: usize = 12;
const DISPATCHES: usize = 13;

/// Counts an upload that went the CPU's way for `why`.
pub(crate) fn refused(why: Why) {
    STATS[WHY_AT + why as usize].fetch_add(1, Relaxed);
}

/// Granularity of the rings' slices: a texture copy's placement wants 512
/// bytes, a root view 4; whole 64 KiB keeps the free list short.
pub(crate) const GRAIN: u64 = 64 << 10;

/// First-fit allocation of `size` bytes in [`GRAIN`]s, without D3D12. A
/// slice given back with a fence value is free once the card has passed it
/// ([`Arena::reclaim`]); slices go back in any order.
#[derive(Debug)]
pub(crate) struct Arena {
    size: u64,
    /// Free ranges (start, length), never touching: joined on release.
    free: BTreeMap<u64, u64>,
    /// Given back, waiting for the card: start, length, fence value.
    waiting: Vec<(u64, u64, u64)>,
    used: u64,
}

impl Arena {
    /// An arena of `size` bytes rounded down to whole grains.
    pub(crate) fn new(size: u64) -> Arena {
        let size = size / GRAIN * GRAIN;
        let mut free = BTreeMap::new();
        if size > 0 {
            free.insert(0, size);
        }
        Arena {
            size,
            free,
            waiting: Vec::new(),
            used: 0,
        }
    }

    /// Its bytes.
    pub(crate) fn size(&self) -> u64 {
        self.size
    }

    /// Bytes reserved or waiting for the card.
    pub(crate) fn used(&self) -> u64 {
        self.used
    }

    /// The start of `len` bytes (rounded up to [`GRAIN`]), or `None` if no
    /// free range holds them now. Never waits.
    pub(crate) fn alloc(&mut self, len: u64) -> Option<u64> {
        let len = len.max(1).checked_next_multiple_of(GRAIN)?;
        let (&at, &have) = self.free.iter().find(|&(_, &have)| have >= len)?;
        self.free.remove(&at);
        if have > len {
            self.free.insert(at + len, have - len);
        }
        self.used += len;
        Some(at)
    }

    /// Gives `[at, at + len)` back once the card has passed `fence` (0:
    /// now).
    pub(crate) fn release(&mut self, at: u64, len: u64, fence: u64) {
        let len = len.max(1).next_multiple_of(GRAIN);
        if fence == 0 {
            self.insert_free(at, len);
        } else {
            self.waiting.push((at, len, fence));
        }
    }

    /// Frees what waits for fence values up to `completed`.
    pub(crate) fn reclaim(&mut self, completed: u64) {
        let mut i = 0;
        while i < self.waiting.len() {
            let (at, len, fence) = self.waiting[i];
            if fence <= completed {
                self.waiting.swap_remove(i);
                self.insert_free(at, len);
            } else {
                i += 1;
            }
        }
    }

    fn insert_free(&mut self, mut at: u64, mut len: u64) {
        debug_assert!(at + len <= self.size);
        self.used -= len;
        if let Some((&before, &blen)) = self.free.range(..at).next_back()
            && before + blen == at
        {
            self.free.remove(&before);
            at = before;
            len += blen;
        }
        if let Some(&alen) = self.free.get(&(at + len)) {
            self.free.remove(&(at + len));
            len += alen;
        }
        self.free.insert(at, len);
    }
}

/// The most bytes one run of a packed table copies: one thread's work.
const RUN_MOST: u64 = 64;

/// A table packed for the shader: `words` are the runs (tiled, linear,
/// bytes; three 32-bit words each) and then the rows (tiled, linear; two
/// each); `tiled_end` and `linear_end` bound every byte it reads and writes
/// (slice 0, no shift).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Packed {
    pub(crate) words: Vec<u32>,
    pub(crate) runs: u32,
    pub(crate) rows: u32,
    pub(crate) tiled_end: u64,
    pub(crate) linear_end: u64,
}

/// `table` packed for the shader, long runs cut into pieces of
/// [`RUN_MOST`] bytes so that the work spreads over threads; `Err` if it is
/// not in whole 32-bit words or does not fit 32-bit offsets.
pub(crate) fn pack(table: &CopyTable) -> Result<Packed, Why> {
    if !table.aligned(4) {
        return Err(Why::Unaligned);
    }
    let mut runs: Vec<(u64, u64, u64)> = Vec::with_capacity(table.runs.len());
    for r in &table.runs {
        let (tiled, linear, len) = (r.tiled, u64::from(r.linear), u64::from(r.len));
        let mut k = 0;
        while k < len {
            runs.push((tiled + k, linear + k, RUN_MOST.min(len - k)));
            k += RUN_MOST;
        }
    }
    let tiled_end = table.rows.iter().map(|r| r.tiled).max().unwrap_or(0)
        + runs.iter().map(|&(t, _, n)| t + n).max().unwrap_or(0);
    let small = |v: u64| u32::try_from(v).map_err(|_| Why::TooBig);
    small(tiled_end)?;
    small(table.linear_bytes)?;
    let mut words = Vec::with_capacity(3 * runs.len() + 2 * table.rows.len());
    for &(t, l, n) in &runs {
        words.extend([small(t)?, small(l)?, small(n)?]);
    }
    for r in &table.rows {
        words.extend([small(r.tiled)?, small(r.linear)?]);
    }
    Ok(Packed {
        words,
        runs: runs.len() as u32,
        rows: table.rows.len() as u32,
        tiled_end,
        linear_end: table.linear_bytes,
    })
}

/// The slice whose table serves array slice `slice` of `layout`, and the
/// bytes the table is moved by for it: slice 0 and a whole slice per slice
/// for a thin layout (its rows then differ by that alone), the slice itself
/// otherwise.
pub(crate) fn table_slice(layout: &Layout, slice: u32) -> (u32, u64) {
    if layout.desc.dimension != Dimension::Tex3d && !layout.thick {
        (0, u64::from(slice) * layout.slice_size)
    } else {
        (slice, 0)
    }
}

/// One job's work: copy by a table, or (`run_count` 0) zeros.
///
/// Root constants: `runs_at` and `rows_at` are word offsets into the table
/// buffer; a group's threads take runs (x) of one row (y, from
/// `row_first`); `shift` moves every tiled offset (a slice of a thin
/// layout); `out_at` is the subresource's offset in the output slice;
/// `raw_len` and `out_len` bound the slices, which every word is checked
/// against. Zeros: `rows_at` words from `out_at` on.
pub(crate) const DETILE: &str = r#"
#define RS "RootConstants(num32BitConstants=8, b0), SRV(t0), SRV(t1), UAV(u0)"
cbuffer Job : register(b0) {
    uint runs_at;
    uint run_count;
    uint rows_at;
    uint row_first;
    uint shift;
    uint out_at;
    uint raw_len;
    uint out_len;
};
ByteAddressBuffer raw : register(t0);
ByteAddressBuffer table : register(t1);
RWByteAddressBuffer outb : register(u0);

[RootSignature(RS)]
[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    if (run_count == 0) {
        uint at = out_at + id.x * 4;
        if (id.x < rows_at && at + 4 <= out_len) {
            outb.Store(at, 0);
        }
        return;
    }
    if (id.x >= run_count) {
        return;
    }
    uint3 run = table.Load3((runs_at + id.x * 3) * 4);
    uint2 row = table.Load2((rows_at + (row_first + id.y) * 2) * 4);
    uint src = row.x + shift + run.x;
    uint dst = out_at + row.y + run.y;
    for (uint k = 0; k < run.z; k += 4) {
        if (src + k + 4 <= raw_len && dst + k + 4 <= out_len) {
            outb.Store(dst + k, raw.Load(src + k));
        }
    }
}
"#;

/// Root parameters of [`DETILE`]'s root signature (its `RS`), in order.
const ROOT_CONSTANTS: u32 = 0;
const ROOT_RAW: u32 = 1;
const ROOT_TABLES: u32 = 2;
const ROOT_OUT: u32 = 3;

/// Most groups along one axis of a dispatch.
const GROUPS_MOST: u32 = 65_535;

/// One dispatch of [`DETILE`] run on the CPU, thread by thread with the
/// shader's own arithmetic (32-bit, wrapping) and bounds checks: the
/// reference the shader is tested against.
#[cfg(test)]
pub(crate) fn emulate(words: &[u32], c: [u32; 8], groups: (u32, u32), raw: &[u8], out: &mut [u8]) {
    let [
        runs_at,
        run_count,
        rows_at,
        row_first,
        shift,
        out_at,
        raw_len,
        out_len,
    ] = c;
    let raw_len = (raw_len as usize).min(raw.len());
    let out_len = (out_len as usize).min(out.len());
    let load = |byte: u32| words[(byte / 4) as usize];
    for y in 0..groups.1 {
        for x in 0..groups.0 * 64 {
            if run_count == 0 {
                let at = out_at.wrapping_add(x * 4) as usize;
                if x < rows_at && at + 4 <= out_len {
                    out[at..at + 4].fill(0);
                }
                continue;
            }
            if x >= run_count {
                continue;
            }
            let run = (runs_at + x * 3) * 4;
            let (t, l, n) = (load(run), load(run + 4), load(run + 8));
            let row = (rows_at + (row_first + y) * 2) * 4;
            let (rt, rl) = (load(row), load(row + 4));
            let src = rt.wrapping_add(shift).wrapping_add(t);
            let dst = out_at.wrapping_add(rl).wrapping_add(l);
            let mut k = 0;
            while k < n {
                let (s, d) = (src.wrapping_add(k) as usize, dst.wrapping_add(k) as usize);
                if s + 4 <= raw_len && d + 4 <= out_len {
                    out[d..d + 4].copy_from_slice(&raw[s..s + 4]);
                }
                k += 4;
            }
        }
    }
}

/// A table in the table buffer: where its words start, and its bounds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Table {
    at: u32,
    runs: u32,
    rows: u32,
    tiled_end: u64,
    linear_end: u64,
}

/// One subresource of an upload: its mip, where it lies in the output
/// slice and how many bytes, and its table with the shift that moves the
/// table to its slice.
#[derive(Debug, Clone)]
pub(crate) struct Part {
    pub(crate) mip: u32,
    offset: u64,
    bytes: u64,
    table: Table,
    shift: u32,
}

/// The slices of one recorded upload, held by the list that recorded it
/// until the list is reset. Given back to the rings when dropped, once the
/// card has run every submission that used it.
pub(crate) struct Upload {
    raw: (u64, u64),
    out: (u64, u64),
    parts: Vec<Part>,
    /// The fence value of the last submission that used it (0: none yet).
    sent: AtomicU64,
}

impl Upload {
    /// Where the output slice starts in [`out_buffer`], in bytes.
    pub(crate) fn out_offset(&self) -> u64 {
        self.out.0
    }

    /// Bytes of title memory the upload copies (its raw slice).
    pub(crate) fn raw_len(&self) -> u64 {
        self.raw.1
    }

    /// Whether a submission that used it may still be running: its slices
    /// may not be written now.
    pub(crate) fn in_flight(&self) -> bool {
        let sent = self.sent.load(Relaxed);
        sent != 0 && crate::host::host().is_none_or(|h| h.completed() < sent)
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        let sent = *self.sent.get_mut();
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(state) = state.as_mut() {
            state.raw_arena.release(self.raw.0, self.raw.1, sent);
            state.out_arena.release(self.out.0, self.out.1, sent);
        }
    }
}

/// One dispatch: its root constants, the GPU addresses of its raw and
/// output slices, and its groups (x, y).
struct Job {
    constants: [u32; 8],
    raw: u64,
    out: u64,
    groups: (u32, u32),
}

/// An upload to read back and compare ([`verify_per_second`]): `len` bytes
/// of the output ring from `at`, the CPU's detile of the same title bytes,
/// the ranges to compare, the texture named, its readback buffer and the
/// fence value of its submission (`u64::MAX`: not sent yet).
struct Check {
    at: u64,
    len: u64,
    expected: Vec<u8>,
    ranges: Vec<(u64, u64)>,
    what: String,
    back: Option<(ID3D12Resource, usize)>,
    fence: u64,
}

struct State {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
    /// The raw ring: resource, mapping, GPU address.
    raw: (ID3D12Resource, usize, u64),
    raw_arena: Arena,
    /// The card's ring: resource, GPU address.
    out: (ID3D12Resource, u64),
    out_arena: Arena,
    /// The table buffer: resource, mapping, GPU address, bytes, bytes used.
    tables: (ID3D12Resource, usize, u64, u64, u64),
    cache: HashMap<Key, Result<Table, Why>>,
    jobs: Vec<Job>,
    /// Uploads this submission fills or uses again: told its fence value
    /// when it is sent.
    filled: Vec<Arc<Upload>>,
    /// Lists for the work before a submission: allocator, list, fence value
    /// of its last run (`u64::MAX`: recorded, not sent yet).
    lists: Vec<(ID3D12CommandAllocator, ID3D12GraphicsCommandList, u64)>,
    checks: Vec<Check>,
    /// The second a check was last taken in, and how many in it.
    checked: (u64, u64),
}

// SAFETY: D3D12 objects (free-threaded) and persistent mappings, used only
// under `STATE`'s lock.
unsafe impl Send for State {}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// A table's identity: the layout, mip and slice it is made for, and the
/// pitches.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    layout: (u8, u32, u32, u32, u32, u32, u32, u32),
    mip: u32,
    slice: u32,
    row_pitch: u64,
    depth_pitch: u64,
}

fn key(layout: &Layout, mip: u32, slice: u32, row_pitch: u64, depth_pitch: u64) -> Key {
    let d = &layout.desc;
    let dimension = match d.dimension {
        Dimension::Tex1d => 1,
        Dimension::Tex2d => 2,
        Dimension::Tex3d => 3,
    };
    Key {
        layout: (
            dimension,
            d.width,
            d.height,
            d.depth,
            d.mips,
            d.element_bytes,
            d.block,
            d.swizzle.0,
        ),
        mip,
        slice,
        row_pitch,
        depth_pitch,
    }
}

/// Whether the switch is on and the rings and the shader are there (made on
/// the first ask, once; a failure is logged and leaves every texture on the
/// CPU's way).
pub(crate) fn ready() -> bool {
    static READY: OnceLock<bool> = OnceLock::new();
    *READY.get_or_init(|| {
        if !on() {
            return false;
        }
        match make() {
            Ok(state) => {
                tracing::info!(
                    raw_mb = state.raw_arena.size() >> 20,
                    out_mb = state.out_arena.size() >> 20,
                    table_mb = state.tables.3 >> 20,
                    verify = verify_per_second(),
                    "VICTORY_GPU_DETILE=1: textures are detiled on the card"
                );
                *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(state);
                true
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "VICTORY_GPU_DETILE=1: no detile on the card (textures are detiled on the CPU)"
                );
                false
            }
        }
    })
}

/// The detile pipeline from `code` (compiled [`DETILE`]; the root signature
/// is inside it).
pub(crate) fn pipeline(
    device: &ID3D12Device,
    code: &[u8],
) -> windows::core::Result<(ID3D12RootSignature, ID3D12PipelineState)> {
    // SAFETY: the blob's bytes, which outlive the calls.
    unsafe {
        let root: ID3D12RootSignature = device.CreateRootSignature(0, code)?;
        let pso: ID3D12PipelineState =
            device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                CS: D3D12_SHADER_BYTECODE {
                    pShaderBytecode: code.as_ptr().cast(),
                    BytecodeLength: code.len(),
                },
                ..Default::default()
            })?;
        Ok((root, pso))
    }
}

fn make() -> windows::core::Result<State> {
    let empty = windows::core::Error::empty;
    let host = crate::host::host().ok_or_else(empty)?;
    let code = crate::host::compile(
        DETILE,
        windows::core::s!("main"),
        windows::core::s!("cs_5_1"),
    )?;
    let (root, pso) = pipeline(&host.device, &code)?;
    let raw_size = mb("VICTORY_GPU_DETILE_RAW_MB", 256, 32, 2048);
    let (raw, raw_data, out, out_size) =
        make_rings(raw_size, mb("VICTORY_GPU_DETILE_OUT_MB", 192, 32, 1024))?;
    let table_size = mb("VICTORY_GPU_DETILE_TABLE_MB", 32, 4, 256);
    let (tables, table_data) = host.upload_buffer(table_size).ok_or_else(empty)?;
    crate::host::name(&tables, || "detile: tables".into());
    // SAFETY: live buffers.
    let (raw_gpu, out_gpu, table_gpu) = unsafe {
        (
            raw.GetGPUVirtualAddress(),
            out.GetGPUVirtualAddress(),
            tables.GetGPUVirtualAddress(),
        )
    };
    Ok(State {
        root,
        pso,
        raw: (raw, raw_data, raw_gpu),
        raw_arena: Arena::new(raw_size),
        out: (out, out_gpu),
        out_arena: Arena::new(out_size),
        tables: (tables, table_data as usize, table_gpu, table_size, 0),
        cache: HashMap::new(),
        jobs: Vec::new(),
        filled: Vec::new(),
        lists: Vec::new(),
        checks: Vec::new(),
        checked: (0, 0),
    })
}

/// The rings: the raw one of `raw_size` bytes and its mapping, and the
/// card's one of up to `out_size` bytes with its size. The card's ring is
/// made only with room under 85 % of the card's budget after it, halved
/// down to 32 MB to find room; without any, no detile on the card.
fn make_rings(
    raw_size: u64,
    mut out_size: u64,
) -> windows::core::Result<(ID3D12Resource, usize, ID3D12Resource, u64)> {
    let empty = windows::core::Error::empty;
    let host = crate::host::host().ok_or_else(empty)?;
    let (raw, raw_data) = host.upload_buffer(raw_size).ok_or_else(empty)?;
    crate::host::name(&raw, || "detile: title bytes".into());
    let out = loop {
        let room = host
            .video_memory()
            .is_none_or(|[budget, used, ..]| used + out_size <= budget / 100 * 85);
        if room && let Some(buffer) = host.uav_buffer(out_size) {
            break buffer;
        }
        out_size /= 2;
        if out_size < 32 << 20 {
            tracing::warn!(
                "VICTORY_GPU_DETILE=1: no room in the card's budget for the detile ring"
            );
            return Err(empty());
        }
    };
    crate::host::name(&out, || "detile: detiled textures".into());
    Ok((raw, raw_data as usize, out, out_size))
}

/// The ring the detiled textures are in: the source of the lists' copies.
/// `None` before [`ready`].
pub(crate) fn out_buffer() -> Option<ID3D12Resource> {
    STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|s| s.out.0.clone())
}

/// One subresource of an upload as the staging state lays it out: its mip
/// and array slice, its footprint's offset and row pitch in bytes, its rows
/// and depth.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Sub {
    pub(crate) mip: u32,
    pub(crate) slice: u32,
    pub(crate) offset: u64,
    pub(crate) row_pitch: u64,
    pub(crate) rows: u32,
    pub(crate) depth: u32,
}

impl State {
    /// The table for a subresource and its shift: from the cache, or made,
    /// checked and put in the table buffer.
    fn table(&mut self, layout: &Layout, sub: &Sub) -> Result<(Table, u64), Why> {
        let (slice, shift) = table_slice(layout, sub.slice);
        let depth_pitch = sub.row_pitch * u64::from(sub.rows);
        let key = key(layout, sub.mip, slice, sub.row_pitch, depth_pitch);
        if let Some(found) = self.cache.get(&key) {
            return found.map(|t| (t, shift));
        }
        let made = layout
            .copy_table(sub.mip, slice, sub.row_pitch as usize, depth_pitch as usize)
            .map_err(|_| Why::NoTable)
            .and_then(|t| pack(&t));
        let packed = match made {
            Ok(packed) => packed,
            Err(why) => {
                // What the layout is does not change: asked once.
                self.cache.insert(key, Err(why));
                return Err(why);
            }
        };
        let bytes = 4 * packed.words.len() as u64;
        let at = self.tables.4.next_multiple_of(16);
        if at + bytes > self.tables.3 || at / 4 > u64::from(u32::MAX) {
            // Not cached: the buffer only fills, so it refuses again, but a
            // refusal for room is not a property of the layout.
            return Err(Why::Tables);
        }
        // SAFETY: inside the table buffer's mapping (checked above); the
        // card reads only tables written before the lists that use them
        // were sent, and a table once written is never written again.
        unsafe {
            std::ptr::copy_nonoverlapping(
                packed.words.as_ptr().cast::<u8>(),
                (self.tables.1 as *mut u8).add(at as usize),
                bytes as usize,
            );
        }
        self.tables.4 = at + bytes;
        let table = Table {
            at: (at / 4) as u32,
            runs: packed.runs,
            rows: packed.rows,
            tiled_end: packed.tiled_end,
            linear_end: packed.linear_end,
        };
        self.cache.insert(key, Ok(table));
        Ok((table, shift))
    }
}

/// The slices and tables for an upload of `raw_len` bytes of title memory
/// into `out_len` bytes laid out as `subs`, or why it goes the CPU's way.
/// Never waits.
pub(crate) fn reserve(
    layout: &Layout,
    subs: &[Sub],
    raw_len: u64,
    out_len: u64,
) -> Result<Arc<Upload>, Why> {
    if !ready() {
        return Err(Why::NotReady);
    }
    let completed = crate::host::host().map_or(0, |h| h.completed());
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let state = guard.as_mut().ok_or(Why::NotReady)?;
    if raw_len > state.raw_arena.size()
        || out_len > state.out_arena.size()
        || raw_len > u64::from(u32::MAX)
        || out_len > u64::from(u32::MAX)
    {
        return Err(Why::TooBig);
    }
    let mut parts = Vec::with_capacity(subs.len());
    for sub in subs {
        let (table, shift) = state.table(layout, sub)?;
        parts.push(part(sub, table, shift, raw_len, out_len)?);
    }
    state.raw_arena.reclaim(completed);
    state.out_arena.reclaim(completed);
    let taken = take_slices(&mut state.raw_arena, &mut state.out_arena, raw_len, out_len);
    let (raw, out) = taken?;
    drop(guard);
    Ok(Arc::new(Upload {
        raw: (raw, raw_len),
        out: (out, out_len),
        parts,
        sent: AtomicU64::new(0),
    }))
}

/// The part of an upload for `sub`, its table moved by `shift`, checked
/// against slices of `raw_len` and `out_len` bytes: every byte the table
/// reads lies in the raw slice, every byte it writes in the subresource's
/// part of the output slice. That part is whole rows of the pitch, but the
/// last subresource's last row ends at its bytes (the footprints' total
/// says no more).
fn part(sub: &Sub, table: Table, shift: u64, raw_len: u64, out_len: u64) -> Result<Part, Why> {
    let whole = sub.row_pitch * u64::from(sub.rows) * u64::from(sub.depth);
    let bytes = whole.min(out_len.saturating_sub(sub.offset));
    if shift + table.tiled_end > raw_len || table.linear_end > bytes {
        return Err(Why::Bounds);
    }
    Ok(Part {
        mip: sub.mip,
        offset: sub.offset,
        bytes,
        table,
        shift: u32::try_from(shift).map_err(|_| Why::TooBig)?,
    })
}

/// [`part`] for a packed table at word 0 of the table buffer: the bytes of
/// the part, or why it is refused.
#[cfg(test)]
pub(crate) fn check_part(
    sub: &Sub,
    packed: &Packed,
    shift: u64,
    raw_len: u64,
    out_len: u64,
) -> Result<u64, Why> {
    let table = Table {
        at: 0,
        runs: packed.runs,
        rows: packed.rows,
        tiled_end: packed.tiled_end,
        linear_end: packed.linear_end,
    };
    part(sub, table, shift, raw_len, out_len).map(|p| p.bytes)
}

/// Both slices of an upload (raw start, output start), or neither: a ring
/// without room now refuses at once, and what the other ring gave is given
/// back.
pub(crate) fn take_slices(
    raw_arena: &mut Arena,
    out_arena: &mut Arena,
    raw_len: u64,
    out_len: u64,
) -> Result<(u64, u64), Why> {
    let raw = raw_arena.alloc(raw_len).ok_or(Why::RawFull)?;
    match out_arena.alloc(out_len) {
        Some(out) => Ok((raw, out)),
        None => {
            raw_arena.release(raw, raw_len, 0);
            Err(Why::OutFull)
        }
    }
}

/// Whether the upload being filled now is to be read back and compared
/// ([`verify_per_second`]).
pub(crate) fn check_due() -> bool {
    let most = verify_per_second();
    if most == 0 {
        return false;
    }
    let now = crate::host::now_ms() / 1000;
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(state) = guard.as_mut() else {
        return false;
    };
    if state.checked.0 != now {
        state.checked = (now, 0);
    }
    if state.checked.1 >= most {
        return false;
    }
    state.checked.1 += 1;
    true
}

/// What a check compares: the CPU's detile of the same bytes into a buffer
/// of the output slice's size, the ranges of it (offset, bytes) that are
/// the texture's (rows without their padding), and the texture named.
pub(crate) struct Expected {
    pub(crate) bytes: Vec<u8>,
    pub(crate) ranges: Vec<(u64, u64)>,
    pub(crate) what: String,
}

/// The dispatches of an upload's `parts` (root constants, groups) into
/// slices of `raw_len` and `out_len` bytes: zeros for the parts of mips not
/// in `held` (a bit a mip; `u32::MAX`: all), the copy by table for the
/// others, split where a dispatch would pass [`GROUPS_MOST`] groups.
fn jobs_for(parts: &[Part], held: u32, raw_len: u32, out_len: u32) -> Vec<([u32; 8], (u32, u32))> {
    let mut jobs = Vec::new();
    for part in parts {
        if part.mip < 32 && held & (1 << part.mip) == 0 {
            let words = (part.bytes / 4) as u32;
            let mut done = 0;
            while done < words {
                let now = (words - done).min(GROUPS_MOST * 64);
                let at = (part.offset + 4 * u64::from(done)) as u32;
                jobs.push((
                    [0, 0, now, 0, 0, at, raw_len, out_len],
                    (now.div_ceil(64), 1),
                ));
                done += now;
            }
            continue;
        }
        let t = part.table;
        let mut first = 0;
        while first < t.rows {
            let rows = (t.rows - first).min(GROUPS_MOST);
            jobs.push((
                [
                    t.at,
                    t.runs,
                    t.at + 3 * t.runs,
                    first,
                    part.shift,
                    part.offset as u32,
                    raw_len,
                    out_len,
                ],
                (t.runs.div_ceil(64), rows),
            ));
            first += rows;
        }
    }
    jobs
}

/// At submission: `raw` (title memory as read now, at most
/// [`Upload::raw_len`] bytes) goes into the upload's raw slice and its jobs
/// into this submission's work; the subresources of mips not in `held` (a
/// bit a mip; `u32::MAX`: all) are zeros. The upload must not be in use by
/// the card ([`Upload::in_flight`]).
pub(crate) fn fill(upload: &Arc<Upload>, raw: &[u8], held: u32, expected: Option<Expected>) {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(state) = guard.as_mut() else {
        return;
    };
    let n = raw.len().min(upload.raw.1 as usize);
    // SAFETY: inside the raw ring's mapping (the slice was reserved for
    // `raw.1` bytes, and the rings live for the run); no
    // submission that reads the slice is running (the caller checked).
    unsafe {
        std::ptr::copy_nonoverlapping(
            raw.as_ptr(),
            (state.raw.1 as *mut u8).add(upload.raw.0 as usize),
            n,
        );
    }
    let raw_gpu = state.raw.2 + upload.raw.0;
    let out_gpu = state.out.1 + upload.out.0;
    // Both fit 32 bits: `reserve` refused larger slices.
    let (raw_len, out_len) = (n as u32, upload.out.1 as u32);
    for (constants, groups) in jobs_for(&upload.parts, held, raw_len, out_len) {
        state.jobs.push(Job {
            constants,
            raw: raw_gpu,
            out: out_gpu,
            groups,
        });
    }
    if let Some(e) = expected {
        state.checks.push(Check {
            at: upload.out.0,
            len: upload.out.1,
            expected: e.bytes,
            ranges: e.ranges,
            what: e.what,
            back: None,
            fence: u64::MAX,
        });
    }
    state.filled.push(upload.clone());
    STATS[DONE].fetch_add(1, Relaxed);
    STATS[DONE_BYTES].fetch_add(n as u64, Relaxed);
}

/// At submission: a list that used `upload` is submitted again while its
/// last submission may still read the slices. They are not written (the
/// detile there is the one of that submission); the upload is noted for
/// this submission's fence value.
pub(crate) fn reuse(upload: &Arc<Upload>) {
    STATS[REUSED].fetch_add(1, Relaxed);
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = guard.as_mut() {
        state.filled.push(upload.clone());
    }
}

/// At submission, after the lists' steps: the list that detiles what they
/// filled, to go ahead of them in the same call; `None` if they filled
/// nothing.
pub(crate) fn pre_list() -> Option<ID3D12CommandList> {
    let host = crate::host::host()?;
    let completed = host.completed();
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let state = guard.as_mut()?;
    land_checks(state, completed);
    if state.jobs.is_empty() {
        return None;
    }
    let jobs = std::mem::take(&mut state.jobs);
    let at = match state.lists.iter().position(|l| l.2 <= completed) {
        Some(at) => at,
        None => {
            // SAFETY: plain object creation.
            let made: windows::core::Result<(ID3D12CommandAllocator, ID3D12GraphicsCommandList)> = unsafe {
                (|| {
                    let allocator: ID3D12CommandAllocator = host
                        .device
                        .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
                    let list: ID3D12GraphicsCommandList = host.device.CreateCommandList(
                        0,
                        D3D12_COMMAND_LIST_TYPE_DIRECT,
                        &allocator,
                        None,
                    )?;
                    list.Close()?;
                    Ok((allocator, list))
                })()
            };
            match made {
                Ok((allocator, list)) => {
                    state.lists.push((allocator, list, 0));
                    state.lists.len() - 1
                }
                Err(e) => crate::host::fail(format_args!(
                    "no list for the detile on the card ({e}, VICTORY_GPU_DETILE=1)"
                )),
            }
        }
    };
    // Readbacks for the checks taken in this submission.
    for check in state
        .checks
        .iter_mut()
        .filter(|c| c.fence == u64::MAX && c.back.is_none())
    {
        check.back = host.shared_buffer(check.len).map(|(b, d)| (b, d as usize));
    }
    let (allocator, list, _) = &state.lists[at];
    let out = &state.out.0;
    let (common, uav, source) = (
        D3D12_RESOURCE_STATE_COMMON,
        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        D3D12_RESOURCE_STATE_COPY_SOURCE,
    );
    // SAFETY: the allocator's last list has run (its fence value is
    // passed); live buffers (the rings and the table buffer live for the
    // run); every job's offsets were checked
    // against its slices when the upload was reserved.
    let recorded = unsafe {
        (|| -> windows::core::Result<()> {
            allocator.Reset()?;
            list.Reset(allocator, None)?;
            // The ring rests in the common state between submissions: the
            // lists' copies after this one read it from there (a buffer is
            // promoted to a copy source implicitly).
            list.ResourceBarrier(&[crate::host::transition(out, common, uav)]);
            list.SetComputeRootSignature(&state.root);
            list.SetPipelineState(&state.pso);
            list.SetComputeRootShaderResourceView(ROOT_TABLES, state.tables.2);
            for job in &jobs {
                list.SetComputeRoot32BitConstants(
                    ROOT_CONSTANTS,
                    8,
                    job.constants.as_ptr().cast(),
                    0,
                );
                list.SetComputeRootShaderResourceView(ROOT_RAW, job.raw);
                list.SetComputeRootUnorderedAccessView(ROOT_OUT, job.out);
                list.Dispatch(job.groups.0, job.groups.1, 1);
            }
            list.ResourceBarrier(&[crate::host::transition(out, uav, source)]);
            for check in state.checks.iter().filter(|c| c.fence == u64::MAX) {
                if let Some((back, _)) = &check.back {
                    list.CopyBufferRegion(back, 0, out, check.at, check.len);
                }
            }
            list.ResourceBarrier(&[crate::host::transition(out, source, common)]);
            list.Close()
        })()
    };
    if let Err(e) = recorded {
        crate::host::fail(format_args!(
            "the detile on the card could not be recorded ({e}, VICTORY_GPU_DETILE=1)"
        ));
    }
    STATS[DISPATCHES].fetch_add(jobs.len() as u64, Relaxed);
    state.lists[at].2 = u64::MAX;
    let list = state.lists[at].1.cast().ok();
    drop(guard);
    status();
    list
}

/// After a submission is sent with fence value `fence`: what it filled or
/// used again, its list and its checks are noted with it.
pub(crate) fn sent(fence: u64) {
    let filled = {
        let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
        let Some(state) = guard.as_mut() else {
            return;
        };
        for l in state.lists.iter_mut().filter(|l| l.2 == u64::MAX) {
            l.2 = fence;
        }
        for c in state.checks.iter_mut().filter(|c| c.fence == u64::MAX) {
            c.fence = fence;
        }
        std::mem::take(&mut state.filled)
    };
    for upload in &filled {
        upload.sent.fetch_max(fence, Relaxed);
    }
    // Dropped here, outside the lock: the last holder gives the slices back
    // under it.
    drop(filled);
}

/// The first byte in `ranges` (offset, bytes) where `got` and `expected`
/// differ.
fn first_difference(got: &[u8], expected: &[u8], ranges: &[(u64, u64)]) -> Option<usize> {
    ranges.iter().find_map(|&(at, len)| {
        let (a, b) = (at as usize, (at + len) as usize);
        let (got, expected) = (got.get(a..b)?, expected.get(a..b)?);
        got.iter()
            .zip(expected)
            .position(|(x, y)| x != y)
            .map(|i| a + i)
    })
}

/// The checks whose submission the card has run: compared, and let go.
fn land_checks(state: &mut State, completed: u64) {
    let mut i = 0;
    while i < state.checks.len() {
        let check = &state.checks[i];
        if check.fence == u64::MAX || check.fence > completed {
            i += 1;
            continue;
        }
        let check = state.checks.swap_remove(i);
        let Some((_, data)) = &check.back else {
            continue;
        };
        // SAFETY: the readback buffer's mapping, `len` bytes, written by a
        // list that has run.
        let got = unsafe { std::slice::from_raw_parts(*data as *const u8, check.len as usize) };
        STATS[CHECKED].fetch_add(1, Relaxed);
        if let Some(at) = first_difference(got, &check.expected, &check.ranges) {
            let n = STATS[DIFFERED].fetch_add(1, Relaxed);
            if n < 32 {
                tracing::error!(
                    texture = %check.what,
                    first_difference = at,
                    card = got[at],
                    cpu = check.expected[at],
                    "VICTORY_GPU_DETILE_VERIFY: the card's detile differs from the CPU's"
                );
            }
        }
    }
}

/// Every ten seconds, with the switch on: what went to the card and what
/// did not.
fn status() {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = crate::host::now_ms();
    let last = LAST.load(Relaxed);
    if now < last + 10_000 || LAST.compare_exchange(last, now, Relaxed, Relaxed).is_err() {
        return;
    }
    let s = |i: usize| STATS[i].load(Relaxed);
    let (raw, out, raw_size, out_size) = STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map_or((0, 0, 0, 0), |st| {
            (
                st.raw_arena.used() >> 20,
                st.out_arena.used() >> 20,
                st.raw_arena.size() >> 20,
                st.out_arena.size() >> 20,
            )
        });
    tracing::info!(
        "gpu detile: {} uploads ({} MB of title memory), {} dispatches; rings in use {raw} of {raw_size} MB raw, \
         {out} of {out_size} MB card; to the CPU: not ready {}, unaligned {}, no table {}, \
         tables full {}, bounds {}, too big {}, raw ring full {}, card ring full {}; reused in use {}; \
         verify: {} checked, {} differed",
        s(DONE),
        s(DONE_BYTES) >> 20,
        s(DISPATCHES),
        s(WHY_AT),
        s(WHY_AT + 1),
        s(WHY_AT + 2),
        s(WHY_AT + 3),
        s(WHY_AT + 4),
        s(WHY_AT + 5),
        s(WHY_AT + 6),
        s(WHY_AT + 7),
        s(REUSED),
        s(CHECKED),
        s(DIFFERED),
    );
}

#[cfg(test)]
pub(crate) mod tests;

//! Pipelines the title makes from precompiled console programs: device
//! methods 0x218 (`DeserializeComputePipelineStateX`) and 0x1c8
//! (`DeserializeGraphicsPipelineStateX`), both `(this, description,
//! interface id, out)`.
//!
//! Layout, from the mandy project's `docs/fspx.md` and checked against the
//! descriptions a run keeps with `SPARK_PIPELINES_X=1` (offsets in bytes,
//! each block a `{u64 address, u64 size}` pair):
//!
//! ```text
//! 0x218  +0x00 root signature, +0x08 dispatch state (28 bytes),
//!        +0x18 a 20-byte block not read, +0x28 the GFX10 program,
//!        +0x38 its records
//! 0x1c8  +0x00 root signature, +0x08 context register state,
//!        +0x18 the vertex-to-pixel semantic table, +0x28/+0x38 pixel
//!        program and records, +0x48/+0x58 vertex program and records,
//!        +0x68/+0x78 mesh program and records, zero up to +0x108
//! state  7 dwords: dword 3 numthreads x | y << 16, dword 4 z in bits
//!        0-15 and bit 20 set for wave32, dword 5 RSRC1, dword 6 RSRC2
//! records  a header (compute: 0x403 and 4 dwords more; a graphics stage:
//!        2 and 2 more) whose second dword counts the bytes from the flags
//!        dword (low byte 0x40) to the end; 32-byte records tagged 0x41
//!        (the resources read, as the FSSM used-resource entries) and 0x42;
//!        then 0x43 and the 16-byte digest that the source container's
//!        `HASH` part holds after its flags
//! program  up to three 12-byte VOP3 `v_nop` words carrying that digest as
//!        literals (no-ops on the hardware), the code, `s_code_end` padding
//! ```
//!
//! A compute pipeline is made from the container the digest names, not
//! from the program at +0x28: our translator takes a program with its FSSM
//! root layout (which user SGPRs hold which root parameter), and the
//! records list only what the program reads. A container the title has
//! passed to a standard method is found by the digest of its `HASH` part,
//! and translated and cached as any other.
//! A container not passed yet, or one whose compute program has another
//! thread-group size than the dispatch state, leaves the call refused as
//! before. Graphics pipelines are not made here: their vertex programs
//! fetch their own vertices from a table the command list would have to
//! build, and their formats come from the bound targets; 0x1c8 only counts
//! how many of its stages a container is known for.
//!
//! Everything but the counting is behind `VICTORY_PIPELINES_X=1` (off
//! unless set). The counting answers whether the title, refused at 0x218,
//! passes the same program's container through 0x1f0 afterwards.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use super::{Shader, hand_out};
use crate::object::{HRESULT, Iid, This};

/// Bytes of a 0x218 description.
const COMPUTE_DESC_BYTES: usize = 0x48;
/// Bytes of a 0x1c8 description.
const GRAPHICS_DESC_BYTES: usize = 0x108;
/// The dispatch state's size; another size is a layout we do not know.
const DISPATCH_STATE_BYTES: u64 = 28;
/// The largest program a description may name.
const MAX_PROGRAM_BYTES: u64 = 16 << 20;
/// The largest records block a description may name.
const MAX_RECORDS_BYTES: u64 = 64 << 10;
/// Title addresses sit between the first 64 KiB and the top of user space.
const TITLE_ADDRESSES: std::ops::Range<u64> = 0x1_0000..0x8000_0000_0000;
/// Record tags.
const RESOURCE_RECORD: u32 = 0x41;
const DECLARATION_RECORD: u32 = 0x42;
const DIGEST_RECORD: u32 = 0x43;
const RECORD_BYTES: usize = 32;
/// The first dword of a compute program's records and of a graphics
/// stage's, and the dwords before their flags dword.
const COMPUTE_RECORDS: u32 = 0x403;
const COMPUTE_HEADER_DWORDS: usize = 5;
const STAGE_RECORDS: u32 = 2;
const STAGE_HEADER_DWORDS: usize = 3;
/// `s_code_end` and `s_nop 0`, the padding after a program's code.
const PADDING: [u32; 2] = [0xbf9f_0000, 0xbf80_0000];

/// Why a description gives no pipeline, as logged.
pub(crate) type Refusal = &'static str;

/// Whether 0x218 makes pipelines and 0x1c8 looks its stages up
/// (`VICTORY_PIPELINES_X=1`; off unless set).
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_PIPELINES_X").is_some_and(|v| v == "1"))
}

/// Whether containers are filed by digest as they are seen.
fn indexing() -> bool {
    on() || cfg!(test)
}

/// A `{address, size}` pair of a description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Block {
    address: u64,
    bytes: u64,
}

/// The pairs of a description from +0x08, `count` of them.
fn blocks(desc: &[u8], count: usize) -> Vec<Block> {
    (0..count)
        .filter_map(|i| {
            let at = 8 + 16 * i;
            let word = |at: usize| {
                let w: [u8; 8] = desc.get(at..at + 8)?.try_into().ok()?;
                Some(u64::from_le_bytes(w))
            };
            Some(Block {
                address: word(at)?,
                bytes: word(at + 8)?,
            })
        })
        .collect()
}

/// A block that may be read: both halves zero (absent), or a title address
/// with a size up to `largest`.
fn check(block: Block, largest: u64) -> Result<Option<Block>, Refusal> {
    match (block.address, block.bytes) {
        (0, 0) => Ok(None),
        (0, _) | (_, 0) => Err("a block with an address and no size, or a size and no address"),
        (_, n) if n > largest => Err("a block larger than any program or records seen"),
        (a, n) if !TITLE_ADDRESSES.contains(&a) || a.checked_add(n).is_none() => {
            Err("a block outside title memory")
        }
        _ => Ok(Some(block)),
    }
}

/// What a compute program's dispatch state says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DispatchState {
    /// `numthreads`.
    pub(crate) threads: [u32; 3],
    /// Runs as 32 lanes per wave (else 64).
    pub(crate) wave32: bool,
    /// `COMPUTE_PGM_RSRC1` and `COMPUTE_PGM_RSRC2`.
    pub(crate) rsrc1: u32,
    pub(crate) rsrc2: u32,
}

fn dword(bytes: &[u8], index: usize) -> Option<u32> {
    let at = 4 * index;
    bytes
        .get(at..at + 4)
        .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
}

/// The dispatch state of 28 bytes; refused when another size or a zero
/// thread-group dimension.
pub(crate) fn dispatch_state(bytes: &[u8]) -> Result<DispatchState, Refusal> {
    if bytes.len() as u64 != DISPATCH_STATE_BYTES {
        return Err("a dispatch state of another size than 28 bytes");
    }
    let d = |i| dword(bytes, i).unwrap_or_default();
    let threads = [d(3) & 0xffff, d(3) >> 16, d(4) & 0xffff];
    if threads.contains(&0) {
        return Err("a dispatch state with a zero thread-group dimension");
    }
    Ok(DispatchState {
        threads,
        wave32: d(4) >> 20 & 1 != 0,
        rsrc1: d(5),
        rsrc2: d(6),
    })
}

/// The source container's digest from a program's records: the header,
/// the 0x41 and 0x42 records, then the 16 bytes after the 0x43 tag.
/// Refused on another header, a size dword that disagrees, an unknown tag
/// or no digest.
pub(crate) fn digest(records: &[u8]) -> Result<[u8; 16], Refusal> {
    let header = match dword(records, 0) {
        Some(COMPUTE_RECORDS) => COMPUTE_HEADER_DWORDS,
        Some(STAGE_RECORDS) => STAGE_HEADER_DWORDS,
        _ => return Err("records with an unknown header"),
    };
    let flags_at = 4 * header;
    if dword(records, 1).map(|n| n as usize + flags_at) != Some(records.len()) {
        return Err("records whose size dword disagrees with their size");
    }
    if dword(records, header).map(|f| f & 0xff) != Some(0x40) {
        return Err("records with unknown flags");
    }
    let mut at = flags_at + 4;
    loop {
        match dword(records, at / 4) {
            Some(RESOURCE_RECORD | DECLARATION_RECORD) => at += RECORD_BYTES,
            Some(DIGEST_RECORD) => {
                return records
                    .get(at + 4..at + 20)
                    .and_then(|d| d.try_into().ok())
                    .ok_or("records cut short in their digest");
            }
            Some(_) => return Err("records with an unknown tag"),
            None => return Err("records without a digest"),
        }
    }
}

/// The bytes of the no-op words before a program's code: 0, 24 or 36. A
/// word counts when it is a VOP3 `v_nop` (opcode 0x180) with a literal
/// source, the form that carries the digest.
pub(crate) fn prologue_bytes(code: &[u8]) -> usize {
    let mut words = 0;
    while words < 3 {
        let at = 3 * words;
        let (Some(first), Some(second), Some(_)) =
            (dword(code, at), dword(code, at + 1), dword(code, at + 2))
        else {
            break;
        };
        if first >> 16 != 0xd580 || second & 0xfffe_0100 != 0x03fe_0100 {
            break;
        }
        words += 1;
    }
    12 * words
}

/// Whether two programs have the same code once the no-op words before it
/// and the padding after it are left out.
pub(crate) fn same_program(a: &[u8], b: &[u8]) -> bool {
    fn code(program: &[u8]) -> &[u8] {
        let mut code = &program[prologue_bytes(program)..];
        while let Some((rest, last)) = code.split_last_chunk::<4>() {
            if !PADDING.contains(&u32::from_le_bytes(*last)) {
                break;
            }
            code = rest;
        }
        code
    }
    code(a) == code(b)
}

/// The digest a container's `HASH` part holds after its 4-byte flags.
pub(crate) fn container_digest(container: &[u8]) -> Option<[u8; 16]> {
    use victory_shader::{Container, FourCC};
    let c = Container::parse(container).ok()?;
    c.part(FourCC::HASH)?.data.get(4..20)?.try_into().ok()
}

/// Containers by digest, while [`indexing`].
static BY_DIGEST: Mutex<Option<HashMap<[u8; 16], Arc<Shader>>>> = Mutex::new(None);

/// Files a container first seen by its digest. The first container of a
/// digest stays: containers with the same `HASH` share their `DXIL` part.
pub(crate) fn file(shader: &Arc<Shader>) {
    if !indexing() {
        return;
    }
    if let Some(digest) = shader.digest {
        let mut index = BY_DIGEST.lock().unwrap_or_else(|e| e.into_inner());
        index
            .get_or_insert_with(HashMap::new)
            .entry(digest)
            .or_insert_with(|| shader.clone());
    }
}

fn by_digest(digest: &[u8; 16]) -> Option<Arc<Shader>> {
    let index = BY_DIGEST.lock().unwrap_or_else(|e| e.into_inner());
    index.as_ref()?.get(digest).cloned()
}

/// Digests of compute programs refused at 0x218 whose container no
/// standard method has passed since, at most this many.
const MAX_REFUSED: usize = 1 << 16;
static REFUSED: Mutex<Option<HashSet<[u8; 16]>>> = Mutex::new(None);
static ANY_REFUSED: AtomicBool = AtomicBool::new(false);
/// 0x218 calls answered with a pipeline and refused; programs refused
/// (a program refused again after its container came counts again); and
/// of those, the ones whose container a standard method passed afterwards.
static MADE: AtomicU64 = AtomicU64::new(0);
static REFUSED_CALLS: AtomicU64 = AtomicU64::new(0);
static REFUSED_PROGRAMS: AtomicU64 = AtomicU64::new(0);
static CAME_BACK: AtomicU64 = AtomicU64::new(0);

fn worth_a_line(n: u64) -> bool {
    n <= 4 || n.is_power_of_two()
}

fn hex(digest: &[u8; 16]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn note_refused(digest: [u8; 16]) {
    REFUSED_CALLS.fetch_add(1, Ordering::Relaxed);
    let mut refused = REFUSED.lock().unwrap_or_else(|e| e.into_inner());
    let refused = refused.get_or_insert_with(HashSet::new);
    if refused.len() < MAX_REFUSED && refused.insert(digest) {
        REFUSED_PROGRAMS.fetch_add(1, Ordering::Relaxed);
        ANY_REFUSED.store(true, Ordering::Relaxed);
    }
}

/// A standard compute creation (`method`: the device method, as logged):
/// counted and logged, once per refusal, when 0x218 refused a program of
/// the same container before. That tells whether the title passes such a
/// program again through a standard method after the refusal.
pub(crate) fn note_standard(shader: &Shader, method: &'static str) {
    if !ANY_REFUSED.load(Ordering::Relaxed) {
        return;
    }
    let Some(digest) = shader.digest else { return };
    let named = REFUSED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .is_some_and(|r| r.remove(&digest));
    if !named {
        return;
    }
    let came_back = CAME_BACK.fetch_add(1, Ordering::Relaxed) + 1;
    if worth_a_line(came_back) {
        tracing::info!(
            target: "pipelines_x",
            method,
            came_back,
            refused_programs = REFUSED_PROGRAMS.load(Ordering::Relaxed),
            refused_calls = REFUSED_CALLS.load(Ordering::Relaxed),
            digest = %hex(&digest),
            hash = %shader.hash,
            "a compute program refused at 0x218 came again through a standard method, with its container"
        );
    }
}

/// A 0x218 description and the blocks it names, read.
pub(crate) struct ComputeX {
    pub(crate) state: DispatchState,
    pub(crate) program: Vec<u8>,
    pub(crate) digest: [u8; 16],
}

/// Reads `bytes` at `address` in title memory through the kernel: none of
/// a description's addresses is known to be mapped.
fn read(address: u64, bytes: u64) -> Result<Vec<u8>, Refusal> {
    let mut out = vec![0u8; bytes as usize];
    if crate::host::read_into(address as *const u8, &mut out) {
        Ok(out)
    } else {
        Err("a block that does not read")
    }
}

/// Reads the 0x218 description at `desc` and the blocks it names.
fn read_compute(desc: *const u8) -> Result<ComputeX, Refusal> {
    if desc.is_null() {
        return Err("no description");
    }
    let desc = read(desc as u64, COMPUTE_DESC_BYTES as u64)?;
    let [state, _, program, records] = blocks(&desc, 4)[..] else {
        return Err("a description cut short");
    };
    let need = |b: Option<Block>, what| b.ok_or(what);
    let state = need(check(state, DISPATCH_STATE_BYTES)?, "no dispatch state")?;
    let program = need(check(program, MAX_PROGRAM_BYTES)?, "no program")?;
    let records = need(check(records, MAX_RECORDS_BYTES)?, "no records")?;
    let state = dispatch_state(&read(state.address, state.bytes)?)?;
    let digest = digest(&read(records.address, records.bytes)?)?;
    Ok(ComputeX {
        state,
        program: read(program.address, program.bytes)?,
        digest,
    })
}

/// Device method 0x218: the compute pipeline of the container the
/// program's digest names, with `VICTORY_PIPELINES_X=1`. `Ok`: what the
/// title gets (the pipeline handed out through `out`); `Err`: why the
/// call is refused, the caller answering `E_NOTIMPL`. A refused program's
/// digest is kept for [`note_standard`].
pub(crate) fn compute(
    desc: *const u8,
    iid: *const Iid,
    out: *mut This,
) -> Result<HRESULT, Refusal> {
    let x = read_compute(desc)?;
    let made = if on() {
        make_compute(&x, iid, out)
    } else {
        Err("not made: VICTORY_PIPELINES_X is off")
    };
    if made.is_err() {
        note_refused(x.digest);
    }
    made
}

/// The pipeline for a read 0x218 description; refused when no container
/// of its digest has been passed or its compute program does not fit.
fn make_compute(x: &ComputeX, iid: *const Iid, out: *mut This) -> Result<HRESULT, Refusal> {
    let shader = by_digest(&x.digest).ok_or("no container of this digest passed yet")?;
    let program = source_program(&shader.container)?;
    let victory_shader::native::StageIo::Cs(io) = &program.io else {
        return Err("the digest's container has no compute program");
    };
    if io.threads != x.state.threads {
        return Err("the digest's container has another thread-group size");
    }
    let made = MADE.fetch_add(1, Ordering::Relaxed) + 1;
    if worth_a_line(made) {
        tracing::info!(
            target: "pipelines_x",
            made,
            digest = %hex(&x.digest),
            hash = %shader.hash,
            threads = ?x.state.threads,
            wave32 = x.state.wave32,
            rsrc = format_args!("{:#x} {:#x}", x.state.rsrc1, x.state.rsrc2),
            container_wave64 = program.wave64,
            same_code = same_program(&x.program, &program.code),
            "a 0x218 compute pipeline made from the container its program names"
        );
    }
    Ok(hand_out(vec![shader], None, iid, out))
}

/// The compute program of a container.
fn source_program(container: &[u8]) -> Result<victory_shader::NativeProgram, Refusal> {
    use victory_shader::{Container, FourCC, HwStage};
    let c = Container::parse(container).map_err(|_| "the digest's container does not parse")?;
    let part = c
        .part(FourCC::DXIL)
        .ok_or("the digest's container has no DXIL part")?;
    victory_shader::native_programs(part.data)
        .map_err(|_| "the digest's container's programs do not parse")?
        .into_iter()
        .find(|p| p.stage == HwStage::Cs)
        .ok_or("the digest's container has no compute program")
}

/// 0x1c8 stages whose container is known and not, with
/// `VICTORY_PIPELINES_X=1`.
static STAGES_KNOWN: AtomicU64 = AtomicU64::new(0);
static STAGES_UNKNOWN: AtomicU64 = AtomicU64::new(0);

/// Device method 0x1c8, which stays refused: why, as logged. With
/// `VICTORY_PIPELINES_X=1` the description is read and its pixel, vertex
/// and mesh stages looked up by digest, and the counts logged now and then,
/// for knowing whether the containers are there to make the pipelines from.
pub(crate) fn graphics(desc: *const u8) -> Refusal {
    if !on() {
        return "not made: VICTORY_PIPELINES_X is off";
    }
    match graphics_stages(desc) {
        Ok(stages) => {
            let mut line = false;
            for (_, known) in &stages {
                let n = if *known {
                    &STAGES_KNOWN
                } else {
                    &STAGES_UNKNOWN
                };
                line |= worth_a_line(n.fetch_add(1, Ordering::Relaxed) + 1);
            }
            if line {
                tracing::info!(
                    target: "pipelines_x",
                    stages = ?stages,
                    known = STAGES_KNOWN.load(Ordering::Relaxed),
                    unknown = STAGES_UNKNOWN.load(Ordering::Relaxed),
                    "0x1c8 stages whose container a standard method has passed"
                );
            }
            "graphics pipelines from programs are not made yet"
        }
        Err(why) => why,
    }
}

/// The stages a 0x1c8 description names, each with whether a container of
/// its digest is known.
fn graphics_stages(desc: *const u8) -> Result<Vec<(&'static str, bool)>, Refusal> {
    if desc.is_null() {
        return Err("no description");
    }
    let desc = read(desc as u64, GRAPHICS_DESC_BYTES as u64)?;
    graphics_layout(&desc)?
        .into_iter()
        .map(|(stage, records)| {
            let digest = digest(&read(records.address, records.bytes)?)?;
            Ok((stage, by_digest(&digest).is_some()))
        })
        .collect()
}

/// The records block of each stage a 0x1c8 description names; refused
/// when a block is malformed, a program comes without records, or the
/// bytes after the mesh pair are not zero.
fn graphics_layout(desc: &[u8]) -> Result<Vec<(&'static str, Block)>, Refusal> {
    if desc.len() != GRAPHICS_DESC_BYTES {
        return Err("a description cut short");
    }
    let b = blocks(desc, 8);
    if desc[8 + 16 * 8..].iter().any(|&x| x != 0) {
        return Err("a graphics description with more than three stages");
    }
    check(b[0], MAX_RECORDS_BYTES)?.ok_or("no register state")?;
    let mut stages = Vec::new();
    for (stage, program, records) in [("pixel", 2, 3), ("vertex", 4, 5), ("mesh", 6, 7)] {
        match (
            check(b[program], MAX_PROGRAM_BYTES)?,
            check(b[records], MAX_RECORDS_BYTES)?,
        ) {
            (Some(_), Some(records)) => stages.push((stage, records)),
            (None, None) => {}
            _ => return Err("a stage program without its records"),
        }
    }
    if stages.iter().all(|(s, _)| *s == "pixel") {
        return Err("a graphics description without a vertex or mesh stage");
    }
    Ok(stages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::S_OK;

    fn dwords(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    const DIGEST: [u8; 16] = [
        0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd,
        0xef,
    ];

    /// Records as the layout above has them: `header` dwords before the
    /// flags, `resources` 0x41 and as many 0x42 records, then the digest
    /// and 12 bytes of zeros.
    fn records(first: u32, header: usize, resources: usize) -> Vec<u8> {
        let mut words = vec![first, 0];
        words.resize(header, 0);
        words.push(0x40 | 4 << 8 | (resources as u32) << 16);
        for tag in [RESOURCE_RECORD, DECLARATION_RECORD] {
            for i in 0..resources {
                words.extend([tag, 0x0303, 8 * i as u32, i as u32, 4, 0x408, u32::MAX, 0]);
            }
        }
        words.push(DIGEST_RECORD);
        let mut bytes = dwords(&words);
        bytes.extend_from_slice(&DIGEST);
        bytes.extend_from_slice(&[0; 12]);
        let size = (bytes.len() - 4 * header) as u32;
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        bytes
    }

    #[test]
    fn records_give_the_digest() {
        assert_eq!(digest(&records(COMPUTE_RECORDS, 5, 6)), Ok(DIGEST));
        assert_eq!(digest(&records(STAGE_RECORDS, 3, 0)), Ok(DIGEST));
        assert_eq!(digest(&records(STAGE_RECORDS, 3, 22)), Ok(DIGEST));
    }

    #[test]
    fn records_out_of_shape_are_refused() {
        let good = records(COMPUTE_RECORDS, 5, 2);
        // Another first dword, or the stage header on compute records.
        let mut other = good.clone();
        other[0] = 7;
        assert!(digest(&other).is_err());
        let mut short = records(STAGE_RECORDS, 5, 2);
        let size = (short.len() - 12) as u32;
        short[4..8].copy_from_slice(&size.to_le_bytes());
        assert!(digest(&short).is_err());
        // A size dword that disagrees.
        assert!(digest(&good[..good.len() - 4]).is_err());
        // An unknown tag where the first record starts.
        let mut tag = good.clone();
        tag[24..28].copy_from_slice(&0x44u32.to_le_bytes());
        assert!(digest(&tag).is_err());
        // No digest record: the records run out.
        let mut none = dwords(&[COMPUTE_RECORDS, 4, 0, 0, 0, 0x40]);
        none.truncate(24);
        assert!(digest(&none).is_err());
        assert!(digest(&[]).is_err());
    }

    fn state(x: u32, y: u32, z: u32, wave32: bool) -> Vec<u8> {
        let z = z | u32::from(wave32) << 20;
        dwords(&[
            0x4005_cf00,
            0x6c09_0102,
            0,
            x | y << 16,
            z,
            0x402c_0345,
            0xb8a,
        ])
    }

    #[test]
    fn dispatch_state_gives_the_group_and_wave() {
        assert_eq!(
            dispatch_state(&state(8, 8, 1, false)),
            Ok(DispatchState {
                threads: [8, 8, 1],
                wave32: false,
                rsrc1: 0x402c_0345,
                rsrc2: 0xb8a,
            })
        );
        let wide = dispatch_state(&state(64, 1, 2, true)).unwrap();
        assert_eq!((wide.threads, wide.wave32), ([64, 1, 2], true));
        assert!(dispatch_state(&state(0, 8, 1, false)).is_err());
        assert!(dispatch_state(&state(8, 8, 1, false)[..24]).is_err());
    }

    /// A no-op word carrying four digest bytes as its literal.
    fn nop(literal: u32) -> [u32; 3] {
        [0xd580_3210, 0x03ff_4d21, literal]
    }

    fn program(nops: usize, code: &[u32]) -> Vec<u8> {
        let mut words: Vec<u32> = (0..nops).flat_map(|i| nop(i as u32)).collect();
        words.extend_from_slice(code);
        words.extend([0xbf9f_0000; 3]);
        dwords(&words)
    }

    const CODE: [u32; 3] = [0xbfa0_0003, 0xbe88_0304, 0xbf81_0000];

    #[test]
    fn prologues_are_skipped() {
        assert_eq!(prologue_bytes(&program(3, &CODE)), 36);
        assert_eq!(prologue_bytes(&program(2, &CODE)), 24);
        assert_eq!(prologue_bytes(&program(0, &CODE)), 0);
        // A VOP3 v_nop without a literal source is code.
        assert_eq!(prologue_bytes(&dwords(&[0xd580_0000, 0x0000_0080, 0])), 0);
        assert_eq!(prologue_bytes(&[0xd5, 0x80]), 0);
    }

    #[test]
    fn programs_compare_without_prologue_and_padding() {
        let mut short = program(2, &CODE);
        short.truncate(short.len() - 8);
        assert!(same_program(&program(3, &CODE), &short));
        assert!(!same_program(
            &program(3, &CODE),
            &program(3, &[0xbfa0_0003, 0xbe88_0305, 0xbf81_0000])
        ));
    }

    /// A 0x1c8 description: register state, semantic table, and the
    /// stages present, each a program and its records.
    fn graphics_desc(pixel: bool, vertex: bool, mesh: bool) -> Vec<u8> {
        let mut words = vec![0x1000u64, 0x2_0000, 388, 0x3_0000, 0xbc];
        for (on, base) in [(pixel, 0x4_0000u64), (vertex, 0x5_0000), (mesh, 0x6_0000)] {
            words.extend(if on {
                [base, 0x300, base + 0x1000, 0xd0]
            } else {
                [0; 4]
            });
        }
        let mut bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        bytes.resize(GRAPHICS_DESC_BYTES, 0);
        bytes
    }

    #[test]
    fn graphics_descriptions_name_their_stages() {
        let stages = |d: &[u8]| {
            graphics_layout(d).map(|s| s.iter().map(|(n, b)| (*n, b.address)).collect::<Vec<_>>())
        };
        assert_eq!(
            stages(&graphics_desc(true, true, false)),
            Ok(vec![("pixel", 0x4_1000), ("vertex", 0x5_1000)])
        );
        assert_eq!(
            stages(&graphics_desc(false, true, false)),
            Ok(vec![("vertex", 0x5_1000)])
        );
        assert_eq!(
            stages(&graphics_desc(true, false, true)),
            Ok(vec![("pixel", 0x4_1000), ("mesh", 0x6_1000)])
        );
        assert!(stages(&graphics_desc(true, false, false)).is_err());
        // A program without records, a word past the mesh pair, a short one.
        let mut lone = graphics_desc(true, true, false);
        lone[0x38..0x40].fill(0);
        assert!(stages(&lone).is_err());
        let mut more = graphics_desc(true, true, false);
        more[0x90] = 1;
        assert!(stages(&more).is_err());
        assert!(stages(&graphics_desc(true, true, false)[..0x100]).is_err());
        // A block above user space.
        let mut high = graphics_desc(true, true, false);
        high[0x58..0x60].copy_from_slice(&(1u64 << 48).to_le_bytes());
        assert!(stages(&high).is_err());
    }

    #[test]
    fn blocks_are_checked() {
        let b = |address, bytes| Block { address, bytes };
        assert_eq!(check(b(0, 0), 64), Ok(None));
        assert_eq!(check(b(0x10_0000, 28), 64), Ok(Some(b(0x10_0000, 28))));
        assert!(check(b(0x10_0000, 0), 64).is_err());
        assert!(check(b(0, 28), 64).is_err());
        assert!(check(b(0x10_0000, 65), 64).is_err());
        assert!(check(b(0x100, 28), 64).is_err());
    }

    /// A container with a `HASH` part of `digest` and one compute program
    /// of `threads` whose code is `code`, laid out as `victory_shader::native`
    /// reads it.
    fn container(digest: &[u8; 16], threads: [u32; 3], code: &[u8]) -> Vec<u8> {
        // FSSM payload: slot count, eight record offsets, one compute record.
        let mut payload = 6u64.to_le_bytes().to_vec();
        payload.resize(0x68, 0);
        for at in [0x08, 0x10, 0x18, 0x20, 0x28, 0x30, 0x48] {
            payload[at..at + 8].copy_from_slice(&0xDEAD_BEEF_DEAD_BEEFu64.to_le_bytes());
        }
        let record = payload.len() as u64;
        payload[0x60..0x68].copy_from_slice(&record.to_le_bytes());
        let stage_data = 0x34;
        let code_at = record + 0x54 + stage_data;
        payload.extend_from_slice(&code_at.to_le_bytes());
        payload.extend_from_slice(&(code.len() as u64).to_le_bytes());
        payload.resize(record as usize + 0x50, 0);
        payload.extend_from_slice(&(16u32 | 20 << 20).to_le_bytes());
        let data_at = payload.len();
        payload.resize(data_at + stage_data as usize, 0);
        payload[data_at + 0x24..data_at + 0x28].copy_from_slice(&0x8cu32.to_le_bytes());
        for (i, t) in threads.iter().enumerate() {
            let at = data_at + 0x28 + 4 * i;
            payload[at..at + 4].copy_from_slice(&t.to_le_bytes());
        }
        payload.extend_from_slice(code);
        let mut fssm = b"FSSM".to_vec();
        fssm.extend_from_slice(&0x101u32.to_le_bytes());
        fssm.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        fssm.extend_from_slice(&payload);
        let mut buf = vec![0u8; zlib_rs::compress_bound(fssm.len())];
        let (stream, rc) = zlib_rs::compress_slice(&mut buf, &fssm, zlib_rs::DeflateConfig::new(6));
        assert_eq!(rc, zlib_rs::ReturnCode::Ok);
        let mut xbch = b"XBCH".to_vec();
        xbch.extend_from_slice(&3u32.to_le_bytes());
        xbch.extend_from_slice(&(stream.len() as u64).to_le_bytes());
        xbch.extend_from_slice(&(fssm.len() as u64).to_le_bytes());
        xbch.extend_from_slice(stream);
        let mut dxil = vec![0u8; 0x18];
        dxil.extend_from_slice(b"XDAT");
        dxil.extend(dwords(&[1, 0x24, 0x0d, 0x18 + 12 + 12, xbch.len() as u32]));
        dxil.extend_from_slice(&xbch);
        let mut hash = vec![0u8; 4];
        hash.extend_from_slice(digest);
        let parts: [(&[u8; 4], &[u8]); 2] = [(b"HASH", &hash), (b"DXIL", &dxil)];
        let table = 0x20 + 4 * parts.len();
        let total = table + parts.iter().map(|(_, d)| 8 + d.len()).sum::<usize>();
        let mut out = b"DXBC".to_vec();
        out.extend_from_slice(&[0; 16]);
        out.extend(dwords(&[1, total as u32, parts.len() as u32]));
        let mut offset = table;
        for (_, d) in parts {
            out.extend(dwords(&[offset as u32]));
            offset += 8 + d.len();
        }
        for (fourcc, d) in parts {
            out.extend_from_slice(fourcc);
            out.extend(dwords(&[d.len() as u32]));
            out.extend_from_slice(d);
        }
        out
    }

    fn compute_x(digest: [u8; 16], threads: [u32; 3]) -> ComputeX {
        ComputeX {
            state: dispatch_state(&state(threads[0], threads[1], threads[2], false)).unwrap(),
            program: program(3, &CODE),
            digest,
        }
    }

    #[test]
    fn containers_are_found_by_their_digest() {
        let c = container(&DIGEST, [8, 8, 1], &program(2, &CODE));
        assert_eq!(container_digest(&c), Some(DIGEST));
        assert_eq!(source_program(&c).unwrap().code, program(2, &CODE));
        let shader = super::super::shader("compute", &c);
        assert_eq!(shader.digest, Some(DIGEST));
        assert!(Arc::ptr_eq(&by_digest(&DIGEST).unwrap(), &shader));
    }

    #[test]
    fn a_compute_pipeline_comes_from_its_container() {
        let digest = [7u8; 16];
        let c = container(&digest, [64, 1, 1], &program(2, &CODE));
        let shader = super::super::shader("compute", &c);
        let mut out: This = std::ptr::null();
        let iid = &super::super::PIPELINE_IIDS[0];
        // Another thread-group size, or a digest never passed: refused.
        assert!(make_compute(&compute_x(digest, [8, 8, 1]), iid, &mut out).is_err());
        assert!(make_compute(&compute_x([9; 16], [64, 1, 1]), iid, &mut out).is_err());
        assert!(out.is_null());
        assert_eq!(
            make_compute(&compute_x(digest, [64, 1, 1]), iid, &mut out),
            Ok(S_OK)
        );
        assert!(!out.is_null());
        assert!(Arc::ptr_eq(
            &super::super::pipeline(out).shaders[0],
            &shader
        ));
    }

    #[test]
    fn descriptions_read_from_memory() {
        let digest = [5u8; 16];
        let state = state(32, 1, 1, true);
        let code = program(3, &CODE);
        let records = {
            let mut r = records(COMPUTE_RECORDS, 5, 1);
            let at = r.len() - 28;
            r[at..at + 16].copy_from_slice(&digest);
            r
        };
        let unknown = [0u8; 20];
        let pairs: [(u64, u64); 4] = [
            (state.as_ptr() as u64, state.len() as u64),
            (unknown.as_ptr() as u64, unknown.len() as u64),
            (code.as_ptr() as u64, code.len() as u64),
            (records.as_ptr() as u64, records.len() as u64),
        ];
        let mut desc = 0x1234u64.to_le_bytes().to_vec();
        for (a, n) in pairs {
            desc.extend_from_slice(&a.to_le_bytes());
            desc.extend_from_slice(&n.to_le_bytes());
        }
        let x = read_compute(desc.as_ptr()).unwrap();
        assert_eq!((x.state.threads, x.state.wave32), ([32, 1, 1], true));
        assert_eq!((x.digest, x.program.len()), (digest, code.len()));
        // Off by default: refused, and the digest kept for the standard
        // methods.
        let mut out: This = std::ptr::null();
        let iid = &super::super::PIPELINE_IIDS[0];
        assert!(compute(desc.as_ptr(), iid, &mut out).is_err());
        assert!(out.is_null());
        let back = CAME_BACK.load(Ordering::Relaxed);
        let c = container(&digest, [32, 1, 1], &code);
        let shader = super::super::shader("compute", &c);
        note_standard(&shader, "0x1f0");
        assert_eq!(CAME_BACK.load(Ordering::Relaxed), back + 1);
        // Once per refusal: later creations of the container are not retries.
        note_standard(&shader, "0x1f0");
        assert_eq!(CAME_BACK.load(Ordering::Relaxed), back + 1);
        // A size of 0 beside an address.
        desc[0x10..0x18].fill(0);
        assert!(read_compute(desc.as_ptr()).is_err());
        assert!(read_compute(std::ptr::null()).is_err());
    }

    #[test]
    fn graphics_stays_refused() {
        assert!(!graphics(std::ptr::null()).is_empty());
    }
}

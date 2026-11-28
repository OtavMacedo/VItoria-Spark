//! The fallback of the card's detile to the CPU's way, and the parts of
//! staging layouts, on the software adapter (skipped without it).

use super::*;
use crate::detile::tests::warp::Warp;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use victory_formats::texture::{Desc, Dimension, Layout, Swizzle};
use windows::Win32::Graphics::Dxgi::Common::*;

/// A mapped upload buffer of `bytes` on `device`, as a staging buffer is
/// (all zeros, not pooled).
fn upload(device: &ID3D12Device, bytes: u64) -> Option<Side> {
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_UPLOAD,
        ..Default::default()
    };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: bytes.max(1),
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        ..Default::default()
    };
    let mut buffer: Option<ID3D12Resource> = None;
    let mut data = std::ptr::null_mut();
    // SAFETY: plain creation and mapping.
    unsafe {
        device
            .CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_NONE,
                &desc,
                D3D12_RESOURCE_STATE_GENERIC_READ,
                None,
                &mut buffer,
            )
            .ok()?;
        buffer.as_ref()?.Map(0, None, Some(&mut data)).ok()?;
    }
    Some(Side {
        buffer: buffer?,
        data: data as usize,
        pooled: 0,
    })
}

/// A layout of `w` x `h` texels, `mips` levels and `depth` array slices
/// (3D: depth slices): BC7 (`bc`) or `bytes` a texel.
fn layout(w: u32, h: u32, mips: u32, depth: u32, bc: bool, bytes: u32, swizzle: Swizzle) -> Layout {
    Layout::new(&Desc {
        dimension: Dimension::Tex2d,
        width: w,
        height: h,
        depth,
        mips,
        element_bytes: if bc { 16 } else { bytes },
        block: if bc { 4 } else { 1 },
        samples: 1,
        swizzle,
    })
    .unwrap()
}

/// A staging state for the texture `layout` describes at `title`, laid out
/// by the device as the host texture is (every level of every slice); with
/// a staging buffer from `device`, or none (detiled on the card).
fn staging(device: &ID3D12Device, title: &[u8], layout: &Layout, cpu: bool) -> Staging {
    let desc = &layout.desc;
    let three = desc.dimension == Dimension::Tex3d;
    let format = match (desc.block, desc.element_bytes) {
        (4, _) => DXGI_FORMAT_BC7_UNORM,
        (_, 8) => DXGI_FORMAT_R16G16B16A16_UNORM,
        _ => DXGI_FORMAT_R8G8B8A8_UNORM,
    };
    let d = D3D12_RESOURCE_DESC {
        Dimension: if three {
            D3D12_RESOURCE_DIMENSION_TEXTURE3D
        } else {
            D3D12_RESOURCE_DIMENSION_TEXTURE2D
        },
        Width: u64::from(desc.width),
        Height: desc.height,
        DepthOrArraySize: desc.depth as u16,
        MipLevels: desc.mips as u16,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        ..Default::default()
    };
    let count = if three {
        desc.mips
    } else {
        desc.mips * desc.depth
    };
    let mut footprints = vec![D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default(); count as usize];
    let mut rows = vec![0u32; count as usize];
    let mut total = 0;
    // SAFETY: outputs for `count` subresources.
    unsafe {
        device.GetCopyableFootprints(
            &d,
            0,
            count,
            0,
            Some(footprints.as_mut_ptr()),
            Some(rows.as_mut_ptr()),
            None,
            Some(&mut total),
        )
    };
    let s = Staging {
        cpu: std::sync::OnceLock::new(),
        #[cfg(feature = "diagnostics")]
        size: total,
        footprints,
        rows,
        layout: layout.clone(),
        mips: desc.mips,
        bytes: total,
        memory: title.as_ptr(),
        last: Mutex::default(),
        dirty: AtomicBool::new(false),
        uploaded: AtomicBool::new(false),
        used: AtomicU64::new(0),
        on_demand: false,
        detiled: AtomicU32::new(0),
        judged: [const { AtomicU64::new(0) }; 2],
        waiting: AtomicU64::new(0),
    };
    if cpu {
        assert!(s.give(upload(device, total).expect("an upload buffer")));
    }
    s
}

/// The staging buffer's bytes.
fn held(s: &Staging) -> Vec<u8> {
    let data = s.cpu.get().expect("a staging buffer").data;
    // SAFETY: the mapping holds `bytes` bytes, written by the refresh on
    // this thread.
    unsafe { std::slice::from_raw_parts(data as *const u8, s.bytes as usize).to_vec() }
}

fn pattern(len: u64) -> Vec<u8> {
    (0..len)
        .map(|i| (i.wrapping_mul(0x9e37_79b9) >> 24) as u8)
        .collect()
}

/// A texture whose staging buffer was let go (as `unstage` and the idle
/// sweep let it go) comes back detiled on the card and finds the rings
/// full: the upload goes the CPU's way at once, with a staging buffer made
/// for it, and the refresh detiles into that buffer although the memory
/// did not change since the texture was last looked at (the state with no
/// buffer had already taken its hash).
#[test]
fn warp_a_full_ring_after_the_staging_buffer_went_detiles_on_the_cpu() {
    use crate::detile::{Arena, GRAIN, Why, take_slices};
    use std::sync::atomic::Ordering::Relaxed;
    let Some(w) = Warp::new() else {
        eprintln!("no software adapter: skipped");
        return;
    };
    let device = w.device().clone();
    let layout = layout(256, 256, 9, 1, true, 16, Swizzle::SW_64KB_S);
    let title = pattern(layout.size);

    // 1. Detiled on the CPU into a staging buffer of its own, which then goes.
    let first = Arc::new(staging(&device, &title, &layout, true));
    first.refresh();
    let want = held(&first);
    assert!(want.iter().any(|&b| b != 0));
    drop(first);

    // 2. The same texture, staged anew with the card's detile on: no
    // staging buffer. A check of it (a list that samples it and uploads
    // nothing) notes the memory and writes nothing.
    let again = Arc::new(staging(&device, &title, &layout, false));
    assert!(again.buffer().is_none());
    again.refresh();
    assert!(
        again.last.lock().unwrap().hash.is_some(),
        "the check took the memory's hash"
    );
    assert!(
        again.dirty.load(Relaxed),
        "a check that finds a change says so"
    );
    again.dirty.store(false, Relaxed);

    // 3. The raw ring full: the card's way is refused at once, nothing
    // stays reserved.
    let (mut raw, mut out) = (Arena::new(GRAIN), Arena::new(1 << 20));
    assert!(raw.alloc(1).is_some(), "fill the raw ring");
    let (raw_used, out_used) = (raw.used(), out.used());
    let routed = route(
        &again,
        || {
            take_slices(&mut raw, &mut out, again.layout.size, again.bytes)
                .map(|_| unreachable!("the ring is full"))
        },
        |bytes| upload(&device, bytes),
    );
    assert!(matches!(routed, Route::Cpu), "the CPU's way");
    assert_eq!(
        (raw.used(), out.used()),
        (raw_used, out_used),
        "no slice half taken"
    );
    assert!(again.buffer().is_some(), "a staging buffer made for it");
    assert!(again.dirty.load(Relaxed), "to be uploaded");
    assert!(
        again.last.lock().unwrap().hash.is_none(),
        "what it was detiled from is forgotten"
    );

    // 4. The submission's refresh: the memory unchanged, and detiled all
    // the same.
    again.refresh();
    assert!(held(&again) == want, "the CPU's way, after the fallback");

    // 5. From now on the CPU's way, without asking the card or making
    // another buffer.
    let routed = route(
        &again,
        || unreachable!("not asked again"),
        |_| unreachable!("made once"),
    );
    assert!(matches!(routed, Route::Cpu));

    // 6. No memory for a staging buffer either: nothing recorded, and
    // asked again later.
    let bare = staging(&device, &title, &layout, false);
    let routed = route(&bare, || Err(Why::OutFull), |_| None);
    assert!(matches!(routed, Route::Nothing));
    assert!(bare.buffer().is_none());
}

/// Without the switch every staging state has its buffer from the start:
/// the route is the CPU's without asking the card at all.
#[test]
fn warp_a_texture_with_a_staging_buffer_never_asks_the_card() {
    let Some(w) = Warp::new() else {
        eprintln!("no software adapter: skipped");
        return;
    };
    let device = w.device().clone();
    let layout = layout(64, 64, 4, 1, false, 4, Swizzle::SW_64KB_S);
    let title = pattern(layout.size);
    let s = staging(&device, &title, &layout, true);
    let routed = route(
        &s,
        || unreachable!("not asked"),
        |_| unreachable!("not made"),
    );
    assert!(matches!(routed, Route::Cpu));
    assert!(s.ensure_cpu(|_| unreachable!("already there")));
}

/// What `crate::detile::reserve` checks of each part, with the device's
/// own footprints: every level of every slice passes (the last one's last
/// row ends at its bytes, not at a whole pitch), and a slice's table is
/// slice 0's moved by whole slices.
#[test]
fn warp_every_part_of_a_staging_layout_fits_its_slices() {
    use crate::detile::{check_part, pack, table_slice};
    let Some(w) = Warp::new() else {
        eprintln!("no software adapter: skipped");
        return;
    };
    let device = w.device().clone();
    for l in [
        layout(256, 256, 9, 1, true, 16, Swizzle::SW_64KB_S),
        layout(1024, 512, 11, 1, true, 16, Swizzle::SW_64KB_D),
        layout(512, 512, 10, 6, true, 16, Swizzle::SW_4KB_S),
        layout(300, 200, 9, 3, false, 4, Swizzle::SW_64KB_S),
    ] {
        let title = vec![0u8; l.size as usize];
        let s = staging(&device, &title, &l, false);
        let subs = s.subs();
        assert_eq!(subs.len(), (l.desc.mips * l.desc.depth) as usize);
        for (i, sub) in subs.iter().enumerate() {
            let (keyed, shift) = table_slice(&l, sub.slice);
            assert_eq!(keyed, 0, "one table for every slice");
            let depth_pitch = sub.row_pitch as usize * sub.rows as usize;
            let packed = pack(
                &l.copy_table(sub.mip, keyed, sub.row_pitch as usize, depth_pitch)
                    .unwrap(),
            )
            .unwrap();
            let bytes = check_part(sub, &packed, shift, l.size, s.bytes)
                .unwrap_or_else(|why| panic!("{:?} part {i}: {why:?}", l.desc));
            assert!(sub.offset + bytes <= s.bytes);
            if i + 1 == subs.len() {
                assert!(
                    bytes < sub.row_pitch * u64::from(sub.rows),
                    "the last part ends inside its last row's pitch"
                );
            }
            // Moved by its shift, slice 0's table reads what this slice's
            // own table reads.
            let own = pack(
                &l.copy_table(sub.mip, sub.slice, sub.row_pitch as usize, depth_pitch)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(own.tiled_end, packed.tiled_end + shift);
        }
    }
}

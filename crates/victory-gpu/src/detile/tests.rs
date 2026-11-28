use super::*;
use victory_formats::texture::{Desc, Swizzle};

fn desc(w: u32, h: u32, mips: u32, bpe: u32, block: u32, depth: u32, swizzle: Swizzle) -> Desc {
    Desc {
        dimension: Dimension::Tex2d,
        width: w,
        height: h,
        depth,
        mips,
        element_bytes: bpe,
        block,
        samples: 1,
        swizzle,
    }
}

/// Bytes that differ from their neighbours, so that a byte moved to the
/// wrong place shows.
fn pattern(len: u64) -> Vec<u8> {
    (0..len)
        .map(|i| (i.wrapping_mul(0x9e37_79b9) >> 24) as u8)
        .collect()
}

/// [`emulate`] of a whole table at word 0, every row in one dispatch.
fn emulate_table(p: &Packed, shift: u32, raw: &[u8], out: &mut [u8]) {
    let c = [
        0,
        p.runs,
        3 * p.runs,
        0,
        shift,
        0,
        raw.len() as u32,
        out.len() as u32,
    ];
    emulate(&p.words, c, (p.runs.div_ceil(64), p.rows), raw, out);
}

fn layouts() -> Vec<Layout> {
    let mut out = Vec::new();
    for swizzle in [
        Swizzle::LINEAR,
        Swizzle::SW_256B_S,
        Swizzle::SW_4KB_S,
        Swizzle::SW_4KB_D,
        Swizzle::SW_64KB_S,
        Swizzle::SW_64KB_D,
    ] {
        out.push(Layout::new(&desc(512, 256, 10, 16, 4, 1, swizzle)).unwrap());
        out.push(Layout::new(&desc(300, 200, 9, 8, 4, 6, swizzle)).unwrap());
        out.push(Layout::new(&desc(257, 129, 9, 4, 1, 3, swizzle)).unwrap());
        out.push(Layout::new(&desc(96, 64, 7, 8, 1, 1, swizzle)).unwrap());
    }
    out.push(
        Layout::new(&Desc {
            dimension: Dimension::Tex3d,
            ..desc(64, 32, 6, 4, 1, 16, Swizzle::SW_4KB_S)
        })
        .unwrap(),
    );
    out.push(
        Layout::new(&Desc {
            dimension: Dimension::Tex3d,
            ..desc(40, 24, 5, 8, 1, 9, Swizzle::SW_64KB_S)
        })
        .unwrap(),
    );
    out
}

#[test]
fn the_arena_fills_refuses_and_frees_by_fence() {
    let mut a = Arena::new(4 * GRAIN);
    let x = a.alloc(GRAIN).unwrap();
    let y = a.alloc(2 * GRAIN - 1).unwrap();
    assert_eq!(a.used(), 3 * GRAIN);
    assert_eq!(a.alloc(2 * GRAIN), None, "full: refused, not waited for");
    let z = a.alloc(1).unwrap();
    assert_eq!(a.alloc(1), None);
    // Given back in another order, waiting for the card.
    a.release(y, 2 * GRAIN - 1, 7);
    a.release(x, GRAIN, 5);
    assert_eq!(
        a.alloc(1),
        None,
        "nothing free until the card passes the fences"
    );
    a.reclaim(5);
    assert_eq!(a.alloc(2 * GRAIN), None, "x alone is free");
    a.reclaim(7);
    // x and y join: three grains in one range.
    assert_eq!(a.alloc(3 * GRAIN), Some(0));
    a.release(0, 3 * GRAIN, 0);
    a.release(z, 1, 0);
    assert_eq!(a.used(), 0);
    assert_eq!(a.alloc(4 * GRAIN), Some(0));
    assert_eq!(Arena::new(GRAIN - 1).alloc(1), None);
}

/// Both slices or neither: a full ring refuses at once, and the other
/// ring's slice goes back.
#[test]
fn slices_are_taken_both_or_neither() {
    let (mut raw, mut out) = (Arena::new(GRAIN), Arena::new(1 << 20));
    assert!(raw.alloc(1).is_some());
    assert_eq!(
        take_slices(&mut raw, &mut out, 1000, 1000),
        Err(Why::RawFull)
    );
    assert_eq!(out.used(), 0);
    let (mut raw, mut out) = (Arena::new(1 << 20), Arena::new(GRAIN));
    assert!(out.alloc(1).is_some());
    assert_eq!(
        take_slices(&mut raw, &mut out, 1000, 1000),
        Err(Why::OutFull)
    );
    assert_eq!(raw.used(), 0, "the raw slice given back");
}

/// A packed table, run as the shader runs it, writes what the CPU's detile
/// writes, for every mip of every layout family; every array slice from
/// slice 0's table and its shift.
#[test]
fn packed_tables_detile_as_the_cpu_does_and_slices_share_one() {
    let mut checked = 0;
    for l in layouts() {
        let raw = pattern(l.size);
        let n = l.desc.element_bytes as usize;
        let slices = if l.desc.dimension == Dimension::Tex3d {
            1
        } else {
            l.slices
        };
        for mip in 0..l.desc.mips {
            let m = l.mips[mip as usize];
            let pitch = (m.width as usize * n).next_multiple_of(256);
            let plane = pitch * m.height as usize;
            let Ok(packed) = l
                .copy_table(mip, 0, pitch, plane)
                .map_err(|_| Why::NoTable)
                .and_then(|t| pack(&t))
            else {
                continue;
            };
            assert!(
                packed
                    .words
                    .chunks(3)
                    .take(packed.runs as usize)
                    .all(|r| u64::from(r[2]) <= RUN_MOST)
            );
            for slice in 0..slices {
                let (keyed, shift) = table_slice(&l, slice);
                let table = if keyed == 0 {
                    packed.clone()
                } else {
                    pack(&l.copy_table(mip, keyed, pitch, plane).unwrap()).unwrap()
                };
                assert!(
                    shift + table.tiled_end <= l.size,
                    "{:?} mip {mip} slice {slice}",
                    l.desc
                );
                let len = plane * m.depth as usize;
                let (mut cpu, mut card) = (vec![0xcc; len], vec![0xcc; len]);
                l.detile(&raw, mip, slice, &mut cpu, pitch, plane).unwrap();
                emulate_table(&table, shift as u32, &raw, &mut card);
                assert!(cpu == card, "{:?} mip {mip} slice {slice}", l.desc);
                checked += 1;
            }
        }
    }
    assert!(checked > 300, "{checked}");
}

#[test]
fn narrow_levels_of_small_elements_are_refused() {
    let l = Layout::new(&desc(256, 256, 9, 1, 1, 1, Swizzle::SW_64KB_S)).unwrap();
    let t = l.copy_table(8, 0, 256, 256).unwrap();
    assert_eq!(pack(&t), Err(Why::Unaligned));
}

/// The shader's checks: a table reaching past the slices writes nothing
/// outside them and reads nothing past the raw slice.
#[test]
fn words_outside_the_slices_are_left_out() {
    let l = Layout::new(&desc(64, 64, 1, 16, 4, 1, Swizzle::SW_64KB_S)).unwrap();
    let p = pack(&l.copy_table(0, 0, 256, 256 * 16).unwrap()).unwrap();
    let raw = vec![7u8; p.tiled_end as usize];
    let mut out = vec![0u8; 100];
    emulate_table(&p, 0, &raw, &mut out);
    assert!(out.iter().all(|&b| b == 0 || b == 7));
    let mut tail = vec![0u8; 4096];
    emulate_table(&p, 1 << 30, &raw, &mut tail);
    assert!(
        tail.iter().all(|&b| b == 0),
        "nothing read past the raw slice"
    );
}

/// A part whose table reaches past its slices is refused before anything
/// is recorded.
#[test]
fn a_part_outside_its_slices_is_refused() {
    let l = Layout::new(&desc(64, 64, 1, 16, 4, 1, Swizzle::SW_64KB_S)).unwrap();
    let p = pack(&l.copy_table(0, 0, 256, 256 * 16).unwrap()).unwrap();
    let sub = Sub {
        mip: 0,
        slice: 0,
        offset: 0,
        row_pitch: 256,
        rows: 16,
        depth: 1,
    };
    assert_eq!(check_part(&sub, &p, 0, l.size, 4096), Ok(4096));
    assert_eq!(
        check_part(&sub, &p, 0, p.tiled_end - 1, 4096),
        Err(Why::Bounds)
    );
    assert_eq!(check_part(&sub, &p, 0, l.size, 4095), Err(Why::Bounds));
    assert_eq!(check_part(&sub, &p, l.size, 2 * l.size, 4096), Ok(4096));
}

#[test]
fn the_first_difference_is_found_inside_the_ranges_only() {
    let expected = [1u8, 2, 3, 4, 5, 6];
    let got = [1u8, 9, 3, 4, 5, 7];
    assert_eq!(first_difference(&got, &expected, &[(2, 3)]), None);
    assert_eq!(
        first_difference(&got, &expected, &[(2, 3), (4, 2)]),
        Some(5)
    );
    assert_eq!(first_difference(&got, &expected, &[(0, 6)]), Some(1));
}

/// The shader itself on a D3D12 device, against the CPU's detile:
/// BC-sized tiled elements, an array slice by shift, a split dispatch and
/// zeros. Skipped without a device.
#[test]
fn warp_the_shader_detiles_as_the_cpu_does() {
    let Some(w) = warp::Warp::new() else {
        eprintln!("no D3D12 device: skipped");
        return;
    };
    let code = crate::host::compile(
        DETILE,
        windows::core::s!("main"),
        windows::core::s!("cs_5_1"),
    )
    .expect("the detile shader compiles");
    let mut checked = 0;
    for l in [
        Layout::new(&desc(512, 512, 10, 16, 4, 2, Swizzle::SW_64KB_S)).unwrap(),
        Layout::new(&desc(300, 200, 9, 8, 4, 1, Swizzle::SW_4KB_D)).unwrap(),
        Layout::new(&desc(200, 100, 1, 4, 1, 1, Swizzle::LINEAR)).unwrap(),
        Layout::new(&desc(256, 128, 8, 4, 1, 3, Swizzle::SW_64KB_D)).unwrap(),
        Layout::new(&Desc {
            dimension: Dimension::Tex3d,
            ..desc(64, 32, 6, 4, 1, 16, Swizzle::SW_4KB_S)
        })
        .unwrap(),
    ] {
        let raw = pattern(l.size);
        let n = l.desc.element_bytes as usize;
        for mip in [0, l.desc.mips - 1] {
            let m = l.mips[mip as usize];
            let pitch = (m.width as usize * n).next_multiple_of(256);
            let plane = pitch * m.height as usize;
            let slice = if l.desc.dimension == Dimension::Tex3d {
                0
            } else {
                l.slices - 1
            };
            let (keyed, shift) = table_slice(&l, slice);
            let p = pack(&l.copy_table(mip, keyed, pitch, plane).unwrap()).unwrap();
            let len = plane * m.depth as usize;
            let mut cpu = vec![0u8; len + 512];
            l.detile(&raw, mip, slice, &mut cpu[512..], pitch, plane)
                .unwrap();
            // Zeros over the first 512 bytes, then the table's copy.
            let got = w.run(&code, &raw, &p, shift as u32, 512, len + 512);
            for z in 0..m.depth as usize {
                for y in 0..m.height as usize {
                    let at = 512 + z * plane + y * pitch;
                    let bytes = m.width as usize * n;
                    assert_eq!(
                        &got[at..at + bytes],
                        &cpu[at..at + bytes],
                        "{:?} mip {mip} z {z} row {y}",
                        l.desc
                    );
                }
            }
            assert!(got[..512].iter().all(|&b| b == 0), "zeros");
            checked += 1;
        }
    }
    eprintln!("detile shader: {checked} subresources byte-exact against the CPU");
}

/// Zeros, a table at a word offset and a split dispatch, as [`jobs_for`]
/// makes them, run on the CPU: the parts land where the CPU's detile puts
/// them, and a level not held is zeros.
#[test]
fn planned_jobs_place_every_part() {
    let l = Layout::new(&desc(128, 64, 3, 8, 4, 2, Swizzle::SW_4KB_S)).unwrap();
    let raw = pattern(l.size);
    // Two slices of three levels, each at its own 256-byte pitch.
    let mut subs = Vec::new();
    let mut offset = 0u64;
    for slice in 0..2 {
        for mip in 0..3 {
            let m = l.mips[mip as usize];
            let row_pitch = (u64::from(m.width) * 8).next_multiple_of(256);
            subs.push(Sub {
                mip,
                slice,
                offset,
                row_pitch,
                rows: m.height,
                depth: 1,
            });
            offset = (offset + row_pitch * u64::from(m.height)).next_multiple_of(512);
        }
    }
    let held = !0b010;
    let (words, jobs) = plan(&l, &subs, l.size, offset, held).unwrap();
    let mut card = vec![0xcc; offset as usize];
    for (c, groups) in &jobs {
        emulate(&words, *c, *groups, &raw, &mut card);
    }
    for sub in &subs {
        let m = l.mips[sub.mip as usize];
        let (at, pitch) = (sub.offset as usize, sub.row_pitch as usize);
        let plane = pitch * m.height as usize;
        if held & (1 << sub.mip) == 0 {
            assert!(card[at..at + plane].iter().all(|&b| b == 0), "{sub:?}");
            continue;
        }
        let mut cpu = vec![0u8; plane];
        l.detile(&raw, sub.mip, sub.slice, &mut cpu, pitch, plane)
            .unwrap();
        for y in 0..m.height as usize {
            let row = y * pitch..y * pitch + m.width as usize * 8;
            assert_eq!(
                &card[at + row.start..at + row.end],
                &cpu[row],
                "{sub:?} row {y}"
            );
        }
    }
}

pub(crate) mod warp {
    use super::super::*;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};

    /// A device on the adapter DXGI calls WARP (the software one on
    /// Windows), or on the default adapter where there is none (Wine with
    /// a translated DXGI), with a queue to run one list at a time.
    pub(crate) struct Warp {
        device: ID3D12Device,
        queue: ID3D12CommandQueue,
        allocator: ID3D12CommandAllocator,
        list: ID3D12GraphicsCommandList,
        fence: ID3D12Fence,
        event: HANDLE,
        sent: std::cell::Cell<u64>,
    }

    impl Warp {
        pub(crate) fn new() -> Option<Warp> {
            // SAFETY: plain DXGI queries and object creation.
            unsafe {
                let mut device: Option<ID3D12Device> = None;
                let warp = CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0))
                    .and_then(|factory| factory.EnumWarpAdapter::<IDXGIAdapter>());
                if let Ok(adapter) = warp {
                    let _ = D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device);
                }
                if device.is_none() {
                    let _ = D3D12CreateDevice(
                        None::<&windows::core::IUnknown>,
                        D3D_FEATURE_LEVEL_11_0,
                        &mut device,
                    );
                }
                let device = device?;
                let queue: ID3D12CommandQueue = device
                    .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                        Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                        ..Default::default()
                    })
                    .ok()?;
                let allocator: ID3D12CommandAllocator = device
                    .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
                    .ok()?;
                let list: ID3D12GraphicsCommandList = device
                    .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)
                    .ok()?;
                list.Close().ok()?;
                let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).ok()?;
                let event = CreateEventW(None, false, false, None).ok()?;
                Some(Warp {
                    device,
                    queue,
                    allocator,
                    list,
                    fence,
                    event,
                    sent: std::cell::Cell::new(0),
                })
            }
        }

        /// The device.
        pub(crate) fn device(&self) -> &ID3D12Device {
            &self.device
        }

        fn buffer(
            &self,
            kind: D3D12_HEAP_TYPE,
            size: u64,
            flags: D3D12_RESOURCE_FLAGS,
            state: D3D12_RESOURCE_STATES,
        ) -> ID3D12Resource {
            let heap = D3D12_HEAP_PROPERTIES {
                Type: kind,
                ..Default::default()
            };
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: size.max(4),
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                Flags: flags,
                ..Default::default()
            };
            let mut buffer: Option<ID3D12Resource> = None;
            // SAFETY: plain creation.
            unsafe {
                self.device
                    .CreateCommittedResource(
                        &heap,
                        D3D12_HEAP_FLAG_NONE,
                        &desc,
                        state,
                        None,
                        &mut buffer,
                    )
                    .unwrap()
            };
            buffer.unwrap()
        }

        fn upload(&self, bytes: &[u8]) -> ID3D12Resource {
            let b = self.buffer(
                D3D12_HEAP_TYPE_UPLOAD,
                bytes.len() as u64,
                D3D12_RESOURCE_FLAG_NONE,
                D3D12_RESOURCE_STATE_GENERIC_READ,
            );
            // SAFETY: a mapped upload buffer of at least `bytes.len()` bytes.
            unsafe {
                let mut data = std::ptr::null_mut();
                b.Map(0, None, Some(&mut data)).unwrap();
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), data.cast::<u8>(), bytes.len());
                b.Unmap(0, None);
            }
            b
        }

        /// Runs the detile shader as `pre_list` records it: zeros over the
        /// first `zeros` bytes, then the table at `zeros` (its rows in two
        /// dispatches, as a table of more than 65,535 rows is split), into
        /// a ring of `out_len` bytes; gives the ring's bytes.
        pub(super) fn run(
            &self,
            code: &[u8],
            raw: &[u8],
            p: &Packed,
            shift: u32,
            zeros: u32,
            out_len: usize,
        ) -> Vec<u8> {
            let bytes: Vec<u8> = p.words.iter().flat_map(|w| w.to_le_bytes()).collect();
            let (raw_b, table_b) = (self.upload(raw), self.upload(&bytes));
            let out = self.buffer(
                D3D12_HEAP_TYPE_DEFAULT,
                out_len as u64,
                D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_COMMON,
            );
            let back = self.buffer(
                D3D12_HEAP_TYPE_READBACK,
                out_len as u64,
                D3D12_RESOURCE_FLAG_NONE,
                D3D12_RESOURCE_STATE_COPY_DEST,
            );
            let (root, pso) = pipeline(&self.device, code).expect("the detile pipeline");
            let (common, uav, source) = (
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            );
            // SAFETY: our device, list and buffers, alive until the wait
            // below has seen the list run.
            unsafe {
                self.allocator.Reset().unwrap();
                self.list.Reset(&self.allocator, None).unwrap();
                let l = &self.list;
                l.ResourceBarrier(&[crate::host::transition(&out, common, uav)]);
                l.SetComputeRootSignature(&root);
                l.SetPipelineState(&pso);
                l.SetComputeRootShaderResourceView(ROOT_RAW, raw_b.GetGPUVirtualAddress());
                l.SetComputeRootShaderResourceView(ROOT_TABLES, table_b.GetGPUVirtualAddress());
                l.SetComputeRootUnorderedAccessView(ROOT_OUT, out.GetGPUVirtualAddress());
                let (raw_len, out_len) = (raw.len() as u32, out_len as u32);
                let words = zeros / 4;
                let c = [0u32, 0, words, 0, 0, 0, raw_len, out_len];
                l.SetComputeRoot32BitConstants(ROOT_CONSTANTS, 8, c.as_ptr().cast(), 0);
                l.Dispatch(words.div_ceil(64), 1, 1);
                let half = p.rows / 2;
                for (first, rows) in [(0, half), (half, p.rows - half)] {
                    if rows == 0 {
                        continue;
                    }
                    let c = [
                        0u32,
                        p.runs,
                        3 * p.runs,
                        first,
                        shift,
                        zeros,
                        raw_len,
                        out_len,
                    ];
                    l.SetComputeRoot32BitConstants(ROOT_CONSTANTS, 8, c.as_ptr().cast(), 0);
                    l.Dispatch(p.runs.div_ceil(64), rows, 1);
                }
                l.ResourceBarrier(&[crate::host::transition(&out, uav, source)]);
                l.CopyBufferRegion(&back, 0, &out, 0, u64::from(out_len));
                l.ResourceBarrier(&[crate::host::transition(&out, source, common)]);
                l.Close().unwrap();
                self.queue.ExecuteCommandLists(&[Some(l.cast().unwrap())]);
                self.sent.set(self.sent.get() + 1);
                self.queue.Signal(&self.fence, self.sent.get()).unwrap();
                self.fence
                    .SetEventOnCompletion(self.sent.get(), self.event)
                    .unwrap();
                WaitForSingleObject(self.event, INFINITE);
                let mut data = std::ptr::null_mut();
                back.Map(0, None, Some(&mut data)).unwrap();
                let got = std::slice::from_raw_parts(data.cast::<u8>(), out_len as usize).to_vec();
                back.Unmap(0, None);
                got
            }
        }

        /// Runs `jobs` (root constants, groups) as `pre_list` records them,
        /// the tables `words` in one buffer, the raw slice `raw`, into a
        /// ring of `out_len` bytes filled with `0xcc` first; gives the
        /// ring's bytes.
        pub(crate) fn run_jobs(
            &self,
            code: &[u8],
            raw: &[u8],
            words: &[u32],
            jobs: &[([u32; 8], (u32, u32))],
            out_len: usize,
        ) -> Vec<u8> {
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            let (raw_b, table_b) = (self.upload(raw), self.upload(&bytes));
            let fill = self.upload(&vec![0xcc; out_len]);
            let out = self.buffer(
                D3D12_HEAP_TYPE_DEFAULT,
                out_len as u64,
                D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_COPY_DEST,
            );
            let back = self.buffer(
                D3D12_HEAP_TYPE_READBACK,
                out_len as u64,
                D3D12_RESOURCE_FLAG_NONE,
                D3D12_RESOURCE_STATE_COPY_DEST,
            );
            let (root, pso) = pipeline(&self.device, code).expect("the detile pipeline");
            let (dest, uav, source) = (
                D3D12_RESOURCE_STATE_COPY_DEST,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            );
            // SAFETY: our device, list and buffers, alive until the wait
            // below has seen the list run.
            unsafe {
                self.allocator.Reset().unwrap();
                self.list.Reset(&self.allocator, None).unwrap();
                let l = &self.list;
                l.CopyBufferRegion(&out, 0, &fill, 0, out_len as u64);
                l.ResourceBarrier(&[crate::host::transition(&out, dest, uav)]);
                l.SetComputeRootSignature(&root);
                l.SetPipelineState(&pso);
                l.SetComputeRootShaderResourceView(ROOT_TABLES, table_b.GetGPUVirtualAddress());
                for (c, groups) in jobs {
                    l.SetComputeRoot32BitConstants(ROOT_CONSTANTS, 8, c.as_ptr().cast(), 0);
                    l.SetComputeRootShaderResourceView(ROOT_RAW, raw_b.GetGPUVirtualAddress());
                    l.SetComputeRootUnorderedAccessView(ROOT_OUT, out.GetGPUVirtualAddress());
                    l.Dispatch(groups.0, groups.1, 1);
                }
                l.ResourceBarrier(&[crate::host::transition(&out, uav, source)]);
                l.CopyBufferRegion(&back, 0, &out, 0, out_len as u64);
                l.Close().unwrap();
                self.queue.ExecuteCommandLists(&[Some(l.cast().unwrap())]);
                self.sent.set(self.sent.get() + 1);
                self.queue.Signal(&self.fence, self.sent.get()).unwrap();
                self.fence
                    .SetEventOnCompletion(self.sent.get(), self.event)
                    .unwrap();
                WaitForSingleObject(self.event, INFINITE);
                let mut data = std::ptr::null_mut();
                back.Map(0, None, Some(&mut data)).unwrap();
                let got = std::slice::from_raw_parts(data.cast::<u8>(), out_len).to_vec();
                back.Unmap(0, None);
                got
            }
        }
    }
}

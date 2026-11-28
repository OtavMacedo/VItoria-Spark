//! `VICTORY_DESC_MEMCPY=1` (off by default): a host shader or unordered
//! access view made before is copied as bytes instead of made again.
//!
//! The draws of a frame make tens of thousands of views in the slots of
//! their descriptor tables, most of them of the same textures and buffers
//! as the draw before. With the switch, each recording thread keeps the
//! views it made in a heap the card does not see, by what the driver made
//! them from (the host resource and every field of the description), and
//! a view it has is copied into the slot as the `step` bytes a descriptor
//! is, instead of the driver call that makes it. The resources of the kept
//! views are held while kept, so a freed resource's address cannot come
//! back as another resource that matches; everything kept goes at each
//! frame (nothing is held longer than about a frame), when the heap is
//! full, and at the first view made with the switch off, so it may
//! alternate inside a run (`SPARK_AB`): under either value each slot gets
//! the view the driver would make.
//!
//! shortcut: a descriptor copied as bytes is the same descriptor only
//! where the driver's descriptors are plain bytes that name no slot of
//! their own (the Windows drivers of NVIDIA and AMD; not vkd3d, where a
//! descriptor carries more than its bytes). Constant views (each at the
//! address its constants have that draw) and samplers are made as before.
//!
//! From trustno1-coder/VIctory-NVIDIA pull request #469 (HungrySharkNet),
//! which copies the bytes of a view its thread made earlier in the same
//! list instead of calling `CopyDescriptorsSimple`; this repository makes
//! every view, so here the copy stands for `Create*View`.

use std::cell::RefCell;
use std::collections::HashMap;
#[cfg(feature = "diagnostics")]
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use windows::Win32::Graphics::Direct3D12::*;
use windows::core::Interface;

/// What a view is made from: the host resource (its interface pointer; 0
/// for none), then the kind of view and the description's fields.
type Key = (usize, [u32; 12]);

/// Views a thread keeps at once.
const SLOTS: u32 = 4096;

/// Whether the switch is on now; with it off, what this thread kept goes.
fn on() -> bool {
    let switch = &crate::switches::DESC_MEMCPY;
    if switch.on() {
        return true;
    }
    if switch.ever() {
        CACHE.with(|cell| {
            if let Ok(mut cache) = cell.try_borrow_mut()
                && let Some(cache) = cache.as_mut()
            {
                cache.slots.forget();
            }
        });
    }
    false
}

/// Makes a shader view of `resource` at `at`, as `CreateShaderResourceView`.
pub(crate) fn srv(
    resource: Option<&ID3D12Resource>,
    desc: Option<&D3D12_SHADER_RESOURCE_VIEW_DESC>,
    at: D3D12_CPU_DESCRIPTOR_HANDLE,
) {
    let Some(host) = crate::host::host() else {
        return;
    };
    // SAFETY: a slot of a view heap, and a live resource or a description.
    let make = |to| unsafe {
        host.device
            .CreateShaderResourceView(resource, desc.map(std::ptr::from_ref), to)
    };
    match on().then(|| srv_key(desc)).flatten() {
        Some(words) => copied(resource, words, at, make),
        None => make(at),
    }
}

/// Makes an unordered access view of `resource` (without a counter) at
/// `at`, as `CreateUnorderedAccessView`.
pub(crate) fn uav(
    resource: Option<&ID3D12Resource>,
    desc: Option<&D3D12_UNORDERED_ACCESS_VIEW_DESC>,
    at: D3D12_CPU_DESCRIPTOR_HANDLE,
) {
    let Some(host) = crate::host::host() else {
        return;
    };
    // SAFETY: a slot of a view heap, and a live resource or a description.
    let make = |to| unsafe {
        host.device.CreateUnorderedAccessView(
            resource,
            None::<&ID3D12Resource>,
            desc.map(std::ptr::from_ref),
            to,
        )
    };
    match on().then(|| uav_key(desc)).flatten() {
        Some(words) => copied(resource, words, at, make),
        None => make(at),
    }
}

/// This thread's views kept for copying: a heap the card does not see,
/// and which of its slots holds which view.
struct Cache {
    start: usize,
    step: usize,
    slots: Slots<ID3D12Resource>,
    _heap: ID3D12DescriptorHeap,
}

thread_local! {
    static CACHE: RefCell<Option<Cache>> = const { RefCell::new(None) };
}

impl Cache {
    fn new() -> Option<Cache> {
        let host = crate::host::host()?;
        // SAFETY: plain creation of a heap the card does not see.
        let (heap, step) = unsafe {
            let heap: ID3D12DescriptorHeap = host
                .device
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                    NumDescriptors: SLOTS,
                    ..Default::default()
                })
                .ok()?;
            let step = host
                .device
                .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV);
            (heap, step)
        };
        // SAFETY: a live heap.
        let start = unsafe { heap.GetCPUDescriptorHandleForHeapStart() }.ptr;
        Some(Cache {
            start,
            step: step as usize,
            slots: Slots::new(SLOTS),
            _heap: heap,
        })
    }
}

/// The view `words` of `resource` at `at`: copied from this thread's heap,
/// made there first (`make`) if it does not hold it.
fn copied(
    resource: Option<&ID3D12Resource>,
    words: [u32; 12],
    at: D3D12_CPU_DESCRIPTOR_HANDLE,
    make: impl FnOnce(D3D12_CPU_DESCRIPTOR_HANDLE),
) {
    CACHE.with(|cell| {
        let Ok(mut cell) = cell.try_borrow_mut() else {
            return make(at);
        };
        if cell.is_none() {
            *cell = Cache::new();
        }
        let Some(cache) = cell.as_mut() else {
            return make(at);
        };
        let key = (resource.map_or(0, |r| r.as_raw() as usize), words);
        let (slot, kept) = cache
            .slots
            .find(crate::frame::current(), key, || resource.cloned());
        let from = cache.start + slot as usize * cache.step;
        if !kept {
            make(D3D12_CPU_DESCRIPTOR_HANDLE { ptr: from });
        }
        #[cfg(feature = "diagnostics")]
        COUNTS[usize::from(kept)].fetch_add(1, Relaxed);
        // SAFETY: a slot of this thread's heap and the caller's slot, a
        // descriptor (`step` bytes) each, in different heaps.
        unsafe {
            std::ptr::copy_nonoverlapping(from as *const u8, at.ptr as *mut u8, cache.step);
        }
    });
    #[cfg(feature = "diagnostics")]
    line();
}

/// Which slot holds which view, and what keeps the views' resources alive
/// while they are kept (`H`).
struct Slots<H> {
    frame: u64,
    capacity: u32,
    found: HashMap<Key, u32, std::hash::BuildHasherDefault<WordHash>>,
    held: Vec<H>,
}

impl<H> Slots<H> {
    fn new(capacity: u32) -> Self {
        Slots {
            frame: 0,
            capacity,
            found: HashMap::default(),
            held: Vec::new(),
        }
    }

    /// Nothing is kept: every view is made again.
    fn forget(&mut self) {
        self.found.clear();
        self.held.clear();
    }

    /// The slot for the view `key` in frame `frame`, and whether it holds
    /// the view already; if not, the slot is the view's from now and
    /// `hold` gives what keeps its resource alive. Everything goes at a
    /// new frame and when every slot is taken.
    fn find(&mut self, frame: u64, key: Key, hold: impl FnOnce() -> Option<H>) -> (u32, bool) {
        if frame != self.frame {
            self.frame = frame;
            self.forget();
        }
        if let Some(&slot) = self.found.get(&key) {
            return (slot, true);
        }
        if self.found.len() as u32 >= self.capacity {
            self.forget();
        }
        let slot = self.found.len() as u32;
        self.found.insert(key, slot);
        self.held.extend(hold());
        (slot, false)
    }
}

/// A hasher of a few words (a [`Key`]): one multiply a word.
#[derive(Default)]
struct WordHash(u64);

impl std::hash::Hasher for WordHash {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(u64::from(b));
        }
    }

    fn write_u32(&mut self, n: u32) {
        self.write_u64(u64::from(n));
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0.rotate_left(5) ^ n).wrapping_mul(0x517c_c1b7_2722_0a95);
    }

    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }
}

/// A shader view description's fields (the ones its dimension uses);
/// `None` for a dimension this does not know.
fn srv_key(desc: Option<&D3D12_SHADER_RESOURCE_VIEW_DESC>) -> Option<[u32; 12]> {
    let mut w = [0u32; 12];
    w[0] = 1;
    let Some(d) = desc else {
        w[1] = u32::MAX;
        return Some(w);
    };
    w[1] = d.Format.0 as u32;
    w[2] = d.ViewDimension.0 as u32;
    w[3] = d.Shader4ComponentMapping;
    let lod = f32::to_bits;
    let mut put = |fields: &[u32]| w[4..4 + fields.len()].copy_from_slice(fields);
    // SAFETY: each arm reads the member the dimension says is in use.
    unsafe {
        let a = &d.Anonymous;
        match d.ViewDimension {
            D3D12_SRV_DIMENSION_BUFFER => {
                let b = a.Buffer;
                put(&[
                    b.FirstElement as u32,
                    (b.FirstElement >> 32) as u32,
                    b.NumElements,
                    b.StructureByteStride,
                    b.Flags.0 as u32,
                ]);
            }
            D3D12_SRV_DIMENSION_TEXTURE1D => {
                let t = a.Texture1D;
                put(&[t.MostDetailedMip, t.MipLevels, lod(t.ResourceMinLODClamp)]);
            }
            D3D12_SRV_DIMENSION_TEXTURE1DARRAY => {
                let t = a.Texture1DArray;
                put(&[
                    t.MostDetailedMip,
                    t.MipLevels,
                    t.FirstArraySlice,
                    t.ArraySize,
                    lod(t.ResourceMinLODClamp),
                ]);
            }
            D3D12_SRV_DIMENSION_TEXTURE2D => {
                let t = a.Texture2D;
                put(&[
                    t.MostDetailedMip,
                    t.MipLevels,
                    t.PlaneSlice,
                    lod(t.ResourceMinLODClamp),
                ]);
            }
            D3D12_SRV_DIMENSION_TEXTURE2DARRAY => {
                let t = a.Texture2DArray;
                put(&[
                    t.MostDetailedMip,
                    t.MipLevels,
                    t.FirstArraySlice,
                    t.ArraySize,
                    t.PlaneSlice,
                    lod(t.ResourceMinLODClamp),
                ]);
            }
            D3D12_SRV_DIMENSION_TEXTURE2DMS => {}
            D3D12_SRV_DIMENSION_TEXTURE2DMSARRAY => {
                let t = a.Texture2DMSArray;
                put(&[t.FirstArraySlice, t.ArraySize]);
            }
            D3D12_SRV_DIMENSION_TEXTURE3D => {
                let t = a.Texture3D;
                put(&[t.MostDetailedMip, t.MipLevels, lod(t.ResourceMinLODClamp)]);
            }
            D3D12_SRV_DIMENSION_TEXTURECUBE => {
                let t = a.TextureCube;
                put(&[t.MostDetailedMip, t.MipLevels, lod(t.ResourceMinLODClamp)]);
            }
            D3D12_SRV_DIMENSION_TEXTURECUBEARRAY => {
                let t = a.TextureCubeArray;
                put(&[
                    t.MostDetailedMip,
                    t.MipLevels,
                    t.First2DArrayFace,
                    t.NumCubes,
                    lod(t.ResourceMinLODClamp),
                ]);
            }
            _ => return None,
        }
    }
    Some(w)
}

/// An unordered access view description's fields, as [`srv_key`].
fn uav_key(desc: Option<&D3D12_UNORDERED_ACCESS_VIEW_DESC>) -> Option<[u32; 12]> {
    let mut w = [0u32; 12];
    w[0] = 2;
    let Some(d) = desc else {
        w[1] = u32::MAX;
        return Some(w);
    };
    w[1] = d.Format.0 as u32;
    w[2] = d.ViewDimension.0 as u32;
    let mut put = |fields: &[u32]| w[3..3 + fields.len()].copy_from_slice(fields);
    // SAFETY: each arm reads the member the dimension says is in use.
    unsafe {
        let a = &d.Anonymous;
        match d.ViewDimension {
            D3D12_UAV_DIMENSION_BUFFER => {
                let b = a.Buffer;
                put(&[
                    b.FirstElement as u32,
                    (b.FirstElement >> 32) as u32,
                    b.NumElements,
                    b.StructureByteStride,
                    b.CounterOffsetInBytes as u32,
                    (b.CounterOffsetInBytes >> 32) as u32,
                    b.Flags.0 as u32,
                ]);
            }
            D3D12_UAV_DIMENSION_TEXTURE1D => put(&[a.Texture1D.MipSlice]),
            D3D12_UAV_DIMENSION_TEXTURE1DARRAY => {
                let t = a.Texture1DArray;
                put(&[t.MipSlice, t.FirstArraySlice, t.ArraySize]);
            }
            D3D12_UAV_DIMENSION_TEXTURE2D => {
                let t = a.Texture2D;
                put(&[t.MipSlice, t.PlaneSlice]);
            }
            D3D12_UAV_DIMENSION_TEXTURE2DARRAY => {
                let t = a.Texture2DArray;
                put(&[t.MipSlice, t.FirstArraySlice, t.ArraySize, t.PlaneSlice]);
            }
            D3D12_UAV_DIMENSION_TEXTURE2DMS => {}
            D3D12_UAV_DIMENSION_TEXTURE2DMSARRAY => {
                let t = a.Texture2DMSArray;
                put(&[t.FirstArraySlice, t.ArraySize]);
            }
            D3D12_UAV_DIMENSION_TEXTURE3D => {
                let t = a.Texture3D;
                put(&[t.MipSlice, t.FirstWSlice, t.WSize]);
            }
            _ => return None,
        }
    }
    Some(w)
}

/// Diagnostics: views made (0) and copied (1) with the switch, since the
/// last line.
#[cfg(feature = "diagnostics")]
static COUNTS: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];

/// Diagnostics: the line of what the switch copied, every 600 frames
/// while it counts anything.
#[cfg(feature = "diagnostics")]
fn line() {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let frame = crate::frame::current();
    let last = LAST.load(Relaxed);
    if frame < last + 600
        || LAST
            .compare_exchange(last, frame, Relaxed, Relaxed)
            .is_err()
    {
        return;
    }
    let [made, copied] = COUNTS.each_ref().map(|n| n.swap(0, Relaxed));
    if made + copied == 0 {
        return;
    }
    let frames = (frame - last).max(1) as f64;
    tracing::info!(
        "views copied as bytes (VICTORY_DESC_MEMCPY) per frame over {frames:.0} frames: {:.0} made by the driver, \
         {:.0} copied from one made before",
        made as f64 / frames,
        copied as f64 / frames,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn texture_2d(
        format: i32,
        first: u32,
        levels: u32,
        plane: u32,
    ) -> D3D12_SHADER_RESOURCE_VIEW_DESC {
        D3D12_SHADER_RESOURCE_VIEW_DESC {
            Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT(format),
            ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
            Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
            Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                Texture2D: D3D12_TEX2D_SRV {
                    MostDetailedMip: first,
                    MipLevels: levels,
                    PlaneSlice: plane,
                    ResourceMinLODClamp: 0.0,
                },
            },
        }
    }

    #[test]
    fn every_field_a_view_is_made_from_is_in_its_key() {
        let base = texture_2d(28, 0, 1, 0);
        let key = srv_key(Some(&base)).unwrap();
        assert_eq!(srv_key(Some(&texture_2d(28, 0, 1, 0))), Some(key));
        for other in [
            texture_2d(29, 0, 1, 0),
            texture_2d(28, 1, 1, 0),
            texture_2d(28, 0, 2, 0),
            texture_2d(28, 0, 1, 1),
            D3D12_SHADER_RESOURCE_VIEW_DESC {
                Shader4ComponentMapping: 0,
                ..base
            },
            D3D12_SHADER_RESOURCE_VIEW_DESC {
                ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2DARRAY,
                ..base
            },
        ] {
            assert_ne!(srv_key(Some(&other)), Some(key));
        }
        // The texture's own view, and the same fields as an unordered
        // access view: other views.
        assert_ne!(srv_key(None), Some(key));
        assert_ne!(srv_key(None), uav_key(None));
        let buffer = |first: u64, n: u32, counter: u64| D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R32_TYPELESS,
            ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
            Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                Buffer: D3D12_BUFFER_UAV {
                    FirstElement: first,
                    NumElements: n,
                    StructureByteStride: 0,
                    CounterOffsetInBytes: counter,
                    Flags: D3D12_BUFFER_UAV_FLAG_RAW,
                },
            },
        };
        let b = uav_key(Some(&buffer(4, 16, 0)));
        assert_eq!(uav_key(Some(&buffer(4, 16, 0))), b);
        assert_ne!(uav_key(Some(&buffer(4 + (1 << 32), 16, 0))), b);
        assert_ne!(uav_key(Some(&buffer(4, 17, 0))), b);
        assert_ne!(uav_key(Some(&buffer(4, 16, 1 << 32))), b);
        // A dimension not known here is not kept.
        let odd = D3D12_SHADER_RESOURCE_VIEW_DESC {
            ViewDimension: D3D12_SRV_DIMENSION_RAYTRACING_ACCELERATION_STRUCTURE,
            ..base
        };
        assert_eq!(srv_key(Some(&odd)), None);
    }

    #[test]
    fn a_view_is_made_once_and_copied_after() {
        let mut s: Slots<()> = Slots::new(4);
        let a = (0x1000, [1; 12]);
        let b = (0x2000, [1; 12]);
        assert_eq!(s.find(7, a, || None), (0, false));
        assert_eq!(s.find(7, a, || None), (0, true));
        // Another resource with the same description: another view.
        assert_eq!(s.find(7, b, || None), (1, false));
        assert_eq!(s.find(7, b, || None), (1, true));
        assert_eq!(s.find(7, a, || None), (0, true));
    }

    #[test]
    fn everything_goes_at_a_new_frame_when_full_and_when_forgotten() {
        let mut s: Slots<()> = Slots::new(2);
        let key = |n: usize| (n, [0; 12]);
        assert_eq!(s.find(1, key(1), || None), (0, false));
        assert_eq!(s.find(2, key(1), || None), (0, false));
        assert_eq!(s.find(2, key(2), || None), (1, false));
        // Full: started again from the first slot.
        assert_eq!(s.find(2, key(3), || None), (0, false));
        assert_eq!(s.find(2, key(1), || None), (1, false));
        s.forget();
        assert_eq!(s.find(2, key(3), || None), (0, false));
    }

    #[test]
    fn kept_views_hold_their_resource_so_its_address_is_not_reused() {
        let mut s = Slots::new(8);
        let resource = Rc::new(0u8);
        let key = (Rc::as_ptr(&resource) as usize, [3; 12]);
        assert_eq!(s.find(1, key, || Some(resource.clone())), (0, false));
        assert_eq!(Rc::strong_count(&resource), 2);
        // A copy holds nothing more.
        assert_eq!(s.find(1, key, || Some(resource.clone())), (0, true));
        assert_eq!(Rc::strong_count(&resource), 2);
        // Let go at the next frame (and remade, held again, if asked).
        assert_eq!(s.find(2, (1, [0; 12]), || None), (0, false));
        assert_eq!(Rc::strong_count(&resource), 1);
        assert_eq!(s.find(2, key, || Some(resource.clone())), (1, false));
        s.forget();
        assert_eq!(Rc::strong_count(&resource), 1);
    }
}

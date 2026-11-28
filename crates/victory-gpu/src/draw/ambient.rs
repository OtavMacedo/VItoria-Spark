//! Finding the images and depths a program the indirect-light estimate
//! follows has bound (`crate::ambient`), from the list's state right after
//! its dispatch.

use super::*;
use crate::ambient::{
    AIR, DEPTH, LIGHT, MISS_ALPHA, MISS_DEPTH, MISS_LIGHT, MISS_OPAQUE, MissNote, RADIANCE, Role,
    Views,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_D24_UNORM_S8_UINT, DXGI_FORMAT_D32_FLOAT, DXGI_FORMAT_D32_FLOAT_S8X24_UINT,
    DXGI_FORMAT_R24G8_TYPELESS, DXGI_FORMAT_R32_FLOAT, DXGI_FORMAT_R32_TYPELESS,
    DXGI_FORMAT_R32G8X24_TYPELESS,
};

/// The host formats the passes read and write: R32_UINT holding R9G9B9E5
/// (the diffuse light), R11G11B10_FLOAT (the others).
const LIGHT_FORMAT: i32 = 42;
const RADIANCE_FORMAT: i32 = 26;

/// A host resource description and the shader-visible handle of its view.
type Found = (D3D12_RESOURCE_DESC, D3D12_GPU_DESCRIPTOR_HANDLE);

/// A view of a depth-stencil texture's stencil plane (by its format, or a
/// 2D view of plane 1): no depth. Read as one, it puts every point far away
/// with its normal to the camera.
fn stencil_view(view: Option<View>) -> bool {
    let view = view.map(|v| crate::descriptor::host::placed_texture(&v).unwrap_or(v));
    match view {
        // The description's words are D3D12_SHADER_RESOURCE_VIEW_DESC's: its
        // union starts at word 4, so a 2D view's plane is word 6.
        Some(View::Shader(_, Some(d))) => {
            matches!(crate::resource::pc_format(d[0]), 22 | 47) || (d[1] == 4 && d[6] == 1)
        }
        _ => false,
    }
}

/// A 2D texture of a depth format, or of the typeless one of a depth.
fn depth_like(d: &D3D12_RESOURCE_DESC) -> bool {
    d.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE2D
        && matches!(
            d.Format,
            DXGI_FORMAT_R32_TYPELESS
                | DXGI_FORMAT_D32_FLOAT
                | DXGI_FORMAT_R32G8X24_TYPELESS
                | DXGI_FORMAT_D32_FLOAT_S8X24_UINT
                | DXGI_FORMAT_R24G8_TYPELESS
                | DXGI_FORMAT_D24_UNORM_S8_UINT
        )
}

/// A 32-bit depth (with or without stencil), as the miss program reads it.
fn depth_32(d: &D3D12_RESOURCE_DESC) -> bool {
    matches!(
        d.Format,
        DXGI_FORMAT_R32_TYPELESS
            | DXGI_FORMAT_D32_FLOAT
            | DXGI_FORMAT_R32G8X24_TYPELESS
            | DXGI_FORMAT_D32_FLOAT_S8X24_UINT
    )
}

/// Whether `d` is twice `(w, h)` each way, give or take a texel (the
/// picture's depth against the reflections' image, of half its size).
fn twice(d: &D3D12_RESOURCE_DESC, (w, h): (u64, u32)) -> bool {
    d.Width.abs_diff(w * 2) <= 1 && u64::from(d.Height).abs_diff(u64::from(h) * 2) <= 1
}

impl State {
    /// The passes of the indirect-light estimate after a dispatch of `p`
    /// (`crate::ambient`), when its image and depth are found where the
    /// role says. Whether anything was recorded: then the list's compute
    /// pipeline and root signature are no longer the title's.
    pub(super) fn ambient(&self, list: &HostList, p: &HostPipeline, role: Role) -> bool {
        let Some(t) = p.stages.first().map(|s| s.translated()) else {
            return false;
        };
        // The miss program's first root constant: which surfaces its rays
        // are of.
        let layer = match role {
            Role::Miss => t
                .used
                .iter()
                .find(|u| u.root == RootType::Constants)
                .and_then(|u| match self.roots_ref(true).get(usize::from(u.slot)) {
                    Some(Root::Constants(c)) => c.first().copied(),
                    _ => None,
                })
                .unwrap_or(u32::MAX),
            _ => 0,
        };
        if role == Role::Miss
            && !(layer == MISS_OPAQUE || (crate::ambient::specular_alpha() && layer == MISS_ALPHA))
        {
            crate::ambient::note_miss(MissNote::OtherLayer);
            return false;
        }
        let Some(heap) = (self.heaps[0] != 0)
            .then(|| crate::descriptor::heap(self.heaps[0] as This).host())
            .flatten()
        else {
            return false;
        };
        // Where a table of root parameter `slot` starts in the heap, in
        // bytes: the one the bind made afresh, or the title's.
        let table_start = |slot: u8| match self.ambient_tables.iter().find(|m| m.0 == slot) {
            Some(m) => m.1,
            None => self.user_value(true, slot, RootType::Table, 0),
        };
        let view_at = |(slot, offset): (u8, u32)| {
            let Some(Root::Table(table)) = self.roots_ref(true).get(usize::from(slot)) else {
                return None;
            };
            crate::descriptor::view(table + u64::from(offset) * 4)
        };
        let entry = |kind: ResourceType, (slot, offset): (u8, u32)| -> Option<Found> {
            t.used.iter().find(|u| {
                u.kind == kind
                    && u.root == RootType::Table
                    && (u.slot, u.table_offset_dwords) == (slot, offset)
            })?;
            // A view of a texture the title placed in its memory names the
            // resource made for that memory.
            let view = view_at((slot, offset))?;
            let view = crate::descriptor::host::placed_texture(&view).unwrap_or(view);
            let resource = match view {
                View::Shader(r, _) | View::Unordered(r, ..) => r,
                _ => return None,
            };
            let host = crate::resource::host_resource(resource as This)?;
            // SAFETY: a live resource.
            let desc = unsafe { host.GetDesc() };
            Some((desc, heap.gpu_handle((table_start(slot) + offset * 4) / 32)))
        };
        let stencil_at = |at: (u8, u32)| stencil_view(view_at(at));
        // Variants of a program bind the image and the depth at other places
        // of their tables: the first entry of that kind that fits.
        let scan = |kind: ResourceType, fits: &dyn Fn(&D3D12_RESOURCE_DESC) -> bool| {
            t.used
                .iter()
                .filter(|u| u.kind == kind && u.root == RootType::Table)
                .filter(|u| {
                    kind != ResourceType::Srv || !stencil_at((u.slot, u.table_offset_dwords))
                })
                .filter_map(|u| entry(kind, (u.slot, u.table_offset_dwords)))
                .find(|(desc, _)| fits(desc))
        };
        let at = match role {
            Role::Resolve => LIGHT,
            Role::Radiance => RADIANCE,
            Role::Air => AIR,
            Role::Miss => MISS_LIGHT,
        };
        let image = match role {
            Role::Miss => entry(ResourceType::Uav, at).or_else(|| {
                scan(ResourceType::Uav, &|d| {
                    d.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE2D
                        && d.Format.0 == RADIANCE_FORMAT
                })
            }),
            _ => entry(ResourceType::Uav, at),
        };
        let image_size = image.as_ref().map(|(d, _)| (d.Width, d.Height));
        let usual_depth = |at: (u8, u32)| {
            if stencil_at(at) {
                once!(
                    ?role,
                    ?at,
                    "indirect-light estimate: the depth's place names a stencil plane; another depth view is taken"
                );
                None
            } else {
                entry(ResourceType::Srv, at)
            }
        };
        let depth = match role {
            Role::Resolve => usual_depth(DEPTH).or_else(|| {
                scan(ResourceType::Srv, &|d| {
                    depth_like(d) && image_size == Some((d.Width, d.Height))
                })
            }),
            Role::Miss => usual_depth(MISS_DEPTH)
                .filter(|(d, _)| image_size.is_some_and(|half| twice(d, half)))
                .or_else(|| {
                    scan(ResourceType::Srv, &|d| {
                        depth_like(d)
                            && depth_32(d)
                            && image_size.is_some_and(|half| twice(d, half))
                    })
                })
                .or_else(|| self.frame_depth(heap, image_size?, table_start(0))),
            Role::Radiance | Role::Air => image,
        };
        let (Some((of, image)), Some((seen, depth))) = (image, depth) else {
            once!(
                ?role,
                "no indirect-light estimate: its image or depth is not where it was looked for"
            );
            if role == Role::Miss {
                crate::ambient::note_miss(MissNote::NotReady);
            }
            return false;
        };
        let format = if role == Role::Resolve {
            LIGHT_FORMAT
        } else {
            RADIANCE_FORMAT
        };
        let volume = of.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE3D;
        let sizes = match role {
            // The reflections' image is of half the picture's size, its
            // depth (32-bit) of the whole.
            Role::Miss => {
                (depth_32(&seen) || seen.Format == DXGI_FORMAT_R32_FLOAT)
                    && seen.Width >= of.Width
                    && seen.Height >= of.Height
                    && seen.Width <= of.Width * 4
            }
            _ => (seen.Width, seen.Height) == (of.Width, of.Height),
        };
        if of.Format.0 != format || !sizes || volume != (role == Role::Air) {
            once!(
                ?role,
                format = of.Format.0,
                "no indirect-light estimate: another image than expected"
            );
            if role == Role::Miss {
                crate::ambient::note_miss(MissNote::NotReady);
            }
            return false;
        }
        let ran = crate::ambient::after(
            list,
            role,
            &Views {
                image,
                depth,
                size: [of.Width as u32, of.Height],
                slices: u32::from(of.DepthOrArraySize),
                depth_size: [seen.Width as u32, seen.Height],
            },
        );
        if ran && role == Role::Miss {
            crate::ambient::note_miss(MissNote::Ran(layer));
        }
        ran
    }

    /// The picture's depth from the frame's table (root parameter 0, which
    /// starts `start` bytes into `heap`) at a place the miss program does
    /// not read: some passes bind it none of their own. A depth-stencil
    /// texture's depth plane first, else any 32-bit depth twice `half`
    /// each way.
    fn frame_depth(
        &self,
        heap: &crate::descriptor::host::HostHeap,
        half: (u64, u32),
        start: u32,
    ) -> Option<Found> {
        /// Places of the table looked through, in dwords (8 a descriptor).
        const PLACES: u32 = 1600;
        let Some(Root::Table(table)) = self.roots_ref(true).first() else {
            return None;
        };
        let mut plain = None;
        for offset in (0..PLACES).step_by(8) {
            let Some(view) = crate::descriptor::view(table + u64::from(offset) * 4) else {
                continue;
            };
            if stencil_view(Some(view.clone())) {
                continue;
            }
            let view = crate::descriptor::host::placed_texture(&view).unwrap_or(view);
            let View::Shader(resource, _) = view else {
                continue;
            };
            let Some(host) = crate::resource::host_resource(resource as This) else {
                continue;
            };
            // SAFETY: a live resource.
            let d = unsafe { host.GetDesc() };
            if d.Dimension != D3D12_RESOURCE_DIMENSION_TEXTURE2D || !twice(&d, half) {
                continue;
            }
            let found = (d, heap.gpu_handle((start + offset * 4) / 32));
            match d.Format {
                DXGI_FORMAT_R32G8X24_TYPELESS | DXGI_FORMAT_D32_FLOAT_S8X24_UINT => {
                    return Some(found);
                }
                DXGI_FORMAT_R32_TYPELESS | DXGI_FORMAT_D32_FLOAT if plain.is_none() => {
                    plain = Some(found)
                }
                _ => {}
            }
        }
        plain
    }
}

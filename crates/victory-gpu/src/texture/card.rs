//! Uploads of title-written textures detiled on the card
//! (`crate::detile`, `VICTORY_GPU_DETILE=1`): which way an upload goes, its
//! copies from the card's ring, and its fill at submission.

use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use windows::Win32::Graphics::Direct3D12::*;

use super::{Side, Staging, staging_buffer, with_read};
use crate::host::{HostList, host};
use crate::object::This;

/// Which way a recorded upload goes.
pub(super) enum Route {
    /// Detiled on the card, from these slices.
    Card(Arc<crate::detile::Upload>),
    /// The CPU's way: the staging buffer is there.
    Cpu,
    /// Neither could be had now: nothing is recorded, the next list that
    /// samples the texture asks again.
    Nothing,
}

/// The way of an upload of `staging` about to be recorded. A staging state
/// with a staging buffer keeps it (the CPU's way; always so without
/// `crate::detile`); one without asks `reserve` for the card's slices, and
/// on any refusal has `make` make its staging buffer at once and goes the
/// CPU's way from then on. Never waits.
pub(super) fn route(
    staging: &Staging,
    reserve: impl FnOnce() -> Result<Arc<crate::detile::Upload>, crate::detile::Why>,
    make: impl FnOnce(u64) -> Option<Side>,
) -> Route {
    if staging.buffer().is_some() {
        return Route::Cpu;
    }
    match reserve() {
        Ok(upload) => Route::Card(upload),
        Err(why) => {
            crate::detile::refused(why);
            if staging.ensure_cpu(make) {
                Route::Cpu
            } else {
                Route::Nothing
            }
        }
    }
}

/// For [`super::upload`], once it has decided to record an upload: records
/// it from the card's ring if the texture is detiled there, and gives the
/// staging buffer to copy from if the upload goes the CPU's way. `None`:
/// recorded on the card, or nothing could be recorded.
pub(super) fn route_upload(
    list: &HostList,
    resource: This,
    texture: &ID3D12Resource,
    staging: &Arc<Staging>,
) -> Option<ID3D12Resource> {
    let routed = route(
        staging,
        || {
            crate::detile::reserve(
                &staging.layout,
                &staging.subs(),
                staging.layout.size,
                staging.bytes,
            )
        },
        |bytes| {
            let (buffer, data, pooled) = staging_buffer(host()?, bytes)?;
            Some(Side {
                buffer,
                data: data as usize,
                pooled,
            })
        },
    );
    match routed {
        Route::Card(upload) => {
            upload_on_card(list, resource, texture, staging.clone(), upload);
            None
        }
        Route::Nothing => None,
        Route::Cpu => staging.buffer().cloned(),
    }
}

/// Records `staging`'s upload from `upload`'s slice of the card's ring:
/// the copies here, the fill and the detile at the list's submission
/// ([`Staging::gpu_fill`]).
fn upload_on_card(
    list: &HostList,
    resource: This,
    texture: &ID3D12Resource,
    staging: Arc<Staging>,
    upload: Arc<crate::detile::Upload>,
) {
    let Some(out) = crate::detile::out_buffer() else {
        return;
    };
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(crate::copystat::C::TexUploads, 1);
    crate::framelog::add(crate::framelog::T::Uploads, 1);
    const COPY_DEST: u32 = 0x400;
    crate::list::ensure_state(list, resource, COPY_DEST);
    for (i, footprint) in staging.footprints.iter().enumerate() {
        let to = D3D12_TEXTURE_COPY_LOCATION {
            // SAFETY: a borrowed pointer the location never releases.
            pResource: unsafe { std::mem::transmute_copy(texture) },
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                SubresourceIndex: i as u32,
            },
        };
        let mut placed = *footprint;
        placed.Offset += upload.out_offset();
        let from = D3D12_TEXTURE_COPY_LOCATION {
            // SAFETY: as above; the ring lives for the run.
            pResource: unsafe { std::mem::transmute_copy(&out) },
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                PlacedFootprint: placed,
            },
        };
        // SAFETY: an open list; both locations name live resources.
        unsafe { list.list.CopyTextureRegion(&to, 0, 0, 0, &from, None) };
    }
    // As the CPU's upload: current from this list's submission on. The fill
    // does not say dirty (it is the upload); a later check that finds the
    // memory changed does.
    {
        let staging = staging.clone();
        list.defer_host(move || {
            staging.uploaded.store(true, Relaxed);
            staging.dirty.store(false, Relaxed);
        });
    }
    list.defer_gpu_texture(staging.clone(), upload);
    // Levels on demand: as the CPU's upload, after the fill judged the
    // levels.
    let on_demand = if staging.on_demand {
        crate::resource::host_tiled(resource)
    } else {
        None
    };
    if let Some(on_demand) = on_demand {
        list.defer_host(move || {
            let waiting = on_demand.uploaded(staging.detiled.load(Relaxed), staging.memory as u64);
            staging.waiting.store(
                if waiting {
                    crate::host::moment().max(1)
                } else {
                    0
                },
                Relaxed,
            );
            // A texture picked by index has no staging buffer after this
            // upload: its slots are made due again by the texture pass.
            if waiting {
                crate::descriptor::host::levels_waiting(staging.memory as u64);
            }
        });
    }
}

impl Staging {
    /// The staging buffer of a texture detiled on the card, made now by
    /// `make` for an upload that goes the CPU's way (`crate::detile` had no
    /// room or no table for it), and kept from then on, as without the
    /// switch. It holds nothing yet, so the next refresh detiles whatever
    /// the memory says. Whether there is one.
    pub(super) fn ensure_cpu(&self, make: impl FnOnce(u64) -> Option<Side>) -> bool {
        if self.buffer().is_some() {
            return true;
        }
        let Some(side) = make(self.bytes) else {
            return false;
        };
        let memory = self.memory as usize;
        crate::host::name(&side.buffer, || {
            format!("staging of {memory:#x} (detiled on the CPU after all)")
        });
        // Another thread may have given it one first: then this one is let
        // go, and theirs is it.
        if self.give(side) {
            self.forget();
            self.dirty.store(true, Relaxed);
        }
        true
    }

    /// The subresources of the staging layout, for `crate::detile::reserve`.
    pub(super) fn subs(&self) -> Vec<crate::detile::Sub> {
        self.footprints
            .iter()
            .zip(&self.rows)
            .enumerate()
            .map(|(i, (f, &rows))| crate::detile::Sub {
                mip: i as u32 % self.mips,
                slice: i as u32 / self.mips,
                offset: f.Offset,
                row_pitch: u64::from(f.Footprint.RowPitch),
                rows,
                depth: f.Footprint.Depth,
            })
            .collect()
    }

    /// At submission, for an upload detiled on the card: title memory as it
    /// is now goes into the upload's slice and its detile into the
    /// submission's first list, whether or not it changed since the last
    /// check (the list's copy reads this slice). Read as a refresh reads it
    /// (through the copy, which stands unmapped pages as zeros), and noted
    /// as a refresh notes it, so that the next check of the texture finds
    /// it as filled now. A list submitted again while its last submission
    /// may still read the slice leaves it as it is.
    pub(crate) fn gpu_fill(&self, upload: &Arc<crate::detile::Upload>) {
        if upload.in_flight() {
            crate::detile::reuse(upload);
            return;
        }
        let started = std::time::Instant::now();
        let (start, size) = (self.memory as usize, self.layout.size as usize);
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let asked = crate::watch::writes(start, start + size, false)
            .or_else(|| crate::watch::writes(start, start + size, true));
        if let Some((now, _)) = asked {
            last.asked = now;
        }
        with_read(size, |read| {
            crate::host::copy_mapped(read.as_mut_ptr(), self.memory, size, false);
            last.hash = Some(blake3::hash(read));
            let held = self.judge_levels();
            let expected = if crate::detile::check_due() {
                self.expected(read, held)
            } else {
                None
            };
            let raw = &read[..(upload.raw_len() as usize).min(size)];
            crate::detile::fill(upload, raw, held, expected);
        });
        drop(last);
        let preparing = &crate::host::PREPARING;
        preparing[2].fetch_add(started.elapsed().as_micros() as u64, Relaxed);
        preparing[3].fetch_add(size as u64, Relaxed);
    }

    /// What the card's detile of `read` must give
    /// (`VICTORY_GPU_DETILE_VERIFY`): the CPU's detile into a buffer laid
    /// out as the staging buffer, with the ranges that are the texture's
    /// (rows without their padding; the whole part for levels not held,
    /// which are zeros). `None` if the CPU cannot detile it.
    pub(super) fn expected(&self, read: &[u8], held: u32) -> Option<crate::detile::Expected> {
        let mut bytes = vec![0u8; self.bytes as usize];
        let mut ranges = Vec::new();
        let n = u64::from(self.layout.desc.element_bytes);
        for (i, (footprint, &rows)) in self.footprints.iter().zip(&self.rows).enumerate() {
            let (mip, slice) = (i as u32 % self.mips, i as u32 / self.mips);
            let f = footprint.Footprint;
            let pitch = f.RowPitch as usize;
            let at = footprint.Offset as usize;
            // The last subresource's last row ends at its bytes, not at a
            // whole pitch.
            let len =
                (pitch * rows as usize * f.Depth as usize).min(bytes.len().saturating_sub(at));
            if mip < 32 && held & (1 << mip) == 0 {
                ranges.push((at as u64, len as u64));
                continue;
            }
            self.layout
                .detile(
                    read,
                    mip,
                    slice,
                    bytes.get_mut(at..at + len)?,
                    pitch,
                    pitch * rows as usize,
                )
                .ok()?;
            let m = self.layout.mips.get(mip as usize)?;
            for z in 0..u64::from(m.depth) {
                for y in 0..u64::from(m.height) {
                    ranges.push((
                        at as u64 + z * (pitch as u64 * u64::from(rows)) + y * pitch as u64,
                        u64::from(m.width) * n,
                    ));
                }
            }
        }
        Some(crate::detile::Expected {
            bytes,
            ranges,
            what: format!("{:?} at {:#x}", self.layout.desc, self.memory as usize),
        })
    }
}

#[cfg(test)]
#[path = "card_tests.rs"]
mod tests;

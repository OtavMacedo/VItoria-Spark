//! Uploads of title-written textures recorded at submission, in the prelude
//! of the list that samples them ([`in_prelude`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering::Relaxed};

use windows::Win32::Graphics::Direct3D12::*;

use super::Staging;

/// 0: not read yet; 1: off; 2: on.
static IN_PRELUDE: AtomicU8 = AtomicU8::new(0);

/// Whether a title texture's upload is recorded when a list that samples it
/// is submitted, in that list's prelude (`VICTORY_TEXTURE_PRELUDE=1`; off
/// by default: the upload is recorded in the list itself).
///
/// A texture's memory is checked when a list that samples it is submitted,
/// and lists are recorded ahead of their submission. Recorded in a list, the
/// upload of what a check found reaches the card with the next list that is
/// recorded: after the lists of the same submission, a frame late where the
/// frame's lists were recorded by then, and in the order the lists were
/// recorded in, which is not the order they are submitted in. In the
/// prelude the upload stands right before the first list submitted that
/// samples the texture, with what the check of that submission found.
pub(crate) fn in_prelude() -> bool {
    match IN_PRELUDE.load(Relaxed) {
        0 => {
            let on = std::env::var_os("VICTORY_TEXTURE_PRELUDE").is_some_and(|v| v == "1");
            IN_PRELUDE.store(if on { 2 } else { 1 }, Relaxed);
            on
        }
        state => state == 2,
    }
}

/// Turns [`in_prelude`] on or off for a test, whatever the environment says.
#[cfg(test)]
pub(crate) fn set_in_prelude(on: bool) {
    IN_PRELUDE.store(if on { 2 } else { 1 }, Relaxed);
}

/// Whether the host texture must be uploaded now, given whether it ever was
/// (`uploaded`) and whether its staging buffer was detiled since (`dirty`).
/// The caller then records the upload: both are left as after it.
fn claim(uploaded: &AtomicBool, dirty: &AtomicBool) -> bool {
    let was_uploaded = uploaded.swap(true, Relaxed);
    let was_dirty = dirty.swap(false, Relaxed);
    !was_uploaded || was_dirty
}

/// A title texture a list samples, held by the list until it is reset: the
/// staging buffer, the host copy it is uploaded into, and for a host copy
/// made without memory (`crate::tiles`) what gives its levels theirs.
pub(crate) struct PreludeUpload {
    staging: Arc<Staging>,
    copy: Arc<crate::resource::HostTexture>,
    on_demand: Option<crate::resource::OnDemand>,
}

// SAFETY: the host texture and the staging buffer are free-threaded objects
// named only in list and queue calls; the flags are atomics.
unsafe impl Send for PreludeUpload {}
// SAFETY: as above.
unsafe impl Sync for PreludeUpload {}

impl PreludeUpload {
    pub(super) fn new(
        staging: Arc<Staging>,
        copy: Arc<crate::resource::HostTexture>,
        on_demand: Option<crate::resource::OnDemand>,
    ) -> Self {
        Self {
            staging,
            copy,
            on_demand,
        }
    }

    /// The staging buffer, for the submission's check of the texture.
    pub(crate) fn staging(&self) -> &Arc<Staging> {
        &self.staging
    }

    /// The host copy the upload writes.
    pub(crate) fn copy(&self) -> &Arc<crate::resource::HostTexture> {
        &self.copy
    }

    /// At submission, after the check of the texture's memory: whether the
    /// prelude of the list being submitted must record the upload. True
    /// once per change, whichever lists hold the texture. Levels on demand
    /// are given their memory here, ahead of the prelude on the queue.
    pub(crate) fn claim(&self) -> bool {
        if !claim(&self.staging.uploaded, &self.staging.dirty) {
            #[cfg(feature = "diagnostics")]
            crate::copystat::add(crate::copystat::C::TexSkipped, 1);
            return false;
        }
        #[cfg(feature = "diagnostics")]
        crate::copystat::add(crate::copystat::C::TexUploads, 1);
        crate::framelog::add(crate::framelog::T::Uploads, 1);
        if let Some(on_demand) = &self.on_demand {
            let memory = self.staging.memory as u64;
            let waiting = on_demand.uploaded(self.staging.detiled.load(Relaxed), memory);
            let since = if waiting {
                crate::host::moment().max(1)
            } else {
                0
            };
            self.staging.waiting.store(since, Relaxed);
            // A texture picked by index has no staging buffer after this
            // upload: its slots are made due again by the texture pass.
            if waiting {
                crate::descriptor::host::levels_waiting(memory);
            }
        }
        true
    }

    /// Records the copy of every subresource of the staging buffer into the
    /// host copy, which must be in the copy destination state. The copy
    /// reads the staging buffer when the card runs it: the list that holds
    /// `self` keeps the buffer alive until then.
    pub(crate) fn record(&self, list: &ID3D12GraphicsCommandList) {
        // Only a texture with a staging buffer is held for a prelude, and
        // the buffer stays as long as the staging state.
        let Some(buffer) = self.staging.buffer() else {
            return;
        };
        for (i, footprint) in self.staging.footprints.iter().enumerate() {
            let to = D3D12_TEXTURE_COPY_LOCATION {
                // SAFETY: a borrowed pointer the location never releases.
                pResource: unsafe { std::mem::transmute_copy(&self.copy.texture) },
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    SubresourceIndex: i as u32,
                },
            };
            let from = D3D12_TEXTURE_COPY_LOCATION {
                // SAFETY: as above.
                pResource: unsafe { std::mem::transmute_copy(buffer) },
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    PlacedFootprint: *footprint,
                },
            };
            // SAFETY: an open list; both locations name live resources.
            unsafe { list.CopyTextureRegion(&to, 0, 0, 0, &from, None) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::claim;
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

    #[test]
    fn an_upload_is_claimed_once_per_change() {
        let (uploaded, dirty) = (AtomicBool::new(false), AtomicBool::new(true));
        // The first list submitted uploads; the others of the submission
        // and the next frame's, with nothing changed, do not.
        assert!(claim(&uploaded, &dirty));
        assert!(!claim(&uploaded, &dirty));
        assert!(!claim(&uploaded, &dirty));
        // A check that detiled is followed by an upload at once, every time.
        for _ in 0..2 {
            dirty.store(true, Relaxed);
            assert!(claim(&uploaded, &dirty));
            assert!(!claim(&uploaded, &dirty));
        }
    }

    #[test]
    fn a_texture_never_uploaded_is_claimed_though_not_detiled() {
        let (uploaded, dirty) = (AtomicBool::new(false), AtomicBool::new(false));
        assert!(claim(&uploaded, &dirty));
        assert!(!claim(&uploaded, &dirty));
    }

    /// Three textures written in turn, one sampled a frame, one frame held:
    /// what the card holds of the texture sampled is always the frame last
    /// written there.
    #[test]
    fn a_ring_of_three_never_shows_an_old_frame() {
        let ring: [(AtomicBool, AtomicBool); 3] =
            std::array::from_fn(|_| (AtomicBool::new(false), AtomicBool::new(false)));
        let mut on_card = [0u32; 3];
        let frames = [
            (0, 1, true),
            (1, 2, true),
            (2, 3, true),
            (0, 4, true),
            (1, 5, true),
            (1, 5, false),
            (2, 6, true),
            (0, 7, true),
            (1, 8, true),
        ];
        for (slot, frame, written) in frames {
            let (uploaded, dirty) = &ring[slot];
            if written {
                dirty.store(true, Relaxed);
            }
            if claim(uploaded, dirty) {
                on_card[slot] = frame;
            }
            assert_eq!(on_card[slot], frame, "slot {slot}");
        }
    }
}

//! With `crate::texture::in_prelude`: the uploads of title-written textures
//! that a list's prelude records when the list is submitted.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use windows::Win32::Graphics::Direct3D12::*;

use super::{Batch, HostList, Step, transition};
use crate::texture::PreludeUpload;

impl HostList {
    /// The list samples a title-written texture: its memory is checked when
    /// the list is submitted, and the list's prelude uploads it if that
    /// check, or one before it, found it changed. Held until the list is
    /// reset.
    pub(crate) fn defer_upload(&self, upload: PreludeUpload) {
        let mut deferred = self.deferred();
        deferred
            .copies
            .push(Step::Texture(upload.staging().clone()));
        deferred.prelude_uploads.push(Arc::new(upload));
    }

    /// [`HostList::defer_texture`] for an upload detiled on the card
    /// (`crate::detile`): its slices are filled and detiled every time the
    /// list is submitted, and held until the list is reset.
    pub(crate) fn defer_gpu_texture(
        &self,
        staging: Arc<crate::texture::Staging>,
        upload: Arc<crate::detile::Upload>,
    ) {
        self.deferred()
            .copies
            .push(Step::GpuTexture(staging, upload));
    }

    /// After the list's submission-time steps: the textures its prelude
    /// must upload. A texture several lists hold is uploaded by the first
    /// one submitted.
    pub(super) fn claim_uploads(&self, batch: &mut Batch) -> Vec<Arc<PreludeUpload>> {
        let held = self.deferred().prelude_uploads.clone();
        if held.is_empty() {
            return held;
        }
        // The checks left for the helper threads say what changed.
        batch.check_textures();
        held.into_iter().filter(|upload| upload.claim()).collect()
    }
}

/// The moves of `uploads`' textures for a prelude: into the copy destination
/// state before its copies (`to_copy`), and for a texture the list keeps no
/// state of (`listed` false for its key) back to the state it was found in
/// (`barriers`). `copying` (by key: the address of the host copy) takes the
/// state each is in after the copies, from which the list's first state is
/// reached.
pub(super) fn moves(
    uploads: &[Arc<PreludeUpload>],
    listed: impl Fn(usize) -> bool,
    copying: &mut HashMap<usize, u32>,
    to_copy: &mut Vec<D3D12_RESOURCE_BARRIER>,
    barriers: &mut Vec<D3D12_RESOURCE_BARRIER>,
) {
    const COPY_DEST: u32 = 0x400;
    let host_state = |state: u32| D3D12_RESOURCE_STATES(state as i32);
    for upload in uploads {
        let copy = upload.copy();
        let key = Arc::as_ptr(copy) as usize;
        let real = copying
            .get(&key)
            .copied()
            .unwrap_or_else(|| copy.state.load(Ordering::Relaxed));
        copying.insert(key, COPY_DEST);
        if real == COPY_DEST {
            continue;
        }
        to_copy.push(transition(
            &copy.texture,
            host_state(real),
            host_state(COPY_DEST),
        ));
        if !listed(key) {
            barriers.push(transition(
                &copy.texture,
                host_state(COPY_DEST),
                host_state(real),
            ));
        }
    }
}

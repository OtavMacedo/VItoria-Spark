//! Letting go of the host copies of title memory the title gave up, at
//! once (`VICTORY_RELEASE_FREED=1`; off by default).
//!
//! A host copy of a window of title memory waited for the idle sweep
//! (`crate::shadow::let_go_idle`, ten seconds with plenty of the PC's
//! memory free, two with little), and a title buffer's copy lived as long
//! as the buffer, whatever happened to the memory under them. Unmapping
//! pages, mapping others in their place and freeing memory are how the
//! title says it is done with what was there; on a PC whose physical
//! memory runs out first, those copies are the PC's memory kept for
//! nothing. With the switch:
//!
//! - when the runtime unmaps or maps title memory
//!   (`crate::title_memory_remapped`), the copies of windows and of title
//!   buffers there that nothing holds go now ([`remapped`]);
//! - when the runtime frees a whole reservation of the title's
//!   ([`title_memory_released`]: `XMemFreeDefault` of 32-bit memory),
//!   what the GPU side noted of the range is forgotten as for an unmapping
//!   (views, placements, what is in place, the host copies of placed
//!   textures there that nothing holds) and the same copies go.
//!
//! A copy a list or a descriptor slot still holds stays, as before: the
//! card may still read it, and the sweep takes it once it is idle. Those
//! that go wait for the submissions running now (`crate::host::retire`).
//!
//! The switch may alternate inside a run (`SPARK_AB=VICTORY_RELEASE_FREED`,
//! `crate::switches`): every call decides on its own, and what went under
//! one arm is made again at its next use under either.
//!
//! After ZigZagBoi1/ScarlettPort pull request #370 (commit 745d6f36f),
//! whose `SCARLETT_FREED_COPIES=drop` does this for the title's own frees.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Since the start: calls of [`remapped`] with the switch on, and of
/// [`title_memory_released`] with it on.
static CALLS: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];

/// Whether the switch is on now (`VICTORY_RELEASE_FREED=1`).
pub fn release_freed() -> bool {
    crate::switches::RELEASE_FREED.on()
}

/// The runtime unmapped or mapped title memory `[at, at + len)`
/// (`crate::watch::title_memory_remapped`, after the views and placements
/// there are forgotten): with the switch, the copies there nothing holds go.
pub(crate) fn remapped(at: usize, len: usize) {
    if len == 0 || !release_freed() {
        return;
    }
    CALLS[0].fetch_add(1, Relaxed);
    crate::shadow::freed_let_go(at as u64, len as u64);
}

/// The runtime is about to free title memory `[at, at + len)` for good (a
/// whole reservation): with the switch, what the GPU side made of it is
/// forgotten and let go as for an unmapping (`crate::title_memory_remapped`
/// does the same for pages it unmaps), and the copies there nothing holds
/// go. The write watch's notes are left as they were without the switch:
/// they are the runtime's to clear when it hands the address out again
/// (`crate::title_memory_committed`).
pub fn title_memory_released(at: usize, len: usize) {
    if len == 0 || !release_freed() {
        return;
    }
    CALLS[1].fetch_add(1, Relaxed);
    crate::texback::remapped(at, len);
    crate::descriptor::host::remapped(at, len);
    crate::inplace::views_gone(at, len);
    crate::inplace::title_memory_freeing(at, len);
    crate::resource::evict_placed_in(at as u64, len as u64);
    crate::shadow::freed_let_go(at as u64, len as u64);
}

/// For the memory line: calls of [`remapped`] and of
/// [`title_memory_released`] with the switch on, and the copies let go and
/// their bytes, since the start. `None` while nothing was done.
#[cfg(feature = "diagnostics")]
pub(crate) fn counts() -> Option<[u64; 4]> {
    let c = [
        CALLS[0].load(Relaxed),
        CALLS[1].load(Relaxed),
        crate::shadow::freed::FREED_GONE[0].load(Relaxed),
        crate::shadow::freed::FREED_GONE[1].load(Relaxed),
    ];
    (c != [0; 4]).then_some(c)
}

//! `VICTORY_BIND_CACHE=1` (off by default): what a host list has bound
//! already is not given to it again.
//!
//! The title sets the same render targets and depth again and again; each
//! time the host made a view per target and called the driver
//! (`OMSetRenderTargets`). With the switch the views are compared with the
//! ones the host list was last given, by what they are made from (the
//! host texture, the title resource and the host view description: format,
//! dimension, level, slices, plane), and a call with the same ones makes
//! and sets nothing. A pipeline the list binds again is not noted by name
//! again either (`crate::host::HostList::used_pipeline`).
//!
//! The host copies of the remembered targets are held while they are
//! remembered: a held host texture cannot be freed, so no other can be
//! made at its address and pass for it. What is remembered goes when the
//! title list is reset, when a cut continues it on a new host list
//! (`crate::draw::State::replay`) and at the first call with the switch
//! off, so it may alternate inside a run (`SPARK_AB`): under either value
//! the host list ends up with the views the title set.
//!
//! LNX-victory 4dc1ac71 (after ZJOU acf478b8) and 4bf44ac9 (after Belobug
//! 92d353f8), where it is on by default.

#[cfg(feature = "diagnostics")]
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Whether the switch is on now.
pub(crate) fn on() -> bool {
    crate::switches::BIND_CACHE.on()
}

/// What one render target or depth view on a host list is made from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct ViewKey {
    /// The host texture (0: a typed null view).
    pub(crate) host: usize,
    /// The title resource the view names.
    pub(crate) title: usize,
    /// The host view description's words (`None`: the texture's own view).
    pub(crate) desc: Option<[u32; 6]>,
}

/// The render target and depth views a host list was last given, and what
/// keeps their host textures alive while they are remembered (`H`).
pub(crate) struct Targets<H> {
    set: Option<(Vec<ViewKey>, Option<ViewKey>)>,
    held: Vec<H>,
}

impl<H> Default for Targets<H> {
    fn default() -> Self {
        Targets {
            set: None,
            held: Vec::new(),
        }
    }
}

impl<H> Targets<H> {
    /// Nothing is remembered: the next views are set.
    pub(crate) fn forget(&mut self) {
        self.set = None;
        self.held.clear();
    }

    /// Whether the host list has exactly these views already, with the
    /// switch `on`: nothing is to be set. Otherwise false, and with `on`
    /// they are remembered as what it will have, `held` with them.
    pub(crate) fn same(
        &mut self,
        on: bool,
        colour: &[ViewKey],
        depth: Option<ViewKey>,
        held: Vec<H>,
    ) -> bool {
        if !on {
            self.forget();
            return false;
        }
        let same = self
            .set
            .as_ref()
            .is_some_and(|(c, d)| c.as_slice() == colour && *d == depth);
        #[cfg(feature = "diagnostics")]
        COUNTS[usize::from(same)].fetch_add(1, Relaxed);
        if !same {
            self.set = Some((colour.to_vec(), depth));
            self.held = held;
        }
        same
    }
}

/// Diagnostics: render target sets made (0) and left out (1), and pipeline
/// binds whose name was not noted again (2), since the last line.
#[cfg(feature = "diagnostics")]
static COUNTS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// Diagnostics: a pipeline bind whose name was not noted again.
#[cfg(feature = "diagnostics")]
pub(crate) fn name_skipped() {
    COUNTS[2].fetch_add(1, Relaxed);
}

/// Diagnostics: the line of what the switch left out, every 600 frames
/// while it counts anything.
#[cfg(all(windows, feature = "diagnostics"))]
pub(crate) fn line() {
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
    let [made, kept, names] = COUNTS.each_ref().map(|n| n.swap(0, Relaxed));
    if made + kept + names == 0 {
        return;
    }
    let frames = (frame - last).max(1) as f64;
    tracing::info!(
        "bind cache (VICTORY_BIND_CACHE) per frame over {frames:.0} frames: render targets set {:.0}, \
         left out as the host list has them {:.0}; pipeline names not noted again {:.0}",
        made as f64 / frames,
        kept as f64 / frames,
        names as f64 / frames,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn key(host: usize, format: u32, level: u32, slice: u32) -> ViewKey {
        ViewKey {
            host,
            title: host + 1,
            desc: Some([format, 4, level, slice, 0, 0]),
        }
    }

    #[test]
    fn the_same_views_are_found_and_others_are_not() {
        let mut t: Targets<()> = Targets::default();
        let colour = [key(0x1000, 28, 0, 0), key(0x2000, 10, 0, 0)];
        let depth = Some(key(0x3000, 40, 0, 0));
        assert!(!t.same(true, &colour, depth, Vec::new()));
        assert!(t.same(true, &colour, depth, Vec::new()));
        // Another format, level, slice, host texture, title resource, a
        // view of its own, another depth, none, fewer targets: all set.
        let changes = [
            key(0x1000, 29, 0, 0),
            key(0x1000, 28, 1, 0),
            key(0x1000, 28, 0, 1),
            key(0x1800, 28, 0, 0),
            ViewKey {
                title: 0x5000,
                ..colour[0]
            },
            ViewKey {
                desc: None,
                ..colour[0]
            },
        ];
        for changed in changes {
            let other = [changed, colour[1]];
            assert!(!t.same(true, &other, depth, Vec::new()), "{changed:?}");
            assert!(!t.same(true, &colour, depth, Vec::new()));
        }
        assert!(!t.same(true, &colour, Some(key(0x3000, 40, 1, 0)), Vec::new()));
        assert!(!t.same(true, &colour, None, Vec::new()));
        assert!(!t.same(true, &colour[..1], None, Vec::new()));
        assert!(t.same(true, &colour[..1], None, Vec::new()));
    }

    #[test]
    fn nothing_is_remembered_with_the_switch_off_or_after_a_reset() {
        let mut t: Targets<()> = Targets::default();
        let colour = [key(0x1000, 28, 0, 0)];
        assert!(!t.same(true, &colour, None, Vec::new()));
        // Off: set every time, and what was remembered goes.
        assert!(!t.same(false, &colour, None, Vec::new()));
        assert!(!t.same(false, &colour, None, Vec::new()));
        // On again: set once more before anything is left out.
        assert!(!t.same(true, &colour, None, Vec::new()));
        assert!(t.same(true, &colour, None, Vec::new()));
        // A reset or a new host list.
        t.forget();
        assert!(!t.same(true, &colour, None, Vec::new()));
    }

    #[test]
    fn remembered_textures_are_held_so_their_address_is_not_reused() {
        let mut t = Targets::default();
        let texture = Rc::new(0u8);
        let colour = [key(Rc::as_ptr(&texture) as usize, 28, 0, 0)];
        assert!(!t.same(true, &colour, None, vec![texture.clone()]));
        // Held by the cache: the title's release does not free it.
        assert_eq!(Rc::strong_count(&texture), 2);
        // A hit holds nothing more.
        assert!(t.same(true, &colour, None, vec![texture.clone()]));
        assert_eq!(Rc::strong_count(&texture), 2);
        // Other targets: the old texture is let go.
        let other = Rc::new(1u8);
        let others = [key(Rc::as_ptr(&other) as usize, 28, 0, 0)];
        assert!(!t.same(true, &others, None, vec![other.clone()]));
        assert_eq!(Rc::strong_count(&texture), 1);
        // Off: everything is let go.
        assert!(!t.same(false, &others, None, Vec::new()));
        assert_eq!(Rc::strong_count(&other), 1);
        // Only a texture no longer held can be freed, and then nothing
        // remembers it: one made at its address is set as new.
        drop(other);
        let reused = Rc::new(2u8);
        let at_same = [key(Rc::as_ptr(&reused) as usize, 28, 0, 0)];
        assert!(!t.same(true, &at_same, None, vec![reused.clone()]));
    }
}

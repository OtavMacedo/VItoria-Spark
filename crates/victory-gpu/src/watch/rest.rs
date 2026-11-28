//! Blocks the title has just opened by a write, left open for a moment
//! before the guards make them read-only again (`VICTORY_GUARD_REST`), and
//! blocks it writes all through a frame, left open for a few frames
//! (`VICTORY_GUARD_HOT`). Both off by default: without them no block rests
//! and the guards do byte for byte what they did before.
//!
//! Why: a refresh that finds a block written makes it read-only again
//! before its copy reads it, and the title, still writing it, faults at
//! once; the next refresh over the block, often within a millisecond, does
//! the same. In avatarmanz's VIctory-AMD-4K 220 of a frame's 586 faults
//! came less than 3 ms after the block was made read-only (run faults-1),
//! every one a `VirtualProtect` under the guards' lock and a fault that
//! waits for that lock; leaving the block open for 3 ms took the threads'
//! time in them from 69 to about 6 ms a frame (7900 XTX, 12900K).
//!
//! What is guaranteed, and how: no write of the title is lost.
//!
//! - A block rests only when a write fault has just opened it
//!   ([`Rests::opened`]): it is then out of the guards' `protected` and
//!   `clean`, and only [`super::protect_here`] puts a block back into
//!   `clean`, which a resting block never reaches (the guards skip it,
//!   [`super::guard_at`]).
//! - Every asking of the guards reports every block of its range that is
//!   not clean (nor a hole) as written, all its pages, whether it is then
//!   made read-only or left resting. So while a block rests every refresh
//!   over it is told it was written and its copy reads its bytes again,
//!   after the asking: a write before the asking is in what the copy
//!   reads, a write after it is reported by the next asking, the block
//!   being still not clean.
//! - When the rest is over the next asking reports the block written once
//!   more and makes it read-only before it returns, so the copy that
//!   follows reads every byte written before the protection, and every
//!   write after it faults and opens the block, as without the switch.
//! - Another view of the same physical page ([`super::protect`]): the
//!   guards still make every other view read-only when they make one
//!   clean, a resting one too (`protect_alias` does not look at rests), so
//!   a page clean at one view is read-only at all its views, as before.
//! - Nothing that answers without the guards trusts a resting block: the
//!   light refresh's bits ([`super::PUBLISHED`]) are set only for clean
//!   blocks, and `nothing_new`/`unchanged_known` ask for clean blocks.
//! - A block remapped by the runtime is forgotten here
//!   ([`Rests::forget`]): what it shows is new.
//!
//! So a resting block costs a copy of its 64 KiB at each refresh over it
//! (the copy compares and sends what changed) instead of a protection
//! change, a fault and a wait for the lock; the rests are kept short for
//! that reason.
//!
//! `VICTORY_GUARD_HOT` takes the selection of fxceless/victory-fork pull
//! request #1097 (`HOT_GUARD`, by RealAzemed, measured on an RTX 3070): a
//! block that faults [`HOT_FAULTS`] times or more within one frame of the
//! title is a ring or constant buffer written all through the frame, and
//! rests for the given number of frames, then is made read-only again and
//! must earn it anew. VIctory-AMD-4K's own `VICTORY_GUARD_HOT` (rests
//! doubling up to a ceiling for blocks written again soon after their
//! rest) measured worse there (run guard-6: 45.6 to 47.8 ms a frame) and
//! is not taken. Measured in pull request #1097 together with other
//! changes, never alone: off by default here too, and a switch of its own.
//!
//! Both are switches of the `SPARK_AB` table (`crate::switches`): every
//! asking and every fault reads them anew, and nothing kept is wrong under
//! the other value. When both are off the notes are dropped at the next
//! asking ([`Rests::drop_if_off`]); a block resting then is open and not
//! clean, which is what any block a fault opened is without the switches,
//! and the next asking over it makes it read-only as it would have. (Not
//! made read-only at the moment the switch goes off: that would be a
//! change of protection no code without the switch makes, of blocks no
//! refresh asked for.)

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Faults within one frame of the title that make a block hot (pull
/// request #1097's threshold).
pub(super) const HOT_FAULTS: u16 = 4;
/// Notes kept before the old ones are let go.
const MANY: usize = 1 << 13;

/// The switches as they stand for one asking or fault: microseconds a
/// block rests after a fault (0: none), frames a hot block rests (0: no
/// block is hot).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Cfg {
    pub(super) rest_us: u64,
    pub(super) hot_frames: u64,
}

impl Cfg {
    pub(super) fn on(self) -> bool {
        self.rest_us != 0 || self.hot_frames != 0
    }

    /// The switches' values in force.
    pub(super) fn now() -> Cfg {
        #[cfg(test)]
        if let Some(cfg) = IN_TEST.get() {
            return cfg;
        }
        Cfg {
            rest_us: crate::switches::GUARD_REST.get(),
            hot_frames: crate::switches::GUARD_HOT.get(),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// [`Cfg::now`] and [`frame`] for what this thread does (`None`: the
    /// switches, and the title's frame).
    pub(super) static IN_TEST: std::cell::Cell<Option<Cfg>> = const { std::cell::Cell::new(None) };
    pub(super) static FRAME_IN_TEST: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Microseconds since the first look: the clock of the rests.
pub(super) fn now_us() -> u64 {
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    EPOCH
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_micros() as u64
}

/// The title's frame, the clock of hot blocks.
pub(super) fn frame() -> u64 {
    #[cfg(test)]
    if let Some(frame) = FRAME_IN_TEST.get() {
        return frame;
    }
    crate::frame::current()
}

/// `VICTORY_GUARD_REST`: unset, `0` or not a number off; `1` the default
/// of 3,000 µs (VIctory-AMD-4K's); a number from 2 up those microseconds,
/// at most 100,000.
pub(crate) fn rest_us(value: Option<&str>) -> u64 {
    match value.and_then(|v| v.trim().parse::<u64>().ok()) {
        None | Some(0) => 0,
        Some(1) => 3000,
        Some(us) => us.min(100_000),
    }
}

/// `VICTORY_GUARD_HOT`: unset, `0` or not a number off; `1` the default of
/// 8 frames (pull request #1097's longest); a number from 2 up those
/// frames, at most 120.
pub(crate) fn hot_frames(value: Option<&str>) -> u64 {
    match value.and_then(|v| v.trim().parse::<u64>().ok()) {
        None | Some(0) => 0,
        Some(1) => 8,
        Some(frames) => frames.min(120),
    }
}

/// When a fault last opened a block, and how hot it is.
#[derive(Clone, Copy, Debug)]
struct Opened {
    /// [`now_us`] of the fault.
    at_us: u64,
    /// The frame of the faults counted in `faults`.
    frame: u64,
    faults: u16,
    /// The frame from which it no longer rests for being hot (0: never
    /// hot).
    hot_until: u64,
}

/// Why a block is left open now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Why {
    /// A fault opened it less than the rest ago.
    Rest,
    /// It is hot.
    Hot,
}

/// The blocks write faults opened, by block; held with the guards.
#[derive(Default)]
pub(super) struct Rests(BTreeMap<usize, Opened>);

impl Rests {
    pub(super) const fn new() -> Rests {
        Rests(BTreeMap::new())
    }

    /// A write fault opened `block` at `now_us` in `frame`.
    pub(super) fn opened(&mut self, block: usize, now_us: u64, frame: u64, cfg: Cfg) {
        if !cfg.on() {
            return;
        }
        if self.0.len() >= MANY {
            // Notes of faults some frames old, and not hot: what they
            // would say no longer holds. (Forgetting one only makes its
            // block read-only at the next asking, as without the switch.)
            self.0
                .retain(|_, o| o.frame.saturating_add(2) >= frame || o.hot_until > frame);
            if self.0.len() >= MANY {
                self.0.clear();
            }
        }
        let o = self.0.entry(block).or_insert(Opened {
            at_us: now_us,
            frame,
            faults: 0,
            hot_until: 0,
        });
        if o.frame != frame {
            o.frame = frame;
            o.faults = 0;
        }
        o.at_us = now_us;
        o.faults = o.faults.saturating_add(1);
        if cfg.hot_frames != 0 && o.faults >= HOT_FAULTS {
            o.faults = 0;
            o.hot_until = frame.saturating_add(cfg.hot_frames);
            COUNTS[HOT].fetch_add(1, Relaxed);
        }
    }

    /// Whether `block` is left open now, and why.
    pub(super) fn rests(&self, block: usize, now_us: u64, frame: u64, cfg: Cfg) -> Option<Why> {
        if !cfg.on() {
            return None;
        }
        let o = self.0.get(&block)?;
        if cfg.hot_frames != 0 && frame < o.hot_until {
            return Some(Why::Hot);
        }
        (cfg.rest_us != 0 && now_us.saturating_sub(o.at_us) < cfg.rest_us).then_some(Why::Rest)
    }

    /// The runtime remapped blocks `[first, last)`.
    pub(super) fn forget(&mut self, first: usize, last: usize) {
        if self.0.is_empty() || first >= last {
            return;
        }
        let gone: Vec<usize> = self.0.range(first..last).map(|(&b, _)| b).collect();
        for block in gone {
            self.0.remove(&block);
        }
    }

    /// With both switches off, the notes go.
    pub(super) fn drop_if_off(&mut self, cfg: Cfg) {
        if !cfg.on() && !self.0.is_empty() {
            self.0.clear();
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.0.len()
    }
}

/// Since they were last taken ([`take`]): blocks an asking reported written
/// and left open because a fault had just opened them, the same for hot
/// blocks, and blocks that became hot.
static COUNTS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
const RESTED: usize = 0;
const RESTED_HOT: usize = 1;
const HOT: usize = 2;

/// An asking left a block open, for `why`.
pub(super) fn left_open(why: Why) {
    COUNTS[match why {
        Why::Rest => RESTED,
        Why::Hot => RESTED_HOT,
    }]
    .fetch_add(1, Relaxed);
}

/// The line of [`COUNTS`], taken, per frame; none with both switches off
/// and nothing counted.
pub(crate) fn line(frames: f64) {
    let [rested, rested_hot, hot] = [RESTED, RESTED_HOT, HOT].map(|i| COUNTS[i].swap(0, Relaxed));
    let cfg = Cfg::now();
    if !cfg.on() && rested + rested_hot + hot == 0 {
        return;
    }
    let frames = frames.max(1.0);
    tracing::info!(
        "guards left open per frame (VICTORY_GUARD_REST={} us, VICTORY_GUARD_HOT={} frames): {:.1} blocks reported \
         written and left open after a fault, {:.1} left open for being hot, {:.2} blocks became hot",
        cfg.rest_us,
        cfg.hot_frames,
        rested as f64 / frames,
        rested_hot as f64 / frames,
        hot as f64 / frames
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const REST: Cfg = Cfg {
        rest_us: 3000,
        hot_frames: 0,
    };
    const BOTH: Cfg = Cfg {
        rest_us: 3000,
        hot_frames: 8,
    };
    const OFF: Cfg = Cfg {
        rest_us: 0,
        hot_frames: 0,
    };

    #[test]
    fn values_read_as_the_switches_document() {
        assert_eq!(rest_us(None), 0);
        assert_eq!(rest_us(Some("0")), 0);
        assert_eq!(rest_us(Some("x")), 0);
        assert_eq!(rest_us(Some("1")), 3000);
        assert_eq!(rest_us(Some(" 500 ")), 500);
        assert_eq!(rest_us(Some("9999999")), 100_000);
        assert_eq!(hot_frames(None), 0);
        assert_eq!(hot_frames(Some("1")), 8);
        assert_eq!(hot_frames(Some("4")), 4);
        assert_eq!(hot_frames(Some("1000")), 120);
    }

    #[test]
    fn a_block_rests_for_the_rest_after_its_fault_and_not_after() {
        let mut r = Rests::new();
        assert_eq!(r.rests(7, 0, 1, REST), None, "never opened");
        r.opened(7, 1000, 1, REST);
        assert_eq!(r.rests(7, 1000, 1, REST), Some(Why::Rest));
        assert_eq!(r.rests(7, 3999, 1, REST), Some(Why::Rest));
        assert_eq!(r.rests(7, 4000, 1, REST), None);
        assert_eq!(r.rests(8, 1000, 1, REST), None, "another block");
        // The switch off: nothing rests, whatever is noted.
        assert_eq!(r.rests(7, 1000, 1, OFF), None);
        // A new fault starts a new rest.
        r.opened(7, 5000, 1, REST);
        assert_eq!(r.rests(7, 7000, 1, REST), Some(Why::Rest));
    }

    #[test]
    fn nothing_is_noted_with_the_switches_off_and_notes_go_when_they_are() {
        let mut r = Rests::new();
        r.opened(7, 1000, 1, OFF);
        assert_eq!(r.len(), 0);
        r.opened(7, 1000, 1, REST);
        r.drop_if_off(REST);
        assert_eq!(r.len(), 1);
        r.drop_if_off(OFF);
        assert_eq!(r.len(), 0);
        assert_eq!(r.rests(7, 1000, 1, REST), None);
    }

    #[test]
    fn a_block_faulting_four_times_in_a_frame_is_hot_for_its_frames() {
        let mut r = Rests::new();
        // Three faults in a frame and one in the next: not hot.
        for at in [0, 10_000, 20_000] {
            r.opened(3, at, 5, BOTH);
        }
        r.opened(3, 30_000, 6, BOTH);
        assert_eq!(r.rests(3, 40_000, 6, BOTH), None);
        // Four in one frame: hot until frame 7 + 8.
        for at in [50_000, 60_000, 70_000] {
            r.opened(3, at, 7, BOTH);
        }
        assert_eq!(r.rests(3, 80_000, 7, BOTH), None);
        r.opened(3, 90_000, 7, BOTH);
        assert_eq!(r.rests(3, 90_000, 7, BOTH), Some(Why::Hot));
        assert_eq!(r.rests(3, 900_000, 14, BOTH), Some(Why::Hot));
        assert_eq!(r.rests(3, 900_000, 15, BOTH), None);
        // Hot only with its switch: the rest alone after the fault.
        assert_eq!(r.rests(3, 90_000, 8, REST), Some(Why::Rest));
        assert_eq!(r.rests(3, 900_000, 8, REST), None);
    }

    #[test]
    fn remapped_blocks_are_forgotten_and_old_notes_let_go() {
        let mut r = Rests::new();
        for block in 10..20 {
            r.opened(block, 0, 1, REST);
        }
        r.forget(12, 15);
        assert_eq!(r.len(), 7);
        assert_eq!(r.rests(12, 0, 1, REST), None);
        assert_eq!(r.rests(15, 0, 1, REST), Some(Why::Rest));
        let mut r = Rests::new();
        for block in 0..MANY {
            r.opened(block, 0, 1, REST);
        }
        // Frames later the old notes go, the new one stays.
        r.opened(MANY, 50_000, 9, REST);
        assert_eq!(r.len(), 1);
        assert_eq!(r.rests(MANY, 50_000, 9, REST), Some(Why::Rest));
    }
}

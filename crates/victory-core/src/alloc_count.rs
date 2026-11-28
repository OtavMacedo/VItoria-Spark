//! `VICTORY_COUNT_ALLOCS`: whether the process's counting allocator
//! (`victory-gpu`'s `copystat::Counting`) counts our own blocks. On unless
//! the variable is `0`; with `0` it hands every call to the allocator under
//! it and touches no counter, and the memory lines say that our own
//! allocations were not counted (instead of showing zeros).
//!
//! An allocator must not read the environment (reading allocates, and so
//! calls the allocator again), so the variable is read once by [`init`] at
//! the start of `main`. Until then the state is unknown and blocks are
//! counted, as they always were; after it the state never changes again.
//!
//! Not a `SPARK_AB` switch: a block counted when made and not when
//! released (or the other way round) would leave the totals wrong for the
//! rest of the run, so it cannot alternate. Comparing it takes separate
//! runs.

use std::sync::atomic::{AtomicU8, Ordering::Relaxed};

/// The variable.
pub const VAR: &str = "VICTORY_COUNT_ALLOCS";

const UNKNOWN: u8 = 0;
const ON: u8 = 1;
const OFF: u8 = 2;

/// The state, on a line of the processor's memory of its own: every
/// allocation of the process reads it, and nothing beside it should be
/// written next to it.
#[repr(align(64))]
pub struct Switch(AtomicU8);

impl Switch {
    /// Unknown: counting until it is decided.
    pub const fn new() -> Switch {
        Switch(AtomicU8::new(UNKNOWN))
    }

    /// Whether blocks are counted now (unknown counts).
    #[inline]
    pub fn counting(&self) -> bool {
        self.0.load(Relaxed) != OFF
    }

    /// The decision, once there is one.
    pub fn decided(&self) -> Option<bool> {
        match self.0.load(Relaxed) {
            ON => Some(true),
            OFF => Some(false),
            _ => None,
        }
    }

    /// Decides from the variable's value (`None`: unset); a state already
    /// decided stays as it is. Returns the state in force.
    pub fn decide(&self, value: Option<&str>) -> bool {
        let state = if wanted(value) { ON } else { OFF };
        let _ = self.0.compare_exchange(UNKNOWN, state, Relaxed, Relaxed);
        self.counting()
    }
}

impl Default for Switch {
    fn default() -> Switch {
        Switch::new()
    }
}

/// Whether a value of the variable asks for counting: anything but `0`.
pub fn wanted(value: Option<&str>) -> bool {
    value.is_none_or(|v| v.trim() != "0")
}

/// The process's switch.
pub static STATE: Switch = Switch::new();

/// Reads the variable into [`STATE`]; call once, early in `main`.
pub fn init() -> bool {
    STATE.decide(std::env::var(VAR).ok().as_deref())
}

/// Whether our own blocks are counted now.
#[inline]
pub fn counting() -> bool {
    STATE.counting()
}

#[cfg(test)]
mod tests {
    use super::{Switch, wanted};

    #[test]
    fn only_zero_turns_counting_off() {
        assert!(wanted(None));
        assert!(wanted(Some("1")));
        assert!(wanted(Some("")));
        assert!(wanted(Some("yes")));
        assert!(!wanted(Some("0")));
        assert!(!wanted(Some(" 0 ")));
    }

    #[test]
    fn unknown_counts_until_decided() {
        let s = Switch::new();
        assert!(s.counting());
        assert_eq!(s.decided(), None);
    }

    #[test]
    fn off_stops_counting() {
        let s = Switch::new();
        assert!(!s.decide(Some("0")));
        assert!(!s.counting());
        assert_eq!(s.decided(), Some(false));
    }

    #[test]
    fn unset_decides_on() {
        let s = Switch::new();
        assert!(s.decide(None));
        assert!(s.counting());
        assert_eq!(s.decided(), Some(true));
    }

    /// The first decision holds: a block's release is counted the way its
    /// making was, for every block made after it.
    #[test]
    fn the_first_decision_holds() {
        let s = Switch::new();
        s.decide(Some("0"));
        assert!(!s.decide(Some("1")));
        assert!(!s.counting());
        let t = Switch::new();
        t.decide(Some("1"));
        assert!(t.decide(Some("0")));
        assert_eq!(t.decided(), Some(true));
    }
}

//! Write faults that open a guarded block without the guards' lock
//! (`VICTORY_FAULT_UNLOCKED=1`; off by default).
//!
//! Why: every write fault took the guards' lock ([`super::GUARDS`]) to
//! open its block, and the refreshes of host copies hold it while they
//! make blocks read-only again. On an RTX 3070 the threads waited 30 to
//! 36 ms a frame for it (the lock's counts, `lockstat`); on an RTX 5080
//! (VIctory-NVIDIA) it was held 49.5 ms a frame and the faults waited 43.6
//! ms, while a fault's own work is a few microseconds.
//!
//! With the switch a fault on a block we made read-only ([`PROTECTED`])
//! that no other view is guarded with ([`ALIASED`]) does this, with no lock
//! of the guards:
//!
//! 1. marks the block in [`OPENING`] (a fault that finds it marked already
//!    waits for that one, 100 microseconds at most, then goes the locked
//!    way);
//! 2. looks at [`ALIASED`]: if the block is guarded with other views, it
//!    takes its mark back and goes the locked way, which opens them all;
//! 3. clears the block's bit of the light refresh ([`super::PUBLISHED`]);
//! 4. gives the block its write access back (`VirtualProtect`);
//! 5. queues the block for the guards' notes ([`OPENED`]).
//!
//! The next holder of the lock, whoever it is, first brings the notes up
//! to date ([`settle`]): the block is no longer read-only nor clean, it
//! rests if the rests are on ([`super::rest`]), and its mark goes. A block
//! guarded together with other views, or one whose protection could not
//! be changed, goes the locked way, as without the switch.
//!
//! What is guaranteed, and why.
//!
//! No write of the title is lost. From the fault's mark (1) until the
//! settling that takes it away, every asking of the guards reports the
//! block written and leaves it as it is (`guard_at`: not made read-only,
//! not noted clean), [`super::publish`] gives it no bit, and
//! `unchanged_known` and `clean` do not count it clean. The settling
//! happens under the lock, after the fault's `VirtualProtect` returned
//! (the fault queues the block after it), and leaves the block noted as
//! any opened block is: open and not clean. So at every moment a write
//! that lands in the block (after step 4) is reported by every asking
//! that begins after it: before the settling by the mark, after it by the
//! notes, which a later asking turns into "read-only again" only after
//! reporting the block.
//!
//! No block is made read-only while a fault opens it. Only holders of the
//! lock make blocks read-only (`protect_here`, `protect_alias`), and they
//! look at the mark first, under the lock; the mark is taken away only
//! under the lock (settling) or by the fault itself before it changed any
//! protection (step 2, or a failed step 4). A guard made just before the
//! mark is taken by the fault's `VirtualProtect` and noted open at the
//! settling.
//!
//! Views guarded together. The fault marks its block (1) and then looks at
//! [`ALIASED`] (2); the guards note the views of a page together, setting
//! their [`ALIASED`] bits, and then look at the marks of all of them
//! (`protect`). Every mark and bit is read and written `SeqCst`, so of the
//! two one sees the other's (the classic store-then-load pairing that
//! needs a single order): either the fault goes the locked way, or the
//! guards open every view of the page under the lock, as a fault under the
//! lock would have. Either way no view is left clean while another view of
//! its page is writable.
//!
//! The light refresh. The fault clears the block's bit (3) after its mark
//! (1) and before the block is writable (4); [`super::publish`] sets a bit
//! only if the mark is not there and looks again after setting it, clearing
//! it if the mark came meanwhile. Both are `SeqCst`: the bit cannot stay set
//! for a marked block. An asking answered by the bits holds the journal's
//! lock as before; a write after the bit is cleared is one that comes after
//! such an asking.
//!
//! The queue ([`OPENED`]) is a mutex the fault holds only to push, never
//! with the guards' lock; [`TO_SETTLE`] is a hint for the takers of the
//! lock. Nothing above depends on how soon the settling comes: until then
//! the block is reported written at every asking.
//!
//! What it costs: the marks, [`PROTECTED`] and [`ALIASED`] are kept only in
//! a run where the switch is on at some time ([`kept`]); without it the
//! guards do byte for byte what they did, with a load of a flag more.
//!
//! In the `SPARK_AB` table: each fault reads the switch anew, and what one
//! arm leaves behind (marks, queued blocks) is honoured under the other:
//! the marks are looked at whenever they are kept, and any holder of the
//! lock settles the queue.
//!
//! After VIctory-NVIDIA 3f550568 (readmet3xt-lab) and pull request #516,
//! by hand: here a block guarded with other views goes the locked way (the
//! guards open every view of a written page; theirs tell the others as
//! written without opening them).

use super::{BLOCK, Bits, Guards, PUBLISHED, rest};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed, Ordering::SeqCst};

/// A mirror of `Guards::protected` for the fault, which takes no lock:
/// set after the block is made read-only, cleared when it leaves the set.
/// A hint only: a fault that does not find the bit goes the locked way.
static PROTECTED: Bits = Bits::new();

/// The blocks a write fault is opening, or has opened, without the guards'
/// lock, until the next holder of the lock has settled them ([`settle`]).
static OPENING: Bits = Bits::new();

/// A mirror of `Guards::aliased`: blocks whose page is guarded together
/// with its other views. Set before the guards look at the views' marks.
static ALIASED: Bits = Bits::new();

/// A block a fault opened without the lock: when, for its rest.
struct Opened {
    block: usize,
    at_us: u64,
    frame: u64,
}

/// The blocks to settle, and how many (a hint for the takers of the lock).
static OPENED: Mutex<Vec<Opened>> = Mutex::new(Vec::new());
static TO_SETTLE: AtomicUsize = AtomicUsize::new(0);

/// How long a fault waits for another opening the same block, in
/// microseconds, before it goes the locked way.
const WAIT_US: u128 = 100;

#[cfg(test)]
thread_local! {
    /// The switch for this thread's faults (`None`: the switch).
    pub(super) static IN_TEST: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Whether the marks and mirrors are kept: the switch is on at some time
/// of the run (in tests, always).
fn kept() -> bool {
    cfg!(test) || crate::switches::FAULT_UNLOCKED.ever()
}

/// Whether this fault takes no lock.
fn on() -> bool {
    #[cfg(test)]
    if let Some(on) = IN_TEST.get() {
        return on;
    }
    crate::switches::FAULT_UNLOCKED.on()
}

/// Read before the handler can run: reading the environment allocates.
pub(super) fn prepare() {
    crate::switches::FAULT_UNLOCKED.get();
    if kept() {
        OPENED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reserve(4096);
    }
}

/// `block` was made read-only and noted in `Guards::protected`.
pub(super) fn protected(block: usize) {
    if kept() {
        // (So that the fault's mark allocates nothing.)
        OPENING.make(block);
        PROTECTED.set(block);
    }
}

/// `block` left `Guards::protected`.
pub(super) fn unprotected(block: usize) {
    if kept() {
        PROTECTED.clear(block);
    }
}

/// `block` was noted in `Guards::aliased` (before its views' marks are
/// looked at: [`any_of`]).
pub(super) fn aliased(block: usize) {
    if kept() {
        ALIASED.set(block);
    }
}

/// `block` left `Guards::aliased`.
pub(super) fn unaliased(block: usize) {
    if kept() {
        ALIASED.clear(block);
    }
}

/// Whether a fault is opening `block` without the lock, not settled yet.
pub(super) fn opening(block: usize) -> bool {
    kept() && OPENING.get(block)
}

/// Whether one of `[first, last)` is.
pub(super) fn any_opening(first: usize, last: usize) -> bool {
    kept() && OPENING.any(first, last)
}

/// Whether one of the views `shown` (addresses) is.
pub(super) fn any_of(shown: &[usize]) -> bool {
    kept() && shown.iter().any(|&at| OPENING.get(at / BLOCK))
}

/// Since the last line: faults opened without the lock; that went the
/// locked way for another view, for a fault opening the same block (after
/// waiting), for a protection that could not be changed; that found the
/// block opened by another while waiting; blocks settled; askings that
/// left a marked block alone; pages whose views were opened because a
/// fault was opening one of them.
static COUNTS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
const OPENED_N: usize = 0;
const BY_VIEWS: usize = 1;
const BY_BUSY: usize = 2;
const BY_FAILED: usize = 3;
const WAITED: usize = 4;
const SETTLED: usize = 5;
const LEFT: usize = 6;
const GROUPS: usize = 7;

/// An asking left a marked block alone.
pub(super) fn left_alone() {
    COUNTS[LEFT].fetch_add(1, Relaxed);
}

/// The guards opened every view of a page for a fault opening one.
pub(super) fn group_opened() {
    COUNTS[GROUPS].fetch_add(1, Relaxed);
}

/// What became of an opening without the lock.
enum Outcome {
    Opened,
    Busy,
    Views,
    Failed,
}

/// For the handler: a write fault at `at`. True if its block was opened
/// without the lock (or by another fault meanwhile) and the write can go
/// on; false to go the locked way, as without the switch.
pub(super) fn fault(at: usize) -> bool {
    let block = at / BLOCK;
    if !on() || !PROTECTED.get(block) {
        return false;
    }
    match open(block) {
        Outcome::Opened => {
            COUNTS[OPENED_N].fetch_add(1, Relaxed);
            #[cfg(feature = "diagnostics")]
            crate::copystat::add(crate::copystat::C::GuardFaults, 1);
            crate::framelog::add(crate::framelog::T::GuardFaults, 1);
            true
        }
        Outcome::Busy => {
            // Another fault opens it: writable in a few microseconds.
            let began = std::time::Instant::now();
            while OPENING.get(block) && began.elapsed().as_micros() < WAIT_US {
                if super::writable(at) {
                    COUNTS[WAITED].fetch_add(1, Relaxed);
                    return true;
                }
                for _ in 0..64 {
                    std::hint::spin_loop();
                }
            }
            COUNTS[BY_BUSY].fetch_add(1, Relaxed);
            false
        }
        Outcome::Views => {
            COUNTS[BY_VIEWS].fetch_add(1, Relaxed);
            false
        }
        Outcome::Failed => {
            COUNTS[BY_FAILED].fetch_add(1, Relaxed);
            false
        }
    }
}

/// Steps 1 to 5 of the module's comment.
fn open(block: usize) -> Outcome {
    use windows_sys::Win32::System::Memory::{PAGE_READONLY, PAGE_READWRITE, VirtualProtect};
    if OPENING.swap_on(block) {
        return Outcome::Busy;
    }
    // (After the mark: see the module's comment.)
    if ALIASED.get(block) {
        OPENING.clear(block);
        return Outcome::Views;
    }
    // Before the block can be written.
    PUBLISHED.clear(block);
    let (at_us, frame) = (rest::now_us(), rest::frame());
    let mut old = 0;
    // SAFETY: title view memory we made read-only; only its protection
    // changes.
    let mut done =
        unsafe { VirtualProtect((block * BLOCK) as *const _, BLOCK, PAGE_READWRITE, &mut old) }
            != 0;
    if done && !matches!(old, PAGE_READONLY | PAGE_READWRITE) {
        // Not what we left there: memory mapped anew meanwhile keeps its
        // protection, and the locked way decides.
        // SAFETY: as above.
        unsafe { VirtualProtect((block * BLOCK) as *const _, BLOCK, old, &mut old) };
        done = false;
    }
    if !done {
        // No protection changed: as if never marked (the bit of the light
        // refresh stays cleared, which costs an asking of the guards).
        OPENING.clear(block);
        return Outcome::Failed;
    }
    let mut queue = OPENED.lock().unwrap_or_else(|e| e.into_inner());
    queue.push(Opened {
        block,
        at_us,
        frame,
    });
    TO_SETTLE.store(queue.len(), SeqCst);
    Outcome::Opened
}

/// Brings into the guards' notes the blocks faults opened without their
/// lock: as `open` does for a block a write fault opened, but for its
/// protection, which is changed already. Done by whoever takes the lock,
/// before anything else (`guards_by`).
pub(super) fn settle(g: &mut Guards) {
    if TO_SETTLE.load(SeqCst) == 0 {
        return;
    }
    let opened: Vec<Opened> = {
        let mut queue = OPENED.lock().unwrap_or_else(|e| e.into_inner());
        TO_SETTLE.store(0, SeqCst);
        queue.drain(..).collect()
    };
    let cfg = rest::Cfg::now();
    for Opened {
        block,
        at_us,
        frame,
    } in opened
    {
        // Noted together with other views after the fault looked: the
        // guards opened them then (`protect`). Should one be noted still,
        // they are opened now, as under the lock.
        if g.aliased.contains(&block) {
            super::open(g, block * BLOCK, (block + 1) * BLOCK);
        }
        if g.protected.remove(&block) {
            PROTECTED.clear(block);
            g.clean.remove(&block);
            PUBLISHED.clear(block);
        }
        // Left open for a moment, as after a fault under the lock.
        if cfg.on() {
            g.rests.opened(block, at_us, frame, cfg);
        }
        COUNTS[SETTLED].fetch_add(1, Relaxed);
        // Last: the notes say open now.
        OPENING.clear(block);
    }
}

/// The line of [`COUNTS`], taken, per frame; none when the switch is
/// never on in the run.
pub(super) fn line(frames: f64) {
    let n: [u64; 8] = std::array::from_fn(|i| COUNTS[i].swap(0, Relaxed));
    if !crate::switches::FAULT_UNLOCKED.ever() {
        return;
    }
    let f = |i: usize| n[i] as f64 / frames.max(1.0);
    tracing::info!(
        "write faults without the guards' lock per frame (VICTORY_FAULT_UNLOCKED={}): {:.1} opened so, {:.1} found it opened by another, \
         went the locked way {:.1} (guarded with other views) {:.1} (another opening it) {:.1} (protection not changed); \
         {:.1} settled; {:.1} askings left a block being opened alone; {:.2} pages opened at every view for one",
        crate::switches::FAULT_UNLOCKED.get(),
        f(OPENED_N),
        f(WAITED),
        f(BY_VIEWS),
        f(BY_BUSY),
        f(BY_FAILED),
        f(SETTLED),
        f(LEFT),
        f(GROUPS)
    );
}

#[cfg(test)]
#[path = "unlocked_tests.rs"]
mod tests;

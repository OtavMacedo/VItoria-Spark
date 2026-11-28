//! A host copy's two notes that every refresh looks at, and how they are
//! looked at with `VICTORY_NOTHING_NEW_FAST=1` (off by default): the
//! ranges shaders of a submission not yet copied back may be writing
//! ([`Pending`]), and the asking after which each page was last copied
//! ([`Copied`]).
//!
//! Without the switch every refresh of a copy takes the lock of its
//! pending ranges to hear, nearly always, that there are none, and the
//! lock of its pages' notes, alone, to ask the journal whether anything is
//! new (`Shadow::nothing_new`); the refreshes of a submission run on
//! several threads (`VICTORY_WIDE_REFRESH`, `VICTORY_WIDE_THREADS`) and the
//! big copies are refreshed from many of them at once. With the switch,
//! the first is a flag read without a lock, and the second a lock shared
//! by every reader. The flag is kept either way (one store under the lock,
//! and only when it changes) and without the switch the readers take the
//! notes' lock for writing, which excludes as a mutex does, so the switch
//! keeps nothing and may change between any two looks (`SPARK_AB`).
//!
//! The flag, and why it is enough. [`Pending::lock`] hands out the ranges
//! under their mutex, and the hand-out stores `!ranges.is_empty()` in the
//! flag, with `Release`, before the mutex is let go, so every change of
//! the ranges (both pushes of `Shadow::writing`, the retain of
//! `Shadow::copy_back_done`, anything to come) moves the flag with it, and
//! the flag is never stored without the mutex: its order of values is the
//! order of the lock's holders. A reader that loads it with `Acquire`
//! therefore sees the ranges as one of the holders left them:
//!
//! - `false` read: the ranges were empty when the last holder whose store
//!   it reads let them go, and the `Acquire` makes everything that holder
//!   did before (a copy-back's writes into title memory and into the copy,
//!   then `copy_back_done`) visible to the reader, as taking the mutex
//!   would. A push being made at that moment is one the reader would miss
//!   by taking the lock just before its writer: with the mutex too, the
//!   answer holds only for the moment it is given. A push that
//!   happens-before the reader (the submitting thread declares the range,
//!   then hands the refresh to the reading thread) is seen: its store
//!   happens-before the load, so the load reads it or a later value.
//! - `true` read: the reader takes the mutex and reads the ranges, as it
//!   does without the switch.
//!
//! Nothing here asks the guards (`crate::watch::GUARDS`): the light
//! refresh's answer (`crate::watch::nothing_new`) reads the journal and the
//! guards' bits, which `VICTORY_GUARD_REST` and `VICTORY_UNGUARD_RUNS` set
//! and clear as they did (a bit only for a clean block; cleared before a
//! block is opened). What changes is only who may hold the copy's notes at
//! once while it asks: readers of the notes still exclude every writer, so
//! each reader sees the notes of one moment, as with the mutex.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Whether the notes are looked at without the locks taken alone
/// (`VICTORY_NOTHING_NEW_FAST=1`).
fn fast() -> bool {
    crate::switches::NOTHING_NEW_FAST.on()
}

/// Since the last line: looks at the pending ranges that the flag answered
/// with no lock, looks that took the lock, and readers of the pages' notes
/// that shared their lock.
static LOOKS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// The ranges shaders of a submission not yet copied back may be writing,
/// with a flag that says whether there are any.
#[derive(Default)]
pub(super) struct Pending {
    ranges: Mutex<Vec<(usize, usize)>>,
    /// `!ranges.is_empty()` as the last holder of `ranges` left them;
    /// stored only under their lock ([`Held`]'s drop).
    any: AtomicBool,
}

/// [`Pending`]'s ranges, locked. Letting go of it moves the flag.
pub(super) struct Held<'a> {
    ranges: MutexGuard<'a, Vec<(usize, usize)>>,
    any: &'a AtomicBool,
}

impl std::ops::Deref for Held<'_> {
    type Target = Vec<(usize, usize)>;
    fn deref(&self) -> &Self::Target {
        &self.ranges
    }
}

impl std::ops::DerefMut for Held<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.ranges
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        // (Runs before the field, the mutex's guard, is let go. The
        // relaxed load reads the last store: every store is made under
        // this lock.)
        let any = !self.ranges.is_empty();
        if self.any.load(Ordering::Relaxed) != any {
            self.any.store(any, Ordering::Release);
        }
    }
}

impl Pending {
    /// The ranges, locked.
    pub(super) fn lock(&self) -> Held<'_> {
        Held {
            ranges: self.ranges.lock().unwrap_or_else(|e| e.into_inner()),
            any: &self.any,
        }
    }

    /// Whether there may be ranges: false only when the switch is on and
    /// the flag says there are none, so that the caller need not take the
    /// lock; true otherwise (the caller takes it, as before).
    pub(super) fn maybe(&self) -> bool {
        if !fast() {
            return true;
        }
        let any = self.any.load(Ordering::Acquire);
        LOOKS[usize::from(any)].fetch_add(1, Ordering::Relaxed);
        any
    }

    /// Whether there are ranges: the flag with the switch, the ranges
    /// under their lock without it.
    pub(super) fn any(&self) -> bool {
        if fast() {
            let any = self.any.load(Ordering::Acquire);
            LOOKS[usize::from(any)].fetch_add(1, Ordering::Relaxed);
            return any;
        }
        !self.lock().is_empty()
    }

    /// The ranges that meet `[from, to)`, cut to it; none without taking
    /// the lock where [`Pending::maybe`] says so.
    pub(super) fn meeting(&self, from: usize, to: usize) -> Vec<(usize, usize)> {
        if !self.maybe() {
            return Vec::new();
        }
        self.lock()
            .iter()
            .filter(|&&(a, e)| a < to && e > from)
            .map(|&(a, e)| (a.max(from), e.min(to)))
            .collect()
    }

    /// The ranges that meet `[from, to)`, whole; as [`Pending::meeting`].
    pub(super) fn meeting_whole(&self, from: usize, to: usize) -> Vec<(usize, usize)> {
        if !self.maybe() {
            return Vec::new();
        }
        self.lock()
            .iter()
            .copied()
            .filter(|&(a, e)| a < to && e > from)
            .collect()
    }
}

/// Per page of title memory from a copy's first on: the write-watch asking
/// after which the copy last took it (0: never).
pub(super) struct Copied(RwLock<Vec<u64>>);

/// [`Copied`]'s notes for reading: shared with the switch, alone without.
pub(super) enum Reading<'a> {
    Shared(RwLockReadGuard<'a, Vec<u64>>),
    Alone(RwLockWriteGuard<'a, Vec<u64>>),
}

impl std::ops::Deref for Reading<'_> {
    type Target = Vec<u64>;
    fn deref(&self) -> &Self::Target {
        match self {
            Reading::Shared(g) => g,
            Reading::Alone(g) => g,
        }
    }
}

impl Copied {
    pub(super) fn new(pages: usize) -> Copied {
        Copied(RwLock::new(vec![0; pages]))
    }

    /// The notes, to read only.
    pub(super) fn read(&self) -> Reading<'_> {
        if fast() {
            LOOKS[2].fetch_add(1, Ordering::Relaxed);
            Reading::Shared(self.0.read().unwrap_or_else(|e| e.into_inner()))
        } else {
            Reading::Alone(self.0.write().unwrap_or_else(|e| e.into_inner()))
        }
    }

    /// The notes, to change.
    pub(super) fn write(&self) -> RwLockWriteGuard<'_, Vec<u64>> {
        self.0.write().unwrap_or_else(|e| e.into_inner())
    }
}

/// The line of the frame meter, with the switch on at some time of the
/// run: per frame, the looks at pending ranges answered by the flag, those
/// that took the lock, and the readers of pages' notes that shared it.
pub(crate) fn line(frames: f64) {
    if !crate::switches::NOTHING_NEW_FAST.ever() {
        return;
    }
    let [flag, locked, shared] =
        [0, 1, 2].map(|i| LOOKS[i].swap(0, Ordering::Relaxed) as f64 / frames.max(1.0));
    tracing::info!(
        "copies' notes looked at without their locks per frame (VICTORY_NOTHING_NEW_FAST): pending ranges by the flag {flag:.0}, with the lock {locked:.0}; pages' notes read sharing their lock {shared:.0}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn the_flag_follows_every_change_of_the_ranges() {
        let p = Pending::default();
        assert!(!p.any.load(Ordering::Acquire));
        p.lock().push((1, 2));
        assert!(p.any.load(Ordering::Acquire));
        p.lock().push((3, 4));
        p.lock().retain(|&r| r != (1, 2));
        assert!(p.any.load(Ordering::Acquire));
        // A holder that only reads leaves it as it was.
        assert_eq!(p.lock().len(), 1);
        assert!(p.any.load(Ordering::Acquire));
        p.lock().retain(|&r| r != (3, 4));
        assert!(!p.any.load(Ordering::Acquire));
        p.lock().clear();
        assert!(!p.any.load(Ordering::Acquire));
    }

    #[test]
    fn meeting_cuts_and_keeps_whole() {
        let p = Pending::default();
        p.lock().extend([(0, 10), (20, 30), (40, 50)]);
        // (Without the switch both take the lock; the answers are the
        // same with it, the flag being up.)
        assert_eq!(p.meeting(5, 25), vec![(5, 10), (20, 25)]);
        assert_eq!(p.meeting_whole(5, 25), vec![(0, 10), (20, 30)]);
        assert!(p.meeting(10, 20).is_empty());
        assert!(p.any());
    }

    /// The message-passing case of the flag: for each of many copies whose
    /// one range is declared, a writer puts its bytes in (a plain store)
    /// and then lets the range go, as a copy-back and `copy_back_done` do;
    /// readers wait for each copy's flag to go down, with no lock and no
    /// other order between the threads, and must then find the bytes in.
    /// It fails if the flag could go down before the change of the ranges
    /// is made, or without `Release`/`Acquire` on a machine that reorders.
    /// Meanwhile a looker takes the copies' locks: whatever it finds, the
    /// flag it reads after letting go matches it.
    #[test]
    fn a_reader_that_finds_no_range_sees_what_the_writer_did_before() {
        const COPIES: usize = 20_000;
        let copies: Vec<(Pending, AtomicUsize)> = (0..COPIES)
            .map(|i| {
                let p = Pending::default();
                p.lock().push((i, i + 1));
                (p, AtomicUsize::new(0))
            })
            .collect();
        std::thread::scope(|s| {
            s.spawn(|| {
                for (i, (p, bytes)) in copies.iter().enumerate() {
                    bytes.store(i + 1, Ordering::Relaxed);
                    p.lock().retain(|&r| r != (i, i + 1));
                }
            });
            for _ in 0..3 {
                s.spawn(|| {
                    for (i, (p, bytes)) in copies.iter().enumerate() {
                        while p.any.load(Ordering::Acquire) {
                            std::hint::spin_loop();
                        }
                        assert_eq!(bytes.load(Ordering::Relaxed), i + 1, "copy {i}");
                    }
                });
            }
            s.spawn(|| {
                for (p, _) in copies.iter().rev() {
                    let empty = p.lock().is_empty();
                    // (Down stays down: nothing pushes again.)
                    if empty {
                        assert!(!p.any.load(Ordering::Acquire));
                    }
                }
            });
        });
        assert!(copies.iter().all(|(p, _)| !p.any.load(Ordering::Acquire)));
    }

    /// A range declared on one thread and handed to another (the
    /// submitting thread declares, a refresh thread looks) is never missed
    /// by the flag.
    #[test]
    fn a_range_declared_before_the_handing_over_is_seen() {
        let p = &Pending::default();
        let (tx, rx) = std::sync::mpsc::channel::<usize>();
        std::thread::scope(|s| {
            s.spawn(move || {
                for round in 0..5_000 {
                    p.lock().push((round, round + 1));
                    tx.send(round).unwrap();
                }
            });
            s.spawn(move || {
                for round in rx.iter() {
                    assert!(p.any.load(Ordering::Acquire), "round {round}");
                    let held = p.lock();
                    assert!(held.contains(&(round, round + 1)));
                }
            });
        });
    }

    /// Readers of the pages' notes, shared or alone, never see a writer's
    /// change half made: the writer sets every note of a round under one
    /// write lock.
    #[test]
    fn readers_of_the_notes_see_one_moment() {
        let c = Copied::new(64);
        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| {
                for round in 1..=5_000u64 {
                    let mut notes = c.write();
                    for at in notes.iter_mut() {
                        *at = round;
                    }
                }
                stop.store(true, Ordering::Release);
            });
            for shared in [true, false, true] {
                let (c, stop) = (&c, &stop);
                s.spawn(move || {
                    while !stop.load(Ordering::Acquire) {
                        let notes = if shared {
                            Reading::Shared(c.0.read().unwrap())
                        } else {
                            Reading::Alone(c.0.write().unwrap())
                        };
                        let first = notes[0];
                        assert!(notes.iter().all(|&at| at == first));
                    }
                });
            }
        });
        assert!(c.read().iter().all(|&at| at == 5_000));
    }
}

//! Diagnostics of the guards' lock ([`super::GUARDS`]): how long threads
//! wait for it and how long they hold it, per frame, by who takes it
//! ([`Who`]). Nothing changes in how it is taken.
//!
//! What it decides: whether a write fault that takes no lock of all the
//! guards (VIctory-NVIDIA 3f550568, pull request #516, `FAULT_UNLOCKED`)
//! is worth it here. On an RTX 5080 the lock was held 49.5 ms a frame.
//!
//! What it costs: the lock is tried first, and only when that fails is a
//! clock read, before and after the wait (the wait is exact). The time it
//! is held is read on one taking in [`SAMPLE`] of each thread and counted
//! [`SAMPLE`] times. In builds without diagnostics there is none of it:
//! the lock is taken as before.

/// Who takes the lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub(super) enum Who {
    /// The write fault handler (title threads).
    Fault,
    /// Askings of the guards: refreshes of host copies, on the submitting
    /// thread and the threads beside it.
    Asking,
    /// The runtime: mappings, kernel and DirectStorage writes, commits.
    Runtime,
}

#[cfg(not(feature = "diagnostics"))]
pub(super) type Held<T> = std::sync::MutexGuard<'static, T>;

/// `m`, locked, by `who`.
#[cfg(not(feature = "diagnostics"))]
pub(super) fn lock<T>(m: &'static std::sync::Mutex<T>, _who: Who) -> Held<T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(feature = "diagnostics")]
pub(super) use timed::{Held, lock};

/// One taking in this many, per thread, has its holding timed.
#[cfg(feature = "diagnostics")]
pub(super) const SAMPLE: u32 = 16;

#[cfg(feature = "diagnostics")]
mod timed {
    use super::{SAMPLE, Who};
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    use std::sync::{Mutex, MutexGuard, TryLockError};
    use std::time::Instant;

    /// Since they were last taken, by [`Who`]: takings that waited, and
    /// nanoseconds waited; takings (counted [`SAMPLE`] at a time) and
    /// nanoseconds held (of the timed ones, times [`SAMPLE`]).
    pub(in super::super) static WAITS: [[AtomicU64; 4]; 3] =
        [const { [const { AtomicU64::new(0) }; 4] }; 3];
    pub(in super::super) const WAITED: usize = 0;
    pub(in super::super) const WAITED_NS: usize = 1;
    pub(in super::super) const TAKEN: usize = 2;
    pub(in super::super) const HELD_NS: usize = 3;

    thread_local! {
        /// Takings by this thread, for [`SAMPLE`].
        static TAKINGS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    /// The lock held, timed if it is one of the [`SAMPLE`].
    pub(in super::super) struct Held<T: 'static> {
        guard: MutexGuard<'static, T>,
        since: Option<(Instant, Who)>,
    }

    impl<T> std::ops::Deref for Held<T> {
        type Target = T;
        fn deref(&self) -> &T {
            &self.guard
        }
    }

    impl<T> std::ops::DerefMut for Held<T> {
        fn deref_mut(&mut self) -> &mut T {
            &mut self.guard
        }
    }

    impl<T> Drop for Held<T> {
        fn drop(&mut self) {
            // (Before the guard, a field, is let go.)
            if let Some((since, who)) = self.since {
                let ns = since.elapsed().as_nanos() as u64;
                let row = &WAITS[who as usize];
                row[HELD_NS].fetch_add(ns * u64::from(SAMPLE), Relaxed);
                row[TAKEN].fetch_add(u64::from(SAMPLE), Relaxed);
            }
        }
    }

    pub(in super::super) fn lock<T>(m: &'static Mutex<T>, who: Who) -> Held<T> {
        let guard = match m.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(e)) => e.into_inner(),
            Err(TryLockError::WouldBlock) => {
                let asked = Instant::now();
                let guard = m.lock().unwrap_or_else(|e| e.into_inner());
                let row = &WAITS[who as usize];
                row[WAITED].fetch_add(1, Relaxed);
                row[WAITED_NS].fetch_add(asked.elapsed().as_nanos() as u64, Relaxed);
                guard
            }
        };
        let n = TAKINGS.get().wrapping_add(1);
        TAKINGS.set(n);
        let since = n.is_multiple_of(SAMPLE).then(|| (Instant::now(), who));
        Held { guard, since }
    }
}

/// The lock's numbers per frame since the last call, taken: by [`Who`],
/// takings that waited, ms waited, takings, ms held; all zero without
/// diagnostics.
pub(crate) fn take(frames: f64) -> [[f64; 4]; 3] {
    #[cfg(feature = "diagnostics")]
    {
        use std::sync::atomic::Ordering::Relaxed;
        let frames = frames.max(1.0);
        std::array::from_fn(|who| {
            let row = &timed::WAITS[who];
            let take = |i: usize| row[i].swap(0, Relaxed) as f64 / frames;
            [
                take(timed::WAITED),
                take(timed::WAITED_NS) / 1e6,
                take(timed::TAKEN),
                take(timed::HELD_NS) / 1e6,
            ]
        })
    }
    #[cfg(not(feature = "diagnostics"))]
    {
        let _ = frames;
        [[0.0; 4]; 3]
    }
}

/// [`take`]'s numbers for the frame-rate line: ms waited and ms held a
/// frame, all takers; and the diagnostics line by taker.
pub(crate) fn line(frames: f64) -> (f64, f64) {
    let n = take(frames);
    let wait: f64 = n.iter().map(|w| w[1]).sum();
    let held: f64 = n.iter().map(|w| w[3]).sum();
    #[cfg(feature = "diagnostics")]
    {
        let [fault, asking, runtime] = n;
        tracing::info!(
            "the guards' lock per frame (waits exact, holding from one taking in {SAMPLE}): waited {wait:.2} ms, held {held:.2} ms; \
             write faults {:.0} waits {:.2} ms, {:.0} takings held {:.2} ms; askings of refreshes {:.0} waits {:.2} ms, \
             {:.0} takings held {:.2} ms; the runtime (mappings, kernel writes) {:.0} waits {:.2} ms, {:.0} takings held {:.2} ms",
            fault[0],
            fault[1],
            fault[2],
            fault[3],
            asking[0],
            asking[1],
            asking[2],
            asking[3],
            runtime[0],
            runtime[1],
            runtime[2],
            runtime[3]
        );
    }
    (wait, held)
}

#[cfg(all(test, feature = "diagnostics"))]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn waits_are_counted_only_when_the_lock_is_held_by_another() {
        static M: Mutex<u32> = Mutex::new(0);
        static FREE: Mutex<()> = Mutex::new(());
        let _alone = FREE.lock().unwrap_or_else(|e| e.into_inner());
        let row = &timed::WAITS[Who::Runtime as usize];
        let read = |i: usize| row[i].load(std::sync::atomic::Ordering::Relaxed);
        let before = [timed::WAITED, timed::WAITED_NS, timed::TAKEN].map(read);
        // Taken free: no wait. A thread taking it SAMPLE times times one.
        std::thread::spawn(|| {
            for _ in 0..SAMPLE {
                *lock(&M, Who::Runtime) += 1;
            }
        })
        .join()
        .unwrap();
        assert!(read(timed::TAKEN) >= before[2] + u64::from(SAMPLE));
        // Held by this thread while another asks: that one waits.
        let held = lock(&M, Who::Runtime);
        let waiter = std::thread::spawn(|| *lock(&M, Who::Runtime) += 1);
        std::thread::sleep(std::time::Duration::from_millis(30));
        drop(held);
        waiter.join().unwrap();
        assert!(read(timed::WAITED) > before[0]);
        assert!(read(timed::WAITED_NS) >= before[1] + 20_000_000);
        assert_eq!(*lock(&M, Who::Runtime), SAMPLE + 1);
    }
}

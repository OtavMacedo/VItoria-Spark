//! The host lists of landed flights reset on a thread of their own
//! (`VICTORY_LIST_REAPER=1`; off by default; `SPARK_AB` can alternate it).
//!
//! When flights sent ahead have landed, their host lists are reset and
//! kept for reuse (`crate::list::recycle`): the dynamic descriptor slots and
//! upload memory their commands named go, and each list's allocators and
//! command lists are reset (`HostList::reset`), driver work of tens of
//! microseconds a list. That ran where the flights were landed: at the end
//! of every submission sent ahead, on the submitting thread, and on the
//! thread that settles flights. A timeline of the world on another fork
//! counted 250 list resets a frame, 20 ms of threads' time, on the thread
//! that feeds the card.
//!
//! With the switch the landed lists go, in the order they landed, to a
//! thread of ours that recycles them there, batch after batch in the same
//! order. Nothing else changes: a list is handed over only once it has run
//! (the same moment it was recycled before), and it goes back to the pool
//! of idle lists only when its reset is done, inside `recycle`, so no list
//! is recorded again before its reset ends. What the pool lacks meanwhile
//! is made new by `crate::list::fresh_list`, as before. The line says how
//! many lists went each way and the most batches that waited at once.
//!
//! This sits beside `VICTORY_REAPER` (`crate::host`), which drops let-go
//! host objects on its own thread: a batch of those drops can take tens of
//! milliseconds, and lists waiting behind it would leave the pool empty.
//!
//! After ProjectLeonida/mandy c0ff3fe6f (`LEONIDA_REAPER`, GPL-3.0), after
//! Grimkaaah/Victory-Grimkaaah f2586ef (GPL-3.0) and mgrein's
//! VIctory-7900XTX a2cf68bb (GPL-3.0-or-later); the README there measured
//! 2.3 to 3.8 ms a frame less with it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};

/// The host lists of landed flights, each flight's lists with the places
/// of those it ran (as `crate::list::recycle` takes them).
pub(crate) type Landed = Vec<(Vec<crate::list::Detached>, Vec<usize>)>;

/// A batch of landed lists on its way to the thread.
struct Batch(Landed);

// SAFETY: the lists have run and nothing else names them: they were taken
// out of the line of flights, and only the thread that takes the batch
// touches them. D3D12 objects are free-threaded, and the same lists were
// already recycled on either of two threads (the submitting one and the
// one that settles flights).
unsafe impl Send for Batch {}

/// What is handed from the threads that land flights to the one that
/// recycles their lists: batches, in the order they came.
struct Handoff<T> {
    waiting: Mutex<Vec<T>>,
    more: Condvar,
    /// The most batches that waited at once since the last line.
    most: AtomicU64,
}

impl<T> Handoff<T> {
    const fn new() -> Self {
        Handoff {
            waiting: Mutex::new(Vec::new()),
            more: Condvar::new(),
            most: AtomicU64::new(0),
        }
    }

    /// Hands `batch` over, behind the ones that wait.
    fn give(&self, batch: T) {
        let waiting = {
            let mut waiting = self.waiting.lock().unwrap_or_else(|e| e.into_inner());
            waiting.push(batch);
            waiting.len()
        };
        self.most.fetch_max(waiting as u64, Ordering::Relaxed);
        self.more.notify_one();
    }

    /// Every batch that waits, oldest first, waiting for one if none does.
    fn take(&self) -> Vec<T> {
        let mut waiting = self.waiting.lock().unwrap_or_else(|e| e.into_inner());
        while waiting.is_empty() {
            waiting = self.more.wait(waiting).unwrap_or_else(|e| e.into_inner());
        }
        std::mem::take(&mut *waiting)
    }
}

static HANDOFF: Handoff<Batch> = Handoff::new();

/// Since the last line: lists recycled where they landed and the
/// microseconds that took; lists recycled by the thread, its batches and
/// microseconds.
static COUNTS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

/// Whether the counts are kept and said: only with the switch, so that a
/// run without it logs as before.
fn counting() -> bool {
    crate::switches::LIST_REAPER.ever()
}

/// Whether the thread runs, started at the first batch for it.
fn started() -> bool {
    static STARTED: OnceLock<bool> = OnceLock::new();
    *STARTED.get_or_init(|| {
        let ok = std::thread::Builder::new()
            .name("victory-list-reaper".into())
            .spawn(reaper)
            .is_ok();
        if ok {
            tracing::info!(
                "the host lists of landed flights are reset by a thread of their own (VICTORY_LIST_REAPER=1)"
            );
        } else {
            tracing::warn!("no thread for the resets of landed lists: they stay where they land");
        }
        ok
    })
}

/// The ran lists of `landed`.
fn ran(landed: &Landed) -> u64 {
    landed.iter().map(|(_, order)| order.len() as u64).sum()
}

/// Recycles the host lists of landed flights: here, as before, or with
/// the switch on the thread of this module.
pub(crate) fn land(landed: Landed) {
    if landed.is_empty() {
        return;
    }
    if crate::switches::LIST_REAPER.on() && started() {
        HANDOFF.give(Batch(landed));
        return;
    }
    let began = counting().then(|| (std::time::Instant::now(), ran(&landed)));
    for (lists, order) in landed {
        crate::list::recycle(lists, &order);
    }
    if let Some((began, lists)) = began {
        COUNTS[0].fetch_add(lists, Ordering::Relaxed);
        COUNTS[1].fetch_add(began.elapsed().as_micros() as u64, Ordering::Relaxed);
    }
}

/// The thread: recycles each batch in the order it came.
fn reaper() {
    loop {
        for Batch(landed) in HANDOFF.take() {
            let began = std::time::Instant::now();
            let lists = ran(&landed);
            for (lists, order) in landed {
                crate::list::recycle(lists, &order);
            }
            COUNTS[2].fetch_add(lists, Ordering::Relaxed);
            COUNTS[3].fetch_add(1, Ordering::Relaxed);
            COUNTS[4].fetch_add(began.elapsed().as_micros() as u64, Ordering::Relaxed);
        }
    }
}

/// The frame line, with the switch: lists reset on the thread and where
/// they landed, per frame.
pub(crate) fn line(frames: f64) {
    if !counting() {
        return;
    }
    let [here, here_us, there, batches, there_us] =
        [0, 1, 2, 3, 4].map(|i| COUNTS[i].swap(0, Ordering::Relaxed) as f64 / frames);
    let most = HANDOFF.most.swap(0, Ordering::Relaxed);
    if here + there == 0.0 {
        return;
    }
    tracing::info!(
        "host lists of landed flights reset per frame (VICTORY_LIST_REAPER={}): {there:.0} by their own thread in \
         {batches:.0} batches, {:.2} ms of it, at most {most} batches waiting; {here:.0} where they landed, {:.2} ms",
        u8::from(crate::switches::LIST_REAPER.on()),
        there_us / 1e3,
        here_us / 1e3
    );
}

#[cfg(test)]
mod tests {
    use super::Handoff;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    /// Every batch handed over is taken once, in the order it came, by a
    /// thread that takes them while more come.
    #[test]
    fn batches_are_taken_once_in_the_order_they_came() {
        let handoff: Arc<Handoff<u32>> = Arc::new(Handoff::new());
        const N: u32 = 20_000;
        let taker = {
            let handoff = handoff.clone();
            std::thread::spawn(move || {
                let mut seen = Vec::new();
                while seen.last() != Some(&(N - 1)) {
                    seen.extend(handoff.take());
                }
                seen
            })
        };
        for i in 0..N {
            handoff.give(i);
            if i % 997 == 0 {
                std::thread::yield_now();
            }
        }
        let seen = taker.join().unwrap();
        assert_eq!(seen, (0..N).collect::<Vec<_>>());
        assert!(handoff.waiting.lock().unwrap().is_empty());
        let most = handoff.most.load(Ordering::Relaxed);
        assert!((1..=u64::from(N)).contains(&most), "{most}");
    }

    /// A taker that waits is woken by the next batch.
    #[test]
    fn a_waiting_taker_is_woken() {
        let handoff: Arc<Handoff<u32>> = Arc::new(Handoff::new());
        let taker = {
            let handoff = handoff.clone();
            std::thread::spawn(move || handoff.take())
        };
        std::thread::sleep(std::time::Duration::from_millis(20));
        handoff.give(7);
        assert_eq!(taker.join().unwrap(), vec![7]);
    }
}

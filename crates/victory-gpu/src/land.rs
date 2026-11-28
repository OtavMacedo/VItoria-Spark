//! What shaders wrote to copies in the card's memory goes into title memory
//! on several threads (`VICTORY_LAND_WIDE=1`; off by default: one piece
//! after the other on the thread that lands flights, as before).
//!
//! The landing thread (`crate::host`'s relay) merged some 1,500 pieces of
//! 64 KiB a frame one after the other, each read from title memory,
//! compared with the copy's CPU side, written back through the kernel after
//! its guards were opened: about 49 ms of CPU a frame on the PC with the
//! RTX 3070, while the frame is about preparing plus the card, one after
//! the other. Each piece is its own bytes of its copy and of title memory,
//! so the pieces of a landing can go to the threads kept for spread work
//! (`crate::shadow::wide_each`, `VICTORY_WIDE_THREADS`) and this one.
//!
//! What stays as it was, so that title memory ends byte for byte as the
//! landing one piece after the other leaves it:
//! - pieces that may hold the same bytes land on one thread, in the order
//!   given: those in the same 64 KiB block of title addresses (the guards'
//!   block, and the write-watch pages of a quiet write-back lie in it), and
//!   those in title views that show the same pages of a section at other
//!   addresses (`crate::inplace::physical_keys`). Two copies may hold the
//!   same title memory, a landing's flights may bring the same piece twice,
//!   and an older slot merged after a newer one would undo a later write;
//! - every piece is merged by the same code as before
//!   (`Shadow::card_merge_relay_beside`): the same title bytes read, the
//!   same stretch written, with the same bytes left out (those the card had
//!   not received);
//! - the title's bytes going in under shader writes
//!   (`Shadow::merge_under_writes`) are kept out of a copy's pieces by a
//!   lock that its landings share and that merge takes alone, in place of
//!   the CPU side's lock, which only one piece of a copy could hold;
//! - the landing is done, whole, before the call returns: the flights'
//!   ranges are done with, and the title told, afterwards, as before.
//!
//! Each landing decides on its own, so `SPARK_AB` may alternate the switch
//! (`crate::switches::LAND_WIDE`): nothing is kept that depends on it.
//!
//! After trustno1-coder/VIctory-NVIDIA 590af7ca (`VICTORY_LAND`, its
//! `land.rs`: 45 ms to 8 ms of the frame's shape there), and the merge on
//! several threads of fxceless/victory-fork pull requests #665 and #1066.
//! Their pieces were grouped by block only; here views of the same pages
//! are grouped too. Not taken: their merge in one AVX2 pass with stores of
//! the changed dwords alone (`VICTORY_EXACT_BACK`, which writes less than
//! the whole stretch, so title memory would not end as the landing one
//! piece after the other leaves it when the title writes meanwhile), and
//! their writes through a view of our own (here `VICTORY_BACK_OWN_VIEW`).

use std::collections::HashMap;
#[cfg(windows)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicUsize, Ordering};

/// About this many bytes of pieces to a job: four pieces of 64 KiB. Fewer
/// jobs cost less to hand out; more keep the threads busy to the end.
pub(crate) const JOB: usize = 256 << 10;

/// Whether this landing goes on several threads.
#[cfg(windows)]
fn wide_on() -> bool {
    crate::switches::LAND_WIDE.on()
}

/// A piece to land: `[lo, hi)` of `copy` (offsets in it), whose bytes as
/// fetched from the card are at address `bytes`, fetched after the send
/// numbered `after` (`Shadow::card_merge_relay`).
#[cfg(windows)]
pub(crate) struct Piece<'a> {
    pub(crate) copy: &'a std::sync::Arc<crate::shadow::Shadow>,
    pub(crate) lo: usize,
    pub(crate) hi: usize,
    pub(crate) bytes: usize,
    pub(crate) after: u64,
}

/// Lands `pieces` in title memory and in their copies' CPU sides, as
/// `Shadow::card_merge_relay` of each in the order given does, then
/// `Shadow::walls_merged` if `walls`. With [`wide_on`], on several threads.
/// Returns the bytes written to title memory.
///
/// # Safety
/// `bytes` of each piece is the address of `hi - lo` readable bytes, which
/// stay as they are until this returns.
#[cfg(windows)]
pub(crate) unsafe fn card(pieces: &[Piece<'_>], walls: bool) -> usize {
    let one = |p: &Piece<'_>, title: &mut Vec<u8>, beside: bool| {
        // SAFETY: the caller's.
        let gpu = unsafe { std::slice::from_raw_parts(p.bytes as *const u8, p.hi - p.lo) };
        let merged = if beside {
            p.copy.card_merge_relay_beside(p.lo, gpu, p.after, title)
        } else {
            p.copy.card_merge_relay(p.lo, gpu, p.after, title)
        };
        if walls {
            p.copy.walls_merged(gpu.len(), merged);
        }
        merged
    };
    if pieces.len() < 2 || !wide_on() {
        let mut title = Vec::new();
        return pieces.iter().map(|p| one(p, &mut title, false)).sum();
    }
    let started = std::time::Instant::now();
    let ranges: Vec<(usize, usize)> = pieces
        .iter()
        .map(|p| (p.copy.title_start_usize() + p.lo, p.hi - p.lo))
        .collect();
    let keys = crate::inplace::physical_keys(&ranges);
    let sizes: Vec<usize> = ranges.iter().map(|r| r.1).collect();
    let (jobs, groups) = plan(&keys, &sizes, JOB);
    let busy = AtomicU64::new(0);
    let written = run(
        &jobs,
        &|i| {
            thread_local! {
                static TITLE: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
            }
            TITLE.with_borrow_mut(|title| one(&pieces[i], title, true))
        },
        &|count: usize, f: &(dyn Fn(usize) + Sync)| {
            crate::shadow::wide_each(count, &|j| {
                let working = std::time::Instant::now();
                f(j);
                busy.fetch_add(working.elapsed().as_micros() as u64, Ordering::Relaxed);
            })
        },
    );
    let counts = [
        1,
        pieces.len() as u64,
        groups as u64,
        jobs.len() as u64,
        started.elapsed().as_micros() as u64,
        busy.into_inner(),
    ];
    for (stat, n) in STATS.iter().zip(counts) {
        stat.fetch_add(n, Ordering::Relaxed);
    }
    written.into_iter().sum()
}

/// Since the last frame-rate line: landings on several threads, their
/// pieces, groups of pieces that may hold the same bytes, jobs, the time
/// the landings took (µs) and the jobs' time on all threads (µs).
#[cfg(windows)]
static STATS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];

/// For the frame-rate line, with landings on several threads since the
/// last one.
#[cfg(windows)]
pub(crate) fn line(frames: f64) {
    let s = STATS
        .each_ref()
        .map(|s| s.swap(0, Ordering::Relaxed) as f64 / frames);
    if s[0] == 0.0 {
        return;
    }
    tracing::info!(
        "landings per frame on several threads (VICTORY_LAND_WIDE=1): {:.1}, of {:.0} pieces in {:.0} groups and {:.0} jobs; \
         {:.2} ms taken, {:.2} ms on all threads",
        s[0],
        s[1],
        s[2],
        s[3],
        s[4] / 1000.0,
        s[5] / 1000.0
    );
}

/// Pieces given in order (`keys[i]`: what piece `i` may share with another,
/// see the file; `bytes[i]`: its size) cut into jobs: each job the numbers
/// of the pieces it lands, in the order to land them. Pieces that share a
/// key, directly or through others, are in one job, in the order given;
/// whole such groups are put together, in the order of their first piece,
/// until a job holds `job` bytes or more. Also how many groups there were.
pub(crate) fn plan(
    keys: &[Vec<(usize, u64)>],
    bytes: &[usize],
    job: usize,
) -> (Vec<Vec<usize>>, usize) {
    let n = keys.len();
    // Each piece's group, as a tree of pieces (the root names it).
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut first: HashMap<(usize, u64), usize> = HashMap::new();
    for (i, keys) in keys.iter().enumerate() {
        for key in keys {
            let other = *first.entry(*key).or_insert(i);
            let (a, b) = (root(&mut parent, i), root(&mut parent, other));
            // The lower piece names the group.
            if a != b {
                parent[a.max(b)] = a.min(b);
            }
        }
    }
    // Groups by their first piece, each piece in order.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut at: HashMap<usize, usize> = HashMap::new();
    for i in 0..n {
        let r = root(&mut parent, i);
        let g = *at.entry(r).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[g].push(i);
    }
    let count = groups.len();
    let mut jobs: Vec<Vec<usize>> = Vec::new();
    let mut size = usize::MAX;
    for group in groups {
        if size >= job {
            jobs.push(Vec::new());
            size = 0;
        }
        size += group
            .iter()
            .map(|&i| bytes.get(i).copied().unwrap_or(0))
            .sum::<usize>();
        jobs.last_mut().expect("pushed above").extend(group);
    }
    (jobs, count)
}

/// Runs the jobs of [`plan`]: `spread` calls the function it is given with
/// every job's number, on whatever threads it likes, and returns when every
/// call has; a job lands its pieces with `land`, in its order. What `land`
/// returned for each piece.
pub(crate) fn run(
    jobs: &[Vec<usize>],
    land: &(dyn Fn(usize) -> usize + Sync),
    spread: &dyn Fn(usize, &(dyn Fn(usize) + Sync)),
) -> Vec<usize> {
    let pieces = jobs.iter().map(Vec::len).sum();
    let written: Vec<AtomicUsize> = (0..pieces).map(|_| AtomicUsize::new(0)).collect();
    spread(jobs.len(), &|j| {
        for &i in &jobs[j] {
            if let Some(w) = written.get(i) {
                w.store(land(i), Ordering::Relaxed);
            }
        }
    });
    written.into_iter().map(AtomicUsize::into_inner).collect()
}

#[cfg(test)]
mod tests;

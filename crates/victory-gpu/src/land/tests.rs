//! [`plan`] and [`run`] against the landing one piece after the other, on
//! a model of title memory, copies and pieces: the same merge as
//! `Shadow::card_merge_relay` (from the first byte the card changed to the
//! last, title bytes read, the card's bytes put where it changed them,
//! bytes the card had not received left out, the stretch written back),
//! with the model's blocks of 256 bytes for the 64 KiB ones, two title
//! views of one section (the second from an offset that is not on a
//! block's line) and private memory.

use super::{plan, run};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

/// The model's block (64 KiB in title memory).
const BLOCK: usize = 256;
/// A title view of a section at address 0, `SECTION` bytes of it.
const SECTION: usize = 16 * BLOCK;
/// A second title view of the section at `VIEW`, `SHOWN` bytes of it,
/// showing the section from `FROM` on.
const VIEW: usize = 40 * BLOCK;
const SHOWN: usize = 6 * BLOCK;
const FROM: usize = 3 * BLOCK + 40;
/// Private title memory at `OWN_AT`, `OWN` bytes, after the section's in
/// the model's physical bytes.
const OWN_AT: usize = 64 * BLOCK;
const OWN: usize = 8 * BLOCK;

fn random(seed: &mut u64) -> usize {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    (*seed >> 8) as usize
}

/// The physical byte a title address shows.
fn physical(at: usize) -> usize {
    match at {
        _ if at < SECTION => at,
        _ if (VIEW..VIEW + SHOWN).contains(&at) => FROM + (at - VIEW),
        _ if (OWN_AT..OWN_AT + OWN).contains(&at) => SECTION + (at - OWN_AT),
        _ => panic!("{at:#x} is not title memory"),
    }
}

/// As `crate::inplace::keys_in`: the block of each address, and for the
/// section's views the section's block of each byte.
fn keys(at: usize, len: usize) -> Vec<(usize, u64)> {
    let mut keys: Vec<(usize, u64)> = (at / BLOCK..(at + len).div_ceil(BLOCK))
        .map(|b| (0, b as u64))
        .collect();
    if at < OWN_AT {
        let (from, to) = (physical(at), physical(at + len - 1) + 1);
        keys.extend((from / BLOCK..to.div_ceil(BLOCK)).map(|b| (1, b as u64)));
    }
    keys
}

/// `[from, to)` without `skip`, as `crate::shadow::outside`.
fn outside(from: usize, to: usize, mut skip: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    skip.sort_unstable();
    let (mut out, mut at) = (Vec::new(), from);
    for (a, b) in skip {
        if b <= at {
            continue;
        }
        if a >= to {
            break;
        }
        if a > at {
            out.push((at, a));
        }
        at = at.max(b);
    }
    if at < to {
        out.push((at, to));
    }
    out
}

/// A copy of title memory `[start, start + held.len())`: its CPU side, and
/// the offsets whose title bytes the card had not received (left out).
struct Copy {
    start: usize,
    held: Vec<AtomicU8>,
    skip: Vec<(usize, usize)>,
}

/// A piece: `[lo, lo + gpu.len())` of copy `copy`, what the card held there.
struct Piece {
    copy: usize,
    lo: usize,
    gpu: Vec<u8>,
}

/// Title memory and copies, and what the landings did: per physical block
/// whether a piece is landing in it now, and the pieces landed there.
struct World {
    title: Vec<AtomicU8>,
    copies: Vec<Copy>,
    busy: Vec<AtomicBool>,
    landed: Vec<Mutex<Vec<usize>>>,
    /// Two pieces in one block at once.
    clashes: AtomicUsize,
    /// Physical bytes some piece wrote back.
    stretched: Vec<AtomicBool>,
}

fn bytes(v: &[AtomicU8]) -> Vec<u8> {
    v.iter().map(|b| b.load(Ordering::Relaxed)).collect()
}

fn atomic(v: &[u8]) -> Vec<AtomicU8> {
    v.iter().map(|&b| AtomicU8::new(b)).collect()
}

/// Title memory and copies as made by [`made`], fresh for each landing.
#[derive(Clone)]
struct Start {
    title: Vec<u8>,
    copies: Vec<(usize, Vec<u8>, Vec<(usize, usize)>)>,
}

impl Start {
    fn world(&self) -> World {
        let blocks = (SECTION + OWN).div_ceil(BLOCK);
        World {
            title: atomic(&self.title),
            copies: (self.copies.iter())
                .map(|(start, held, skip)| Copy {
                    start: *start,
                    held: atomic(held),
                    skip: skip.clone(),
                })
                .collect(),
            busy: (0..blocks).map(|_| AtomicBool::new(false)).collect(),
            landed: (0..blocks).map(|_| Mutex::new(Vec::new())).collect(),
            clashes: AtomicUsize::new(0),
            stretched: (0..self.title.len())
                .map(|_| AtomicBool::new(false))
                .collect(),
        }
    }
}

impl World {
    /// Lands piece `i` as `Shadow::card_merge_relay_held` does. The bytes
    /// written back to title memory.
    fn land(&self, pieces: &[Piece], i: usize) -> usize {
        let piece = &pieces[i];
        let copy = &self.copies[piece.copy];
        let title_at = copy.start + piece.lo;
        let blocks: Vec<usize> = {
            let (a, b) = (physical(title_at), physical(title_at + piece.gpu.len() - 1));
            (a / BLOCK..=b / BLOCK).collect()
        };
        for &b in &blocks {
            if self.busy[b].swap(true, Ordering::AcqRel) {
                self.clashes.fetch_add(1, Ordering::Relaxed);
            }
            self.landed[b].lock().unwrap().push(i);
        }
        // Long enough for another thread to come into the same block.
        std::thread::yield_now();
        let written = self.merge(copy, piece);
        for &b in &blocks {
            self.busy[b].store(false, Ordering::Release);
        }
        written
    }

    fn merge(&self, copy: &Copy, piece: &Piece) -> usize {
        let (lo, gpu) = (piece.lo, &piece.gpu[..]);
        let held = &copy.held[lo..lo + gpu.len()];
        let differs = |k: usize| gpu[k] != held[k].load(Ordering::Relaxed);
        let Some(first) = (0..gpu.len()).find(|&k| differs(k)) else {
            return 0;
        };
        let last = (0..gpu.len()).rev().find(|&k| differs(k)).unwrap_or(first);
        let (at, n) = (lo + first, last + 1 - first);
        let skip: Vec<(usize, usize)> = (copy.skip.iter().copied())
            .filter(|&(a, b)| a < at + n && b > at)
            .collect();
        let start = copy.start + at;
        let mut title: Vec<u8> = (0..n)
            .map(|k| self.title[physical(start + k)].load(Ordering::Relaxed))
            .collect();
        let mut changed = false;
        for (a, b) in outside(at, at + n, skip) {
            for k in a..b {
                let (g, h) = (gpu[k - lo], &copy.held[k]);
                let t = &mut title[k - at];
                if g != h.load(Ordering::Relaxed) {
                    changed |= *t != g;
                    *t = g;
                    h.store(g, Ordering::Relaxed);
                }
            }
        }
        if !changed {
            return 0;
        }
        for (k, t) in title.iter().enumerate() {
            let p = physical(start + k);
            self.title[p].store(*t, Ordering::Relaxed);
            self.stretched[p].store(true, Ordering::Relaxed);
        }
        n
    }

    /// Title memory, the copies' CPU sides.
    fn end(&self) -> (Vec<u8>, Vec<Vec<u8>>) {
        (
            bytes(&self.title),
            self.copies.iter().map(|c| bytes(&c.held)).collect(),
        )
    }

    /// Every block's pieces landed in the order given, none two at once.
    fn in_order(&self) -> bool {
        self.clashes.load(Ordering::Relaxed) == 0
            && (self.landed.iter()).all(|l| l.lock().unwrap().is_sorted_by(|a, b| a < b))
    }
}

/// Title memory, six copies (two over the section's first view, two over
/// its second, one of them from an offset off the blocks' lines, two over
/// the private memory) and `count` pieces, as the relay gives them: each
/// inside one block of title addresses, some the same piece again, some
/// of another flight's slot.
fn made(seed: &mut u64, count: usize) -> (Start, Vec<Piece>) {
    let title: Vec<u8> = (0..SECTION + OWN).map(|_| random(seed) as u8).collect();
    let spans = [
        (0, SECTION),
        (2 * BLOCK + 17, 8 * BLOCK),
        (VIEW, SHOWN),
        (VIEW + BLOCK + 5, 3 * BLOCK),
        (OWN_AT, 3 * BLOCK),
        (OWN_AT + 9, 5 * BLOCK),
    ];
    let copies: Vec<(usize, Vec<u8>, Vec<(usize, usize)>)> = (spans.iter())
        .map(|&(start, len)| {
            // The CPU side holds title memory, here and there older.
            let mut held: Vec<u8> = (0..len).map(|k| title[physical(start + k)]).collect();
            for _ in 0..6 {
                let at = random(seed) % len;
                let n = (random(seed) % 40).min(len - at);
                held[at..at + n].fill(random(seed) as u8);
            }
            let skip = (0..random(seed) % 4)
                .map(|_| {
                    let at = random(seed) % len;
                    (at, (at + 1 + random(seed) % 60).min(len))
                })
                .collect();
            (start, held, skip)
        })
        .collect();
    let mut pieces: Vec<Piece> = Vec::new();
    while pieces.len() < count {
        if !pieces.is_empty() && random(seed).is_multiple_of(6) {
            // The same piece again, from another flight.
            let again = &pieces[random(seed) % pieces.len()];
            let (copy, lo) = (again.copy, again.lo);
            let gpu = again
                .gpu
                .iter()
                .map(|&b| b ^ (random(seed) % 3) as u8)
                .collect();
            pieces.push(Piece { copy, lo, gpu });
            continue;
        }
        let copy = random(seed) % copies.len();
        let (start, held, _) = &copies[copy];
        let lo = random(seed) % held.len();
        // Up to the end of the block of title addresses, as the relay cuts.
        let end = ((start + lo) / BLOCK + 1) * BLOCK - start;
        let hi = (lo + 1 + random(seed) % BLOCK).min(end).min(held.len());
        let mut gpu = held[lo..hi].to_vec();
        for _ in 0..1 + random(seed) % 3 {
            let at = random(seed) % gpu.len();
            let n = (random(seed) % 24).min(gpu.len() - at);
            gpu[at..at + n].fill(random(seed) as u8);
        }
        pieces.push(Piece { copy, lo, gpu });
    }
    let start = Start { title, copies };
    (start, pieces)
}

fn plan_of(start: &Start, pieces: &[Piece], job: usize) -> (Vec<Vec<usize>>, usize) {
    let keys: Vec<Vec<(usize, u64)>> = (pieces.iter())
        .map(|p| keys(start.copies[p.copy].0 + p.lo, p.gpu.len()))
        .collect();
    let sizes: Vec<usize> = pieces.iter().map(|p| p.gpu.len()).collect();
    plan(&keys, &sizes, job)
}

/// Calls `f` with every job's number, one after the other, in `order`.
fn in_turn(order: Vec<usize>) -> impl Fn(usize, &(dyn Fn(usize) + Sync)) {
    move |count: usize, f: &(dyn Fn(usize) + Sync)| {
        assert_eq!(order.len(), count);
        order.iter().for_each(|&j| f(j));
    }
}

/// Calls `f` with every job's number on `threads` threads at once.
fn at_once(threads: usize) -> impl Fn(usize, &(dyn Fn(usize) + Sync)) {
    move |count: usize, f: &(dyn Fn(usize) + Sync)| {
        let next = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| {
                    loop {
                        let j = next.fetch_add(1, Ordering::Relaxed);
                        if j >= count {
                            break;
                        }
                        f(j);
                    }
                });
            }
        });
    }
}

/// Every way of handing out the jobs leaves title memory, the copies' CPU
/// sides and the bytes written as the landing one piece after the other
/// does, with pieces in one block (of addresses or of the view's physical
/// pages) landed in the order given and never two at once: jobs in the
/// order given, backwards, shuffled, on four threads, on the kept threads
/// (`crate::shadow::wide_each`), and jobs from one piece to all of them.
#[test]
fn jobs_land_as_one_piece_after_the_other() {
    let mut seed = 0x5eed_1a2d_u64;
    for round in 0..60 {
        let count = 1 + random(&mut seed) % 120;
        let (start, pieces) = made(&mut seed, count);
        let alone = start.world();
        let one: Vec<usize> = (0..pieces.len()).map(|i| alone.land(&pieces, i)).collect();
        let expected = alone.end();
        for job in [1, BLOCK, 4 * BLOCK, usize::MAX] {
            let (jobs, groups) = plan_of(&start, &pieces, job);
            assert!(groups >= 1 && groups <= pieces.len() && jobs.len() <= groups);
            let mut shuffled: Vec<usize> = (0..jobs.len()).collect();
            for i in (1..shuffled.len()).rev() {
                shuffled.swap(i, random(&mut seed) % (i + 1));
            }
            let mut ways: Vec<(&str, Box<dyn Fn(usize, &(dyn Fn(usize) + Sync))>)> = vec![
                ("in order", Box::new(in_turn((0..jobs.len()).collect()))),
                (
                    "backwards",
                    Box::new(in_turn((0..jobs.len()).rev().collect())),
                ),
                ("shuffled", Box::new(in_turn(shuffled))),
                ("four threads", Box::new(at_once(4))),
            ];
            #[cfg(windows)]
            ways.push((
                "kept threads",
                Box::new(|count: usize, f: &(dyn Fn(usize) + Sync)| {
                    crate::shadow::wide_each(count, f)
                }),
            ));
            for (way, spread) in &ways {
                let world = start.world();
                let written = run(&jobs, &|i| world.land(&pieces, i), spread.as_ref());
                let what = format!("round {round}, jobs of {job} bytes, {way}");
                assert!(world.in_order(), "{what}: pieces of a block out of order");
                assert_eq!(written, one, "{what}: bytes written");
                let (title, held) = world.end();
                assert!(title == expected.0, "{what}: title memory");
                assert!(held == expected.1, "{what}: the copies' CPU sides");
            }
            ways.clear();
        }
    }
}

/// With the title writing bytes no landing writes back, while the jobs
/// land on four threads: those bytes end as the title last wrote them, and
/// every other byte as the landing one piece after the other leaves it.
#[test]
fn title_bytes_beside_the_landings_stay_the_titles() {
    let mut seed = 0x0b5e_55ed_u64;
    for round in 0..20 {
        let (start, pieces) = made(&mut seed, 100);
        let alone = start.world();
        (0..pieces.len()).for_each(|i| {
            alone.land(&pieces, i);
        });
        let expected = alone.end();
        let free: Vec<usize> = (0..start.title.len())
            .filter(|&p| !alone.stretched[p].load(Ordering::Relaxed))
            .collect();
        assert!(!free.is_empty());
        let (jobs, _) = plan_of(&start, &pieces, BLOCK);
        let world = start.world();
        let stop = AtomicBool::new(false);
        let last = std::thread::scope(|s| {
            let writer = s.spawn(|| {
                let mut value = 0u8;
                let mut passes = 0;
                while !stop.load(Ordering::Relaxed) || passes < 2 {
                    value = value.wrapping_add(1);
                    for &p in &free {
                        world.title[p].store(value, Ordering::Relaxed);
                    }
                    passes += 1;
                }
                value
            });
            run(&jobs, &|i| world.land(&pieces, i), &at_once(4));
            stop.store(true, Ordering::Relaxed);
            writer.join().unwrap()
        });
        let (title, held) = world.end();
        assert!(world.in_order(), "round {round}");
        assert!(held == expected.1, "round {round}: the copies' CPU sides");
        for (p, (&got, &want)) in title.iter().zip(&expected.0).enumerate() {
            let want = if free.binary_search(&p).is_ok() {
                last
            } else {
                want
            };
            assert_eq!(got, want, "round {round}: physical byte {p:#x}");
        }
    }
}

/// Pieces that share a key, directly or through other pieces, are one
/// group in one job, in the order given; groups go whole into jobs of
/// about the bytes asked for, in the order of their first pieces.
#[test]
fn pieces_that_may_share_bytes_are_one_group_in_order() {
    let k = |list: &[(usize, u64)]| list.to_vec();
    let keys = vec![
        k(&[(0, 1)]),
        k(&[(0, 2)]),
        k(&[(0, 3), (5, 9)]),
        k(&[(0, 1)]),
        k(&[(0, 4), (5, 9)]),
        k(&[(0, 2), (0, 4)]),
        k(&[(0, 7)]),
    ];
    let bytes = [10; 7];
    // 0-3; 1-5-4-2 (through block 4 and the view's page 9); 6.
    let (jobs, groups) = plan(&keys, &bytes, 1);
    assert_eq!(groups, 3);
    assert_eq!(jobs, vec![vec![0, 3], vec![1, 2, 4, 5], vec![6]]);
    let (jobs, _) = plan(&keys, &bytes, 25);
    assert_eq!(jobs, vec![vec![0, 3, 1, 2, 4, 5], vec![6]]);
    let (jobs, _) = plan(&keys, &bytes, usize::MAX);
    assert_eq!(jobs, vec![vec![0, 3, 1, 2, 4, 5, 6]]);
    assert_eq!(plan(&[], &[], 1), (Vec::<Vec<usize>>::new(), 0));
}

use super::REHASH_FOR_TEST;
use crate::shadow::Shadow;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use windows::Win32::Graphics::Direct3D12::ID3D12Resource;

const LEN: usize = 1 << 20;
const PIECE: usize = 64 << 10;
const PIECES: usize = LEN / PIECE;

fn rng(seed: u64) -> impl FnMut() -> u64 {
    let mut s = seed;
    move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    }
}

/// A title buffer of [`LEN`] bytes (on a piece's edge) with a copy in
/// the card's memory whose shader writes were declared once, so that
/// the card keeps a hash of each piece.
struct Copy {
    base: usize,
    copy: Arc<Shadow>,
    readback: ID3D12Resource,
}

impl Copy {
    fn new(seed: u64) -> Copy {
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc,
        };
        const MEM_WRITE_WATCH: u32 = 0x20_0000;
        let host = super::host().expect("a D3D12 device");
        assert!(host.card_ready(), "the card's pipelines");
        // SAFETY: a private allocation, kept until the process ends.
        let base = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                LEN,
                MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH,
                PAGE_READWRITE,
            )
        } as usize;
        assert_ne!(base, 0);
        assert_eq!(base % PIECE, 0, "allocations start on 64 KiB");
        let mut next = rng(seed);
        // SAFETY: the allocation's bytes.
        unsafe { std::slice::from_raw_parts_mut(base as *mut u8, LEN) }
            .iter_mut()
            .for_each(|b| *b = next() as u8);
        crate::shadow::register(base, base as u64, LEN as u64);
        let copy = crate::shadow::find(base as u64, LEN as u64).expect("the copy");
        assert!(copy.carded(), "the copy must be in the card's memory");
        let list = host.new_list().unwrap();
        copy.refresh(&list, base as u64, LEN as u64);
        copy.gpu_writes(&list, base as u64, LEN as u64);
        list.close();
        host.execute(&[&list]);
        let readback = host
            .read_back_buffer(LEN as u64)
            .expect("a read-back buffer");
        Copy {
            base,
            copy,
            readback,
        }
    }

    #[allow(clippy::mut_from_ref)]
    fn title(&self) -> &mut [u8] {
        // SAFETY: the allocation, which lives on; the test's thread only.
        unsafe { std::slice::from_raw_parts_mut(self.base as *mut u8, LEN) }
    }

    /// `len` bytes of `from` (the card side or its hashes), read back.
    fn read(&self, from: &ID3D12Resource, len: usize) -> Vec<u8> {
        let host = super::host().unwrap();
        let list = host.new_list().unwrap();
        // SAFETY: live buffers of `len` bytes or more.
        unsafe {
            list.list
                .CopyBufferRegion(&self.readback, 0, from, 0, len as u64)
        };
        list.close();
        host.execute(&[&list]);
        let mut mapped = std::ptr::null_mut();
        // SAFETY: the card wrote them, and was waited for.
        unsafe {
            self.readback.Map(0, None, Some(&mut mapped)).unwrap();
            let out = std::slice::from_raw_parts(mapped.cast::<u8>(), len).to_vec();
            self.readback.Unmap(0, None);
            out
        }
    }

    fn card(&self) -> Vec<u8> {
        self.read(&self.copy.buffer, LEN)
    }

    /// The card's hash of each piece, 16 bytes a piece.
    fn hashes(&self) -> Vec<u8> {
        let hashes = self
            .copy
            .card_sides()
            .and_then(|s| s.1)
            .expect("the card's hashes of the copy");
        self.read(&hashes, PIECES * 16)
    }

    /// The title changes `runs` (each byte of them), and the runs go
    /// into the CPU side and are noted for the card.
    fn change(&self, runs: &[(usize, usize)], flip: u8) {
        let title = self.title();
        for &(a, b) in runs {
            title[a..b].iter_mut().for_each(|t| *t ^= flip);
        }
        self.copy.card_put_runs(title, runs);
    }
}

/// [`super::CARD_COMPARE`]'s hash of a 64 KiB piece of a copy that
/// starts on a piece's edge.
fn piece_hash(bytes: &[u8]) -> [u8; 16] {
    const MUL: [u32; 4] = [0x0100_0193, 0x9E37_79B1, 0x85EB_CA77, 0xC2B2_AE3D];
    let mut h = [0x9E37_79B9u32, 0x85EB_CA6B, 0xC2B2_AE35, 0x27D4_EB2F];
    for chunk in bytes.chunks_exact(16) {
        for k in 0..4 {
            let w = u32::from_le_bytes(chunk[4 * k..4 * k + 4].try_into().unwrap());
            h[k] = (h[k] ^ w).wrapping_mul(MUL[k]);
        }
        h = [
            h[1] ^ (h[0] >> 15),
            h[2] ^ (h[1] >> 15),
            h[3] ^ (h[2] >> 15),
            h[0] ^ (h[3] >> 15),
        ];
    }
    let mut out = [0; 16];
    for k in 0..4 {
        out[4 * k..4 * k + 4].copy_from_slice(&h[k].to_le_bytes());
    }
    out
}

/// Random runs: in one piece, across piece edges, several in a piece,
/// in pieces that touch and in pieces apart, overlapping each other.
fn random_runs(next: &mut impl FnMut() -> u64, case: usize) -> Vec<(usize, usize)> {
    let count = [1usize, 2, 3, 16, 100, 1000, 5000][case % 7];
    let longest = [1usize, 16, 300, PIECE + 77][(case / 7) % 4];
    let mut runs: Vec<(usize, usize)> = (0..count)
        .map(|_| {
            let a = next() as usize % (LEN - 1);
            (a, (a + 1 + next() as usize % longest).min(LEN))
        })
        .collect();
    // Half the cases in address order, as the merge notes them.
    if case.is_multiple_of(2) {
        runs.sort_unstable();
    }
    runs
}

/// Every way, the pieces the ranges sent up touch are hashed again from
/// the bytes the card now holds, and no other piece's hash changes;
/// three copies given the same changes end with the same hashes byte
/// for byte. Random runs, 56 cases.
#[test]
#[ignore = "needs a D3D12 adapter; run alone"]
fn joined_rehash_leaves_the_hashes_a_pass_a_range_does() {
    let narrow = !super::card_hash_wide();
    // A pass a range, touching pieces joined, each piece once.
    let ways = [
        (2u8, Copy::new(0x5EED)),
        (1, Copy::new(0x5EED)),
        (3, Copy::new(0x5EED)),
    ];
    let apart = &ways[0].1;
    let host = super::host().unwrap();
    let mut next = rng(0xA11CE);
    let mut passes = [0u64; 3];
    for case in 0..56usize {
        let runs = random_runs(&mut next, case);
        let flip = 1 | next() as u8;
        let old = apart.hashes();
        for (k, (mode, copy)) in ways.iter().enumerate() {
            assert_eq!(copy.hashes(), old, "case {case}: the same hashes before");
            REHASH_FOR_TEST.store(*mode, Ordering::Relaxed);
            copy.change(&runs, flip);
            let counted = super::CARD_PASSES[0].load(Ordering::Relaxed);
            host.card_sync_timed();
            passes[k] += super::CARD_PASSES[0].load(Ordering::Relaxed) - counted;
        }
        REHASH_FOR_TEST.store(0, Ordering::Relaxed);
        let card = apart.card();
        assert!(
            card == apart.title(),
            "case {case}: the card side took the title's bytes"
        );
        let got = apart.hashes();
        for (mode, copy) in &ways[1..] {
            assert!(
                copy.card() == card,
                "case {case}: every copy holds the same bytes"
            );
            assert_eq!(
                copy.hashes(),
                got,
                "case {case}: way {mode} leaves the hashes a pass a range does"
            );
        }
        for p in 0..PIECES {
            let touched = runs
                .iter()
                .any(|&(a, b)| a < (p + 1) * PIECE && b > p * PIECE);
            let (hash, was) = (&got[16 * p..16 * p + 16], &old[16 * p..16 * p + 16]);
            if !touched {
                assert_eq!(hash, was, "case {case}: piece {p} was not sent up");
            } else if narrow {
                assert_eq!(
                    hash,
                    piece_hash(&card[p * PIECE..(p + 1) * PIECE]),
                    "case {case}: piece {p} hashed again"
                );
            }
        }
    }
    assert!(
        passes[1] <= passes[2] && passes[2] < passes[0],
        "joined took fewer passes: {passes:?}"
    );
    eprintln!(
        "joined_rehash_leaves_the_hashes_a_pass_a_range_does: 56 cases, the same hashes; hash passes a range each {}, touching joined {}, each piece once {}",
        passes[0], passes[1], passes[2]
    );
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// The submitting thread's time in `card_before` and the card's for
/// ranges sent up, the three ways of [`super::rehash_runs`]. Cases:
/// `pages` x `runs` changed runs of two bytes, evenly apart in each of
/// the first `pages` 64 KiB pieces. `REHASH_BENCH_ROUNDS` (15) rounds
/// measured after two, the three ways in turn inside a round. Prints a
/// table; asserts nothing.
#[test]
#[ignore = "a benchmark: needs a D3D12 adapter and a quiet machine"]
fn rehash_bench() {
    let rounds: usize = std::env::var("REHASH_BENCH_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);
    let host = super::host().unwrap();
    let copy = Copy::new(0xBE7C);
    println!(
        "rehash_bench: median of {rounds} rounds; VICTORY_CARD_HASH_WIDE={}",
        std::env::var("VICTORY_CARD_HASH_WIDE").unwrap_or_else(|_| "unset".into())
    );
    println!(
        "| pages x runs | way | ranges sent | hash passes | submitting thread (us) | card (us) |"
    );
    println!("|---|---|---:|---:|---:|---:|");
    let cases: &[(usize, usize)] = &[
        (1, 1),
        (1, 16),
        (1, 256),
        (1, 4096),
        (1, 16384),
        (4, 16384),
        (2, 1),
        (4, 1),
        (16, 1),
        (16, 16),
        (16, 256),
    ];
    let mut flip = 0u8;
    for &(pages, runs) in cases {
        let stride = PIECE / runs;
        let ranges: Vec<(usize, usize)> = (0..pages)
            .flat_map(|p| {
                (0..runs).map(move |r| (p * PIECE + r * stride, p * PIECE + r * stride + 2))
            })
            .collect();
        // Per way: the submitting thread's times, the card's, ranges, passes.
        let mut out: [(Vec<f64>, Vec<f64>, u64, u64); 3] = Default::default();
        for round in 0..rounds + 2 {
            for k in [round % 3, (round + 1) % 3, (round + 2) % 3] {
                REHASH_FOR_TEST.store([2, 1, 3][k], Ordering::Relaxed);
                flip = flip.wrapping_add(2) | 1;
                copy.change(&ranges, flip);
                let counted = (
                    crate::shadow::CARD_STATS[1].load(Ordering::Relaxed),
                    super::CARD_PASSES[0].load(Ordering::Relaxed),
                );
                let (spent, card) = host.card_sync_timed();
                let o = &mut out[k];
                o.2 = crate::shadow::CARD_STATS[1].load(Ordering::Relaxed) - counted.0;
                o.3 = super::CARD_PASSES[0].load(Ordering::Relaxed) - counted.1;
                if round >= 2 {
                    o.0.push(spent.as_secs_f64() * 1e6);
                    o.1.push(card * 1e6);
                }
            }
        }
        for (k, way) in [
            "a pass a range (=0)",
            "touching joined (=1)",
            "each piece once (default)",
        ]
        .into_iter()
        .enumerate()
        {
            let o = &mut out[k];
            println!(
                "| {pages} x {runs} | {way} | {} | {} | {:.1} | {:.1} |",
                o.2,
                o.3,
                median(&mut o.0),
                median(&mut o.1)
            );
        }
    }
    REHASH_FOR_TEST.store(0, Ordering::Relaxed);
}

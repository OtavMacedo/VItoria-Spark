//! The placed textures the title fills, by the title memory they lie in:
//! for letting go of their host copies when the runtime unmaps or maps
//! that memory (`crate::resource::evict_placed_in`).
//!
//! That letting go walked every title texture alive at each unmapping or
//! mapping, working out the bytes of each placed one from its layout to
//! see whether it lies in the range: some 1.5 ms a call with the tens of
//! thousands of textures of a loaded world, at some 140,000 calls a run
//! (most of them while it loads), against 0.05 ms for the rest of the
//! call. Here each texture is kept, with its bytes worked out once, in
//! every 64 KiB bucket of title memory it touches, so a range is answered
//! from the buckets it covers: the textures that lie there and the few
//! that share a bucket with its ends.
//!
//! - A texture lies in `[start, end)` for as long as it is here: its
//!   description and its address never change (the title places a new
//!   resource, with a new address and description, rather than changing
//!   one), so its bytes are worked out once.
//! - Textures at the same address (one memory viewed with two
//!   descriptions) are each here, told apart by the resource.
//! - A texture leaves before it is freed (`Drop for Resource`), under the
//!   lock of the table that holds this, so whoever walks a range under that
//!   lock may read every texture it finds.
//! - A texture over many buckets is met once a bucket, and answered once:
//!   in the bucket of the first byte it shares with the range.

use std::collections::BTreeMap;

/// Bits of an address below its bucket: 64 KiB buckets, the console's
/// large page and the alignment most placed textures have.
pub(crate) const BUCKET_BITS: u32 = 16;

/// Placed textures (by resource address) and the title memory each lies
/// in.
pub(crate) struct PlacedAt {
    /// (bucket, texture) for every bucket a texture's bytes touch, with
    /// the texture's bytes `[start, end)`.
    by_bucket: BTreeMap<(u64, usize), (u64, u64)>,
    /// Each texture's bytes, to find its buckets again when it leaves.
    extent: BTreeMap<usize, (u64, u64)>,
}

impl PlacedAt {
    pub(crate) const fn new() -> Self {
        PlacedAt {
            by_bucket: BTreeMap::new(),
            extent: BTreeMap::new(),
        }
    }

    /// Texture `texture` lies in `[start, start + len)`. A texture of no
    /// bytes lies nowhere and is not kept; one kept already is kept with
    /// its new bytes instead. Whether it is kept.
    pub(crate) fn insert(&mut self, texture: usize, start: u64, len: u64) -> bool {
        self.remove(texture);
        if len == 0 {
            return false;
        }
        let end = start.saturating_add(len);
        if end == start {
            return false;
        }
        self.extent.insert(texture, (start, end));
        for bucket in start >> BUCKET_BITS..=(end - 1) >> BUCKET_BITS {
            self.by_bucket.insert((bucket, texture), (start, end));
        }
        true
    }

    /// Texture `texture` leaves; whether it was here.
    pub(crate) fn remove(&mut self, texture: usize) -> bool {
        let Some((start, end)) = self.extent.remove(&texture) else {
            return false;
        };
        for bucket in start >> BUCKET_BITS..=(end - 1) >> BUCKET_BITS {
            self.by_bucket.remove(&(bucket, texture));
        }
        true
    }

    /// Textures kept.
    #[cfg(any(test, feature = "diagnostics"))]
    pub(crate) fn len(&self) -> usize {
        self.extent.len()
    }

    /// Whether `texture` is kept.
    #[cfg(test)]
    pub(crate) fn contains(&self, texture: usize) -> bool {
        self.extent.contains_key(&texture)
    }

    /// The bytes `[start, end)` texture `texture` is kept with, if it is
    /// kept (for [`check`]'s log).
    #[cfg(any(test, feature = "diagnostics"))]
    pub(crate) fn extent_of(&self, texture: usize) -> Option<(u64, u64)> {
        self.extent.get(&texture).copied()
    }

    /// Calls `each` once for every texture that shares a byte with `[base,
    /// base + len)` (as `crate::resource::ranges_overlap` says), in bucket
    /// order. Returns how many entries were looked at: the cost of the
    /// call, for the log.
    pub(crate) fn each_over(&self, base: u64, len: u64, mut each: impl FnMut(usize)) -> u64 {
        let end = base.saturating_add(len);
        if len == 0 || end == base {
            return 0;
        }
        let (first, last) = (base >> BUCKET_BITS, (end - 1) >> BUCKET_BITS);
        let mut looked = 0;
        for (&(bucket, texture), &(start, stop)) in
            self.by_bucket.range((first, 0)..=(last, usize::MAX))
        {
            looked += 1;
            // Over the range, and this is the bucket of the first byte
            // they share (a texture over several buckets is met in each).
            if start < end && base < stop && bucket == start.max(base) >> BUCKET_BITS {
                each(texture);
            }
        }
        looked
    }
}

/// The index checked against a walk over every texture, one call in so
/// many (`VICTORY_SWEEP_CHECK`, the diagnostics build alone; off by
/// default): at such a call of `crate::resource::evict_placed_in` both
/// find the placed textures over the range, and what one found and the
/// other did not is counted and, for the first calls that differ, logged.
/// The walk is the one `VICTORY_SWEEP_WALK=1` does (some 1.5 ms a call with
/// a loaded world's textures), so one call in 64 by default.
///
/// - `VICTORY_SWEEP_CHECK=1`: one call in [`DEFAULT_EVERY`]; `=N` (2 or
///   more): one call in N; unset, `0` or anything else: off.
/// - Not in the table of switches that may alternate (`crate::switches`):
///   it changes nothing but adds a walk to the calls it checks, so an arm
///   with it would only measure its own cost. Read once.
#[cfg(any(test, feature = "diagnostics"))]
pub(crate) mod check {
    #[cfg(feature = "diagnostics")]
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    /// One call in so many with `VICTORY_SWEEP_CHECK=1`.
    pub(crate) const DEFAULT_EVERY: u64 = 64;

    /// The calls logged when they differ; the rest are only counted.
    #[cfg(feature = "diagnostics")]
    const LOGGED: u64 = 16;

    /// One call in how many is checked, from the variable's value; 0 off.
    pub(crate) fn every_from(value: Option<&str>) -> u64 {
        match value.map(str::trim) {
            Some("1") => DEFAULT_EVERY,
            Some(v) => v.parse::<u64>().ok().filter(|&n| n >= 2).unwrap_or(0),
            None => 0,
        }
    }

    /// [`every_from`] of `VICTORY_SWEEP_CHECK`, read once.
    #[cfg(feature = "diagnostics")]
    pub(crate) fn every() -> u64 {
        static EVERY: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        *EVERY.get_or_init(|| every_from(std::env::var("VICTORY_SWEEP_CHECK").ok().as_deref()))
    }

    /// Calls seen with the check on; then calls checked, calls that
    /// differed, textures only the walk found and textures only the index
    /// found, since the start.
    #[cfg(feature = "diagnostics")]
    static COUNTS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

    /// Whether this call is one to check.
    #[cfg(feature = "diagnostics")]
    pub(crate) fn due() -> bool {
        let every = every();
        every != 0 && COUNTS[0].fetch_add(1, Relaxed).is_multiple_of(every)
    }

    /// What the walk found and the index did not, and what the index found
    /// and the walk did not: each sorted, without repeats.
    pub(crate) fn differ(mut walk: Vec<usize>, mut index: Vec<usize>) -> (Vec<usize>, Vec<usize>) {
        for v in [&mut walk, &mut index] {
            v.sort_unstable();
            v.dedup();
        }
        let only = |a: &[usize], b: &[usize]| -> Vec<usize> {
            a.iter()
                .copied()
                .filter(|t| b.binary_search(t).is_err())
                .collect()
        };
        (only(&walk, &index), only(&index, &walk))
    }

    /// A call checked over `[base, base + len)`: how many textures only the
    /// walk and only the index found, and a word on each of them (logged
    /// for the first calls that differ).
    #[cfg(feature = "diagnostics")]
    pub(crate) fn note(base: u64, len: u64, walk_only: usize, index_only: usize, said: &[String]) {
        COUNTS[1].fetch_add(1, Relaxed);
        if walk_only == 0 && index_only == 0 {
            return;
        }
        let nth = COUNTS[2].fetch_add(1, Relaxed);
        COUNTS[3].fetch_add(walk_only as u64, Relaxed);
        COUNTS[4].fetch_add(index_only as u64, Relaxed);
        if nth < LOGGED {
            tracing::warn!(
                "placed textures over unmapped title memory {base:#x}+{len:#x}: the index and a walk over every texture differ (VICTORY_SWEEP_CHECK): {walk_only} only the walk found, {index_only} only the index found: {}",
                said.join("; ")
            );
        }
    }

    /// For the memory line: one call in how many, calls checked, calls that
    /// differed, textures only the walk found and only the index found.
    /// `None` with the check off or before its first call.
    #[cfg(feature = "diagnostics")]
    pub(crate) fn counts() -> Option<[u64; 5]> {
        let checked = COUNTS[1].load(Relaxed);
        (every() != 0 && checked != 0).then(|| {
            [
                every(),
                checked,
                COUNTS[2].load(Relaxed),
                COUNTS[3].load(Relaxed),
                COUNTS[4].load(Relaxed),
            ]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The textures over a range as a walk over every one finds them.
    fn walk(all: &[(usize, u64, u64)], base: u64, len: u64) -> Vec<usize> {
        let mut v: Vec<usize> = all
            .iter()
            .filter(|&&(_, start, size)| {
                size != 0
                    && len != 0
                    && start < base.saturating_add(len)
                    && base < start.saturating_add(size)
            })
            .map(|&(t, _, _)| t)
            .collect();
        v.sort_unstable();
        v
    }

    fn over(index: &PlacedAt, base: u64, len: u64) -> Vec<usize> {
        let mut v = Vec::new();
        index.each_over(base, len, |t| v.push(t));
        v.sort_unstable();
        v
    }

    /// A small generator, so the test needs no crate and repeats.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    #[test]
    fn the_index_finds_what_a_walk_over_every_texture_finds() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut index = PlacedAt::new();
        let mut all: Vec<(usize, u64, u64)> = Vec::new();
        for round in 0..20_000usize {
            match rng.below(10) {
                // A texture placed: mostly small, some over many buckets,
                // some at an address another has.
                0..=5 => {
                    let start = if !all.is_empty() && rng.below(4) == 0 {
                        all[rng.below(all.len() as u64) as usize].1
                    } else {
                        rng.below(64 << 20) & !0xff
                    };
                    let size = match rng.below(8) {
                        0 => 0,
                        1 => rng.below(8 << 20),
                        _ => rng.below(256 << 10) + 1,
                    };
                    let texture = 0x1000 + round * 16;
                    let kept = index.insert(texture, start, size);
                    assert_eq!(kept, size != 0);
                    if kept {
                        all.push((texture, start, size));
                    }
                }
                // A texture freed.
                6 if !all.is_empty() => {
                    let (t, _, _) = all.swap_remove(rng.below(all.len() as u64) as usize);
                    assert!(index.remove(t));
                    assert!(!index.remove(t), "only once");
                }
                // A range unmapped.
                _ => {
                    let base = rng.below(72 << 20);
                    let len = match rng.below(4) {
                        0 => rng.below(0x100),
                        1 => rng.below(16 << 20),
                        _ => rng.below(512 << 10),
                    };
                    assert_eq!(
                        over(&index, base, len),
                        walk(&all, base, len),
                        "{base:#x}+{len:#x}"
                    );
                }
            }
            assert_eq!(index.len(), all.len());
        }
        // Every texture, from a range over all of title memory.
        assert_eq!(over(&index, 0, u64::MAX), walk(&all, 0, u64::MAX));
    }

    #[test]
    fn textures_at_one_address_are_each_found_and_leave_alone() {
        let mut index = PlacedAt::new();
        assert!(index.insert(1, 0x10_0000, 0x1_0000));
        assert!(index.insert(2, 0x10_0000, 0x4_0000));
        assert!(index.insert(3, 0x10_0000, 0x100));
        assert_eq!(over(&index, 0x10_0000, 1), [1, 2, 3]);
        // Past the small ones' ends: the big one alone.
        assert_eq!(over(&index, 0x11_0000, 0x1000), [2]);
        assert!(index.remove(2));
        assert_eq!(over(&index, 0x10_0000, 0x4_0000), [1, 3]);
        assert!(index.contains(1) && !index.contains(2) && index.contains(3));
        assert_eq!(index.extent_of(3), Some((0x10_0000, 0x10_0100)));
        assert_eq!(index.extent_of(2), None);
    }

    #[test]
    fn a_texture_over_many_buckets_is_answered_once() {
        let mut index = PlacedAt::new();
        index.insert(7, 0x1_8000, 10 << 16);
        let mut n = 0;
        let looked = index.each_over(0, 1 << 24, |_| n += 1);
        assert_eq!(n, 1);
        assert_eq!(looked, 11, "one entry in each bucket it touches");
        // A range inside it, starting mid-bucket.
        n = 0;
        index.each_over(0x5_4321, 0x2_0000, |_| n += 1);
        assert_eq!(n, 1);
    }

    #[test]
    fn a_texture_that_leaves_takes_all_its_buckets() {
        let mut index = PlacedAt::new();
        index.insert(1, 0, 5 << 16);
        index.insert(2, 3 << 16, 1 << 16);
        assert!(index.remove(1));
        assert_eq!(index.by_bucket.len(), 1);
        assert_eq!(over(&index, 0, 8 << 16), [2]);
        // Kept again with other bytes: the old buckets go.
        index.insert(2, 9 << 16, 1);
        assert_eq!(index.by_bucket.len(), 1);
        assert!(over(&index, 3 << 16, 1 << 16).is_empty());
        assert_eq!(over(&index, 9 << 16, 1), [2]);
    }

    #[test]
    fn the_check_reads_its_variable() {
        use check::{DEFAULT_EVERY, every_from};
        assert_eq!(every_from(None), 0);
        assert_eq!(every_from(Some("0")), 0);
        assert_eq!(every_from(Some("")), 0);
        assert_eq!(every_from(Some("yes")), 0);
        assert_eq!(every_from(Some("1")), DEFAULT_EVERY);
        assert_eq!(every_from(Some(" 1 ")), DEFAULT_EVERY);
        assert_eq!(every_from(Some("2")), 2);
        assert_eq!(every_from(Some("1000")), 1000);
    }

    #[test]
    fn the_check_says_what_each_side_alone_found() {
        use check::differ;
        assert_eq!(differ(vec![3, 1, 2], vec![2, 3, 1]), (vec![], vec![]));
        assert_eq!(differ(vec![], vec![]), (vec![], vec![]));
        // Repeats count once; each side's own, sorted.
        assert_eq!(
            differ(vec![5, 1, 5, 9], vec![9, 4, 4]),
            (vec![1, 5], vec![4])
        );
        assert_eq!(differ(vec![7], vec![]), (vec![7], vec![]));
        assert_eq!(differ(vec![], vec![8, 6]), (vec![], vec![6, 8]));
    }

    #[test]
    fn ranges_of_no_bytes_and_the_top_of_memory() {
        let mut index = PlacedAt::new();
        assert!(!index.insert(1, 0x1000, 0));
        assert!(index.insert(2, u64::MAX - 0x1000, 0x1000));
        assert!(over(&index, 0x1000, 0).is_empty());
        assert_eq!(over(&index, u64::MAX - 0x800, 0x800), [2]);
        assert_eq!(over(&index, u64::MAX - 0x800, u64::MAX), [2]);
        assert!(index.remove(2));
        assert_eq!(index.len(), 0);
    }
}

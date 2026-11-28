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

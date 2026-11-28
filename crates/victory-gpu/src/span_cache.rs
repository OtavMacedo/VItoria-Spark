//! The spans of host copies kept from pass to pass for the pass over the
//! view heap's buffer views without marks (`draw::State::heap_buffers`,
//! `VICTORY_BUFFER_SWEEP`).
//!
//! That pass keeps, per slot, the host copy and range its buffer view
//! reads, and looks again only at the slots written since (all of them
//! every 16 passes). But it then worked out the span of every host copy
//! from every slot kept, each pass: a walk over the 80,000 to 200,000 slots
//! of the world, hashed by copy, for the few hundred that changed. Here
//! the span of each copy is kept with the slots that read it, and worked
//! out again only for a copy one of whose slots went at an end of its span.
//!
//! What is kept is made from the pass's own map of slots and nothing else:
//! each change of that map is made here too ([`Spans::remove`],
//! [`Spans::insert`], [`Spans::clear`]), so after every pass
//! - a copy is here if and only if some slot of the map reads it, with
//!   exactly those slots;
//! - its span is the lowest start and highest end of their ranges, as the
//!   walk would find it: a range added widens it; a range gone from inside
//!   it leaves it as it was (other slots hold both ends); a range gone at
//!   an end has it worked out again from the slots left.
//!
//! A span therefore never covers a range no slot reads now: a view whose
//! buffer moved, shrank or went is a written slot, its old range leaves
//! the map, and the span with it. Copies are told apart by the address of
//! their shared object, as the walk did, and each copy here holds a
//! reference to it: no copy made after one was let go can take its
//! address while a span of the old one is kept. Without the switch the
//! cache is dropped at each pass; with it, made whole from the map at the
//! first pass with it on. So it may alternate (`SPARK_AB`).
//!
//! After XaviFortes/VIctory-RDNA commit f8eb38d (spans kept per part of
//! the heap's slots).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Whether the pass without marks keeps its spans from pass to pass
/// (`VICTORY_BUFFER_SWEEP=1`; off by default: worked out from every slot
/// each pass, as before). In turn with `alt` or `SPARK_AB`.
pub(crate) fn on() -> bool {
    crate::switches::BUFFER_SWEEP.on()
}

struct Span<C> {
    copy: C,
    slots: HashSet<u32>,
    from: u64,
    to: u64,
    /// A range at one of its ends went: worked out again from `slots`.
    stale: bool,
}

/// Per host copy (by the address of its shared object), the slots that
/// read it and the span they cover.
pub(crate) struct Spans<C> {
    copies: HashMap<usize, Span<C>>,
}

impl<C: Clone> Spans<C> {
    pub(crate) fn new() -> Self {
        Spans {
            copies: HashMap::new(),
        }
    }

    /// Made whole from the pass's map: `(slot, key, copy, from, to)`.
    pub(crate) fn of<'a>(ranges: impl IntoIterator<Item = (u32, usize, &'a C, u64, u64)>) -> Self
    where
        C: 'a,
    {
        let mut spans = Spans::new();
        for (slot, key, copy, from, to) in ranges {
            spans.insert(slot, key, copy, from, to);
        }
        BUILT.fetch_add(1, Relaxed);
        spans
    }

    /// The map was emptied.
    pub(crate) fn clear(&mut self) {
        self.copies.clear();
    }

    /// Slot `slot`, which read `[from, to)` of copy `key`, left the map.
    pub(crate) fn remove(&mut self, slot: u32, key: usize, from: u64, to: u64) {
        let Some(span) = self.copies.get_mut(&key) else {
            return;
        };
        if !span.slots.remove(&slot) {
            return;
        }
        if span.slots.is_empty() {
            self.copies.remove(&key);
        } else if from <= span.from || to >= span.to {
            span.stale = true;
        }
    }

    /// Slot `slot` now reads `[from, to)` of copy `copy` (`key`: the
    /// address of its shared object). The map held nothing for it.
    pub(crate) fn insert(&mut self, slot: u32, key: usize, copy: &C, from: u64, to: u64) {
        let span = self.copies.entry(key).or_insert_with(|| Span {
            copy: copy.clone(),
            slots: HashSet::new(),
            from,
            to,
            stale: false,
        });
        span.slots.insert(slot);
        span.from = span.from.min(from);
        span.to = span.to.max(to);
    }

    /// Per copy, the span its slots cover: `(key, copy, from, to)`.
    /// `range`: the range of a slot in the map.
    pub(crate) fn spans(
        &mut self,
        range: impl Fn(u32) -> Option<(u64, u64)>,
    ) -> Vec<(usize, C, u64, u64)> {
        let mut again = 0u64;
        let spans: Vec<_> = self
            .copies
            .iter_mut()
            .map(|(&key, span)| {
                if span.stale {
                    span.stale = false;
                    again += 1;
                    let (mut from, mut to) = (u64::MAX, 0);
                    for (f, t) in span.slots.iter().filter_map(|&s| range(s)) {
                        from = from.min(f);
                        to = to.max(t);
                    }
                    (span.from, span.to) = (from, to);
                }
                (key, span.copy.clone(), span.from, span.to)
            })
            .collect();
        STATS[1].fetch_add(spans.len() as u64 - again, Relaxed);
        STATS[2].fetch_add(again, Relaxed);
        spans
    }
}

/// Times the cache was made whole from the pass's map (the first pass with
/// the switch on, and after each turn off).
static BUILT: AtomicU64 = AtomicU64::new(0);
/// Passes with the cache, spans served as kept, spans worked out again.
static STATS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// Counts a pass with the cache; every 256 of them, a line of what it did.
pub(crate) fn note_pass(views: usize) {
    if STATS[0].fetch_add(1, Relaxed) % 256 != 255 {
        return;
    }
    tracing::info!(
        "spans of host copies kept from pass to pass (VICTORY_BUFFER_SWEEP), last 256 passes over the heap's buffer views: {} spans served as kept, {} worked out again from their slots; made whole {} times so far; {views} views kept now",
        STATS[1].swap(0, Relaxed),
        STATS[2].swap(0, Relaxed),
        BUILT.load(Relaxed),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    type Map = HashMap<u32, (Arc<u64>, u64, u64)>;

    fn key(copy: &Arc<u64>) -> usize {
        Arc::as_ptr(copy) as usize
    }

    /// What the pass did each time without the cache: every slot, hashed
    /// by copy.
    fn walk(map: &Map) -> Vec<(usize, u64, u64)> {
        let mut spans: HashMap<usize, (u64, u64)> = HashMap::new();
        for (copy, from, to) in map.values() {
            let span = spans.entry(key(copy)).or_insert((*from, *to));
            span.0 = span.0.min(*from);
            span.1 = span.1.max(*to);
        }
        let mut spans: Vec<_> = spans.into_iter().map(|(k, (f, t))| (k, f, t)).collect();
        spans.sort_unstable();
        spans
    }

    fn cached(cache: &mut Spans<Arc<u64>>, map: &Map) -> Vec<(usize, u64, u64)> {
        let mut spans: Vec<_> = cache
            .spans(|s| map.get(&s).map(|r| (r.1, r.2)))
            .into_iter()
            .map(|(k, copy, f, t)| {
                // The copy served is the one its key names.
                assert_eq!(k, key(&copy));
                (k, f, t)
            })
            .collect();
        spans.sort_unstable();
        spans
    }

    /// One slot changed as the pass changes it: out of the map, and in
    /// again with its new range if it has one.
    fn set(
        map: &mut Map,
        cache: &mut Spans<Arc<u64>>,
        slot: u32,
        new: Option<(Arc<u64>, u64, u64)>,
    ) {
        if let Some((copy, from, to)) = map.remove(&slot) {
            cache.remove(slot, key(&copy), from, to);
        }
        if let Some((copy, from, to)) = new {
            cache.insert(slot, key(&copy), &copy, from, to);
            map.insert(slot, (copy, from, to));
        }
    }

    #[test]
    fn a_range_that_moves_shrinks_or_goes_leaves_its_span() {
        let a = Arc::new(1u64);
        let mut map = Map::new();
        let mut cache = Spans::new();
        set(&mut map, &mut cache, 1, Some((a.clone(), 0, 64)));
        set(&mut map, &mut cache, 2, Some((a.clone(), 64, 128)));
        set(&mut map, &mut cache, 3, Some((a.clone(), 128, 4096)));
        assert_eq!(cached(&mut cache, &map), vec![(key(&a), 0, 4096)]);
        // The view at the top shrank (its buffer was made smaller).
        set(&mut map, &mut cache, 3, Some((a.clone(), 128, 192)));
        assert_eq!(cached(&mut cache, &map), vec![(key(&a), 0, 192)]);
        // The view at the bottom moved up.
        set(&mut map, &mut cache, 1, Some((a.clone(), 96, 100)));
        assert_eq!(cached(&mut cache, &map), vec![(key(&a), 64, 192)]);
        // One from inside went: the ends stay.
        set(&mut map, &mut cache, 1, None);
        assert_eq!(cached(&mut cache, &map), vec![(key(&a), 64, 192)]);
        // The rest went: no span is served for the copy any more, and the
        // cache holds no reference to it.
        set(&mut map, &mut cache, 2, None);
        set(&mut map, &mut cache, 3, None);
        assert!(cached(&mut cache, &map).is_empty());
        assert_eq!(Arc::strong_count(&a), 1);
    }

    /// A copy let go and another made, perhaps at the same address: the
    /// old copy's span never stands for the new one.
    #[test]
    fn a_copy_let_go_lends_its_span_to_no_other() {
        let mut map = Map::new();
        let mut cache = Spans::new();
        let old = Arc::new(7u64);
        set(&mut map, &mut cache, 5, Some((old.clone(), 0, 1 << 20)));
        // The slot is written with a view of another buffer: the old copy
        // is let go by everything but whoever still holds it.
        set(&mut map, &mut cache, 5, None);
        drop(old);
        let new = Arc::new(8u64);
        set(&mut map, &mut cache, 5, Some((new.clone(), 4096, 8192)));
        assert_eq!(cached(&mut cache, &map), vec![(key(&new), 4096, 8192)]);
        // Kept spans hold their copy: while a slot reads it no other copy
        // can be made at its address.
        let held = Arc::new(9u64);
        set(&mut map, &mut cache, 6, Some((held.clone(), 0, 16)));
        let held_key = key(&held);
        drop(held);
        let other = Arc::new(10u64);
        assert_ne!(key(&other), held_key);
    }

    /// Random changes as the pass makes them, the map emptied now and
    /// then: after each pass the cache serves what the walk over every
    /// slot finds.
    #[test]
    fn the_cache_serves_what_the_walk_over_every_slot_finds() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let mut copies: Vec<Arc<u64>> = (0..6).map(Arc::new).collect();
        let mut map = Map::new();
        let mut cache = Spans::new();
        for pass in 0..2_000u64 {
            if pass % 97 == 0 {
                map.clear();
                cache.clear();
            }
            for _ in 0..next(40) {
                let slot = next(300) as u32;
                let new = (next(5) != 0).then(|| {
                    let copy = copies[next(copies.len() as u64) as usize].clone();
                    let from = next(1 << 16) & !15;
                    (copy, from, from + 16 + (next(64) << 4))
                });
                set(&mut map, &mut cache, slot, new);
            }
            // Now and then a copy is let go by the list of copies and a new
            // one made (its old slots keep the old until written).
            if next(10) == 0 {
                let i = next(copies.len() as u64) as usize;
                copies[i] = Arc::new(pass);
            }
            assert_eq!(cached(&mut cache, &map), walk(&map), "pass {pass}");
        }
        // Made whole from the map: the same.
        let mut whole = Spans::of(map.iter().map(|(&s, (c, f, t))| (s, key(c), c, *f, *t)));
        assert_eq!(cached(&mut whole, &map), walk(&map));
    }
}

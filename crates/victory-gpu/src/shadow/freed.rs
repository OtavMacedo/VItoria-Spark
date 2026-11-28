//! Copies of title memory the title has given up, let go at once
//! (`VICTORY_RELEASE_FREED=1`, `crate::freed`).
//!
//! A host copy of a window of title memory waits for the idle sweep
//! ([`super::let_go_idle`]) and a title buffer's copy for the buffer's
//! release ([`super::unregister`]), whatever happens to the memory under
//! it. When the runtime unmaps or maps pages there, or frees the memory,
//! what the copy holds is no longer what the title has there: a copy
//! nothing holds is memory of the PC's kept for nothing.

use super::{Arc, BTreeMap, Entry, LOOSE, Ordering, Shadow, WINDOW, buffers, tables_changed};
use std::sync::atomic::AtomicU64;

/// Since the start: copies let go by [`freed_let_go`] and their bytes.
pub(crate) static FREED_GONE: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];

/// Whether a copy may go now: only the table holds it (no list not yet
/// reset, no pass over the heap, no submission comparing it), only the
/// copy holds its host buffer (no descriptor slot views it), no range
/// shaders write waits to go back to title memory, and it is a copy (what
/// is in place, `crate::inplace`, has its own path). What
/// [`super::let_go_idle`] asks, without the idle time.
fn unheld(shadow: &Arc<Shadow>) -> bool {
    shadow.place.is_none()
        && Arc::strong_count(shadow) == 1
        && crate::host::references(&shadow.buffer) == 1
        && shadow
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
}

/// The keys of `map` (start, length) whose span touches `[at, end)`; a
/// window starts at most [`WINDOW`] below what it holds.
fn touching<V>(map: &BTreeMap<(u64, u64), V>, at: u64, end: u64) -> Vec<(u64, u64)> {
    if end <= at {
        return Vec::new();
    }
    map.range((at.saturating_sub(WINDOW), 0)..(end, 0))
        .map(|(key, _)| *key)
        .filter(|key| key.0.saturating_add(key.1) > at && key.0 < end)
        .collect()
}

/// The title memory `[at, at + len)` was unmapped, mapped anew or freed:
/// the host copies of windows there that nothing holds leave the table of
/// windows, and the copies of title buffers there that nothing holds leave
/// their entry (the buffer stays registered; its next use makes a copy
/// anew). A copy that a list or a slot still holds stays, as before: the
/// card may still read it. Those that go wait for the submissions running
/// now ([`crate::host::retire`]). Returns how many went.
///
/// As in [`super::let_go_idle`], each copy is looked at again after the
/// tables' count moved ([`super::TABLES_CHANGED`]): a thread that took one
/// from its memory of asks before the count moved holds it by then.
pub(crate) fn freed_let_go(at: u64, len: u64) -> u64 {
    if len == 0 {
        return 0;
    }
    let end = at.saturating_add(len);
    let mut gone: Vec<Arc<Shadow>> = Vec::new();
    {
        // Held from the look to the removal: `loose` hands a copy out
        // under the same lock.
        let mut map = LOOSE.lock().unwrap_or_else(|e| e.into_inner());
        let keys: Vec<(u64, u64)> = touching(&map, at, end)
            .into_iter()
            .filter(|key| map.get(key).is_some_and(unheld))
            .collect();
        if !keys.is_empty() {
            tables_changed();
            for key in keys {
                if map.get(&key).is_some_and(unheld) {
                    gone.extend(map.remove(&key));
                }
            }
        }
    }
    {
        // As `lookup` finds a buffer: among the 256 that start below.
        let mut map = buffers();
        let hit = |entry: &Entry| entry.end > at && entry.shadow.as_ref().is_some_and(unheld);
        if map.range(..(end, 0)).rev().take(256).any(|(_, e)| hit(e)) {
            tables_changed();
            for (_, entry) in map.range_mut(..(end, 0)).rev().take(256) {
                if hit(entry) {
                    gone.extend(entry.shadow.take());
                }
            }
        }
    }
    let n = gone.len() as u64;
    if n != 0 {
        FREED_GONE[0].fetch_add(n, Ordering::Relaxed);
        FREED_GONE[1].fetch_add(gone.iter().map(|s| s.len).sum::<u64>(), Ordering::Relaxed);
    }
    for shadow in gone {
        crate::host::retire(shadow);
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touching_finds_the_windows_over_a_range() {
        let mut map = BTreeMap::new();
        for key in [
            (0u64, WINDOW),
            (WINDOW, WINDOW),
            (2 * WINDOW, 0x1000),
            (3 * WINDOW, WINDOW),
        ] {
            map.insert(key, ());
        }
        assert_eq!(touching(&map, WINDOW + 8, WINDOW + 24), [(WINDOW, WINDOW)]);
        assert_eq!(
            touching(&map, WINDOW - 1, WINDOW + 1),
            [(0, WINDOW), (WINDOW, WINDOW)]
        );
        // Past a window's end: not that window.
        assert!(touching(&map, 2 * WINDOW + 0x1000, 2 * WINDOW + 0x2000).is_empty());
        assert_eq!(touching(&map, 0, 4 * WINDOW).len(), 4);
        // No bytes, or a range that runs backwards: nothing.
        assert!(touching(&map, WINDOW, WINDOW).is_empty());
        assert!(touching(&map, 2 * WINDOW, WINDOW).is_empty());
        // The top of the address space does not wrap.
        map.insert((u64::MAX - 0x1000, 0x1000), ());
        assert_eq!(
            touching(&map, u64::MAX - 0x800, u64::MAX),
            [(u64::MAX - 0x1000, 0x1000)]
        );
    }
}

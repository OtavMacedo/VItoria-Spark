use super::Spare;

/// Kept things come back by size, the last given first, and nothing is
/// kept past the limit.
#[test]
fn kept_things_come_back_by_size_within_the_limit() {
    let mut spare: Spare<u32> = Spare::new();
    assert_eq!(spare.take(4), None);
    assert_eq!(spare.give(1, 4, 10), None);
    assert_eq!(spare.give(2, 1, 10), None);
    assert_eq!(spare.give(3, 4, 10), None);
    assert_eq!(spare.bytes, 9);
    // Past the limit: handed back, nothing changes.
    assert_eq!(spare.give(4, 4, 10), Some(4));
    assert_eq!(spare.bytes, 9);
    assert_eq!(spare.take(2), None, "no thing of that size");
    assert_eq!(spare.take(4), Some(3));
    assert_eq!(spare.take(4), Some(1));
    assert_eq!(spare.take(4), None);
    assert_eq!(spare.take(1), Some(2));
    assert_eq!(spare.bytes, 0);
    assert!(spare.kept.is_empty());
}

/// A trim frees only what stayed kept through the whole window since the
/// last one, oldest first; the first trim only starts the window.
#[test]
fn a_trim_frees_only_what_nobody_asked_for_since_the_last() {
    let mut spare: Spare<u32> = Spare::new();
    for (thing, size) in [(1, 4), (2, 4), (3, 4)] {
        assert_eq!(spare.give(thing, size, 100), None);
    }
    assert!(spare.trim().is_empty());
    assert_eq!(spare.bytes, 12);
    // One taken and given back: 8 bytes stayed kept all the while.
    assert_eq!(spare.take(4), Some(3));
    assert_eq!(spare.give(3, 4, 100), None);
    let gone = spare.trim();
    assert_eq!(gone.iter().map(|g| g.1).sum::<u64>(), 8);
    assert_eq!(
        gone.iter().map(|g| g.0).collect::<Vec<_>>(),
        vec![1, 2],
        "the oldest go"
    );
    assert_eq!(spare.bytes, 4);
    // Nothing asked for in the next window: the rest goes too.
    assert_eq!(
        spare.trim().iter().map(|g| g.0).collect::<Vec<_>>(),
        vec![3]
    );
    assert_eq!(spare.bytes, 0);
    assert!(spare.trim().is_empty());
}

/// Sizes that do not add up to the low mark exactly: no more than it goes.
#[test]
fn a_trim_never_frees_past_the_low_mark() {
    let mut spare: Spare<u32> = Spare::new();
    assert_eq!(spare.give(1, 8, 100), None);
    assert_eq!(spare.give(2, 2, 100), None);
    assert_eq!(spare.give(3, 8, 100), None);
    assert!(spare.trim().is_empty());
    // Low mark 10 after one 8 was out for a while.
    assert_eq!(spare.take(8), Some(3));
    assert_eq!(spare.give(3, 8, 100), None);
    let gone: Vec<u32> = spare.trim().into_iter().map(|g| g.0).collect();
    assert_eq!(gone, vec![1, 2]);
    assert_eq!(spare.bytes, 8);
}

//! Blocks left open after a write fault ([`super::rest`]): reported
//! written at every asking while they rest, made read-only at the first
//! asking after, and no write of theirs lost.

use super::tests::{drop_views, two_views};
use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const SIZE: usize = 2 * BLOCK;

/// The switches for this thread, as given, until dropped.
struct Switches;

impl Switches {
    fn set(rest_us: u64, hot_frames: u64, frame: Option<u64>) -> Switches {
        rest::IN_TEST.set(Some(rest::Cfg {
            rest_us,
            hot_frames,
        }));
        rest::FRAME_IN_TEST.set(frame);
        Switches
    }
}

impl Drop for Switches {
    fn drop(&mut self) {
        rest::IN_TEST.set(None);
        rest::FRAME_IN_TEST.set(None);
    }
}

/// The protection of the page at `at` now.
fn protection(at: usize) -> u32 {
    use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    assert_ne!(
        unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) },
        0
    );
    info.Protect
}

fn store(at: usize, v: u64) {
    // SAFETY: a view of the test's own section; the handler opens a
    // guarded block.
    unsafe { (at as *mut u64).write_volatile(v) };
}

#[test]
fn a_block_a_fault_opened_rests_and_is_reported_at_every_asking() {
    use windows_sys::Win32::System::Memory::{PAGE_READONLY, PAGE_READWRITE};
    let (section, view, other) = two_views(SIZE);
    let block = |b: usize| Some(vec![view / PAGE + b]);
    let switches = Switches::set(200_000, 0, None);
    assert_eq!(
        guard(view, view + SIZE),
        Some(vec![view / PAGE, view / PAGE + 1])
    );
    assert_eq!(
        guard(view, view + SIZE),
        Some(Vec::new()),
        "nothing written yet"
    );
    // The fault opens the block, which rests.
    store(view + 8, 1);
    assert_eq!(guard(view, view + SIZE), block(0));
    assert_eq!(protection(view), PAGE_READWRITE, "left open while it rests");
    assert_eq!(
        protection(view + BLOCK),
        PAGE_READONLY,
        "the other block guarded"
    );
    // A write with no fault while it rests is reported all the same.
    store(view + 16, 2);
    assert_eq!(guard(view, view + SIZE), block(0));
    // Asked about at its first page alone too (a view known by its rest).
    assert_eq!(guard(view, view + BLOCK), block(0));
    // The rest over: reported once more and made read-only.
    std::thread::sleep(std::time::Duration::from_millis(250));
    assert_eq!(guard(view, view + SIZE), block(0));
    assert_eq!(protection(view), PAGE_READONLY);
    assert_eq!(guard(view, view + SIZE), Some(Vec::new()));
    // The switch goes off while a block rests: the next asking reports it
    // and makes it read-only, as without the switch.
    store(view + 8, 3);
    drop(switches);
    let _off = Switches::set(0, 0, None);
    assert_eq!(guard(view, view + SIZE), block(0));
    assert_eq!(protection(view), PAGE_READONLY);
    assert_eq!(guard(view, view + SIZE), Some(Vec::new()));
    // A resting block remapped is forgotten with its rest.
    let _on = Switches::set(200_000, 0, None);
    store(view + 8, 4);
    assert!(
        guards()
            .rests
            .rests(view / BLOCK, rest::now_us(), 0, rest::Cfg::now())
            .is_some()
    );
    title_memory_remapped(view, BLOCK);
    assert!(
        guards()
            .rests
            .rests(view / BLOCK, rest::now_us(), 0, rest::Cfg::now())
            .is_none()
    );
    drop_views(section, &[view, other], SIZE);
}

#[test]
fn a_block_faulting_four_times_in_a_frame_rests_for_its_frames() {
    use windows_sys::Win32::System::Memory::{PAGE_READONLY, PAGE_READWRITE};
    let (section, view, other) = two_views(SIZE);
    let block = |b: usize| Some(vec![view / PAGE + b]);
    let _hot = Switches::set(0, 3, Some(100));
    assert!(guard(view, view + SIZE).is_some());
    // Three faults in the frame: made read-only after each.
    for v in 0..3 {
        store(view + 8, v);
        assert_eq!(guard(view, view + SIZE), block(0));
        assert_eq!(protection(view), PAGE_READONLY);
    }
    // The fourth makes it hot: open until frame 103.
    store(view + 8, 3);
    for frame in 100..103 {
        rest::FRAME_IN_TEST.set(Some(frame));
        store(view + 8, frame);
        assert_eq!(guard(view, view + SIZE), block(0));
        assert_eq!(protection(view), PAGE_READWRITE);
    }
    rest::FRAME_IN_TEST.set(Some(103));
    assert_eq!(guard(view, view + SIZE), block(0));
    assert_eq!(protection(view), PAGE_READONLY);
    assert_eq!(guard(view, view + SIZE), Some(Vec::new()));
    drop_views(section, &[view, other], SIZE);
}

/// A thread writes a word over and over; another asks about the block and
/// takes the word whenever it is reported written. The word it holds is
/// never older than the last write done before its asking began.
fn no_write_is_lost(rest_us: u64, hot_frames: u64, through_other: bool) {
    const WRITES: u64 = 20_000;
    let (section, view, other) = two_views(SIZE);
    let written = if through_other { other } else { view };
    let last = AtomicU64::new(0);
    let done = AtomicBool::new(false);
    // Taken in, read-only.
    assert!(writes(view, view + SIZE, true).is_some());
    std::thread::scope(|s| {
        s.spawn(|| {
            let _switches = Switches::set(rest_us, hot_frames, Some(0));
            for v in 1..=WRITES {
                rest::FRAME_IN_TEST.set(Some(v / 500));
                store(written + BLOCK + 64, v);
                last.store(v, Ordering::Release);
                if v % 64 == 0 {
                    std::thread::yield_now();
                }
            }
            done.store(true, Ordering::Release);
        });
        let _switches = Switches::set(rest_us, hot_frames, Some(0));
        let page = view / PAGE + 1;
        let (mut seen, mut held) = (0u64, 0u64);
        let mut round = 0u64;
        loop {
            let finished = done.load(Ordering::Acquire);
            let before = last.load(Ordering::Acquire);
            rest::FRAME_IN_TEST.set(Some(round / 50));
            round += 1;
            let (now, pages) = writes(view, view + SIZE, true).expect("a view");
            if pages.iter().any(|&(p, at)| p == page && at > seen) {
                // SAFETY: the word, read after the asking.
                held = unsafe { ((view + BLOCK + 64) as *const u64).read_volatile() };
                seen = now;
            }
            assert!(
                held >= before,
                "the copy holds {held}, written before its asking: {before} (rest {rest_us} us, hot {hot_frames}, round {round})"
            );
            if finished {
                assert_eq!(held, WRITES);
                break;
            }
        }
    });
    drop_views(section, &[view, other], SIZE);
}

#[test]
fn no_write_to_a_resting_block_is_lost() {
    no_write_is_lost(300, 0, false);
    no_write_is_lost(300, 2, false);
    no_write_is_lost(0, 2, false);
    no_write_is_lost(0, 0, false);
}

#[test]
fn no_write_through_another_view_of_a_resting_block_is_lost() {
    no_write_is_lost(300, 0, true);
    no_write_is_lost(300, 2, true);
}

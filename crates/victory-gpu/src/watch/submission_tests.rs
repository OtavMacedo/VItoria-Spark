use super::*;
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
};

struct Reservation(usize);

impl Reservation {
    fn new(bytes: usize) -> Self {
        const MEM_WRITE_WATCH: u32 = 0x20_0000;
        // SAFETY: reserve fresh address space. Each test commits only the pages it needs.
        let at = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                bytes,
                MEM_RESERVE | MEM_WRITE_WATCH,
                PAGE_READWRITE,
            )
        };
        assert!(!at.is_null());
        Self(at as usize)
    }

    fn commit(&self, offset: usize) -> usize {
        let at = self.0 + offset;
        // SAFETY: the offset is a kernel page inside the live reservation.
        assert_eq!(
            unsafe { VirtualAlloc(at as *const _, KERNEL_PAGE, MEM_COMMIT, PAGE_READWRITE) }
                as usize,
            at
        );
        at
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        // SAFETY: this test owns the entire reservation.
        unsafe { VirtualFree(self.0 as *mut _, 0, MEM_RELEASE) };
    }
}

#[test]
fn calls_and_coverage_for_two_runs_in_one_reservation() {
    let width = (AHEAD_GAP + 1) * PAGE;
    let memory = Reservation::new(3 * width);
    let left = memory.commit(0);
    let right = memory.commit(2 * width);
    let ranges = [
        (right, right + width),
        (left, left + width),
        (left, left + width),
    ];
    let pieces = sorted_pages(&ranges);
    let mut old = Journal::default();
    ask_nearby(&mut old, &pieces);
    ask_remaining(&mut old, &ranges);
    if count_asks() {
        assert_eq!(old.stats.calls, [1, 1, 0, 0]);
        assert_eq!(
            old.stats.pages.iter().sum::<u64>(),
            (2 * width / KERNEL_PAGE) as u64
        );
        eprintln!(
            "old calls {:?}, 4 KiB pages {:?}",
            old.stats.calls, old.stats.pages
        );
    }
    let mut new = Journal::default();
    ask_reservations(&mut new, &pieces);
    assert!(new.kept);
    assert_eq!(new.ahead.len(), 2);
    if count_asks() {
        assert_eq!(new.stats.calls, [1, 0, 0, 0]);
        assert_eq!(new.stats.pages[0], (3 * width / KERNEL_PAGE) as u64);
        eprintln!(
            "new calls {:?}, 4 KiB pages {:?}",
            new.stats.calls, new.stats.pages
        );
    }
}

#[test]
fn a_submission_journals_writes_in_holes_and_only_caches_requested_pieces() {
    let memory = Reservation::new(5 * PAGE);
    let left = memory.commit(0);
    let right = memory.commit(4 * PAGE);
    let gap = memory.commit(2 * PAGE);
    let first = left / PAGE;
    let pieces = [(first, first + 1), (first + 4, first + 5)];
    let mut j = Journal::default();
    // SAFETY: ordinary title stores to the committed islands.
    unsafe {
        (left as *mut u8).write_volatile(1);
        (gap as *mut u8).write_volatile(1)
    };
    ask_reservations(&mut j, &pieces);
    let before = j.asked;
    for _ in 0..2 {
        assert_eq!(
            cached(&j, first, first + 1),
            Some((before, vec![(first, before)]))
        );
    }
    assert!(cached(&j, first + 2, first + 3).is_none());
    assert_eq!(
        j.written[&(gap / PAGE)],
        before,
        "the reset in the gap was journalled"
    );
    assert_eq!(
        kernel_pages(&mut j, first, first + 5, Caller::Other),
        Some(0)
    );

    // A store between submissions needs no mapping notification. Commit a
    // previously reserved kernel page inside the first piece as well.
    let late = memory.commit(KERNEL_PAGE);
    // SAFETY: both addresses are now committed and writable.
    unsafe {
        (late as *mut u8).write_volatile(2);
        (right as *mut u8).write_volatile(2)
    };
    ask_reservations(&mut j, &pieces);
    assert!(j.asked > before);
    for p in [first, right / PAGE] {
        assert_eq!(cached(&j, p, p + 1), Some((j.asked, vec![(p, j.asked)])));
    }
    assert_eq!(
        j.written[&(gap / PAGE)],
        before,
        "no repeated report after the reset"
    );
    assert_eq!(
        kernel_pages(&mut j, first, first + 5, Caller::Other),
        Some(0)
    );
}

#[test]
fn writes_racing_the_reset_are_in_this_submission_or_the_next() {
    let memory = Reservation::new(3 * PAGE);
    let addresses = [memory.commit(0), memory.commit(2 * PAGE)];
    let pieces = addresses.map(|at| (at / PAGE, at / PAGE + 1));
    let mut j = Journal::default();
    for byte in 1..=16 {
        let start = std::sync::Barrier::new(3);
        std::thread::scope(|threads| {
            for at in addresses {
                let start = &start;
                threads.spawn(move || {
                    start.wait();
                    // SAFETY: each writer has a distinct committed page.
                    unsafe { (at as *mut u8).write_volatile(byte) };
                });
            }
            start.wait();
            ask_reservations(&mut j, &pieces);
        });
        let racing = j.asked;
        ask_reservations(&mut j, &pieces);
        for (page, end) in pieces {
            let (_, seen) = cached(&j, page, end).unwrap();
            assert_eq!(seen.len(), 1);
            assert!(seen[0].1 >= racing, "the store escaped both resets");
        }
        assert_eq!(
            kernel_pages(&mut j, pieces[0].0, pieces[1].1, Caller::Other),
            Some(0)
        );
    }
}

#[test]
fn overlapping_copies_of_a_mapped_view_still_observe_alias_writes() {
    let (section, view, alias) = tests::two_views(PAGE);
    let ranges: Vec<_> = (0..32)
        .map(|i| (view + i * 64, view + i * 64 + 64))
        .collect();
    let pieces = sorted_pages(&ranges);
    let mut old = Journal::default();
    ask_nearby(&mut old, &pieces);
    ask_remaining(&mut old, &ranges);
    let mut j = Journal::default();
    assert!(guard(view, view + PAGE).is_some());
    ask_reservations(&mut j, &pieces);
    assert!(j.kept && j.ahead.is_empty());
    if count_asks() {
        assert_eq!(old.stats.calls, [32, 1, 30, 0]);
        assert_eq!(j.stats.calls, [u64::from(force_view_ask()), 0, 0, 0]);
    }
    // SAFETY: the other writable view faults into the existing write guard.
    unsafe { (alias as *mut u8).write_volatile(1) };
    assert_eq!(guard(view, view + PAGE), Some(vec![view / PAGE]));
    tests::drop_views(section, &[view, alias], PAGE);
}

#[test]
fn a_piece_crossing_the_allocation_cannot_hide_a_watched_piece_inside_it() {
    let memory = Reservation::new(2 * PAGE);
    let at = memory.commit(PAGE);
    let first = memory.0 / PAGE;
    let mut j = Journal::default();
    // SAFETY: the write is in the second committed island.
    unsafe { (at as *mut u8).write_volatile(1) };
    ask_reservations(&mut j, &[(first, first + 3), (first + 1, first + 2)]);
    assert!(cached(&j, first, first + 3).is_none());
    assert_eq!(
        cached(&j, first + 1, first + 2),
        Some((j.asked, vec![(first + 1, j.asked)]))
    );
}

#[test]
fn even_a_completely_dirty_answer_fits_in_one_call() {
    let memory = Reservation::new(PAGE);
    let first = memory.0 / PAGE;
    for offset in (0..PAGE).step_by(KERNEL_PAGE) {
        let at = memory.commit(offset);
        // SAFETY: one ordinary store in each committed kernel page.
        unsafe { (at as *mut u8).write_volatile(1) };
    }
    let mut j = Journal::default();
    assert_eq!(
        kernel_pages(&mut j, first, first + 1, Caller::Other),
        Some(PAGE / KERNEL_PAGE)
    );
    assert_eq!(
        kernel_pages(&mut j, first, first + 1, Caller::Other),
        Some(0)
    );
    if count_asks() {
        assert_eq!(j.stats.calls, [0, 0, 0, 2]);
        assert_eq!(j.stats.pages, [0, 0, 0, (2 * PAGE / KERNEL_PAGE) as u64]);
    }
}

#[test]
fn cached_answers_keep_holes_and_reservation_boundaries() {
    let mut j = Journal {
        ordered: true,
        ..Default::default()
    };
    keep_requested(&mut j.ahead, &[(1, 3), (3, 6), (7, 10)]);
    keep_requested(&mut j.ahead, &[(8, 9), (9, 11)]);
    keep_requested(&mut j.ahead, &[(11, 12)]);
    for (first, last) in [(1, 6), (7, 10), (9, 11), (11, 12)] {
        assert!(cached(&j, first, last).is_some());
    }
    for (first, last) in [(5, 8), (6, 7), (10, 12)] {
        assert!(cached(&j, first, last).is_none());
    }
}

#[test]
fn two_tiny_reads_do_not_walk_a_large_unrequested_gap() {
    let memory = Reservation::new(32 << 20);
    let pages = [memory.commit(0) / PAGE, memory.commit(31 << 20) / PAGE];
    let mut j = Journal::default();
    ask_reservations(&mut j, &pages.map(|page| (page, page + 1)));
    assert_eq!(j.ahead.len(), 2);
    if count_asks() {
        assert_eq!(j.stats.calls, [2, 0, 0, 0]);
        assert_eq!(j.stats.pages[0], (2 * PAGE / KERNEL_PAGE) as u64);
    }
}

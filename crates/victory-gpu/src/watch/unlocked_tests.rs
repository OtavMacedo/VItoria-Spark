//! Write faults that take no lock of the guards ([`super`]): what each
//! step leaves the holders of the lock to see, views guarded together, and
//! writers, refreshes and remappings all at once.

use super::super::tests::drop_views;
use super::super::*;
use super::{IN_TEST, OPENING, PROTECTED, opening};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// This thread's faults take no lock (or do), until dropped.
struct Unlocked;

impl Unlocked {
    fn set(on: bool) -> Unlocked {
        IN_TEST.set(Some(on));
        Unlocked
    }
}

impl Drop for Unlocked {
    fn drop(&mut self) {
        IN_TEST.set(None);
    }
}

/// A pagefile section mapped once, not told to the runtime's views: no
/// other view of its pages, so its faults can take no lock.
fn one_view(bytes: usize) -> (usize, usize) {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Memory::{
        CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile, PAGE_READWRITE,
    };
    // SAFETY: a fresh pagefile section, mapped once; [`drop_views`]
    // unmaps it and closes the handle.
    unsafe {
        let section = CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            std::ptr::null(),
            PAGE_READWRITE,
            0,
            bytes as u32,
            std::ptr::null(),
        );
        assert!(!section.is_null());
        let view = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, bytes).Value as usize;
        assert_ne!(view, 0);
        (section as usize, view)
    }
}

fn store(at: usize, v: u64) {
    // SAFETY: a word of a test's own view; the handler opens a guarded
    // block.
    unsafe { (at as *mut u64).write_volatile(v) };
}

fn load(at: usize) -> u64 {
    // SAFETY: as above, read.
    unsafe { (at as *const u64).read_volatile() }
}

/// Step 4 of a fault, by hand.
fn make_writable(block: usize) {
    use windows_sys::Win32::System::Memory::{PAGE_READWRITE, VirtualProtect};
    let mut old = 0;
    // SAFETY: a block of a test's own view.
    assert_ne!(
        unsafe { VirtualProtect((block * BLOCK) as *const _, BLOCK, PAGE_READWRITE, &mut old) },
        0
    );
}

/// Step 5 of a fault, by hand: queued for the next holder of the lock.
fn queue(block: usize) {
    let mut queue = super::OPENED.lock().unwrap_or_else(|e| e.into_inner());
    queue.push(super::Opened {
        block,
        at_us: rest::now_us(),
        frame: rest::frame(),
    });
    super::TO_SETTLE.store(queue.len(), Ordering::SeqCst);
}

/// A fault that takes no lock opens its block itself; the next holder of
/// the lock settles it, and the asking after the write reports the block
/// and guards it again.
#[test]
fn a_fault_that_takes_no_lock_opens_its_block_and_is_settled() {
    const SIZE: usize = 2 * BLOCK;
    let (section, view) = one_view(SIZE);
    let page = |b: usize| view / PAGE + b;
    assert_eq!(guard(view, view + SIZE), Some(vec![page(0), page(1)]));
    assert!(PROTECTED.get(view / BLOCK) && PROTECTED.get(view / BLOCK + 1));
    let before = super::COUNTS[super::OPENED_N].load(Ordering::Relaxed);
    {
        let _on = Unlocked::set(true);
        store(view + BLOCK + 8, 1);
    }
    assert!(super::COUNTS[super::OPENED_N].load(Ordering::Relaxed) > before);
    assert!(writable(view + BLOCK) && !writable(view));
    assert_eq!(guard(view, view + SIZE), Some(vec![page(1)]));
    assert!(
        !opening(view / BLOCK + 1),
        "settled by a holder of the lock"
    );
    assert!(!writable(view + BLOCK), "guarded again");
    assert_eq!(guard(view, view + SIZE), Some(Vec::new()));
    // The write lands, and a second one faults again.
    assert_eq!(load(view + BLOCK + 8), 1);
    {
        let _on = Unlocked::set(true);
        store(view + BLOCK + 8, 2);
    }
    assert_eq!(guard(view, view + SIZE), Some(vec![page(1)]));
    assert_eq!(load(view + BLOCK + 8), 2);
    // (Looked at while the view is still ours: another test may map its
    // own at the address once it is let go.)
    title_memory_remapped(view, SIZE);
    assert!(!PROTECTED.get(view / BLOCK), "forgotten with the mapping");
    drop_views(section, &[view], SIZE);
}

/// What the holders of the lock do with a block a fault is opening
/// without it, before that is settled: it is reported written at every
/// asking and left as it is, not counted clean nor given a bit of the
/// light refresh. Settled, all is as after any fault.
#[test]
fn a_block_being_opened_is_reported_and_left_alone() {
    const SIZE: usize = 2 * BLOCK;
    let (section, view) = one_view(SIZE);
    let b = view / BLOCK + 1;
    let page = view / PAGE + 1;
    assert!(guard(view, view + SIZE).is_some());
    assert_eq!(guard(view, view + SIZE), Some(Vec::new()));
    assert!(clean(b));
    // Steps 1 and 3, as the fault does them.
    assert!(!super::OPENING.swap_on(b));
    PUBLISHED.clear(b);
    for _ in 0..2 {
        assert_eq!(guard(view, view + SIZE), Some(vec![page]));
        assert!(!writable(view + BLOCK), "its protection is the fault's");
        assert!(!clean(b));
    }
    // An asking that had the guards make it clean gives it no bit.
    publish(
        &JOURNAL.lock().unwrap_or_else(|e| e.into_inner()),
        &[page],
        view,
        view + SIZE,
        false,
    );
    assert!(!PUBLISHED.get(b));
    // Steps 4 and 5: writable now, and settled by the next holder.
    make_writable(b);
    store(view + BLOCK + 8, 7);
    queue(b);
    assert_eq!(guard(view, view + SIZE), Some(vec![page]));
    assert!(!opening(b) && !writable(view + BLOCK));
    assert_eq!(load(view + BLOCK + 8), 7);
    assert_eq!(guard(view, view + SIZE), Some(Vec::new()));
    drop_views(section, &[view], SIZE);
}

/// A fault opening one view of a page had not seen the views noted
/// together when the guards noted them: they see its mark, and open every
/// view of the page as that fault would have under the lock, until it is
/// settled. No view is left read-only and clean while another is
/// writable.
#[test]
fn views_noted_together_while_one_is_being_opened_are_all_opened() {
    let (section, one, two) = super::super::tests::two_views(BLOCK);
    let block = |view: usize| view / BLOCK;
    // The fault at the second view, past step 2: it found no views noted.
    assert!(!opening(block(two)));
    assert!(!OPENING.swap_on(block(two)));
    // The first asking of the first view guards it and notes both views.
    assert_eq!(guard(one, one + BLOCK), Some(vec![block(one)]));
    assert!(writable(one), "opened with the other view");
    assert!(writable(two), "left to its fault");
    assert!(!clean(block(one)));
    assert_eq!(guard(one, one + BLOCK), Some(vec![block(one)]));
    assert!(writable(one));
    // The fault ends and is settled: guarded together from then on.
    queue(block(two));
    assert_eq!(guard(one, one + BLOCK), Some(vec![block(one)]));
    assert!(!opening(block(two)));
    assert!(!writable(one) && !writable(two));
    assert!(super::ALIASED.get(block(one)) && super::ALIASED.get(block(two)));
    assert_eq!(guard(one, one + BLOCK), Some(Vec::new()));
    // A write through the other view now goes the locked way, which opens
    // both.
    let before = super::COUNTS[super::BY_VIEWS].load(Ordering::Relaxed);
    {
        let _on = Unlocked::set(true);
        store(two + 8, 1);
    }
    assert!(super::COUNTS[super::BY_VIEWS].load(Ordering::Relaxed) > before);
    assert!(!opening(block(two)), "its mark taken back");
    assert!(writable(one) && writable(two));
    assert_eq!(guard(one, one + BLOCK), Some(vec![block(one)]));
    drop_views(section, &[one, two], BLOCK);
}

/// The bits of ranges: any, swap, and pieces made without a bit set.
#[test]
fn bits_any_and_swap() {
    static BITS: Bits = Bits::new();
    assert!(!BITS.any(0, 1 << 40));
    BITS.make(1 << 30);
    BITS.make(1 << 20);
    assert!(!BITS.any(0, 1 << 40), "a piece made, no bit set");
    assert!(!BITS.swap_on(70) && BITS.swap_on(70) && BITS.get(70));
    assert!(BITS.any(70, 71) && BITS.any(0, 1000) && !BITS.any(0, 70) && !BITS.any(71, 1 << 40));
    BITS.set((1 << 28) + 5);
    assert!(
        BITS.any(71, 1 << 40) && !BITS.any(71, 1 << 28) && BITS.any((1 << 28) + 5, (1 << 28) + 6)
    );
    assert!(BITS.swap_on(1 << 60), "past the address space: no bit");
    assert!(!BITS.any(1 << 59, usize::MAX));
}

/// Placeholder memory of two blocks whose first block a view is mapped
/// into and unmapped from, as the runtime does.
struct Streamed {
    at: usize,
    section: usize,
}

impl Streamed {
    fn new() -> Streamed {
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, MEM_PRESERVE_PLACEHOLDER, MEM_RELEASE, MEM_RESERVE,
            MEM_RESERVE_PLACEHOLDER, PAGE_NOACCESS, PAGE_READWRITE, VirtualAlloc2, VirtualFree,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: a fresh placeholder split at its first block, and a
        // pagefile section of one block.
        unsafe {
            let at = VirtualAlloc2(
                GetCurrentProcess(),
                std::ptr::null(),
                2 * BLOCK,
                MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                PAGE_NOACCESS,
                std::ptr::null_mut(),
                0,
            ) as usize;
            assert_ne!(at, 0);
            assert_ne!(
                VirtualFree(at as *mut _, BLOCK, MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER),
                0
            );
            let section = CreateFileMappingW(
                -1isize as _,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                BLOCK as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            Streamed {
                at,
                section: section as usize,
            }
        }
    }

    /// Maps the view, told as the runtime tells it.
    fn map(&self) {
        use windows_sys::Win32::System::Memory::{
            MEM_REPLACE_PLACEHOLDER, MapViewOfFile3, PAGE_READWRITE,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        {
            let _changing = title_memory_remapping();
            // SAFETY: the placeholder's first block, a placeholder now.
            let view = unsafe {
                MapViewOfFile3(
                    self.section as *mut _,
                    GetCurrentProcess(),
                    self.at as *const _,
                    0,
                    BLOCK,
                    MEM_REPLACE_PLACEHOLDER,
                    PAGE_READWRITE,
                    std::ptr::null_mut(),
                    0,
                )
            };
            assert_eq!(view.Value as usize, self.at);
            title_memory_remapped(self.at, BLOCK);
        }
    }

    /// Unmaps it, leaving the placeholder.
    fn unmap(&self) {
        use windows_sys::Win32::System::Memory::{
            MEM_PRESERVE_PLACEHOLDER, MEMORY_MAPPED_VIEW_ADDRESS, UnmapViewOfFile2,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let _changing = title_memory_remapping();
        title_memory_remapped(self.at, BLOCK);
        // SAFETY: the view `map` put there.
        unsafe {
            UnmapViewOfFile2(
                GetCurrentProcess(),
                MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: self.at as *mut _,
                },
                MEM_PRESERVE_PLACEHOLDER,
            )
        };
    }
}

impl Drop for Streamed {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Memory::{MEM_RELEASE, VirtualFree};
        title_memory_remapped(self.at, 2 * BLOCK);
        // SAFETY: the two placeholders and the section, ours.
        unsafe {
            VirtualFree(self.at as *mut _, 0, MEM_RELEASE);
            VirtualFree((self.at + BLOCK) as *mut _, 0, MEM_RELEASE);
            windows_sys::Win32::Foundation::CloseHandle(self.section as *mut _);
        }
    }
}

/// Writers store words over and over, two to a block, most with faults
/// that take no lock; askers ask about the blocks and take each word
/// whenever its block is reported written; meanwhile another thread maps
/// and unmaps a view beside them, asks about it and writes it, and tells
/// of writes of its own into the writers' blocks (`writing`). The word an
/// asker holds is never older than the last store done before its asking
/// began, and every asker ends with the last one.
fn no_write_is_lost(rest_us: u64) {
    const WRITERS: usize = 4;
    const BLOCKS: usize = 2;
    const SIZE: usize = BLOCKS * BLOCK;
    /// How long the writers write.
    const FOR: std::time::Duration = std::time::Duration::from_millis(400);
    let (section, view) = one_view(SIZE);
    let word = |t: usize| view + (t % BLOCKS) * BLOCK + 64 * (t + 1);
    let last: [AtomicU64; WRITERS] = [const { AtomicU64::new(0) }; WRITERS];
    let done = AtomicBool::new(false);
    let streamed = Streamed::new();
    assert!(writes(view, view + SIZE, true).is_some());
    let count = |i: usize| super::COUNTS[i].load(Ordering::Relaxed);
    let opened = count(super::OPENED_N);
    std::thread::scope(|s| {
        let writers: Vec<_> = (0..WRITERS)
            .map(|t| {
                let last = &last;
                s.spawn(move || {
                    // One of them goes the locked way.
                    let _on = Unlocked::set(t != WRITERS - 1);
                    let began = std::time::Instant::now();
                    let mut v = 0;
                    while began.elapsed() < FOR {
                        v += 1;
                        store(word(t), v);
                        last[t].store(v, Ordering::Release);
                        if v % 16 == 0 {
                            std::thread::yield_now();
                        }
                    }
                    v
                })
            })
            .collect();
        for _ in 0..2 {
            let (last, done) = (&last, &done);
            s.spawn(move || {
                let _rest = rest_cfg(rest_us);
                let mut seen = [0u64; WRITERS];
                let mut held = [0u64; WRITERS];
                let mut round = 0u64;
                loop {
                    let finished = done.load(Ordering::Acquire);
                    let before: [u64; WRITERS] =
                        std::array::from_fn(|t| last[t].load(Ordering::Acquire));
                    round += 1;
                    let (now, pages) = writes(view, view + SIZE, true).expect("a view");
                    for t in 0..WRITERS {
                        let page = word(t) / PAGE;
                        if pages.iter().any(|&(p, at)| p == page && at > seen[t]) {
                            held[t] = load(word(t));
                            seen[t] = now;
                        }
                        assert!(
                            held[t] >= before[t],
                            "writer {t}: the copy holds {}, written before its asking: {} (rest {rest_us} us, round {round})",
                            held[t],
                            before[t]
                        );
                    }
                    if finished {
                        assert_eq!(held, before, "the last stores");
                        break;
                    }
                }
            });
        }
        let (done, streamed) = (&done, &streamed);
        s.spawn(move || {
            let _on = Unlocked::set(true);
            let _rest = rest_cfg(rest_us);
            let at = streamed.at;
            let mut n = 0u64;
            while !done.load(Ordering::Acquire) {
                n += 1;
                streamed.map();
                assert!(writes(at, at + 2 * BLOCK, true).is_some());
                let (asked, _) = writes(at, at + 2 * BLOCK, true).expect("a view");
                store(at + 8, n);
                let (_, pages) = writes(at, at + 2 * BLOCK, true).expect("a view");
                assert!(
                    pages.iter().any(|&(p, w)| p == at / PAGE && w > asked),
                    "the streamed view's write"
                );
                streamed.unmap();
                writing(view + (n as usize % BLOCKS) * BLOCK + 8, 8);
            }
        });
        let stores: Vec<u64> = writers.into_iter().map(|w| w.join().unwrap()).collect();
        done.store(true, Ordering::Release);
        eprintln!(
            "rest {rest_us} us: stores {stores:?}, faults opened without the lock {}",
            count(super::OPENED_N) - opened
        );
    });
    // Enough faults took no lock to have raced the askers.
    assert!(count(super::OPENED_N) - opened > 100);
    drop(streamed);
    drop_views(section, &[view], SIZE);
}

/// The rests for this thread, until dropped.
struct RestCfg;

fn rest_cfg(rest_us: u64) -> RestCfg {
    rest::IN_TEST.set(Some(rest::Cfg {
        rest_us,
        hot_frames: 0,
    }));
    RestCfg
}

impl Drop for RestCfg {
    fn drop(&mut self) {
        rest::IN_TEST.set(None);
    }
}

#[test]
fn no_write_is_lost_when_faults_take_no_lock() {
    no_write_is_lost(0);
}

#[test]
fn no_write_to_resting_blocks_is_lost_when_faults_take_no_lock() {
    no_write_is_lost(300);
}

/// With the light refresh, a fault that takes no lock takes its block's
/// bit before the block can be written: the asking after the write goes
/// to the guards and reports the block, though nobody took the guards'
/// lock between.
#[test]
fn a_fault_that_takes_no_lock_takes_the_blocks_bit() {
    // (The light refresh's tests take turns: they count its askings.)
    let _turn = super::super::tests::light_turn();
    const SIZE: usize = 2 * BLOCK;
    let (section, view) = one_view(SIZE);
    let b = view / BLOCK + 1;
    let (first, _) = writes_as(view, view + SIZE, true, 2).expect("a view");
    assert!(PUBLISHED.get(b), "made clean and noted");
    {
        let _on = Unlocked::set(true);
        store(view + BLOCK + 8, 1);
    }
    assert!(!PUBLISHED.get(b));
    let (_, written) = writes_as(view, view + SIZE, true, 2).expect("a view");
    assert!(
        written
            .iter()
            .any(|&(p, at)| p == view / PAGE + 1 && at > first),
        "the written block is reported"
    );
    drop_views(section, &[view], SIZE);
}

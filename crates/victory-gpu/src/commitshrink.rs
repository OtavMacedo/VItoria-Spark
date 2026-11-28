//! Keeping the host's caches of the PC's memory short while Windows'
//! commit is near its limit (`VICTORY_COMMIT_SHRINK_PCT=<percent>`, 85 is
//! the value to try; off by default).
//!
//! The caches of the PC's memory the host keeps for later (idle host lists
//! with their allocators, spare scratch chunks, idle copies of title memory
//! and staging buffers) are all commit. Their idle times went by free
//! physical memory alone (`crate::host::ram_idle_ms`), and crashes with the
//! commit at its limit came while physical memory was still free. With the
//! switch, once a second (the host's sweep):
//!
//! - the system's commit (`total - available` of the paging file's line of
//!   `GlobalMemoryStatusEx`) is compared with the percent: past it the
//!   host is *tight*, and it stays so until the commit falls five points
//!   below it ([`next_tight`]), so that it does not flip every second;
//! - while tight, the idle host lists past [`IDLE_LISTS_TIGHT`] are
//!   destroyed and no more are kept (`crate::list`), the spare scratch
//!   chunks no list took since the last sweep are freed
//!   (`crate::host::trim_spare_scratch`), and the idle times of the PC's
//!   memory caches are the shortest ones, as with little physical memory
//!   free (`crate::host::ram_idle_ms`).
//!
//! It may alternate inside a run (`SPARK_AB=VICTORY_COMMIT_SHRINK_PCT=0/85`,
//! `crate::switches`): [`tight`] is false whenever the switch is off, and
//! what the tight mode let go is made again when it is needed.
//!
//! After ZigZagBoi1/ScarlettPort pull request #370 (commit 745d6f36f),
//! items 4 and 5.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

/// The most host lists kept idle while tight: each keeps its allocators and
/// buffers (some 0.7 MB of commit a list in ScarlettPort's measure; up to
/// 350 MB for the 512 kept otherwise).
pub(crate) const IDLE_LISTS_TIGHT: usize = 32;

/// Points under the percent the commit must fall to before the host is no
/// longer tight.
const BAND: u64 = 5;

/// Whether the last sweep found the commit past the percent.
static TIGHT: AtomicBool = AtomicBool::new(false);

/// Since the start: sweeps that found the host tight, times it became
/// tight, idle host lists destroyed, bytes of spare scratch freed; and the
/// commit seen last, in percent of its limit.
static COUNTS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

/// The percent of the commit limit past which the host is tight; 0: off.
fn percent() -> u64 {
    crate::switches::COMMIT_SHRINK_PCT.get()
}

/// Whether the host keeps its caches of the PC's memory short now: the
/// switch is on and the last sweep found the commit past its percent.
pub(crate) fn tight() -> bool {
    TIGHT.load(Relaxed) && percent() != 0
}

/// Whether a commit of `total - available` bytes against a limit of
/// `total` makes the host tight at `percent`, having been so (`was`) or not:
/// on past `percent`, off below `percent - BAND`.
fn next_tight(was: bool, total: u64, available: u64, percent: u64) -> bool {
    if total == 0 || percent == 0 {
        return false;
    }
    let used = u128::from(total.saturating_sub(available)) * 100;
    let total = u128::from(total);
    if was {
        used >= total * u128::from(percent.saturating_sub(BAND))
    } else {
        used > total * u128::from(percent)
    }
}

/// The system's commit limit and what of it is still available, in bytes.
fn commit() -> Option<(u64, u64)> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: plain data the call fills in.
    let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    m.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: a sized structure.
    let ok = unsafe { GlobalMemoryStatusEx(&mut m) } != 0;
    ok.then_some((m.ullTotalPageFile, m.ullAvailPageFile))
}

/// From the host's once-a-second sweep: is the host tight, and if so the
/// caches above are cut.
pub(crate) fn sweep() {
    let percent = percent();
    if percent == 0 {
        TIGHT.store(false, Relaxed);
        return;
    }
    let Some((total, available)) = commit() else {
        return;
    };
    if total != 0 {
        let used = u128::from(total.saturating_sub(available)) * 100 / u128::from(total);
        COUNTS[4].store(used as u64, Relaxed);
    }
    let was = TIGHT.load(Relaxed);
    let now = next_tight(was, total, available, percent);
    if now != was {
        TIGHT.store(now, Relaxed);
        if now {
            COUNTS[1].fetch_add(1, Relaxed);
        }
        tracing::info!(
            commit_used_mib = total.saturating_sub(available) >> 20,
            commit_limit_mib = total >> 20,
            "system commit {} (VICTORY_COMMIT_SHRINK_PCT={percent}): the host's caches of the PC's memory {}",
            if now {
                "past the percent"
            } else {
                "back under the percent"
            },
            if now {
                "are kept short"
            } else {
                "are kept as before"
            }
        );
    }
    if !now {
        return;
    }
    COUNTS[0].fetch_add(1, Relaxed);
    COUNTS[2].fetch_add(crate::list::trim_idle_to(IDLE_LISTS_TIGHT) as u64, Relaxed);
    COUNTS[3].fetch_add(crate::host::trim_spare_scratch(), Relaxed);
}

/// For the memory line: [`COUNTS`], while the switch is on.
#[cfg(feature = "diagnostics")]
pub(crate) fn counts() -> Option<[u64; 5]> {
    (percent() != 0).then(|| std::array::from_fn(|i| COUNTS[i].load(Relaxed)))
}

#[cfg(test)]
mod tests {
    use super::next_tight;

    const GIB: u64 = 1 << 30;

    /// On past the percent, off again five points under it; nothing with
    /// no limit or with the switch off.
    #[test]
    fn tight_turns_on_past_the_percent_and_off_five_points_under() {
        assert!(!next_tight(false, 0, 0, 85));
        assert!(!next_tight(false, 100 * GIB, 0, 0));
        assert!(!next_tight(false, 100 * GIB, 15 * GIB, 85));
        assert!(next_tight(false, 100 * GIB, 14 * GIB, 85));
        assert!(next_tight(true, 100 * GIB, 20 * GIB, 85));
        assert!(!next_tight(true, 100 * GIB, 21 * GIB, 85));
        // A 16 GiB PC with a 32 GiB limit and 28 GiB committed.
        assert!(next_tight(false, 32 * GIB, 4 * GIB, 85));
        // More available than the limit (a sample taken while it grew):
        // nothing committed.
        assert!(!next_tight(true, 32 * GIB, 40 * GIB, 85));
    }
}

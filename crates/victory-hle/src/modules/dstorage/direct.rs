//! How the workers read files (`VICTORY_DSTORAGE_IO`).
//!
//! Unset (or anything but `direct`): as before, every worker reads through
//! the one handle the title opened, and Windows keeps what was read in its
//! file cache. `direct`: each worker reads through a handle of its own
//! opened with `FILE_FLAG_NO_BUFFERING`, past the file cache, so that the
//! game's files do not hold gigabytes of RAM in the cache's standby pages
//! on a PC that has none to spare (16 GB). The cost: a part of a file read
//! twice comes from the disk twice.
//!
//! Without the cache, Windows reads whole sectors only: offsets, sizes and
//! buffer addresses must be multiples of the sector size. A request that is
//! not is read whole sectors around it into an aligned buffer of the
//! worker's (1 MiB at a time) and copied out ([`read_sectors`]).
//!
//! Read once and kept for the run, and not one of the `SPARK_AB` switches:
//! what an arm read through the cache stays in the cache under the other,
//! so a run that alternated it would not measure either.
//!
//! The `direct` way of TaynanGT/Vice-Tay fbc1830 (which also has `handles`
//! and `overlapped`, not brought here).

use std::os::windows::fs::FileExt;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// How files are read (`VICTORY_DSTORAGE_IO`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Io {
    /// Unset: all workers through the one handle, through the file cache.
    Cached,
    /// `direct`: a handle for each worker, past the file cache.
    Direct,
}

impl Io {
    /// The way asked for, read once.
    pub(super) fn chosen() -> Io {
        static CHOSEN: OnceLock<Io> = OnceLock::new();
        *CHOSEN.get_or_init(|| {
            let spec = std::env::var("VICTORY_DSTORAGE_IO").ok();
            let io = Io::parse(spec.as_deref());
            match (io, spec.as_deref().map(str::trim)) {
                (Io::Direct, _) => tracing::info!(
                    "dstorage: files read past the file cache, a handle per worker (VICTORY_DSTORAGE_IO=direct)"
                ),
                (_, Some(other)) if !other.is_empty() => tracing::warn!(
                    value = other,
                    "dstorage: VICTORY_DSTORAGE_IO takes `direct`; files read as before"
                ),
                _ => {}
            }
            io
        })
    }

    /// [`chosen`](Io::chosen) for a setting.
    fn parse(spec: Option<&str>) -> Io {
        match spec.map(str::trim) {
            Some("direct") => Io::Direct,
            _ => Io::Cached,
        }
    }
}

/// A file as the workers read it: the handle the title opened, and with
/// `direct` those made from it when a worker first reads.
pub(super) struct Files {
    first: std::fs::File,
    others: Box<[OnceLock<Option<std::fs::File>>]>,
}

impl Files {
    pub(super) fn new(first: std::fs::File, io: Io) -> Files {
        let others = match io {
            Io::Cached => 0,
            Io::Direct => std::thread::available_parallelism().map_or(4, |n| n.get()),
        };
        Files {
            first,
            others: (0..others).map(|_| OnceLock::new()).collect(),
        }
    }

    /// Reads at `offset`; as [`FileExt::seek_read`], which it is when
    /// files are read as before.
    pub(super) fn read_at(&self, out: &mut [u8], offset: u64) -> std::io::Result<usize> {
        let other = match self.others.len() {
            0 => None,
            n => self.others[this_thread() % n]
                .get_or_init(|| reopen(&self.first))
                .as_ref(),
        };
        match other {
            Some(file) => read_direct(file, out, offset),
            None => self.first.seek_read(out, offset),
        }
    }
}

/// Reads past the file cache, its bounces and its fallbacks, since the last
/// [`take_counts`].
static READS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static BOUNCED: AtomicU64 = AtomicU64::new(0);
static FALLBACKS: AtomicU64 = AtomicU64::new(0);

/// What was read past the file cache since the last call, for the debug
/// summary: reads, bytes, reads that went through the aligned buffer, and
/// handles that could not be opened past the cache (whose reads went
/// through the cache).
pub(super) fn take_counts() -> [u64; 4] {
    [&READS, &BYTES, &BOUNCED, &FALLBACKS].map(|c| c.swap(0, Ordering::Relaxed))
}

/// A number for this thread: 0 for the first that asks, and so on.
fn this_thread() -> usize {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    thread_local! {
        static NUMBER: usize = NEXT.fetch_add(1, Ordering::Relaxed) as usize;
    }
    NUMBER.with(|n| *n)
}

/// Another handle to `file`, for overlapped reads past the file cache;
/// `None` if the host gives none (then the first one is used).
fn reopen(file: &std::fs::File) -> Option<std::fs::File> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows_sys::Win32::Foundation::{GENERIC_READ, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_NO_BUFFERING, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, ReOpenFile,
    };
    let flags = FILE_FLAG_OVERLAPPED | FILE_FLAG_NO_BUFFERING;
    // SAFETY: a file we hold open.
    let handle = unsafe { ReOpenFile(file.as_raw_handle(), GENERIC_READ, FILE_SHARE_READ, flags) };
    if handle == INVALID_HANDLE_VALUE {
        FALLBACKS.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            error = %std::io::Error::last_os_error(),
            "dstorage: a file could not be opened past the file cache; read through it"
        );
        return None;
    }
    // SAFETY: a handle just opened, ours alone.
    Some(unsafe { std::fs::File::from_raw_handle(handle) })
}

/// A read through a handle opened for overlapped reads, waited for.
fn read_overlapped(file: &std::fs::File, out: &mut [u8], offset: u64) -> std::io::Result<usize> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_HANDLE_EOF, ERROR_IO_PENDING, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::ReadFile;
    use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Threading::CreateEventW;
    /// Set when this thread's read is done.
    struct Event(HANDLE);
    impl Drop for Event {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: our own event.
                unsafe { CloseHandle(self.0) };
            }
        }
    }
    thread_local! {
        static DONE: Event =
            // SAFETY: a plain manual-reset event.
            Event(unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) });
    }
    DONE.with(|done| {
        if done.0.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let mut overlapped = OVERLAPPED {
            hEvent: done.0,
            ..Default::default()
        };
        overlapped.Anonymous.Anonymous.Offset = offset as u32;
        overlapped.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
        // 1 GiB, a multiple of every sector size.
        let len = out.len().min(1 << 30) as u32;
        let mut n = 0;
        // SAFETY: `out` and `overlapped` stay until the read is done, which
        // is waited for.
        let mut read = unsafe {
            ReadFile(
                file.as_raw_handle(),
                out.as_mut_ptr(),
                len,
                std::ptr::null_mut(),
                &mut overlapped,
            )
        } != 0;
        let mut error = std::io::Error::last_os_error();
        if read || error.raw_os_error() == Some(ERROR_IO_PENDING as i32) {
            // SAFETY: as above.
            read =
                unsafe { GetOverlappedResult(file.as_raw_handle(), &overlapped, &mut n, 1) } != 0;
            error = std::io::Error::last_os_error();
        }
        match error.raw_os_error() {
            _ if read => Ok(n as usize),
            Some(code) if code == ERROR_HANDLE_EOF as i32 => Ok(0),
            _ => Err(error),
        }
    })
}

/// Every disk's sectors are at most this; offsets, sizes and addresses
/// that are multiples of it are multiples of the sector size too.
const SECTOR: usize = 4096;
/// Read at a time through the aligned buffer.
const MOST: usize = 1 << 20;

/// A read past the file cache, counted.
fn read_direct(file: &std::fs::File, out: &mut [u8], offset: u64) -> std::io::Result<usize> {
    thread_local! {
        static BUFFER: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    let (n, bounced) = BUFFER.with_borrow_mut(|buffer| {
        read_sectors(out, offset, buffer, |into, at| {
            read_overlapped(file, into, at)
        })
    })?;
    READS.fetch_add(1, Ordering::Relaxed);
    BYTES.fetch_add(n as u64, Ordering::Relaxed);
    BOUNCED.fetch_add(u64::from(bounced), Ordering::Relaxed);
    Ok(n)
}

/// Reads at `offset` into `out` through `read`, which takes only whole
/// sectors (offset, size and address multiples of [`SECTOR`]) and gives
/// fewer bytes only at the end of the file, and says whether the read went
/// through `buffer` (grown to [`MOST`] + [`SECTOR`] on first use): `out`
/// is read into as it is when it is whole sectors, and otherwise the
/// sectors around its first [`MOST`] bytes (less what comes before them)
/// are read into `buffer` and copied out. Gives fewer bytes than
/// `out.len()` when the end of the file or [`MOST`] comes first, and 0 at
/// or past the end, as [`FileExt::seek_read`].
fn read_sectors(
    out: &mut [u8],
    offset: u64,
    buffer: &mut Vec<u8>,
    mut read: impl FnMut(&mut [u8], u64) -> std::io::Result<usize>,
) -> std::io::Result<(usize, bool)> {
    let sector = SECTOR as u64;
    if offset.is_multiple_of(sector)
        && out.len().is_multiple_of(SECTOR)
        && (out.as_ptr() as usize).is_multiple_of(SECTOR)
    {
        return Ok((read(out, offset)?, false));
    }
    if buffer.len() < MOST + SECTOR {
        buffer.resize(MOST + SECTOR, 0);
    }
    let skip = buffer.as_ptr().align_offset(SECTOR);
    let before = (offset % sector) as usize;
    let len = (before + out.len()).next_multiple_of(SECTOR).min(MOST);
    let aligned = &mut buffer[skip..skip + len];
    let got = read(aligned, offset - before as u64)?;
    let n = got.saturating_sub(before).min(out.len());
    out[..n].copy_from_slice(&aligned[before..before + n]);
    Ok((n, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 + i / 251) as u8).collect()
    }

    /// A reader that, as Windows past the file cache, refuses anything but
    /// whole sectors, and gives fewer bytes only at the end of the file.
    fn strict(file: &[u8]) -> impl FnMut(&mut [u8], u64) -> std::io::Result<usize> + '_ {
        move |into: &mut [u8], at: u64| {
            if !at.is_multiple_of(SECTOR as u64)
                || !into.len().is_multiple_of(SECTOR)
                || !(into.as_ptr() as usize).is_multiple_of(SECTOR)
            {
                return Err(std::io::Error::from_raw_os_error(87));
            }
            let at = (at as usize).min(file.len());
            let n = into.len().min(file.len() - at);
            into[..n].copy_from_slice(&file[at..at + n]);
            Ok(n)
        }
    }

    /// A buffer of `len` bytes at an address `misalign` past a sector's.
    fn placed(len: usize, misalign: usize) -> (Vec<u8>, usize) {
        let room = vec![0u8; len + 2 * SECTOR];
        let start = room.as_ptr().align_offset(SECTOR) + misalign;
        (room, start)
    }

    #[test]
    fn the_setting_takes_direct_only() {
        assert_eq!(Io::parse(None), Io::Cached);
        assert_eq!(Io::parse(Some("")), Io::Cached);
        assert_eq!(Io::parse(Some("handles")), Io::Cached);
        assert_eq!(Io::parse(Some("Direct")), Io::Cached);
        assert_eq!(Io::parse(Some("direct")), Io::Direct);
        assert_eq!(Io::parse(Some(" direct ")), Io::Direct);
    }

    /// Whatever the offset, size and address, the reader is asked for whole
    /// sectors only, and the bytes are the file's, to its end.
    #[test]
    fn reads_past_the_cache_ask_for_whole_sectors_only() {
        let file = pattern(3 * MOST + 12_345);
        let mut buffer = Vec::new();
        let ats = [
            0,
            1,
            511,
            4095,
            4096,
            8191,
            12_345,
            MOST - 1,
            MOST,
            2 * MOST + 4097,
        ];
        let lens = [
            1,
            2,
            100,
            4095,
            4096,
            4097,
            8192,
            65_536,
            MOST - 1,
            MOST,
            MOST + 1,
            2 * MOST,
        ];
        for at in ats {
            for len in lens {
                for misalign in [0, 1, 16, 4095] {
                    let (mut room, start) = placed(len, misalign);
                    let out = &mut room[start..start + len];
                    let mut done = 0;
                    while done < len {
                        let (n, bounced) = read_sectors(
                            &mut out[done..],
                            (at + done) as u64,
                            &mut buffer,
                            strict(&file),
                        )
                        .unwrap();
                        let whole = (at + done).is_multiple_of(SECTOR)
                            && (len - done).is_multiple_of(SECTOR)
                            && (out[done..].as_ptr() as usize).is_multiple_of(SECTOR);
                        assert_eq!(bounced, !whole, "{at} {len} {misalign} {done}");
                        if n == 0 {
                            break;
                        }
                        assert!(n <= MOST || !bounced);
                        done += n;
                    }
                    let end = (at + len).min(file.len());
                    assert_eq!(done, end - at, "{at} {len} {misalign}");
                    assert!(out[..done] == file[at..end], "{at} {len} {misalign}");
                }
            }
        }
    }

    /// At and past the end of the file: 0 bytes, also through the buffer.
    #[test]
    fn reads_past_the_cache_end_with_the_file() {
        let file = pattern(10_000);
        let mut buffer = Vec::new();
        let mut out = [0u8; 16];
        for at in [9_999, 10_000, 10_001, 12_288, 40_000] {
            let (n, _) = read_sectors(&mut out, at, &mut buffer, strict(&file)).unwrap();
            assert_eq!(n, 10_000usize.saturating_sub(at as usize).min(16), "{at}");
        }
        let (mut room, start) = placed(2 * SECTOR, 0);
        let out = &mut room[start..start + 2 * SECTOR];
        assert_eq!(
            read_sectors(out, 8192, &mut buffer, strict(&file)).unwrap(),
            (10_000 - 8192, false)
        );
        assert_eq!(
            read_sectors(out, 12_288, &mut buffer, strict(&file)).unwrap(),
            (0, false)
        );
    }

    /// Both ways of reading a real file give its bytes, also past its end.
    #[test]
    fn every_way_of_reading_gives_the_same_bytes() {
        let plain = pattern(300_001);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        std::fs::write(&path, &plain).unwrap();
        for io in [Io::Cached, Io::Direct] {
            let file = Files::new(std::fs::File::open(&path).unwrap(), io);
            for (at, len) in [
                (0, 4096),
                (1, 100),
                (4095, 2),
                (8192, 200_000),
                (12_345, 250_000),
                (299_990, 11),
                (300_000, 1),
            ] {
                let mut out = vec![0u8; len];
                let mut done = 0;
                while done < len {
                    let n = file.read_at(&mut out[done..], (at + done) as u64).unwrap();
                    assert_ne!(n, 0, "{io:?} {at} {len}");
                    done += n;
                }
                assert!(out == plain[at..at + len], "{io:?} {at} {len}");
            }
            let mut out = [0u8; 16];
            assert_eq!(file.read_at(&mut out, 299_999).unwrap(), 2, "{io:?}");
            assert_eq!(file.read_at(&mut out, 300_001).unwrap(), 0, "{io:?}");
            assert_eq!(file.read_at(&mut out, 400_000).unwrap(), 0, "{io:?}");
        }
    }
}

//! What the session's first line says of the PC, and the memory the writer
//! thread samples: cheap system questions, no personal details.

/// The Windows build ("Windows 11, build 26200"), or the OS family.
pub fn os() -> String {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct VersionInfo {
            size: u32,
            major: u32,
            minor: u32,
            build: u32,
            platform: u32,
            service_pack: [u16; 128],
        }
        #[link(name = "ntdll")]
        unsafe extern "system" {
            fn RtlGetVersion(info: *mut VersionInfo) -> i32;
        }
        let mut v = VersionInfo {
            size: size_of::<VersionInfo>() as u32,
            major: 0,
            minor: 0,
            build: 0,
            platform: 0,
            service_pack: [0; 128],
        };
        // SAFETY: a sized structure the call fills in; it does not fail.
        if unsafe { RtlGetVersion(&mut v) } == 0 {
            let name = if v.major == 10 && v.build >= 22000 {
                "Windows 11".to_owned()
            } else {
                format!("Windows {}.{}", v.major, v.minor)
            };
            return format!("{name}, build {}", v.build);
        }
    }
    std::env::consts::OS.to_owned()
}

/// The processor's brand string.
pub fn cpu() -> String {
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::x86_64::__cpuid;
        // cpuid exists on every x86-64 processor.
        let top = __cpuid(0x8000_0000).eax;
        if top >= 0x8000_0004 {
            let mut bytes = Vec::with_capacity(48);
            for leaf in 0x8000_0002..=0x8000_0004u32 {
                let r = __cpuid(leaf);
                for reg in [r.eax, r.ebx, r.ecx, r.edx] {
                    bytes.extend_from_slice(&reg.to_le_bytes());
                }
            }
            let s = String::from_utf8_lossy(&bytes);
            let s = s.trim_matches(char::from(0)).trim();
            if !s.is_empty() {
                return s.split_whitespace().collect::<Vec<_>>().join(" ");
            }
        }
    }
    "desconhecida".to_owned()
}

/// Logical processors.
pub fn logical_cpus() -> usize {
    std::thread::available_parallelism().map_or(0, |n| n.get())
}

/// Physical memory: total and free, in MiB.
pub fn memory_mb() -> Option<(u64, u64)> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        // SAFETY: plain data the call fills in.
        let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        m.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
        // SAFETY: a sized structure.
        if unsafe { GlobalMemoryStatusEx(&mut m) } != 0 {
            return Some((m.ullTotalPhys >> 20, m.ullAvailPhys >> 20));
        }
    }
    None
}

/// The folders card drivers keep their shader caches in, under the
/// user's own folders: (environment variable, path below it).
const DRIVER_CACHES: [(&str, &str); 5] = [
    ("LOCALAPPDATA", "NVIDIA/DXCache"),
    ("LOCALAPPDATA", "D3DSCache"),
    ("LOCALAPPDATA", "AMD/DxCache"),
    ("LOCALAPPDATA", "AMD/DxcCache"),
    ("LOCALAPPDATA", "Intel/ShaderCache"),
];

/// The card drivers' shader caches of this user: megabytes and files.
/// A cache of tens of gigabytes makes the host device slow to create and
/// a run's numbers unlike the next one's. `None` where there is none.
pub fn driver_cache() -> Option<(u64, u64)> {
    let (mut bytes, mut files) = (0u64, 0u64);
    for (var, below) in DRIVER_CACHES {
        let Some(base) = std::env::var_os(var) else {
            continue;
        };
        let (b, f) = folder_size(&std::path::Path::new(&base).join(below), 3);
        bytes += b;
        files += f;
    }
    (files > 0).then_some((bytes >> 20, files))
}

/// Bytes and files under `dir`, `depth` folders deep at most; what cannot
/// be read counts as nothing.
fn folder_size(dir: &std::path::Path, depth: u32) -> (u64, u64) {
    let (mut bytes, mut files) = (0u64, 0u64);
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_file() {
            bytes += meta.len();
            files += 1;
        } else if meta.is_dir() && depth > 0 {
            let (b, f) = folder_size(&entry.path(), depth - 1);
            bytes += b;
            files += f;
        }
    }
    (bytes, files)
}

/// This process's page faults so far.
pub fn page_faults() -> Option<u64> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::ProcessStatus::{
            K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: plain data the call fills in.
        let mut c: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
        c.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        // SAFETY: our own process and a sized structure.
        if unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) } != 0 {
            return Some(u64::from(c.PageFaultCount));
        }
    }
    None
}

/// `(year, month, day, hour, minute, second, millisecond)` in UTC.
pub fn utc(unix_ms: u64) -> (i64, u32, u32, u32, u32, u32, u32) {
    let secs = (unix_ms / 1000) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (
        y,
        m,
        d,
        (rem / 3600) as u32,
        (rem % 3600 / 60) as u32,
        (rem % 60) as u32,
        (unix_ms % 1000) as u32,
    )
}

/// RFC 3339 with milliseconds: `2026-11-26T15:30:12.345Z`.
pub fn rfc3339(unix_ms: u64) -> String {
    let (y, mo, d, h, mi, s, ms) = utc(unix_ms);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{ms:03}Z")
}

/// For a folder name: `20261126T153012Z`.
pub fn compact(unix_ms: u64) -> String {
    let (y, mo, d, h, mi, s, _) = utc(unix_ms);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_dates() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00.000Z");
        // 2026-11-26 15:30:12.345 UTC
        assert_eq!(rfc3339(1_795_707_012_345), "2026-11-26T15:30:12.345Z");
        assert_eq!(compact(1_795_707_012_345), "20261126T153012Z");
        // A leap day.
        assert_eq!(rfc3339(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn system_questions_answer() {
        assert!(!os().is_empty());
        assert!(!cpu().is_empty());
        if cfg!(windows) {
            let (total, free) = memory_mb().unwrap();
            assert!(total >= free && total > 0);
            assert!(page_faults().unwrap() > 0);
        }
    }
}

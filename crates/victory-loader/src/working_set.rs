//! A hard ceiling on this process's working set (`VICTORY_WORKING_SET_MB`;
//! unset or 0, the default: none).
//!
//! On a PC with less RAM than the title asks for (16 GB: the Series S
//! profile commits 15 to 18 GB of private memory in the world, the RTX 4090
//! test PC holds 23.5 GB), Windows otherwise takes the pages it needs from
//! everything that runs, the desktop included, and the PC stalls on paging
//! before the game does. With a ceiling, the pages past it are the game's
//! own: Windows trims them to its standby and modified lists, from where
//! they come back without a disk read while they last (a soft fault), and
//! from the page file once Windows has given their RAM to someone else (a
//! hard fault). Measured on a 16 GB laptop with an RTX 3050 6 GB: with no
//! ceiling, free RAM ran out before the world loaded; with 4,500 MB both
//! runs loaded the world and ran in it for minutes, with 0.5 to 1.2 GB of
//! RAM free.
//!
//! What the value means: the most RAM Windows lets the process hold at
//! once (the maximum is hard). The minimum is a soft 64 MB, so nothing is
//! held back from the rest of the PC: the ceiling only bounds the game, it
//! never reserves RAM for it. The cost is the game's own faults: the
//! smaller the ceiling, the more of its pages it takes back from the
//! standby list (or the page file) each frame. The laptop's preset was
//! RAM - 11.6 GB, at least 3 GB: 4,500 MB on 16 GB; 5,000 MB was no faster
//! there.
//!
//! Not with `VICTORY_MEMORY_FLOOR_GB`: Windows keeps one minimum and one
//! maximum per process, the floor is a hard minimum and this a hard
//! maximum, and the second call would undo the first. With both set, the
//! floor stays and the ceiling is not set (said so in the log).
//!
//! The ceiling is set once, before the title starts, and Windows keeps it
//! for the process; pages trimmed under it stay trimmed. It is not one of
//! the `SPARK_AB` switches for that reason.
//!
//! From TaynanGT/Vice-Tay 53dc899 (by nidscode), merged there as 6d26edd.

/// The ceiling asked for, in bytes: `VICTORY_WORKING_SET_MB`, from 1,024 MB
/// (smaller would keep only the layer's own code in memory) up; `None`
/// for none (unset, 0, or not a number).
pub fn asked() -> Option<u64> {
    let spec = std::env::var("VICTORY_WORKING_SET_MB").ok()?;
    let ceiling = parse_mb(&spec);
    if ceiling.is_none() && spec.trim() != "0" {
        eprintln!(
            "VIctory: VICTORY_WORKING_SET_MB={spec}: expected megabytes, 1024 or more (0: none)"
        );
    }
    ceiling
}

/// [`asked`] for a setting: megabytes from 1,024 up, as bytes; `None` for
/// fewer, for a number that is not one, and for one whose bytes would not
/// fit in an address (`usize`) or in `u64`.
pub fn parse_mb(spec: &str) -> Option<u64> {
    let mb: u64 = spec.trim().parse().ok()?;
    if mb < 1024 {
        return None;
    }
    let bytes = mb.checked_mul(1 << 20)?;
    usize::try_from(bytes).ok()?;
    Some(bytes)
}

/// Whether the floor (`VICTORY_MEMORY_FLOOR_GB`, `victory_hle::modules::
/// xmem::keep_in_memory`) is asked for, as that function reads it: a whole
/// number above 0.
#[cfg_attr(not(windows), allow(dead_code))]
fn floor_asked(spec: Option<&str>) -> bool {
    spec.and_then(|v| v.trim().parse::<usize>().ok())
        .is_some_and(|gb| gb > 0)
}

/// Sets the ceiling [`asked`] for, if any, and says what it did.
#[cfg(windows)]
pub fn apply_asked() {
    use windows_sys::Win32::System::Memory::{
        QUOTA_LIMITS_HARDWS_MAX_ENABLE, QUOTA_LIMITS_HARDWS_MIN_DISABLE, SetProcessWorkingSetSizeEx,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let Some(ceiling) = asked() else {
        return;
    };
    if floor_asked(std::env::var("VICTORY_MEMORY_FLOOR_GB").ok().as_deref()) {
        tracing::warn!(
            ceiling_mb = ceiling >> 20,
            "working set ceiling (VICTORY_WORKING_SET_MB) not set: VICTORY_MEMORY_FLOOR_GB is set, and one would undo the other"
        );
        eprintln!(
            "VIctory: working set ceiling {} MB: not set (VICTORY_MEMORY_FLOOR_GB is set)",
            ceiling >> 20
        );
        return;
    }
    // A soft minimum, so that Windows can trim under RAM pressure.
    const FLOOR: usize = 64 << 20;
    // SAFETY: the pseudo-handle of this process, which needs no closing.
    let set = unsafe {
        SetProcessWorkingSetSizeEx(
            GetCurrentProcess(),
            FLOOR,
            ceiling as usize, // fits: `parse_mb` checks it
            QUOTA_LIMITS_HARDWS_MAX_ENABLE | QUOTA_LIMITS_HARDWS_MIN_DISABLE,
        )
    } != 0;
    let error = if set {
        0
    } else {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    };
    tracing::info!(
        ceiling_mb = ceiling >> 20,
        set,
        error,
        "working set ceiling (VICTORY_WORKING_SET_MB): past it Windows trims the game's own pages"
    );
    eprintln!(
        "VIctory: working set ceiling {} MB: {}",
        ceiling >> 20,
        if set {
            "set".to_string()
        } else {
            format!("not set (error {error})")
        }
    );
}

#[cfg(test)]
mod tests {
    use super::{floor_asked, parse_mb};

    #[test]
    fn the_working_set_ceiling_is_read_in_megabytes_from_1024() {
        assert_eq!(parse_mb("4500"), Some(4500 << 20));
        assert_eq!(parse_mb(" 1024 "), Some(1024 << 20));
        assert_eq!(parse_mb("1023"), None);
        assert_eq!(parse_mb("0"), None);
        assert_eq!(parse_mb("4.5"), None);
        assert_eq!(parse_mb(""), None);
        assert_eq!(parse_mb("-4500"), None);
        // Bytes that would not fit: refused, not wrapped around.
        assert_eq!(parse_mb(&u64::MAX.to_string()), None);
        assert_eq!(parse_mb(&((u64::MAX >> 20) + 1).to_string()), None);
        assert_eq!(
            parse_mb(&(u64::MAX >> 20).to_string()),
            Some((u64::MAX >> 20) << 20)
        );
        assert_eq!(parse_mb("18446744073709551616"), None);
    }

    #[test]
    fn the_floor_is_seen_as_xmem_reads_it() {
        assert!(!floor_asked(None));
        assert!(!floor_asked(Some("")));
        assert!(!floor_asked(Some("0")));
        assert!(!floor_asked(Some("x")));
        assert!(!floor_asked(Some("1.5")));
        assert!(floor_asked(Some("14")));
        assert!(floor_asked(Some(" 4 ")));
    }
}

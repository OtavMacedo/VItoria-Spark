//! The switches that may alternate inside a run (`SPARK_AB`,
//! `victory_core::ab`), in one table.
//!
//! A switch belongs here only if every use of it decides on its own: the
//! arm changes while other threads are in the middle of the next frame,
//! so nothing may be kept that is right under one value and wrong under
//! the other. A switch read once to size or to make something stays out.

use victory_core::ab::Toggle;

/// `VICTORY_CARD_SYNC_BATCH` (`crate::host`).
pub(crate) static CARD_SYNC_BATCH: Toggle = Toggle::flag("VICTORY_CARD_SYNC_BATCH");
/// `VICTORY_COPY_PLAIN` (`crate::plaincopy`).
pub(crate) static COPY_PLAIN: Toggle = Toggle::flag("VICTORY_COPY_PLAIN");
/// `VICTORY_MERGE_MASK` (`crate::card_mask`): the threshold, 0 off.
pub(crate) static MERGE_MASK: Toggle = Toggle::new("VICTORY_MERGE_MASK", merge_mask);
/// `SPARK_PHASES` (`crate::phases`): frames read part by part for the
/// session log, with `1`. In the table so that a run can measure what
/// the reading itself costs.
pub(crate) static PHASES: Toggle = Toggle::flag("SPARK_PHASES");
/// `VICTORY_SKIP_WC_BACK` (`crate::census`). What was not copied back under
/// the arm that skips stays so under the other: a run that alternates it
/// measures time, not the image.
pub(crate) static SKIP_WC_BACK: Toggle = Toggle::flag("VICTORY_SKIP_WC_BACK");
/// `VICTORY_WC_NO_BACK` (`crate::shadow`): shader writes to big copies of
/// write-combining title memory stay on the card, decided at each
/// declaration. As with [`SKIP_WC_BACK`], what stayed on the card under one
/// arm is not brought back under the other, so a run that alternates it
/// measures time, not the image. Its floor, `VICTORY_WC_NO_BACK_MIN_KB`, is
/// read once and holds in both arms.
pub(crate) static WC_NO_BACK: Toggle = Toggle::flag("VICTORY_WC_NO_BACK");
/// `VICTORY_STEP_INDEX` (`crate::host`).
pub(crate) static STEP_INDEX: Toggle = Toggle::flag("VICTORY_STEP_INDEX");
/// `VICTORY_TEXTURE_ROOM_PERCENT` (`crate::resource`): percent, 50 to 97.
pub(crate) static TEXTURE_ROOM_PERCENT: Toggle =
    Toggle::new("VICTORY_TEXTURE_ROOM_PERCENT", texture_room_percent);

/// `VICTORY_WIDE_MERGE` (`crate::host`).
pub(crate) static WIDE_MERGE: Toggle = Toggle::flag("VICTORY_WIDE_MERGE");
/// `VICTORY_WIDE_REFRESH` (`crate::host`): each flush decides on its own
/// whether its refreshes go to the helper threads.
pub(crate) static WIDE_REFRESH: Toggle = Toggle::flag("VICTORY_WIDE_REFRESH");

/// `VICTORY_WIDE_THREADS` (`crate::shadow`): how many of the threads kept
/// for work spread over threads take jobs ([`wide_threads`]). As many
/// threads are kept as the largest value of the run asks for, and the
/// others are parked while a smaller one is in force, so the number may
/// alternate: `SPARK_AB=VICTORY_WIDE_THREADS=4/8` (the plain name would
/// give the arms `0`, the default, and `1`).
pub(crate) static WIDE_THREADS: Toggle = Toggle::new("VICTORY_WIDE_THREADS", wide_threads);

/// `VICTORY_WIDE_WAKE_ONE` (`crate::shadow`): each call of the spread work
/// decides on its own how many kept threads it wakes.
pub(crate) static WIDE_WAKE_ONE: Toggle = Toggle::flag("VICTORY_WIDE_WAKE_ONE");

/// `VICTORY_LIST_REAPER` (`crate::list_reaper`): each landing decides on
/// its own where its lists are reset; lists handed to the thread under one
/// arm are reset there under the other.
pub(crate) static LIST_REAPER: Toggle = Toggle::flag("VICTORY_LIST_REAPER");

/// `VICTORY_MAPPED_BATCH` (`crate::texture`).
pub(crate) static MAPPED_BATCH: Toggle = Toggle::flag("VICTORY_MAPPED_BATCH");

/// `VICTORY_HEAP_PASS_MARKED` (`crate::heap_pass`; each change of arm starts the pass anew).
pub(crate) static HEAP_PASS_MARKED: Toggle = Toggle::flag("VICTORY_HEAP_PASS_MARKED");

/// `VICTORY_HEAP_LOCKS` (`crate::descriptor::host`).
pub(crate) static HEAP_LOCKS: Toggle = Toggle::flag("VICTORY_HEAP_LOCKS");

/// `VICTORY_ASYNC_PRESENT` (`crate::queue`).
pub(crate) static ASYNC_PRESENT: Toggle = Toggle::flag("VICTORY_ASYNC_PRESENT");

/// `VICTORY_UNGUARD_RUNS` (`crate::watch`): each opening of guarded blocks
/// decides on its own; the protections it leaves are the same either way.
pub(crate) static UNGUARD_RUNS: Toggle = Toggle::flag("VICTORY_UNGUARD_RUNS");

/// `VICTORY_READ_BACK_POOL` (`crate::host`): a buffer made under the other
/// arm is let go, not kept.
pub(crate) static READ_BACK_POOL: Toggle = Toggle::flag("VICTORY_READ_BACK_POOL");

/// `VICTORY_UNDER_WIDE` (`crate::shadow`): each range of the merge under
/// flights' writes decides on its own; the bytes put in are the same.
pub(crate) static UNDER_WIDE: Toggle = Toggle::flag("VICTORY_UNDER_WIDE");

/// `VICTORY_BIND_CACHE` (`crate::bind_cache`; what it remembers goes at
/// the first call with it off).
pub(crate) static BIND_CACHE: Toggle = Toggle::flag("VICTORY_BIND_CACHE");

/// `VICTORY_DESC_MEMCPY` (`crate::descriptor::bytes`; what it kept goes
/// at the first view made with it off).
pub(crate) static DESC_MEMCPY: Toggle = Toggle::flag("VICTORY_DESC_MEMCPY");

/// `VICTORY_GUARD_REST` (`crate::watch`'s rests): microseconds a block a
/// write fault opened is left open, 0 off. Every asking and every fault
/// reads it; a block resting when it goes off is open and not clean, as a
/// block any fault opened, and is made read-only at its next asking.
pub(crate) static GUARD_REST: Toggle = Toggle::new("VICTORY_GUARD_REST", crate::watch::rest_us);
/// `VICTORY_GUARD_HOT` (`crate::watch`'s rests): frames a block that
/// faults four times in a frame is left open, 0 off. As above.
pub(crate) static GUARD_HOT: Toggle = Toggle::new("VICTORY_GUARD_HOT", crate::watch::hot_frames);

/// A switch of the table: the part of the frame it changes (what the
/// summary's frame parts show it in), and the settings that leave it
/// without effect.
pub struct Entry {
    /// The switch.
    pub toggle: &'static Toggle,
    /// The frame part it changes: `copias`, `copia_de_volta`, `preparo`,
    /// `texturas` or `gpu`; `medicao` for a switch of the measuring
    /// itself, which no plan made from a run's problems proposes.
    pub part: &'static str,
    /// Another variable, a test of its value (`None` unset), and why the
    /// switch does nothing when the test holds.
    pub inert: &'static [(&'static str, fn(Option<&str>) -> bool, &'static str)],
}

/// `VICTORY_QUEUE2` with bit 2: title bytes go up on the copy queue.
fn uploads_on_copy_queue(value: Option<&str>) -> bool {
    value
        .and_then(|v| v.trim().parse::<u32>().ok())
        .is_some_and(|bits| bits & 2 != 0)
}

/// Every switch above.
pub static TABLE: [Entry; 24] = [
    Entry {
        toggle: &ASYNC_PRESENT,
        part: "preparo",
        inert: &[],
    },
    Entry {
        toggle: &BIND_CACHE,
        part: "preparo",
        inert: &[],
    },
    Entry {
        toggle: &HEAP_LOCKS,
        part: "preparo",
        inert: &[],
    },
    Entry {
        toggle: &HEAP_PASS_MARKED,
        part: "preparo",
        inert: &[],
    },
    Entry {
        toggle: &MAPPED_BATCH,
        part: "preparo",
        inert: &[],
    },
    Entry {
        toggle: &DESC_MEMCPY,
        part: "preparo",
        inert: &[],
    },
    Entry {
        toggle: &CARD_SYNC_BATCH,
        part: "copias",
        inert: &[],
    },
    Entry {
        toggle: &COPY_PLAIN,
        part: "copias",
        inert: &[],
    },
    Entry {
        toggle: &GUARD_HOT,
        part: "copias",
        inert: &[],
    },
    Entry {
        toggle: &GUARD_REST,
        part: "copias",
        inert: &[],
    },
    Entry {
        toggle: &MERGE_MASK,
        part: "copias",
        inert: &[(
            "VICTORY_QUEUE2",
            uploads_on_copy_queue,
            "with bit 2 the changed bytes go up through the copy queue, which the masked upload does not use",
        )],
    },
    Entry {
        toggle: &PHASES,
        part: "medicao",
        inert: &[],
    },
    Entry {
        toggle: &SKIP_WC_BACK,
        part: "copia_de_volta",
        inert: &[],
    },
    Entry {
        toggle: &WC_NO_BACK,
        part: "copia_de_volta",
        inert: &[],
    },
    Entry {
        toggle: &STEP_INDEX,
        part: "preparo",
        inert: &[],
    },
    Entry {
        toggle: &TEXTURE_ROOM_PERCENT,
        part: "texturas",
        inert: &[],
    },
    Entry {
        toggle: &WIDE_MERGE,
        part: "copia_de_volta",
        inert: &[],
    },
    Entry {
        toggle: &WIDE_REFRESH,
        part: "copias",
        inert: &[],
    },
    Entry {
        toggle: &UNGUARD_RUNS,
        part: "copias",
        inert: &[],
    },
    Entry {
        toggle: &READ_BACK_POOL,
        part: "preparo",
        inert: &[],
    },
    Entry {
        toggle: &UNDER_WIDE,
        part: "preparo",
        inert: &[],
    },
    Entry {
        toggle: &WIDE_THREADS,
        part: "copias",
        inert: &[],
    },
    Entry {
        toggle: &WIDE_WAKE_ONE,
        part: "copias",
        inert: &[],
    },
    Entry {
        toggle: &LIST_REAPER,
        part: "preparo",
        inert: &[],
    },
];

/// Checks `SPARK_AB`/`SPARK_PLAN` against the table, before the title
/// starts: the error says which switch cannot alternate and which can
/// (`victory_core::ab::check`), or which setting would leave an
/// alternating switch without effect (the run would measure nothing).
pub fn check_ab() -> Result<(), String> {
    let toggles: Vec<&Toggle> = TABLE.iter().map(|e| e.toggle).collect();
    victory_core::ab::check(&toggles)?;
    check_inert(|name| std::env::var(name).ok())
}

/// [`check_ab`]'s second half, with the environment read by `var`.
fn check_inert(var: impl Fn(&str) -> Option<String>) -> Result<(), String> {
    let Some(program) = victory_core::ab::program() else {
        return Ok(());
    };
    for (name, plan) in &program.experiments {
        for s in &plan.switches {
            let Some(entry) = TABLE.iter().find(|e| e.toggle.name() == s.name) else {
                continue;
            };
            for (other, holds, why) in entry.inert {
                if holds(var(other).as_deref()) {
                    return Err(format!(
                        "{} (experiment {name}) has no effect with {other}={}: {why}",
                        s.name,
                        var(other).unwrap_or_default()
                    ));
                }
            }
        }
    }
    Ok(())
}

/// `VICTORY_MERGE_MASK`: unset, `0` or not a number off (0); `1` the
/// default threshold; a number from 2 up that threshold.
fn merge_mask(value: Option<&str>) -> u64 {
    match value.and_then(|v| v.trim().parse::<u64>().ok()) {
        None | Some(0) => 0,
        Some(1) => crate::card_mask::DEFAULT_THRESHOLD as u64,
        Some(n) => n,
    }
}

/// `VICTORY_WIDE_THREADS`: the helper threads of work spread over threads
/// on this PC ([`helpers_for`] its logical processors).
fn wide_threads(value: Option<&str>) -> u64 {
    let cores = std::thread::available_parallelism().ok().map(|n| n.get());
    helpers_for(cores, value.and_then(|v| v.trim().parse().ok())) as u64
}

/// How many helper threads work for `crate::shadow`'s spread work on a PC
/// of `cores` logical processors (`None`: not known): `asked` if given and
/// not 0, kept in 1 to all of them but two (the submitting thread and the
/// title's own need theirs; 14 if not known); else a quarter of them, 2 to
/// 6 (4 if not known), as without the variable.
pub(crate) fn helpers_for(cores: Option<usize>, asked: Option<usize>) -> usize {
    match asked {
        Some(n) if n > 0 => n.clamp(1, cores.map_or(14, |c| c.saturating_sub(2)).max(1)),
        _ => cores.map_or(4, |c| c / 4).clamp(2, 6),
    }
}

/// `VICTORY_TEXTURE_ROOM_PERCENT`: 90 unless a number, kept in 50 to 97.
fn texture_room_percent(value: Option<&str>) -> u64 {
    value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(90, |percent| percent.clamp(50, 97))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_read_as_their_switches_document() {
        assert_eq!(merge_mask(None), 0);
        assert_eq!(merge_mask(Some("0")), 0);
        assert_eq!(merge_mask(Some("x")), 0);
        assert_eq!(
            merge_mask(Some("1")),
            crate::card_mask::DEFAULT_THRESHOLD as u64
        );
        assert_eq!(merge_mask(Some(" 64 ")), 64);
        assert_eq!(texture_room_percent(None), 90);
        assert_eq!(texture_room_percent(Some("94")), 94);
        assert_eq!(texture_room_percent(Some("10")), 50);
        assert_eq!(texture_room_percent(Some("100")), 97);
    }

    /// Without the variable, a quarter of the logical processors, 2 to 6;
    /// with it, as asked, up to all of them but two.
    #[test]
    fn helpers_are_a_quarter_of_the_processors_or_as_asked() {
        assert_eq!(helpers_for(Some(16), None), 4);
        assert_eq!(helpers_for(Some(12), None), 3);
        assert_eq!(helpers_for(Some(4), None), 2);
        assert_eq!(helpers_for(Some(64), None), 6);
        assert_eq!(helpers_for(None, None), 4);
        assert_eq!(helpers_for(Some(16), Some(0)), 4);
        assert_eq!(helpers_for(Some(16), Some(8)), 8);
        assert_eq!(helpers_for(Some(16), Some(1)), 1);
        assert_eq!(helpers_for(Some(16), Some(99)), 14);
        assert_eq!(helpers_for(Some(2), Some(5)), 1);
        assert_eq!(helpers_for(None, Some(99)), 14);
        assert_eq!(wide_threads(Some("x")), wide_threads(None));
    }

    #[test]
    fn the_table_names_each_switch_once() {
        let parts = [
            "copias",
            "copia_de_volta",
            "preparo",
            "texturas",
            "gpu",
            "medicao",
        ];
        assert!(TABLE.iter().all(|e| parts.contains(&e.part)));
        assert!(uploads_on_copy_queue(Some("3")) && !uploads_on_copy_queue(Some("1")));
        assert!(!uploads_on_copy_queue(None));
        let mut names: Vec<&str> = TABLE.iter().map(|e| e.toggle.name()).collect();
        names.sort_unstable();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n);
        assert!(
            names
                .iter()
                .all(|n| n.starts_with("VICTORY_") || n.starts_with("SPARK_"))
        );
    }
}

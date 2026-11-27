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

/// `VICTORY_MAPPED_BATCH` (`crate::texture`).
pub(crate) static MAPPED_BATCH: Toggle = Toggle::flag("VICTORY_MAPPED_BATCH");

/// `VICTORY_HEAP_PASS_MARKED` (`crate::heap_pass`; each change of arm starts the pass anew).
pub(crate) static HEAP_PASS_MARKED: Toggle = Toggle::flag("VICTORY_HEAP_PASS_MARKED");

/// `VICTORY_HEAP_LOCKS` (`crate::descriptor::host`).
pub(crate) static HEAP_LOCKS: Toggle = Toggle::flag("VICTORY_HEAP_LOCKS");

/// `VICTORY_ASYNC_PRESENT` (`crate::queue`).
pub(crate) static ASYNC_PRESENT: Toggle = Toggle::flag("VICTORY_ASYNC_PRESENT");

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
pub static TABLE: [Entry; 13] = [
    Entry {
        toggle: &ASYNC_PRESENT,
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

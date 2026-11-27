//! Why draws and dispatches did not run on the host, by cause.
//!
//! One count of dropped work cannot tell a stage left out on purpose from
//! a shader the translator lacks; the causes are few and fixed, so each is
//! a counter of its own and the summary names the one to work on.

use super::json::{self, Obj};

/// Why a draw or dispatch was not run on the host.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub enum DropCause {
    /// Its host pipeline is still being made.
    BeingMade,
    /// One of its shaders does not translate.
    Untranslated,
    /// The host refused its pipeline.
    Refused,
    /// The title had set no pipeline.
    NoPipeline,
    /// A stage of the ray-tracing structure builder, which is off.
    BuilderOff,
    /// Its resources could not be bound.
    Unbound,
    /// Left out by a diagnostics switch.
    Skipped,
    /// The host call itself failed, or no cause was found.
    Other,
}

/// How many [`DropCause`]s there are.
pub const DROP_CAUSES: usize = 8;

/// The causes' names in the session files (`spark.<name>` in the
/// `draws_dropped` event), by [`DropCause`] as an index.
pub const DROP_NAMES: [&str; DROP_CAUSES] = [
    "being_made",
    "untranslated",
    "refused",
    "no_pipeline",
    "builder_off",
    "unbound",
    "skipped",
    "other",
];

/// The causes that are a choice of the settings, not a fault to fix.
pub const DROP_ON_PURPOSE: [DropCause; 2] = [DropCause::BuilderOff, DropCause::Skipped];

/// Whether the cause named `name` is one of [`DROP_ON_PURPOSE`].
pub fn on_purpose(name: &str) -> bool {
    DROP_ON_PURPOSE
        .iter()
        .any(|c| DROP_NAMES[*c as usize] == name)
}

/// Samples kept (one each five seconds: a day).
const MAX_SAMPLES: usize = 17_280;

/// The frame-rate lines' drops by cause, per frame.
#[derive(Default)]
pub struct DropTally {
    samples: Vec<[f32; DROP_CAUSES]>,
}

impl DropTally {
    /// One frame-rate line's drops per frame, by cause.
    pub fn add(&mut self, per_frame: [f32; DROP_CAUSES]) {
        if self.samples.len() < MAX_SAMPLES {
            self.samples.push(per_frame);
        }
    }

    /// The median of cause `i` over the lines, per frame.
    fn median(&self, i: usize) -> f64 {
        let mut v: Vec<f32> = self.samples.iter().map(|s| s[i]).collect();
        if v.is_empty() {
            return 0.0;
        }
        v.sort_by(f32::total_cmp);
        f64::from(v[v.len() / 2])
    }

    /// Per frame (medians): drops that are a fault, and drops on purpose.
    /// `None` without a sample.
    pub fn split(&self) -> Option<(f64, f64)> {
        if self.samples.is_empty() {
            return None;
        }
        let on_purpose = |i: usize| DROP_ON_PURPOSE.iter().any(|c| *c as usize == i);
        let sum = |want: bool| {
            (0..DROP_CAUSES)
                .filter(|i| on_purpose(*i) == want)
                .map(|i| self.median(i))
                .sum::<f64>()
        };
        Some((sum(false), sum(true)))
    }

    /// The causes seen, the most drops a frame first: name, median and
    /// most per frame.
    pub fn top(&self) -> Vec<(&'static str, f64, f64)> {
        let mut v: Vec<_> = (0..DROP_CAUSES)
            .map(|i| {
                let max = self.samples.iter().map(|s| s[i]).fold(0.0f32, f32::max);
                (DROP_NAMES[i], self.median(i), f64::from(max))
            })
            .filter(|(_, _, max)| *max > 0.0)
            .collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.2.total_cmp(&a.2)));
        v
    }

    /// `dropped_by_cause` of `resumo.json`.
    pub fn to_json(&self) -> String {
        json::array(self.top().into_iter().map(|(name, median, max)| {
            Obj::new()
                .str("cause", name)
                .num("per_frame_median", median)
                .num("per_frame_max", max)
                .bool(
                    "on_purpose",
                    DROP_ON_PURPOSE
                        .iter()
                        .any(|c| DROP_NAMES[*c as usize] == name),
                )
                .finish()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_on_purpose_are_told_from_faults() {
        let mut t = DropTally::default();
        assert_eq!(t.split(), None);
        for _ in 0..3 {
            let mut s = [0.0; DROP_CAUSES];
            s[DropCause::BuilderOff as usize] = 380.0;
            s[DropCause::Refused as usize] = 3.0;
            t.add(s);
        }
        assert_eq!(t.split(), Some((3.0, 380.0)));
        assert_eq!(
            t.to_json(),
            r#"[{"cause":"builder_off","per_frame_median":380,"per_frame_max":380,"on_purpose":true},{"cause":"refused","per_frame_median":3,"per_frame_max":3,"on_purpose":false}]"#
        );
    }
}

//! What each frame's time went to: one sample a presented frame, kept as
//! histograms, so the summary can say which part is the usual frame's
//! largest, which grows in its tail and which the frames of more than a
//! second were made of.
//!
//! The frame-rate line averages a second or more of frames, and its
//! medians hide a part that is small in most frames and large in a few.
//! Here every world frame adds one count per part: a dozen adds a frame,
//! under the lock the frame's length already takes.
//!
//! Parts are measured on several threads and some lie inside others
//! (copies, the write watch and textures inside preparing), so they do
//! not add up to the frame; each is read against the frame on its own.

use std::fmt::Write;

use super::json::{self, Obj};
use super::summary::Histogram;

/// A part of a frame's time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub enum Phase {
    /// Preparing submissions on the CPU, all of it.
    Preparing,
    /// Of preparing: copies of title memory.
    Copies,
    /// Of preparing: asking the system which pages were written.
    WriteWatch,
    /// Texture refreshes on every thread (reading, hashing, detiling).
    Textures,
    /// Waiting for the card.
    Gpu,
    /// Copying back what shaders wrote.
    CopyBack,
    /// The title's present call.
    Present,
    /// The runtime changing what title memory shows.
    Remap,
    /// Waiting for host pipelines being made.
    PipelineWait,
    /// The card with nothing of ours queued.
    CardIdle,
    /// Disk workers' time in finished requests.
    Disk,
    /// The title's own frame waits.
    Pace,
}

/// How many [`Phase`]s there are.
pub const PHASES: usize = 12;

/// The phases' names in the session files, by [`Phase`] as an index.
pub const PHASE_NAMES: [&str; PHASES] = [
    "preparing",
    "copies",
    "write_watch",
    "textures",
    "gpu",
    "copy_back",
    "present",
    "remap",
    "pipeline_wait",
    "card_idle",
    "disk",
    "pace",
];

/// The phases a long frame may be blamed on: those that do not lie inside
/// another and that stall a frame when they run long.
const BLAMED: [Phase; 5] = [
    Phase::Preparing,
    Phase::Gpu,
    Phase::CopyBack,
    Phase::Remap,
    Phase::PipelineWait,
];
/// The share of a long frame its largest blamed phase must reach to be
/// its cause; below it the frame is blamed on paging or left unexplained.
const BLAME_SHARE: f64 = 0.25;
/// Causes of long frames past the phases: paging, and none found.
const CAUSE_FAULTS: usize = PHASES;
const CAUSE_UNEXPLAINED: usize = PHASES + 1;

/// One frame's parts: microseconds per [`Phase`] and what moved in it.
#[derive(Clone, Copy, Default, Debug)]
pub struct FrameParts {
    /// Microseconds, by [`Phase`] as an index.
    pub us: [u64; PHASES],
    /// Bytes of title memory copied for submissions.
    pub copy_bytes: u64,
    /// Bytes copied back from the card.
    pub copy_back_bytes: u64,
    /// Askings of the system's write watch, and the 4 KiB pages they
    /// covered.
    pub watch_calls: u64,
    pub watch_pages: u64,
}

/// The frames of one scope (the world, or its stable scene).
#[derive(Default)]
struct Scope {
    frames: u64,
    frame_ms: f64,
    hist: [Histogram; PHASES],
    ms: [f64; PHASES],
    copy_bytes: u64,
    copy_back_bytes: u64,
    watch_calls: u64,
    watch_pages: u64,
}

impl Scope {
    fn add(&mut self, frame_ms: f64, parts: &FrameParts) {
        self.frames += 1;
        self.frame_ms += frame_ms;
        for (i, us) in parts.us.iter().enumerate() {
            let ms = *us as f64 / 1e3;
            self.hist[i].add(ms);
            self.ms[i] += ms;
        }
        self.copy_bytes += parts.copy_bytes;
        self.copy_back_bytes += parts.copy_back_bytes;
        self.watch_calls += parts.watch_calls;
        self.watch_pages += parts.watch_pages;
    }

    /// The mean of phase `i`, ms a frame.
    fn mean(&self, i: usize) -> f64 {
        self.ms[i] / self.frames.max(1) as f64
    }

    /// Megabytes a second while the phase ran; `None` if it never did.
    fn rate(&self, bytes: u64, phase: Phase) -> Option<f64> {
        let ms = self.ms[phase as usize];
        (ms > 0.0).then(|| bytes as f64 / 1e6 / (ms / 1e3))
    }

    fn to_json(&self, scope: &str) -> String {
        if self.frames == 0 {
            return "null".to_owned();
        }
        let n = self.frames as f64;
        let frame_mean = self.frame_ms / n;
        let parts = json::array((0..PHASES).map(|i| {
            let h = &self.hist[i];
            Obj::new()
                .str("name", PHASE_NAMES[i])
                .num("mean_ms", self.mean(i))
                .opt_num("p50_ms", h.percentile(0.50))
                .opt_num("p95_ms", h.percentile(0.95))
                .opt_num("p99_ms", h.percentile(0.99))
                .opt_num("max_ms", h.max())
                .num("share_pct", self.mean(i) / frame_mean.max(1e-9) * 100.0)
                .finish()
        }));
        Obj::new()
            .str("scope", scope)
            .int("frames", self.frames)
            .num("frame_mean_ms", frame_mean)
            .raw("parts", &parts)
            .num("copies_mb_per_frame", self.copy_bytes as f64 / 1e6 / n)
            .opt_num("copies_mb_per_s", self.rate(self.copy_bytes, Phase::Copies))
            .num(
                "copy_back_mb_per_frame",
                self.copy_back_bytes as f64 / 1e6 / n,
            )
            .opt_num(
                "copy_back_mb_per_s",
                self.rate(self.copy_back_bytes, Phase::CopyBack),
            )
            .num("watch_calls_per_frame", self.watch_calls as f64 / n)
            .num("watch_pages_per_frame", self.watch_pages as f64 / n)
            .opt_num(
                "watch_us_per_call",
                (self.watch_calls > 0)
                    .then(|| self.ms[Phase::WriteWatch as usize] * 1e3 / self.watch_calls as f64),
            )
            .finish()
    }
}

/// The world's frames by part, and what its long frames were made of.
#[derive(Default)]
pub struct PhaseStats {
    world: Scope,
    stable: Scope,
    /// Per cause (a phase, paging, none found): long frames and their ms.
    long: [(u64, f64); PHASES + 2],
}

impl PhaseStats {
    /// A world frame of `frame_ms` with its parts. `stable`: it is of the
    /// stable scene; `long`: it counts as a long frame; `paging`: the
    /// memory sampler saw a burst of page faults around it.
    pub fn frame(
        &mut self,
        frame_ms: f64,
        stable: bool,
        long: bool,
        paging: bool,
        parts: &FrameParts,
    ) {
        self.world.add(frame_ms, parts);
        if stable {
            self.stable.add(frame_ms, parts);
        }
        if long {
            let (phase, us) = BLAMED
                .iter()
                .map(|p| (*p as usize, parts.us[*p as usize]))
                .max_by_key(|(_, us)| *us)
                .unwrap_or((CAUSE_UNEXPLAINED, 0));
            let cause = if us as f64 / 1e3 >= frame_ms * BLAME_SHARE {
                phase
            } else if paging {
                CAUSE_FAULTS
            } else {
                CAUSE_UNEXPLAINED
            };
            self.long[cause].0 += 1;
            self.long[cause].1 += frame_ms;
        }
    }

    /// Whether any frame came with its parts.
    pub fn any(&self) -> bool {
        self.world.frames > 0
    }

    /// The scope the summary shows: the stable scene's once it has frames.
    fn shown(&self) -> (&Scope, &'static str) {
        if self.stable.frames > 0 {
            (&self.stable, "stable")
        } else {
            (&self.world, "world")
        }
    }

    fn cause_name(i: usize) -> &'static str {
        match i {
            CAUSE_FAULTS => "page_faults",
            CAUSE_UNEXPLAINED => "unexplained",
            i => PHASE_NAMES[i],
        }
    }

    /// The causes of long frames, the most frames first.
    fn long_causes(&self) -> Vec<(&'static str, u64, f64)> {
        let mut v: Vec<_> = self
            .long
            .iter()
            .enumerate()
            .filter(|(_, (n, _))| *n > 0)
            .map(|(i, (n, ms))| (Self::cause_name(i), *n, *ms))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        v
    }

    /// `phases` of `resumo.json`: the stable scene's (the world's before
    /// it), or `null` without frames.
    pub fn to_json(&self) -> String {
        let (scope, name) = self.shown();
        scope.to_json(name)
    }

    /// `phases_world` of `resumo.json`.
    pub fn world_json(&self) -> String {
        self.world.to_json("world")
    }

    /// `long_frame_causes` of `resumo.json`.
    pub fn long_json(&self) -> String {
        json::array(self.long_causes().into_iter().map(|(cause, n, ms)| {
            Obj::new()
                .str("cause", cause)
                .int("frames", n)
                .num("ms", ms)
                .finish()
        }))
    }

    /// The section of `resumo.md`, empty without frames.
    pub fn to_markdown(&self) -> String {
        let (scope, name) = self.shown();
        let mut s = String::new();
        if scope.frames == 0 {
            return s;
        }
        let _ = writeln!(
            s,
            "\n## Fases do quadro ({} quadros {}; as fases se sobrepõem e não somam o quadro)\n",
            scope.frames,
            if name == "stable" {
                "da cena estável"
            } else {
                "do mundo"
            }
        );
        let _ = writeln!(s, "| fase | média ms | p50 | p95 | p99 | % do quadro |");
        let _ = writeln!(s, "|---|---|---|---|---|---|");
        let frame_mean = scope.frame_ms / scope.frames as f64;
        let mut order: Vec<usize> = (0..PHASES).collect();
        order.sort_by(|a, b| scope.mean(*b).total_cmp(&scope.mean(*a)));
        let f = |v: Option<f64>| v.map_or("—".to_owned(), |v| format!("{v:.1}"));
        for i in order {
            if scope.mean(i) < 0.05 {
                continue;
            }
            let h = &scope.hist[i];
            let _ = writeln!(
                s,
                "| {} | {:.1} | {} | {} | {} | {:.0} |",
                PHASE_NAMES[i],
                scope.mean(i),
                f(h.percentile(0.50)),
                f(h.percentile(0.95)),
                f(h.percentile(0.99)),
                scope.mean(i) / frame_mean.max(1e-9) * 100.0
            );
        }
        let rate = |v: Option<f64>| v.map_or("—".to_owned(), |v| format!("{v:.0} MB/s"));
        let n = scope.frames as f64;
        let _ = writeln!(
            s,
            "\nCópias: {:.1} MB/quadro a {}; cópia de volta: {:.1} MB/quadro a {}; write watch: {:.0} consultas/quadro sobre {:.0} páginas.",
            scope.copy_bytes as f64 / 1e6 / n,
            rate(scope.rate(scope.copy_bytes, Phase::Copies)),
            scope.copy_back_bytes as f64 / 1e6 / n,
            rate(scope.rate(scope.copy_back_bytes, Phase::CopyBack)),
            scope.watch_calls as f64 / n,
            scope.watch_pages as f64 / n
        );
        let long = self.long_causes();
        if !long.is_empty() {
            let text: Vec<String> = long
                .iter()
                .map(|(cause, n, ms)| format!("{cause} {n} ({:.1} s)", ms / 1e3))
                .collect();
            let _ = writeln!(
                s,
                "\nQuadros de mais de 1 s por causa (a maior fase, se passa de 25 % do quadro): {}.",
                text.join(", ")
            );
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(preparing_ms: u64, gpu_ms: u64, remap_ms: u64) -> FrameParts {
        let mut p = FrameParts::default();
        p.us[Phase::Preparing as usize] = preparing_ms * 1000;
        p.us[Phase::Copies as usize] = preparing_ms * 500;
        p.us[Phase::Gpu as usize] = gpu_ms * 1000;
        p.us[Phase::Remap as usize] = remap_ms * 1000;
        p.copy_bytes = 50_000_000;
        p.watch_calls = 100;
        p.watch_pages = 4000;
        p
    }

    #[test]
    fn percentiles_and_shares_are_per_phase() {
        let mut s = PhaseStats::default();
        for i in 0..100 {
            // One frame in ten spends 80 ms preparing instead of 20.
            let prep = if i % 10 == 9 { 80 } else { 20 };
            s.frame(100.0, true, false, false, &parts(prep, 40, 0));
        }
        let json = s.to_json();
        assert!(
            json.starts_with(r#"{"scope":"stable","frames":100,"frame_mean_ms":100,"#),
            "{json}"
        );
        assert!(
            json.contains(
                r#"{"name":"preparing","mean_ms":26,"p50_ms":20.05,"p95_ms":80,"p99_ms":80,"max_ms":80,"share_pct":26}"#
            ),
            "{json}"
        );
        // 50 MB in 10 ms (13 ms in the slow frames) of copying.
        assert!(json.contains(r#""copies_mb_per_frame":50,"#), "{json}");
        assert!(json.contains(r#""watch_calls_per_frame":100,"#), "{json}");
        let md = s.to_markdown();
        assert!(
            md.contains("| gpu | 40.0 | 40.0 | 40.0 | 40.0 | 40 |"),
            "{md}"
        );
        // Phases that never ran are left out of the table.
        assert!(!md.contains("| disk |"), "{md}");
    }

    #[test]
    fn a_long_frame_is_blamed_on_its_largest_part_or_on_paging() {
        let mut s = PhaseStats::default();
        s.frame(1500.0, false, true, false, &parts(100, 200, 1200));
        s.frame(1200.0, false, true, false, &parts(100, 900, 10));
        s.frame(1300.0, false, true, false, &parts(90, 800, 10));
        s.frame(2000.0, false, true, true, &parts(50, 60, 10));
        s.frame(2500.0, false, true, false, &parts(50, 60, 10));
        assert_eq!(
            s.long_json(),
            r#"[{"cause":"gpu","frames":2,"ms":2500},{"cause":"page_faults","frames":1,"ms":2000},{"cause":"remap","frames":1,"ms":1500},{"cause":"unexplained","frames":1,"ms":2500}]"#
        );
        // No stable frame yet: the world's are what is shown.
        assert!(s.to_json().starts_with(r#"{"scope":"world","frames":5,"#));
    }

    #[test]
    fn nothing_is_written_without_frames() {
        let s = PhaseStats::default();
        assert!(!s.any());
        assert_eq!(s.to_json(), "null");
        assert_eq!(s.to_markdown(), "");
        assert_eq!(s.long_json(), "[]");
    }

    /// The cost of a frame's sample: far below a thousandth of the
    /// shortest frame (16 ms), so the phases are always on.
    #[test]
    fn a_frame_costs_microseconds() {
        let mut s = PhaseStats::default();
        let p = parts(20, 40, 5);
        let n = 200_000u32;
        let began = std::time::Instant::now();
        for i in 0..n {
            s.frame(100.0 + f64::from(i % 7), i % 2 == 0, false, false, &p);
        }
        let per_frame_us = began.elapsed().as_secs_f64() * 1e6 / f64::from(n);
        println!("phase sample: {per_frame_us:.3} us a frame");
        assert!(per_frame_us < 16.0, "{per_frame_us} us a frame");
    }
}

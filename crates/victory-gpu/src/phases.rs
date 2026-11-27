//! The presenter's reading of what each frame was made of, for the
//! session log (`victory_core::session::phases`).
//!
//! The frame log's totals run from the start in every build; at each
//! present their differences since the present before are that frame's
//! parts. Sixteen atomic loads a frame, on the thread that presents:
//! nothing is locked and nothing is written here. `SPARK_PHASES=0` leaves
//! the frames without parts and the write watch untimed (`crate::watch`).
//!
//! Work of the next frame is already under way on other threads when a
//! frame is presented, so a part's time lands in the frame it ended in;
//! over the frames of a scene the shares are right.

use victory_core::session::phases::{FrameParts, PHASES, Phase};

use crate::framelog::{T, total};

/// The total each phase is the difference of.
const SOURCES: [(Phase, T); PHASES] = [
    (Phase::Preparing, T::PrepareUs),
    (Phase::Copies, T::CopyUs),
    (Phase::WriteWatch, T::WatchUs),
    (Phase::Textures, T::RefreshUs),
    (Phase::Gpu, T::CardWaitUs),
    (Phase::CopyBack, T::CopyBackUs),
    (Phase::Present, T::PresentUs),
    (Phase::Remap, T::RemapUs),
    (Phase::PipelineWait, T::PipelineWaitUs),
    (Phase::CardIdle, T::CardIdleUs),
    (Phase::Disk, T::DiskUs),
    (Phase::Pace, T::PaceUs),
];

/// The totals at one moment, laid out as a frame's parts.
fn read() -> FrameParts {
    let mut now = FrameParts::default();
    for (phase, source) in SOURCES {
        now.us[phase as usize] = total(source);
    }
    now.copy_bytes = total(T::CopyBytes);
    now.copy_back_bytes = total(T::CopyBackBytes);
    now.watch_calls = total(T::WatchCalls);
    now.watch_pages = total(T::WatchPages);
    now
}

/// `now` less `then`, part by part.
fn since(now: &FrameParts, then: &FrameParts) -> FrameParts {
    FrameParts {
        us: std::array::from_fn(|i| now.us[i].wrapping_sub(then.us[i])),
        copy_bytes: now.copy_bytes.wrapping_sub(then.copy_bytes),
        copy_back_bytes: now.copy_back_bytes.wrapping_sub(then.copy_back_bytes),
        watch_calls: now.watch_calls.wrapping_sub(then.watch_calls),
        watch_pages: now.watch_pages.wrapping_sub(then.watch_pages),
    }
}

/// Whether frames are read part by part (`SPARK_PHASES=0`: no). A switch
/// of the table (`crate::switches`): it may alternate inside a run.
pub(crate) fn on() -> bool {
    crate::switches::PHASES.on()
}

/// `sum` with `parts` added, part by part.
fn add(sum: &mut FrameParts, parts: &FrameParts) {
    for (total, us) in sum.us.iter_mut().zip(parts.us) {
        *total += us;
    }
    sum.copy_bytes += parts.copy_bytes;
    sum.copy_back_bytes += parts.copy_back_bytes;
    sum.watch_calls += parts.watch_calls;
    sum.watch_pages += parts.watch_pages;
}

/// The totals at the last present, and the frames read since the last
/// frame-rate line.
pub(crate) struct Reader {
    /// `None` while the phases are off: the first frame after they come
    /// on has no totals to be the difference of.
    last: Option<FrameParts>,
    /// The parts of the frames read since the last line, summed.
    sum: FrameParts,
    frames: u32,
}

impl Reader {
    pub(crate) fn new() -> Reader {
        Reader {
            last: on().then(read),
            sum: FrameParts::default(),
            frames: 0,
        }
    }

    /// The parts of the frame that ends now; `None` with the phases off
    /// and for the first frame after they come on.
    pub(crate) fn frame(&mut self) -> Option<FrameParts> {
        if !on() {
            self.last = None;
            return None;
        }
        let now = read();
        let parts = since(&now, &self.last.replace(now)?);
        add(&mut self.sum, &parts);
        self.frames += 1;
        Some(parts)
    }

    /// The `frame_phases` event of `eventos.jsonl`: each part's mean over
    /// the frames read since the last call, in ms a frame. Nothing without
    /// a frame read.
    pub(crate) fn line(&mut self) {
        let frames = f64::from(std::mem::take(&mut self.frames));
        let parts = std::mem::take(&mut self.sum);
        if frames <= 0.0 {
            return;
        }
        let ms = |p: Phase| parts.us[p as usize] as f64 / 1e3 / frames;
        tracing::info!(
            spark.kind = "frame_phases",
            spark.frames = frames,
            spark.preparing_ms = ms(Phase::Preparing),
            spark.copies_ms = ms(Phase::Copies),
            spark.write_watch_ms = ms(Phase::WriteWatch),
            spark.textures_ms = ms(Phase::Textures),
            spark.gpu_ms = ms(Phase::Gpu),
            spark.copy_back_ms = ms(Phase::CopyBack),
            spark.present_ms = ms(Phase::Present),
            spark.remap_ms = ms(Phase::Remap),
            spark.pipeline_wait_ms = ms(Phase::PipelineWait),
            spark.card_idle_ms = ms(Phase::CardIdle),
            spark.disk_ms = ms(Phase::Disk),
            spark.pace_ms = ms(Phase::Pace),
            spark.copies_mb = parts.copy_bytes as f64 / 1e6 / frames,
            spark.copy_back_mb = parts.copy_back_bytes as f64 / 1e6 / frames,
            spark.watch_calls = parts.watch_calls as f64 / frames,
            spark.watch_pages = parts.watch_pages as f64 / frames,
            "frame phases"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use victory_core::session::phases::PHASE_NAMES;

    #[test]
    fn every_phase_has_a_source_at_its_own_index() {
        for (i, (phase, _)) in SOURCES.iter().enumerate() {
            assert_eq!(*phase as usize, i, "{}", PHASE_NAMES[i]);
        }
    }

    #[test]
    fn a_frame_is_the_difference_of_the_totals() {
        let mut reader = Reader::new();
        if !on() {
            assert!(reader.frame().is_none());
            return;
        }
        crate::framelog::add(T::RemapUs, 1500);
        crate::framelog::add(T::CopyBytes, 4096);
        let Some(parts) = reader.frame() else {
            panic!("a frame read with the phases on has parts");
        };
        // Other tests add to the same totals: at least what was added here.
        assert!(parts.us[Phase::Remap as usize] >= 1500);
        assert!(parts.copy_bytes >= 4096);
        // The line's sum is of the frames read, not of the totals.
        assert_eq!(reader.frames, 1);
        assert_eq!(reader.sum.copy_bytes, parts.copy_bytes);
        reader.line();
        assert_eq!((reader.frames, reader.sum.copy_bytes), (0, 0));
    }
}

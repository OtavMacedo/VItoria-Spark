//! The run's summary (`resumo.json`, `resumo.md`): what the session's
//! events add up to, short enough to give an assistant whole.
//!
//! Everything here is plain data fed with the time (seconds since the
//! session started) by its caller, so tests drive it without a run.

use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use crate::ab::Tag;

use super::ab::AbStats;
use super::drops::{DROP_CAUSES, DROP_NAMES, DropTally};
use super::json::{self, Obj};
use super::phases::{FrameParts, PhaseStats};

/// Host submissions between two presents from which a frame is one of the
/// world (the title's menus and loading screens send two or three; the
/// frame log's `WORLD_SUBMISSIONS`).
pub const WORLD_SUBMISSIONS: u64 = 6;
/// World frames in a row that mark the world's start.
const WORLD_STREAK: u32 = 10;
/// A frame longer than this (ms) is a long one.
const LONG_MS: f64 = 1000.0;
/// Distinct keys kept per table; more are counted together.
const MAX_KEYS: usize = 512;
/// Frame-rate samples kept (one each five seconds: a day).
const MAX_SAMPLES: usize = 17_280;
/// Rows in each top list.
const TOP: usize = 10;
/// Shaders that do not translate listed by hash; more are only counted.
const MAX_UNTRANSLATED: usize = 64;
/// Page faults a second from which a long frame with no part to blame is
/// blamed on paging.
const PAGING_FAULTS_PER_S: f64 = 5000.0;
/// Draws a frame from which the scene is the stable one measured (the
/// world drawn whole, not a loading or half-loaded scene), unless
/// `SPARK_STABLE_DRAWS` says another.
pub const STABLE_DRAWS: f64 = 4500.0;
/// Seconds after the world's first frame before the scene counts as
/// stable (streaming catches up), unless `SPARK_STABLE_AFTER_S` says
/// another.
pub const STABLE_AFTER_S: f64 = 60.0;

/// The level of an event, most severe first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    pub fn name(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        }
    }
}

/// A field's value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    I64(i64),
    U64(u64),
    F64(f64),
    Bool(bool),
}

impl Value {
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Value::I64(v) => Some(v as f64),
            Value::U64(v) => Some(v as f64),
            Value::F64(v) => Some(v),
            Value::Str(ref s) => s.parse().ok(),
            Value::Bool(_) => None,
        }
    }

    pub fn text(&self) -> String {
        match self {
            Value::Str(s) => s.clone(),
            Value::I64(v) => v.to_string(),
            Value::U64(v) => v.to_string(),
            Value::F64(v) => json::number(*v),
            Value::Bool(v) => v.to_string(),
        }
    }

    pub fn json(&self) -> String {
        match self {
            Value::Str(s) => json::string(s),
            Value::I64(v) => v.to_string(),
            Value::U64(v) => v.to_string(),
            Value::F64(v) => json::number(*v),
            Value::Bool(v) => v.to_string(),
        }
    }
}

/// One event as the summary sees it.
pub struct EventView<'a> {
    pub level: Level,
    pub target: &'a str,
    pub message: &'a str,
    pub fields: &'a [(&'static str, Value)],
}

impl EventView<'_> {
    pub fn field(&self, name: &str) -> Option<&Value> {
        self.fields.iter().find(|(n, _)| *n == name).map(|(_, v)| v)
    }

    fn num(&self, name: &str) -> Option<f64> {
        self.field(name).and_then(Value::as_f64)
    }

    fn text(&self, name: &str) -> Option<String> {
        self.field(name).map(Value::text)
    }
}

/// Frame lengths, in buckets: tenths of a millisecond up to 100 ms, whole
/// milliseconds up to a second, tens of milliseconds up to ten seconds.
#[derive(Default)]
pub struct Histogram {
    counts: Vec<u64>,
    n: u64,
    max_ms: f64,
}

impl Histogram {
    const LEN: usize = 1000 + 900 + 900 + 1;

    fn bucket(ms: f64) -> usize {
        let ms = ms.max(0.0);
        if ms < 100.0 {
            (ms * 10.0) as usize
        } else if ms < 1000.0 {
            1000 + (ms - 100.0) as usize
        } else if ms < 10_000.0 {
            1900 + ((ms - 1000.0) / 10.0) as usize
        } else {
            Self::LEN - 1
        }
        .min(Self::LEN - 1)
    }

    /// The middle of a bucket.
    fn middle(&self, b: usize) -> f64 {
        if b < 1000 {
            (b as f64 + 0.5) / 10.0
        } else if b < 1900 {
            100.0 + (b - 1000) as f64 + 0.5
        } else if b < 2800 {
            1000.0 + (b - 1900) as f64 * 10.0 + 5.0
        } else {
            self.max_ms
        }
    }

    pub fn add(&mut self, ms: f64) {
        if !ms.is_finite() {
            return;
        }
        if self.counts.is_empty() {
            self.counts = vec![0; Self::LEN];
        }
        self.counts[Self::bucket(ms)] += 1;
        self.n += 1;
        self.max_ms = self.max_ms.max(ms);
    }

    pub fn count(&self) -> u64 {
        self.n
    }

    /// The length below which `p` (0 to 1) of the frames lie.
    pub fn percentile(&self, p: f64) -> Option<f64> {
        if self.n == 0 {
            return None;
        }
        let rank = ((p * self.n as f64).ceil() as u64).clamp(1, self.n);
        let mut seen = 0;
        for (b, &c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= rank {
                return Some(self.middle(b).min(self.max_ms));
            }
        }
        Some(self.max_ms)
    }

    pub fn max(&self) -> Option<f64> {
        (self.n > 0).then_some(self.max_ms)
    }
}

/// One frame-rate line's numbers (per frame, averaged over its second).
#[derive(Clone, Copy, Default, Debug)]
pub struct Rate {
    pub fps: f64,
    pub ms: f64,
    pub preparing: f64,
    pub copies: f64,
    pub textures: f64,
    pub gpu: f64,
    pub copy_back: f64,
    pub draws: f64,
    pub dropped: f64,
    pub world: bool,
    /// Megabytes a frame: copied for submissions, uploaded as textures,
    /// copied back.
    pub copies_mb: f64,
    pub textures_mb: f64,
    pub copy_back_mb: f64,
    /// Host pipelines queued and not made when the line was written.
    pub making: f64,
}

/// Counts by key, with a detail kept from the first time and how many
/// distinct "others" (shader hashes, say) came with it.
#[derive(Default)]
struct Tally {
    rows: HashMap<String, Row>,
    /// Counted under keys past [`MAX_KEYS`].
    overflow: u64,
}

#[derive(Default)]
struct Row {
    count: u64,
    first_s: f64,
    level: Option<Level>,
    target: String,
    detail: String,
    others: HashSet<String>,
}

impl Tally {
    fn add(&mut self, key: &str, now_s: f64, fill: impl FnOnce(&mut Row)) -> Option<&mut Row> {
        if !self.rows.contains_key(key) {
            if self.rows.len() >= MAX_KEYS {
                self.overflow += 1;
                return None;
            }
            let mut row = Row {
                first_s: now_s,
                ..Row::default()
            };
            fill(&mut row);
            self.rows.insert(key.to_owned(), row);
        }
        let row = self.rows.get_mut(key)?;
        row.count += 1;
        Some(row)
    }

    fn total(&self) -> u64 {
        self.rows.values().map(|r| r.count).sum::<u64>() + self.overflow
    }

    /// The most counted first (then the most severe, then by key).
    fn top(&self, n: usize) -> Vec<(&String, &Row)> {
        let mut v: Vec<_> = self.rows.iter().collect();
        v.sort_by(|a, b| {
            b.1.count
                .cmp(&a.1.count)
                .then(a.1.level.cmp(&b.1.level))
                .then(a.0.cmp(b.0))
        });
        v.truncate(n);
        v
    }
}

/// A crash: an exception nobody handled, a panic, or a failure that ends
/// the run (the host GPU's).
#[derive(Clone, Debug, Default)]
pub struct Crash {
    /// What it was ("unhandled exception", "panic", "host GPU failure").
    pub what: String,
    /// The exception code, if an exception.
    pub code: Option<u32>,
    /// `module+offset` of the faulting instruction, if known.
    pub location: Option<String>,
    /// More about it (the access, the message), on one line.
    pub detail: String,
    /// The faulting thread's frames, `module+offset` each.
    pub stack: Vec<String>,
}

#[derive(Default)]
pub struct Summary {
    frames: Histogram,
    world: Histogram,
    /// The last present: when (s) and the submissions total then.
    last_present: Option<(f64, u64)>,
    long_frames: u64,
    long_world_frames: u64,
    /// Page faults a second at the time of the longest-faulting long frame.
    long_frame_faults: f64,
    first_world_s: Option<f64>,
    streak: u32,
    streak_start: f64,
    window_world: u32,
    window_frames: u32,
    rates: Vec<Rate>,
    refused: Tally,
    untranslated: Tally,
    device_lost: Vec<(f64, String)>,
    crash: Option<(f64, Crash)>,
    ram_total_mb: Option<u64>,
    ram_free_min_mb: Option<u64>,
    faults: Option<(f64, u64)>,
    faults_first: Option<(f64, u64)>,
    fault_rate: f64,
    fault_rate_max: f64,
    vram_used_max_mb: Option<f64>,
    vram_budget_min_mb: Option<f64>,
    shared_used_max_mb: Option<f64>,
    warnings: Tally,
    adapter: Vec<(String, String)>,
    pipelines_making_max: f64,
    pub dropped_events: u64,
    pub suppressed_events: u64,
    end: Option<String>,
    now_s: f64,
    /// Per experiment of a run whose switches alternate (`SPARK_AB`, one;
    /// `SPARK_PLAN`, several): its name and its two arms.
    ab: Vec<(String, AbStats)>,
    /// The experiment of the last frame tagged: the meter's next window is
    /// its.
    ab_last: usize,
    /// What makes the scene the stable one: draws a frame and seconds
    /// after the world's start (`None`: [`STABLE_DRAWS`], [`STABLE_AFTER_S`]).
    stable_rule: Option<(f64, f64)>,
    /// When the scene became stable: the first one-second window of the
    /// meter that met the rule.
    stable_from_s: Option<f64>,
    /// World frames presented since the scene became stable.
    stable: Histogram,
    /// The meter's one-second windows of the stable scene that met the
    /// rule's draws.
    stable_windows: Vec<Rate>,
    /// Of them, those with host pipelines still being made: frames of a
    /// scene whose caches were not warm.
    stable_making_windows: u64,
    /// The world's frames by part.
    phases: PhaseStats,
    /// Dropped draws by cause: the `draws_dropped` event waiting for its
    /// frame-rate line, and the lines of the world.
    pending_drops: Option<[f32; DROP_CAUSES]>,
    drops: DropTally,
    /// Shaders that do not translate: hash, stage and what stopped them.
    untranslated_shaders: Vec<(String, String, String)>,
    /// The card driver's shader cache when the run started: megabytes
    /// and files.
    driver_cache: Option<(f64, f64)>,
}

/// What a message says with its numbers taken out, so that one warning
/// with a different count each time is one row.
fn normalise(message: &str) -> String {
    let mut out = String::with_capacity(message.len().min(200));
    let mut chars = message.chars().peekable();
    // Digits inside a word (`v_permlane16_b32`) are part of a name.
    let mut prev = ' ';
    while let Some(c) = chars.next() {
        let in_word = prev.is_alphanumeric() || prev == '_';
        prev = c;
        if in_word {
            out.push(c);
        } else if c == '0' && chars.peek() == Some(&'x') {
            chars.next();
            while chars.peek().is_some_and(char::is_ascii_hexdigit) {
                chars.next();
            }
            out.push_str("0x#");
        } else if c.is_ascii_digit() {
            while chars
                .peek()
                .is_some_and(|c| c.is_ascii_digit() || *c == '.')
            {
                chars.next();
            }
            out.push('#');
        } else {
            out.push(c);
        }
        if out.len() >= 200 {
            out.push('…');
            break;
        }
    }
    out
}

fn short(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        let mut t: String = s.chars().take(n).collect();
        t.push('…');
        t
    }
}

/// For a Markdown table cell.
fn cell(s: &str, n: usize) -> String {
    short(s, n).replace('|', "\\|").replace('\n', " ")
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[v.len() / 2])
}

fn fmt_ms(v: Option<f64>) -> String {
    v.map_or("—".to_owned(), |v| format!("{v:.1}"))
}

fn fmt_s(s: f64) -> String {
    if s >= 3600.0 {
        format!(
            "{}h{:02}m{:02}s",
            (s / 3600.0) as u64,
            (s % 3600.0 / 60.0) as u64,
            (s % 60.0) as u64
        )
    } else if s >= 60.0 {
        format!("{}m{:02}s", (s / 60.0) as u64, (s % 60.0) as u64)
    } else {
        format!("{s:.1} s")
    }
}

/// The medians of `rates`; `world` says whether they are the world's.
fn medians_of(world: bool, rates: &[&Rate]) -> Medians {
    let m = |f: fn(&Rate) -> f64| median(rates.iter().map(|r| f(r)).collect());
    Medians {
        world,
        samples: rates.len(),
        fps: m(|r| r.fps),
        ms: m(|r| r.ms),
        preparing: m(|r| r.preparing),
        copies: m(|r| r.copies),
        textures: m(|r| r.textures),
        gpu: m(|r| r.gpu),
        copy_back: m(|r| r.copy_back),
        draws: m(|r| r.draws),
        dropped: m(|r| r.dropped),
        copies_mb: m(|r| r.copies_mb),
        textures_mb: m(|r| r.textures_mb),
        copy_back_mb: m(|r| r.copy_back_mb),
    }
}

/// Medians of the frame-rate samples of the world (all of them if none is).
#[derive(Default, Debug, Clone, Copy)]
pub struct Medians {
    pub world: bool,
    pub samples: usize,
    pub fps: Option<f64>,
    pub ms: Option<f64>,
    pub preparing: Option<f64>,
    pub copies: Option<f64>,
    pub textures: Option<f64>,
    pub gpu: Option<f64>,
    pub copy_back: Option<f64>,
    pub draws: Option<f64>,
    pub dropped: Option<f64>,
    pub copies_mb: Option<f64>,
    pub textures_mb: Option<f64>,
    pub copy_back_mb: Option<f64>,
}

impl Summary {
    pub fn new() -> Self {
        Self::default()
    }

    fn tick(&mut self, now_s: f64) {
        self.now_s = self.now_s.max(now_s);
    }

    /// The run is still going at `now_s` (its duration so far).
    pub fn touch(&mut self, now_s: f64) {
        self.tick(now_s);
    }

    /// A frame was presented; `submissions` is the run's total of host
    /// submissions so far.
    pub fn present(&mut self, now_s: f64, submissions: u64) {
        self.present_tagged(now_s, submissions, None);
    }

    /// [`Summary::present`] for a run whose switches alternate: `tag` is
    /// what the frame that ended ran under (`crate::ab`).
    pub fn present_tagged(&mut self, now_s: f64, submissions: u64, tag: Option<Tag>) {
        self.present_parts(now_s, submissions, tag, None);
    }

    /// [`Summary::present_tagged`] with what the frame that ended was made
    /// of, when the presenter measured it.
    pub fn present_parts(
        &mut self,
        now_s: f64,
        submissions: u64,
        tag: Option<Tag>,
        parts: Option<&FrameParts>,
    ) {
        self.tick(now_s);
        let Some((then, before)) = self.last_present.replace((now_s, submissions)) else {
            return;
        };
        let ms = (now_s - then) * 1000.0;
        let world = submissions.saturating_sub(before) >= WORLD_SUBMISSIONS;
        self.frame(then, ms, world);
        let stable = world && self.stable_from_s.is_some();
        if stable {
            self.stable.add(ms);
        }
        if let Some(parts) = parts
            && world
            && self.first_world_s.is_some()
        {
            let paging = self.fault_rate > PAGING_FAULTS_PER_S;
            self.phases.frame(ms, stable, ms > LONG_MS, paging, parts);
        }
        if let Some(tag) = tag {
            self.ab_last = tag.exp;
            if let Some((_, ab)) = self.ab.get_mut(tag.exp) {
                ab.frame(tag, ms, stable);
            }
        }
    }

    /// The run alternates switches: what each arm sets (`NAME=value ...`)
    /// and a block's length in seconds. Before the first frame.
    pub fn ab_plan(
        &mut self,
        name: &str,
        labels: [String; 2],
        factors: Vec<String>,
        period_s: f64,
    ) {
        self.ab.push((
            name.to_owned(),
            AbStats::new(labels, period_s).with_factors(factors),
        ));
    }

    /// What makes the scene stable: `draws` a frame or more, from
    /// `after_s` seconds after the world's first frame on. Before the
    /// first frame.
    pub fn stable_rule(&mut self, draws: f64, after_s: f64) {
        self.stable_rule = Some((draws.max(0.0), after_s.max(0.0)));
    }

    fn rule(&self) -> (f64, f64) {
        self.stable_rule.unwrap_or((STABLE_DRAWS, STABLE_AFTER_S))
    }

    /// The frame meter's numbers for the frames presented since its last
    /// call (about a second of them), at `now_s`: they mark when the scene
    /// becomes stable, and the stable scene's and the arms' frame parts
    /// come from them.
    pub fn window(&mut self, now_s: f64, rate: Rate) {
        self.tick(now_s);
        let (draws, after_s) = self.rule();
        if self.stable_from_s.is_none()
            && rate.draws >= draws
            && self
                .first_world_s
                .is_some_and(|world_s| now_s - world_s >= after_s)
        {
            self.stable_from_s = Some(now_s);
        }
        if self.stable_from_s.is_some()
            && rate.draws >= draws
            && self.stable_windows.len() < MAX_SAMPLES
        {
            self.stable_windows.push(rate);
            self.stable_making_windows += u64::from(rate.making > 0.0);
        }
        if let Some((_, ab)) = self.ab.get_mut(self.ab_last) {
            ab.window(rate);
        }
    }

    /// One frame of `ms` that started at `start_s`.
    pub fn frame(&mut self, start_s: f64, ms: f64, world: bool) {
        self.frames.add(ms);
        self.window_frames += 1;
        if world {
            self.world.add(ms);
            self.window_world += 1;
            if self.streak == 0 {
                self.streak_start = start_s;
            }
            self.streak += 1;
            if self.streak >= WORLD_STREAK && self.first_world_s.is_none() {
                self.first_world_s = Some(self.streak_start);
            }
        } else {
            self.streak = 0;
        }
        if ms > LONG_MS {
            self.long_frames += 1;
            if self.first_world_s.is_some() {
                self.long_world_frames += 1;
            }
            self.long_frame_faults = self.long_frame_faults.max(self.fault_rate);
        }
    }

    /// The system's memory, sampled about once a second.
    pub fn memory(&mut self, now_s: f64, total_mb: u64, free_mb: u64, faults: u64) {
        self.tick(now_s);
        self.ram_total_mb = Some(total_mb);
        self.ram_free_min_mb = Some(self.ram_free_min_mb.map_or(free_mb, |m| m.min(free_mb)));
        if let Some((then, before)) = self.faults.replace((now_s, faults))
            && now_s > then
        {
            self.fault_rate = faults.saturating_sub(before) as f64 / (now_s - then);
            self.fault_rate_max = self.fault_rate_max.max(self.fault_rate);
        }
        self.faults_first.get_or_insert((now_s, faults));
    }

    /// The mean page faults a second over the run.
    fn fault_rate_mean(&self) -> Option<f64> {
        let ((t0, f0), (t1, f1)) = (self.faults_first?, self.faults?);
        (t1 > t0).then(|| f1.saturating_sub(f0) as f64 / (t1 - t0))
    }

    pub fn crash(&mut self, now_s: f64, crash: Crash) {
        self.tick(now_s);
        // The first is the cause; what follows is often its consequence.
        if self.crash.is_none() {
            self.crash = Some((now_s, crash));
        }
    }

    pub fn end(&mut self, now_s: f64, reason: &str) {
        self.tick(now_s);
        self.end.get_or_insert_with(|| reason.to_owned());
    }

    /// An event at warning level or above, or one marked `spark.kind`.
    pub fn event(&mut self, now_s: f64, e: &EventView) {
        self.tick(now_s);
        match e.field("spark.kind").map(Value::text).as_deref() {
            Some("frame_rate") => self.rate(e),
            Some("pipeline_refused") => {
                let key = e
                    .text("shaders")
                    .or_else(|| e.text("hash"))
                    .unwrap_or_default();
                let mut why = e.text("error").map_or_else(
                    || e.message.to_owned(),
                    |err| format!("{}: {err}", e.message),
                );
                // The cause the host found in the program, when it found one.
                if let Some(cause) = e.text("cause").filter(|c| !c.is_empty()) {
                    why = format!("{why} ({cause})");
                }
                self.refused.add(&key, now_s, |r| r.detail = why);
            }
            Some("shader_untranslated") => {
                // Offsets and counts out, so one instruction is one row.
                let what = normalise(
                    &e.text("spark.instruction")
                        .or_else(|| e.text("error"))
                        .unwrap_or_else(|| "?".into()),
                );
                let hash = e.text("hash").unwrap_or_default();
                let stage = e.text("stage").unwrap_or_default();
                if self.untranslated_shaders.len() < MAX_UNTRANSLATED
                    && !self.untranslated_shaders.iter().any(|(h, _, _)| *h == hash)
                {
                    self.untranslated_shaders
                        .push((hash.clone(), stage.clone(), what.clone()));
                }
                if let Some(row) = self.untranslated.add(&what, now_s, |r| {
                    r.detail = e.text("error").unwrap_or_default();
                }) && row.others.len() < 4096
                {
                    row.others.insert(format!("{hash}.{stage}"));
                }
            }
            Some("device_lost") => {
                if self.device_lost.len() < 8 {
                    let why = e
                        .text("spark.reason")
                        .unwrap_or_else(|| e.message.to_owned());
                    self.device_lost.push((now_s, short(&why, 600)));
                }
            }
            Some("video_memory") => {
                let max = |slot: &mut Option<f64>, v: Option<f64>| {
                    if let Some(v) = v {
                        *slot = Some(slot.map_or(v, |s: f64| s.max(v)));
                    }
                };
                max(&mut self.vram_used_max_mb, e.num("spark.vram_used_mb"));
                max(&mut self.shared_used_max_mb, e.num("spark.shared_used_mb"));
                if let Some(b) = e.num("spark.vram_budget_mb").filter(|b| *b > 0.0) {
                    self.vram_budget_min_mb =
                        Some(self.vram_budget_min_mb.map_or(b, |m: f64| m.min(b)));
                }
            }
            Some("draws_dropped") => {
                self.pending_drops = Some(std::array::from_fn(|i| {
                    e.num(&format!("spark.{}", DROP_NAMES[i])).unwrap_or(0.0) as f32
                }));
            }
            Some("driver_cache") => {
                self.driver_cache = Some((
                    e.num("spark.mb").unwrap_or(0.0),
                    e.num("spark.files").unwrap_or(0.0),
                ));
            }
            Some("adapter") => {
                self.adapter = e
                    .fields
                    .iter()
                    .filter_map(|(n, v)| {
                        let n = n.strip_prefix("spark.")?;
                        (n != "kind").then(|| (n.to_owned(), v.text()))
                    })
                    .collect();
            }
            _ => {}
        }
        if e.level <= Level::Warn {
            let message = normalise(e.message);
            let key = format!("{}|{}|{}", e.level.name(), e.target, message);
            self.warnings.add(&key, now_s, |r| {
                r.level = Some(e.level);
                r.target = e.target.to_owned();
                r.detail = short(e.message, 300);
            });
        }
    }

    fn rate(&mut self, e: &EventView) {
        let n = |k: &str| e.num(k).unwrap_or(0.0);
        // Most of the frames since the last line were of the world.
        let world = self.window_frames > 0 && self.window_world * 2 >= self.window_frames;
        (self.window_frames, self.window_world) = (0, 0);
        self.pipelines_making_max = self.pipelines_making_max.max(n("spark.making"));
        if let Some(drops) = self.pending_drops.take()
            && world
        {
            self.drops.add(drops);
        }
        if self.rates.len() >= MAX_SAMPLES {
            return;
        }
        self.rates.push(Rate {
            fps: n("spark.fps"),
            ms: n("spark.ms"),
            preparing: n("spark.preparing_ms"),
            copies: n("spark.copies_ms"),
            textures: n("spark.textures_ms"),
            gpu: n("spark.gpu_ms"),
            copy_back: n("spark.copy_back_ms"),
            draws: n("spark.draws"),
            dropped: n("spark.dropped"),
            world,
            copies_mb: n("spark.copies_mb"),
            textures_mb: n("spark.textures_mb"),
            copy_back_mb: n("spark.copy_back_mb"),
            making: n("spark.making"),
        });
    }

    pub fn medians(&self) -> Medians {
        let world: Vec<&Rate> = self.rates.iter().filter(|r| r.world).collect();
        let (is_world, rates): (bool, Vec<&Rate>) = if world.is_empty() {
            (false, self.rates.iter().collect())
        } else {
            (true, world)
        };
        medians_of(is_world, &rates)
    }

    /// Medians of the stable scene's one-second windows (none before it).
    pub fn stable_medians(&self) -> Medians {
        let rates: Vec<&Rate> = self.stable_windows.iter().collect();
        medians_of(true, &rates)
    }

    /// Where to look next, most pressing first (at most four).
    pub fn next_targets(&self) -> Vec<String> {
        let mut out = Vec::new();
        let m = self.medians();
        if let Some((_, c)) = &self.crash {
            out.push(format!(
                "**Falha** ({}{}){}: comece pela pilha em \"Falha\" e pelos últimos avisos; em eventos.jsonl, \
                 as linhas de nível ERROR logo antes dela.",
                c.what,
                c.code.map(|c| format!(" {c:#010x}")).unwrap_or_default(),
                c.location.as_ref().map(|l| format!(" em `{l}`")).unwrap_or_default()
            ));
        }
        if !self.device_lost.is_empty() {
            out.push(
                "**Dispositivo perdido / TDR**: a placa parou de responder. Veja o motivo em \"Dispositivo perdido\", \
                 os avisos `slow host submission` e as pipelines feitas logo antes (eventos.jsonl, target \
                 `victory_gpu::host`); se a tela piscou, conferir TdrDelay."
                    .to_owned(),
            );
        }
        if self.first_world_s.is_none() && self.frames.count() > 0 {
            out.push(
                "**Não chegou ao mundo** (nenhum trecho de quadros com 6+ submissões): o problema está no \
                 carregamento/menus; veja os avisos e erros e as últimas mensagens de eventos.jsonl."
                    .to_owned(),
            );
        }
        let draws = m.draws.unwrap_or(0.0);
        let mut dropped = m.dropped.unwrap_or(0.0);
        // Work left out on purpose is not a fault of pipelines or shaders.
        if let Some((faults, on_purpose)) = self.drops.split() {
            if on_purpose >= 5.0 {
                let causes: Vec<String> = self
                    .drops
                    .top()
                    .iter()
                    .filter(|(name, _, _)| super::drops::on_purpose(name))
                    .map(|(name, median, _)| format!("{name} {median:.0}"))
                    .collect();
                out.push(format!(
                    "**Trabalho deixado de fora de propósito**: {on_purpose:.0} por quadro por escolha dos ajustes \
                     ({}); não é falha de tradução. Ver `dropped_by_cause` no resumo.json.",
                    causes.join(", ")
                ));
            }
            dropped = faults;
        }
        if dropped >= 5.0 || (draws > 0.0 && dropped / draws >= 0.01) {
            let mut s = format!(
                "**Draws descartados**: {dropped:.0} por quadro sem pipeline do host (de {draws:.0}). Corrigir as \
                 pipelines e shaders das tabelas \"Pipelines recusadas\" e \"Instruções não traduzidas\""
            );
            if let Some((what, _)) = self.untranslated.top(1).first() {
                let _ = write!(s, "; a instrução mais frequente é `{}`", short(what, 80));
            }
            s.push('.');
            out.push(s);
        } else if let Some((what, row)) = self.untranslated.top(1).first() {
            out.push(format!(
                "**Shaders não traduzidos**: `{}` barra {} shader(s); implementar essa instrução no tradutor \
                 (victory-shader) tira os draws deles do chão.",
                short(what, 80),
                row.others.len().max(1)
            ));
        }
        let faults_high = self.long_frame_faults > 5000.0
            && self
                .fault_rate_mean()
                .is_none_or(|mean| self.long_frame_faults > 3.0 * mean);
        let ram_low = self.ram_free_min_mb.is_some_and(|f| f < 1024);
        if self.long_world_frames > 0 && (faults_high || ram_low) {
            out.push(format!(
                "**Memória**: {} quadro(s) de mais de 1 s no mundo com {}; ver \"Memória\" e os orçamentos \
                 (VICTORY_*_BUDGET), e a linha `video memory` em eventos.jsonl.",
                self.long_world_frames,
                match (faults_high, ram_low) {
                    (true, true) => "muitas falhas de página e pouca RAM livre",
                    (true, false) => "muitas falhas de página",
                    _ => "pouca RAM livre",
                }
            ));
        } else if self.long_world_frames >= 3 {
            out.push(format!(
                "**Engasgos**: {} quadro(s) de mais de 1 s no mundo; procure em eventos.jsonl `host pipeline` \
                 (compilação na hora) e `slow host submission` perto desses momentos.",
                self.long_world_frames
            ));
        }
        if let (Some(used), Some(budget)) = (self.vram_used_max_mb, self.vram_budget_min_mb)
            && used > budget * 0.95
        {
            out.push(format!(
                "**VRAM no limite**: uso de até {used:.0} MB para orçamento de {budget:.0} MB; o sistema move \
                 recursos para a RAM compartilhada (lento). Reduzir orçamentos de texturas/cópias."
            ));
        }
        if let (Some(gpu), Some(prep)) = (m.gpu, m.preparing)
            && gpu + prep > 0.0
        {
            if gpu > prep {
                out.push(format!(
                    "**Placa é o limite**: {gpu:.1} ms/quadro na GPU contra {prep:.1} ms preparando. Ver as \
                     pipelines mais caras (avisos `slow host submission` com a lista de pipelines) e passes \
                     repetidos."
                ));
            } else {
                let parts = [
                    (
                        m.copies.unwrap_or(0.0),
                        "cópias de memória do título (copies)",
                    ),
                    (m.textures.unwrap_or(0.0), "texturas (refresh/detile)"),
                    (m.copy_back.unwrap_or(0.0), "cópia de volta (copy back)"),
                ];
                let (ms, what) = parts
                    .iter()
                    .copied()
                    .max_by(|a, b| a.0.total_cmp(&b.0))
                    .unwrap_or((0.0, ""));
                out.push(format!(
                    "**CPU (preparação) é o limite**: {prep:.1} ms/quadro preparando contra {gpu:.1} ms na GPU; \
                     o maior pedaço é {what} com {ms:.1} ms. Ver as linhas periódicas desse componente em \
                     eventos.jsonl (target `victory_gpu::host`)."
                ));
            }
        }
        if out.is_empty() {
            out.push(
                "Nada se destaca: rode por mais tempo no mundo (pelo menos 2 min) para ter amostras.".to_owned(),
            );
        }
        out.truncate(4);
        out
    }

    /// `resumo.json`; `meta` is the session's metadata object.
    pub fn to_json(&self, meta: &str) -> String {
        let pct = |h: &Histogram| {
            Obj::new()
                .int("frames", h.count())
                .opt_num("p50_ms", h.percentile(0.50))
                .opt_num("p95_ms", h.percentile(0.95))
                .opt_num("p99_ms", h.percentile(0.99))
                .opt_num("max_ms", h.max())
                .opt_num(
                    "fps_p50",
                    h.percentile(0.50).filter(|v| *v > 0.0).map(|v| 1000.0 / v),
                )
                .finish()
        };
        let medians_json = |m: Medians| {
            Obj::new()
                .bool("world", m.world)
                .int("samples", m.samples as u64)
                .opt_num("fps", m.fps)
                .opt_num("ms", m.ms)
                .opt_num("preparing_ms", m.preparing)
                .opt_num("copies_ms", m.copies)
                .opt_num("textures_ms", m.textures)
                .opt_num("gpu_ms", m.gpu)
                .opt_num("copy_back_ms", m.copy_back)
                .opt_num("draws", m.draws)
                .opt_num("dropped", m.dropped)
                .opt_num("copies_mb", m.copies_mb)
                .opt_num("textures_mb", m.textures_mb)
                .opt_num("copy_back_mb", m.copy_back_mb)
                .finish()
        };
        let medians = medians_json(self.medians());
        let (stable_draws, stable_after_s) = self.rule();
        let stable = Obj::new()
            .num("draws_at_least", stable_draws)
            .num("after_world_s", stable_after_s)
            .opt_num("from_s", self.stable_from_s)
            .raw("frames", &pct(&self.stable))
            .raw("window_medians", &medians_json(self.stable_medians()))
            .int("windows_making_pipelines", self.stable_making_windows)
            .finish();
        let untranslated_shaders =
            json::array(self.untranslated_shaders.iter().map(|(hash, stage, what)| {
                Obj::new()
                    .str("hash", hash)
                    .str("stage", stage)
                    .str("instruction", what)
                    .finish()
            }));
        let measurement = Obj::new()
            .opt_num(
                "device_create_ms",
                self.adapter
                    .iter()
                    .find(|(k, _)| k == "create_ms")
                    .and_then(|(_, v)| v.parse().ok()),
            )
            .opt_num("driver_cache_mb", self.driver_cache.map(|c| c.0))
            .opt_num("driver_cache_files", self.driver_cache.map(|c| c.1))
            .finish();
        let tally = |t: &Tally, others: bool| {
            json::array(t.top(TOP).into_iter().map(|(k, r)| {
                let mut o = Obj::new()
                    .str("key", k)
                    .int("count", r.count)
                    .num("first_s", r.first_s);
                if !r.detail.is_empty() {
                    o = o.str("detail", &r.detail);
                }
                if others {
                    o = o.int("distinct_shaders", r.others.len() as u64);
                }
                o.finish()
            }))
        };
        let warnings = json::array(self.warnings.top(TOP).into_iter().map(|(_, r)| {
            Obj::new()
                .str("level", r.level.map_or("?", Level::name))
                .str("target", &r.target)
                .str("message", &r.detail)
                .int("count", r.count)
                .num("first_s", r.first_s)
                .finish()
        }));
        let crash = match &self.crash {
            None => "null".to_owned(),
            Some((at, c)) => Obj::new()
                .num("at_s", *at)
                .str("what", &c.what)
                .raw(
                    "code",
                    &c.code
                        .map_or("null".into(), |c| json::string(&format!("{c:#010x}"))),
                )
                .raw(
                    "location",
                    &c.location.as_deref().map_or("null".into(), json::string),
                )
                .str("detail", &c.detail)
                .raw(
                    "stack",
                    &json::array(c.stack.iter().map(|s| json::string(s))),
                )
                .finish(),
        };
        let mut adapter = Obj::new();
        for (k, v) in &self.adapter {
            adapter.push_raw(k, &json::string(v));
        }
        let opt_u = |v: Option<u64>| v.map(|v| v as f64);
        Obj::new()
            .int("format", 1)
            .raw("session", meta)
            .num("duration_s", self.now_s)
            .raw(
                "end",
                &self.end.as_deref().map_or("null".into(), json::string),
            )
            .opt_num("first_world_frame_s", self.first_world_s)
            .raw("frames_world", &pct(&self.world))
            .raw("frames_all", &pct(&self.frames))
            .int("long_frames_over_1s", self.long_frames)
            .int("long_world_frames_over_1s", self.long_world_frames)
            .raw("frame_rate_medians", &medians)
            .raw("stable_scene", &stable)
            .raw("phases", &self.phases.to_json())
            .raw("phases_world", &self.phases.world_json())
            .raw("long_frame_causes", &self.phases.long_json())
            .raw("dropped_by_cause", &self.drops.to_json())
            .raw("untranslated_shaders", &untranslated_shaders)
            .raw("measurement", &measurement)
            .raw(
                "ab",
                &self
                    .ab
                    .first()
                    .map_or("null".into(), |(_, ab)| ab.to_json()),
            )
            .raw(
                "ab_experiments",
                &json::array(self.ab.iter().map(|(name, ab)| {
                    Obj::new()
                        .str("name", name)
                        .raw("ab", &ab.to_json())
                        .finish()
                })),
            )
            .num("pipelines_being_made_max", self.pipelines_making_max)
            .raw("adapter", &adapter.finish())
            .raw("refused_pipelines", &tally(&self.refused, false))
            .int("refused_pipelines_total", self.refused.total())
            .raw(
                "untranslated_instructions",
                &tally(&self.untranslated, true),
            )
            .int("untranslated_total", self.untranslated.total())
            .raw(
                "device_lost",
                &json::array(
                    self.device_lost
                        .iter()
                        .map(|(t, w)| Obj::new().num("at_s", *t).str("reason", w).finish()),
                ),
            )
            .raw("crash", &crash)
            .raw(
                "memory",
                &Obj::new()
                    .opt_num("ram_total_mb", opt_u(self.ram_total_mb))
                    .opt_num("ram_free_min_mb", opt_u(self.ram_free_min_mb))
                    .num("page_faults_per_s_max", self.fault_rate_max)
                    .opt_num("page_faults_per_s_mean", self.fault_rate_mean())
                    .num("page_faults_per_s_in_long_frames", self.long_frame_faults)
                    .opt_num("vram_used_max_mb", self.vram_used_max_mb)
                    .opt_num("vram_budget_min_mb", self.vram_budget_min_mb)
                    .opt_num("shared_used_max_mb", self.shared_used_max_mb)
                    .finish(),
            )
            .raw("warnings_top", &warnings)
            .int("warnings_total", self.warnings.total())
            .int("events_dropped", self.dropped_events)
            .int("events_suppressed", self.suppressed_events)
            .raw(
                "next_targets",
                &json::array(self.next_targets().iter().map(|s| json::string(s))),
            )
            .finish()
    }

    /// `resumo.md`; `meta` are the headline facts (label, value).
    pub fn to_markdown(&self, meta: &[(String, String)]) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "# Resumo da sessão (VItoria Spark)\n");
        for (k, v) in meta {
            let _ = writeln!(s, "- **{k}**: {}", short(v, 160));
        }
        if !self.adapter.is_empty() {
            let gpu: Vec<String> = self
                .adapter
                .iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect();
            let _ = writeln!(s, "- **GPU**: {}", short(&gpu.join(", "), 200));
        }
        let _ = writeln!(s, "\n## Visão geral\n");
        let _ = writeln!(s, "- Duração: {}", fmt_s(self.now_s));
        let _ = writeln!(
            s,
            "- Primeiro quadro do mundo: {}",
            self.first_world_s
                .map_or("não chegou".to_owned(), |t| format!(
                    "{} após o início",
                    fmt_s(t)
                ))
        );
        let _ = writeln!(
            s,
            "- Fim: {}",
            self.end
                .as_deref()
                .unwrap_or("em andamento (resumo periódico)")
        );
        if self.dropped_events + self.suppressed_events > 0 {
            let _ = writeln!(
                s,
                "- Eventos fora do eventos.jsonl: {} perdidos (fila cheia), {} repetidos suprimidos",
                self.dropped_events, self.suppressed_events
            );
        }

        let _ = writeln!(s, "\n## Próximo alvo\n");
        for (i, t) in self.next_targets().iter().enumerate() {
            let _ = writeln!(s, "{}. {t}", i + 1);
        }

        let _ = writeln!(s, "\n## Quadros\n");
        let _ = writeln!(
            s,
            "| quadros | n | p50 ms | p95 ms | p99 ms | máx ms | fps (p50) |"
        );
        let _ = writeln!(s, "|---|---|---|---|---|---|---|");
        for (name, h) in [
            ("cena estável", &self.stable),
            ("mundo", &self.world),
            ("todos", &self.frames),
        ] {
            let p50 = h.percentile(0.5);
            let _ = writeln!(
                s,
                "| {name} | {} | {} | {} | {} | {} | {} |",
                h.count(),
                fmt_ms(p50),
                fmt_ms(h.percentile(0.95)),
                fmt_ms(h.percentile(0.99)),
                fmt_ms(h.max()),
                p50.filter(|v| *v > 0.0)
                    .map_or("—".into(), |v| format!("{:.1}", 1000.0 / v))
            );
        }
        let _ = writeln!(
            s,
            "\nQuadros de mais de 1 s: {} (no mundo: {}).",
            self.long_frames, self.long_world_frames
        );
        let (stable_draws, stable_after_s) = self.rule();
        let _ = writeln!(
            s,
            "\nCena estável ({stable_draws:.0}+ draws por quadro, {stable_after_s:.0} s depois do 1º quadro do mundo): {}.",
            self.stable_from_s.map_or(
                "não alcançada (SPARK_STABLE_DRAWS e SPARK_STABLE_AFTER_S mudam a regra)"
                    .to_owned(),
                |t| format!("desde {} após o início", fmt_s(t))
            )
        );

        let m = self.medians();
        let _ = writeln!(
            s,
            "\n## Composição do quadro (medianas de {} amostras{})\n",
            m.samples,
            if m.world { ", só mundo" } else { ", todas" }
        );
        let _ = writeln!(
            s,
            "| amostras | fps | ms/quadro | preparando | cópias | texturas | GPU | cópia de volta | draws | descartados |"
        );
        let _ = writeln!(s, "|---|---|---|---|---|---|---|---|---|---|");
        let stable = self.stable_medians();
        let rows = [
            (format!("{} linhas de 5 s", m.samples), m),
            (
                format!("{} janelas de 1 s da cena estável", stable.samples),
                stable,
            ),
        ];
        for (name, m) in rows.iter().filter(|(_, m)| m.samples > 0) {
            let _ = writeln!(
                s,
                "| {name} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                fmt_ms(m.fps),
                fmt_ms(m.ms),
                fmt_ms(m.preparing),
                fmt_ms(m.copies),
                fmt_ms(m.textures),
                fmt_ms(m.gpu),
                fmt_ms(m.copy_back),
                m.draws.map_or("—".into(), |v| format!("{v:.0}")),
                m.dropped.map_or("—".into(), |v| format!("{v:.0}"))
            );
        }
        if self.pipelines_making_max > 0.0 {
            let _ = writeln!(
                s,
                "\nPipelines sendo feitas ao mesmo tempo: até {:.0}.",
                self.pipelines_making_max
            );
        }
        let causes = self.drops.top();
        if !causes.is_empty() {
            let text: Vec<String> = causes
                .iter()
                .map(|(name, median, max)| format!("{name} {median:.0} (máx {max:.0})"))
                .collect();
            let _ = writeln!(
                s,
                "\nDescartados por causa, por quadro: {}.",
                text.join(", ")
            );
        }
        s.push_str(&self.phases.to_markdown());
        for (i, (name, ab)) in self.ab.iter().enumerate() {
            let md = ab.to_markdown();
            if self.ab.len() > 1 {
                s.push_str(&md.replacen(
                    "## A/B na rodada (SPARK_AB)",
                    &format!("## A/B na rodada: experimento {i} ({name})"),
                    1,
                ));
            } else {
                s.push_str(&md);
            }
        }

        if self.refused.total() > 0 {
            let _ = writeln!(
                s,
                "\n## Pipelines recusadas (total {})\n",
                self.refused.total()
            );
            let _ = writeln!(s, "| n | shaders | motivo |");
            let _ = writeln!(s, "|---|---|---|");
            for (k, r) in self.refused.top(TOP) {
                let _ = writeln!(
                    s,
                    "| {} | `{}` | {} |",
                    r.count,
                    cell(k, 90),
                    cell(&r.detail, 120)
                );
            }
        }
        if self.untranslated.total() > 0 {
            let _ = writeln!(
                s,
                "\n## Instruções não traduzidas (total {})\n",
                self.untranslated.total()
            );
            let _ = writeln!(s, "| n | instrução | shaders distintos |");
            let _ = writeln!(s, "|---|---|---|");
            for (k, r) in self.untranslated.top(TOP) {
                let _ = writeln!(
                    s,
                    "| {} | `{}` | {} |",
                    r.count,
                    cell(k, 90),
                    r.others.len()
                );
            }
        }
        if !self.device_lost.is_empty() {
            let _ = writeln!(s, "\n## Dispositivo perdido / TDR\n");
            for (t, w) in &self.device_lost {
                let _ = writeln!(s, "- aos {}: {}", fmt_s(*t), short(w, 300));
            }
        }
        if let Some((t, c)) = &self.crash {
            let _ = writeln!(s, "\n## Falha\n");
            let _ = writeln!(
                s,
                "- aos {}: {}{}{}",
                fmt_s(*t),
                c.what,
                c.code.map(|c| format!(" {c:#010x}")).unwrap_or_default(),
                c.location
                    .as_ref()
                    .map(|l| format!(" em `{l}`"))
                    .unwrap_or_default()
            );
            if !c.detail.is_empty() {
                let _ = writeln!(s, "- {}", short(&c.detail, 300));
            }
            if !c.stack.is_empty() {
                let _ = writeln!(s, "\n```");
                for f in c.stack.iter().take(16) {
                    let _ = writeln!(s, "{f}");
                }
                if c.stack.len() > 16 {
                    let _ = writeln!(s, "… mais {} (ver resumo.json)", c.stack.len() - 16);
                }
                let _ = writeln!(s, "```");
            }
        }

        let _ = writeln!(s, "\n## Memória\n");
        let mb = |v: Option<f64>| v.map_or("—".to_owned(), |v| format!("{v:.0} MB"));
        let _ = writeln!(
            s,
            "- RAM: {} no total, mínimo livre {}",
            mb(self.ram_total_mb.map(|v| v as f64)),
            mb(self.ram_free_min_mb.map(|v| v as f64))
        );
        let _ = writeln!(
            s,
            "- Falhas de página/s: máx {:.0}, média {}, em quadros de mais de 1 s {:.0}",
            self.fault_rate_max,
            self.fault_rate_mean()
                .map_or("—".into(), |v| format!("{v:.0}")),
            self.long_frame_faults
        );
        let _ = writeln!(
            s,
            "- VRAM: uso máx {}, orçamento mín {}, compartilhada máx {}",
            mb(self.vram_used_max_mb),
            mb(self.vram_budget_min_mb),
            mb(self.shared_used_max_mb)
        );

        if self.warnings.total() > 0 {
            let _ = writeln!(
                s,
                "\n## Avisos e erros (top {TOP} de {})\n",
                self.warnings.total()
            );
            let _ = writeln!(s, "| n | nível | target | mensagem |");
            let _ = writeln!(s, "|---|---|---|---|");
            for (_, r) in self.warnings.top(TOP) {
                let _ = writeln!(
                    s,
                    "| {} | {} | `{}` | {} |",
                    r.count,
                    r.level.map_or("?", Level::name),
                    cell(&r.target, 40),
                    cell(&r.detail, 140)
                );
            }
        }
        let _ = writeln!(
            s,
            "\n---\nDiagnóstico completo (problemas em ordem, com a correção sugerida de cada um): `python scripts/motor.py diagnosticar <esta pasta>`.\nDetalhes: `eventos.jsonl` nesta pasta (um JSON por linha; filtre por `target`/`level`). Guia: docs/logs.md."
        );
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(fps: f64, prep: f64, gpu: f64, dropped: f64) -> Vec<(&'static str, Value)> {
        vec![
            ("spark.kind", Value::Str("frame_rate".into())),
            ("spark.fps", Value::F64(fps)),
            ("spark.ms", Value::F64(1000.0 / fps)),
            ("spark.preparing_ms", Value::F64(prep)),
            ("spark.copies_ms", Value::F64(prep * 0.6)),
            ("spark.textures_ms", Value::F64(prep * 0.3)),
            ("spark.gpu_ms", Value::F64(gpu)),
            ("spark.copy_back_ms", Value::F64(0.5)),
            ("spark.draws", Value::F64(2400.0)),
            ("spark.dropped", Value::F64(dropped)),
            ("spark.making", Value::U64(3)),
        ]
    }

    fn info<'a>(fields: &'a [(&'static str, Value)]) -> EventView<'a> {
        EventView {
            level: Level::Info,
            target: "victory_gpu::host",
            message: "frame rate",
            fields,
        }
    }

    #[test]
    fn frames_with_parts_drops_by_cause_and_the_driver_cache_reach_the_summary() {
        use super::super::drops::DropCause;
        use super::super::phases::Phase;
        let mut s = Summary::new();
        let mut parts = FrameParts::default();
        parts.us[Phase::Remap as usize] = 30_000;
        parts.us[Phase::Gpu as usize] = 10_000;
        let (mut t, mut subs) = (0.0, 0);
        for _ in 0..60 {
            subs += 12;
            s.present_parts(t, subs, None, Some(&parts));
            t += 0.05;
        }
        let mut drops = vec![("spark.kind", Value::Str("draws_dropped".into()))];
        drops.push(("spark.builder_off", Value::F64(380.0)));
        drops.push(("spark.refused", Value::F64(2.0)));
        s.event(t, &info(&drops));
        s.event(t, &info(&rate(20.0, 18.0, 10.0, 382.0)));
        let cache = [
            ("spark.kind", Value::Str("driver_cache".into())),
            ("spark.mb", Value::U64(31_000)),
            ("spark.files", Value::U64(12)),
        ];
        s.event(t, &info(&cache));
        let adapter = [
            ("spark.kind", Value::Str("adapter".into())),
            ("spark.create_ms", Value::U64(45_000)),
        ];
        s.event(t, &info(&adapter));
        assert_eq!(DropCause::Other as usize, DROP_CAUSES - 1);

        let json = s.to_json("{}");
        assert!(
            json.contains(r#""phases":{"scope":"world","frames":50,"#),
            "{json}"
        );
        assert!(
            json.contains(r#"{"cause":"builder_off","per_frame_median":380,"#),
            "{json}"
        );
        assert!(
            json.contains(
                r#""measurement":{"device_create_ms":45000,"driver_cache_mb":31000,"driver_cache_files":12}"#
            ),
            "{json}"
        );
        // 380 of the 382 drops are a setting's choice: the two refused ones
        // alone do not make dropped draws the target.
        let targets = s.next_targets().join("\n");
        assert!(targets.contains("(builder_off 380); não"), "{targets}");
        assert!(!targets.contains("**Draws descartados**"), "{targets}");
        let md = s.to_markdown(&[]);
        assert!(
            md.contains("## Fases do quadro (50 quadros do mundo"),
            "{md}"
        );
        assert!(
            md.contains("Descartados por causa, por quadro: builder_off 380"),
            "{md}"
        );
    }

    /// A synthetic run: 20 s of menus, then a minute of world frames at
    /// 30 a second with a few long ones, refused pipelines and a shader
    /// instruction the translator lacks.
    pub(super) fn synthetic() -> Summary {
        let mut s = Summary::new();
        let mut t = 0.0;
        let mut subs = 0;
        // Menus: 30 fps, 2 submissions a frame.
        while t < 20.0 {
            subs += 2;
            s.present(t, subs);
            t += 1.0 / 30.0;
        }
        // The world: 25 to 40 ms frames, 12 submissions; every 10 s one of
        // 1.5 s, during which the memory sampler saw many page faults.
        let mut i = 0u64;
        let mut faults = 1_000_000u64;
        while t < 80.0 {
            subs += 12;
            s.present(t, subs);
            i += 1;
            let ms = if i.is_multiple_of(300) {
                1500.0
            } else {
                25.0 + (i % 16) as f64
            };
            if i.is_multiple_of(30) {
                // Right before a long frame the sampler sees a burst.
                let long = i.is_multiple_of(300);
                faults += if long { 60_000 } else { 2_000 };
                s.memory(t, 16_384, if long { 700 } else { 1_800 }, faults);
            }
            if i.is_multiple_of(150) {
                let f = rate(30.0, 18.0, 24.0, 40.0);
                s.event(t, &info(&f));
            }
            t += ms / 1000.0;
        }
        for (shaders, n) in [("[a1b2c3d4e5f60718.ps]", 5), ("[0011223344556677.cs]", 2)] {
            for _ in 0..n {
                let f = vec![
                    ("spark.kind", Value::Str("pipeline_refused".into())),
                    ("shaders", Value::Str(shaders.into())),
                    ("error", Value::Str("0x80070057".into())),
                ];
                s.event(
                    30.0,
                    &EventView {
                        level: Level::Warn,
                        target: "victory_gpu::pipeline::host",
                        message: "host pipeline refused",
                        fields: &f,
                    },
                );
            }
        }
        for (hash, what) in [
            ("aa", "v_permlane16_b32"),
            ("bb", "v_permlane16_b32"),
            ("cc", "image_bvh_intersect_ray"),
        ] {
            let f = vec![
                ("spark.kind", Value::Str("shader_untranslated".into())),
                ("hash", Value::Str(hash.into())),
                ("stage", Value::Str("cs".into())),
                ("spark.instruction", Value::Str(what.into())),
                (
                    "error",
                    Value::Str(format!("GFX10 code at 0x40: {what} is not supported")),
                ),
            ];
            s.event(
                25.0,
                &EventView {
                    level: Level::Warn,
                    target: "victory_gpu::pipeline",
                    message: "shader does not translate; its pipelines' draws are dropped",
                    fields: &f,
                },
            );
        }
        let f = vec![
            ("spark.kind", Value::Str("video_memory".into())),
            ("spark.vram_used_mb", Value::U64(7_900)),
            ("spark.vram_budget_mb", Value::U64(7_400)),
            ("spark.shared_used_mb", Value::U64(1_200)),
        ];
        s.event(60.0, &info(&f));
        let f = vec![
            ("spark.kind", Value::Str("adapter".into())),
            ("spark.name", Value::Str("Placa de teste".into())),
            ("spark.driver", Value::Str("32.0.15.1".into())),
            ("spark.vram_mb", Value::U64(8_192)),
            ("spark.wave_max", Value::U64(32)),
        ];
        s.event(1.0, &info(&f));
        for frame in [1201, 1202, 1203] {
            let msg = format!("slow host submission at frame {frame}");
            s.event(
                40.0,
                &EventView {
                    level: Level::Warn,
                    target: "victory_gpu::host",
                    message: &msg,
                    fields: &[],
                },
            );
        }
        s.end(80.0, "o título saiu (código 0)");
        s
    }

    #[test]
    fn histogram_percentiles() {
        let mut h = Histogram::default();
        for i in 1..=100 {
            h.add(i as f64);
        }
        assert_eq!(h.count(), 100);
        let p50 = h.percentile(0.5).unwrap();
        assert!((49.0..=51.0).contains(&p50), "{p50}");
        let p99 = h.percentile(0.99).unwrap();
        assert!((98.0..=100.0).contains(&p99), "{p99}");
        assert_eq!(h.max(), Some(100.0));
        h.add(5000.0);
        assert_eq!(h.percentile(1.0), Some(5000.0));
        assert!(Histogram::default().percentile(0.5).is_none());
    }

    #[test]
    fn world_frames_and_first_world_frame() {
        let s = synthetic();
        let t = s.first_world_s.unwrap();
        assert!((19.9..20.1).contains(&t), "{t}");
        // Menu frames are not the world's.
        assert!(s.world.count() < s.frames.count());
        let p50 = s.world.percentile(0.5).unwrap();
        assert!((30.0..36.0).contains(&p50), "{p50}");
        assert!(s.long_world_frames >= 3, "{}", s.long_world_frames);
        assert_eq!(s.long_frames, s.long_world_frames);
    }

    #[test]
    fn rates_are_split_by_world() {
        let mut s = Summary::new();
        // A menu second, then a world second.
        for i in 0..30 {
            s.present(i as f64 / 30.0, i * 2);
        }
        let menu = rate(30.0, 2.0, 1.0, 0.0);
        s.event(1.0, &info(&menu));
        let mut subs = 60;
        for i in 0..30 {
            subs += 10;
            s.present(1.0 + i as f64 / 30.0, subs);
        }
        let world = rate(20.0, 30.0, 10.0, 0.0);
        s.event(2.0, &info(&world));
        let m = s.medians();
        assert!(m.world);
        assert_eq!(m.samples, 1);
        assert_eq!(m.preparing, Some(30.0));
    }

    #[test]
    fn the_stable_scene_waits_for_draws_and_time() {
        let mut s = Summary::new();
        s.stable_rule(4000.0, 10.0);
        let window = |draws: f64, preparing: f64| Rate {
            draws,
            preparing,
            ..Rate::default()
        };
        // The world from 0 s: 12 submissions a frame, a frame of 100 ms;
        // each second a window, with few draws before 15 s and many after.
        let mut subs = 0;
        for i in 0..300u32 {
            let t = f64::from(i) * 0.1;
            subs += 12;
            s.present(t, subs);
            if i % 10 == 0 {
                let (draws, preparing) = if i < 150 {
                    (2000.0, 90.0)
                } else {
                    (5000.0, 40.0)
                };
                s.window(t, window(draws, preparing));
            }
        }
        // Many draws came at 15 s, past the 10 s after the world's start.
        let from = s.stable_from_s.unwrap();
        assert!((14.9..15.2).contains(&from), "{from}");
        assert!(s.stable.count() > 100 && s.stable.count() < s.world.count());
        let m = s.stable_medians();
        assert_eq!(m.preparing, Some(40.0));
        assert!(m.samples >= 14, "{}", m.samples);
        let md = s.to_markdown(&[]);
        assert!(md.contains("| cena estável |"), "{md}");
        assert!(md.contains("janelas de 1 s da cena estável"), "{md}");
        assert!(s.to_json("{}").contains(r#""stable_scene":{"#));

        // Many draws too early do not count.
        let mut early = Summary::new();
        early.stable_rule(4000.0, 60.0);
        let mut subs = 0;
        for i in 0..300 {
            subs += 12;
            let t = f64::from(i) * 0.1;
            early.present(t, subs);
            early.window(t, window(5000.0, 40.0));
        }
        assert_eq!(early.stable_from_s, None);
        assert_eq!(early.stable.count(), 0);
        assert!(early.to_markdown(&[]).contains("não alcançada"));
    }

    #[test]
    fn tallies_dedupe_and_count() {
        let s = synthetic();
        let refused = s.refused.top(10);
        assert_eq!(refused[0].0, "[a1b2c3d4e5f60718.ps]");
        assert_eq!(refused[0].1.count, 5);
        assert_eq!(s.refused.total(), 7);
        let un = s.untranslated.top(10);
        assert_eq!(un[0].0, "v_permlane16_b32");
        assert_eq!(un[0].1.count, 2);
        assert_eq!(un[0].1.others.len(), 2);
        // Three warnings that differ only in a number are one row.
        let slow: Vec<_> = s
            .warnings
            .top(50)
            .into_iter()
            .filter(|(k, _)| k.contains("slow host submission"))
            .collect();
        assert_eq!(slow.len(), 1);
        assert_eq!(slow[0].1.count, 3);
    }

    #[test]
    fn normalise_takes_numbers_out() {
        assert_eq!(
            normalise("frame 12, 3.5 ms at 0x1f00"),
            "frame #, # ms at 0x#"
        );
    }

    #[test]
    fn next_targets_follow_the_numbers() {
        let s = synthetic();
        let t = s.next_targets().join("\n");
        assert!(t.contains("Draws descartados"), "{t}");
        assert!(t.contains("v_permlane16_b32"), "{t}");
        assert!(t.contains("Memória"), "{t}");
        assert!(t.contains("VRAM no limite"), "{t}");
        assert!(s.next_targets().len() <= 4);

        // GPU-bound, nothing else wrong.
        let mut g = Summary::new();
        let mut subs = 0;
        for i in 0..200 {
            subs += 10;
            g.present(i as f64 * 0.03, subs);
            if i % 50 == 49 {
                let f = rate(33.0, 5.0, 25.0, 0.0);
                g.event(i as f64 * 0.03, &info(&f));
            }
        }
        let t = g.next_targets();
        assert_eq!(t.len(), 1, "{t:?}");
        assert!(t[0].contains("Placa é o limite"), "{t:?}");

        // CPU-bound: the biggest part is named.
        let mut c = Summary::new();
        let mut subs = 0;
        for i in 0..200 {
            subs += 10;
            c.present(i as f64 * 0.03, subs);
            if i % 50 == 49 {
                let f = rate(20.0, 40.0, 10.0, 0.0);
                c.event(i as f64 * 0.03, &info(&f));
            }
        }
        let t = c.next_targets().join("\n");
        assert!(
            t.contains("CPU (preparação)") && t.contains("cópias de memória"),
            "{t}"
        );
    }

    #[test]
    fn a_crash_comes_first() {
        let mut s = synthetic();
        s.crash(
            81.0,
            Crash {
                what: "unhandled exception".into(),
                code: Some(0xC000_0005),
                location: Some("title.exe+0x1234".into()),
                detail: "read 0x0".into(),
                stack: vec!["title.exe+0x1234".into(), "title.exe+0x99".into()],
            },
        );
        let t = s.next_targets();
        assert!(
            t[0].starts_with("**Falha**") && t[0].contains("0xc0000005"),
            "{t:?}"
        );
        let md = s.to_markdown(&[]);
        assert!(md.contains("## Falha") && md.contains("title.exe+0x99"));
    }

    #[test]
    fn markdown_is_short_and_json_is_balanced() {
        let s = synthetic();
        let meta = vec![("Versão".to_owned(), "0.1.0 (abc1234)".to_owned())];
        let md = s.to_markdown(&meta);
        assert!(md.lines().count() < 150, "{} lines", md.lines().count());
        for h in [
            "## Visão geral",
            "## Próximo alvo",
            "## Quadros",
            "## Memória",
            "## Avisos e erros",
        ] {
            assert!(md.contains(h), "{h}");
        }
        let j = s.to_json("{}");
        assert!(j.starts_with('{') && j.ends_with('}'));
        let depth = j.chars().fold(0i32, |d, c| match c {
            '{' | '[' => d + 1,
            '}' | ']' => d - 1,
            _ => d,
        });
        assert_eq!(depth, 0);
        assert!(j.contains("\"first_world_frame_s\":"));
        assert!(j.contains("\"key\":\"v_permlane16_b32\""));
    }

    /// Prints the synthetic run's summary (`cargo test -p victory-core
    /// example_summary -- --nocapture`), the example in docs/logs.md.
    #[test]
    fn example_summary() {
        let s = synthetic();
        let meta = vec![
            (
                "Versão".to_owned(),
                "0.1.0 (git abc1234, release)".to_owned(),
            ),
            (
                "Sistema".to_owned(),
                "Windows 11 build 26200.1234; 16 CPUs; 16384 MB".to_owned(),
            ),
        ];
        println!("{}", s.to_markdown(&meta));
    }
}

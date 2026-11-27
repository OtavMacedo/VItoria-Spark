//! The session log: what a run leaves for people and tools to read after it.
//!
//! [`start`] makes `logs/<UTC time>-<pid>/` (or under `SPARK_LOG_DIR`) and
//! from then on:
//!
//! * `eventos.jsonl`: one JSON object a line, the first the session's
//!   metadata (build, PC, settings), then one per tracing event at info
//!   and above (`SPARK_LOG_JSON_LEVEL` says otherwise; `SPARK_LOG_JSON=0`:
//!   no file). Written by a thread of its own through a bounded queue:
//!   when the queue is full an event is dropped and counted, never waited
//!   for. An event from one place in the code more than
//!   [`RATE_EVENTS`] times in [`RATE_WINDOW`] is counted instead of
//!   written, and the count is written when the window ends.
//! * `resumo.json` and `resumo.md`: the [`summary`] of the run, rewritten
//!   every minute (so a crash leaves one) and at the end.
//!
//! Personal details (the home folder, the title's folder, the user's and
//! computer's names, e-mail addresses) are replaced everywhere ([`anon`]).
//!
//! Fields named `spark.*` are for these files: the console log does not
//! show them (`crate::logging`), and `spark.kind` marks an event the
//! summary reads (`frame_rate`, `pipeline_refused`, `shader_untranslated`,
//! `device_lost`, `video_memory`, `adapter`, `draws_dropped`,
//! `driver_cache`; `frame_phases` is for the file alone).

mod ab;
pub mod anon;
pub mod drops;
mod json;
pub mod offline;
pub mod phases;
pub mod pipelines_x;
pub mod summary;
pub mod system;

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock};

use std::time::{Duration, Instant};

use tracing_subscriber::field::Visit;
use tracing_subscriber::layer::Context;

pub use summary::{Crash, Level};
use summary::{EventView, Summary, Value};

/// Events from one place in the code written per window; more are counted.
pub const RATE_EVENTS: u32 = 30;
pub const RATE_WINDOW: Duration = Duration::from_secs(10);
/// Lines the writer's queue holds.
const QUEUE: usize = 8192;
/// How often the summary is rewritten during the run.
const SUMMARY_EVERY: Duration = Duration::from_secs(60);
/// The same with switches alternating (`crate::ab`).
const SUMMARY_EVERY_AB: Duration = Duration::from_secs(15);

/// What the front end knows of its build.
pub struct BuildInfo {
    pub version: &'static str,
    pub git: &'static str,
    pub profile: &'static str,
}

enum Msg {
    Line(String),
    Flush(SyncSender<()>),
    Stop(SyncSender<()>),
}

struct Session {
    dir: PathBuf,
    start: Instant,
    started_ms: u64,
    tx: OnceLock<SyncSender<Msg>>,
    /// The writer was told to stop: nothing more is queued.
    stopped: AtomicBool,
    summary: Mutex<Summary>,
    anon: anon::Anonymizer,
    /// The most verbose level written to `eventos.jsonl` (`None`: no file).
    json_level: Option<Level>,
    /// The metadata object (JSON) and its headline facts for `resumo.md`.
    meta_json: String,
    meta_lines: Vec<(String, String)>,
    /// Per call site: when its window started, events in it, and counted.
    rates: Mutex<HashMap<usize, (Instant, u32, u64)>>,
}

static SESSION: OnceLock<Session> = OnceLock::new();
static ACTIVE: AtomicBool = AtomicBool::new(false);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static SUPPRESSED: AtomicU64 = AtomicU64::new(0);

fn active() -> Option<&'static Session> {
    if ACTIVE.load(Relaxed) {
        SESSION.get()
    } else {
        None
    }
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn level_named(s: &str) -> Option<Level> {
    match s.to_ascii_lowercase().as_str() {
        "error" => Some(Level::Error),
        "warn" => Some(Level::Warn),
        "info" => Some(Level::Info),
        "debug" => Some(Level::Debug),
        "trace" => Some(Level::Trace),
        _ => None,
    }
}

/// The level the session's layer must see (it reads info events for the
/// summary whatever is written to the file).
pub(crate) fn layer_level() -> Level {
    let file = env("SPARK_LOG_JSON_LEVEL")
        .and_then(|l| level_named(&l))
        .unwrap_or(Level::Info);
    file.max(Level::Info)
}

/// Starts the session (once; later calls return the same folder).
/// `game_dir`: the title's folder, named `<JOGO>` in the files. `None` if
/// the folder could not be made.
pub fn start(build: BuildInfo, game_dir: Option<&Path>) -> Option<PathBuf> {
    let base = env("SPARK_LOG_DIR").map_or_else(|| PathBuf::from("logs"), PathBuf::from);
    start_in(&base, build, game_dir)
}

/// [`start`] under `base`.
fn start_in(base: &Path, build: BuildInfo, game_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(s) = SESSION.get() {
        return Some(s.dir.clone());
    }
    let started_ms = system::unix_ms();
    let dir = base.join(format!(
        "{}-{}",
        system::compact(started_ms),
        std::process::id()
    ));
    // Absolute, so the working folder changing later does not move it.
    let dir = std::path::absolute(&dir).unwrap_or(dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(error = %e, "session log folder could not be made; no eventos.jsonl or resumo");
        return None;
    }
    let anon = anon::Anonymizer::from_env(game_dir);
    let json_level = match env("SPARK_LOG_JSON").as_deref() {
        Some("0") => None,
        _ => Some(
            env("SPARK_LOG_JSON_LEVEL")
                .and_then(|l| level_named(&l))
                .unwrap_or(Level::Info),
        ),
    };
    let (meta_json, meta_lines) = metadata(&build, started_ms, &anon, &dir);
    let file = match json_level {
        Some(_) => std::fs::File::create(dir.join("eventos.jsonl"))
            .map_err(|e| tracing::warn!(error = %e, "eventos.jsonl could not be made"))
            .ok(),
        None => None,
    };
    let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE);
    let session = Session {
        dir: dir.clone(),
        start: Instant::now(),
        started_ms,
        tx: OnceLock::new(),
        stopped: AtomicBool::new(false),
        summary: Mutex::new({
            let mut summary = Summary::new();
            let env_num = |name: &str| {
                std::env::var(name)
                    .ok()
                    .and_then(|v| v.trim().parse::<f64>().ok())
            };
            summary.stable_rule(
                env_num("SPARK_STABLE_DRAWS").unwrap_or(summary::STABLE_DRAWS),
                env_num("SPARK_STABLE_AFTER_S").unwrap_or(summary::STABLE_AFTER_S),
            );
            for (name, plan) in crate::ab::program().map_or(&[][..], |p| &p.experiments[..]) {
                summary.ab_plan(
                    name,
                    [plan.label(0), plan.label(1)],
                    (0..plan.factors).map(|f| plan.factor_label(f)).collect(),
                    plan.period_s,
                );
            }
            summary
        }),
        anon,
        json_level: json_level.filter(|_| file.is_some()),
        meta_json,
        meta_lines,
        rates: Mutex::new(HashMap::new()),
    };
    if SESSION.set(session).is_err() {
        return SESSION.get().map(|s| s.dir.clone());
    }
    let session = SESSION.get()?;
    let mut file = file.map(|f| std::io::BufWriter::with_capacity(1 << 16, f));
    if let Some(f) = &mut file {
        let _ = writeln!(f, "{}", session.meta_json);
        let _ = f.flush();
    }
    let spawned = std::thread::Builder::new()
        .name("spark-log".into())
        .spawn(move || writer(rx, file));
    if spawned.is_ok() {
        let _ = session.tx.set(tx);
    }
    ACTIVE.store(true, Relaxed);
    let shown = session.anon.apply(&dir.to_string_lossy()).into_owned();
    tracing::info!(dir = %shown, "session log (eventos.jsonl, resumo.md)");
    // Walking the drivers' cache folders takes a moment on a slow disk:
    // not on the thread that starts the title.
    let _ = std::thread::Builder::new()
        .name("spark-driver-cache".into())
        .spawn(|| {
            if let Some((mb, files)) = system::driver_cache() {
                tracing::info!(
                    spark.kind = "driver_cache",
                    spark.mb = mb,
                    spark.files = files,
                    "the card drivers' shader caches hold {mb} MB in {files} files"
                );
            }
        });
    Some(dir)
}

/// `text` with personal details replaced, as the session's files have it;
/// unchanged before a session starts.
pub(crate) fn anonymise(text: &str) -> String {
    match SESSION.get() {
        Some(s) => s.anon.apply(text).into_owned(),
        None => text.to_owned(),
    }
}

/// The session's folder, once started.
pub fn dir() -> Option<&'static Path> {
    SESSION.get().map(|s| s.dir.as_path())
}

/// The session's metadata line, and its headline facts.
fn metadata(
    build: &BuildInfo,
    started_ms: u64,
    anon: &anon::Anonymizer,
    dir: &Path,
) -> (String, Vec<(String, String)>) {
    let os = system::os();
    let cpu = system::cpu();
    let cpus = system::logical_cpus();
    let ram = system::memory_mb().map(|(total, _)| total);
    let mut settings: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k.starts_with("VICTORY_") || k.starts_with("SPARK_"))
        .map(|(k, v)| (k, anon::settings_value(&anon.apply(&v))))
        .collect();
    settings.sort();
    let mut env_obj = json::Obj::new();
    for (k, v) in &settings {
        env_obj.push_raw(k, &json::string(v));
    }
    let meta = json::Obj::new()
        .str("kind", "session")
        .int("format", 1)
        .str("started", &system::rfc3339(started_ms))
        .int("pid", u64::from(std::process::id()))
        .str("version", build.version)
        .str("git", build.git)
        .str("profile", build.profile)
        .str("os", &os)
        .str("cpu", &cpu)
        .int("logical_cpus", cpus as u64)
        .opt_num("ram_mb", ram.map(|r| r as f64))
        .str(
            "gpu",
            "see the event with spark.kind=adapter (written once the host device is made)",
        )
        .str("dir", &anon.apply(&dir.to_string_lossy()))
        .raw("env", &env_obj.finish())
        .finish();
    let settings_line = if settings.is_empty() {
        "(nenhuma)".to_owned()
    } else {
        settings
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let lines = vec![
        (
            "Versão".to_owned(),
            format!("{} (git {}, {})", build.version, build.git, build.profile),
        ),
        ("Início".to_owned(), system::rfc3339(started_ms)),
        (
            "Sistema".to_owned(),
            format!(
                "{os}; {cpu}, {cpus} CPUs lógicas; RAM {}",
                ram.map_or("?".into(), |r| format!("{r} MB"))
            ),
        ),
        ("Ajustes".to_owned(), settings_line),
    ];
    (meta, lines)
}

impl Session {
    fn now_s(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    /// The summary, waiting at most `patience` for it (a crashing thread
    /// may be the one holding it).
    fn summary_within(&self, patience: Duration) -> Option<std::sync::MutexGuard<'_, Summary>> {
        let until = Instant::now() + patience;
        loop {
            match self.summary.try_lock() {
                Ok(g) => return Some(g),
                Err(std::sync::TryLockError::Poisoned(p)) => return Some(p.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) if Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(_) => return None,
            }
        }
    }

    fn send(&self, msg: Msg) -> bool {
        let Some(tx) = self.tx.get().filter(|_| !self.stopped.load(Relaxed)) else {
            return false;
        };
        match tx.try_send(msg) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                DROPPED.fetch_add(1, Relaxed);
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    /// Waits up to `patience` for the writer to have written what is queued.
    fn flush(&self, patience: Duration) {
        let (ack, done) = std::sync::mpsc::sync_channel(1);
        if self.send(Msg::Flush(ack)) {
            let _ = done.recv_timeout(patience);
        }
    }

    /// Writes `resumo.json` and `resumo.md` (each through a temporary file,
    /// so a reader never sees half of one).
    fn write_summary(&self, patience: Duration) {
        let Some(mut summary) = self.summary_within(patience) else {
            return;
        };
        summary.dropped_events = DROPPED.load(Relaxed);
        summary.suppressed_events = SUPPRESSED.load(Relaxed);
        let now = self.now_s();
        summary.touch(now);
        let json = summary.to_json(&self.meta_json);
        let md = summary.to_markdown(&self.meta_lines);
        drop(summary);
        for (name, text) in [("resumo.json", json), ("resumo.md", md)] {
            let text = self.anon.apply(&text).into_owned();
            let tmp = self.dir.join(format!("{name}.tmp"));
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, self.dir.join(name));
            }
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>) {
        let meta = event.metadata();
        let level = match *meta.level() {
            tracing::Level::ERROR => Level::Error,
            tracing::Level::WARN => Level::Warn,
            tracing::Level::INFO => Level::Info,
            tracing::Level::DEBUG => Level::Debug,
            tracing::Level::TRACE => Level::Trace,
        };
        let to_file = self.json_level.is_some_and(|l| level <= l);
        // Info events without a mark matter to the file only.
        let marked = meta.fields().field("spark.kind").is_some();
        if !to_file && !marked && level > Level::Warn {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let now = self.now_s();
        if marked || level <= Level::Warn {
            let view = EventView {
                level,
                target: meta.target(),
                message: &fields.message,
                fields: &fields.values,
            };
            self.summary
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .event(now, &view);
        }
        if !to_file {
            return;
        }
        // Rate limit per call site (events the summary reads always pass).
        let site = meta as *const _ as usize;
        let mut earlier = 0;
        if !marked {
            let mut rates = self.rates.lock().unwrap_or_else(|e| e.into_inner());
            let entry = rates.entry(site).or_insert((Instant::now(), 0, 0));
            if entry.0.elapsed() >= RATE_WINDOW {
                earlier = entry.2;
                *entry = (Instant::now(), 0, 0);
            }
            entry.1 += 1;
            if entry.1 > RATE_EVENTS {
                entry.2 += 1;
                SUPPRESSED.fetch_add(1, Relaxed);
                return;
            }
        }
        if earlier > 0 {
            let line = self.line(
                Level::Info,
                "spark",
                "repeated events counted instead of written",
                &[
                    ("of_target", Value::Str(meta.target().to_owned())),
                    ("of_message", Value::Str(fields.message.clone())),
                    ("suppressed", Value::U64(earlier)),
                    ("window_s", Value::U64(RATE_WINDOW.as_secs())),
                ],
            );
            self.send(Msg::Line(line));
        }
        let line = self.line(level, meta.target(), &fields.message, &fields.values);
        self.send(Msg::Line(line));
    }

    /// One `eventos.jsonl` line.
    fn line(&self, level: Level, target: &str, message: &str, fields: &[(&str, Value)]) -> String {
        let now_ms = self.started_ms + self.start.elapsed().as_millis() as u64;
        let mut f = json::Obj::new();
        for (k, v) in fields {
            let v = match v {
                Value::Str(s) => json::string(&self.anon.apply(s)),
                v => v.json(),
            };
            f.push_raw(k, &v);
        }
        let mut o = json::Obj::new()
            .str("ts", &system::rfc3339(now_ms))
            .num("t", (self.now_s() * 1000.0).round() / 1000.0)
            .str("level", level.name())
            .str("target", target);
        if let Some(name) = std::thread::current().name() {
            o = o.str("thread", name);
        }
        o.str("message", &self.anon.apply(message))
            .raw("fields", &f.finish())
            .finish()
    }
}

/// An event's message and other fields.
#[derive(Default)]
struct Fields {
    message: String,
    values: Vec<(&'static str, Value)>,
}

impl Visit for Fields {
    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.values.push((field.name(), Value::F64(value)));
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.values.push((field.name(), Value::I64(value)));
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.values.push((field.name(), Value::U64(value)));
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.values.push((field.name(), Value::Bool(value)));
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_owned();
        } else {
            self.values
                .push((field.name(), Value::Str(value.to_owned())));
        }
    }
    fn record_error(
        &mut self,
        field: &tracing::field::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        self.values
            .push((field.name(), Value::Str(value.to_string())));
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let text = format!("{value:?}");
        if field.name() == "message" {
            self.message = text;
        } else {
            self.values.push((field.name(), Value::Str(text)));
        }
    }
}

/// The tracing layer that feeds the session (installed by
/// `crate::logging::init`; it does nothing until [`start`]).
pub struct SessionLayer;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SessionLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        if let Some(s) = active() {
            s.on_event(event);
        }
    }
}

/// The writer thread: lines to the file, the memory sampled every second,
/// the summary rewritten every minute.
fn writer(rx: Receiver<Msg>, mut file: Option<std::io::BufWriter<std::fs::File>>) {
    let mut sampled = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    let mut summarised = Instant::now();
    let write = |file: &mut Option<std::io::BufWriter<std::fs::File>>, line: &str| {
        if let Some(f) = file {
            let _ = f.write_all(line.as_bytes());
            let _ = f.write_all(b"\n");
        }
    };
    let flush = |file: &mut Option<std::io::BufWriter<std::fs::File>>| {
        if let Some(f) = file {
            let _ = f.flush();
        }
    };
    loop {
        let mut next = rx.recv_timeout(Duration::from_millis(250));
        loop {
            match next {
                Ok(Msg::Line(line)) => write(&mut file, &line),
                Ok(Msg::Flush(ack)) => {
                    flush(&mut file);
                    let _ = ack.send(());
                }
                Ok(Msg::Stop(ack)) => {
                    flush(&mut file);
                    if let Some(s) = SESSION.get() {
                        s.write_summary(Duration::from_secs(1));
                    }
                    let _ = ack.send(());
                    return;
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    flush(&mut file);
                    return;
                }
            }
            next = rx.try_recv().map_err(|e| match e {
                std::sync::mpsc::TryRecvError::Empty => RecvTimeoutError::Timeout,
                std::sync::mpsc::TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
            });
        }
        flush(&mut file);
        let Some(s) = SESSION.get() else { continue };
        if sampled.elapsed() >= Duration::from_secs(1) {
            sampled = Instant::now();
            if let (Some((total, free)), Some(faults)) =
                (system::memory_mb(), system::page_faults())
            {
                s.summary.lock().unwrap_or_else(|e| e.into_inner()).memory(
                    s.now_s(),
                    total,
                    free,
                    faults,
                );
            }
        }
        // A run with experiments is watched by a script that stops it, or
        // moves to the next experiment, on the summary's numbers.
        let every = if crate::ab::program().is_some() {
            SUMMARY_EVERY_AB
        } else {
            SUMMARY_EVERY
        };
        if summarised.elapsed() >= every {
            summarised = Instant::now();
            s.write_summary(Duration::from_secs(1));
        }
    }
}

/// A frame was presented; `submissions` is the run's total of host
/// submissions so far (a frame with six or more is one of the world).
/// One short lock a frame.
///
/// This is also where a run's alternating switches change arm
/// (`crate::ab`), with or without a session.
pub fn frame_presented(submissions: u64) {
    frame_presented_parts(submissions, None);
}

/// [`frame_presented`] with what the frame that ended was made of
/// ([`phases::FrameParts`]), for the summary's phases: the same one lock.
pub fn frame_presented_parts(submissions: u64, parts: Option<&phases::FrameParts>) {
    let tag = crate::ab::frame_presented();
    if let Some(s) = active() {
        let now = s.now_s();
        s.summary
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .present_parts(now, submissions, tag, parts);
    }
}

/// The frame meter's numbers for the frames presented since its last call
/// (about a second of them, every second): they mark the stable scene and
/// give its frame parts, and each arm's in a run whose switches alternate.
/// One short lock a second.
pub fn frame_window(rate: summary::Rate) {
    if let Some(s) = active() {
        let now = s.now_s();
        s.summary
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .window(now, rate);
    }
}

/// A crash that ends the run: recorded, written to `eventos.jsonl` and the
/// summary rewritten before returning (from a crash handler: it waits at
/// most about a second in all).
pub fn crash(c: Crash) {
    let Some(s) = active() else { return };
    let now = s.now_s();
    let line = s.line(
        Level::Error,
        "spark::crash",
        &format!("crash: {}", c.what),
        &[
            (
                "code",
                Value::Str(c.code.map(|c| format!("{c:#010x}")).unwrap_or_default()),
            ),
            (
                "location",
                Value::Str(c.location.clone().unwrap_or_default()),
            ),
            ("detail", Value::Str(c.detail.clone())),
            ("stack", Value::Str(c.stack.join(" < "))),
        ],
    );
    if let Some(mut summary) = s.summary_within(Duration::from_millis(300)) {
        summary.crash(now, c);
        summary.end(now, "falha");
    }
    s.send(Msg::Line(line));
    s.flush(Duration::from_millis(500));
    s.write_summary(Duration::from_millis(300));
}

/// The run ends (`reason`, said in the summary): the queue is written out
/// and the summary written a last time. Safe to call more than once.
pub fn finish(reason: &str) {
    let Some(s) = active() else { return };
    let now = s.now_s();
    if let Some(mut summary) = s.summary_within(Duration::from_millis(300)) {
        summary.end(now, reason);
    }
    let first = !s.stopped.swap(true, Relaxed);
    let stopped = first
        && s.tx.get().is_some_and(|tx| {
            let (ack, done) = std::sync::mpsc::sync_channel(1);
            // Waits for room in the queue, at most as long as the writer
            // takes to write what is before it.
            tx.send(Msg::Stop(ack)).is_ok() && done.recv_timeout(Duration::from_secs(2)).is_ok()
        });
    if !stopped {
        s.write_summary(Duration::from_millis(300));
    }
}

#[cfg(test)]
mod tests;

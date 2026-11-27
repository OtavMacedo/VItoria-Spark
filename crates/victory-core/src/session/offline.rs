//! What a run leaves so its shader and pipeline failures can be tried
//! again without the title (`SPARK_OFFLINE=1`; off unless set).
//!
//! A shader that does not translate, or whose pipeline the host refuses,
//! is known only while the title runs. With the switch on, each such
//! shader's container is written to `offline/<hash>.<stage>.cdxil` in the
//! session's folder and a line of `offline/manifesto.jsonl` says what
//! failed and why. `victory translate <folder> --pipelines --list` then
//! translates the same containers and makes pipelines on the real device:
//! a fix to the translator is checked in seconds, with no run.
//!
//! The containers are the title's own data: they stay in the session's
//! folder, outside the repository, like every other output of a run.

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use super::json::Obj;

/// The most containers and bytes one run writes; failures past them are
/// counted in the summary's tables as before, without a file.
const MAX_FILES: usize = 512;
const MAX_BYTES: u64 = 256 << 20;
/// The most manifest lines one run writes.
const MAX_LINES: usize = 4096;

/// One failure to try again offline.
pub struct Failure<'a> {
    /// `untranslated` or `refused`.
    pub kind: &'a str,
    /// The shader: the 16 hex digits logs name it by, and its stage.
    pub hash: &'a str,
    pub stage: &'a str,
    /// The container as the title gave it.
    pub container: &'a [u8],
    /// The translator's or the host's error.
    pub error: &'a str,
    /// For a refused pipeline, all of its shaders (`stage hash`, joined).
    pub pipeline: &'a str,
}

#[derive(Default)]
struct State {
    written: HashSet<String>,
    bytes: u64,
    lines: usize,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Whether failures are recorded (`SPARK_OFFLINE=1`).
pub fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARK_OFFLINE").is_some_and(|v| v == "1"))
}

/// Records `failure` in the session's folder, if recording is on and a
/// session is started. Errors writing are dropped: the run goes on.
pub fn record(failure: &Failure) {
    if !on() {
        return;
    }
    let Some(dir) = super::dir() else { return };
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let state = state.get_or_insert_with(State::default);
    let _ = record_in(&dir.join("offline"), state, failure, &super::anonymise);
}

/// [`record`] into `dir`, with `anon` applied to the error's text.
fn record_in(
    dir: &Path,
    state: &mut State,
    f: &Failure,
    anon: &dyn Fn(&str) -> String,
) -> std::io::Result<()> {
    // Hashes are 16 hex digits; anything else would be a path of its own.
    let plain = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric());
    if !plain(f.hash) || !plain(f.stage) || state.lines >= MAX_LINES {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let name = format!("{}.{}.cdxil", f.hash, f.stage);
    let mut saved = state.written.contains(&name);
    if !saved
        && state.written.len() < MAX_FILES
        && state.bytes + f.container.len() as u64 <= MAX_BYTES
    {
        std::fs::write(dir.join(&name), f.container)?;
        state.bytes += f.container.len() as u64;
        state.written.insert(name.clone());
        saved = true;
    }
    let line = Obj::new()
        .str("kind", f.kind)
        .str("hash", f.hash)
        .str("stage", f.stage)
        .int("bytes", f.container.len() as u64)
        .raw(
            "file",
            &if saved {
                super::json::string(&name)
            } else {
                "null".to_owned()
            },
        )
        .str("error", &anon(f.error))
        .str("pipeline", f.pipeline)
        .finish();
    let mut manifest = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("manifesto.jsonl"))?;
    writeln!(manifest, "{line}")?;
    state.lines += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_leaves_its_container_once_and_a_line_each_time() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::default();
        let f = Failure {
            kind: "untranslated",
            hash: "00112233aabbccdd",
            stage: "pixel",
            container: b"synthetic container",
            error: "GFX10 code at 0x40: v_foo is not supported",
            pipeline: "",
        };
        let anon = |s: &str| s.replace("v_foo", "v_bar");
        record_in(dir.path(), &mut state, &f, &anon).unwrap();
        let refused = Failure {
            kind: "refused",
            error: "E_INVALIDARG",
            pipeline: "vertex 8899aabbccddeeff + pixel 00112233aabbccdd",
            ..f
        };
        record_in(dir.path(), &mut state, &refused, &anon).unwrap();
        // A name that is not a hash writes nothing.
        let odd = Failure {
            hash: "../x",
            ..refused
        };
        record_in(dir.path(), &mut state, &odd, &anon).unwrap();

        let saved = std::fs::read(dir.path().join("00112233aabbccdd.pixel.cdxil")).unwrap();
        assert_eq!(saved, b"synthetic container");
        assert_eq!(state.written.len(), 1);
        let manifest = std::fs::read_to_string(dir.path().join("manifesto.jsonl")).unwrap();
        let lines: Vec<&str> = manifest.lines().collect();
        assert_eq!(lines.len(), 2, "{manifest}");
        assert_eq!(
            lines[0],
            r#"{"kind":"untranslated","hash":"00112233aabbccdd","stage":"pixel","bytes":19,"file":"00112233aabbccdd.pixel.cdxil","error":"GFX10 code at 0x40: v_bar is not supported","pipeline":""}"#
        );
        assert!(lines[1].contains(r#""kind":"refused""#), "{}", lines[1]);
    }

    #[test]
    fn past_the_byte_limit_only_the_line_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State {
            bytes: MAX_BYTES,
            ..State::default()
        };
        let f = Failure {
            kind: "untranslated",
            hash: "00112233aabbccdd",
            stage: "compute",
            container: b"x",
            error: "e",
            pipeline: "",
        };
        record_in(dir.path(), &mut state, &f, &|s| s.to_owned()).unwrap();
        assert!(!dir.path().join("00112233aabbccdd.compute.cdxil").exists());
        let manifest = std::fs::read_to_string(dir.path().join("manifesto.jsonl")).unwrap();
        assert!(manifest.contains(r#""file":null"#), "{manifest}");
    }
}

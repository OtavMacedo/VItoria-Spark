//! The descriptions the title passes to the two console pipeline methods
//! not written yet (device methods 0x1c8 and 0x218), kept so that their
//! layout can be worked out away from the title (`SPARK_PIPELINES_X=1`;
//! off unless set).
//!
//! Each distinct description is written to `pipelines-x/` in the session's
//! folder as its raw bytes, with every block it names by an address and a
//! size beside it, and a line of `pipelines-x/manifesto.jsonl` that lists
//! the description's words and where each block came from. The bytes are
//! the title's own data: they stay in the session's folder, outside the
//! repository, like every other output of a run.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use super::json::{Obj, array, string};

/// The most descriptions kept per method; later ones are not written.
const MAX_PER_SLOT: usize = 48;
/// The most bytes one run writes here.
const MAX_BYTES: u64 = 64 << 20;

/// A block a description names: the word holding its address, the address
/// in title memory and the bytes read there.
pub struct Block<'a> {
    pub word: usize,
    pub address: u64,
    pub bytes: &'a [u8],
}

#[derive(Default)]
struct State {
    seen: HashSet<(u16, u64)>,
    per_slot: Vec<(u16, usize)>,
    bytes: u64,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Whether descriptions are kept (`SPARK_PIPELINES_X=1`).
pub fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARK_PIPELINES_X").is_some_and(|v| v == "1"))
}

/// Keeps `desc` (the description's bytes as read) and its `blocks`, once
/// per distinct description of method `slot`, if keeping is on, a session
/// is started and its folder is outside the source tree. `calls`: how many
/// times the method was called so far. Errors writing are dropped: the run
/// goes on.
pub fn record(slot: u16, calls: u32, desc: &[u8], blocks: &[Block]) {
    if !on() {
        return;
    }
    let Some(dir) = super::dir() else { return };
    if crate::policy::ensure_outside_repo(dir).is_err() {
        return;
    }
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let state = state.get_or_insert_with(State::default);
    let _ = record_in(&dir.join("pipelines-x"), state, slot, calls, desc, blocks);
}

/// [`record`] into `dir`.
fn record_in(
    dir: &Path,
    state: &mut State,
    slot: u16,
    calls: u32,
    desc: &[u8],
    blocks: &[Block],
) -> std::io::Result<()> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    desc.hash(&mut hasher);
    if !state.seen.insert((slot, hasher.finish())) {
        return Ok(());
    }
    let n = match state.per_slot.iter_mut().find(|(s, _)| *s == slot) {
        Some((_, n)) => n,
        None => {
            state.per_slot.push((slot, 0));
            &mut state.per_slot.last_mut().expect("just pushed").1
        }
    };
    let size: u64 = desc.len() as u64 + blocks.iter().map(|b| b.bytes.len() as u64).sum::<u64>();
    if *n >= MAX_PER_SLOT || state.bytes + size > MAX_BYTES {
        return Ok(());
    }
    let index = *n;
    *n += 1;
    state.bytes += size;
    std::fs::create_dir_all(dir)?;
    let stem = format!("{slot:x}-{index:02}");
    std::fs::write(dir.join(format!("{stem}.desc")), desc)?;
    let words = desc.chunks_exact(8).map(|w| {
        string(&format!(
            "{:#x}",
            u64::from_le_bytes(w.try_into().expect("8 bytes"))
        ))
    });
    let mut listed = Vec::new();
    for block in blocks {
        let name = format!("{stem}-w{:02}.bin", block.word);
        std::fs::write(dir.join(&name), block.bytes)?;
        listed.push(
            Obj::new()
                .int("word", block.word as u64)
                .str("address", &format!("{:#x}", block.address))
                .int("bytes", block.bytes.len() as u64)
                .str("file", &name)
                .finish(),
        );
    }
    let line = Obj::new()
        .str("slot", &format!("{slot:#x}"))
        .int("index", index as u64)
        .int("calls", u64::from(calls))
        .str("desc", &format!("{stem}.desc"))
        .raw("words", &array(words))
        .raw("blocks", &array(listed))
        .finish();
    let mut manifest = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("manifesto.jsonl"))?;
    writeln!(manifest, "{line}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc(words: &[u64]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn a_description_is_kept_once_with_its_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::default();
        let d = desc(&[0x1000, 0x2000, 4]);
        let blocks = [Block {
            word: 1,
            address: 0x2000,
            bytes: b"code",
        }];
        record_in(dir.path(), &mut state, 0x218, 1, &d, &blocks).unwrap();
        record_in(dir.path(), &mut state, 0x218, 2, &d, &blocks).unwrap();
        record_in(dir.path(), &mut state, 0x1c8, 3, &d, &[]).unwrap();

        assert_eq!(std::fs::read(dir.path().join("218-00.desc")).unwrap(), d);
        assert_eq!(
            std::fs::read(dir.path().join("218-00-w01.bin")).unwrap(),
            b"code"
        );
        assert!(dir.path().join("1c8-00.desc").exists());
        let manifest = std::fs::read_to_string(dir.path().join("manifesto.jsonl")).unwrap();
        let lines: Vec<&str> = manifest.lines().collect();
        assert_eq!(lines.len(), 2, "{manifest}");
        assert_eq!(
            lines[0],
            r#"{"slot":"0x218","index":0,"calls":1,"desc":"218-00.desc","words":["0x1000","0x2000","0x4"],"blocks":[{"word":1,"address":"0x2000","bytes":4,"file":"218-00-w01.bin"}]}"#
        );
        assert!(
            lines[1].starts_with(r#"{"slot":"0x1c8","index":0,"calls":3"#),
            "{}",
            lines[1]
        );
    }

    #[test]
    fn past_the_limits_nothing_more_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::default();
        for i in 0..MAX_PER_SLOT as u64 + 2 {
            record_in(dir.path(), &mut state, 0x218, 1, &desc(&[i]), &[]).unwrap();
        }
        let manifest = std::fs::read_to_string(dir.path().join("manifesto.jsonl")).unwrap();
        assert_eq!(manifest.lines().count(), MAX_PER_SLOT);

        let mut full = State {
            bytes: MAX_BYTES,
            ..State::default()
        };
        record_in(dir.path(), &mut full, 0x1c8, 1, &desc(&[1]), &[]).unwrap();
        assert!(!dir.path().join("1c8-00.desc").exists());
    }
}

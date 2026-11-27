//! Switches that alternate inside one run, so that the run measures a
//! setting both ways in the same scene (`SPARK_AB`).
//!
//! Two runs of the same build differ by more than most changes are worth:
//! the scene, the memory left and the card's state are never the same
//! twice. Blocks of one run, taken in turn, share all of that, and the
//! session's summary reports the two populations side by side.
//!
//! `SPARK_AB=NAME[=a/b][+NAME[=a/b]...][:seconds]` names the switches and
//! the length of a block (20 s unless said, 2 to 600). Without values a
//! switch is `0` in arm A and `1` in arm B. `NAME=alt` in the environment
//! is `SPARK_AB=NAME` for a switch of the table given to [`check`]; any
//! other switch set to `alt` keeps what its own code does with that value.
//!
//! The arm changes when a frame is presented, never inside the presenter's
//! frame. Work of the next frame already under way on other threads sees
//! the change midway, so only a switch whose every use decides on its own,
//! with nothing kept from the value before, may be a [`Toggle`].
//!
//! Blocks follow the Thue-Morse sequence (A B B A B A A B ...): blocks 2j
//! and 2j+1 are always one of each arm, and a slow drift of the scene
//! falls on both arms alike.
//!
//! `*` between switches makes each side a factor of its own
//! (`SPARK_AB=NAME*OTHER+THIRD*FOURTH` has three: `OTHER` and `THIRD`
//! change together). Every block then runs one cell of the factorial, a
//! level per factor ([`cell_of`]), so one run measures every factor on
//! all of its blocks and shows whether two of them interact. Up to
//! [`MAX_FACTORS`].
//!
//! `SPARK_PLAN=<file.toml>` runs several experiments, one after the other,
//! in one run (the title's start and loading paid once):
//!
//! ```toml
//! periodo_s = 20
//! [[experimento]]
//! nome = "merge"
//! ab = "VICTORY_WIDE_MERGE"
//! [[experimento]]
//! nome = "aa"
//! ab = "VICTORY_STEP_INDEX=0/0"
//! ```
//!
//! The experiment in force is the number in `<file.toml>.atual` (0 when it
//! is missing), read about once a second: the script that watches the run
//! writes the next one when the current has decided. A new experiment
//! starts with one block of arm A left out of the numbers (a washout of
//! what the last one left behind), then its own Thue-Morse blocks.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// The block length without one in `SPARK_AB`, seconds.
pub const DEFAULT_PERIOD_S: f64 = 20.0;
/// Shortest and longest block, seconds.
const PERIOD_RANGE_S: (f64, f64) = (2.0, 600.0);
/// The share of a block after a change of arm left out of the numbers
/// (caches and queues filled under the other arm run out in it).
const SETTLE_SHARE: f64 = 0.1;
/// The least time left out after a change of arm, seconds.
const SETTLE_MIN_S: f64 = 1.0;
/// The most factors of one plan: 32 cells, each seen once in 32 blocks.
pub const MAX_FACTORS: usize = 5;

/// One switch of a plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Switch {
    /// The environment variable's name.
    pub name: String,
    /// Its value in arm A and in arm B.
    pub values: [String; 2],
    /// The factor it belongs to (0 in a plan of one).
    pub factor: u8,
}

/// What alternates and how fast.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// A block's length, seconds.
    pub period_s: f64,
    /// The switches, each with its two values.
    pub switches: Vec<Switch>,
    /// How many factors the switches make up, 1 to [`MAX_FACTORS`].
    pub factors: u8,
}

impl Plan {
    /// Reads `NAME[=a/b][+NAME[=a/b]...][*NAME...][:seconds]`. The error
    /// says what is wrong, for the person who wrote the variable.
    pub fn parse(spec: &str) -> Result<Plan, String> {
        let spec = spec.trim();
        let (names, period_s) = match spec.rsplit_once(':') {
            Some((names, seconds)) => {
                let s: f64 = seconds
                    .trim()
                    .parse()
                    .map_err(|_| format!("SPARK_AB: `{seconds}` is not a number of seconds"))?;
                if !(PERIOD_RANGE_S.0..=PERIOD_RANGE_S.1).contains(&s) {
                    return Err(format!(
                        "SPARK_AB: a block of {s} s is outside {} to {} s",
                        PERIOD_RANGE_S.0, PERIOD_RANGE_S.1
                    ));
                }
                (names, s)
            }
            None => (spec, DEFAULT_PERIOD_S),
        };
        let mut switches = Vec::new();
        let mut factors = 0u8;
        for group in names.split('*') {
            if usize::from(factors) == MAX_FACTORS {
                return Err(format!(
                    "SPARK_AB: more than {MAX_FACTORS} factors (`*`) in one plan"
                ));
            }
            for part in group.split(['+', ',']).map(str::trim) {
                let (name, values) = match part.split_once('=') {
                    Some((name, values)) => {
                        let (a, b) = values.split_once('/').ok_or_else(|| {
                            format!("SPARK_AB: `{part}` needs two values, as in NAME=a/b")
                        })?;
                        (name.trim(), [a.trim().to_owned(), b.trim().to_owned()])
                    }
                    None => (part, ["0".to_owned(), "1".to_owned()]),
                };
                let well_formed = !name.is_empty()
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
                if !well_formed {
                    return Err(format!("SPARK_AB: `{name}` is not a switch's name"));
                }
                if switches.iter().any(|s: &Switch| s.name == name) {
                    return Err(format!("SPARK_AB: `{name}` is named twice"));
                }
                switches.push(Switch {
                    name: name.to_owned(),
                    values,
                    factor: factors,
                });
            }
            factors += 1;
        }
        Ok(Plan {
            period_s,
            switches,
            factors,
        })
    }

    /// The time left out of the numbers after a change of arm, seconds.
    pub fn settle_s(&self) -> f64 {
        (self.period_s * SETTLE_SHARE).max(SETTLE_MIN_S)
    }

    /// The block that holds `t_s` (seconds since the first frame).
    pub fn block_at(&self, t_s: f64) -> u32 {
        (t_s.max(0.0) / self.period_s) as u32
    }

    /// What cell `cell` sets, as `NAME=value` joined by spaces: bit `f`
    /// of the cell is factor `f`'s level, so in a plan of one factor the
    /// cell is the arm (0 or 1).
    pub fn label(&self, cell: usize) -> String {
        let parts: Vec<String> = self
            .switches
            .iter()
            .map(|s| format!("{}={}", s.name, s.values[(cell >> s.factor) & 1]))
            .collect();
        parts.join(" ")
    }

    /// Factor `factor`'s switches with both values, as `NAME=a/b` joined
    /// by `+`.
    pub fn factor_label(&self, factor: u8) -> String {
        let parts: Vec<String> = self
            .switches
            .iter()
            .filter(|s| s.factor == factor)
            .map(|s| format!("{}={}/{}", s.name, s.values[0], s.values[1]))
            .collect();
        parts.join("+")
    }

    /// The cell of block `block`.
    pub fn cell_at(&self, block: u32) -> u8 {
        cell_of(block, self.factors)
    }
}

/// The arm (0 or 1) of block `block`: its place in the Thue-Morse sequence.
pub fn arm_of(block: u32) -> u8 {
    (block.count_ones() & 1) as u8
}

/// The cell of block `block` in a plan of `factors` factors: bit `f` is
/// the Thue-Morse arm of `block >> f`. Factor 0 alternates as a plan of
/// one does, and every run of `2^factors` blocks that starts at a multiple
/// of that holds each cell once, so the factors are balanced against one
/// another and each against a slow drift.
pub fn cell_of(block: u32, factors: u8) -> u8 {
    (0..factors.clamp(1, MAX_FACTORS as u8)).fold(0, |cell, f| cell | arm_of(block >> f) << f)
}

/// What a presented frame ran under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tag {
    /// The experiment in force (0 for `SPARK_AB`).
    pub exp: usize,
    /// The block the frame began in.
    pub block: u32,
    /// That block's cell ([`cell_of`]): the arm, 0 or 1, in a plan of
    /// one factor.
    pub arm: u8,
    /// Whether the frame began after the settling time of the last change.
    pub settled: bool,
}

/// The plan's clock: which experiment and block are in force and since
/// when. Plain data fed with the time, so tests drive it without a run.
#[derive(Debug, Default)]
pub struct Clock {
    /// The experiment in force.
    exp: usize,
    /// When its block 0 starts, seconds; before it, its washout.
    epoch_s: f64,
    /// The block in force.
    block: u32,
    /// When the arm last changed, seconds.
    changed_s: f64,
    /// When the last frame was presented, seconds.
    last_s: f64,
}

impl Clock {
    /// A frame was presented at `now_s`: the tag of the frame that ended,
    /// and the arm in force from now if it changed.
    pub fn present(&mut self, plan: &Plan, now_s: f64) -> (Tag, Option<u8>) {
        self.present_in(plan, now_s, self.exp)
    }

    /// [`Clock::present`] with experiment `exp` asked for: when it is not
    /// the one in force, it starts now, with a block of washout in arm A.
    pub fn present_in(&mut self, plan: &Plan, now_s: f64, exp: usize) -> (Tag, Option<u8>) {
        let washout = self.last_s < self.epoch_s;
        let tag = Tag {
            exp: self.exp,
            block: self.block,
            arm: if washout { 0 } else { plan.cell_at(self.block) },
            settled: !washout
                && (self.last_s - self.changed_s >= plan.settle_s() || self.block == 0),
        };
        self.last_s = now_s;
        if exp != self.exp {
            let before = tag.arm;
            (self.exp, self.epoch_s, self.block, self.changed_s) =
                (exp, now_s + plan.period_s, 0, now_s);
            return (tag, (before != 0).then_some(0));
        }
        if now_s < self.epoch_s {
            return (tag, None);
        }
        let block = plan.block_at(now_s - self.epoch_s);
        let mut change = None;
        if block != self.block {
            if plan.cell_at(block) != plan.cell_at(self.block) {
                self.changed_s = now_s;
                change = Some(plan.cell_at(block));
            }
            self.block = block;
        }
        (tag, change)
    }

    /// The experiment in force.
    pub fn exp(&self) -> usize {
        self.exp
    }
}

/// The cell in force, read by every [`Toggle`] that alternates.
static ARM: AtomicU8 = AtomicU8::new(0);
/// The experiment in force (`SPARK_PLAN`; 0 for `SPARK_AB`).
static EXP: AtomicUsize = AtomicUsize::new(0);

/// The experiments of a run: one for `SPARK_AB`, the list of `SPARK_PLAN`.
#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    /// Each experiment's name and plan; every plan has the program's block.
    pub experiments: Vec<(String, Plan)>,
    /// The file whose number is the experiment in force (`SPARK_PLAN` only).
    pub control: Option<PathBuf>,
}

/// `SPARK_PLAN`'s file.
#[derive(serde::Deserialize)]
struct PlanFile {
    periodo_s: Option<f64>,
    experimento: Vec<PlanEntry>,
}

#[derive(serde::Deserialize)]
struct PlanEntry {
    nome: String,
    ab: String,
}

impl Program {
    /// Reads a `SPARK_PLAN` file's text; `path` is where it lies (its
    /// control file is beside it).
    pub fn parse(text: &str, path: &std::path::Path) -> Result<Program, String> {
        let file: PlanFile =
            toml::from_str(text).map_err(|e| format!("SPARK_PLAN: {}: {e}", path.display()))?;
        let period_s = file.periodo_s.unwrap_or(DEFAULT_PERIOD_S);
        if !(PERIOD_RANGE_S.0..=PERIOD_RANGE_S.1).contains(&period_s) {
            return Err(format!(
                "SPARK_PLAN: a block of {period_s} s is outside {} to {} s",
                PERIOD_RANGE_S.0, PERIOD_RANGE_S.1
            ));
        }
        if file.experimento.is_empty() {
            return Err("SPARK_PLAN: no [[experimento]]".to_owned());
        }
        let mut experiments = Vec::new();
        for e in file.experimento {
            let mut plan =
                Plan::parse(&e.ab).map_err(|m| format!("{m} (experiment {})", e.nome))?;
            plan.period_s = period_s;
            experiments.push((e.nome, plan));
        }
        let mut control = path.as_os_str().to_owned();
        control.push(".atual");
        Ok(Program {
            experiments,
            control: Some(PathBuf::from(control)),
        })
    }

    /// The experiment `exp`'s plan, if there is one.
    pub fn plan(&self, exp: usize) -> Option<&Plan> {
        self.experiments.get(exp).map(|(_, p)| p)
    }
}

/// The run's experiments: `SPARK_PLAN`, else `SPARK_AB` with the switches
/// of the table set to `alt` added. Read once, on first use: [`check`]
/// comes before it. A malformed one is none here; [`check`] reports it.
pub fn program() -> Option<&'static Program> {
    static PROGRAM: OnceLock<Option<Program>> = OnceLock::new();
    PROGRAM
        .get_or_init(|| {
            if let Some(path) = std::env::var_os("SPARK_PLAN") {
                let path = PathBuf::from(path);
                let text = std::fs::read_to_string(&path).ok()?;
                return Program::parse(&text, &path).ok();
            }
            let table = TABLE.get().map_or(&[][..], Vec::as_slice);
            from_env(std::env::vars(), table).map(|plan| Program {
                experiments: vec![("ab".to_owned(), plan)],
                control: None,
            })
        })
        .as_ref()
}

/// The names of the switches that may alternate, as [`check`] was given
/// them. Empty until then.
static TABLE: OnceLock<Vec<&'static str>> = OnceLock::new();

/// The plan of the experiment in force, if the run has experiments.
pub fn plan() -> Option<&'static Plan> {
    program().and_then(|p| p.plan(EXP.load(Relaxed)))
}

/// The plan a set of environment variables asks for; a switch set to
/// `alt` joins it only if `table` names it. A malformed `SPARK_AB` gives
/// none here; [`check`] is what reports it.
fn from_env(vars: impl Iterator<Item = (String, String)>, table: &[&str]) -> Option<Plan> {
    let mut plan: Option<Plan> = None;
    let mut alt = Vec::new();
    for (name, value) in vars {
        if name == "SPARK_AB" {
            plan = Plan::parse(&value).ok();
        } else if value.trim() == "alt" && table.contains(&name.as_str()) {
            alt.push(name);
        }
    }
    alt.sort();
    if alt.is_empty() {
        return plan;
    }
    let mut plan = plan.unwrap_or(Plan {
        period_s: DEFAULT_PERIOD_S,
        switches: Vec::new(),
        factors: 1,
    });
    for name in alt {
        if !plan.switches.iter().any(|s| s.name == name) {
            plan.switches.push(Switch {
                name,
                values: ["0".to_owned(), "1".to_owned()],
                factor: 0,
            });
        }
    }
    Some(plan)
}

/// Checks the run's plan against `table`, the switches that may alternate,
/// and keeps the table for [`plan`]. Called once, before any switch is
/// read. An error is for the person: `SPARK_AB` is malformed, or names a
/// switch that is not in the table. The caller refuses to start the title
/// then, since the run would measure nothing.
pub fn check(table: &[&Toggle]) -> Result<(), String> {
    let _ = TABLE.set(table.iter().map(|t| t.name).collect());
    if let Some(path) = std::env::var_os("SPARK_PLAN") {
        let path = PathBuf::from(path);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("SPARK_PLAN: {}: {e}", path.display()))?;
        let program = Program::parse(&text, &path)?;
        for (_, plan) in &program.experiments {
            check_plan(plan, table)?;
        }
        return Ok(());
    }
    match std::env::var("SPARK_AB") {
        Ok(spec) => check_spec(&spec, table),
        Err(_) => Ok(()),
    }
}

/// [`check`] for the text of `SPARK_AB`.
fn check_spec(spec: &str, table: &[&Toggle]) -> Result<(), String> {
    check_plan(&Plan::parse(spec)?, table)
}

/// Every switch of `plan` is in `table`.
fn check_plan(plan: &Plan, table: &[&Toggle]) -> Result<(), String> {
    for s in &plan.switches {
        if !table.iter().any(|t| t.name == s.name) {
            let mut known: Vec<&str> = table.iter().map(|t| t.name).collect();
            known.sort_unstable();
            return Err(format!(
                "SPARK_AB: {} cannot alternate inside a run (it is read once, or keeps state from its value). Switches that can: {}",
                s.name,
                known.join(", ")
            ));
        }
    }
    Ok(())
}

/// A frame was presented: the tag of the frame that ended, if a plan is
/// in force. Changes the arm for the frames to come when a block ends.
/// Called once a frame, by the presenter.
pub fn frame_presented() -> Option<Tag> {
    /// The clock, the run's start, and when the control file was last read.
    static CLOCK: Mutex<Option<(Instant, Clock, Instant, usize)>> = Mutex::new(None);
    let program = program()?;
    let mut guard = CLOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (epoch, clock, read, wanted) = guard.get_or_insert_with(|| {
        let now = Instant::now();
        (now, Clock::default(), now, 0)
    });
    if let Some(control) = &program.control
        && read.elapsed().as_secs_f64() >= 1.0
    {
        *read = Instant::now();
        if let Some(n) = std::fs::read_to_string(control)
            .ok()
            .and_then(|t| t.trim().parse::<usize>().ok())
        {
            *wanted = n;
        }
    }
    // Past the last experiment: every switch at its own value, as without
    // a plan, until the run ends.
    let plan = program
        .plan(*wanted)
        .or_else(|| program.plan(clock.exp()))?;
    let (tag, change) = clock.present_in(plan, epoch.elapsed().as_secs_f64(), *wanted);
    if clock.exp() != EXP.load(Relaxed) {
        EXP.store(clock.exp(), Relaxed);
        tracing::info!(
            spark.kind = "ab_experiment",
            spark.exp = clock.exp() as u64,
            "experiment {} of the run (SPARK_PLAN) from now: {}",
            clock.exp(),
            program
                .experiments
                .get(clock.exp())
                .map_or("none, the switches at their own values", |(name, _)| name)
        );
    }
    if let Some(arm) = change {
        ARM.store(arm, Relaxed);
        tracing::info!(
            spark.kind = "ab_arm",
            spark.arm = u64::from(arm),
            spark.block = u64::from(clock.block),
            "A/B inside the run: {} from now: {}",
            if plan.factors > 1 {
                format!("cell {arm}")
            } else {
                format!("arm {}", ["A", "B"][usize::from(arm & 1)])
            },
            plan.label(usize::from(arm))
        );
    }
    Some(tag)
}

/// The cell in force (0 for A, 1 for B in a plan of one factor) when the
/// run has experiments, else `None`: what a capture of the frame is
/// labelled with.
pub fn arm() -> Option<u8> {
    plan().map(|_| ARM.load(Relaxed))
}

/// A toggle's values, read once.
#[derive(Clone, Debug)]
enum State {
    /// The same for the whole run.
    Fixed(u64),
    /// Per experiment, one per level with the switch's factor, or `None`
    /// where the experiment leaves the switch alone; and the value it has
    /// then.
    Arms(Vec<Option<([u64; 2], u8)>>, u64),
}

/// A switch of the environment that may alternate inside a run. Its value
/// is read once; with the switch in the run's plan, both arms' values are,
/// and [`Toggle::get`] gives the one of the arm in force.
pub struct Toggle {
    name: &'static str,
    parse: fn(Option<&str>) -> u64,
    state: OnceLock<State>,
}

/// A flag's reading: `1` is on, anything else and unset are off.
fn flag(value: Option<&str>) -> u64 {
    u64::from(value.is_some_and(|v| v.trim() == "1"))
}

impl Toggle {
    /// A flag: on (1) with the value `1`, off (0) otherwise.
    pub const fn flag(name: &'static str) -> Toggle {
        Toggle::new(name, flag)
    }

    /// A switch whose value `parse` reads; `None` is the variable unset.
    pub const fn new(name: &'static str, parse: fn(Option<&str>) -> u64) -> Toggle {
        Toggle {
            name,
            parse,
            state: OnceLock::new(),
        }
    }

    /// The environment variable's name.
    pub fn name(&self) -> &'static str {
        self.name
    }

    fn state(&self) -> &State {
        self.state.get_or_init(|| {
            let own = (self.parse)(std::env::var(self.name).ok().as_deref());
            let Some(program) = program() else {
                return State::Fixed(own);
            };
            let per_exp: Vec<Option<([u64; 2], u8)>> = program
                .experiments
                .iter()
                .map(|(_, plan)| {
                    let s = plan.switches.iter().find(|s| s.name == self.name)?;
                    Some((
                        [
                            (self.parse)(Some(&s.values[0])),
                            (self.parse)(Some(&s.values[1])),
                        ],
                        s.factor,
                    ))
                })
                .collect();
            if per_exp.iter().all(Option::is_none) {
                State::Fixed(own)
            } else {
                State::Arms(per_exp, own)
            }
        })
    }

    /// The value in force.
    pub fn get(&self) -> u64 {
        match self.state() {
            State::Fixed(v) => *v,
            State::Arms(per_exp, own) => match per_exp.get(EXP.load(Relaxed)) {
                Some(Some((v, factor))) => v[usize::from(ARM.load(Relaxed) >> factor & 1)],
                _ => *own,
            },
        }
    }

    /// Whether the value in force is not zero.
    pub fn on(&self) -> bool {
        self.get() != 0
    }

    /// Whether the value is not zero at some time of the run: what a
    /// one-time preparation of the switch's path asks.
    pub fn ever(&self) -> bool {
        match self.state() {
            State::Fixed(v) => *v != 0,
            State::Arms(per_exp, own) => {
                *own != 0
                    || per_exp
                        .iter()
                        .flatten()
                        .any(|(v, _)| v[0] != 0 || v[1] != 0)
            }
        }
    }

    /// Whether the run's experiments alternate this switch.
    pub fn alternates(&self) -> bool {
        matches!(self.state(), State::Arms(..))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_reads_names_values_and_seconds() {
        let p = Plan::parse("VICTORY_STEP_INDEX").unwrap();
        assert_eq!(p.period_s, DEFAULT_PERIOD_S);
        assert_eq!(p.label(0), "VICTORY_STEP_INDEX=0");
        assert_eq!(p.label(1), "VICTORY_STEP_INDEX=1");

        let p =
            Plan::parse("VICTORY_TEXTURE_ROOM_PERCENT=80/94 + VICTORY_STEP_INDEX : 30").unwrap();
        assert_eq!(p.period_s, 30.0);
        assert_eq!(p.switches.len(), 2);
        assert_eq!(
            p.label(1),
            "VICTORY_TEXTURE_ROOM_PERCENT=94 VICTORY_STEP_INDEX=1"
        );
        assert_eq!(p.settle_s(), 3.0);
    }

    #[test]
    fn a_malformed_plan_says_why() {
        for bad in [
            "",
            "victory_x",
            "VICTORY_X:abc",
            "VICTORY_X:1",
            "VICTORY_X:601",
            "VICTORY_X=1",
            "VICTORY_X+VICTORY_X",
        ] {
            let e = Plan::parse(bad).unwrap_err();
            assert!(e.starts_with("SPARK_AB: "), "{bad}: {e}");
        }
    }

    #[test]
    fn factors_are_read_and_their_cells_are_balanced() {
        let p = Plan::parse("VICTORY_A * VICTORY_B=80/94 + VICTORY_C * VICTORY_D : 10").unwrap();
        assert_eq!(p.factors, 3);
        assert_eq!(p.factor_label(1), "VICTORY_B=80/94+VICTORY_C=0/1");
        // Cell 0b110: factor 0 at its first level, factors 1 and 2 at their second.
        assert_eq!(
            p.label(0b110),
            "VICTORY_A=0 VICTORY_B=94 VICTORY_C=1 VICTORY_D=1"
        );
        assert!(Plan::parse("A*B*C*D*E*F").is_err());
        for factors in 1..=MAX_FACTORS as u8 {
            let cells = 1u32 << factors;
            for start in [0, cells, 7 * cells] {
                let mut seen: Vec<u8> = (start..start + cells)
                    .map(|b| cell_of(b, factors))
                    .collect();
                seen.sort_unstable();
                let all: Vec<u8> = (0..cells as u8).collect();
                assert_eq!(seen, all, "{factors} factors from block {start}");
            }
        }
        // Factor 0 is the arm of a plan of one.
        for block in 0..64 {
            assert_eq!(cell_of(block, 4) & 1, arm_of(block));
            assert_eq!(cell_of(block, 1), arm_of(block));
        }
    }

    #[test]
    fn neighbouring_blocks_are_one_of_each_arm() {
        for j in 0..1000u32 {
            assert_ne!(arm_of(2 * j), arm_of(2 * j + 1), "pair {j}");
        }
        let first: Vec<u8> = (0..8).map(arm_of).collect();
        assert_eq!(first, [0, 1, 1, 0, 1, 0, 0, 1]);
    }

    #[test]
    fn the_arm_changes_at_a_present_and_settles() {
        let plan = Plan::parse("VICTORY_X:10").unwrap();
        let mut clock = Clock::default();
        let mut changes = Vec::new();
        let mut unsettled = 0;
        // A frame every quarter of a second for four blocks.
        for i in 1..160 {
            let now = f64::from(i) * 0.25;
            let (tag, change) = clock.present(&plan, now);
            // The frame that ended began a quarter of a second before.
            let began = now - 0.25;
            assert_eq!(tag.arm, arm_of(tag.block));
            // A frame is of the block in force when it began, which is the
            // block of its start or, for the first frame after a boundary
            // that fell inside the frame before, the one before.
            assert!(tag.block == plan.block_at(began) || tag.block + 1 == plan.block_at(began));
            unsettled += u32::from(!tag.settled);
            if let Some(arm) = change {
                changes.push((now, arm));
            }
        }
        // Blocks 0 to 3 are A B B A: two changes, none between blocks 1 and 2.
        assert_eq!(changes, [(10.0, 1), (30.0, 0)]);
        // A second after each change: four frames each.
        assert_eq!(unsettled, 8);
    }

    #[test]
    fn a_new_experiment_starts_with_a_washout_in_arm_a() {
        let plan = Plan::parse("VICTORY_X:10").unwrap();
        let mut clock = Clock::default();
        // Experiment 0 to 15 s: blocks 0 (A) and 1 (B).
        for i in 1..=60 {
            clock.present_in(&plan, f64::from(i) * 0.25, 0);
        }
        assert_eq!((clock.exp(), clock.block), (0, 1));
        // Experiment 1 asked at 15.25 s: back to A at once.
        let (tag, change) = clock.present_in(&plan, 15.25, 1);
        assert_eq!((tag.exp, tag.arm), (0, 1));
        assert_eq!(change, Some(0));
        // Its washout to 25.25 s: arm A, left out.
        let mut washout = 0;
        let mut first_settled = None;
        for i in 62..=200 {
            let now = f64::from(i) * 0.25;
            let (tag, _) = clock.present_in(&plan, now, 1);
            assert_eq!(tag.exp, 1);
            if !tag.settled && tag.block == 0 {
                washout += 1;
            } else if first_settled.is_none() {
                first_settled = Some(now);
            }
        }
        assert_eq!(washout, 40, "a block of 10 s at four frames a second");
        assert!(first_settled.is_some_and(|t| (25.4..25.6).contains(&t)));
        // Then its own blocks from 0: A, B at 35.25 s.
        assert_eq!(clock.block, 2);
    }

    #[test]
    fn a_plan_file_lists_experiments() {
        let text = "periodo_s = 15\n[[experimento]]\nnome = \"um\"\nab = \"VICTORY_A\"\n\
                    [[experimento]]\nnome = \"dois\"\nab = \"VICTORY_B=2/3+VICTORY_C\"\n";
        let p = Program::parse(text, std::path::Path::new("plano.toml")).unwrap();
        assert_eq!(p.experiments.len(), 2);
        assert_eq!(p.experiments[1].0, "dois");
        assert_eq!(p.plan(1).unwrap().period_s, 15.0);
        assert_eq!(p.plan(1).unwrap().label(1), "VICTORY_B=3 VICTORY_C=1");
        assert_eq!(
            p.control.unwrap(),
            std::path::PathBuf::from("plano.toml.atual")
        );
        assert!(
            Program::parse(
                "periodo_s = 1\n[[experimento]]\nnome=\"x\"\nab=\"VICTORY_A\"",
                std::path::Path::new("p")
            )
            .is_err()
        );
        assert!(
            Program::parse(
                "[[experimento]]\nnome=\"x\"\nab=\"bad\"",
                std::path::Path::new("p")
            )
            .is_err()
        );
    }

    #[test]
    fn switches_set_to_alt_join_the_plan() {
        let vars = |v: &[(&str, &str)]| {
            v.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect::<Vec<_>>()
                .into_iter()
        };
        let table = ["VICTORY_A", "VICTORY_B"];
        assert_eq!(
            from_env(vars(&[("VICTORY_A", "1"), ("PATH", "alt")]), &table),
            None
        );
        // A switch outside the table keeps its own `alt`.
        assert_eq!(from_env(vars(&[("VICTORY_D", "alt")]), &table), None);
        let p = from_env(
            vars(&[
                ("VICTORY_B", "alt"),
                ("VICTORY_A", "alt"),
                ("VICTORY_D", "alt"),
            ]),
            &table,
        )
        .unwrap();
        assert_eq!(p.label(1), "VICTORY_A=1 VICTORY_B=1");
        assert_eq!(p.period_s, DEFAULT_PERIOD_S);
        let p = from_env(
            vars(&[("SPARK_AB", "VICTORY_C=2/3:40"), ("VICTORY_A", "alt")]),
            &table,
        )
        .unwrap();
        assert_eq!(p.label(0), "VICTORY_C=2 VICTORY_A=0");
        assert_eq!(p.period_s, 40.0);
    }

    #[test]
    fn a_toggle_outside_the_plan_reads_its_variable_once() {
        // No variable of these names is set by anything.
        static FLAG: Toggle = Toggle::flag("VICTORY_AB_TEST_UNSET_FLAG");
        static NUMBER: Toggle = Toggle::new("VICTORY_AB_TEST_UNSET_NUMBER", |v| {
            v.and_then(|v| v.parse().ok()).unwrap_or(90)
        });
        assert!(!FLAG.on() && !FLAG.ever() && !FLAG.alternates());
        assert_eq!(NUMBER.get(), 90);
        assert!(NUMBER.ever());
        assert_eq!(flag(Some(" 1 ")), 1);
        assert_eq!(flag(Some("alt")), 0);
    }

    #[test]
    fn a_plan_naming_a_switch_outside_the_table_is_refused() {
        static KNOWN: Toggle = Toggle::flag("VICTORY_KNOWN");
        let table: &[&Toggle] = &[&KNOWN];
        assert_eq!(check_spec("VICTORY_KNOWN:30", table), Ok(()));
        let e = check_spec("VICTORY_KNOWN+VICTORY_OTHER", table).unwrap_err();
        assert!(
            e.contains("VICTORY_OTHER cannot alternate") && e.ends_with("VICTORY_KNOWN"),
            "{e}"
        );
        assert!(check_spec("VICTORY_KNOWN:0", table).is_err());
    }
}

//! The two populations of a run whose switches alternate (`SPARK_AB`,
//! [`crate::ab`]): frame lengths and frame parts per arm, and whether the
//! difference between the arms is more than the run's own noise.
//!
//! The verdict comes from pairs of neighbouring blocks, one of each arm:
//! each pair gives one difference of the blocks' median frames, and the
//! pairs' mean is tested against zero (Student's t, 95 %). Frames of one
//! block are not independent samples (a slow stretch of the scene slows
//! all of them), blocks nearly are.

use std::collections::BTreeMap;
use std::fmt::Write;

use super::json::{self, Obj};
use super::summary::{Histogram, Rate};
use crate::ab::Tag;

/// World frames a block needs for its median to enter a pair.
const MIN_BLOCK_FRAMES: usize = 5;
/// Blocks kept: more than a day of the shortest.
const MAX_BLOCKS: usize = 50_000;
/// Frames kept per block for its median.
const MAX_BLOCK_FRAMES: usize = 20_000;
/// One-second windows kept per arm.
const MAX_WINDOWS: usize = 100_000;
/// A frame longer than this (ms) is a long one.
const LONG_MS: f64 = 1000.0;

/// One arm's numbers.
#[derive(Default)]
struct Arm {
    /// World frames, settled.
    frames: Histogram,
    /// Their total length, ms.
    total_ms: f64,
    /// Those longer than [`LONG_MS`].
    long: u64,
    /// The meter's one-second windows that lay wholly in this arm.
    windows: Vec<Rate>,
}

/// The pairs' verdict on one statistic of the blocks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Paired {
    /// Pairs of blocks compared.
    pub pairs: usize,
    /// Pairs in which arm B's frame was the shorter.
    pub b_shorter: usize,
    /// Mean of (B - A), ms: negative when B is faster.
    pub mean_ms: f64,
    /// Half the width of the 95 % interval around the mean, ms.
    pub half_ms: f64,
}

impl Paired {
    /// Whether the interval leaves zero out.
    pub fn significant(&self) -> bool {
        self.pairs >= 3 && self.mean_ms.abs() > self.half_ms
    }
}

/// The two-sided 95 % point of Student's t with `df` degrees of freedom.
fn t95(df: usize) -> f64 {
    const TABLE: [f64; 30] = [
        12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228, 2.201, 2.179, 2.160,
        2.145, 2.131, 2.120, 2.110, 2.101, 2.093, 2.086, 2.080, 2.074, 2.069, 2.064, 2.060, 2.056,
        2.052, 2.048, 2.045, 2.042,
    ];
    match df {
        0 => f64::INFINITY,
        1..=30 => TABLE[df - 1],
        31..=60 => 2.021,
        61..=120 => 2.000,
        _ => 1.960,
    }
}

/// The mean of the differences and its 95 % interval; `None` without two.
fn paired(diffs: &[f64]) -> Option<Paired> {
    let n = diffs.len();
    if n < 2 {
        return None;
    }
    let mean = diffs.iter().sum::<f64>() / n as f64;
    let var = diffs.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
    Some(Paired {
        pairs: n,
        b_shorter: diffs.iter().filter(|d| **d < 0.0).count(),
        mean_ms: mean,
        half_ms: t95(n - 1) * (var / n as f64).sqrt(),
    })
}

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[v.len() / 2])
}

/// What a run's alternating switches add up to.
pub struct AbStats {
    /// What each arm sets (`NAME=value ...`).
    labels: [String; 2],
    /// Each factor's switches with both values (`NAME=a/b`): one for a
    /// plain A/B, more for a factorial, whose blocks are cells.
    factors: Vec<String>,
    /// A block's length, seconds.
    period_s: f64,
    arms: [Arm; 2],
    /// Per block: its cell (the arm, in a plain A/B) and its settled
    /// world frames, ms.
    blocks: BTreeMap<u32, (u8, Vec<f32>)>,
    /// The frames since the meter's last window: their count, how many
    /// were of the world, the arm of the first, and whether all were
    /// settled frames of that arm.
    window: (u32, u32, u8, bool),
}

impl AbStats {
    /// For a plan with these arms and block length (seconds).
    pub fn new(labels: [String; 2], period_s: f64) -> AbStats {
        AbStats {
            labels,
            factors: Vec::new(),
            period_s,
            arms: Default::default(),
            blocks: BTreeMap::new(),
            window: (0, 0, 0, true),
        }
    }

    /// The plan's factors, each as `NAME=a/b` (see [`crate::ab::Plan::factor_label`]).
    pub fn with_factors(mut self, factors: Vec<String>) -> AbStats {
        self.factors = factors;
        self
    }

    /// A frame of `ms` ended under `tag`; `world` says whether it counts
    /// (a frame of the world, after the world's start).
    pub fn frame(&mut self, tag: Tag, ms: f64, world: bool) {
        let arm = tag.arm & 1;
        let (n, of_world, first, clean) = &mut self.window;
        if *n == 0 {
            *first = arm;
        }
        *n += 1;
        *of_world += u32::from(world);
        *clean &= tag.settled && arm == *first;
        if !(world && tag.settled) {
            return;
        }
        let a = &mut self.arms[usize::from(arm)];
        a.frames.add(ms);
        a.total_ms += ms;
        a.long += u64::from(ms > LONG_MS);
        if self.blocks.len() < MAX_BLOCKS || self.blocks.contains_key(&tag.block) {
            let block = &mut self
                .blocks
                .entry(tag.block)
                .or_insert((tag.arm, Vec::new()))
                .1;
            if block.len() < MAX_BLOCK_FRAMES {
                block.push(ms as f32);
            }
        }
    }

    /// The meter's numbers for the frames since its last window. Kept if
    /// those frames were all settled frames of one arm, most of the world.
    pub fn window(&mut self, rate: Rate) {
        let (n, of_world, arm, clean) = std::mem::replace(&mut self.window, (0, 0, 0, true));
        let windows = &mut self.arms[usize::from(arm)].windows;
        if n > 0 && clean && of_world * 2 >= n && windows.len() < MAX_WINDOWS {
            windows.push(rate);
        }
    }

    /// Differences (B - A) of neighbouring blocks' `stat`, ms.
    fn diffs(&self, stat: fn(&[f32]) -> f64) -> Vec<f64> {
        let mut out = Vec::new();
        for (&block, (arm, frames)) in &self.blocks {
            if block % 2 != 0 || frames.len() < MIN_BLOCK_FRAMES {
                continue;
            }
            let Some((other_arm, other)) = self.blocks.get(&(block + 1)) else {
                continue;
            };
            // Neighbouring blocks differ in factor 0 alone.
            if other.len() < MIN_BLOCK_FRAMES || other_arm & 1 == arm & 1 {
                continue;
            }
            let (a, b) = if arm & 1 == 0 {
                (frames, other)
            } else {
                (other, frames)
            };
            out.push(stat(b) - stat(a));
        }
        out
    }

    /// The pairs' verdict on the blocks' median frame.
    pub fn median_verdict(&self) -> Option<Paired> {
        paired(&self.diffs(|f| {
            let mut v: Vec<f64> = f.iter().map(|x| f64::from(*x)).collect();
            median(&mut v).unwrap_or(0.0)
        }))
    }

    /// The pairs' verdict on the blocks' mean frame, which long frames move.
    pub fn mean_verdict(&self) -> Option<Paired> {
        paired(&self.diffs(|f| f.iter().map(|x| f64::from(*x)).sum::<f64>() / f.len() as f64))
    }

    fn part(&self, arm: usize, f: fn(&Rate) -> f64) -> Option<f64> {
        let mut v: Vec<f64> = self.arms[arm].windows.iter().map(f).collect();
        median(&mut v)
    }

    /// One sentence in Portuguese on what the pairs say.
    fn sentence(&self) -> String {
        let Some(v) = self.median_verdict() else {
            return "Blocos insuficientes para comparar (são precisos pelo menos 2 pares de blocos no mundo)."
                .to_owned();
        };
        let base = self.arms[0].frames.percentile(0.5).filter(|p| *p > 0.0);
        let pct = |ms: f64| base.map_or(String::new(), |b| format!(" ({:+.1} %)", ms / b * 100.0));
        if v.significant() {
            format!(
                "**B é mais {}**: mediana do quadro {:+.1} ms{} contra A, IC 95 % de {:+.1} a {:+.1} ms, em {} pares de \
                 blocos (B mais curto em {}).",
                if v.mean_ms < 0.0 { "rápido" } else { "lento" },
                v.mean_ms,
                pct(v.mean_ms),
                v.mean_ms - v.half_ms,
                v.mean_ms + v.half_ms,
                v.pairs,
                v.b_shorter
            )
        } else {
            format!(
                "**Sem diferença detectável**: B − A = {:+.1} ms{}, IC 95 % de {:+.1} a {:+.1} ms, em {} pares de blocos \
                 (B mais curto em {}). Com este ruído, só uma diferença acima de {:.1} ms{} apareceria; mais blocos \
                 estreitam o intervalo.",
                v.mean_ms,
                pct(v.mean_ms),
                v.mean_ms - v.half_ms,
                v.mean_ms + v.half_ms,
                v.pairs,
                v.b_shorter,
                v.half_ms,
                pct(v.half_ms)
            )
        }
    }

    /// The `ab` object of `resumo.json`.
    pub fn to_json(&self) -> String {
        let arm = |i: usize| {
            let a = &self.arms[i];
            let n = a.frames.count();
            Obj::new()
                .str("sets", &self.labels[i])
                .int("frames", n)
                .opt_num("p50_ms", a.frames.percentile(0.50))
                .opt_num("p95_ms", a.frames.percentile(0.95))
                .opt_num("p99_ms", a.frames.percentile(0.99))
                .opt_num("max_ms", a.frames.max())
                .opt_num("mean_ms", (n > 0).then(|| a.total_ms / n as f64))
                .int("long_frames_over_1s", a.long)
                .int("windows", a.windows.len() as u64)
                .opt_num("preparing_ms", self.part(i, |r| r.preparing))
                .opt_num("copies_ms", self.part(i, |r| r.copies))
                .opt_num("textures_ms", self.part(i, |r| r.textures))
                .opt_num("gpu_ms", self.part(i, |r| r.gpu))
                .opt_num("copy_back_ms", self.part(i, |r| r.copy_back))
                .opt_num("draws", self.part(i, |r| r.draws))
                .opt_num("dropped", self.part(i, |r| r.dropped))
                .opt_num("guard_faults", self.part(i, |r| r.guard_faults))
                .opt_num("guard_lock_wait_ms", self.part(i, |r| r.guard_lock_wait_ms))
                .opt_num("guard_lock_held_ms", self.part(i, |r| r.guard_lock_held_ms))
                .finish()
        };
        let verdict = |v: Option<Paired>| match v {
            None => "null".to_owned(),
            Some(v) => Obj::new()
                .int("pairs", v.pairs as u64)
                .int("b_shorter", v.b_shorter as u64)
                .num("b_minus_a_ms", v.mean_ms)
                .num("ci95_low_ms", v.mean_ms - v.half_ms)
                .num("ci95_high_ms", v.mean_ms + v.half_ms)
                .bool("significant", v.significant())
                .finish(),
        };
        // Per block, for the analysis outside (scripts/motor/estat.py): its
        // index, arm, settled frames of the stable scene and their numbers.
        let blocks = json::array(self.blocks.iter().map(|(&i, (arm, frames))| {
            let mut v: Vec<f64> = frames.iter().map(|x| f64::from(*x)).collect();
            v.sort_by(f64::total_cmp);
            let n = v.len();
            let at = |p: f64| v.get(((n as f64 - 1.0) * p).round() as usize).copied();
            Obj::new()
                .int("i", u64::from(i))
                .int("arm", u64::from(*arm & 1))
                .int("cell", u64::from(*arm))
                .int("n", n as u64)
                .opt_num("p50_ms", at(0.5))
                .opt_num("p95_ms", at(0.95))
                .opt_num("mean_ms", (n > 0).then(|| v.iter().sum::<f64>() / n as f64))
                .int("over_1s", v.iter().filter(|x| **x > LONG_MS).count() as u64)
                .finish()
        }));
        Obj::new()
            .num("period_s", self.period_s)
            .raw(
                "factors",
                &json::array(self.factors.iter().map(|f| json::string(f))),
            )
            .int("blocks", self.blocks.len() as u64)
            .raw("block_stats", &blocks)
            .raw("a", &arm(0))
            .raw("b", &arm(1))
            .raw("median_frame", &verdict(self.median_verdict()))
            .raw("mean_frame", &verdict(self.mean_verdict()))
            .raw("verdict", &json::string(&self.sentence()))
            .finish()
    }

    /// The section of `resumo.md`.
    pub fn to_markdown(&self) -> String {
        let mut s = String::new();
        let f = |v: Option<f64>| v.map_or("—".to_owned(), |v| format!("{v:.1}"));
        let _ = writeln!(s, "\n## A/B na rodada (SPARK_AB)\n");
        let _ = writeln!(
            s,
            "Blocos de {:.0} s alternados na mesma rodada ({} blocos com quadros do mundo); o começo de cada troca fica fora.\n",
            self.period_s,
            self.blocks.len()
        );
        if self.factors.len() > 1 {
            let _ = writeln!(
                s,
                "Fatorial de {} fatores (`{}`): cada bloco roda uma combinação; A e B abaixo são só os dois níveis do \
                 1º fator. O efeito de cada fator, com intervalo e interações, sai de `python scripts/motor.py ab <pasta>`.\n",
                self.factors.len(),
                self.factors.join("` × `")
            );
        }
        let _ = writeln!(s, "- **A**: `{}`", self.labels[0]);
        let _ = writeln!(s, "- **B**: `{}`\n", self.labels[1]);
        let _ = writeln!(s, "{}\n", self.sentence());
        if let Some(v) = self.mean_verdict() {
            let _ = writeln!(
                s,
                "Pela média do quadro (sensível a engasgos): B − A = {:+.1} ms, IC 95 % de {:+.1} a {:+.1} ms{}.\n",
                v.mean_ms,
                v.mean_ms - v.half_ms,
                v.mean_ms + v.half_ms,
                if v.significant() {
                    ""
                } else {
                    " (não significativo)"
                }
            );
        }
        let _ = writeln!(
            s,
            "| braço | quadros | p50 ms | p95 ms | p99 ms | média ms | > 1 s | janelas | preparando | cópias | texturas | GPU | cópia de volta | draws | descartados |"
        );
        let _ = writeln!(
            s,
            "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"
        );
        for (i, name) in ["A", "B"].iter().enumerate() {
            let a = &self.arms[i];
            let n = a.frames.count();
            let _ = writeln!(
                s,
                "| {name} | {n} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                f(a.frames.percentile(0.50)),
                f(a.frames.percentile(0.95)),
                f(a.frames.percentile(0.99)),
                f((n > 0).then(|| a.total_ms / n as f64)),
                a.long,
                a.windows.len(),
                f(self.part(i, |r| r.preparing)),
                f(self.part(i, |r| r.copies)),
                f(self.part(i, |r| r.textures)),
                f(self.part(i, |r| r.gpu)),
                f(self.part(i, |r| r.copy_back)),
                self.part(i, |r| r.draws)
                    .map_or("—".into(), |v| format!("{v:.0}")),
                self.part(i, |r| r.dropped)
                    .map_or("—".into(), |v| format!("{v:.0}")),
            );
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ab::{Clock, Plan};

    /// A run of `blocks` blocks of ten seconds at about `fps`, whose arm B
    /// frames are `b_ms` longer; `wobble` is a slow drift of the scene.
    fn run(blocks: u32, a_ms: f64, b_extra_ms: f64, wobble_ms: f64) -> AbStats {
        let plan = Plan::parse("VICTORY_X:10").unwrap();
        let mut stats = AbStats::new([plan.label(0), plan.label(1)], plan.period_s);
        let mut clock = Clock::default();
        let (mut now, mut arm, mut second) = (0.0f64, 0u8, 0.0f64);
        let mut seed = 7u64;
        while now < f64::from(blocks) * 10.0 {
            // A deterministic jitter of a few percent.
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let jitter = ((seed >> 40) % 1000) as f64 / 1000.0 - 0.5;
            let drift = wobble_ms * (now / 37.0).sin();
            let ms = a_ms + drift + jitter * a_ms * 0.1 + if arm == 1 { b_extra_ms } else { 0.0 };
            now += ms / 1000.0;
            let (tag, change) = clock.present(&plan, now);
            stats.frame(tag, ms, true);
            if let Some(next) = change {
                arm = next;
            }
            if now - second >= 1.0 {
                second = now;
                stats.window(Rate {
                    ms,
                    preparing: ms * 0.6,
                    gpu: ms * 0.4,
                    ..Rate::default()
                });
            }
        }
        stats
    }

    #[test]
    fn a_real_difference_is_found_through_drift() {
        let stats = run(16, 250.0, -20.0, 40.0);
        let v = stats.median_verdict().unwrap();
        assert_eq!(v.pairs, 8);
        assert!(v.significant(), "{v:?}");
        assert!((v.mean_ms + 20.0).abs() < 6.0, "{v:?}");
        assert!(
            stats.sentence().starts_with("**B é mais rápido**"),
            "{}",
            stats.sentence()
        );
        // Windows that straddle a change of arm are left out of both arms.
        let (a, b) = (
            stats.part(0, |r| r.ms).unwrap(),
            stats.part(1, |r| r.ms).unwrap(),
        );
        assert!(a - b > 10.0, "{a} {b}");
        let json = stats.to_json();
        assert!(json.contains(r#""significant":true"#), "{json}");
        assert!(json.contains(r#""sets":"VICTORY_X=1""#), "{json}");
        assert!(json.contains(r#""block_stats":[{"i":0,"arm":0,"#), "{json}");
        let md = stats.to_markdown();
        assert!(
            md.contains("## A/B na rodada") && md.contains("| B | "),
            "{md}"
        );
    }

    #[test]
    fn the_same_setting_twice_shows_no_difference() {
        let stats = run(16, 250.0, 0.0, 40.0);
        let v = stats.median_verdict().unwrap();
        assert!(!v.significant(), "{v:?}");
        assert!(stats.sentence().starts_with("**Sem diferença detectável**"));
    }

    #[test]
    fn too_few_blocks_give_no_verdict() {
        let stats = run(2, 250.0, -20.0, 0.0);
        assert_eq!(stats.median_verdict(), None);
        assert!(stats.sentence().starts_with("Blocos insuficientes"));
        assert!(stats.to_json().contains(r#""median_frame":null"#));
    }

    #[test]
    fn frames_outside_the_world_and_unsettled_ones_stay_out() {
        let mut stats = AbStats::new(["a".into(), "b".into()], 10.0);
        let tag = |settled| Tag {
            exp: 0,
            block: 1,
            arm: 1,
            settled,
        };
        stats.frame(tag(true), 100.0, false);
        stats.frame(tag(false), 100.0, true);
        assert_eq!(stats.arms[1].frames.count(), 0);
        // The window held an unsettled frame.
        stats.window(Rate::default());
        assert!(stats.arms[1].windows.is_empty());
        stats.frame(tag(true), 1500.0, true);
        stats.window(Rate::default());
        assert_eq!((stats.arms[1].frames.count(), stats.arms[1].long), (1, 1));
        assert_eq!(stats.arms[1].windows.len(), 1);
    }

    #[test]
    fn students_t_narrows_with_more_pairs() {
        assert!(t95(1) > t95(2) && t95(30) > t95(1000));
        let few = paired(&[-10.0, -12.0, -8.0]).unwrap();
        let many = paired(&[-10.0, -12.0, -8.0, -10.0, -12.0, -8.0, -11.0, -9.0]).unwrap();
        assert!(few.half_ms > many.half_ms);
        assert!(few.significant() && many.significant());
        assert_eq!(paired(&[1.0]), None);
    }
}

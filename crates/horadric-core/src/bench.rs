//! `horadric bench`: the same task run under different settings, each in a
//! throwaway clone, so a claim about saving tokens becomes a number from
//! this project's own work.
//!
//! A spec names the tasks (a prompt, and how to tell the work was done)
//! and the variants (what each changes: a line added to the system prompt,
//! settings such as hooks, environment variables). Every task runs under
//! every variant a few times, round robin so a slow hour of the API does
//! not land on one variant. The report sets each variant's medians beside
//! the first variant's, which is the baseline (docs/plan/lore.md part 7).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::spend::{self, Spend};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Spec {
    /// The commit every clone starts from. HEAD of the project when empty.
    #[serde(default)]
    pub commit: String,
    /// The model every run uses.
    #[serde(default)]
    pub model: String,
    /// Runs of each task under each variant, unless the task says.
    #[serde(default = "two")]
    pub repeats: usize,
    /// Runs at once.
    #[serde(default = "one")]
    pub parallel: usize,
    /// A run still going after this many minutes is stopped and counted as
    /// failed.
    #[serde(default = "twenty_five")]
    pub minutes: u64,
    /// Added to every variant's system prompt.
    #[serde(default)]
    pub system: String,
    /// A shell command run once in a clone before any run, such as a build
    /// that fills the shared target folder, so no run pays for compiling
    /// the dependencies. `{horadric}` becomes this build's path.
    #[serde(default)]
    pub prepare: String,
    pub tasks: Vec<Task>,
    pub variants: Vec<Variant>,
}

fn one() -> usize {
    1
}
fn two() -> usize {
    2
}
fn twenty_five() -> u64 {
    25
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Task {
    pub name: String,
    pub prompt: String,
    #[serde(default)]
    pub repeats: Option<usize>,
    /// A shell command run in the clone afterwards: the work was done when
    /// it exits 0.
    #[serde(default)]
    pub check: String,
    /// Words the final answer must hold, any case: for a task that only
    /// asks.
    #[serde(default)]
    pub expect: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Variant {
    pub name: String,
    /// Added to the system prompt after the spec's.
    #[serde(default)]
    pub system: String,
    /// Claude Code settings for the run, such as hooks. `{horadric}` in
    /// any string becomes the path of this build of Horadric.
    #[serde(default)]
    pub settings: Value,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// The model for this variant's runs, in place of the spec's.
    #[serde(default)]
    pub model: String,
    /// Whether the runs get the plugins the user has enabled, which every
    /// real session lists. Off by default, so a variant that only changes
    /// a prompt is not measured against a moving plugin list.
    #[serde(default)]
    pub plugins: bool,
}

/// One task run under one variant, as the results file keeps it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub task: String,
    pub variant: String,
    pub repeat: usize,
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub spend: Spend,
    pub passed: bool,
    #[serde(default)]
    pub seconds: u64,
    /// Why the run failed, when it did before its check.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

/// The runs a spec asks for, as (task, variant, repeat) indexes, round
/// robin: every variant's first run of a task before any second one.
pub fn jobs(spec: &Spec) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    let most = spec
        .tasks
        .iter()
        .map(|t| t.repeats.unwrap_or(spec.repeats))
        .max()
        .unwrap_or(0);
    for r in 0..most {
        for (t, task) in spec.tasks.iter().enumerate() {
            if r >= task.repeats.unwrap_or(spec.repeats) {
                continue;
            }
            for v in 0..spec.variants.len() {
                out.push((t, v, r));
            }
        }
    }
    out
}

/// Puts this build's path where a variant's settings say `{horadric}`.
pub fn fill(settings: &Value, horadric: &str) -> Value {
    match settings {
        Value::String(s) => Value::String(s.replace("{horadric}", horadric)),
        Value::Array(a) => Value::Array(a.iter().map(|v| fill(v, horadric)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), fill(v, horadric)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Whether a question's answer holds every word it must.
pub fn answered(answer: &str, expect: &[String]) -> bool {
    let answer = answer.to_lowercase();
    expect.iter().all(|w| answer.contains(&w.to_lowercase()))
}

fn median(mut v: Vec<u64>) -> u64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[v.len() / 2]
}

/// The difference from the baseline, "-31%" or "+4%", or "base" for the
/// baseline itself.
fn versus(value: u64, base: u64, is_base: bool) -> String {
    if is_base {
        return "base".into();
    }
    if base == 0 {
        return "?".into();
    }
    let change = (value as f64 - base as f64) * 100.0 / base as f64;
    format!("{:+.0}%", change)
}

/// The report: per task, each variant's runs, how many passed, and the
/// median tokens, calls, tokens a call and window at the end, the tokens
/// beside the baseline's. Variants come in the order `order` gives,
/// the first of them the baseline; a variant not in it comes after.
pub fn report(runs: &[Run], order: &[String]) -> String {
    let mut tasks: Vec<&str> = Vec::new();
    for r in runs {
        if !tasks.contains(&r.task.as_str()) {
            tasks.push(&r.task);
        }
    }
    let mut variants: Vec<&str> = order.iter().map(String::as_str).collect();
    for r in runs {
        if !variants.contains(&r.variant.as_str()) {
            variants.push(&r.variant);
        }
    }
    let mut out = String::new();
    for task in tasks {
        out.push_str(&format!("{task}\n"));
        out.push_str(&format!(
            "  {:<16} {:>6} {:>8} {:>7} {:>6} {:>8} {:>8} {:>8}\n",
            "variant", "passed", "tokens", "vs base", "calls", "per call", "end", "results"
        ));
        let of = |v: &str| -> Vec<&Run> {
            runs.iter()
                .filter(|r| r.task == task && r.variant == v)
                .collect()
        };
        let base = variants
            .iter()
            .map(|v| of(v))
            .find(|r| !r.is_empty())
            .map(|r| median(r.iter().map(|r| r.spend.tokens.total()).collect()))
            .unwrap_or(0);
        let mut first = true;
        for v in &variants {
            let rs = of(v);
            if rs.is_empty() {
                continue;
            }
            let m = |f: &dyn Fn(&Spend) -> u64| median(rs.iter().map(|r| f(&r.spend)).collect());
            let tokens = m(&|s| s.tokens.total());
            let results = m(&|s| spend::tokens_of(s.results.values().sum()));
            out.push_str(&format!(
                "  {:<16} {:>6} {:>8} {:>7} {:>6} {:>8} {:>8} {:>8}\n",
                v,
                format!("{}/{}", rs.iter().filter(|r| r.passed).count(), rs.len()),
                spend::count(tokens),
                versus(tokens, base, first),
                m(&|s| s.calls),
                spend::count(m(&|s| s.per_call())),
                spend::count(m(&|s| s.last_window)),
                spend::count(results),
            ));
            first = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec() -> Spec {
        serde_json::from_value(json!({
            "tasks": [
                {"name": "ask", "prompt": "?", "repeats": 3, "expect": ["pack.rs"]},
                {"name": "edit", "prompt": "do"}
            ],
            "variants": [{"name": "base"}, {"name": "lean", "system": "Be lean."}]
        }))
        .unwrap()
    }

    #[test]
    fn a_spec_takes_its_defaults() {
        let s = spec();
        assert_eq!((s.repeats, s.parallel, s.minutes), (2, 1, 25));
        assert!(s.commit.is_empty() && s.variants[0].env.is_empty());
    }

    #[test]
    fn jobs_go_round_robin_and_a_task_keeps_its_own_repeats() {
        let j = jobs(&spec());
        assert_eq!(
            j,
            vec![
                (0, 0, 0),
                (0, 1, 0),
                (1, 0, 0),
                (1, 1, 0),
                (0, 0, 1),
                (0, 1, 1),
                (1, 0, 1),
                (1, 1, 1),
                (0, 0, 2),
                (0, 1, 2),
            ]
        );
    }

    #[test]
    fn the_build_goes_where_settings_say_horadric() {
        let s = json!({"hooks": {"PreToolUse": [{"matcher": "Read", "hooks": [
            {"type": "command", "command": "\"{horadric}\" guard read", "timeout": 10}
        ]}]}});
        let f = fill(&s, "C:/b/horadric.exe");
        assert_eq!(
            f["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "\"C:/b/horadric.exe\" guard read"
        );
        assert_eq!(f["hooks"]["PreToolUse"][0]["hooks"][0]["timeout"], 10);
    }

    #[test]
    fn an_answer_must_hold_every_word() {
        let e = vec!["pack.rs".to_string(), "Rarity".to_string()];
        assert!(answered("In PACK.rs, weighed by rarity.", &e));
        assert!(!answered("In pack.rs.", &e));
        assert!(answered("anything", &[]));
    }

    fn run(task: &str, variant: &str, tokens: u64, calls: u64, passed: bool) -> Run {
        let mut spend = Spend {
            calls,
            last_window: tokens / calls,
            ..Spend::default()
        };
        spend.tokens.cache_read = tokens;
        spend.results.insert("Read".into(), tokens / 10);
        Run {
            task: task.into(),
            variant: variant.into(),
            spend,
            passed,
            ..Run::default()
        }
    }

    #[test]
    fn the_report_sets_each_variant_beside_the_baseline() {
        let runs = [
            run("ask", "lean", 600_000, 6, true),
            run("ask", "base", 1_000_000, 10, true),
            run("ask", "base", 1_200_000, 12, false),
            run("ask", "base", 800_000, 8, true),
            run("ask", "lean", 400_000, 5, true),
        ];
        let r = report(&runs, &["base".into(), "lean".into()]);
        let lines: Vec<&str> = r.lines().collect();
        assert_eq!(lines[0], "ask");
        assert!(lines[1].contains("vs base"));
        assert!(lines[2].starts_with("  base"));
        assert!(lines[2].contains("2/3") && lines[2].contains("1M") && lines[2].contains("base"));
        assert!(lines[3].starts_with("  lean"));
        assert!(lines[3].contains("2/2") && lines[3].contains("600k") && lines[3].contains("-40%"));
    }

    #[test]
    fn differences_read_as_signed_percents() {
        assert_eq!(versus(69, 100, false), "-31%");
        assert_eq!(versus(104, 100, false), "+4%");
        assert_eq!(versus(1, 0, false), "?");
        assert_eq!(versus(5, 5, true), "base");
    }

    #[test]
    fn a_run_round_trips() {
        let r = run("ask", "base", 1_000, 2, true);
        let back: Run = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
        assert!(!serde_json::to_string(&r).unwrap().contains("error"));
    }
}

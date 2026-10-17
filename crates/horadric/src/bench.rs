//! `horadric bench <spec.json>`: runs a spec's tasks under its variants,
//! each run a headless `claude` in a clone of its own, and reports what
//! each variant spent beside the baseline. See `horadric_core::bench`.
//!
//! A clone, not a worktree: the agent is told by CLAUDE.md to merge its
//! worktree into main, and a clone with no remote has no main to merge
//! into or push to. The runs read only project settings, so the user's
//! hooks do not post them to the running Horadric as sessions of their
//! own, and their plugins are left out of every variant alike.

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use horadric_core::bench::{self, Run, Spec, Task, Variant};
use serde_json::{json, Value};

const USAGE: &str = "\
usage: horadric bench <spec.json>       Run every task of the spec under every variant,
                                        each in a throwaway clone, and report what
                                        each variant spent beside the first
       horadric bench report <results.jsonl> [spec.json]
                                        The report of runs already made";

pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        None | Some("-h" | "--help" | "help") => Err(USAGE.into()),
        Some("report") => {
            let results = args.get(1).ok_or(USAGE)?;
            let order = match args.get(2) {
                Some(s) => read_spec(Path::new(s))?
                    .variants
                    .into_iter()
                    .map(|v| v.name)
                    .collect(),
                None => Vec::new(),
            };
            print!("{}", bench::report(&read_runs(Path::new(results)), &order));
            Ok(())
        }
        Some(spec) => start(Path::new(spec)),
    }
}

fn read_spec(path: &Path) -> Result<Spec, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_runs(path: &Path) -> Vec<Run> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Everything the runs share.
struct Bench {
    spec: Spec,
    repo: PathBuf,
    commit: String,
    root: PathBuf,
    exe: String,
    mcp: PathBuf,
    results: PathBuf,
}

fn start(spec_path: &Path) -> Result<(), String> {
    let spec = read_spec(spec_path)?;
    if spec.tasks.is_empty() || spec.variants.is_empty() {
        return Err("the spec names no tasks or no variants".into());
    }
    let repo = PathBuf::from(git(Path::new("."), &["rev-parse", "--show-toplevel"])?);
    let commit = git(
        &repo,
        &[
            "rev-parse",
            if spec.commit.is_empty() {
                "HEAD"
            } else {
                &spec.commit
            },
        ],
    )?;
    let root = std::env::temp_dir()
        .join("horadric-bench")
        .join(unix_now().to_string());
    fs::create_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    // A copy, so a build of the checkout mid bench does not change the
    // guard or `heavy` the runs call, or fail on a locked file.
    let exe_path = root.join("horadric.exe");
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    fs::copy(&me, &exe_path).map_err(|e| format!("cannot copy {}: {e}", me.display()))?;
    let exe = exe_path.to_string_lossy().replace('\\', "/");
    // The lore's tools, from the dev state so the runs' notes stay out of
    // the project's own.
    let mcp = root.join("mcp.json");
    let config = json!({"mcpServers": {"horadric": {
        "type": "stdio", "command": exe, "args": ["mcp"], "env": {"HORADRIC_DEV": "1"}
    }}});
    fs::write(&mcp, config.to_string()).map_err(|e| e.to_string())?;
    let results = root.join("results.jsonl");
    if !spec.prepare.is_empty() {
        let dir = root.join("prepare");
        let b = Bench {
            spec: spec.clone(),
            repo: repo.clone(),
            commit: commit.clone(),
            root: root.clone(),
            exe: exe.clone(),
            mcp: mcp.clone(),
            results: results.clone(),
        };
        clone(&b, &dir)?;
        let began = Instant::now();
        let ok = check(&b, &dir, &spec.prepare.replace("{horadric}", &exe));
        println!(
            "Prepared in {}s{}",
            began.elapsed().as_secs(),
            if ok { "" } else { ", and it failed" }
        );
        let _ = fs::remove_dir_all(&dir);
    }
    let jobs = bench::jobs(&spec);
    println!(
        "{} runs of {} at {}, {} at once. Results in {}",
        jobs.len(),
        repo.display(),
        &commit[..commit.len().min(10)],
        spec.parallel.max(1),
        results.display()
    );
    let order: Vec<String> = spec.variants.iter().map(|v| v.name.clone()).collect();
    let parallel = spec.parallel.max(1);
    let b = Arc::new(Bench {
        spec,
        repo,
        commit,
        root,
        exe,
        mcp,
        results: results.clone(),
    });
    let queue = Arc::new(Mutex::new(jobs.into_iter().collect::<VecDeque<_>>()));
    let workers: Vec<_> = (0..parallel)
        .map(|_| {
            let (b, queue) = (b.clone(), queue.clone());
            std::thread::spawn(move || {
                while let Some((t, v, r)) = next(&queue) {
                    let run = one(&b, &b.spec.tasks[t], &b.spec.variants[v], r);
                    println!(
                        "{} / {} #{}: {} in {}s, {}",
                        run.task,
                        run.variant,
                        run.repeat + 1,
                        if run.passed { "passed" } else { "FAILED" },
                        run.seconds,
                        if run.error.is_empty() {
                            run.spend.line()
                        } else {
                            run.error.clone()
                        }
                    );
                    if let Ok(line) = serde_json::to_string(&run) {
                        if let Ok(mut f) = OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&b.results)
                        {
                            let _ = writeln!(f, "{line}");
                        }
                    }
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    // The shared build folder is gigabytes and of no use once the runs
    // are done; the results, answers and diffs stay.
    let _ = fs::remove_dir_all(b.root.join("target"));
    println!();
    print!("{}", bench::report(&read_runs(&results), &order));
    Ok(())
}

/// The next job, taken under the lock and let go of at once, so the other
/// workers are not held up while this one runs.
fn next(queue: &Mutex<VecDeque<(usize, usize, usize)>>) -> Option<(usize, usize, usize)> {
    queue.lock().ok()?.pop_front()
}

/// One task under one variant: clone, run, measure, check, clean up.
fn one(b: &Bench, task: &Task, variant: &Variant, repeat: usize) -> Run {
    let name: String = format!("{}-{}-{}", task.name, variant.name, repeat + 1)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let dir = b.root.join(&name);
    let mut run = Run {
        task: task.name.clone(),
        variant: variant.name.clone(),
        repeat,
        ..Run::default()
    };
    let began = Instant::now();
    if let Err(e) = clone(b, &dir) {
        run.error = e;
        return run;
    }
    let settings = b.root.join(format!("{name}.settings.json"));
    let filled = bench::fill(&variant.settings, &b.exe);
    let mut filled = if filled.is_object() {
        filled
    } else {
        json!({})
    };
    if variant.plugins {
        // The user's settings are not read, so their plugins come in here.
        if let Some(on) = user_plugins() {
            filled["enabledPlugins"] = on;
        }
    }
    let _ = fs::write(&settings, filled.to_string());
    let system: Vec<String> = [&b.spec.system, &variant.system]
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| s.replace("{horadric}", &b.exe))
        .collect();
    let mut c = Command::new("claude");
    c.current_dir(&dir)
        .arg("-p")
        .arg(&task.prompt)
        .args(["--output-format", "json"])
        .args(["--permission-mode", "bypassPermissions"])
        .args(["--setting-sources", "project,local"])
        .arg("--settings")
        .arg(&settings)
        .arg("--mcp-config")
        .arg(&b.mcp)
        .arg("--strict-mcp-config");
    let model = if variant.model.is_empty() {
        &b.spec.model
    } else {
        &variant.model
    };
    if !model.is_empty() {
        c.args(["--model", model]);
    }
    if !system.is_empty() {
        c.arg("--append-system-prompt").arg(system.join("\n\n"));
    }
    for (k, _) in std::env::vars() {
        if k.starts_with("HORADRIC_") {
            c.env_remove(k);
        }
    }
    c.env("CARGO_TARGET_DIR", b.root.join("target"));
    c.envs(&variant.env);
    let stderr = fs::File::create(b.root.join(format!("{name}.stderr"))).ok();
    c.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(stderr.map(Stdio::from).unwrap_or_else(Stdio::null));
    let out = match finish(c, Duration::from_secs(b.spec.minutes * 60)) {
        Ok(out) => out,
        Err(e) => {
            run.error = e;
            run.seconds = began.elapsed().as_secs();
            let _ = fs::remove_dir_all(&dir);
            return run;
        }
    };
    let v: Value = serde_json::from_str(out.trim()).unwrap_or(Value::Null);
    run.session = v
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let answer = v.get("result").and_then(Value::as_str).unwrap_or_default();
    let _ = fs::write(b.root.join(format!("{name}.answer.md")), answer);
    if let Some(s) = horadric_hooks::transcript::spend(&dir.to_string_lossy(), &run.session) {
        run.spend = s;
    }
    if v.get("is_error").and_then(Value::as_bool) == Some(true) || run.session.is_empty() {
        run.error = format!(
            "claude ended with an error: {}",
            v.get("subtype")
                .and_then(Value::as_str)
                .unwrap_or("no result")
        );
    }
    if let Ok(diff) = git(&dir, &["diff"]) {
        let _ = fs::write(b.root.join(format!("{name}.diff")), diff);
    }
    run.passed = run.error.is_empty()
        && bench::answered(answer, &task.expect)
        && (task.check.is_empty() || check(b, &dir, &task.check));
    run.seconds = began.elapsed().as_secs();
    let _ = fs::remove_dir_all(&dir);
    run
}

/// The plugins enabled in the user's own Claude Code settings.
fn user_plugins() -> Option<Value> {
    let home = horadric_hooks::home()?;
    let path = Path::new(&home).join(".claude").join("settings.json");
    let v: Value = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    v.get("enabledPlugins").cloned()
}

fn clone(b: &Bench, dir: &Path) -> Result<(), String> {
    let at = dir.to_string_lossy().into_owned();
    git(
        &b.root,
        &[
            "clone",
            "--quiet",
            "--no-checkout",
            &b.repo.to_string_lossy(),
            &at,
        ],
    )?;
    git(dir, &["checkout", "--quiet", "--detach", &b.commit])?;
    git(dir, &["remote", "remove", "origin"])?;
    Ok(())
}

/// Whether the task's check passes in the clone.
fn check(b: &Bench, dir: &Path, command: &str) -> bool {
    Command::new("bash")
        .args(["-c", command])
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", b.root.join("target"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Runs `c` to its end or `longest`, whichever is first, and gives its
/// standard output. One that runs too long is stopped.
fn finish(mut c: Command, longest: Duration) -> Result<String, String> {
    let mut child = c.spawn().map_err(|e| format!("cannot start claude: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + longest;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("stopped after {} minutes", longest.as_secs() / 60));
            }
            Ok(None) => std::thread::sleep(Duration::from_secs(2)),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(reader.join().unwrap_or_default())
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

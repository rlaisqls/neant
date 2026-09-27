//! `tests/corpus/<program>/`: the domain corpus (docs/corpus.md) — control loops, kernels, a
//! parser and a graph search, each rooted at `main.nt` and sharing `tests/corpus/lib/` by `use`.
//! A program runs from its own directory with `main.args` (one per line) and must print
//! `main.out` and reproduce `main.cost`; a `rejected_*` case must be refused with an error
//! containing `main.err`. `tests/corpus/tiers.txt` is the tier of every function in every
//! program, as the reports state it — the table docs/corpus.md counts — and must match them.

use std::path::{Path, PathBuf};
use std::process::Command;

fn neant() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_neant"))
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/corpus")
}

/// A report's function lines as `name tier`: the tier is the report's own word, `unknown` for a
/// line that says `unknown:`.
fn tiers(report: &str) -> Vec<(String, String)> {
    report.lines()
        .filter(|l| !l.starts_with(' ') && !l.starts_with("struct ") && !l.is_empty())
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            let name = words.next()?.to_string();
            let rest: Vec<&str> = words.collect();
            if rest.first() == Some(&"unknown:") { return Some((name, "unknown".into())); }
            let tier = rest.iter().rev().map(|w| w.trim_end_matches(',')).find(|w| matches!(*w, "exact" | "modulo" | "bound" | "declared"))?;
            Some((name, tier.to_string()))
        })
        .collect()
}

#[test]
fn corpus() {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(corpus_dir())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("main.nt").exists())
        .collect();
    cases.sort();
    assert!(cases.len() >= 4, "the corpus has {} programs", cases.len());

    let mut failures = Vec::new();
    let mut table = String::new();
    for dir in &cases {
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).ok();
        let run = |args: &[&str]| Command::new(neant()).args(args).current_dir(dir).output().unwrap();
        if let Some(want) = read("main.err") {
            let want = want.trim();
            let out = run(&["check", "main.nt"]);
            let stderr = String::from_utf8_lossy(&out.stderr);
            if out.status.success() {
                failures.push(format!("{name}: expected rejection containing `{want}`, but it was accepted"));
            } else if !stderr.contains(want) {
                failures.push(format!("{name}: expected error containing `{want}`, got:\n{stderr}"));
            }
            continue;
        }
        match read("main.cost") {
            Some(want) => {
                let got = String::from_utf8_lossy(&run(&["cost", "main.nt"]).stdout).to_string();
                if got != want {
                    failures.push(format!("{name}: cost report differs\n--- got ---\n{got}--- want ---\n{want}"));
                }
                for (f, t) in tiers(&want) { table.push_str(&format!("{name:<10} {f:<16} {t}\n")); }
            }
            None => failures.push(format!("{name}: no main.cost; every program's report is pinned")),
        }
        let args: Vec<String> = read("main.args").map(|s| s.lines().map(String::from).collect()).unwrap_or_default();
        let mut cmd = vec!["run".to_string(), "main.nt".to_string(), "--".to_string()];
        cmd.extend(args);
        let out = Command::new(neant()).args(&cmd).current_dir(dir).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let code = out.status.code().unwrap_or(-1);
        let want_out = read("main.out").unwrap_or_default();
        let want_exit: i32 = read("main.exit").and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        if stdout != want_out || code != want_exit {
            failures.push(format!(
                "{name}: exit {code} (want {want_exit})\n--- stdout ---\n{stdout}--- want ---\n{want_out}--- stderr ---\n{}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }
    let pinned = std::fs::read_to_string(corpus_dir().join("tiers.txt")).unwrap_or_default();
    if pinned != table {
        failures.push(format!("tests/corpus/tiers.txt is stale\n--- from the reports ---\n{table}--- pinned ---\n{pinned}"));
    }
    if !failures.is_empty() {
        panic!("{} of {} corpus programs failed:\n\n{}", failures.len(), cases.len(), failures.join("\n"));
    }
}

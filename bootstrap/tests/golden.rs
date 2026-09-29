//! Every `tests/golden/*.nt` with a `.cost` file must also reproduce it under `neant cost`, one with a `.eval` file the report `neant cost --eval` prints at it (`.evalcost`), and one with a `.scop` file its SCoP export. It either runs — stdout must equal `.out`, exit code must equal `.exit`
//! (default 0) — or, when a `.err` file exists, must be rejected with an error containing it.
//! A `.args` file holds the program's arguments, one per line: it is run as `neant run f.nt --
//! args…` from `tests/golden`, so a path among them names a file there, and once more as the
//! binary `neant build` wrote, given the same arguments directly.

use std::path::{Path, PathBuf};
use std::process::Command;

fn neant() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_neant"))
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/golden")
}

#[test]
fn golden() {
    let mut files: Vec<PathBuf> = std::fs::read_dir(golden_dir())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "nt"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no golden programs found");

    let mut failures = Vec::new();
    for nt in &files {
        let stem = nt.with_extension("");
        let name = nt.file_name().unwrap().to_string_lossy().to_string();
        let err_file = stem.with_extension("err");
        if err_file.exists() {
            let want = std::fs::read_to_string(&err_file).unwrap().trim().to_string();
            let out = Command::new(neant()).arg("check").arg(nt).output().unwrap();
            let stderr = String::from_utf8_lossy(&out.stderr);
            if out.status.success() {
                failures.push(format!("{name}: expected rejection containing `{want}`, but it was accepted"));
            } else if !stderr.contains(&want) {
                failures.push(format!("{name}: expected error containing `{want}`, got:\n{stderr}"));
            }
            continue;
        }
        let cost_file = stem.with_extension("cost");
        if cost_file.exists() {
            let want = std::fs::read_to_string(&cost_file).unwrap();
            let out = Command::new(neant()).arg("cost").arg(nt).output().unwrap();
            let got = String::from_utf8_lossy(&out.stdout).to_string();
            if got != want {
                failures.push(format!("{name}: cost report differs\n--- got ---\n{got}--- want ---\n{want}"));
            }
        }
        // `.eval`: its first line is `--eval`'s argument, and `.evalcost` what the report must be
        // there — the time line and what it reads (`serial`, `divs`), which the plain report lacks
        let eval_file = stem.with_extension("eval");
        if eval_file.exists() {
            let ev = std::fs::read_to_string(&eval_file).unwrap();
            let want = std::fs::read_to_string(stem.with_extension("evalcost")).unwrap_or_default();
            let out = Command::new(neant()).arg("cost").arg(nt).arg("--eval").arg(ev.lines().next().unwrap_or("").trim()).output().unwrap();
            let got = String::from_utf8_lossy(&out.stdout).to_string();
            if got != want {
                failures.push(format!("{name}: evaluated cost report differs\n--- got ---\n{got}--- want ---\n{want}"));
            }
        }
        // `.scop`: the first line names the function, the rest is what `emit --scop` must print
        let scop_file = stem.with_extension("scop");
        if scop_file.exists() {
            let want = std::fs::read_to_string(&scop_file).unwrap();
            let (func, want) = want.split_once('\n').unwrap();
            let out = Command::new(neant()).arg("emit").arg("--scop").arg(func.trim()).arg(nt).output().unwrap();
            let got = String::from_utf8_lossy(&out.stdout).to_string();
            if got != want {
                failures.push(format!("{name}: SCoP export differs\n--- got ---\n{got}--- want ---\n{want}"));
            }
        }
        let want_out = std::fs::read_to_string(stem.with_extension("out")).unwrap_or_default();
        let want_exit: i32 = std::fs::read_to_string(stem.with_extension("exit"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let args: Option<Vec<String>> = std::fs::read_to_string(stem.with_extension("args")).ok()
            .map(|s| s.lines().map(String::from).collect());
        let mut run = Command::new(neant());
        run.arg("run").arg(nt);
        if let Some(a) = &args { run.arg("--").args(a).current_dir(golden_dir()); }
        let out = run.output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let code = out.status.code().unwrap_or(-1);
        if let Some(a) = &args {
            let bin = std::env::temp_dir().join(format!("neant-golden-{}-{}", std::process::id(), stem.file_name().unwrap().to_string_lossy()));
            let built = Command::new(neant()).arg("build").arg(nt).arg("-o").arg(&bin).output().unwrap();
            let direct = Command::new(&bin).args(a).current_dir(golden_dir()).output();
            let _ = std::fs::remove_file(&bin);
            let _ = std::fs::remove_file(bin.with_extension("c"));
            match direct {
                Ok(d) if built.status.success() && d.stdout == out.stdout && d.status.code() == out.status.code() => {}
                _ => failures.push(format!("{name}: the built binary, given its arguments directly, does not do what `neant run` did")),
            }
        }
        if stdout != want_out || code != want_exit {
            let stderr = String::from_utf8_lossy(&out.stderr);
            failures.push(format!(
                "{name}: exit {code} (want {want_exit})\n--- stdout ---\n{stdout}--- want ---\n{want_out}--- stderr ---\n{stderr}"
            ));
        }
    }
    if !failures.is_empty() {
        panic!("{} of {} golden programs failed:\n\n{}", failures.len(), files.len(), failures.join("\n"));
    }
}

/// `tests/golden/modules/<case>/`: a program over several files (docs/modules-design.md), rooted
/// at `main.nt` and run from the case's own directory so paths print as a reader writes them.
/// `main.err`, `.out`, `.exit`, `.cost` as above; `main.stderr` must appear in a run's stderr, and
/// a `costs.lock` must be up to date under `neant lock --check`.
#[test]
fn modules() {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(golden_dir().join("modules"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("main.nt").exists())
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "no multi-file programs found");

    let run = |dir: &Path, args: &[&str]| Command::new(neant()).args(args).current_dir(dir).output().unwrap();
    let mut failures = Vec::new();
    for dir in &cases {
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).ok();
        if let Some(want) = read("main.err") {
            let want = want.trim();
            let out = run(dir, &["check", "main.nt"]);
            let stderr = String::from_utf8_lossy(&out.stderr);
            if out.status.success() {
                failures.push(format!("{name}: expected rejection containing `{want}`, but it was accepted"));
            } else if !stderr.contains(want) {
                failures.push(format!("{name}: expected error containing `{want}`, got:\n{stderr}"));
            }
            continue;
        }
        if let Some(want) = read("main.cost") {
            let got = String::from_utf8_lossy(&run(dir, &["cost", "main.nt"]).stdout).to_string();
            if got != want {
                failures.push(format!("{name}: cost report differs\n--- got ---\n{got}--- want ---\n{want}"));
            }
        }
        if dir.join("costs.lock").exists() {
            let out = run(dir, &["lock", "--check", "main.nt"]);
            if !out.status.success() {
                failures.push(format!("{name}: costs.lock is stale:\n{}", String::from_utf8_lossy(&out.stdout)));
            }
        }
        let want_out = read("main.out").unwrap_or_default();
        let want_exit: i32 = read("main.exit").and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        let out = run(dir, &["run", "main.nt"]);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let code = out.status.code().unwrap_or(-1);
        let want_err = read("main.stderr").map(|s| s.trim().to_string());
        if stdout != want_out || code != want_exit || want_err.as_ref().is_some_and(|w| !stderr.contains(w.as_str())) {
            failures.push(format!(
                "{name}: exit {code} (want {want_exit})\n--- stdout ---\n{stdout}--- want ---\n{want_out}--- stderr ---\n{stderr}--- want in stderr ---\n{}\n",
                want_err.unwrap_or_default()
            ));
        }
    }
    if !failures.is_empty() {
        panic!("{} of {} multi-file programs failed:\n\n{}", failures.len(), cases.len(), failures.join("\n"));
    }
}

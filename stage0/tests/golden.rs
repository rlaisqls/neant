//! Every `tests/golden/*.nt` either runs — stdout must equal `.out`, exit code must equal `.exit`
//! (default 0) — or, when a `.err` file exists, must be rejected with an error containing it.

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
        let want_out = std::fs::read_to_string(stem.with_extension("out")).unwrap_or_default();
        let want_exit: i32 = std::fs::read_to_string(stem.with_extension("exit"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let out = Command::new(neant()).arg("run").arg(nt).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let code = out.status.code().unwrap_or(-1);
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

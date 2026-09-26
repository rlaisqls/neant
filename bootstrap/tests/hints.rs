//! `neant hints` pinned: every `tests/hints/<name>.json` must be what `neant hints
//! tests/golden/<name>.nt` prints, run from the repository root — an exact program (`dot`), one
//! with an unknown, a recurrence and a modulo (`fib`), a type error (`err_arity`) and a broken
//! `#[cost]` budget (`err_budget`). The editor extension (editors/vscode) reads nothing else.

use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn hints() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join("tests/hints"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    assert!(files.len() >= 4, "tests/hints holds fewer programs than it should");
    let mut failures = Vec::new();
    for want_file in &files {
        let stem = want_file.file_stem().unwrap().to_string_lossy().to_string();
        let src = format!("tests/golden/{stem}.nt");
        let out = Command::new(env!("CARGO_BIN_EXE_neant")).arg("hints").arg(&src).current_dir(&root).output().unwrap();
        let got = String::from_utf8_lossy(&out.stdout).to_string();
        let want = std::fs::read_to_string(want_file).unwrap();
        if !out.status.success() { failures.push(format!("{stem}: exit {:?}", out.status.code())); }
        if got != want { failures.push(format!("{stem}: hints differ\n--- got ---\n{got}--- want ---\n{want}")); }
    }
    if !failures.is_empty() { panic!("{}", failures.join("\n")); }
}

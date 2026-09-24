//! Stage D (4): the compiler's own `costs.lock` is committed, and **must be what `neant lock`
//! writes today** for `compiler/*.nt` concatenated as `build.sh` does. A change to the compiler's
//! text or to the calculus that moves any of its 276 lines has to show up as a change to the
//! lockfile, which is where the exact count is read from; `compiler/lock.sh` regenerates it and
//! prints the count.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(sub)
}

#[test]
fn compiler_costs_lock_is_current() {
    let dir = std::env::temp_dir().join(format!("neant-lock-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut all = String::new();
    for f in ["lex", "parse", "check", "emit", "poly", "cost", "main"] {
        all.push_str(&std::fs::read_to_string(repo(&format!("compiler/{f}.nt"))).unwrap());
    }
    std::fs::write(dir.join("all.nt"), all).unwrap();
    std::fs::copy(repo("compiler/costs.lock"), dir.join("costs.lock")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_neant")).arg("lock").arg(dir.join("all.nt")).arg("--check").output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "compiler/costs.lock is stale — run compiler/lock.sh and commit the diff:\n{}{}",
        String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
}

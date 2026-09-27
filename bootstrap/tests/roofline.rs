//! The roofline line of `neant cost --eval` (docs/cost-model.md § Time): the predicted time is the
//! longer of `work·τ` and `moves/BW`, and says which term bound it. `dot` at a size in cache is
//! work-bound; at the same size with a tiny `BW` it is moves-bound; with `--tau 1 --bw 1` the time
//! is exactly `max(work, moves)` nanoseconds, so the arithmetic is pinned, not only the shape.

use std::path::Path;
use std::process::Command;

fn eval_line(extra: &[&str]) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let out = Command::new(env!("CARGO_BIN_EXE_neant"))
        .args(["cost", "tests/golden/dot.nt", "--eval", "a.len()=1000,b.len()=1000,B=64"])
        .args(extra)
        .current_dir(&root)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    text.lines().find(|l| l.trim_start().starts_with("at ") && l.contains("time")).unwrap_or_else(|| panic!("no time line in:\n{text}")).to_string()
}

fn field(line: &str, key: &str) -> f64 {
    let at = line.find(key).unwrap_or_else(|| panic!("no `{key}` in {line}")) + key.len();
    line[at..].split_whitespace().next().unwrap().parse().unwrap()
}

#[test]
fn roofline() {
    let unit = eval_line(&["--tau", "1", "--bw", "1"]);
    let (w, m, t) = (field(&unit, "work "), field(&unit, "moves "), field(&unit, "time "));
    assert!((t - w.max(m) / 1e9).abs() <= 1e-3 * t, "time {t} is not max(work {w}, moves {m}) ns: {unit}");
    assert!(eval_line(&["--tau", "1", "--bw", "1000000"]).contains("(work-bound)"));
    assert!(eval_line(&["--tau", "0.000001", "--bw", "0.001"]).contains("(moves-bound)"));
}

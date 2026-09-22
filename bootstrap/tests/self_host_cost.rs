//! The cost calculus's exit test (docs/self-hosting-cost-design.md §8). `neant cost` prints a
//! `work` column per function; the self-hosted pass prints the same column, and **the two strings
//! must be identical** — the same rational reduction, the same term order, the same printing.
//! String equality is deliberately strict: it catches `n²/2 − n/2` written as `0.5·n² − 0.5·n`,
//! and a term order that happens to agree on this corpus and not in general.
//!
//! Where the Rust compiler solves a recurrence the self-hosted pass cannot — two or more recursive
//! calls per invocation — it must say **unknown** rather than a number. A cost calculus that
//! guesses is worse than one that declines, so that is asserted and not merely tolerated.
//!
//! Two groups of functions differ **on purpose**, and are listed by name. `ys = xs` on whole arrays
//! costs 1 when the compiler can prove the assignment is in place and the array's length when it
//! cannot. The proof is M5's uniqueness analysis; the self-hosted compiler has none, so its
//! emitter always copies (docs/self-hosting-arrays-design.md §7) and its cost says so. The two
//! reports disagree because the two compilers emit different code, which is what a cost is a claim
//! about.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn neant() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_neant"))
}

fn repo(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(sub)
}

/// `(file, function)` where the self-hosted cost is higher because the self-hosted emitter copies
/// a whole-array assignment the Rust compiler proves is in place. Deleting a line from here is
/// what growing move checking into `compiler/check.nt` would look like.
const COPIES_INSTEAD: &[(&str, &str)] = &[
    ("arrayview.nt", "main"),
    ("reassign_inplace.nt", "main"),
    ("reassign_len_size.nt", "main"),
    ("reassign_named_size.nt", "main"),
];

/// The number of functions whose `work` the self-hosted pass reproduces exactly. In the test so
/// that widening the slice means changing a number someone has to look at.
const EXACT: usize = 71;

/// The same for `moves`, whose slice is narrower: a function that calls anything is unknown,
/// because a callee's traffic depends on what is already resident (design §11).
const EXACT_MOVES: usize = 26;

/// `(file, function)` where the self-hosted `moves` differs because **the self-hosted emitter
/// always lays an array of structs out as AoS** (docs/self-hosting-arrays-design.md §4) while the
/// Rust compiler chooses AoS or SoA from the cost model. Same principle as `COPIES_INSTEAD`,
/// arrived at independently: a cost is a claim about the code the compiler emits.
const AOS_INSTEAD: &[(&str, &str)] = &[
    ("arrayview.nt", "tagged"),
    ("particles.nt", "step"),
    ("particles.nt", "kinetic"),
    ("particles.nt", "centroid_x"),
    ("structs.nt", "sum_x"),
    ("structs.nt", "sum_all"),
    ("structs.nt", "shift"),
];



/// The driver is a committed fragment, not a string in this file: a test that embeds the program
/// it runs drifts from it silently, which this one did once.
fn driver() -> String {
    std::fs::read_to_string(repo("compiler/costdump.nt")).unwrap()
}

/// `neant cost`'s report, as `function -> Some(work)` or `None` when it says `unknown`.
fn rust_report(out: &str) -> BTreeMap<String, Option<String>> {
    let mut m = BTreeMap::new();
    for line in out.lines() {
        if line.starts_with(' ') || line.trim().is_empty() { continue; }
        let Some((name, rest)) = line.split_once(' ') else { continue };
        let rest = rest.trim_start();
        if let Some(w) = rest.strip_prefix("work ") {
            let w = match w.find("moves ") { Some(i) => &w[..i], None => w };
            m.insert(name.to_string(), Some(w.trim_end().to_string()));
        } else if rest.starts_with("unknown") {
            m.insert(name.to_string(), None);
        }
    }
    m
}

/// The `moves` column, for the functions that have one and are not piecewise. A regime is not a
/// polynomial and this slice does not compute one, so those are left out entirely rather than
/// compared and excused.
fn rust_moves(out: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for line in out.lines() {
        if line.starts_with(' ') || line.trim().is_empty() { continue; }
        let Some(i) = line.find(" moves ") else { continue };
        let rest = &line[i + 7..];
        let Some(j) = rest.find("  ") else { continue };
        let (mv, tag) = (rest[..j].trim(), &rest[j..]);
        if tag.contains("regime") || mv.is_empty() { continue; }
        let name = line.split_whitespace().next().unwrap_or("").to_string();
        m.insert(name, mv.to_string());
    }
    m
}

#[test]
fn self_hosted_work_agrees_with_bootstrap() {
    let stages: String = ["compiler/lex.nt", "compiler/parse.nt", "compiler/check.nt",
                          "compiler/emit.nt", "compiler/poly.nt", "compiler/cost.nt"]
        .iter().map(|p| std::fs::read_to_string(repo(p)).unwrap()).collect();
    let dir = std::env::temp_dir().join(format!("neant-cost-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let nt = dir.join("cost_driver.nt");
    std::fs::write(&nt, format!("{stages}\n{}", driver())).unwrap();

    // The reporter is **compiled by the self-hosted compiler**, from the committed seed, rather
    // than interpreted. Two things follow. It is some fifteen times faster, which is what makes a
    // corpus-wide comparison affordable at all. And it puts the cost pass inside the fixpoint:
    // the cost calculus now survives being compiled by the compiler it is part of, which nothing
    // tested before — `compiler/main.nt` is a filter with no argv, so the cost pass could not be
    // a mode of the compiler binary and had to be a second one.
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let seed = dir.join("seed");
    let built = Command::new(&cc).args(["-O1", "-std=gnu11", "-w", "-o"]).arg(&seed)
        .arg(repo("bootstrap/neant.c")).arg(repo("bootstrap/rt.c")).output().unwrap();
    assert!(built.status.success(), "cc rejected the committed seed:\n{}",
        String::from_utf8_lossy(&built.stderr));
    let emitted = Command::new(&seed).stdin(std::fs::File::open(&nt).unwrap()).output().unwrap();
    assert!(emitted.status.success(),
        "the self-hosted compiler could not compile the cost pass (exit {:?}: 2 parser, 3 checker, \
         4 emitter, 5 size)", emitted.status.code());
    let reporter_c = dir.join("reporter.c");
    std::fs::write(&reporter_c, &emitted.stdout).unwrap();
    let reporter = dir.join("reporter");
    let built = Command::new(&cc).args(["-O1", "-std=gnu11", "-w", "-o"]).arg(&reporter)
        .arg(&reporter_c).arg(repo("bootstrap/rt.c")).output().unwrap();
    assert!(built.status.success(), "cc rejected the C the self-hosted compiler wrote for the \
        cost pass ({}):\n{}", reporter_c.display(), String::from_utf8_lossy(&built.stderr));

    let mut files: Vec<PathBuf> = std::fs::read_dir(repo("tests/golden")).unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "nt"))
        .collect();
    files.sort();

    let (mut exact, mut unknown, mut failures) = (0, 0, Vec::new());
    let mut exact_moves = 0;
    for f in &files {
        // only what the self-hosted compiler can read, and only what checks
        if Command::new(neant()).arg("parsedump").arg(f).output().unwrap().status.code() == Some(2) { continue; }
        if !Command::new(neant()).arg("check").arg(f).output().unwrap().status.success() { continue; }
        let name = f.file_name().unwrap().to_string_lossy().to_string();

        let report = String::from_utf8_lossy(
            &Command::new(neant()).arg("cost").arg(f).output().unwrap().stdout).into_owned();
        let want = rust_report(&report);
        let want_moves = rust_moves(&report);
        let run = Command::new(&reporter)
            .stdin(std::fs::File::open(f).unwrap()).output().unwrap();
        assert!(run.status.success(), "the self-hosted cost reporter failed on {name}:\n{}",
            String::from_utf8_lossy(&run.stderr));
        let out = String::from_utf8_lossy(&run.stdout);
        let mut lines = out.lines();
        assert_eq!(lines.next(), Some("0"), "{name}: the self-hosted checker rejected it");
        assert_eq!(lines.next(), Some("0"), "{name}: the self-hosted parser rejected it");
        let rows: Vec<Vec<&str>> = lines.map(|l| l.split('\t').collect()).collect();
        let got: BTreeMap<&str, &str> = rows.iter()
            .filter(|r| r.len() >= 2).map(|r| (r[0], r[1])).collect();
        let got_moves: BTreeMap<&str, &str> = rows.iter()
            .filter(|r| r.len() >= 3).map(|r| (r[0], r[2])).collect();

        for (fname, mw) in &want_moves {
            let Some(g) = got_moves.get(fname.as_str()) else { continue };
            if *g == "unknown" { continue; }
            let listed = AOS_INSTEAD.contains(&(name.as_str(), fname.as_str()));
            if g == mw {
                exact_moves += 1;
                if listed {
                    failures.push(format!("{name} {fname} is listed as a moves difference, but the \
                        two agree — delete it from AOS_INSTEAD"));
                }
            } else if !listed {
                failures.push(format!("{name} {fname}: `neant cost` says moves [{mw}], the \
                    self-hosted pass says [{g}]"));
            }
        }
        for (fname, w) in &want {
            let Some(g) = got.get(fname.as_str()) else { continue };
            let copies = COPIES_INSTEAD.contains(&(name.as_str(), fname.as_str()));
            match w {
                None => {
                    if *g != "unknown" {
                        failures.push(format!("{name} {fname}: `neant cost` says unknown, the \
                            self-hosted pass says {g} — it must decline, not guess"));
                    }
                }
                Some(w) if *g == "unknown" => { unknown += 1; let _ = w; }
                Some(w) if g == w => {
                    exact += 1;
                    if copies {
                        failures.push(format!("{name} {fname} is listed as differing because the \
                            self-hosted emitter copies, but the two agree — delete it from \
                            COPIES_INSTEAD"));
                    }
                }
                Some(w) => {
                    if !copies {
                        failures.push(format!("{name} {fname}: `neant cost` says [{w}], the \
                            self-hosted pass says [{g}]"));
                    }
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{} disagreements:\n\n{}", failures.len(), failures.join("\n"));
    assert_eq!(exact, EXACT, "the self-hosted pass reproduces {exact} work columns exactly, not \
        {EXACT}; {unknown} more it declines. Widening or narrowing the slice means changing EXACT.");
    assert_eq!(exact_moves, EXACT_MOVES, "the self-hosted pass reproduces {exact_moves} moves \
        columns exactly, not {EXACT_MOVES}.");
}

//! The cost calculus's exit test (docs/self-hosting-cost-design.md §8). `neant cost` prints a
//! `work` column per function; the self-hosted pass prints the same column, and **the two strings
//! must be identical** — the same rational reduction, the same term order, the same printing.
//! String equality is deliberately strict: it catches `n²/2 − n/2` written as `0.5·n² − 0.5·n`,
//! and a term order that happens to agree on this corpus and not in general.
//!
//! Where the Rust compiler itself declines — `fib`, whose two calls each shrink the measure by a
//! constant and so cost exponentially — the self-hosted pass must decline too. A cost calculus
//! that guesses is worse than one that declines, so that is asserted and not merely tolerated.
//!
//! The `work` column has **no declines left**: every in-slice golden function is either reproduced
//! exactly or differs for one of the two recorded reasons.
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
/// **Empty.** It held the four `main`s where this emitter copied a whole-array assignment the Rust
/// proves is in place — the last divergence between the two compilers, closed when the self-hosted
/// checker learned to make that proof itself. Kept as a list rather than deleted because the next
/// deliberate divergence should land here and be argued, not absorbed.
const COPIES_INSTEAD: &[(&str, &str)] = &[];

/// Functions whose **footprint** this pass states more narrowly than `neant cost`: it reports none
/// where the Rust reports one. A narrowing, not a disagreement — the two never state *different*
/// footprints, and a missing one only costs a caller a credit it could have had.
///
/// `parse`'s `number` is the whole list. Its `while` carries a `decreasing` measure the compiler
/// cannot follow, so both compilers call its cost unknown; the Rust has still recorded the site on
/// `pos` by then and this walk has not, because it stops at the loop it cannot bound instead of
/// walking the body for sites it will not cost. Widening it means collecting sites past a decline,
/// which is a change to the walk, not to the footprint.
const FOOTPRINT_NARROWER: &[(&str, &str)] = &[("parse.nt", "number")];

/// The same for the **footprint lower bound**: stated where `neant cost` states one and this pass
/// states none. A `while` loop is given no loop atom by this pass — only a `for` mints one — so a
/// site inside one has no image to count and no bound follows from it. `moves` for both of these is
/// exact; it is only the bound that is missing, and in the safe direction.
///
/// `owned.nt`'s `sum_doubled` is the third, for a different reason: its bound is its *callee's*,
/// handed up — "a bound on a part is a bound on the whole, once" — and this pass states a function's
/// bound from its own sites only. `sum_doubled` indexes no parameter array; it passes one to
/// `doubled` and walks what comes back.
const BOUND_NARROWER: &[(&str, &str)] = &[("while.nt", "count_lt"), ("while.nt", "first_zero"),
                                          ("owned.nt", "sum_doubled")];

/// The number of functions whose `work` the self-hosted pass reproduces exactly. In the test so
/// that widening the slice means changing a number someone has to look at.
const EXACT: usize = 98;

/// The same for `moves`, whose slice is narrower: a function that calls anything is unknown,
/// because a callee's traffic depends on what is already resident — which it now computes, so a
/// call is unknown only inside a loop or into a callee whose own cost forks (design §14). Five of
/// these are
/// **piecewise** — a scattered walk costs the array's footprint when it fits in `M` and a line per
/// touch when it does not — and their two regimes and the condition between them are compared as
/// one string, exactly as the single-piece ones are (design §13).
///
/// Nested loops settle here too, since `settle_moves` (design §18): `matmul`, `stencil` and `tri`'s
/// `pairs` and the `main`s that call them, whose regimes are compared piece by piece including the
/// order the report lists them in.
///
/// **Nothing is declined.** Every `moves` column either matches or is one of the four in
/// `COPIES_INSTEAD` — a footprint is a range now, so a callee that reads two fields of a four-field
/// particle leaves half the array resident and the next call over the other half pays in full.
const EXACT_MOVES: usize = 98;

/// The same for the **footprint**: one entry per array parameter and the condition under which the
/// whole of it is resident on return, whitespace-normalised so the report's column padding is not
/// part of the comparison. Counted over every function, so one that should state no footprint and
/// states none counts too.
const EXACT_FOOT: usize = 115;

/// The same for the **footprint lower bound** — `moves` cannot be less than the distinct bytes a
/// function's parameter arrays reach. Counted over every function, so a `main` that should have no
/// bound and gets none counts too: a bound invented where `neant cost` states none is as wrong as
/// a missing one, and only one of those two shows up as a difference.
const EXACT_BOUNDS: usize = 113;




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

/// The `moves` column. A function whose cost is piecewise has an empty column and its regimes on
/// the indented lines that follow; those are joined into `poly if cond | poly if cond`, which is
/// what `compiler/costdump.nt` prints for a forked cost, so the two still compare as strings.
fn rust_moves(out: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    let lines: Vec<&str> = out.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with(' ') || line.trim().is_empty() { continue; }
        let Some(k) = line.find(" moves ") else { continue };
        let name = line.split_whitespace().next().unwrap_or("").to_string();
        let rest = &line[k + 7..];
        let Some(j) = rest.find("  ") else { continue };
        let (mv, tag) = (rest[..j].trim(), &rest[j..]);
        if !mv.is_empty() { m.insert(name, mv.to_string()); continue; }
        if !tag.contains("regime") { continue; }
        // the pieces, in the order the report lists them
        let mut pieces = Vec::new();
        for l in lines[i + 1..].iter().take_while(|l| l.starts_with(' ')) {
            let t = l.trim();
            let Some(p) = t.strip_prefix("moves ") else { continue };
            // one space, not two: the report pads the polynomial into a column, and a polynomial
            // long enough to fill it leaves a single space — which silently dropped `pairs`'
            // first regime and compared the rest against a truncated expectation
            let Some(c) = p.find(" if ") else { continue };
            pieces.push(format!("{} if {}", p[..c].trim(), p[c + 4..].trim()));
        }
        if !pieces.is_empty() { m.insert(name, pieces.join(" | ")); }
    }
    m
}

/// The `lower bound … (footprint, …)` line of `neant cost`, per function. A function with no such
/// line is not in the map, and the self-hosted pass must print `none` for it — the half of this
/// that keeps a bound from being invented.
/// The `footprint …` line of `neant cost`, per function, in the shape the self-hosted pass prints:
/// the entries with runs of spaces collapsed, then `| resident <poly>` or `| none`.
fn rust_foot(out: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    let mut cur = String::new();
    for line in out.lines() {
        if !line.starts_with(' ') && !line.trim().is_empty() {
            cur = line.split_whitespace().next().unwrap_or("").to_string();
            continue;
        }
        let t = line.trim();
        let Some(rest) = t.strip_prefix("footprint") else { continue };
        let rest = rest.trim();
        let (entries, res) = match rest.find("  no residue claimed") {
            Some(i) => (&rest[..i], "| none".to_string()),
            None => match rest.find("  resident after if ") {
                Some(i) => {
                    let c = &rest[i + "  resident after if ".len()..];
                    (&rest[..i], format!("| resident {}", c.trim_end().trim_end_matches(" < M")))
                }
                None => (rest, "| none".to_string()),
            },
        };
        let e = entries.split_whitespace().collect::<Vec<_>>().join(" ");
        m.insert(cur.clone(), format!("{e} {res}").trim().to_string());
    }
    m
}

fn rust_bounds(out: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    let mut cur = String::new();
    for line in out.lines() {
        if !line.starts_with(' ') && !line.trim().is_empty() {
            cur = line.split_whitespace().next().unwrap_or("").to_string();
            continue;
        }
        let t = line.trim();
        let Some(rest) = t.strip_prefix("lower bound") else { continue };
        if !rest.contains("(footprint") { continue; }
        let Some(p) = rest.trim().strip_prefix("moves ") else { continue };
        let b = match p.find("  ") { Some(i) => &p[..i], None => p };
        m.insert(cur.clone(), b.trim().to_string());
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
    let mut exact_bounds = 0;
    let mut exact_foot = 0;
    let mut rejected = 0;
    for f in &files {
        // only what the self-hosted compiler can read
        if Command::new(neant()).arg("parsedump").arg(f).output().unwrap().status.code() == Some(2) { continue; }
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        // **A program `neant check` rejects must be rejected here too**, and for the ones it
        // rejects over a `#[cost]` bound that is the only test of the assertion rule — the checker
        // parity test runs the checker alone and cannot see a breach, because whether a bound holds
        // is a question for the cost pass. Checked here, where the cost pass is.
        if !Command::new(neant()).arg("check").arg(f).output().unwrap().status.success() {
            let run = Command::new(&reporter)
                .stdin(std::fs::File::open(f).unwrap()).output().unwrap();
            let out = String::from_utf8_lossy(&run.stdout);
            // the reporter prints the checker's verdict and then the parser's; rejected by
            // either is rejected
            let mut vs = out.lines();
            let v = vs.next().unwrap_or("");
            let vp = vs.next().unwrap_or("");
            if v == "0" && vp == "0" {
                failures.push(format!("{name}: `neant check` rejects it, the self-hosted pass \
                    accepts it — a breached `#[cost]` bound is a check error"));
            }
            rejected += 1;
            continue;
        }

        let report = String::from_utf8_lossy(
            &Command::new(neant()).arg("cost").arg(f).output().unwrap().stdout).into_owned();
        let want = rust_report(&report);
        let want_moves = rust_moves(&report);
        let want_bounds = rust_bounds(&report);
        let want_foot = rust_foot(&report);
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
        let got_bounds: BTreeMap<&str, &str> = rows.iter()
            .filter(|r| r.len() >= 4).map(|r| (r[0], r[3])).collect();
        let got_foot: BTreeMap<&str, &str> = rows.iter()
            .filter(|r| r.len() >= 5).map(|r| (r[0], r[4])).collect();
        for (fname, g) in &got_foot {
            let listed = FOOTPRINT_NARROWER.contains(&(name.as_str(), fname));
            match (want_foot.get(*fname), *g) {
                (None, "none") => exact_foot += 1,
                (None, other) => failures.push(format!("{name} {fname}: `neant cost` states no \
                    footprint, the self-hosted pass invented [{other}]")),
                (Some(_), "none") if listed => {}
                (Some(w), "none") => failures.push(format!("{name} {fname}: `neant cost` states \
                    footprint [{w}], the self-hosted pass states none and is not listed as narrower")),
                (Some(w), g) if w == g => exact_foot += 1,
                (Some(w), g) => failures.push(format!("{name} {fname}: `neant cost` states \
                    footprint [{w}], the self-hosted pass says [{g}]")),
            }
        }
        for (fname, g) in &got_bounds {
            if BOUND_NARROWER.contains(&(name.as_str(), fname)) && *g == "none" {
                assert!(want_bounds.contains_key(*fname),
                    "{name} {fname} is listed as a narrower bound, but `neant cost` states none");
                continue;
            }
            match (want_bounds.get(*fname), *g) {
                (None, "none") => exact_bounds += 1,
                (None, other) => failures.push(format!("{name} {fname}: `neant cost` states no \
                    footprint bound, the self-hosted pass invented [{other}]")),
                (Some(w), "none") => failures.push(format!("{name} {fname}: `neant cost` bounds \
                    moves below by [{w}], the self-hosted pass states none")),
                (Some(w), g) if w == g => exact_bounds += 1,
                (Some(w), g) => failures.push(format!("{name} {fname}: `neant cost` bounds moves \
                    below by [{w}], the self-hosted pass says [{g}]")),
            }
        }

        for (fname, mw) in &want_moves {
            let Some(g) = got_moves.get(fname.as_str()) else { continue };
            if *g == "unknown" { continue; }
            // the only reason a `moves` column still differs: the emitter copies a whole-array
            // assignment the Rust proves is in place. The layout family is gone — the self-hosted
            // compiler chooses AoS or SoA for itself now (docs/self-hosting-layout-design.md).
            let listed = COPIES_INSTEAD.contains(&(name.as_str(), fname.as_str()));
            if g == mw {
                exact_moves += 1;

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
    assert!(rejected >= 1, "no in-slice program is rejected any more; the assertion rule has \
        nothing left testing it");
    assert_eq!(exact_foot, EXACT_FOOT, "the self-hosted pass agrees on {exact_foot} footprint \
        columns, not {EXACT_FOOT}.");
    assert_eq!(exact_bounds, EXACT_BOUNDS, "the self-hosted pass agrees on {exact_bounds} \
        footprint-bound columns, not {EXACT_BOUNDS}.");
}

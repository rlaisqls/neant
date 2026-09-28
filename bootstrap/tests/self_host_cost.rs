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

/// `(file, function)` where the self-hosted `moves` are higher because it counts neighbouring
/// sites as separate streams: `stencil`'s five reads of `src` share three rows, which the Rust
/// charges once (cost-model § Moves, neighbouring sites, 2026-09-27) and this pass, which has no
/// such rule, charges as five. A deliberate divergence, argued here as the list above asks: the
/// self-hosted pass states more, never less, and `main` inherits it through the call.
const NEIGHBOURS_INSTEAD: &[(&str, &str)] = &[("stencil.nt", "stencil"), ("stencil.nt", "main")];

/// `(file, function)` where the self-hosted `moves` are higher because it sums a triangular loop's
/// laps: `tri`'s `pairs` runs `j in i..a.len()`, which the Rust charges the hull of its laps once
/// while they fit (cost-model § Moves, a triangle, 2026-09-28) and this pass charges lap by lap.
const TRIANGLES_INSTEAD: &[(&str, &str)] = &[("tri.nt", "pairs"), ("tri.nt", "main")];

/// `(file, function)` where the self-hosted `moves` are higher because a call whose every
/// footprint range is resident still pays what the credit leaves above zero there; the Rust moves
/// nothing for it (cost-model § Moves, a resident call, 2026-09-28). Each is a `main` calling the
/// same callee twice over the same arrays.
const RESIDENT_INSTEAD: &[(&str, &str)] = &[("arrayview.nt", "main"), ("chains.nt", "main"), ("dot.nt", "main"),
    ("particles.nt", "main"), ("repeat.nt", "main"), ("saxpy.nt", "main"), ("structs.nt", "main"),
    ("warm.nt", "main"), ("while.nt", "main"), ("words.nt", "main")];

/// Functions whose **footprint** this pass states more narrowly than `neant cost`: it reports none,
/// or fewer arrays, where the Rust reports one. A narrowing, not a disagreement — the two never state *different*
/// footprints, and a missing one only costs a caller a credit it could have had.
///
/// `parse`'s `number` is the whole list. Its `while` carries a `decreasing` measure the compiler
/// cannot follow, so both compilers call its cost unknown; the Rust has still recorded the site on
/// `pos` by then and this walk has not, because it stops at the loop it cannot bound instead of
/// walking the body for sites it will not cost. Widening it means collecting sites past a decline,
/// which is a change to the walk, not to the footprint.
///
/// `bfs.nt`'s `bfs` is the same stop one loop later, and states a subset rather than none: its
/// inner loop runs `offsets[u]..offsets[u + 1]`, which the Rust now bounds by a size read from
/// memory (stage D (2)) and walks, reaching `edges`; this pass has no read atoms, declines that
/// loop, and states the three arrays it touched before it. A listed function may state fewer
/// entries than `neant cost`, never a different one, and claims no residue.
///
/// `walk.nt`'s four are the same stop at the first loop: each is a `while s >= 0` down a list, which
/// the Rust bounds by the longest walk along the link (stage D (3)) and this pass, with no walk
/// atom, declines before it has recorded a site.
///
/// The last three stop after a site, not before one: `parse.nt`'s `atom` at its call to `number`,
/// which this pass cannot cost; `modulo.nt`'s `twice` at a callee whose cost is unknown, which the
/// Rust makes a term (stage D (1)) and this pass declines; and `widen.nt`'s `pairs` at a call
/// whose argument is a read. A walk that stopped has not seen every site, so what it saw is stated
/// as whole arrays with no residue — what the first pair of the list's rule asks, and exactly what
/// it once did not do: it stated `grid`'s `xs[a]` alone as exact and resident, having stopped at
/// `0..xs[a]` before reaching `xs[b]`.
///
/// `arena_tree.nt`'s refused `chase` reads three fields of one AoS element; this pass states the first field's
/// eight bytes of it, a subset of the element the Rust states.
///
/// `slots.nt`'s three loop to a bound read from the array they write, which this pass, with no
/// read atoms at all, declines before recording it.
///
/// `particles.nt`'s `step` and `tri.nt`'s `pairs` state a whole array here where the Rust states
/// its ranges: two SoA fields kept apart, and a triangle's lanes widened to their hull (cost-model
/// § Moves, footprint, 2026-09-28) — a whole array is the wider claim and credits nothing.
///
/// `whileshapes.nt`'s two are the condition this pass still misreads as the Rust did until
/// 2026-09-27: `j > start` with both sides locals taken as a loop in `start`, and `i + 1 < n` with
/// no variable alone on a side; it declines them before recording `xs`.
///
/// `lexer.nt`'s `words` is a scan whose index moves only inside nested loops that surely run
/// (§ A scan), which this pass, with no scan rule, declines before it records `xs`.
///
/// `worklist.nt`'s `reach` is a worklist the Rust bounds (§ A bounded worklist) and this pass,
/// which has no rule for a bound the body pushes, declines before recording `next` — rightly now:
/// until 2026-09-27 it missed an assignment in an `if` at a block's tail and costed such a loop as
/// if its bound held still.
///
/// `amortised.nt`'s `field` is that stop too, a `while i < xs.len() && …`; `sum_fields` stops at
/// its call to `field` inside a loop the Rust amortises (§ An amortised scan) and this pass, with no
/// scan rule, cannot bound, after writing `out` but before recording it.
///
/// `scan.nt`'s `word_end` and `trimmed`, and `scan_refused.nt`'s `word_end`, are the same stop
/// again: each loop is `while i < xs.len() && xs[i] … `, whose trip the Rust takes from the first
/// conjunct (cost-model § Loops without a range) — in `trimmed`'s second loop by a scan (§ A scan)
/// — and this pass, with neither rule, declines before it has recorded the site on `xs`.
const FOOTPRINT_NARROWER: &[(&str, &str)] = &[("parse.nt", "number"), ("bfs.nt", "bfs"),
                                              ("walk.nt", "sum_list"), ("walk.nt", "sum_all"),
                                              ("walk.nt", "double_list"), ("walk.nt", "chain_len"),
                                              ("parse.nt", "atom"), ("modulo.nt", "twice"),
                                              ("widen.nt", "pairs"),
                                              ("scan.nt", "word_end"), ("scan.nt", "trimmed"),
                                              ("scan_refused.nt", "word_end"),
                                              ("amortised.nt", "field"), ("amortised.nt", "sum_fields"),
                                              ("worklist.nt", "reach"), ("lexer.nt", "words"),
                                              ("whileshapes.nt", "from_one"), ("whileshapes.nt", "sort_from"),
                                              ("particles.nt", "step"), ("tri.nt", "pairs"),
                                              ("slots.nt", "count"), ("slots.nt", "fields"), ("slots.nt", "rewrite"),
                                              ("arena_tree.nt", "chase")];

/// Functions whose **cost this pass declines**, where the two footprints differ: a footprint
/// without a cost is used by no caller, which is then without a cost too. `neant cost` states the
/// whole arena for a recursion over a tree it costs, which visits every node it reaches
/// (docs/cost-model.md § Recursion, a tree; a forest), where this pass states the body's own, one
/// node, or none; and for a mutual recursion both decline, this pass states the whole arena where
/// `neant cost` states the body's own. A scan to a sentinel, a cursor in a slot and a loop to an
/// element read are costed there and declined here.
const DECLINED_FOOT: &[(&str, &str)] = &[("arena_tree.nt", "sum"), ("arena_tree.nt", "same"),
                                         ("arena_tree.nt", "deep"), ("forest.nt", "grow"),
                                         ("forest.nt", "spell"),
                                         ("forest.nt", "has"), ("forest.nt", "has_list"),
                                         ("forest.nt", "size"), ("forest.nt", "size_list"),
                                         ("forest.nt", "dup"), ("forest.nt", "dup2"),
                                         ("forest.nt", "lap"), ("forest.nt", "lap_one"),
                                         ("forest.nt", "deep"), ("forest.nt", "deep_list"), ("sentinel.nt", "find"),
                                         ("sentinel.nt", "skip"), ("sentinel.nt", "stuck"),
                                         ("cursor.nt", "skip_sp"), ("cursor.nt", "via_call"),
                                         ("cursor.nt", "moving_end"), ("cursor.nt", "upto"),
                                         ("cursor.nt", "pass"), ("cursor.nt", "repeat")];

/// The same for the **footprint lower bound**: stated where `neant cost` states one and this pass
/// states none. A `while` loop is given no loop atom by this pass — only a `for` mints one — so a
/// site inside one has no image to count and no bound follows from it. `moves` for both of these is
/// exact; it is only the bound that is missing, and in the safe direction.
///
/// `owned.nt`'s `sum_doubled` is the third, for a different reason: its bound is its *callee's*,
/// handed up — "a bound on a part is a bound on the whole, once" — and this pass states a function's
/// bound from its own sites only. `sum_doubled` indexes no parameter array; it passes one to
/// `doubled` and walks what comes back. `amortised.nt`'s `sum_fields` has no lower bound here for
/// the reason it has no footprint: the walk declined its loop before recording `out`.
const BOUND_NARROWER: &[(&str, &str)] = &[("while.nt", "count_lt"), ("while.nt", "first_zero"),
                                          ("owned.nt", "sum_doubled"), ("amortised.nt", "sum_fields"),
                                          ("whileshapes.nt", "from_one")];

/// The number of functions whose `work` the self-hosted pass reproduces exactly. In the test so
/// that widening the slice means changing a number someone has to look at.
const EXACT: usize = 106;

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
const EXACT_MOVES: usize = 92;

/// The same for the **footprint**: one entry per array parameter and the condition under which the
/// whole of it is resident on return, whitespace-normalised so the report's column padding is not
/// part of the comparison. Counted over every function, so one that should state no footprint and
/// states none counts too.
const EXACT_FOOT: usize = 149;

/// The same for the **footprint lower bound** — `moves` cannot be less than the distinct bytes a
/// function's parameter arrays reach. Counted over every function, so a `main` that should have no
/// bound and gets none counts too: a bound invented where `neant cost` states none is as wrong as
/// a missing one, and only one of those two shows up as a difference.
const EXACT_BOUNDS: usize = 192;




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

/// Whether footprint `g` names only arrays `w` names too, each with `w`'s range or the whole array,
/// and claims no residue: what a walk that stopped early states of what a longer walk found. The
/// whole array claims less than a range does — a fit test may use it, a credit may not. A range
/// from the same start and shorter is inside `w`'s, and then a residue no larger than `w`'s is too.
fn foot_subset(w: &str, g: &str) -> bool {
    fn entries(s: &str) -> (Vec<String>, String) {
        let (es, res) = s.rsplit_once(" | ").map_or((s, s), |(a, b)| (a, b));
        let toks: Vec<&str> = es.split(' ').collect();
        let mut out: Vec<String> = Vec::new();
        for (i, t) in toks.iter().enumerate() {
            let starts = t.ends_with(':') && toks.get(i + 1).is_some_and(|n| n.starts_with('['));
            match out.last_mut() {
                Some(e) if !starts => { e.push(' '); e.push_str(t); }
                _ => out.push(t.to_string()),
            }
        }
        (out, res.to_string())
    }
    // `x: [lo, lo + c)`, as the array, `lo` and `c`: a range this pass states shorter from the
    // same start is inside the Rust's
    fn span(e: &str) -> Option<(&str, &str, u64)> {
        let (a, r) = e.split_once(": [")?;
        let (lo, hi) = r.strip_suffix(')')?.split_once(", ")?;
        let c = hi.strip_prefix(lo)?.strip_prefix(" + ")?.parse().ok()?;
        Some((a, lo, c))
    }
    let resident = |r: &str| r.strip_prefix("resident ").and_then(|n| n.parse::<u64>().ok());
    let ((we, wr), (ge, gr)) = (entries(w), entries(g));
    let none = gr == "none" || gr == "| none"
        || resident(&gr).is_some_and(|g| resident(&wr).is_some_and(|w| g <= w));
    let array = |e: &str| e.split(':').next().unwrap_or("").to_string();
    none && ge.iter().all(|e| we.contains(e)
        || (e.ends_with("(whole array)") && we.iter().any(|x| array(x) == array(e)))
        || span(e).is_some_and(|(a, lo, c)| we.iter().any(|x| span(x)
            .is_some_and(|(b, lo2, c2)| a == b && lo == lo2 && c <= c2))))
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
                (Some(_), _) if DECLINED_FOOT.contains(&(name.as_str(), fname))
                    && got.get(fname) == Some(&"unknown") => {}
                (Some(_), "none") if listed => {}
                (Some(w), "none") => failures.push(format!("{name} {fname}: `neant cost` states \
                    footprint [{w}], the self-hosted pass states none and is not listed as narrower")),
                (Some(w), g) if w == g => exact_foot += 1,
                (Some(w), g) if listed && foot_subset(w, g) => {}
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
            let listed = COPIES_INSTEAD.contains(&(name.as_str(), fname.as_str()))
                || NEIGHBOURS_INSTEAD.contains(&(name.as_str(), fname.as_str()))
                || TRIANGLES_INSTEAD.contains(&(name.as_str(), fname.as_str()))
                || RESIDENT_INSTEAD.contains(&(name.as_str(), fname.as_str()));
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

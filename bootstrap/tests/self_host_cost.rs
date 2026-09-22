//! The cost calculus's exit test (docs/self-hosting-cost-design.md §8). `neant cost` prints a
//! `work` column per function; the self-hosted pass prints the same column, and **the two strings
//! must be identical** — the same rational reduction, the same term order, the same printing.
//! String equality is deliberately strict: it catches `n²/2 − n/2` written as `0.5·n² − 0.5·n`,
//! and a term order that happens to agree on this corpus and not in general.
//!
//! Where the Rust compiler solves a recurrence — a `while` with a measure, or self-recursion — the
//! self-hosted pass must say **unknown** rather than a number. A cost calculus that guesses is
//! worse than one that declines, so that is asserted and not merely tolerated.
//!
//! One group of functions differs **on purpose**, and is listed by name. `ys = xs` on whole arrays
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
const EXACT: usize = 53;

fn driver() -> String {
    format!(r#"extern fn read_stdin(buf: &mut [u8]) -> i64 uses io, unbounded;
extern fn write_stdout(buf: &[u8], n: i64) -> i64 uses io, unbounded;

fn main() {{
    let mut buf = [b'\0'; 262144];
    let n = read_stdin(&mut buf);
    let mut toks = [Token {{ kind: 0, start: 0, len: 0, ival: 0 }}; 262144];
    let n_toks = lex(&buf, n, &mut toks);
    let mut nodes = [Node {{ kind: 0, a: 0, b: 0, c: 0, d: 0, ival: 0, next: 0 }}; 262144];
    let mut st = [0, 0, 0, 0, 0];
    let first = parse_program(&buf, &toks, n_toks, &mut st, &mut nodes);
    let mut types = [Ty {{ kind: 0, elem: 0, mutable: 0, size: 0 }}; 65536];
    let mut syms = [Sym {{ name: 0, ty: 0, mutable: 0 }}; 65536];
    let mut sigs = [Sig {{ name: -1, params: 0, n_params: 0, ret: 0, ext: 0 }}; 4096];
    let mut strs = [Str {{ name: 0, fields: 0, n_fields: 0 }}; 1024];
    let mut flds = [Fld {{ name: 0, ty: 0 }}; 4096];
    let mut ptys = [0; 16384];
    let mut ntys = [-1; 262144];
    let mut cst = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let bad = check_program(&buf, &toks, &nodes, &mut types, &mut syms, &mut sigs, &mut ptys, &mut strs, &mut flds, &mut ntys, &mut cst, first);

    let mut facs = [Fac {{ atom: 0, exp: 0 }}; 262144];
    let mut terms = [Term {{ fac: 0, n_fac: 0, cn: 0, cd: 0 }}; 262144];
    let mut pols = [Pol {{ term: 0, n_term: 0 }}; 262144];
    let mut pst = [0, 0, 0, 0, 0];
    let mut env = [Env {{ name: 0, val: 0, len: 0 }}; 4096];
    let mut wst = [0, 0, 0, 0];
    let mut scratch = [0; 64];
    let mut order = [0; 4096];
    let mut fw = [-1; 4096];
    let mut pnames = [0; 256];
    let mut parr = [0; 256];
    let mut out = [b'\0'; 262144];
    let mut est = [0, 0, 0];

    // two passes, so a call to a function declared later still finds its work
    let mut pass = 0;
    while pass < 2 {{
        let mut f = first;
        let mut si = 0;
        while f >= 0 {{
            if nodes[f].kind == 110 {{
                fw[si] = w_func(&buf, &toks, &nodes, &types, &ntys, &sigs, &ptys, &fw, &mut facs, &mut terms, &mut pols, &mut pst, &mut env, &mut wst, &mut scratch, f, si);
                si += 1;
            }} else if nodes[f].kind == 116 {{
                si += 1;
            }}
            f = nodes[f].next;
        }}
        pass += 1;
    }}

    let mut f = first;
    let mut si = 0;
    while f >= 0 {{
        if nodes[f].kind == 110 {{
            emit_span(&mut out, &mut est, &buf, &toks, nodes[f].a);
            emit_b(&mut out, &mut est, b'\t');
            if fw[si] < 0 {{
                let u = b"unknown";
                emit_bytes(&mut out, &mut est, &u);
            }} else {{
                let mut p = nodes[f].b;
                let mut i = 0;
                while p >= 0 {{
                    pnames[i] = nodes[p].a;
                    let t = ptys[sigs[si].params + i];
                    parr[i] = if t >= 0 && (types[t].kind == 5 || types[t].kind == 6) {{ 1 }} else {{ 0 }};
                    i += 1;
                    p = nodes[p].next;
                }}
                let before = est[2];
                pol_print(&mut out, &mut est, &buf, &toks, &facs, &terms, &pols, &pnames, &parr, fw[si], &mut order);
                if est[2] != before {{
                    est[2] = before;
                    let u = b" [unprintable]";
                    emit_bytes(&mut out, &mut est, &u);
                }}
            }}
            emit_b(&mut out, &mut est, b'\n');
            si += 1;
        }} else if nodes[f].kind == 116 {{
            si += 1;
        }}
        f = nodes[f].next;
    }}
    println(bad);
    println(st[2]);
    write_stdout(&out, est[0]);
}}
"#)
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

#[test]
fn self_hosted_work_agrees_with_bootstrap() {
    let stages: String = ["compiler/lex.nt", "compiler/parse.nt", "compiler/check.nt",
                          "compiler/emit.nt", "compiler/poly.nt", "compiler/cost.nt"]
        .iter().map(|p| std::fs::read_to_string(repo(p)).unwrap()).collect();
    let dir = std::env::temp_dir().join(format!("neant-cost-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let nt = dir.join("cost_driver.nt");
    std::fs::write(&nt, format!("{stages}\n{}", driver())).unwrap();

    let mut files: Vec<PathBuf> = std::fs::read_dir(repo("tests/golden")).unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "nt"))
        .collect();
    files.sort();

    let (mut exact, mut unknown, mut failures) = (0, 0, Vec::new());
    for f in &files {
        // only what the self-hosted compiler can read, and only what checks
        if Command::new(neant()).arg("parsedump").arg(f).output().unwrap().status.code() == Some(2) { continue; }
        if !Command::new(neant()).arg("check").arg(f).output().unwrap().status.success() { continue; }
        let name = f.file_name().unwrap().to_string_lossy().to_string();

        let want = rust_report(&String::from_utf8_lossy(
            &Command::new(neant()).arg("cost").arg(f).output().unwrap().stdout));
        let run = Command::new(neant()).arg("run").arg(&nt)
            .stdin(std::fs::File::open(f).unwrap()).output().unwrap();
        assert!(run.status.success(), "the self-hosted cost driver failed on {name}:\n{}",
            String::from_utf8_lossy(&run.stderr));
        let out = String::from_utf8_lossy(&run.stdout);
        let mut lines = out.lines();
        assert_eq!(lines.next(), Some("0"), "{name}: the self-hosted checker rejected it");
        assert_eq!(lines.next(), Some("0"), "{name}: the self-hosted parser rejected it");
        let got: BTreeMap<&str, &str> = lines
            .filter_map(|l| l.split_once('\t')).collect();

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
}

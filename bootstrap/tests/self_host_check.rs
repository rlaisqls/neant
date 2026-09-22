//! The self-hosted checker's exit test (docs/self-hosting-checker-design.md §6), in two halves.
//!
//! **Verdict parity**: on every `tests/golden/*.nt` file inside the parser's slice, the
//! self-hosted checker must accept exactly when `neant check` does. That set includes six
//! negative goldens, each firing a different rule (arity, `break` outside a loop, `if` branch
//! types, no implicit numeric conversion, mutability, a missing return value).
//!
//! **Struct probes**: the corpus's only in-slice struct programs are the three negative goldens,
//! so rejecting every struct would still look like parity. The probes below are written here in
//! accepted and rejected groups, and each verdict must match `neant check`'s.
//!
//! **Types, pinned**: `fib.nt`'s type code for every expression, in depth-first order, hand-checked
//! once against the source. Accept/reject alone would not notice `1 + 2` typed as `f64`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn neant() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_neant"))
}

fn repo(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(sub)
}

/// `fib.nt`, checked by hand: the function bodies' and every expression's type code, depth-first.
/// `0` i64, `2` bool, `4` unit. `fib`: body `0`, the else-less `if` `4`, its condition `2`, …
const FIB_TYPES: &str = "0\n4\n2\n0\n0\n4\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n2\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n4\n4\n0\n0\n4\n0\n0\n";

fn driver(path: &str) -> String {
    format!(r#"
extern fn read_file(path: &[u8], buf: &mut [u8]) -> i64 uses io, unbounded;

fn type_code(types: &[Ty], t: i64) -> i64 {{
    if t < 0 {{ return -1; }}
    let k = types[t].kind;
    if k < 5 {{ return k; }}
    let e = types[t].elem;
    if e < 0 || types[e].kind >= 5 {{ return 99; }}
    if k == 5 {{ return 10 + types[e].kind; }}
    if types[t].mutable == 1 {{ return 30 + types[e].kind; }}
    20 + types[e].kind
}}

fn dump_ty(nodes: &[Node], types: &[Ty], ntys: &[i64], n: i64) {{
    if n < 0 {{ return; }}
    let k = nodes[n].kind;
    if k >= 60 && k < 90 {{ println(type_code(types, ntys[n])); }}
    if k == 66 || k == 68 || k == 74 || k == 102 || k == 91 {{
        dump_ty(nodes, types, ntys, nodes[n].a);
        dump_ty(nodes, types, ntys, nodes[n].b);
    }} else if k == 67 || k == 73 || k == 95 || k == 96 || k == 103 || k == 104 || k == 69 {{
        dump_ty(nodes, types, ntys, nodes[n].a);
    }} else if k == 75 || k == 93 {{
        dump_ty(nodes, types, ntys, nodes[n].a);
        dump_ty(nodes, types, ntys, nodes[n].b);
        dump_ty(nodes, types, ntys, nodes[n].c);
    }} else if k == 76 {{
        dump_ty_list(nodes, types, ntys, nodes[n].a);
        dump_ty(nodes, types, ntys, nodes[n].b);
    }} else if k == 90 {{
        dump_ty(nodes, types, ntys, nodes[n].b);
        dump_ty(nodes, types, ntys, nodes[n].c);
    }} else if k == 92 {{
        dump_ty(nodes, types, ntys, nodes[n].b);
        dump_ty(nodes, types, ntys, nodes[n].c);
        dump_ty(nodes, types, ntys, nodes[n].d);
    }} else if k == 71 {{
        dump_ty_list(nodes, types, ntys, nodes[n].b);
    }} else if k == 110 {{
        dump_ty_list(nodes, types, ntys, nodes[n].b);
        dump_ty(nodes, types, ntys, nodes[n].c);
        dump_ty(nodes, types, ntys, nodes[n].d);
    }} else if k == 111 {{
        dump_ty(nodes, types, ntys, nodes[n].b);
    }}
}}

fn dump_ty_list(nodes: &[Node], types: &[Ty], ntys: &[i64], head: i64) {{
    let mut n = head;
    while n >= 0 {{
        dump_ty(nodes, types, ntys, n);
        n = nodes[n].next;
    }}
}}

fn main() {{
    let path = b"{path}";
    let mut buf = [b'\0'; 65536];
    let n = read_file(&path, &mut buf);
    let mut toks = [Token {{ kind: 0, start: 0, len: 0, ival: 0 }}; 65536];
    let n_toks = lex(&buf, n, &mut toks);
    let mut nodes = [Node {{ kind: 0, a: 0, b: 0, c: 0, d: 0, ival: 0, next: 0 }}; 65536];
    let mut st = [0, 0, 0, 0, 0];
    let first = parse_program(&buf, &toks, n_toks, &mut st, &mut nodes);
    if st[2] != 0 {{
        println(2);
    }} else {{
        let mut types = [Ty {{ kind: 0, elem: 0, mutable: 0 }}; 4096];
        let mut syms = [Sym {{ name: 0, ty: 0, mutable: 0 }}; 4096];
        let mut sigs = [Sig {{ name: 0, params: 0, n_params: 0, ret: 0 }}; 1024];
        let mut strs = [Str {{ name: 0, fields: 0, n_fields: 0 }}; 1024];
        let mut flds = [Fld {{ name: 0, ty: 0 }}; 4096];
        let mut ptys = [0; 4096];
        let mut ntys = [-1; 65536];
        let mut cst = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let bad = check_program(&buf, &toks, &nodes, &mut types, &mut syms, &mut sigs, &mut ptys, &mut strs, &mut flds, &mut ntys, &mut cst, first);
        println(bad);
        if bad == 0 {{
            dump_ty_list(&nodes, &types, &ntys, first);
        }}
    }}
}}
"#)
}

/// Runs the self-hosted lexer + parser + checker over `file`; returns its stdout.
fn self_hosted(dir: &Path, stages: &str, file: &Path) -> String {
    let path = file.to_string_lossy();
    assert!(!path.contains(['"', '\\', '\n']), "path needs escaping: {path}");
    let nt = dir.join(format!("{}.nt", file.file_stem().unwrap().to_string_lossy()));
    std::fs::write(&nt, format!("{stages}\n{}", driver(&path))).unwrap();
    let out = Command::new(neant()).arg("run").arg(&nt).output().unwrap();
    assert!(out.status.success(), "self-hosted driver failed on {path}:\n{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn self_hosted_check_agrees_with_bootstrap() {
    let stages = ["compiler/lex.nt", "compiler/parse.nt", "compiler/check.nt"]
        .map(|p| std::fs::read_to_string(repo(p)).unwrap())
        .join("\n");
    let dir = std::env::temp_dir().join(format!("neant-self-host-check-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let mut files: Vec<PathBuf> = std::fs::read_dir(repo("tests/golden"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "nt"))
        .collect();
    files.sort();

    let (mut compared, mut failures) = (0, Vec::new());
    for f in &files {
        // only what the parser can read: `parsedump` exits 2 on anything else
        let parses = Command::new(neant()).arg("parsedump").arg(f).output().unwrap();
        if parses.status.code() == Some(2) { continue; }
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let want_ok = Command::new(neant()).arg("check").arg(f).output().unwrap().status.success();
        let got = self_hosted(&dir, &stages, f);
        let verdict = got.lines().next().unwrap_or("");
        // "0" accepted, "1" rejected by the checker, "2" rejected by the parser
        let got_ok = verdict == "0";
        compared += 1;
        if got_ok != want_ok {
            failures.push(format!("{name}: self-hosted said {verdict}, `neant check` said {}",
                if want_ok { "ok" } else { "rejected" }));
        }
        if name == "fib.nt" && got != format!("0\n{FIB_TYPES}") {
            failures.push(format!("fib.nt: types differ from the hand-checked pin\n--- got ---\n{got}"));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(compared >= 10, "only {compared} files were inside the parser's slice; the slice or the corpus changed");
    if !failures.is_empty() {
        panic!("{} of {compared} files disagree:\n\n{}", failures.len(), failures.join("\n"));
    }
}

/// Struct programs, written in pairs: the first group must be accepted and the second rejected,
/// and in both cases the self-hosted checker must agree with `neant check`. They differ by one
/// thing each, so a checker that simply rejected every struct would fail the first group and one
/// that accepted everything would fail the second.
const STRUCT_PROBES: &[(&str, &str)] = &[
    ("literal and field read", "struct P { x: i64, y: i64 }
fn main() { let p = P { x: 1, y: 2 }; println(p.x + p.y); }"),
    ("fields out of order", "struct P { x: i64, y: i64 }
fn main() { let p = P { y: 2, x: 1 }; println(p.x); }"),
    ("mixed field types", "struct P { x: i64, f: f64, b: bool }
fn main() { let p = P { x: 1, f: 2.0, b: true }; if p.b { println(p.x); } }"),
    ("struct parameter and return", "struct P { x: i64, y: i64 }
fn mk(a: i64) -> P { P { x: a, y: a + 1 } }
fn sum(p: P) -> i64 { p.x + p.y }
fn main() { println(sum(mk(3))); }"),
    ("field assignment", "struct P { x: i64, y: i64 }
fn main() { let mut p = P { x: 1, y: 2 }; p.x = 7; p.y += 1; println(p.x + p.y); }"),
    ("two structs with a shared field name", "struct A { v: i64 }
struct B { v: f64 }
fn main() { let a = A { v: 1 }; let b = B { v: 2.0 }; println(a.v); println(b.v); }"),
];

const STRUCT_PROBES_BAD: &[(&str, &str)] = &[
    ("unknown field read", "struct P { x: i64, y: i64 }
fn main() { let p = P { x: 1, y: 2 }; println(p.z); }"),
    ("field missing from the literal", "struct P { x: i64, y: i64 }
fn main() { let p = P { x: 1 }; println(p.x); }"),
    ("a field given twice, one omitted", "struct P { x: i64, y: i64 }
fn main() { let p = P { x: 1, x: 2 }; println(p.x); }"),
    ("field value of the wrong type", "struct P { x: i64, y: i64 }
fn main() { let p = P { x: 1, y: 2.0 }; println(p.x); }"),
    ("unknown field in the literal", "struct P { x: i64, y: i64 }
fn main() { let p = P { x: 1, z: 2 }; println(p.x); }"),
    ("a field read off a non-struct", "fn main() { let n = 1; println(n.x); }"),
    ("a struct field written through a non-`mut` binding", "struct P { x: i64, y: i64 }
fn main() { let p = P { x: 1, y: 2 }; p.x = 7; println(p.x); }"),
    ("an unknown type named in a signature", "fn f(p: Q) -> i64 { 1 }
fn main() { println(1); }"),
    ("a struct field whose type is another struct", "struct A { v: i64 }
struct B { a: A }
fn main() { println(1); }"),
    ("a struct where a scalar is wanted", "struct P { x: i64 }
fn main() { let p = P { x: 1 }; println(p.x + p); }"),
];

#[test]
fn self_hosted_check_agrees_on_struct_probes() {
    let stages = ["compiler/lex.nt", "compiler/parse.nt", "compiler/check.nt"]
        .map(|p| std::fs::read_to_string(repo(p)).unwrap())
        .join("\n");
    let dir = std::env::temp_dir().join(format!("neant-self-host-probe-{}", std::process::id()));
    // the probe sources go in their own directory: `self_hosted` names the driver after the file's
    // stem, so writing both side by side would have the driver overwrite the probe it reads
    let src_dir = dir.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    let mut failures = Vec::new();
    let all = STRUCT_PROBES.iter().map(|p| (true, p)).chain(STRUCT_PROBES_BAD.iter().map(|p| (false, p)));
    for (i, (want_ok, (what, src))) in all.enumerate() {
        let f = src_dir.join(format!("probe{i}.nt"));
        std::fs::write(&f, format!("{src}\n")).unwrap();
        let rust_ok = Command::new(neant()).arg("check").arg(&f).output().unwrap().status.success();
        if rust_ok != want_ok {
            failures.push(format!("{what}: `neant check` said {rust_ok}, the probe wants {want_ok}"));
            continue;
        }
        let verdict = self_hosted(&dir, &stages, &f).lines().next().unwrap_or("").to_string();
        if (verdict == "0") != want_ok {
            failures.push(format!("{what}: self-hosted said {verdict}, `neant check` said {}",
                if want_ok { "ok" } else { "rejected" }));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{} of {} probes disagree:\n\n{}",
        failures.len(), STRUCT_PROBES.len() + STRUCT_PROBES_BAD.len(), failures.join("\n"));
}

/// The fixpoint, as far as it goes: the self-hosted lexer, parser and checker, concatenated, are a
/// neant program of ~1500 lines — and the self-hosted lexer, parser and checker read it, parse it
/// and type-check it. Not "a program like the compiler": the compiler's own source, the three
/// stages that exist, checked by themselves.
///
/// `compiler/emit.nt` is deliberately not in the concatenation. It is still outside the slice —
/// its first `s.len()` is a method call, which the parser rejects by design — and this test says
/// where the frontier is, so widening the slice moves the frontier here and not only in prose.
#[test]
fn the_self_hosted_front_end_checks_its_own_source() {
    let names = ["compiler/lex.nt", "compiler/parse.nt", "compiler/check.nt"];
    let stages = names.map(|p| std::fs::read_to_string(repo(p)).unwrap()).join("\n");
    let dir = std::env::temp_dir().join(format!("neant-self-host-fixpoint-{}", std::process::id()));
    let src_dir = dir.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    // the subject: the three stages as one file, which is what a driver concatenates anyway
    let subject = src_dir.join("frontend.nt");
    std::fs::write(&subject, &stages).unwrap();
    let out = self_hosted(&dir, &stages, &subject);
    let verdict = out.lines().next().unwrap_or("");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(verdict, "0",
        "the self-hosted front end no longer accepts its own {} lines ({}): {}",
        stages.lines().count(),
        if verdict == "2" { "the parser rejected it" } else { "the checker rejected it" },
        names.join(" + "));
}

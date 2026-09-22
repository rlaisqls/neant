//! The self-hosted checker's exit test (docs/self-hosting-checker-design.md §6), in two halves.
//!
//! **Verdict parity**: on every `tests/golden/*.nt` file inside the parser's slice, the
//! self-hosted checker must accept exactly when `neant check` does. That set includes six
//! negative goldens, each firing a different rule (arity, `break` outside a loop, `if` branch
//! types, no implicit numeric conversion, mutability, a missing return value).
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
    let mut st = [0, 0, 0];
    let first = parse_program(&toks, n_toks, &mut st, &mut nodes);
    if st[2] != 0 {{
        println(2);
    }} else {{
        let mut types = [Ty {{ kind: 0, elem: 0, mutable: 0 }}; 4096];
        let mut syms = [Sym {{ name: 0, ty: 0, mutable: 0 }}; 4096];
        let mut sigs = [Sig {{ name: 0, params: 0, n_params: 0, ret: 0 }}; 1024];
        let mut ptys = [0; 4096];
        let mut ntys = [-1; 65536];
        let mut cst = [0, 0, 0, 0, 0, 0, 0];
        let bad = check_program(&buf, &toks, &nodes, &mut types, &mut syms, &mut sigs, &mut ptys, &mut ntys, &mut cst, first);
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

//! The self-hosted parser's exit test (docs/self-hosting-parser-design.md §7). For every
//! `tests/golden/*.nt` file that stays inside the first slice's grammar (§6 — no struct literals,
//! chains, closures, comprehensions, array literals, `extern`, attributes or `struct`
//! definitions), `compiler/lex.nt` + `compiler/parse.nt` run through the neant compiler itself
//! must produce the same depth-first sequence of node kinds `bootstrap/src/parse.rs` does
//! (`neant parsedump`, which exits 2 and names the construct on a file outside the slice, so
//! those are skipped rather than counted as failures).

use std::path::{Path, PathBuf};
use std::process::Command;

fn neant() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_neant"))
}

fn repo(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(sub)
}

/// The driver appended after `lex.nt` and `parse.nt`: reads one file, lexes, parses, and walks the
/// node arena depth-first in the field order the design's table fixes.
const DRIVER: &str = r#"
extern fn read_file(path: &[u8], buf: &mut [u8]) -> i64 uses io, unbounded;

fn dump_node(nodes: &[Node], n: i64) {
    if n < 0 { return; }
    let k = nodes[n].kind;
    println(k);
    if k == 66 || k == 68 || k == 74 || k == 102 || k == 91 || k == 78 {
        dump_node(nodes, nodes[n].a);
        dump_node(nodes, nodes[n].b);
    } else if k == 67 || k == 73 || k == 95 || k == 96 || k == 103 || k == 104 || k == 69 || k == 72 {
        dump_node(nodes, nodes[n].a);
    } else if k == 75 || k == 93 {
        dump_node(nodes, nodes[n].a);
        dump_node(nodes, nodes[n].b);
        dump_node(nodes, nodes[n].c);
    } else if k == 76 {
        dump_list(nodes, nodes[n].a);
        dump_node(nodes, nodes[n].b);
    } else if k == 90 {
        dump_node(nodes, nodes[n].b);
        dump_node(nodes, nodes[n].c);
    } else if k == 92 {
        dump_node(nodes, nodes[n].b);
        dump_node(nodes, nodes[n].c);
        dump_node(nodes, nodes[n].d);
    } else if k == 71 {
        dump_list(nodes, nodes[n].b);
    } else if k == 110 {
        dump_list(nodes, nodes[n].b);
        dump_node(nodes, nodes[n].c);
        dump_node(nodes, nodes[n].d);
    } else if k == 111 {
        dump_node(nodes, nodes[n].b);
    } else if k == 112 || k == 70 {
        dump_list(nodes, nodes[n].b);
    } else if k == 113 || k == 114 {
        dump_node(nodes, nodes[n].b);
    }
}

fn dump_list(nodes: &[Node], head: i64) {
    let mut n = head;
    while n >= 0 {
        dump_node(nodes, n);
        n = nodes[n].next;
    }
}

fn main() {
    let path = b"@PATH@";
    let mut buf = [b'\0'; 65536];
    let n = read_file(&path, &mut buf);
    let mut toks = [Token { kind: 0, start: 0, len: 0, ival: 0 }; 65536];
    let n_toks = lex(&buf, n, &mut toks);
    let mut nodes = [Node { kind: 0, a: 0, b: 0, c: 0, d: 0, ival: 0, next: 0 }; 65536];
    let mut st = [0, 0, 0, 0, 0];
    let first = parse_program(&buf, &toks, n_toks, &mut st, &mut nodes);
    if st[2] != 0 {
        println(-1);
    } else {
        dump_list(&nodes, first);
    }
}
"#;

#[test]
fn self_hosted_parse_matches_bootstrap() {
    let lex_nt = std::fs::read_to_string(repo("compiler/lex.nt")).unwrap();
    let parse_nt = std::fs::read_to_string(repo("compiler/parse.nt")).unwrap();
    let files: Vec<PathBuf> = std::fs::read_dir(repo("tests/golden"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "nt"))
        .collect();
    assert!(!files.is_empty(), "no golden programs found");

    let dir = std::env::temp_dir().join(format!("neant-self-host-parse-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (mut checked, mut rejected, mut skipped) = (0, 0, 0);
    let mut failures = Vec::new();
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let want = Command::new(neant()).arg("parsedump").arg(f).output().unwrap();
        // exit 0: parsed, in scope — compare the trees. exit 1: the bootstrap parser rejected it
        // (a negative golden) — require the self-hosted parser to reject it too, whatever its own
        // reason. exit 2: outside the first slice's grammar — nothing to compare.
        let code = want.status.code();
        if code == Some(2) {
            skipped += 1;
            continue;
        }
        let path = f.to_string_lossy();
        assert!(!path.contains(['"', '\\', '\n']), "path needs escaping: {path}");
        let driver = dir.join(format!("{}.nt", f.file_stem().unwrap().to_string_lossy()));
        std::fs::write(&driver, format!("{lex_nt}\n{parse_nt}\n{}", DRIVER.replace("@PATH@", &path))).unwrap();
        let got = Command::new(neant()).arg("run").arg(&driver).output().unwrap();
        if !got.status.success() {
            failures.push(format!("{name}: self-hosted driver failed:\n{}", String::from_utf8_lossy(&got.stderr)));
            continue;
        }
        if code == Some(1) {
            rejected += 1;
            if got.stdout != b"-1\n" {
                failures.push(format!("{name}: the bootstrap parser rejects this, the self-hosted one accepted it:\n{}",
                    String::from_utf8_lossy(&got.stdout)));
            }
            continue;
        }
        checked += 1;
        if want.stdout != got.stdout {
            failures.push(format!("{name}: node kinds differ\n--- bootstrap ---\n{}--- self-hosted ---\n{}",
                String::from_utf8_lossy(&want.stdout), String::from_utf8_lossy(&got.stdout)));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(checked >= 10, "only {checked} files were inside the parser's slice; the slice or the corpus changed");
    if !failures.is_empty() {
        panic!("{} failures over {checked} compared and {rejected} rejected-by-both files ({skipped} out of slice):\n\n{}",
            failures.len(), failures.join("\n"));
    }
}

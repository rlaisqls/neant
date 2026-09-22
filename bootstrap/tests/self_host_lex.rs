//! The self-hosted lexer's exit test (docs/self-hosting-design.md §4): for every `tests/golden/
//! *.nt` file, `compiler/lex.nt` run through the neant compiler itself must produce the same
//! sequence of token kinds as `bootstrap/src/lex.rs` does (`neant lexdump`, a debug command that
//! exists for this comparison — self-hosting-design.md, main.rs's `lex_kind_number`). Spans and
//! literal values are not compared here, only the kind at each position: which is exactly the
//! set of decisions a lexer makes that this stage was built to get right.
//!
//! The corpus is the goldens **and `compiler/*.nt`**. The compiler's own source is the one text
//! this lexer is certain to have to read, and it uses spellings no golden does: it is where
//! `b'\''` — a byte literal holding an escaped quote, which the first version of the scanner ran
//! straight past — was finally caught.

use std::path::{Path, PathBuf};
use std::process::Command;

fn neant() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_neant"))
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/golden")
}

#[test]
fn self_hosted_lex_matches_bootstrap() {
    let lex_nt = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../compiler/lex.nt")).unwrap();
    let mut files: Vec<PathBuf> = std::fs::read_dir(golden_dir())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "nt"))
        .collect();
    assert!(!files.is_empty(), "no golden programs found");
    let compiler = Path::new(env!("CARGO_MANIFEST_DIR")).join("../compiler");
    let stages: Vec<PathBuf> = std::fs::read_dir(&compiler)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "nt"))
        .collect();
    assert!(stages.len() >= 4, "the self-hosted stages are missing from {}", compiler.display());
    files.extend(stages);

    let dir = std::env::temp_dir().join(format!("neant-self-host-lex-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut failures = Vec::new();
    for f in &files {
        let driver = dir.join(format!("{}_driver.nt", f.file_stem().unwrap().to_string_lossy()));
        std::fs::write(&driver, format!(
            "{lex_nt}\n\nextern fn read_file(path: &[u8], buf: &mut [u8]) -> i64 uses io, unbounded;\n\nfn main() {{\n    let path = {};\n    let mut buf = [b'\\0'; 262144];\n    let n = read_file(&path, &mut buf);\n    let mut toks = [Token {{ kind: 0, start: 0, len: 0, ival: 0 }}; 262144];\n    let count = lex(&buf, n, &mut toks);\n    for i in 0..count {{\n        println(toks[i].kind);\n    }}\n}}\n",
            byte_string_literal(&f.to_string_lossy()),
        )).unwrap();

        let want = Command::new(neant()).arg("lexdump").arg(f).output().unwrap();
        let got = Command::new(neant()).arg("run").arg(&driver).output().unwrap();
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        if !want.status.success() {
            failures.push(format!("{name}: bootstrap lexer itself failed:\n{}", String::from_utf8_lossy(&want.stderr)));
            continue;
        }
        if !got.status.success() {
            failures.push(format!("{name}: self-hosted driver failed:\n{}", String::from_utf8_lossy(&got.stderr)));
            continue;
        }
        if want.stdout != got.stdout {
            failures.push(format!("{name}: token kinds differ\n--- bootstrap ---\n{}--- self-hosted ---\n{}",
                String::from_utf8_lossy(&want.stdout), String::from_utf8_lossy(&got.stdout)));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    if !failures.is_empty() {
        panic!("{} of {} files' self-hosted tokenisation differs:\n\n{}", failures.len(), files.len(), failures.join("\n"));
    }
}

/// A byte-string literal the real `bootstrap/src/lex.rs` reads (this test's driver file is
/// compiled by the actual `neant` binary, not by the self-hosted lexer under test) — a path from
/// `CARGO_MANIFEST_DIR`/`golden_dir` never holds a quote, backslash or newline, so it is valid
/// inside `b"..."` unescaped.
fn byte_string_literal(path: &str) -> String {
    assert!(!path.contains(['"', '\\', '\n']), "path needs escaping: {path}");
    format!("b\"{path}\"")
}

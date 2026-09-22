//! The self-hosted emitter's exit test (docs/self-hosting-emitter-design.md §5), and the first
//! end-to-end one: for every `tests/golden/*.nt` file the self-hosted chain can read and check,
//! `compiler/{lex,parse,check,emit}.nt` — run through the neant compiler — emits C, `cc` compiles
//! it, and **its output must equal `neant run`'s byte for byte**. Not a token sequence, not a tree:
//! the program's own behaviour, which two implementations cannot agree on by accident.

use std::path::{Path, PathBuf};
use std::process::Command;

fn neant() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_neant"))
}

fn repo(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(sub)
}

fn driver(input: &str, output: &str) -> String {
    format!(r#"
extern fn read_file(path: &[u8], buf: &mut [u8]) -> i64 uses io, unbounded;
extern fn write_file(path: &[u8], buf: &[u8], n: i64) -> i64 uses io, unbounded;

fn main() {{
    let inpath = b"{input}";
    let outpath = b"{output}";
    let mut buf = [b'\0'; 65536];
    let n = read_file(&inpath, &mut buf);
    let mut toks = [Token {{ kind: 0, start: 0, len: 0, ival: 0 }}; 65536];
    let n_toks = lex(&buf, n, &mut toks);
    let mut nodes = [Node {{ kind: 0, a: 0, b: 0, c: 0, d: 0, ival: 0, next: 0 }}; 65536];
    let mut st = [0, 0, 0, 0, 0];
    let first = parse_program(&buf, &toks, n_toks, &mut st, &mut nodes);
    if st[2] != 0 {{
        println(-2);
    }} else {{
        let mut types = [Ty {{ kind: 0, elem: 0, mutable: 0, size: 0 }}; 4096];
        let mut syms = [Sym {{ name: 0, ty: 0, mutable: 0 }}; 4096];
        let mut sigs = [Sig {{ name: -1, params: 0, n_params: 0, ret: 0, ext: 0 }}; 1024];
        let mut strs = [Str {{ name: 0, fields: 0, n_fields: 0 }}; 1024];
        let mut flds = [Fld {{ name: 0, ty: 0 }}; 4096];
        let mut ptys = [0; 4096];
        let mut ntys = [-1; 65536];
        let mut cst = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let bad = check_program(&buf, &toks, &nodes, &mut types, &mut syms, &mut sigs, &mut ptys, &mut strs, &mut flds, &mut ntys, &mut cst, first);
        if bad != 0 {{
            println(-1);
        }} else {{
            let mut out = [b'\0'; 262144];
            let mut est = [0, 0, 0];
            let len = emit_program(&mut out, &mut est, &buf, &toks, &nodes, &types, &strs, &flds, &ntys, &sigs, &ptys, first);
            if len < 0 {{
                // the emitter refused: something in this program is outside its slice
                println(len);
            }} else {{
                println(write_file(&outpath, &out, len));
            }}
        }}
    }}
}}
"#)
}

#[test]
fn self_hosted_emit_runs_the_same() {
    let stages = ["compiler/lex.nt", "compiler/parse.nt", "compiler/check.nt", "compiler/emit.nt"]
        .map(|p| std::fs::read_to_string(repo(p)).unwrap())
        .join("\n");
    let dir = std::env::temp_dir().join(format!("neant-self-host-emit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let mut files: Vec<PathBuf> = std::fs::read_dir(repo("tests/golden"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "nt"))
        .collect();
    files.sort();

    let (mut compared, mut failures) = (Vec::new(), Vec::new());
    for f in &files {
        // only what the self-hosted chain can read, and only programs that check: a rejected
        // program has no output to compare
        if Command::new(neant()).arg("parsedump").arg(f).output().unwrap().status.code() == Some(2) { continue; }
        if !Command::new(neant()).arg("check").arg(f).output().unwrap().status.success() { continue; }
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let stem = f.file_stem().unwrap().to_string_lossy().to_string();
        let path = f.to_string_lossy();
        assert!(!path.contains(['"', '\\', '\n']), "path needs escaping: {path}");

        let c_out = dir.join(format!("{stem}.c"));
        let nt = dir.join(format!("{stem}_driver.nt"));
        std::fs::write(&nt, format!("{stages}\n{}", driver(&path, &c_out.to_string_lossy()))).unwrap();

        let run = Command::new(neant()).arg("run").arg(&nt).output().unwrap();
        if !run.status.success() {
            failures.push(format!("{name}: the self-hosted compiler failed to run:\n{}", String::from_utf8_lossy(&run.stderr)));
            continue;
        }
        let wrote: i64 = String::from_utf8_lossy(&run.stdout).trim().parse().unwrap_or(-9);
        if wrote <= 0 {
            // -2 the parser, -1 the checker, -3 the emitter refusing, anything else write_file
            failures.push(format!("{name}: the self-hosted compiler emitted nothing (code {wrote}; \
                -2 parser, -1 checker, -3 emitter)"));
            continue;
        }

        let bin = dir.join(&stem);
        let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
        let built = Command::new(&cc).args(["-O2", "-std=gnu11", "-w", "-o"]).arg(&bin).arg(&c_out).output().unwrap();
        if !built.status.success() {
            failures.push(format!("{name}: cc rejected the emitted C ({}):\n{}", c_out.display(), String::from_utf8_lossy(&built.stderr)));
            continue;
        }

        compared.push(name.clone());
        let want = Command::new(neant()).arg("run").arg(f).output().unwrap();
        let got = Command::new(&bin).output().unwrap();
        if want.stdout != got.stdout {
            failures.push(format!("{name}: output differs\n--- neant run ---\n{}--- self-hosted ---\n{}",
                String::from_utf8_lossy(&want.stdout), String::from_utf8_lossy(&got.stdout)));
        }
    }
    if failures.is_empty() { let _ = std::fs::remove_dir_all(&dir); }
    assert!(compared.len() >= 5, "only {} programs made it through the self-hosted chain; the slice changed", compared.len());
    // a skip is otherwise silent — a program that stops parsing is simply not compared — so the
    // goldens that carry a whole feature of the slice are named here rather than left to a count
    for want in ["structval.nt", "arrayview.nt", "words.nt"] {
        assert!(compared.iter().any(|n| n == want),
            "{want} did not reach the comparison; what it covers fell out of the slice. compared: {compared:?}");
    }
    if !failures.is_empty() {
        panic!("{} of {} programs differ (artifacts kept in {}):\n\n{}",
            failures.len(), compared.len(), dir.display(), failures.join("\n"));
    }
}


/// **The fixpoint.** Everything above compares the self-hosted compiler against the Rust one.
/// This compares it against *itself*, which is the only test that says the thing is a compiler
/// rather than a program that agrees with one.
///
/// 1. `stage1` is the four stages plus the driver that makes them a program — neant source, run
///    by the Rust compiler's interpreter.
/// 2. `stage1` reads its own source and writes `stage2.c`; `cc` compiles that with `rt.c` into a
///    native binary.
/// 3. `stage2` reads the same source and writes C again.
///
/// **That C must be byte-identical to `stage2.c`.** A compiler that has reached its fixpoint emits
/// itself; one that has not — because it is compiled differently from how it compiles, or because
/// some construct survives one pass and not the next — does not. Nothing about the Rust compiler
/// is in the comparison: it built stage2 and then stepped out.
///
/// It also grew out of a weaker test that stopped at `cc -c`, because `main` lived in a driver and
/// `extern fn` was outside the slice. It is not outside any more.
#[test]
fn the_self_hosted_compiler_reaches_its_fixpoint() {
    let names = ["compiler/lex.nt", "compiler/parse.nt", "compiler/check.nt", "compiler/emit.nt"];
    let stages = names.map(|p| std::fs::read_to_string(repo(p)).unwrap()).join("\n");
    let dir = std::env::temp_dir().join(format!("neant-fixpoint-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // the arenas are sized for the compiler's own source, not for a golden
    let big = |d: String| d.replace("[b'\\0'; 65536]", "[b'\\0'; 262144]")
        .replace("; 65536]", "; 262144]").replace("; 4096]", "; 16384]");

    // stage1: reads `input.nt`, writes `emitted.c`. Its own source is what it will be given.
    let input = dir.join("input.nt");
    let emitted = dir.join("emitted.c");
    let stage1 = dir.join("stage1.nt");
    let stage1_src = format!("{stages}\n{}",
        big(driver(&input.to_string_lossy(), &emitted.to_string_lossy())));
    std::fs::write(&stage1, &stage1_src).unwrap();

    // stage1 compiles stage1, under the Rust compiler
    let stage2_c = dir.join("stage2.c");
    let bootstrap_driver = dir.join("bootstrap.nt");
    std::fs::write(&bootstrap_driver, format!("{stages}\n{}",
        big(driver(&stage1.to_string_lossy(), &stage2_c.to_string_lossy())))).unwrap();
    let run = Command::new(neant()).arg("run").arg(&bootstrap_driver).output().unwrap();
    assert!(run.status.success(), "stage1 failed on its own source:\n{}",
        String::from_utf8_lossy(&run.stderr));
    let wrote: i64 = String::from_utf8_lossy(&run.stdout).trim().parse().unwrap_or(-9);
    assert!(wrote > 0, "stage1 emitted nothing for its own source (code {wrote}; \
        -2 parser, -1 checker, -3 emitter)");

    // stage2: the same compiler, native
    let stage2 = dir.join("stage2");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let built = Command::new(&cc).args(["-O1", "-std=gnu11", "-w", "-o"]).arg(&stage2)
        .arg(&stage2_c).arg(repo("bootstrap/rt.c")).output().unwrap();
    assert!(built.status.success(), "cc rejected stage2.c ({}):\n{}",
        stage2_c.display(), String::from_utf8_lossy(&built.stderr));

    // stage2 compiles stage1 — the same input stage1 was just given
    std::fs::copy(&stage1, &input).unwrap();
    let out = Command::new(&stage2).output().unwrap();
    assert!(out.status.success(), "stage2 failed on stage1's source:\n{}",
        String::from_utf8_lossy(&out.stderr));
    let wrote2: i64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(-9);
    assert!(wrote2 > 0, "stage2 emitted nothing for stage1's source (code {wrote2})");

    let a = std::fs::read(&stage2_c).unwrap();
    let b = std::fs::read(&emitted).unwrap();
    assert_eq!(a.len(), b.len(),
        "stage2 emitted {} bytes where stage1 emitted {} — the compiler does not reach its \
         fixpoint (artifacts in {})", b.len(), a.len(), dir.display());
    assert!(a == b, "stage2's output differs from stage1's although the lengths match \
        (artifacts in {})", dir.display());
    let _ = std::fs::remove_dir_all(&dir);
}

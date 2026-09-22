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
        let mut syms = [Sym {{ name: 0, ty: 0, mutable: 0, root: 0 }}; 4096];
        let mut sigs = [Sig {{ name: -1, params: 0, n_params: 0, ret: 0, ext: 0 }}; 1024];
        let mut strs = [Str {{ name: 0, fields: 0, n_fields: 0 }}; 1024];
        let mut flds = [Fld {{ name: 0, ty: 0 }}; 4096];
        let mut ptys = [0; 4096];
        let mut ntys = [-1; 65536];
        let mut cst = [0; 1024];
        cst[16] = st[1];
    let bad = check_program(&buf, &toks, &mut nodes, &mut types, &mut syms, &mut sigs, &mut ptys, &mut strs, &mut flds, &mut ntys, &mut cst, first);
        if bad != 0 {{
            println(-1);
        }} else {{
            let mut slay = [0; 1024];
            choose_layouts(&buf, &toks, &nodes, &types, &strs, &flds, &ntys, &sigs, &ptys,
                           &mut slay, first, cst[7]);
            let mut out = [b'\0'; 262144];
            let mut est = [0, 0, 0, 0];
            let len = emit_program(&mut out, &mut est, &buf, &toks, &nodes, &types, &strs, &flds, &slay, &ntys, &sigs, &ptys, first);
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
    let stages = ["compiler/lex.nt", "compiler/parse.nt", "compiler/check.nt", "compiler/emit.nt",
                  "compiler/poly.nt", "compiler/cost.nt"]
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



/// **The fixpoint, and seed two.** Everything above compares the self-hosted compiler against the
/// Rust one. This compares it against *itself*, which is the only test that says the thing is a
/// compiler rather than a program that agrees with one.
///
/// 1. `stage1` is `compiler/*.nt` concatenated — the four stages and `main.nt`, the committed
///    driver — as neant source, run by the Rust compiler's interpreter.
/// 2. `stage1` reads its own source from stdin and writes C; `cc` compiles that with `rt.c` into
///    a native binary.
/// 3. `stage2` reads the same source and writes C again.
///
/// **That C must be byte-identical to what stage1 wrote,** and to the committed `bootstrap/neant.c`. A
/// compiler that has reached its fixpoint emits itself; one that has not — because it is compiled
/// differently from how it compiles, or because some construct survives one pass and not the next
/// — does not. Nothing about the Rust compiler is in the first comparison: it built stage2 and
/// then stepped out.
///
/// The second comparison is what keeps **seed two** honest. `bootstrap/neant.c` is checked in so that a C
/// compiler and nothing else can rebuild the chain, and a checked-in artifact that nothing checks
/// is a file that silently stops matching its source. Regenerate it with `compiler/build.sh`.
#[test]
fn the_self_hosted_compiler_reaches_its_fixpoint() {
    // the cost pass is part of the compiler now: the layout choice runs before emission, so what
    // the compiler emits and what the cost reporter prints describe the same program
    let names = ["compiler/lex.nt", "compiler/parse.nt", "compiler/check.nt", "compiler/emit.nt",
                 "compiler/poly.nt", "compiler/cost.nt", "compiler/main.nt"];
    // concatenated in dependency order, which is the whole build system: there is no module
    // system, so these are fragments of one program (compiler/build.sh does the same)
    let stage1_src: String = names.iter().map(|p| std::fs::read_to_string(repo(p)).unwrap()).collect();
    let dir = std::env::temp_dir().join(format!("neant-fixpoint-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let stage1 = dir.join("all.nt");
    std::fs::write(&stage1, &stage1_src).unwrap();

    // stage1 compiles stage1, under the Rust compiler
    let run = Command::new(neant()).arg("run").arg(&stage1)
        .stdin(std::fs::File::open(&stage1).unwrap()).output().unwrap();
    assert!(run.status.success(), "stage1 failed on its own source (exit {:?}: 2 parser, \
        3 checker, 4 emitter, 5 size):\n{}", run.status.code(), String::from_utf8_lossy(&run.stderr));
    let stage2_c = dir.join("stage2.c");
    std::fs::write(&stage2_c, &run.stdout).unwrap();
    assert!(run.stdout.len() > 1000, "stage1 emitted {} bytes for its own source", run.stdout.len());

    // stage2: the same compiler, native
    let stage2 = dir.join("stage2");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let built = Command::new(&cc).args(["-O1", "-std=gnu11", "-w", "-o"]).arg(&stage2)
        .arg(&stage2_c).arg(repo("bootstrap/rt.c")).output().unwrap();
    assert!(built.status.success(), "cc rejected the C stage1 wrote for its own source ({}):\n{}",
        stage2_c.display(), String::from_utf8_lossy(&built.stderr));

    // stage2 compiles stage1 — the same input stage1 was just given
    let again = Command::new(&stage2).stdin(std::fs::File::open(&stage1).unwrap()).output().unwrap();
    assert!(again.status.success(), "stage2 failed on stage1's source (exit {:?})", again.status.code());
    assert_eq!(again.stdout.len(), run.stdout.len(),
        "stage2 emitted {} bytes where stage1 emitted {} — the compiler does not reach its \
         fixpoint (artifacts in {})", again.stdout.len(), run.stdout.len(), dir.display());
    assert!(again.stdout == run.stdout,
        "stage2's output differs from stage1's although the lengths match (artifacts in {})",
        dir.display());

    // seed two, as committed
    let seed = std::fs::read(repo("bootstrap/neant.c")).expect("bootstrap/neant.c is missing; run compiler/build.sh");
    assert!(seed == run.stdout,
        "bootstrap/neant.c is {} bytes and the compiler now emits {} — seed two is stale; \
         regenerate it with compiler/build.sh", seed.len(), run.stdout.len());
    let _ = std::fs::remove_dir_all(&dir);
}

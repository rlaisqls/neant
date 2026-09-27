//! A program's input beyond stdin: its command-line arguments and a named file
//! (docs/decisions.md, "Program input"). The four builtins are ordinary `extern` declarations —
//! a declared cost, `uses io`, a C symbol in `bootstrap/rt.c` — written once here in the
//! language's own syntax and added to a program that calls one without defining that name
//! itself. Nothing else in the compiler knows them by name: the checker sees an extern, the cost
//! pass a declared callee, the emitter a C function.
//!
//! What is new is not here but in the cost pass: an extern that returns `[T]` names its result's
//! length `result.len()` in its declaration, and a caller gets a size atom of its own for it,
//! `data.len()`, free the way a parameter's length is.

use crate::ast;
use crate::lex::{Tok, Token};

/// The declarations. A read of `n` bytes is priced as the model prices a sequential write of `n`
/// bytes (`[b'\0'; n]` is work `n`, moves `n`): the bytes land in a fresh array once. A path is
/// read once, the same way. The constants are `neant measure`'s (docs/cost-model.md § Program
/// input): a call into `rt.c` that the compiler cannot inline is about ten instructions, and opening
/// a file about two thousand in user space — the kernel's side of it is not in the counter.
const PRELUDE: &str = r#"
#[cost(work_at_most = "10", moves_at_most = "0")]
extern fn arg_count() -> i64 uses io;
#[cost(work_at_most = "result.len()", moves_at_most = "result.len()")]
extern fn arg(k: i64) -> [u8] uses io;
#[cost(work_at_most = "result.len() + path.len() + 2500", moves_at_most = "result.len() + path.len()")]
extern fn read_file(path: &[u8]) -> [u8] uses io;
#[cost(work_at_most = "path.len() + 2500", moves_at_most = "path.len()")]
extern fn file_size(path: &[u8]) -> i64 uses io;
#[cost(work_at_most = "s.len() + 200", moves_at_most = "s.len()")]
extern fn print_bytes(s: &[u8], n: i64) uses io;
"#;

/// The builtins. `print_bytes` is output, not input, but it is the same kind of thing — an
/// extern with a declared cost in `rt.c` (docs/decisions.md §13).
pub const NAMES: [&str; 5] = ["arg_count", "arg", "read_file", "file_size", "print_bytes"];

/// The builtins `toks` calls — a name directly followed by `(`, so a comment or a variable of the
/// same name does not count.
pub fn called(toks: &[Token]) -> Vec<&'static str> {
    NAMES.iter().copied().filter(|n| toks.windows(2).any(|w| matches!(&w[0].tok, Tok::Ident(s) if s == n) && w[1].tok == Tok::LParen)).collect()
}

/// Append the declaration of every builtin in `used` that `prog` does not define itself: a
/// program's own `extern fn read_file(path: &[u8], buf: &mut [u8]) -> i64` (the self-hosting
/// tests have one, over `rt.c`'s older pair) keeps its meaning. A builtin is marked by line 0,
/// which no function in a program has; the emitter reads that to prefix its C symbol.
pub fn add(prog: &mut ast::Program, used: &[&str]) {
    if used.is_empty() { return; }
    let toks = crate::lex::lex(PRELUDE).expect("the input prelude lexes");
    let prelude = crate::parse::parse(toks).expect("the input prelude parses");
    for mut f in prelude.funcs {
        if !used.contains(&f.name.as_str()) || prog.funcs.iter().any(|g| g.name == f.name) { continue; }
        f.line = 0;
        prog.funcs.push(f);
    }
}

pub fn is_builtin(f: &crate::ir::Func) -> bool {
    f.line == 0 && f.body.is_none() && NAMES.contains(&f.name.as_str())
}

/// What `neant measure --fn <builtin>` runs at size `n` (docs/cost-model.md § Program input): a
/// `main` that calls the builtin `repeat` times on a real input of `n` bytes — an argument of `n`
/// bytes given to the binary, or a file of `n` bytes written into `dir` — or, without the call, the
/// same loop, to be subtracted. The ordinary driver cannot: it fills a path with a pattern, runs
/// the binary with no arguments, and has no value for `result.len()`, which is not a parameter.
pub struct Probe {
    pub module: crate::ir::Module,
    /// the binary's arguments
    pub args: Vec<String>,
    /// the sizes the declaration is evaluated at, as `--eval` takes them
    pub ev: String,
}

pub fn probe(name: &str, n: i64, repeat: i64, with_call: bool, dir: &std::path::Path) -> Result<Probe, String> {
    let file = dir.join(format!("probe{n}.bin"));
    let path = file.to_string_lossy().to_string();
    if path.contains(['"', '\\', '\n']) { return Err(format!("the probe's path needs escaping: {path}")); }
    let (call, args, ev) = match name {
        "arg_count" => ("acc += arg_count();".to_string(), vec![], "n=0".to_string()),
        "arg" => ("let a = arg(0);\n        acc += a.len();".to_string(), vec!["x".repeat(n as usize)], format!("k=0,result.len()={n}")),
        "read_file" | "file_size" => {
            std::fs::write(&file, vec![b'x'; n as usize]).map_err(|e| format!("{path}: {e}"))?;
            let call = if name == "read_file" { "let d = read_file(&p);\n        acc += d.len();" } else { "acc += file_size(&p);" };
            (call.to_string(), vec![], format!("path.len()={}{}", path.len(), if name == "read_file" { format!(",result.len()={n}") } else { String::new() }))
        }
        // `n` bytes written, whole, to the binary's stdout (a pipe the driver drains)
        "print_bytes" => ("print_bytes(&s, s.len());\n        acc += 1;".to_string(), vec![], format!("s.len()={n},n={n}")),
        other => return Err(format!("`{other}` is not an input builtin")),
    };
    let body = if with_call { call } else { "acc += 1;".to_string() };
    let src = format!("fn main() {{\n    let s = [b'x'; {n}];\n    let p = b\"{path}\";\n    let mut acc = 0;\n    for r in 0..{repeat} {{\n        {body}\n    }}\n    println(acc + p.len() + s.len());\n}}\n");
    let toks = crate::lex::lex(&src).map_err(|e| e.to_string())?;
    let used = called(&toks);
    let mut prog = crate::parse::parse(toks).map_err(|e| e.to_string())?;
    add(&mut prog, &used);
    let module = crate::types::check(&prog).map_err(|e| e.to_string())?;
    Ok(Probe { module, args, ev })
}

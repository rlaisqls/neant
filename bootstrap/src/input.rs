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
/// read once, the same way.
const PRELUDE: &str = r#"
#[cost(work_at_most = "1", moves_at_most = "0")]
extern fn arg_count() -> i64 uses io;
#[cost(work_at_most = "result.len()", moves_at_most = "result.len()")]
extern fn arg(k: i64) -> [u8] uses io;
#[cost(work_at_most = "result.len() + path.len()", moves_at_most = "result.len() + path.len()")]
extern fn read_file(path: &[u8]) -> [u8] uses io;
#[cost(work_at_most = "path.len()", moves_at_most = "path.len()")]
extern fn file_size(path: &[u8]) -> i64 uses io;
"#;

pub const NAMES: [&str; 4] = ["arg_count", "arg", "read_file", "file_size"];

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

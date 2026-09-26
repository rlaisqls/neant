//! A program that spans files (docs/modules-design.md). `use "path";` at the top level names a
//! file, resolved from the directory of the file that names it; each file is loaded once, a cycle
//! is rejected with its chain, and every item lands in one flat namespace. The files share one
//! line space, numbered through the concatenation in load order, so the checker, the cost pass
//! and the emitter see what `cat` would give them; `Sources` maps a line back to its file wherever
//! one leaves the compiler. A root with no `use` is parsed exactly as before and nothing is
//! rewritten.

use crate::ast::Program;
use crate::diag::Error;
use crate::lex::{self, Tok, Token};
use crate::parse;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Where each file's lines sit in the program's one line space: file `k` holds the global lines
/// `base + 1 ..= base + lines`.
pub struct Sources {
    root: String,
    files: Vec<(String, u32, u32)>,
    multi: bool,
}

impl Sources {
    pub fn multi(&self) -> bool { self.multi }

    /// A global line as `(path, the file's own line)`.
    fn place(&self, line: u32) -> Option<(&str, u32)> {
        self.files.iter().find(|(_, base, n)| line > *base && line <= base + n).map(|(p, base, _)| (p.as_str(), line - base))
    }

    fn at(&self, line: u32, col: u32) -> String {
        match self.place(line) { Some((p, l)) => format!("{p}:{l}:{col}"), None => format!("{}:{line}:{col}", self.root) }
    }

    /// A diagnostic, `path:line:col: message`, as the driver prints it.
    pub fn error(&self, e: &Error) -> String {
        if !self.multi() { return format!("{}:{e}", self.root); }
        format!("{}: {}", self.at(e.line, e.col), self.relabel(&e.msg))
    }

    /// Text with `line N` in it — the report, a note, a `#[cost]` violation — with every `line N`
    /// made `path:N`. The identity on a single-file program.
    pub fn relabel(&self, s: &str) -> String {
        if !self.multi() { return s.to_string(); }
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(i) = rest.find("line ") {
            let (head, tail) = rest.split_at(i);
            out.push_str(head);
            let digits: String = tail[5..].chars().take_while(|c| c.is_ascii_digit()).collect();
            let word_start = !out.chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_');
            let placed = if digits.is_empty() || !word_start { None } else { digits.parse().ok().and_then(|n| self.place(n)) };
            match placed {
                Some((p, l)) => { out.push_str(&format!("{p}:{l}")); rest = &tail[5 + digits.len()..]; }
                None => { out.push_str("line "); rest = &tail[5..]; }
            }
        }
        out.push_str(rest);
        out
    }

    /// `#[cost]` violations are `line N: …`; on a multi-file program the file leads instead.
    pub fn violation(&self, v: &str) -> String {
        if !self.multi() { return format!("{}:{v}", self.root); }
        self.relabel(v)
    }

    /// The emitted C's bounds check prints `(line %d)` with a global line; give it the table from
    /// line ranges to paths so it prints `(path:N)`. Only on a multi-file program, and only the
    /// driver's copy of the C: the emitter is unchanged.
    pub fn patch_c(&self, c: &str) -> String {
        const CALL: &str = r#"fprintf(stderr, "index %lld out of bounds for length %lld (line %d)\n", (long long)i, (long long)n, line);"#;
        const DEF: &str = "static inline int64_t nt_idx(";
        if !self.multi() || !c.contains(CALL) { return c.to_string(); }
        let rows: Vec<String> = self.files.iter()
            .map(|(p, base, _)| format!("{{{base}, \"{}\"}}", p.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect();
        let table = format!(
            "static const char *nt_src(int line, int *local) {{\n    static const struct {{ int base; const char *path; }} t[] = {{{}}};\n    int k = {};\n    while (k > 0 && line <= t[k].base) k--;\n    *local = line - t[k].base;\n    return t[k].path;\n}}\n",
            rows.join(", "), rows.len() - 1);
        let call = r#"{ int nt_l; const char *nt_f = nt_src(line, &nt_l); fprintf(stderr, "index %lld out of bounds for length %lld (%s:%d)\n", (long long)i, (long long)n, nt_f, nt_l); }"#;
        c.replacen(DEF, &format!("{table}{DEF}"), 1).replacen(CALL, call, 1)
    }
}

/// The root file and everything it `use`s, as one program. The error is the whole diagnostic.
pub fn load(root: &Path, src: &str) -> Result<(Program, Sources), String> {
    let root_name = root.display().to_string();
    let mut sources = Sources { root: root_name.clone(), files: Vec::new(), multi: false };
    let toks = lex::lex(src).map_err(|e| format!("{root_name}:{e}"))?;
    let (uses, toks) = split_uses(toks).map_err(|e| format!("{root_name}:{e}"))?;
    if uses.is_empty() {
        let used = crate::input::called(&toks);
        let mut prog = parse::parse(toks).map_err(|e| format!("{root_name}:{e}"))?;
        crate::input::add(&mut prog, &used);
        sources.files.push((root_name, 0, lines(src)));
        return Ok((prog, sources));
    }
    sources.multi = true;
    let mut l = Loader { sources, parsed: Vec::new(), prog: Program { funcs: Vec::new(), structs: Vec::new() }, next: 0, done: HashSet::new(), stack: Vec::new() };
    let canon = std::fs::canonicalize(root).map_err(|e| format!("{root_name}: {e}"))?;
    l.visit(root_name, canon, src, toks, uses)?;
    // every file's struct names, so a struct literal may name one defined in any file
    let known: Vec<String> = l.parsed.iter()
        .flat_map(|ts| ts.windows(2).filter_map(|w| match (&w[0].tok, &w[1].tok) { (Tok::Struct, Tok::Ident(n)) => Some(n.clone()), _ => None }))
        .collect();
    // the input builtins any file calls (docs/decisions.md §9), declared once for the program
    let used: Vec<&str> = crate::input::NAMES.iter().copied()
        .filter(|n| l.parsed.iter().any(|ts| crate::input::called(ts).contains(n))).collect();
    for toks in std::mem::take(&mut l.parsed) {
        let p = parse::parse_knowing(toks, &known).map_err(|e| l.sources.error(&e))?;
        l.prog.funcs.extend(p.funcs);
        l.prog.structs.extend(p.structs);
    }
    l.duplicates()?;
    crate::input::add(&mut l.prog, &used);
    Ok((l.prog, l.sources))
}

struct Use { path: String, line: u32, col: u32 }

struct Loader {
    sources: Sources,
    /// each file's tokens, in load order and in the program's line space
    parsed: Vec<Vec<Token>>,
    prog: Program,
    next: u32,
    done: HashSet<PathBuf>,
    stack: Vec<(PathBuf, String)>,
}

impl Loader {
    fn visit(&mut self, name: String, canon: PathBuf, src: &str, mut toks: Vec<Token>, uses: Vec<Use>) -> Result<(), String> {
        self.stack.push((canon.clone(), name.clone()));
        let dir = Path::new(&name).parent().map(Path::to_path_buf).unwrap_or_default();
        for u in uses {
            let target = if Path::new(&u.path).is_absolute() { PathBuf::from(&u.path) } else { dir.join(&u.path) };
            let tname = target.display().to_string();
            let here = format!("{name}:{}:{}", u.line, u.col);
            let tcanon = std::fs::canonicalize(&target).map_err(|e| format!("{here}: cannot read `{tname}`: {e}"))?;
            if let Some(i) = self.stack.iter().position(|(c, _)| *c == tcanon) {
                let mut chain: Vec<&str> = self.stack[i..].iter().map(|(_, n)| n.as_str()).collect();
                chain.push(&self.stack[i].1);
                return Err(format!("{here}: `use \"{}\"` closes a cycle: {}", u.path, chain.join(" → ")));
            }
            if self.done.contains(&tcanon) { continue; }
            let tsrc = std::fs::read_to_string(&target).map_err(|e| format!("{here}: cannot read `{tname}`: {e}"))?;
            let ttoks = lex::lex(&tsrc).map_err(|e| format!("{tname}:{e}"))?;
            let (tuses, ttoks) = split_uses(ttoks).map_err(|e| format!("{tname}:{e}"))?;
            self.visit(tname, tcanon, &tsrc, ttoks, tuses)?;
        }
        self.stack.pop();
        self.done.insert(canon);
        // this file's lines follow those of every file loaded before it
        let base = self.next;
        let n = lines(src);
        self.next += n;
        self.sources.files.push((name, base, n));
        for t in &mut toks { t.line += base; }
        self.parsed.push(toks);
        Ok(())
    }

    /// One flat namespace: a function or a struct defined twice anywhere in the program is
    /// rejected at the second place, naming both.
    fn duplicates(&self) -> Result<(), String> {
        let mut funcs: HashMap<&str, (u32, u32)> = HashMap::new();
        for f in &self.prog.funcs {
            if let Some(&(l, c)) = funcs.get(f.name.as_str()) {
                return Err(self.twice("function", &f.name, (l, c), (f.line, f.col)));
            }
            funcs.insert(&f.name, (f.line, f.col));
        }
        let mut structs: HashMap<&str, (u32, u32)> = HashMap::new();
        for s in &self.prog.structs {
            if let Some(&(l, c)) = structs.get(s.name.as_str()) {
                return Err(self.twice("struct", &s.name, (l, c), (s.line, s.col)));
            }
            structs.insert(&s.name, (s.line, s.col));
        }
        Ok(())
    }

    fn twice(&self, what: &str, name: &str, first: (u32, u32), second: (u32, u32)) -> String {
        let (a, b) = (self.sources.at(first.0, first.1), self.sources.at(second.0, second.1));
        format!("{b}: {what} `{name}` is defined twice: at {a} and {b}")
    }
}

fn lines(src: &str) -> u32 { src.matches('\n').count() as u32 + 1 }

/// Takes `use "path";` out of a file's tokens at brace depth 0. It names a file, not a node of
/// the program, so the parser never sees it; an `use` that is not followed by a string is left
/// for the parser to reject as it did before.
fn split_uses(toks: Vec<Token>) -> crate::diag::Result<(Vec<Use>, Vec<Token>)> {
    let mut uses = Vec::new();
    let mut out = Vec::with_capacity(toks.len());
    let mut depth = 0i32;
    let mut i = 0;
    while i < toks.len() {
        match &toks[i].tok {
            Tok::LParen | Tok::LBracket | Tok::LBrace => depth += 1,
            Tok::RParen | Tok::RBracket | Tok::RBrace => depth -= 1,
            Tok::Ident(s) if depth == 0 && s == "use" => {
                if let Some(Tok::Str(p)) = toks.get(i + 1).map(|t| &t.tok) {
                    let (line, col) = (toks[i].line, toks[i].col);
                    if !matches!(toks.get(i + 2).map(|t| &t.tok), Some(Tok::Semi)) {
                        return crate::diag::err(line, col, format!("`use \"{p}\"` ends with `;`"));
                    }
                    uses.push(Use { path: p.clone(), line, col });
                    i += 3;
                    continue;
                }
            }
            _ => {}
        }
        out.push(toks[i].clone());
        i += 1;
    }
    Ok((uses, out))
}

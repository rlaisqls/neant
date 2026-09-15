//! No precedence table, no backtracking: a statement is a flat list of primaries resolved
//! right-to-left, so parsing is linear in the token count.
use crate::lex::Tok;
use crate::value::*;
use std::collections::VecDeque;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub enum Ast {
    Const(Value), Name(String), Verb(char), Advb(char, Box<Ast>),
    Mo(Box<Ast>, Box<Ast>), Dy(Box<Ast>, Box<Ast>, Box<Ast>), App(Box<Ast>, Box<Ast>), Call(Box<Ast>, Vec<Ast>),
    List(Vec<Ast>), Assign(String, Box<Ast>), Lambda(Vec<String>, Vec<Ast>),
    If(Box<Ast>, Vec<Ast>), While(Box<Ast>, Vec<Ast>), Cond(Vec<Ast>),
    Noun(Box<Ast>),                            // parenthesized verb: `(+/)` is a value, not an infix operator
    IndexAssign(String, Vec<Ast>, Box<Ast>),   // x[i;j]:v
    Return(Box<Ast>),                          // :x  early return from a lambda
    GAssign(String, Box<Ast>),                 // x::v  assign the global even inside a lambda
    Do(Box<Ast>, Vec<Ast>),                    // do[n; ...]
    Break,                                     // leave the innermost while/do
    At(u32, Box<Ast>),                         // statement tagged with its source line; compiles to a line-table entry
}

pub struct Parser { t: Vec<Tok>, lines: Vec<u32>, i: usize, stmt_lines: Vec<u32> }

const SELECT_KW: [&str; 3] = ["by", "from", "where"];

impl Parser {
    pub fn with_lines(t: Vec<Tok>, lines: Vec<u32>) -> Parser { Parser { t, lines, i: 0, stmt_lines: vec![] } }
    /// Line each top-level statement starts on (for runtime error messages).
    pub fn stmt_lines(&self) -> &[u32] { &self.stmt_lines }
    fn line_here(&self) -> u32 { self.lines.get(self.i).copied().unwrap_or(1) }
    fn peek(&self, k: usize) -> Option<&Tok> { self.t.get(self.i + k) }
    fn next(&mut self) -> Option<Tok> { let t = self.t.get(self.i).cloned(); self.i += 1; t }
    fn at(&self, k: usize, p: char) -> bool { self.peek(k) == Some(&Tok::Punct(p)) }
    fn kw(&self, k: usize, w: &str) -> bool { matches!(self.peek(k), Some(Tok::Name(n)) if n == w) }

    /// Parse everything; errors carry the line of the token being looked at.
    pub fn program(&mut self) -> R<Vec<Ast>> {
        self.stmts(None, true).map_err(|e| match self.lines.get(self.i.min(self.lines.len().saturating_sub(1))) {
            Some(l) if !self.lines.is_empty() => NError(format!("{} at line {l}", e.0)),
            _ => e,
        })
    }

    /// `mark`: this is a statement list (lambda/if/while/do body, cond arms, top level), so each statement
    /// is wrapped in At(line) for the line table. A parenthesized list is a value context and is not marked.
    fn stmts(&mut self, close: Option<char>, mark: bool) -> R<Vec<Ast>> {
        let mut out = Vec::new();
        loop {
            match self.peek(0) {
                None => return if close.is_none() { Ok(out) } else { err(format!("parse: missing {}", close.unwrap())) },
                Some(Tok::Punct(c)) if Some(*c) == close => { self.i += 1; return Ok(out); }
                Some(Tok::Punct(';')) => self.i += 1,
                // a closer that is not the one we are waiting for: expr() would consume nothing and we would spin
                Some(Tok::Punct(c @ (')' | ']' | '}'))) => return err(format!("parse: unexpected {c}")),
                _ => {
                    let line = self.line_here();
                    if close.is_none() { self.stmt_lines.push(line); }
                    if let Some(e) = self.expr()? { out.push(if mark { Ast::At(line, Box::new(e)) } else { e }) }
                }
            }
        }
    }

    fn rhs(&mut self) -> R<Ast> { self.expr()?.ok_or_else(|| NError("parse: empty assignment".into())) }

    fn expr(&mut self) -> R<Option<Ast>> {
        if self.at(0, ':') { self.i += 1; return Ok(Some(Ast::Return(Box::new(self.rhs()?)))); }
        if let Some(Tok::Name(n)) = self.peek(0) {
            let n = n.clone();
            if self.at(1, ':') && self.at(2, ':') {                               // x:: e
                self.i += 3;
                return Ok(Some(Ast::GAssign(n, Box::new(self.rhs()?))));
            }
            if self.at(1, ':') {                                                  // x: e
                self.i += 2;
                return Ok(Some(Ast::Assign(n, Box::new(self.rhs()?))));
            }
            if let (Some(Tok::Verb(c)), true) = (self.peek(1), self.at(2, ':')) {  // x+: e  ==  x: x+e
                let c = *c; self.i += 3;
                let e = Ast::Dy(Box::new(Ast::Verb(c)), Box::new(Ast::Name(n.clone())), Box::new(self.rhs()?));
                return Ok(Some(Ast::Assign(n, Box::new(e))));
            }
            if self.at(1, '[') {                                                  // x[i;j]: e   x[i]+: e
                let save = self.i; self.i += 2;
                let idx = self.args()?;
                if !idx.is_empty() {
                    if self.at(0, ':') {
                        self.i += 1;
                        return Ok(Some(Ast::IndexAssign(n, idx, Box::new(self.rhs()?))));
                    }
                    if let (Some(Tok::Verb(c)), true) = (self.peek(0), self.at(1, ':')) {
                        let c = *c; self.i += 2;
                        let cur = Ast::Call(Box::new(Ast::Name(n.clone())), idx.clone());
                        let e = Ast::Dy(Box::new(Ast::Verb(c)), Box::new(cur), Box::new(self.rhs()?));
                        return Ok(Some(Ast::IndexAssign(n, idx, Box::new(e))));
                    }
                }
                self.i = save;
            }
        }
        let mut items = VecDeque::new();
        loop {
            match self.peek(0) {
                None | Some(Tok::Punct(';' | ')' | ']' | '}')) => break,
                _ => items.push_back(self.primary()?),
            }
        }
        resolve(items)
    }

    fn primary(&mut self) -> R<Ast> {
        let tok = self.next().ok_or_else(|| NError("parse: unexpected end".into()))?;
        let mut node = match tok {
            Tok::Num(v) | Tok::Syms(v) => Ast::Const(v),
            Tok::Str(s) => Ast::Const(if s.len() == 1 { Value::Char(s[0]) } else { chars(s) }),
            Tok::Name(n) if (n == "if" || n == "while" || n == "do") && self.at(0, '[') => {
                self.i += 1;
                let mut args = self.stmts(Some(']'), true)?;
                if args.is_empty() { return err(format!("parse: {n} needs a condition")); }
                let c = Box::new(args.remove(0));
                return Ok(match n.as_str() { "if" => Ast::If(c, args), "while" => Ast::While(c, args), _ => Ast::Do(c, args) });
            }
            Tok::Name(n) if n == "select" => return self.select_form(),
            Tok::Name(n) if n == "break" => return Ok(Ast::Break),
            Tok::Name(n) => Ast::Name(n),
            Tok::Verb('$') if self.at(0, '[') => { self.i += 1; return Ok(Ast::Cond(self.stmts(Some(']'), true)?)); }
            Tok::Verb(c) => Ast::Verb(c),
            Tok::Adv(c) => return err(format!("parse: dangling adverb {}", advf(c))),
            Tok::Punct('(') => {
                let mut body = self.stmts(Some(')'), false)?;
                match body.len() {
                    1 => { let e = body.pop().unwrap(); if is_fnlike(&e) { Ast::Noun(Box::new(e)) } else { e } }
                    _ => Ast::List(body),
                }
            }
            Tok::Punct('{') => {
                let mut params = None;
                if self.at(0, '[') {
                    self.i += 1;
                    let mut ps = Vec::new();
                    loop {
                        match self.next() {
                            Some(Tok::Punct(']')) => break,
                            Some(Tok::Name(p)) => ps.push(p),
                            Some(Tok::Punct(';')) => {}
                            _ => return err("parse: bad parameter list"),
                        }
                    }
                    params = Some(ps);
                }
                let body = self.stmts(Some('}'), true)?;
                let params = params.unwrap_or_else(|| {
                    let mut hi = 0;
                    for s in &body {
                        walk(s, &mut |a| if let Ast::Name(n) = a {
                            hi = hi.max(match n.as_str() { "x" => 1, "y" => 2, "z" => 3, _ => 0 });
                        });
                    }
                    ["x", "y", "z"][..hi.max(1)].iter().map(|s| s.to_string()).collect()
                });
                Ast::Lambda(params, body)
            }
            Tok::Punct(c) => return err(format!("parse: unexpected {c:?}")),
        };
        loop {
            if let Some(Tok::Adv(c)) = self.peek(0) {
                let c = *c; self.i += 1; node = Ast::Advb(c, Box::new(node));
            } else if self.at(0, '[') {
                self.i += 1; let args = self.args()?; node = Ast::Call(Box::new(node), args);
            } else { break; }
        }
        Ok(node)
    }

    /// Call arguments after `[`: an empty slot is Null, which makes the call a projection (`f[;2]`).
    fn args(&mut self) -> R<Vec<Ast>> {
        let mut out = Vec::new();
        loop {
            let e = self.expr()?;
            match self.next() {
                Some(Tok::Punct(';')) => out.push(e.unwrap_or(Ast::Const(Value::Null))),
                Some(Tok::Punct(']')) => {
                    if let Some(e) = e { out.push(e) } else if !out.is_empty() { out.push(Ast::Const(Value::Null)) }
                    return Ok(out);
                }
                _ => return err("parse: missing ]"),
            }
        }
    }

    // ---- select cols by keys from t where conds   ->   qsel[t; where-fn; by-dict; col-dict]  (boot/table.nt)
    fn select_form(&mut self) -> R<Ast> {
        let cols = if self.kw(0, "by") || self.kw(0, "from") { vec![] } else { self.sel_list()? };
        let by = if self.kw(0, "by") { self.i += 1; self.sel_list()? } else { vec![] };
        if !self.kw(0, "from") { return err("parse: select needs from"); }
        self.i += 1;
        let from = self.sel_expr()?.ok_or_else(|| NError("parse: select needs a table".into()))?;
        let wh = if self.kw(0, "where") { self.i += 1; self.sel_list()? } else { vec![] };
        Ok(desugar_select(cols, by, from, wh.into_iter().map(|(_, e)| e).collect()))
    }
    /// comma-separated `[name:] expr` items, up to by/from/where or a terminator
    fn sel_list(&mut self) -> R<Vec<(Option<String>, Ast)>> {
        let mut out = Vec::new();
        loop {
            let name = match (self.peek(0), self.at(1, ':')) {
                (Some(Tok::Name(n)), true) => { let n = n.clone(); self.i += 2; Some(n) }
                _ => None,
            };
            let e = self.sel_expr()?.ok_or_else(|| NError("parse: empty select item".into()))?;
            out.push((name, e));
            if self.peek(0) == Some(&Tok::Verb(',')) { self.i += 1; continue; }
            return Ok(out);
        }
    }
    /// an expression that also stops at a top-level `,` and at the select keywords
    fn sel_expr(&mut self) -> R<Option<Ast>> {
        let mut items = VecDeque::new();
        loop {
            match self.peek(0) {
                None | Some(Tok::Punct(';' | ')' | ']' | '}')) | Some(Tok::Verb(',')) => break,
                Some(Tok::Name(n)) if SELECT_KW.contains(&n.as_str()) => break,
                _ => items.push_back(self.primary()?),
            }
        }
        resolve(items)
    }
}

/// Column expressions become unary lambdas over the table `.t`; every name resolves to a column if the
/// table has one, else to the global: n -> $[`n in key .t; .t`n; n]
fn desugar_select(cols: Vec<(Option<String>, Ast)>, by: Vec<(Option<String>, Ast)>, from: Ast, wh: Vec<Ast>) -> Ast {
    let absent = || Ast::Const(ints(vec![]));
    let lam = |e: Ast| Ast::Lambda(vec![".t".into()], vec![colref(e)]);
    let dict = |items: Vec<(Option<String>, Ast)>| -> Ast {
        if items.is_empty() { return absent(); }
        let mut n = 0;
        let names: Vec<Rc<str>> = items.iter().map(|(nm, e)| match (nm, e) {
            (Some(nm), _) => Rc::from(nm.as_str()),
            (None, e) if auto_name(e).is_some() => Rc::from(auto_name(e).unwrap()),
            _ => { let s = if n == 0 { "x".to_string() } else { format!("x{n}") }; n += 1; Rc::from(s.as_str()) }
        }).collect();
        Ast::Dy(Box::new(Ast::Verb('!')), Box::new(Ast::Const(syms(names))), Box::new(Ast::List(items.into_iter().map(|(_, e)| lam(e)).collect())))
    };
    let wl = if wh.is_empty() { absent() } else {
        let mut it = wh.into_iter();
        let mut acc = it.next().unwrap();
        for c in it { acc = Ast::Dy(Box::new(Ast::Verb('&')), Box::new(acc), Box::new(c)); }
        lam(acc)
    };
    Ast::Call(Box::new(Ast::Name("qsel".into())), vec![from, wl, dict(by), dict(cols)])
}
/// q names an unnamed column after the last name in it: `sum b` -> b
fn auto_name(e: &Ast) -> Option<&str> {
    match e { Ast::Name(v) => Some(v), Ast::App(_, x) | Ast::Mo(_, x) => auto_name(x), _ => None }
}
fn colref(a: Ast) -> Ast {
    let b = |x: Ast| Box::new(colref(x));
    let many = |xs: Vec<Ast>| xs.into_iter().map(colref).collect();
    match a {
        Ast::Name(n) if n != ".t" => {
            let sym = || Ast::Const(Value::Symbol(Rc::from(n.as_str())));
            let t = || Box::new(Ast::Name(".t".into()));
            Ast::Cond(vec![
                Ast::Dy(Box::new(Ast::Name("in".into())), Box::new(sym()), Box::new(Ast::App(Box::new(Ast::Name("key".into())), t()))),
                Ast::App(t(), Box::new(sym())),
                Ast::Name(n),
            ])
        }
        Ast::Advb(c, x) => Ast::Advb(c, b(*x)),
        Ast::Mo(f, x) => Ast::Mo(b(*f), b(*x)),
        Ast::Dy(f, x, y) => Ast::Dy(b(*f), b(*x), b(*y)),
        Ast::App(f, x) => Ast::App(b(*f), b(*x)),
        Ast::Call(f, xs) => Ast::Call(b(*f), many(xs)),
        Ast::List(xs) => Ast::List(many(xs)),
        Ast::Cond(xs) => Ast::Cond(many(xs)),
        Ast::Noun(x) => Ast::Noun(b(*x)),
        Ast::Lambda(p, body) => Ast::Lambda(p, many(body)),
        Ast::If(c, body) => Ast::If(b(*c), many(body)),
        Ast::While(c, body) => Ast::While(b(*c), many(body)),
        Ast::Do(c, body) => Ast::Do(b(*c), many(body)),
        Ast::Assign(n, e) => Ast::Assign(n, b(*e)),
        Ast::GAssign(n, e) => Ast::GAssign(n, b(*e)),
        Ast::IndexAssign(n, i, e) => Ast::IndexAssign(n, many(i), b(*e)),
        Ast::Return(e) => Ast::Return(b(*e)),
        Ast::At(l, e) => Ast::At(l, b(*e)),
        other => other,
    }
}

fn is_fnlike(a: &Ast) -> bool {
    match a { Ast::Verb(_) | Ast::Advb(..) => true, Ast::Name(n) => crate::prims::INFIX.contains(&n.as_str()), _ => false }
}

/// [noun verb rest] -> Dy, [verb rest] -> Mo, [noun rest] -> App. Right operand is the rest of the line.
fn resolve(mut items: VecDeque<Ast>) -> R<Option<Ast>> {
    if items.len() <= 1 { return Ok(items.pop_front()); }
    let need = |r: Option<Ast>| r.ok_or_else(|| NError("parse: incomplete expression".into()));
    let h = items.pop_front().unwrap();
    if is_fnlike(&h) { return Ok(Some(Ast::Mo(Box::new(h), Box::new(need(resolve(items)?)?)))); }
    if is_fnlike(&items[0]) {
        let f = items.pop_front().unwrap();
        return Ok(Some(Ast::Dy(Box::new(f), Box::new(h), Box::new(need(resolve(items)?)?))));
    }
    Ok(Some(Ast::App(Box::new(h), Box::new(need(resolve(items)?)?))))
}

/// Visit every node, not descending into nested lambdas (they have their own scope).
pub fn walk(a: &Ast, f: &mut impl FnMut(&Ast)) {
    f(a);
    match a {
        Ast::Advb(_, x) | Ast::Assign(_, x) | Ast::GAssign(_, x) | Ast::Noun(x) | Ast::Return(x) | Ast::At(_, x) => walk(x, f),
        Ast::Mo(g, x) | Ast::App(g, x) => { walk(g, f); walk(x, f) }
        Ast::Dy(g, x, y) => { walk(g, f); walk(x, f); walk(y, f) }
        Ast::IndexAssign(_, i, e) => { for x in i { walk(x, f) } walk(e, f) }
        Ast::Call(g, args) => { walk(g, f); for x in args { walk(x, f) } }
        Ast::List(xs) | Ast::Cond(xs) => for x in xs { walk(x, f) },
        Ast::If(c, b) | Ast::While(c, b) | Ast::Do(c, b) => { walk(c, f); for x in b { walk(x, f) } }
        Ast::Lambda(..) | Ast::Const(_) | Ast::Name(_) | Ast::Verb(_) | Ast::Break => {}
    }
}

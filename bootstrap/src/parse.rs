//! Recursive descent with precedence climbing. Rust-shaped: blocks are expressions, the last
//! expression of a block without a `;` is its value, `if` is an expression.

use crate::ast::*;
use crate::diag::{err, Result};
use crate::lex::{Tok, Token};

pub fn parse(toks: Vec<Token>) -> Result<Program> {
    // struct names first, so `S { … }` can be told from a block wherever a struct literal may stand
    let mut struct_names = Vec::new();
    for w in toks.windows(2) {
        if let (Tok::Struct, Tok::Ident(n)) = (&w[0].tok, &w[1].tok) { struct_names.push(n.clone()); }
    }
    let mut p = Parser { toks, pos: 0, struct_names, no_struct_lit: 0 };
    let mut funcs = Vec::new();
    let mut structs = Vec::new();
    while p.peek() != &Tok::Eof {
        let attrs = p.attributes()?;
        if p.at(&Tok::Struct) { structs.push(p.struct_def(attrs)?); } else { funcs.push(p.func(attrs)?); }
    }
    Ok(Program { funcs, structs })
}

/// `#[name(key = "value", …)]` or `#[name(word)]`
struct Attr {
    name: String,
    pairs: Vec<(String, String, u32, u32)>,
    words: Vec<String>,
    line: u32,
    col: u32,
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    struct_names: Vec<String>,
    /// inside an `if`/`while` condition or a `for` range: `S {` is not a struct literal there, the
    /// `{` opens the body
    no_struct_lit: usize,
}

enum Item {
    Stmt(Stmt),
    Tail(Expr),
}

impl Parser {
    fn peek(&self) -> &Tok { &self.toks[self.pos].tok }
    fn here(&self) -> (u32, u32) { let t = &self.toks[self.pos]; (t.line, t.col) }
    fn next(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if self.pos < self.toks.len() - 1 { self.pos += 1; }
        t
    }
    fn at(&self, t: &Tok) -> bool { self.peek() == t }
    fn eat(&mut self, t: &Tok) -> bool {
        if self.at(t) { self.next(); true } else { false }
    }
    fn expect(&mut self, t: Tok, what: &str) -> Result<Token> {
        if self.at(&t) { Ok(self.next()) } else {
            let (l, c) = self.here();
            err(l, c, format!("expected {what}, found {}", describe(self.peek())))
        }
    }
    fn ident(&mut self, what: &str) -> Result<(String, u32, u32)> {
        let (l, c) = self.here();
        match self.next().tok {
            Tok::Ident(s) => Ok((s, l, c)),
            t => err(l, c, format!("expected {what}, found {}", describe(&t))),
        }
    }

    fn attributes(&mut self) -> Result<Vec<Attr>> {
        let mut out = Vec::new();
        while self.at(&Tok::Hash) {
            let (line, col) = self.here();
            self.next();
            self.expect(Tok::LBracket, "`[`")?;
            let (name, _, _) = self.ident("attribute name")?;
            let mut pairs = Vec::new();
            let mut words = Vec::new();
            self.expect(Tok::LParen, "`(`")?;
            while !self.at(&Tok::RParen) {
                let (key, kl, kc) = self.ident("an attribute key")?;
                if self.eat(&Tok::Eq) {
                    let (vl, vc) = self.here();
                    let Tok::Str(val) = self.next().tok else { return err(vl, vc, "an attribute value is a string: `\"n log n\"`") };
                    pairs.push((key, val, kl, kc));
                } else {
                    words.push(key);
                }
                if !self.eat(&Tok::Comma) { break; }
            }
            self.expect(Tok::RParen, "`)`")?;
            self.expect(Tok::RBracket, "`]`")?;
            out.push(Attr { name, pairs, words, line, col });
        }
        Ok(out)
    }

    fn struct_def(&mut self, attrs: Vec<Attr>) -> Result<StructDef> {
        let mut layout = None;
        for a in attrs {
            if a.name != "layout" { return err(a.line, a.col, format!("`#[{}]` does not apply to a struct; only `#[layout(aos)]` / `#[layout(soa)]`", a.name)); }
            match a.words.as_slice() {
                [w] if w == "aos" || w == "soa" => layout = Some(w.clone()),
                _ => return err(a.line, a.col, "`#[layout(...)]` takes `aos` or `soa`"),
            }
        }
        let (line, col) = self.here();
        self.expect(Tok::Struct, "`struct`")?;
        let (name, _, _) = self.ident("struct name")?;
        self.expect(Tok::LBrace, "`{`")?;
        let mut fields = Vec::new();
        while !self.at(&Tok::RBrace) {
            let (f, fl, fc) = self.ident("field name")?;
            self.expect(Tok::Colon, "`:`")?;
            let ty = self.type_expr()?;
            fields.push((f, ty, fl, fc));
            if !self.eat(&Tok::Comma) { break; }
        }
        self.expect(Tok::RBrace, "`}`")?;
        Ok(StructDef { name, fields, layout, line, col })
    }

    fn func(&mut self, attrs: Vec<Attr>) -> Result<Func> {
        let mut asserts = Vec::new();
        for a in attrs {
            if a.name != "cost" { return err(a.line, a.col, format!("unknown attribute `{}` on a function; only `#[cost(...)]` exists", a.name)); }
            if !a.words.is_empty() { return err(a.line, a.col, "`#[cost(...)]` takes `key = \"expr\"` pairs"); }
            asserts.extend(a.pairs);
        }
        let (line, col) = self.here();
        let is_extern = self.eat(&Tok::Extern);
        self.expect(Tok::Fn, "`fn`")?;
        let (name, _, _) = self.ident("function name")?;
        self.expect(Tok::LParen, "`(`")?;
        let mut params = Vec::new();
        while !self.at(&Tok::RParen) {
            let (pname, pl, pc) = self.ident("parameter name")?;
            self.expect(Tok::Colon, "`:`")?;
            let ty = self.type_expr()?;
            params.push(Param { name: pname, ty, line: pl, col: pc });
            if !self.eat(&Tok::Comma) { break; }
        }
        self.expect(Tok::RParen, "`)`")?;
        let ret = if self.eat(&Tok::Arrow) { self.type_expr()? } else { TypeExpr::Unit };
        let mut uses = Vec::new();
        if self.eat(&Tok::Uses) {
            loop {
                let (e, _, _) = self.ident("an effect (`io`, `unbounded`)")?;
                uses.push(e);
                if !self.eat(&Tok::Comma) { break; }
            }
        }
        let body = if is_extern {
            self.expect(Tok::Semi, "`;` after an extern declaration")?;
            None
        } else {
            Some(self.block()?)
        };
        Ok(Func { name, params, ret, body, uses, asserts, line, col })
    }

    fn type_expr(&mut self) -> Result<TypeExpr> {
        let (l, c) = self.here();
        match self.peek().clone() {
            Tok::Ident(s) => { self.next(); Ok(TypeExpr::Named(s)) }
            Tok::LParen => {
                self.next();
                self.expect(Tok::RParen, "`)`")?;
                Ok(TypeExpr::Unit)
            }
            Tok::LBracket => {
                self.next();
                let elem = self.type_expr()?;
                if self.eat(&Tok::RBracket) { return Ok(TypeExpr::Owned(Box::new(elem))); }
                self.expect(Tok::Semi, "`;` in array type")?;
                let n = self.expr()?;
                self.expect(Tok::RBracket, "`]`")?;
                Ok(TypeExpr::Array(Box::new(elem), Box::new(n)))
            }
            Tok::Amp => {
                self.next();
                let mutable = self.eat(&Tok::Mut);
                self.expect(Tok::LBracket, "`[` after `&`")?;
                let elem = self.type_expr()?;
                self.expect(Tok::RBracket, "`]`")?;
                Ok(TypeExpr::Slice(Box::new(elem), mutable))
            }
            t => err(l, c, format!("expected a type, found {}", describe(&t))),
        }
    }

    fn block(&mut self) -> Result<Block> {
        let (line, col) = self.here();
        self.expect(Tok::LBrace, "`{`")?;
        let mut stmts = Vec::new();
        let mut tail = None;
        while !self.at(&Tok::RBrace) {
            if self.at(&Tok::Eof) {
                return err(line, col, "unclosed `{`");
            }
            match self.item()? {
                Item::Stmt(s) => stmts.push(s),
                Item::Tail(e) => { tail = Some(Box::new(e)); break; }
            }
        }
        self.expect(Tok::RBrace, "`}`")?;
        Ok(Block { stmts, tail, line, col })
    }

    fn item(&mut self) -> Result<Item> {
        let (line, col) = self.here();
        match self.peek() {
            Tok::Let => {
                self.next();
                let mutable = self.eat(&Tok::Mut);
                let (name, _, _) = self.ident("variable name")?;
                let ty = if self.eat(&Tok::Colon) { Some(self.type_expr()?) } else { None };
                self.expect(Tok::Eq, "`=`")?;
                let init = self.expr()?;
                self.expect(Tok::Semi, "`;`")?;
                Ok(Item::Stmt(Stmt::Let { name, mutable, ty, init, line, col }))
            }
            Tok::For => {
                self.next();
                let (var, _, _) = self.ident("loop variable")?;
                self.expect(Tok::In, "`in`")?;
                self.no_struct_lit += 1;
                let start = self.expr()?;
                self.expect(Tok::DotDot, "`..`")?;
                let end = self.expr()?;
                self.no_struct_lit -= 1;
                let body = self.block()?;
                Ok(Item::Stmt(Stmt::For { var, start, end, body, line, col }))
            }
            Tok::Return => {
                self.next();
                let e = if self.at(&Tok::Semi) { None } else { Some(self.expr()?) };
                self.expect(Tok::Semi, "`;`")?;
                Ok(Item::Stmt(Stmt::Return(e, line, col)))
            }
            Tok::While => {
                self.next();
                self.no_struct_lit += 1;
                let cond = self.expr()?;
                let decreasing = if self.eat(&Tok::Decreasing) { Some(self.expr()?) } else { None };
                self.no_struct_lit -= 1;
                let body = self.block()?;
                Ok(Item::Stmt(Stmt::While { cond, decreasing, body, line, col }))
            }
            Tok::Break => {
                self.next();
                self.expect(Tok::Semi, "`;`")?;
                Ok(Item::Stmt(Stmt::Break(line, col)))
            }
            _ => {
                // Rust's rule, and for Rust's reason: in *statement* position an expression that
                // begins with a block-like form — `if` or `{` — ends at that block, and a binary
                // operator on the next line starts a new statement rather than continuing it.
                // Without this, an else-less `if` followed by a line starting with `-` parses as
                // one subtraction; that bit three times while self-hosting (compiler/lex.nt,
                // compiler/check.nt, and a debug driver) and was worked around with a stray `;`
                // each time. `let x = if c { 1 } else { 2 } + 1;` is unaffected: it is not in
                // statement position. To use a block-like expression as an operand here, bracket
                // it — `(if c { 1 } else { 2 }) + 1` — which is what Rust asks for too.
                let block_like = matches!(self.peek(), Tok::If | Tok::LBrace);
                let e = if block_like { self.primary()? } else { self.expr()? };
                let compound = match self.peek() {
                    Tok::Eq => Some(None),
                    Tok::PlusEq => Some(Some(BinOp::Add)),
                    Tok::MinusEq => Some(Some(BinOp::Sub)),
                    Tok::StarEq => Some(Some(BinOp::Mul)),
                    Tok::SlashEq => Some(Some(BinOp::Div)),
                    _ => None,
                };
                if let Some(op) = compound {
                    self.next();
                    let value = self.expr()?;
                    self.expect(Tok::Semi, "`;`")?;
                    return Ok(Item::Stmt(Stmt::Assign { target: e, op, value, line, col }));
                }
                if self.eat(&Tok::Semi) { return Ok(Item::Stmt(Stmt::Expr(e))); }
                if self.at(&Tok::RBrace) { return Ok(Item::Tail(e)); }
                if matches!(e.kind, ExprKind::If(..) | ExprKind::Block(_)) {
                    return Ok(Item::Stmt(Stmt::Expr(e)));
                }
                let (l, c) = self.here();
                err(l, c, format!("expected `;`, found {}", describe(self.peek())))
            }
        }
    }

    // ---- expressions ----

    fn expr(&mut self) -> Result<Expr> { self.or() }

    fn binary_level(&mut self, ops: &[(Tok, BinOp)], next: fn(&mut Self) -> Result<Expr>) -> Result<Expr> {
        let mut lhs = next(self)?;
        loop {
            let Some((_, op)) = ops.iter().find(|(t, _)| self.at(t)) else { break };
            let op = *op;
            let (l, c) = self.here();
            self.next();
            let rhs = next(self)?;
            lhs = Expr { kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)), line: l, col: c };
        }
        Ok(lhs)
    }

    fn or(&mut self) -> Result<Expr> {
        self.binary_level(&[(Tok::PipePipe, BinOp::Or)], Self::and)
    }
    fn and(&mut self) -> Result<Expr> {
        self.binary_level(&[(Tok::AmpAmp, BinOp::And)], Self::cmp)
    }
    fn cmp(&mut self) -> Result<Expr> {
        // comparisons do not chain
        let lhs = self.add()?;
        let op = match self.peek() {
            Tok::EqEq => BinOp::Eq, Tok::Ne => BinOp::Ne, Tok::Lt => BinOp::Lt,
            Tok::Le => BinOp::Le, Tok::Gt => BinOp::Gt, Tok::Ge => BinOp::Ge,
            _ => return Ok(lhs),
        };
        let (l, c) = self.here();
        self.next();
        let rhs = self.add()?;
        Ok(Expr { kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)), line: l, col: c })
    }
    fn add(&mut self) -> Result<Expr> {
        self.binary_level(&[(Tok::Plus, BinOp::Add), (Tok::Minus, BinOp::Sub)], Self::mul)
    }
    fn mul(&mut self) -> Result<Expr> {
        self.binary_level(&[(Tok::Star, BinOp::Mul), (Tok::Slash, BinOp::Div), (Tok::Percent, BinOp::Rem)], Self::cast)
    }
    fn cast(&mut self) -> Result<Expr> {
        let mut e = self.unary()?;
        while self.at(&Tok::As) {
            let (l, c) = self.here();
            self.next();
            let ty = self.type_expr()?;
            e = Expr { kind: ExprKind::Cast(Box::new(e), ty), line: l, col: c };
        }
        Ok(e)
    }
    fn unary(&mut self) -> Result<Expr> {
        let (l, c) = self.here();
        match self.peek() {
            Tok::Minus => {
                self.next();
                let e = self.unary()?;
                Ok(Expr { kind: ExprKind::Unary(UnOp::Neg, Box::new(e)), line: l, col: c })
            }
            Tok::Bang => {
                self.next();
                let e = self.unary()?;
                Ok(Expr { kind: ExprKind::Unary(UnOp::Not, Box::new(e)), line: l, col: c })
            }
            Tok::Amp => {
                self.next();
                let mutable = self.eat(&Tok::Mut);
                let e = self.unary()?;
                Ok(Expr { kind: ExprKind::Ref(Box::new(e), mutable), line: l, col: c })
            }
            _ => self.postfix(),
        }
    }
    fn postfix(&mut self) -> Result<Expr> {
        let mut e = self.primary()?;
        loop {
            let (l, c) = self.here();
            match self.peek() {
                Tok::LBracket => {
                    self.next();
                    let idx = self.expr()?;
                    self.expect(Tok::RBracket, "`]`")?;
                    e = Expr { kind: ExprKind::Index(Box::new(e), Box::new(idx)), line: l, col: c };
                }
                Tok::Dot => {
                    self.next();
                    let (name, _, _) = self.ident("a field or method name")?;
                    if self.eat(&Tok::LParen) {
                        let args = self.args()?;
                        e = Expr { kind: ExprKind::MethodCall(Box::new(e), name, args), line: l, col: c };
                    } else {
                        e = Expr { kind: ExprKind::Field(Box::new(e), name), line: l, col: c };
                    }
                }
                _ => return Ok(e),
            }
        }
    }
    fn args(&mut self) -> Result<Vec<Expr>> {
        let mut args = Vec::new();
        while !self.at(&Tok::RParen) {
            args.push(self.expr()?);
            if !self.eat(&Tok::Comma) { break; }
        }
        self.expect(Tok::RParen, "`)`")?;
        Ok(args)
    }
    fn primary(&mut self) -> Result<Expr> {
        let (l, c) = self.here();
        let mk = |kind| Ok(Expr { kind, line: l, col: c });
        match self.peek().clone() {
            Tok::Int(v) => { self.next(); mk(ExprKind::Int(v)) }
            Tok::Float(v) => { self.next(); mk(ExprKind::Float(v)) }
            Tok::Byte(v) => { self.next(); mk(ExprKind::Byte(v)) }
            Tok::Bytes(v) => { self.next(); mk(ExprKind::Bytes(v)) }
            Tok::True => { self.next(); mk(ExprKind::Bool(true)) }
            Tok::False => { self.next(); mk(ExprKind::Bool(false)) }
            Tok::Ident(name) => {
                self.next();
                if self.at(&Tok::LParen) {
                    self.next();
                    let args = self.args()?;
                    mk(ExprKind::Call(name, args))
                } else if self.at(&Tok::LBrace) && self.no_struct_lit == 0 && self.struct_names.contains(&name) {
                    self.next();
                    let mut fields = Vec::new();
                    while !self.at(&Tok::RBrace) {
                        let (f, _, _) = self.ident("field name")?;
                        self.expect(Tok::Colon, "`:`")?;
                        // the body of a block is parsed normally: `S { f: if c { 1 } else { 2 } }` is fine
                        let saved = self.no_struct_lit;
                        self.no_struct_lit = 0;
                        let v = self.expr()?;
                        self.no_struct_lit = saved;
                        fields.push((f, v));
                        if !self.eat(&Tok::Comma) { break; }
                    }
                    self.expect(Tok::RBrace, "`}`")?;
                    mk(ExprKind::StructLit(name, fields))
                } else {
                    mk(ExprKind::Var(name))
                }
            }
            Tok::LParen => {
                self.next();
                let e = self.expr()?;
                self.expect(Tok::RParen, "`)`")?;
                Ok(e)
            }
            Tok::LBracket => {
                self.next();
                if self.at(&Tok::RBracket) {
                    return err(l, c, "empty array literal has no element type");
                }
                let first = self.expr()?;
                if self.at(&Tok::For) {
                    self.next();
                    let (var, _, _) = self.ident("comprehension variable")?;
                    self.expect(Tok::In, "`in`")?;
                    let source = self.expr()?;
                    let cond = if self.eat(&Tok::If) { Some(Box::new(self.expr()?)) } else { None };
                    self.expect(Tok::RBracket, "`]`")?;
                    return mk(ExprKind::Comprehension { elem: Box::new(first), var, source: Box::new(source), cond });
                }
                if self.eat(&Tok::Semi) {
                    let n = self.expr()?;
                    self.expect(Tok::RBracket, "`]`")?;
                    return mk(ExprKind::ArrayRepeat(Box::new(first), Box::new(n)));
                }
                let mut elems = vec![first];
                while self.eat(&Tok::Comma) {
                    if self.at(&Tok::RBracket) { break; }
                    elems.push(self.expr()?);
                }
                self.expect(Tok::RBracket, "`]`")?;
                mk(ExprKind::ArrayLit(elems))
            }
            Tok::If => {
                self.next();
                self.no_struct_lit += 1;
                let cond = self.expr()?;
                self.no_struct_lit -= 1;
                let then = self.block()?;
                let els = if self.eat(&Tok::Else) {
                    if self.at(&Tok::If) {
                        // `else if` is an else block whose value is the inner if
                        let (il, ic) = self.here();
                        let inner = self.primary()?;
                        Some(Block { stmts: vec![], tail: Some(Box::new(inner)), line: il, col: ic })
                    } else {
                        Some(self.block()?)
                    }
                } else { None };
                mk(ExprKind::If(Box::new(cond), then, els))
            }
            Tok::LBrace => {
                let b = self.block()?;
                mk(ExprKind::Block(b))
            }
            Tok::Pipe => {
                self.next();
                let mut params = Vec::new();
                while !self.at(&Tok::Pipe) {
                    let (p, _, _) = self.ident("closure parameter")?;
                    params.push(p);
                    if !self.eat(&Tok::Comma) { break; }
                }
                self.expect(Tok::Pipe, "`|`")?;
                let body = self.expr()?;
                mk(ExprKind::Lambda(params, Box::new(body)))
            }
            t => err(l, c, format!("expected an expression, found {}", describe(&t))),
        }
    }
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::Ident(s) => format!("`{s}`"),
        Tok::Int(v) => format!("`{v}`"),
        Tok::Float(v) => format!("`{v}`"),
        Tok::Byte(v) => format!("`b'{}'`", *v as char),
        Tok::Bytes(_) => "a byte string".to_string(),
        Tok::Str(_) => "a string".to_string(),
        Tok::Eof => "end of file".to_string(),
        other => {
            let s = match other {
                Tok::Fn => "fn", Tok::Let => "let", Tok::Mut => "mut", Tok::If => "if", Tok::Else => "else",
                Tok::For => "for", Tok::In => "in", Tok::Return => "return", Tok::True => "true",
                Tok::False => "false", Tok::As => "as", Tok::LParen => "(", Tok::RParen => ")",
                Tok::LBracket => "[", Tok::RBracket => "]", Tok::LBrace => "{", Tok::RBrace => "}",
                Tok::Comma => ",", Tok::Semi => ";", Tok::Colon => ":", Tok::Arrow => "->", Tok::Dot => ".",
                Tok::DotDot => "..", Tok::Eq => "=", Tok::EqEq => "==", Tok::Ne => "!=", Tok::Lt => "<",
                Tok::Le => "<=", Tok::Gt => ">", Tok::Ge => ">=", Tok::Plus => "+", Tok::Minus => "-",
                Tok::Star => "*", Tok::Slash => "/", Tok::Percent => "%", Tok::PlusEq => "+=",
                Tok::MinusEq => "-=", Tok::StarEq => "*=", Tok::SlashEq => "/=", Tok::Amp => "&",
                Tok::AmpAmp => "&&", Tok::Pipe => "|", Tok::PipePipe => "||", Tok::Bang => "!", Tok::Hash => "#",
                Tok::While => "while", Tok::Break => "break", Tok::Decreasing => "decreasing",
                Tok::Extern => "extern", Tok::Uses => "uses", Tok::Struct => "struct",
                _ => "?",
            };
            format!("`{s}`")
        }
    }
}

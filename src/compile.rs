//! AST -> stack bytecode. Verbs and verb+adverb become constants, so `+/x` is one Monad op
//! whose callee the VM can fuse; lambdas get slot-indexed locals. A lambda that reads a local of an
//! enclosing lambda captures it by value at creation (MkClosure); captured slots follow its own locals.
use crate::parse::{walk, Ast};
use crate::prims::prim;
use crate::value::*;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone)]
struct Scope { slots: HashMap<String, u32>, nbase: u32, captures: Vec<String>, parent: Option<Box<Scope>> }
impl Scope {
    fn visible(&self, n: &str) -> bool {
        self.slots.contains_key(n) || self.captures.iter().any(|c| c == n) || self.parent.as_ref().is_some_and(|p| p.visible(n))
    }
}

/// loops: one frame per enclosing while/do — (is do-loop, Jmp positions of its breaks to patch)
pub struct Compiler { pub ops: Vec<Op>, pub consts: Vec<Value>, pub lines: Vec<u32>, cur: u32, scope: Option<Scope>, loops: Vec<(bool, Vec<usize>)> }

impl Compiler {
    fn k(&mut self, v: Value) -> u32 { self.consts.push(v); (self.consts.len() - 1) as u32 }
    fn emit(&mut self, op: Op) -> usize { self.ops.push(op); self.lines.push(self.cur); self.ops.len() - 1 }
    fn patch(&mut self, pos: usize) {
        let t = self.ops.len() as u32;
        self.ops[pos] = match self.ops[pos] { Op::Jmp(_) => Op::Jmp(t), Op::Jmpf(_) => Op::Jmpf(t), Op::Loop(_) => Op::Loop(t), o => o };
    }
    fn name(&mut self, n: &str) -> u32 { self.k(Value::Symbol(Rc::from(n))) }
    /// Local slot for a name — own local, already-captured, or newly captured from an enclosing lambda. None: global.
    fn slot(&mut self, n: &str) -> Option<u32> {
        let sc = self.scope.as_mut()?;
        if let Some(&s) = sc.slots.get(n) { return Some(s); }
        if let Some(i) = sc.captures.iter().position(|c| c == n) { return Some(sc.nbase + i as u32); }
        if sc.parent.as_ref().is_some_and(|p| p.visible(n)) {
            sc.captures.push(n.to_string());
            return Some(sc.nbase + sc.captures.len() as u32 - 1);
        }
        None
    }

    pub fn emit_ast(&mut self, n: &Ast) -> R<()> {
        match n {
            Ast::Const(v) => { let k = self.k(v.clone()); self.emit(Op::Push(k)); }
            Ast::Name(s) => match self.slot(s) {
                Some(slot) => { self.emit(Op::LoadL(slot)); }
                None => { let k = self.name(s); self.emit(Op::LoadG(k)); }
            },
            Ast::Assign(s, e) => {
                if let Some(rhs) = append_rhs(s, e) {   // x,: y  ->  Take x so join can extend in place
                    self.emit_ast(rhs)?;
                    let kj = self.k(Value::Prim(prim(',')));
                    match self.slot(s) {
                        Some(slot) => { self.emit(Op::TakeL(slot)); self.emit(Op::Dyad(kj)); self.emit(Op::StoreL(slot)); }
                        None => { let k = self.name(s); self.emit(Op::TakeG(k)); self.emit(Op::Dyad(kj)); self.emit(Op::StoreG(k)); }
                    }
                    return Ok(());
                }
                self.emit_ast(e)?;
                match self.slot(s) {
                    Some(slot) => { self.emit(Op::StoreL(slot)); }
                    None => { let k = self.name(s); self.emit(Op::StoreG(k)); }
                }
            }
            Ast::GAssign(s, e) => {
                if let Some(rhs) = append_rhs(s, e) {
                    self.emit_ast(rhs)?;
                    let kj = self.k(Value::Prim(prim(',')));
                    let k = self.name(s); self.emit(Op::TakeG(k)); self.emit(Op::Dyad(kj)); self.emit(Op::StoreG(k));
                    return Ok(());
                }
                self.emit_ast(e)?; let k = self.name(s); self.emit(Op::StoreG(k));
            }
            Ast::IndexAssign(s, idx, e) => {
                self.emit_ast(e)?;
                for i in idx.iter().rev() { self.emit_ast(i)?; }
                let n = idx.len() as u32;
                match self.slot(s) {
                    Some(slot) => { self.emit(Op::TakeL(slot)); self.emit(Op::Amend(n)); self.emit(Op::StoreL(slot)); }
                    None => { let k = self.name(s); self.emit(Op::TakeG(k)); self.emit(Op::Amend(n)); self.emit(Op::StoreG(k)); }
                }
            }
            Ast::Verb(_) | Ast::Advb(..) => match fnconst(n) {
                Some(f) => { let k = self.k(f); self.emit(Op::Push(k)); }
                None => { let Ast::Advb(c, inner) = n else { unreachable!() }; self.emit_ast(inner)?; self.emit(Op::MkAdv(*c)); }
            },
            Ast::Mo(f, x) => { self.emit_ast(x)?; self.callee(f, 1)?; }
            Ast::Dy(f, x, y) => { self.emit_ast(y)?; self.emit_ast(x)?; self.callee(f, 2)?; }
            Ast::App(f, x) => { self.emit_ast(x)?; self.emit_ast(f)?; self.emit(Op::Call(1)); }
            Ast::Call(f, args) => {
                for a in args.iter().rev() { self.emit_ast(a)?; }
                self.emit_ast(f)?; self.emit(Op::Call(args.len() as u32));
            }
            Ast::List(xs) => {
                for a in xs.iter().rev() { self.emit_ast(a)?; }
                self.emit(Op::List(xs.len() as u32));
            }
            Ast::Noun(inner) => self.emit_ast(inner)?,
            Ast::Return(e) => { self.emit_ast(e)?; self.emit(Op::Ret); }
            Ast::Lambda(params, body) => {
                let (code, caps) = compile_fn(params, body, self.scope.clone())?;
                for c in &caps { self.emit_ast(&Ast::Name(c.clone()))?; }   // captured values, pushed before the lambda
                let k = self.k(Value::Lambda(Rc::new(code))); self.emit(Op::Push(k));
                if !caps.is_empty() { self.emit(Op::MkClosure(caps.len() as u32)); }
            }
            Ast::Cond(args) => {
                let mut ends = Vec::new(); let mut i = 0;
                while i + 1 < args.len() {
                    self.emit_ast(&args[i])?; let j = self.emit(Op::Jmpf(0));
                    self.emit_ast(&args[i + 1])?; ends.push(self.emit(Op::Jmp(0))); self.patch(j); i += 2;
                }
                if i < args.len() { self.emit_ast(&args[i])?; } else { self.null(); }
                for e in ends { self.patch(e); }
            }
            Ast::If(c, body) => {
                self.emit_ast(c)?; let j = self.emit(Op::Jmpf(0));
                self.block(body)?; self.patch(j); self.null();
            }
            Ast::While(c, body) => {
                let top = self.ops.len();
                self.emit_ast(c)?; let j = self.emit(Op::Jmpf(0));
                self.loops.push((false, vec![]));
                self.block(body)?; self.emit(Op::Jmp(top as u32)); self.patch(j);
                for b in self.loops.pop().unwrap().1 { self.patch(b); }
                self.null();
            }
            Ast::Do(n, body) => {
                self.emit_ast(n)?;
                let top = self.ops.len(); let j = self.emit(Op::Loop(0));
                self.loops.push((true, vec![]));
                self.block(body)?; self.emit(Op::Jmp(top as u32)); self.patch(j);
                for b in self.loops.pop().unwrap().1 { self.patch(b); }
                self.null();
            }
            // a statement's line: every op emitted for it is tagged with it, then the enclosing line resumes
            Ast::At(l, e) => { let save = self.cur; self.cur = *l; self.emit_ast(e)?; self.cur = save; }
            Ast::Break => {
                let Some(&(is_do, _)) = self.loops.last() else { return err("compile: break outside a loop") };
                if is_do { self.emit(Op::Pop); }   // drop the loop counter
                let j = self.emit(Op::Jmp(0));
                self.loops.last_mut().unwrap().1.push(j);
            }
        }
        Ok(())
    }

    /// Emit the call for a function-position node: constant callee -> Monad/Dyad, else Call.
    fn callee(&mut self, f: &Ast, arity: u32) -> R<()> {
        match fnconst(f) {
            Some(v) => { let k = self.k(v); self.emit(if arity == 1 { Op::Monad(k) } else { Op::Dyad(k) }); }
            None => { self.emit_ast(f)?; self.emit(Op::Call(arity)); }
        }
        Ok(())
    }
    fn null(&mut self) { let k = self.k(Value::Null); self.emit(Op::Push(k)); }
    fn block(&mut self, stmts: &[Ast]) -> R<()> {
        for s in stmts { self.emit_ast(s)?; self.emit(Op::Pop); }
        Ok(())
    }
    fn body(&mut self, stmts: &[Ast]) -> R<()> {
        if stmts.is_empty() { self.null(); return Ok(()); }
        for (i, s) in stmts.iter().enumerate() {
            self.emit_ast(s)?;
            if i + 1 < stmts.len() { self.emit(Op::Pop); }
        }
        Ok(())
    }
}

/// `x: x, rhs` (what `x,: rhs` parses to) -> Some(rhs)
fn append_rhs<'a>(name: &str, e: &'a Ast) -> Option<&'a Ast> {
    match e {
        Ast::Dy(f, x, rhs) if matches!(**f, Ast::Verb(',')) && matches!(&**x, Ast::Name(m) if m == name) => Some(rhs),
        _ => None,
    }
}

fn fnconst(n: &Ast) -> Option<Value> {
    match n {
        Ast::Verb(c) => Some(Value::Prim(prim(*c))),
        Ast::Advb(c, inner) => fnconst(inner).map(|f| Value::Adv(*c, Rc::new(f))),
        _ => None,
    }
}

/// Compile a lambda body; returns the code and the enclosing-local names it captured, in slot order.
fn compile_fn(params: &[String], body: &[Ast], parent: Option<Scope>) -> R<(FnCode, Vec<String>)> {
    let mut slots: HashMap<String, u32> = params.iter().enumerate().map(|(i, p)| (p.clone(), i as u32)).collect();
    for s in body {
        walk(s, &mut |a| if let Ast::Assign(n, _) = a {
            if !slots.contains_key(n) { slots.insert(n.clone(), slots.len() as u32); }
        });
    }
    let nbase = slots.len() as u32;
    let mut c = Compiler { ops: vec![], consts: vec![], lines: vec![], cur: 0, scope: Some(Scope { slots, nbase, captures: vec![], parent: parent.map(Box::new) }), loops: vec![] };
    c.body(body)?;
    let sc = c.scope.take().unwrap();
    Ok((FnCode { ops: c.ops, consts: c.consts, lines: c.lines, params: params.to_vec(), nlocals: nbase as usize }, sc.captures))
}

pub fn compile_stmt(ast: &Ast) -> R<(Vec<Op>, Vec<Value>, Vec<u32>)> {
    let mut c = Compiler { ops: vec![], consts: vec![], lines: vec![], cur: 0, scope: None, loops: vec![] };
    c.emit_ast(ast)?;
    Ok((c.ops, c.consts, c.lines))
}

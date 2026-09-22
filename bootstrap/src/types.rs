//! Name resolution and type checking in one pass, producing the typed IR. Every rule here that
//! rejects a program is a rule the cost calculus will need: arrays live in locals, views come
//! from `&local`, sizes are attached where a value is born.

use std::collections::HashMap;

use crate::ast;
use crate::ast::{BinOp, UnOp};
use crate::diag::{err, Result};
use crate::ir::*;

pub fn check(prog: &ast::Program) -> Result<Module> {
    // signatures first, so calls can go in any order
    let mut sigs: HashMap<String, (FuncId, Vec<Ty>, Ty)> = HashMap::new();
    for (i, f) in prog.funcs.iter().enumerate() {
        if sigs.contains_key(&f.name) {
            return err(f.line, f.col, format!("function `{}` is defined twice", f.name));
        }
        if f.name == "println" {
            return err(f.line, f.col, "`println` is a builtin");
        }
        let mut ptys = Vec::new();
        for p in &f.params {
            let ty = resolve_type(&p.ty, p.line, p.col)?;
            match ty {
                Ty::Array(..) => return err(p.line, p.col, "arrays are passed as views: write `&[T]` or `&mut [T]`"),
                _ => ptys.push(ty),
            }
        }
        let ret = resolve_type(&f.ret, f.line, f.col)?;
        if ret.is_arrayish() {
            return err(f.line, f.col, "functions return scalars or `()` for now; write into an `&mut [T]` parameter");
        }
        sigs.insert(f.name.clone(), (i, ptys, ret));
    }
    let main = sigs.get("main");
    match main {
        None => return err(1, 1, "no `fn main()`"),
        Some((_, p, r)) if !p.is_empty() || *r != Ty::Unit => {
            let f = prog.funcs.iter().find(|f| f.name == "main").unwrap();
            return err(f.line, f.col, "`main` takes no parameters and returns `()`");
        }
        _ => {}
    }

    let mut funcs = Vec::new();
    for f in &prog.funcs {
        funcs.push(check_func(f, &sigs)?);
    }
    Ok(Module { funcs })
}

/// Only the size-free part of a type expression; sizes are attached by the checker where a
/// value is created.
fn resolve_type(t: &ast::TypeExpr, line: u32, col: u32) -> Result<Ty> {
    Ok(match t {
        ast::TypeExpr::Unit => Ty::Unit,
        ast::TypeExpr::Named(n) => match n.as_str() {
            "i64" => Ty::I64,
            "f64" => Ty::F64,
            "bool" => Ty::Bool,
            other => return err(line, col, format!("unknown type `{other}`")),
        },
        ast::TypeExpr::Slice(elem, m) => {
            let e = resolve_type(elem, line, col)?;
            if !e.is_scalar() {
                return err(line, col, "element type of a slice must be scalar for now");
            }
            Ty::Slice(Box::new(e), *m, Size::Const(-1))
        }
        ast::TypeExpr::Array(elem, n) => {
            let e = resolve_type(elem, line, col)?;
            if !e.is_scalar() {
                return err(line, col, "element type of an array must be scalar for now");
            }
            match n.kind {
                ast::ExprKind::Int(k) if k >= 0 => Ty::Array(Box::new(e), Size::Const(k)),
                _ => return err(n.line, n.col, "an array type's length must be an integer literal here"),
            }
        }
    })
}

struct Ctx<'a> {
    sigs: &'a HashMap<String, (FuncId, Vec<Ty>, Ty)>,
    locals: Vec<Local>,
    scopes: Vec<HashMap<String, LocalId>>,
    sizes: Vec<SizeInfo>,
    ret: Ty,
    in_loop: usize,
    /// inside a closure body: assignment is an error, so a chain stage is pure and fusion is safe
    in_closure: usize,
    fresh: usize,
    /// extra `let`s a terminal needs before its loop (`seen` for max/min)
    pending_lets: Vec<Stmt>,
}

fn check_func(f: &ast::Func, sigs: &HashMap<String, (FuncId, Vec<Ty>, Ty)>) -> Result<Func> {
    let (_, ptys, ret) = &sigs[&f.name];
    let mut cx = Ctx { sigs, locals: vec![], scopes: vec![HashMap::new()], sizes: vec![], ret: ret.clone(), in_loop: 0, in_closure: 0, fresh: 0, pending_lets: vec![] };
    let mut params = Vec::new();
    for (p, ty) in f.params.iter().zip(ptys) {
        // a slice parameter's size is its own variable, named after the parameter
        let ty = match ty {
            Ty::Slice(e, m, _) => {
                let sv = cx.new_size(format!("{}.len()", p.name));
                Ty::Slice(e.clone(), *m, Size::Var(sv))
            }
            t => t.clone(),
        };
        if cx.scopes[0].contains_key(&p.name) {
            return err(p.line, p.col, format!("parameter `{}` is declared twice", p.name));
        }
        let id = cx.declare(&p.name, ty, false);
        params.push(id);
    }
    let body = cx.block(&f.body)?;
    if body.tail.is_none() && *ret != Ty::Unit && !ends_in_return(&body) {
        return err(f.line, f.col, format!("`{}` returns `{ret}` but its body has no value", f.name));
    }
    if let Some(t) = &body.tail {
        if !t.ty.same_shape(ret) {
            return err(t.line, 0, format!("`{}` returns `{ret}`, but its body has type `{}`", f.name, t.ty));
        }
    }
    Ok(Func { name: f.name.clone(), params, ret: ret.clone(), locals: cx.locals, sizes: cx.sizes, body, line: f.line })
}

fn ends_in_return(b: &Block) -> bool {
    matches!(b.stmts.last(), Some(Stmt::Return(_)))
}

impl<'a> Ctx<'a> {
    fn new_size(&mut self, name: String) -> SizeVar {
        self.sizes.push(SizeInfo { name });
        self.sizes.len() - 1
    }
    fn declare(&mut self, name: &str, ty: Ty, mutable: bool) -> LocalId {
        self.locals.push(Local { name: name.to_string(), ty, mutable });
        let id = self.locals.len() - 1;
        self.scopes.last_mut().unwrap().insert(name.to_string(), id);
        id
    }
    fn lookup(&self, name: &str) -> Option<LocalId> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }
    fn local_by_expr(&self, e: &ast::Expr, what: &str) -> Result<LocalId> {
        match &e.kind {
            ast::ExprKind::Var(n) => self.lookup(n).map_or_else(
                || err(e.line, e.col, format!("unknown variable `{n}`")),
                Ok,
            ),
            _ => err(e.line, e.col, format!("{what} must be a variable for now")),
        }
    }

    fn block(&mut self, b: &ast::Block) -> Result<Block> {
        self.scopes.push(HashMap::new());
        let mut stmts = Vec::new();
        for s in &b.stmts {
            stmts.push(self.stmt(s)?);
        }
        let tail = match &b.tail {
            Some(e) => Some(Box::new(self.expr(e)?)),
            None => None,
        };
        self.scopes.pop();
        let ty = match &tail {
            Some(t) => t.ty.clone(),
            None => Ty::Unit,
        };
        if ty.is_arrayish() {
            return err(b.line, b.col, "a block cannot have an array value for now");
        }
        Ok(Block { stmts, tail, ty })
    }

    fn stmt(&mut self, s: &ast::Stmt) -> Result<Stmt> {
        match s {
            ast::Stmt::Let { name, mutable, ty, init, line, col } => {
                let declared = match ty {
                    Some(t) => Some(resolve_type(t, *line, *col)?),
                    None => None,
                };
                // arrays are born here and only here
                match &init.kind {
                    ast::ExprKind::ArrayLit(elems) => {
                        let mut out = Vec::new();
                        for e in elems {
                            let ce = self.expr(e)?;
                            if !ce.ty.is_scalar() {
                                return err(e.line, e.col, "array elements must be scalar");
                            }
                            if let Some(first) = out.first() {
                                let f: &Expr = first;
                                if f.ty != ce.ty {
                                    return err(e.line, e.col, format!("array elements differ in type: `{}` and `{}`", f.ty, ce.ty));
                                }
                            }
                            out.push(ce);
                        }
                        let elem = out[0].ty.clone();
                        let ty = Ty::Array(Box::new(elem), Size::Const(out.len() as i64));
                        self.check_declared(&declared, &ty, *line, *col)?;
                        let id = self.declare(name, ty, *mutable);
                        Ok(Stmt::LetArray(id, out))
                    }
                    ast::ExprKind::Comprehension { elem, var, source, cond } => {
                        if cond.is_some() {
                            return err(init.line, init.col, "the length of a filtered comprehension depends on the data; reduce it (`.sum()`, `.count()`, ...) or drop the `if`");
                        }
                        let src = self.local_by_expr(source, "the source of a comprehension")?;
                        if !self.locals[src].ty.is_arrayish() {
                            return err(source.line, source.col, format!("iterating over a `{}`", self.locals[src].ty));
                        }
                        let src_elem = self.locals[src].ty.elem().unwrap().clone();
                        let len = Expr { kind: ExprKind::Len(src), ty: Ty::I64, line: init.line };
                        // body: { let var = src[k]; elem }
                        self.scopes.push(HashMap::new());
                        let k = self.fresh_local("k", Ty::I64, false);
                        let x = self.declare(var, src_elem.clone(), false);
                        let load = Expr { kind: ExprKind::Index(src, Box::new(Expr { kind: ExprKind::Local(k), ty: Ty::I64, line: init.line })), ty: src_elem, line: init.line };
                        let ce = self.expr(elem)?;
                        if !ce.ty.is_scalar() {
                            return err(elem.line, elem.col, "comprehension elements must be scalar");
                        }
                        self.scopes.pop();
                        let body = Block { stmts: vec![Stmt::Let(x, load)], tail: Some(Box::new(ce.clone())), ty: ce.ty.clone() };
                        let size = match self.locals[src].ty.size() { Some(sz) => sz.clone(), None => Size::Const(-1) };
                        let ty = Ty::Array(Box::new(ce.ty.clone()), size);
                        self.check_declared(&declared, &ty, *line, *col)?;
                        let id = self.declare(name, ty, *mutable);
                        Ok(Stmt::LetBuild { id, len, var: k, body })
                    }
                    ast::ExprKind::ArrayRepeat(e, n) => {
                        let ce = self.expr(e)?;
                        if !ce.ty.is_scalar() {
                            return err(e.line, e.col, "array elements must be scalar");
                        }
                        let cn = self.expr(n)?;
                        if cn.ty != Ty::I64 {
                            return err(n.line, n.col, format!("array length must be `i64`, found `{}`", cn.ty));
                        }
                        let size = match &cn.kind {
                            ExprKind::Int(k) => Size::Const(*k),
                            ExprKind::Local(l) => {
                                let nm = self.locals[*l].name.clone();
                                Size::Var(self.new_size(nm))
                            }
                            ExprKind::Len(l) => {
                                let nm = format!("{}.len()", self.locals[*l].name);
                                Size::Var(self.new_size(nm))
                            }
                            _ => {
                                let k = self.sizes.len();
                                Size::Var(self.new_size(format!("{name}.len()#{k}")))
                            }
                        };
                        let ty = Ty::Array(Box::new(ce.ty.clone()), size);
                        self.check_declared(&declared, &ty, *line, *col)?;
                        let id = self.declare(name, ty, *mutable);
                        Ok(Stmt::LetRepeat(id, ce, cn))
                    }
                    _ => {
                        let ce = self.expr(init)?;
                        if let Ty::Array(..) = ce.ty {
                            return err(init.line, init.col, "an array cannot be moved into another variable; take a view with `&`");
                        }
                        if ce.ty == Ty::Unit {
                            return err(init.line, init.col, "cannot bind a `()` value");
                        }
                        self.check_declared(&declared, &ce.ty, *line, *col)?;
                        let id = self.declare(name, ce.ty.clone(), *mutable);
                        Ok(Stmt::Let(id, ce))
                    }
                }
            }
            ast::Stmt::Assign { target, op, value, line, col } => {
                if self.in_closure > 0 {
                    return err(*line, *col, "a closure in a chain cannot assign; it must be a pure function of its arguments");
                }
                let val = self.expr(value)?;
                let (lv, tty) = match &target.kind {
                    ast::ExprKind::Var(n) => {
                        let id = self.lookup(n).map_or_else(|| err(target.line, target.col, format!("unknown variable `{n}`")), Ok)?;
                        let l = &self.locals[id];
                        if !l.mutable {
                            return err(target.line, target.col, format!("`{n}` is not mutable; declare it with `let mut`"));
                        }
                        if l.ty.is_arrayish() {
                            return err(target.line, target.col, "cannot assign a whole array or view; assign elements");
                        }
                        (LValue::Var(id), l.ty.clone())
                    }
                    ast::ExprKind::Index(base, idx) => {
                        let id = self.local_by_expr(base, "the array being indexed")?;
                        let ci = self.expr(idx)?;
                        if ci.ty != Ty::I64 {
                            return err(idx.line, idx.col, format!("index must be `i64`, found `{}`", ci.ty));
                        }
                        let l = &self.locals[id];
                        let elem = match &l.ty {
                            Ty::Array(e, _) => {
                                if !l.mutable {
                                    return err(target.line, target.col, format!("`{}` is not mutable; declare it with `let mut`", l.name));
                                }
                                (**e).clone()
                            }
                            Ty::Slice(e, true, _) => (**e).clone(),
                            Ty::Slice(_, false, _) => return err(target.line, target.col, format!("`{}` is a `&[T]` view; writing needs `&mut [T]`", l.name)),
                            t => return err(target.line, target.col, format!("cannot index a value of type `{t}`")),
                        };
                        (LValue::Index(id, ci, target.line), elem)
                    }
                    _ => return err(*line, *col, "assignment target must be a variable or an element"),
                };
                if let Some(op) = op {
                    if !tty.is_numeric() {
                        return err(target.line, target.col, format!("`{}=` needs a numeric target, found `{tty}`", op.c_str()));
                    }
                }
                if val.ty != tty {
                    return err(value.line, value.col, format!("assigning `{}` to a `{tty}`", val.ty));
                }
                Ok(Stmt::Assign(lv, *op, val))
            }
            ast::Stmt::For { var, start, end, body, line, col } => {
                let s = self.expr(start)?;
                let e = self.expr(end)?;
                if s.ty != Ty::I64 || e.ty != Ty::I64 {
                    return err(*line, *col, "range bounds must be `i64`");
                }
                self.scopes.push(HashMap::new());
                let id = self.declare(var, Ty::I64, false);
                self.in_loop += 1;
                let b = self.block(body)?;
                self.in_loop -= 1;
                self.scopes.pop();
                if b.ty != Ty::Unit {
                    return err(body.line, body.col, format!("a loop body has type `()`, found `{}`", b.ty));
                }
                Ok(Stmt::For { var: id, start: s, end: e, body: b })
            }
            ast::Stmt::Expr(e) => {
                let ce = self.expr(e)?;
                if ce.ty.is_arrayish() {
                    return err(e.line, e.col, "an array expression on its own does nothing");
                }
                Ok(Stmt::Expr(ce))
            }
            ast::Stmt::Return(e, line, col) => {
                let ce = match e {
                    Some(e) => Some(self.expr(e)?),
                    None => None,
                };
                let ty = ce.as_ref().map_or(Ty::Unit, |e| e.ty.clone());
                if !ty.same_shape(&self.ret) {
                    return err(*line, *col, format!("returning `{ty}` from a function that returns `{}`", self.ret));
                }
                Ok(Stmt::Return(ce))
            }
        }
    }

    fn check_declared(&self, declared: &Option<Ty>, actual: &Ty, line: u32, col: u32) -> Result<()> {
        if let Some(d) = declared {
            let ok = match (d, actual) {
                (Ty::Array(a, Size::Const(n)), Ty::Array(b, Size::Const(m))) => a == b && n == m,
                (Ty::Array(a, _), Ty::Array(b, _)) => a == b,
                (a, b) => a.same_shape(b),
            };
            if !ok {
                return err(line, col, format!("declared `{d}` but the value has type `{actual}`"));
            }
        }
        Ok(())
    }

    fn expr(&mut self, e: &ast::Expr) -> Result<Expr> {
        let line = e.line;
        let mk = |kind, ty| Ok(Expr { kind, ty, line });
        match &e.kind {
            ast::ExprKind::Int(v) => mk(ExprKind::Int(*v), Ty::I64),
            ast::ExprKind::Float(v) => mk(ExprKind::Float(*v), Ty::F64),
            ast::ExprKind::Bool(v) => mk(ExprKind::Bool(*v), Ty::Bool),
            ast::ExprKind::Var(n) => {
                let id = self.lookup(n).map_or_else(|| err(e.line, e.col, format!("unknown variable `{n}`")), Ok)?;
                let ty = self.locals[id].ty.clone();
                mk(ExprKind::Local(id), ty)
            }
            ast::ExprKind::Binary(op, a, b) => {
                let ca = self.expr(a)?;
                let cb = self.expr(b)?;
                if ca.ty != cb.ty {
                    return err(e.line, e.col, format!("`{}` between `{}` and `{}`; no implicit conversion, use `as`", op.c_str(), ca.ty, cb.ty));
                }
                let ty = if op.is_arith() {
                    if !ca.ty.is_numeric() {
                        return err(e.line, e.col, format!("`{}` on `{}`", op.c_str(), ca.ty));
                    }
                    ca.ty.clone()
                } else if op.is_cmp() {
                    if !ca.ty.is_scalar() {
                        return err(e.line, e.col, format!("`{}` on `{}`", op.c_str(), ca.ty));
                    }
                    if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) && ca.ty == Ty::Bool {
                        return err(e.line, e.col, "ordering comparison on `bool`");
                    }
                    Ty::Bool
                } else {
                    if ca.ty != Ty::Bool {
                        return err(e.line, e.col, format!("`{}` needs `bool`, found `{}`", op.c_str(), ca.ty));
                    }
                    Ty::Bool
                };
                mk(ExprKind::Binary(*op, Box::new(ca), Box::new(cb)), ty)
            }
            ast::ExprKind::Unary(op, a) => {
                let ca = self.expr(a)?;
                match op {
                    UnOp::Neg if !ca.ty.is_numeric() => return err(e.line, e.col, format!("`-` on `{}`", ca.ty)),
                    UnOp::Not if ca.ty != Ty::Bool => return err(e.line, e.col, format!("`!` on `{}`", ca.ty)),
                    _ => {}
                }
                let ty = ca.ty.clone();
                mk(ExprKind::Unary(*op, Box::new(ca)), ty)
            }
            ast::ExprKind::Index(base, idx) => {
                let id = self.local_by_expr(base, "the array being indexed")?;
                let ci = self.expr(idx)?;
                if ci.ty != Ty::I64 {
                    return err(idx.line, idx.col, format!("index must be `i64`, found `{}`", ci.ty));
                }
                let elem = match self.locals[id].ty.elem() {
                    Some(t) => t.clone(),
                    None => return err(e.line, e.col, format!("cannot index a value of type `{}`", self.locals[id].ty)),
                };
                mk(ExprKind::Index(id, Box::new(ci)), elem)
            }
            ast::ExprKind::Call(name, args) if name == "min" || name == "max" => {
                if args.len() != 2 {
                    return err(e.line, e.col, format!("`{name}` takes two arguments"));
                }
                let a = self.expr(&args[0])?;
                let b = self.expr(&args[1])?;
                if a.ty != b.ty || !a.ty.is_numeric() {
                    return err(e.line, e.col, format!("`{name}` of `{}` and `{}`", a.ty, b.ty));
                }
                let ty = a.ty.clone();
                mk(ExprKind::MinMax(name == "min", Box::new(a), Box::new(b)), ty)
            }
            ast::ExprKind::Call(name, args) => {
                if name == "println" {
                    if args.len() != 1 {
                        return err(e.line, e.col, "`println` takes one argument");
                    }
                    let a = self.expr(&args[0])?;
                    if !a.ty.is_scalar() {
                        return err(args[0].line, args[0].col, format!("`println` prints scalars, found `{}`", a.ty));
                    }
                    return mk(ExprKind::Println(Box::new(a)), Ty::Unit);
                }
                let Some((fid, ptys, ret)) = self.sigs.get(name).cloned() else {
                    return err(e.line, e.col, format!("unknown function `{name}`"));
                };
                if args.len() != ptys.len() {
                    return err(e.line, e.col, format!("`{name}` takes {} argument(s), {} given", ptys.len(), args.len()));
                }
                let mut cargs = Vec::new();
                for (a, pty) in args.iter().zip(&ptys) {
                    let ca = self.expr(a)?;
                    let ok = match (&ca.ty, pty) {
                        (Ty::Slice(ea, ma, _), Ty::Slice(ep, mp, _)) => ea == ep && (*ma || !*mp),
                        (Ty::Array(..), Ty::Slice(..)) => return err(a.line, a.col, "pass a view of the array: `&name` or `&mut name`"),
                        (a, b) => a == b,
                    };
                    if !ok {
                        return err(a.line, a.col, format!("argument of type `{}` where `{pty}` is expected", ca.ty));
                    }
                    cargs.push(ca);
                }
                mk(ExprKind::Call(fid, cargs), ret)
            }
            ast::ExprKind::MethodCall(recv, name, args) => {
                if name == "len" {
                    if !args.is_empty() {
                        return err(e.line, e.col, "`.len()` takes no arguments");
                    }
                    let id = self.local_by_expr(recv, "the receiver of `.len()`")?;
                    if !self.locals[id].ty.is_arrayish() {
                        return err(e.line, e.col, format!("`.len()` on `{}`", self.locals[id].ty));
                    }
                    return mk(ExprKind::Len(id), Ty::I64);
                }
                self.chain(e)
            }
            ast::ExprKind::Lambda(..) => err(e.line, e.col, "a closure can only be the argument of a chain stage (`map`, `filter`, `fold`, ...)"),
            ast::ExprKind::Comprehension { .. } => err(e.line, e.col, "a comprehension either initialises a `let` or is reduced: `[..].sum()`"),
            ast::ExprKind::Ref(inner, mutable) => {
                let id = self.local_by_expr(inner, "the operand of `&`")?;
                let l = &self.locals[id];
                let ty = match &l.ty {
                    Ty::Array(el, sz) => {
                        if *mutable && !l.mutable {
                            return err(e.line, e.col, format!("`&mut` of `{}`, which is not `let mut`", l.name));
                        }
                        Ty::Slice(el.clone(), *mutable, sz.clone())
                    }
                    Ty::Slice(el, m, sz) => {
                        if *mutable && !*m {
                            return err(e.line, e.col, format!("`&mut` of `{}`, which is a `&[T]` view", l.name));
                        }
                        Ty::Slice(el.clone(), *mutable, sz.clone())
                    }
                    t => return err(e.line, e.col, format!("`&` of a `{t}`; views are of arrays")),
                };
                mk(ExprKind::Ref(id, *mutable), ty)
            }
            ast::ExprKind::Cast(inner, ty) => {
                let ci = self.expr(inner)?;
                let target = resolve_type(ty, e.line, e.col)?;
                if !ci.ty.is_numeric() || !target.is_numeric() {
                    return err(e.line, e.col, format!("`as` converts between `i64` and `f64`; found `{}` as `{target}`", ci.ty));
                }
                mk(ExprKind::Cast(Box::new(ci), target.clone()), target)
            }
            ast::ExprKind::If(cond, then, els) => {
                let cc = self.expr(cond)?;
                if cc.ty != Ty::Bool {
                    return err(cond.line, cond.col, format!("`if` condition is `{}`, not `bool`", cc.ty));
                }
                let ct = self.block(then)?;
                let ce = match els {
                    Some(b) => Some(self.block(b)?),
                    None => None,
                };
                let ty = match &ce {
                    None => {
                        if ct.ty != Ty::Unit {
                            return err(e.line, e.col, format!("`if` without `else` has type `()`, but the branch has type `{}`", ct.ty));
                        }
                        Ty::Unit
                    }
                    Some(b) => {
                        if b.ty != ct.ty {
                            return err(e.line, e.col, format!("`if` branches have types `{}` and `{}`", ct.ty, b.ty));
                        }
                        ct.ty.clone()
                    }
                };
                mk(ExprKind::If(Box::new(cc), ct, ce), ty)
            }
            ast::ExprKind::Block(b) => {
                let cb = self.block(b)?;
                let ty = cb.ty.clone();
                mk(ExprKind::Block(cb), ty)
            }
            ast::ExprKind::ArrayLit(_) | ast::ExprKind::ArrayRepeat(..) => {
                err(e.line, e.col, "an array literal can only initialise a `let` for now")
            }
        }
    }

    fn next_fresh(&mut self) -> usize { self.fresh += 1; self.fresh }

    /// A compiler-made local; the `#` keeps it out of the user's namespace.
    fn fresh_local(&mut self, base: &str, ty: Ty, mutable: bool) -> LocalId {
        let n = self.next_fresh();
        self.declare(&format!("{base}#{n}"), ty, mutable)
    }

    fn local_expr(&self, id: LocalId, line: u32) -> Expr {
        Expr { kind: ExprKind::Local(id), ty: self.locals[id].ty.clone(), line }
    }

    /// An iterator chain, reduced. `xs.iter().map(|x| ..).filter(|x| ..).sum()` and the
    /// comprehension form `[e for x in xs if c].sum()` become one loop over the source with
    /// the stages inlined into its body — fusion by construction, there is no other form.
    fn chain(&mut self, e: &ast::Expr) -> Result<Expr> {
        // flatten: receiver ← stage ← stage ← terminal
        let mut stages: Vec<(&str, &Vec<ast::Expr>, u32, u32)> = Vec::new();
        let mut cur = e;
        while let ast::ExprKind::MethodCall(recv, name, args) = &cur.kind {
            stages.push((name.as_str(), args, cur.line, cur.col));
            cur = recv;
        }
        stages.reverse();
        let (source_expr, mut pre_stages): (&ast::Expr, Vec<Stage>) = match &cur.kind {
            ast::ExprKind::Comprehension { elem, var, source, cond } => {
                let mut st = Vec::new();
                if let Some(c) = cond { st.push(Stage::Filter(vec![var.clone()], c)); }
                st.push(Stage::Map(vec![var.clone()], elem));
                (source, st)
            }
            _ => (cur, vec![]),
        };
        let src = self.local_by_expr(source_expr, "the source of a chain")?;
        if !self.locals[src].ty.is_arrayish() {
            return err(source_expr.line, source_expr.col, format!("iterating over a `{}`", self.locals[src].ty));
        }
        let Some((term_name, term_args, tline, tcol)) = stages.pop() else {
            return err(e.line, e.col, "a chain needs a terminal");
        };
        let mut all: Vec<Stage> = std::mem::take(&mut pre_stages);
        let mut zip_src: Option<LocalId> = None;
        for (name, args, line, col) in stages {
            let lam = |k: usize| -> Result<(&Vec<String>, &ast::Expr)> {
                match args.get(k).map(|a| &a.kind) {
                    Some(ast::ExprKind::Lambda(ps, body)) => Ok((ps, body)),
                    _ => err(line, col, format!("`.{name}()` takes a closure")),
                }
            };
            match name {
                "iter" => { if !args.is_empty() { return err(line, col, "`.iter()` takes no arguments"); } }
                "map" => { let (ps, b) = lam(0)?; all.push(Stage::Map(ps.clone(), b)); }
                "filter" => { let (ps, b) = lam(0)?; all.push(Stage::Filter(ps.clone(), b)); }
                "enumerate" => all.push(Stage::Enumerate),
                "zip" => {
                    if zip_src.is_some() || !all.is_empty() {
                        return err(line, col, "`.zip()` must come first, directly after the source");
                    }
                    let Some(other) = args.first() else { return err(line, col, "`.zip()` takes the other array") };
                    let o = self.local_by_expr(other, "the argument of `.zip()`")?;
                    if !self.locals[o].ty.is_arrayish() {
                        return err(other.line, other.col, format!("zipping with a `{}`", self.locals[o].ty));
                    }
                    zip_src = Some(o);
                }
                other => return err(line, col, format!("unknown chain stage `.{other}()`; stages are iter, map, filter, zip, enumerate; terminals are sum, count, fold, max, min, any, all")),
            }
        }

        // the loop
        self.scopes.push(HashMap::new());
        let line = e.line;
        let elem_ty = self.locals[src].ty.elem().unwrap().clone();
        let i = self.fresh_local("i", Ty::I64, false);
        let len = match zip_src {
            None => Expr { kind: ExprKind::Len(src), ty: Ty::I64, line },
            Some(o) => Expr { kind: ExprKind::MinMax(true, Box::new(Expr { kind: ExprKind::Len(src), ty: Ty::I64, line }), Box::new(Expr { kind: ExprKind::Len(o), ty: Ty::I64, line })), ty: Ty::I64, line },
        };
        let idx = |this: &Self, l: LocalId| Expr { kind: ExprKind::Index(l, Box::new(this.local_expr(i, line))), ty: this.locals[l].ty.elem().unwrap().clone(), line };
        // current values flowing through the stages, as locals
        let mut body_stmts: Vec<Stmt> = Vec::new();
        let x0 = self.fresh_local("x", elem_ty.clone(), false);
        body_stmts.push(Stmt::Let(x0, idx(self, src)));
        let mut vals: Vec<LocalId> = vec![x0];
        if let Some(o) = zip_src {
            let oty = self.locals[o].ty.elem().unwrap().clone();
            let y0 = self.fresh_local("y", oty, false);
            body_stmts.push(Stmt::Let(y0, idx(self, o)));
            vals.push(y0);
        }
        // filters wrap everything after them; collect (stmts-so-far, condition) breakpoints
        let mut guards: Vec<(Vec<Stmt>, Expr)> = Vec::new();
        for st in &all {
            match st {
                Stage::Enumerate => {
                    let k = self.fresh_local("k", Ty::I64, false);
                    body_stmts.push(Stmt::Let(k, self.local_expr(i, line)));
                    vals.insert(0, k);
                }
                Stage::Map(ps, body) => {
                    let (binds, v) = self.apply_closure(ps, body, &vals, line)?;
                    body_stmts.extend(binds);
                    if v.ty == Ty::Unit || v.ty.is_arrayish() {
                        return err(body.line, body.col, format!("a `map` closure must produce a scalar, found `{}`", v.ty));
                    }
                    let nv = self.fresh_local("v", v.ty.clone(), false);
                    body_stmts.push(Stmt::Let(nv, v));
                    vals = vec![nv];
                }
                Stage::Filter(ps, body) => {
                    let (binds, c) = self.apply_closure(ps, body, &vals, line)?;
                    body_stmts.extend(binds);
                    if c.ty != Ty::Bool {
                        return err(body.line, body.col, format!("a `filter` closure must produce `bool`, found `{}`", c.ty));
                    }
                    guards.push((std::mem::take(&mut body_stmts), c));
                }
            }
        }
        // the terminal
        let cur_ty = if vals.len() == 1 { self.locals[vals[0]].ty.clone() } else { Ty::Unit };
        let (acc, init, update): (LocalId, Expr, Vec<Stmt>) = match term_name {
            "sum" => {
                if vals.len() != 1 || !cur_ty.is_numeric() { return err(tline, tcol, format!("`.sum()` needs one numeric value per element, found `{cur_ty}`")); }
                let acc = self.fresh_local("acc", cur_ty.clone(), true);
                let zero = if cur_ty == Ty::F64 { ExprKind::Float(0.0) } else { ExprKind::Int(0) };
                (acc, Expr { kind: zero, ty: cur_ty.clone(), line }, vec![Stmt::Assign(LValue::Var(acc), Some(BinOp::Add), self.local_expr(vals[0], line))])
            }
            "count" => {
                let acc = self.fresh_local("acc", Ty::I64, true);
                (acc, Expr { kind: ExprKind::Int(0), ty: Ty::I64, line }, vec![Stmt::Assign(LValue::Var(acc), Some(BinOp::Add), Expr { kind: ExprKind::Int(1), ty: Ty::I64, line })])
            }
            "max" | "min" => {
                if vals.len() != 1 || !cur_ty.is_numeric() { return err(tline, tcol, format!("`.{term_name}()` needs one numeric value per element, found `{cur_ty}`")); }
                let acc = self.fresh_local("acc", cur_ty.clone(), true);
                let seen = self.fresh_local("seen", Ty::Bool, true);
                let zero = if cur_ty == Ty::F64 { ExprKind::Float(0.0) } else { ExprKind::Int(0) };
                let cmp = if term_name == "max" { BinOp::Gt } else { BinOp::Lt };
                let better = Expr { kind: ExprKind::Binary(cmp, Box::new(self.local_expr(vals[0], line)), Box::new(self.local_expr(acc, line))), ty: Ty::Bool, line };
                let notseen = Expr { kind: ExprKind::Unary(UnOp::Not, Box::new(self.local_expr(seen, line))), ty: Ty::Bool, line };
                let cond = Expr { kind: ExprKind::Binary(BinOp::Or, Box::new(notseen), Box::new(better)), ty: Ty::Bool, line };
                let then = Block { stmts: vec![
                    Stmt::Assign(LValue::Var(acc), None, self.local_expr(vals[0], line)),
                    Stmt::Assign(LValue::Var(seen), None, Expr { kind: ExprKind::Bool(true), ty: Ty::Bool, line }),
                ], tail: None, ty: Ty::Unit };
                let upd = Stmt::Expr(Expr { kind: ExprKind::If(Box::new(cond), then, None), ty: Ty::Unit, line });
                // `seen` is declared here so it is initialised before the loop, alongside acc
                self.pending_lets.push(Stmt::Let(seen, Expr { kind: ExprKind::Bool(false), ty: Ty::Bool, line }));
                (acc, Expr { kind: zero, ty: cur_ty.clone(), line }, vec![upd])
            }
            "any" | "all" => {
                if vals.len() != 1 || cur_ty != Ty::Bool { return err(tline, tcol, format!("`.{term_name}()` needs one `bool` per element; use `.map(|x| ..)` or a closure argument")); }
                let acc = self.fresh_local("acc", Ty::Bool, true);
                let (init, op) = if term_name == "any" { (false, BinOp::Or) } else { (true, BinOp::And) };
                let upd = Stmt::Assign(LValue::Var(acc), None, Expr { kind: ExprKind::Binary(op, Box::new(self.local_expr(acc, line)), Box::new(self.local_expr(vals[0], line))), ty: Ty::Bool, line });
                (acc, Expr { kind: ExprKind::Bool(init), ty: Ty::Bool, line }, vec![upd])
            }
            "fold" => {
                let Some(init_e) = term_args.first() else { return err(tline, tcol, "`.fold(init, |acc, x| ..)`") };
                let init = self.expr(init_e)?;
                if !init.ty.is_scalar() { return err(init_e.line, init_e.col, "a fold accumulator must be scalar"); }
                let acc = self.fresh_local("acc", init.ty.clone(), true);
                let Some(ast::ExprKind::Lambda(ps, body)) = term_args.get(1).map(|a| &a.kind) else { return err(tline, tcol, "`.fold(init, |acc, x| ..)`") };
                let mut args = vec![acc];
                args.extend(vals.iter().copied());
                let (binds, v) = self.apply_closure(ps, body, &args, line)?;
                if v.ty != init.ty { return err(body.line, body.col, format!("the fold closure returns `{}` but the accumulator is `{}`", v.ty, init.ty)); }
                let mut upd = binds;
                upd.push(Stmt::Assign(LValue::Var(acc), None, v));
                (acc, init, upd)
            }
            other => return err(tline, tcol, format!("unknown chain terminal `.{other}()`; terminals are sum, count, fold, max, min, any, all")),
        };
        if !term_args.is_empty() && term_name != "fold" {
            return err(tline, tcol, format!("`.{term_name}()` takes no arguments"));
        }
        // assemble: filters nest the remainder in `if`
        let mut inner: Vec<Stmt> = std::mem::take(&mut body_stmts);
        inner.extend(update);
        for (before, cond) in guards.into_iter().rev() {
            let then = Block { stmts: inner, tail: None, ty: Ty::Unit };
            inner = before;
            inner.push(Stmt::Expr(Expr { kind: ExprKind::If(Box::new(cond), then, None), ty: Ty::Unit, line }));
        }
        self.scopes.pop();
        let acc_ty = self.locals[acc].ty.clone();
        let mut stmts = vec![Stmt::Let(acc, init)];
        stmts.extend(std::mem::take(&mut self.pending_lets));
        stmts.push(Stmt::For { var: i, start: Expr { kind: ExprKind::Int(0), ty: Ty::I64, line }, end: len, body: Block { stmts: inner, tail: None, ty: Ty::Unit } });
        Ok(Expr { kind: ExprKind::Block(Block { stmts, tail: Some(Box::new(self.local_expr(acc, line))), ty: acc_ty.clone() }), ty: acc_ty, line })
    }

    /// Bind a closure's parameters to the current values and check its body in that scope.
    /// Returns the `let`s that bind the parameters and the body expression.
    fn apply_closure(&mut self, params: &[String], body: &ast::Expr, vals: &[LocalId], line: u32) -> Result<(Vec<Stmt>, Expr)> {
        if params.len() != vals.len() {
            return err(body.line, body.col, format!("closure takes {} argument(s) but {} value(s) flow into it", params.len(), vals.len()));
        }
        self.scopes.push(HashMap::new());
        let mut binds = Vec::new();
        for (p, &v) in params.iter().zip(vals) {
            let ty = self.locals[v].ty.clone();
            let id = self.declare(p, ty, false);
            binds.push(Stmt::Let(id, self.local_expr(v, line)));
        }
        self.in_closure += 1;
        let r = self.expr(body);
        self.in_closure -= 1;
        self.scopes.pop();
        Ok((binds, r?))
    }
}

enum Stage<'a> {
    Map(Vec<String>, &'a ast::Expr),
    Filter(Vec<String>, &'a ast::Expr),
    Enumerate,
}

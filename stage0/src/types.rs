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
            return err(f.line, f.col, "functions return scalars or `()` in stage 0; write into an `&mut [T]` parameter");
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
                return err(line, col, "element type of a slice must be scalar in stage 0");
            }
            Ty::Slice(Box::new(e), *m, Size::Const(-1))
        }
        ast::TypeExpr::Array(elem, n) => {
            let e = resolve_type(elem, line, col)?;
            if !e.is_scalar() {
                return err(line, col, "element type of an array must be scalar in stage 0");
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
}

fn check_func(f: &ast::Func, sigs: &HashMap<String, (FuncId, Vec<Ty>, Ty)>) -> Result<Func> {
    let (_, ptys, ret) = &sigs[&f.name];
    let mut cx = Ctx { sigs, locals: vec![], scopes: vec![HashMap::new()], sizes: vec![], ret: ret.clone(), in_loop: 0 };
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
            _ => err(e.line, e.col, format!("{what} must be a variable in stage 0")),
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
            return err(b.line, b.col, "a block cannot have an array value in stage 0");
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
                if name != "len" {
                    return err(e.line, e.col, format!("unknown method `.{name}()`; only `.len()` exists in stage 0"));
                }
                if !args.is_empty() {
                    return err(e.line, e.col, "`.len()` takes no arguments");
                }
                let id = self.local_by_expr(recv, "the receiver of `.len()`")?;
                if !self.locals[id].ty.is_arrayish() {
                    return err(e.line, e.col, format!("`.len()` on `{}`", self.locals[id].ty));
                }
                mk(ExprKind::Len(id), Ty::I64)
            }
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
                err(e.line, e.col, "an array literal can only initialise a `let` in stage 0")
            }
        }
    }
}

//! The measured tier. For a function the calculus cannot bound, build a driver that calls it on
//! inputs of size `n`, run it over a sweep of `n` under the hardware counters, and fit
//! `~n^k` to instructions (work) and L2 refills × line (moves). What is measured is written
//! into `costs.lock` marked `measured`, with the range it was measured on.

use crate::ast::BinOp;
use crate::ir::*;

/// How each parameter's size follows `n`: `n`, `n*n`, `n*4`, `n/2`, or a constant.
#[derive(Debug, Clone)]
pub enum Shape { N, NTimesN, NTimes(i64), NDiv(i64), Const(i64) }

impl Shape {
    pub fn parse(s: &str) -> Option<Shape> {
        let s = s.replace(' ', "");
        if s == "n" { return Some(Shape::N); }
        if s == "n*n" || s == "n^2" || s == "n²" { return Some(Shape::NTimesN); }
        if let Some(k) = s.strip_prefix("n*") { return k.parse().ok().map(Shape::NTimes); }
        if let Some(k) = s.strip_suffix("*n") { return k.parse().ok().map(Shape::NTimes); }
        if let Some(k) = s.strip_prefix("n/") { return k.parse().ok().map(Shape::NDiv); }
        s.parse().ok().map(Shape::Const)
    }
    pub fn at(&self, n: i64) -> i64 {
        match self { Shape::N => n, Shape::NTimesN => n * n, Shape::NTimes(k) => n * k, Shape::NDiv(k) => n / k, Shape::Const(c) => *c }
    }
}

/// A copy of the module whose `main` calls `fid` `repeat` times on inputs shaped by `shapes`
/// (by parameter index) at size `n`, and prints something derived from the results so nothing
/// is optimised away.
pub fn driver(m: &Module, fid: FuncId, n: i64, repeat: i64, shapes: &[Shape]) -> Module {
    driver_with(m, fid, n, repeat, shapes, true)
}

/// The same driver with the call replaced by a constant of the return type: what the setup and
/// the loop cost on their own, to be subtracted.
pub fn baseline(m: &Module, fid: FuncId, n: i64, repeat: i64, shapes: &[Shape]) -> Module {
    driver_with(m, fid, n, repeat, shapes, false)
}

fn driver_with(m: &Module, fid: FuncId, n: i64, repeat: i64, shapes: &[Shape], with_call: bool) -> Module {
    let f = &m.funcs[fid];
    let line = f.line;
    let mut locals: Vec<Local> = Vec::new();
    let mut stmts: Vec<Stmt> = Vec::new();
    let new_local = |locals: &mut Vec<Local>, name: &str, ty: Ty, mutable: bool| -> LocalId {
        locals.push(Local { name: name.into(), ty, mutable }); locals.len() - 1
    };
    let int = |v: i64| Expr { kind: ExprKind::Int(v), ty: Ty::I64, line };
    let mut args: Vec<Expr> = Vec::new();
    for (k, &p) in f.params.iter().enumerate() {
        let pl = &f.locals[p];
        let size = shapes.get(k).map_or(n, |s| s.at(n));
        match &pl.ty {
            Ty::Slice(elem, mutable, _) => {
                let elem = (**elem).clone();
                let arr = new_local(&mut locals, &pl.name, Ty::Array(Box::new(elem.clone()), Size::Const(size)), true);
                let zero = Expr { kind: match elem { Ty::F64 => ExprKind::Float(0.0), Ty::Bool => ExprKind::Bool(false), Ty::U8 => ExprKind::Byte(0), _ => ExprKind::Int(0) }, ty: elem.clone(), line };
                stmts.push(Stmt::LetRepeat(arr, zero, int(size)));
                // fill with a pattern so values are not all equal: i, i as f64, i % 251, i % 2 == 0
                let i = new_local(&mut locals, "i", Ty::I64, false);
                let iv = Expr { kind: ExprKind::Local(i), ty: Ty::I64, line };
                let val = match elem {
                    Ty::I64 => iv.clone(),
                    Ty::F64 => Expr { kind: ExprKind::Cast(Box::new(iv.clone()), Ty::F64), ty: Ty::F64, line },
                    Ty::U8 => Expr { kind: ExprKind::Cast(Box::new(Expr { kind: ExprKind::Binary(BinOp::Rem, Box::new(iv.clone()), Box::new(int(251))), ty: Ty::I64, line }), Ty::U8), ty: Ty::U8, line },
                    _ => Expr { kind: ExprKind::Binary(BinOp::Eq, Box::new(Expr { kind: ExprKind::Binary(BinOp::Rem, Box::new(iv.clone()), Box::new(int(2))), ty: Ty::I64, line }), Box::new(int(0))), ty: Ty::Bool, line },
                };
                stmts.push(Stmt::For { var: i, start: int(0), end: int(size), body: Block { stmts: vec![Stmt::Assign(LValue::Index(arr, iv, line), None, val)], tail: None, ty: Ty::Unit } });
                args.push(Expr { kind: ExprKind::Ref(arr, *mutable), ty: Ty::Slice(Box::new(elem), *mutable, Size::Const(size)), line });
            }
            Ty::I64 => args.push(int(size)),
            Ty::F64 => args.push(Expr { kind: ExprKind::Float(1.0), ty: Ty::F64, line }),
            Ty::U8 => args.push(Expr { kind: ExprKind::Byte(1), ty: Ty::U8, line }),
            Ty::Bool => args.push(Expr { kind: ExprKind::Bool(true), ty: Ty::Bool, line }),
            _ => {}
        }
    }
    // the repeat loop, accumulating the result into something printed
    let r = new_local(&mut locals, "r", Ty::I64, false);
    let call = if with_call { Expr { kind: ExprKind::Call(fid, args), ty: f.ret.clone(), line } } else {
        Expr { kind: match f.ret { Ty::F64 => ExprKind::Float(1.0), Ty::Bool => ExprKind::Bool(true), Ty::U8 => ExprKind::Byte(1), Ty::Unit => ExprKind::Bool(true), _ => ExprKind::Local(r) }, ty: if f.ret == Ty::Unit { Ty::Bool } else { f.ret.clone() }, line }
    };
    let ret_ty = if with_call { f.ret.clone() } else if f.ret == Ty::Unit { Ty::Bool } else { f.ret.clone() };
    let (acc_stmts, body): (Vec<Stmt>, Vec<Stmt>) = match &ret_ty {
        Ty::I64 | Ty::F64 | Ty::U8 => {
            let acc_ty = if ret_ty == Ty::F64 { Ty::F64 } else { Ty::I64 };
            let acc = new_local(&mut locals, "acc", acc_ty.clone(), true);
            let zero = Expr { kind: if acc_ty == Ty::F64 { ExprKind::Float(0.0) } else { ExprKind::Int(0) }, ty: acc_ty.clone(), line };
            let val = if ret_ty == Ty::U8 { Expr { kind: ExprKind::Cast(Box::new(call), Ty::I64), ty: Ty::I64, line } } else { call };
            (vec![Stmt::Let(acc, zero)], vec![Stmt::Assign(LValue::Var(acc), Some(BinOp::Add), val)])
        }
        Ty::Bool => {
            let acc = new_local(&mut locals, "acc", Ty::I64, true);
            let then = Block { stmts: vec![Stmt::Assign(LValue::Var(acc), Some(BinOp::Add), int(1))], tail: None, ty: Ty::Unit };
            (vec![Stmt::Let(acc, int(0))], vec![Stmt::Expr(Expr { kind: ExprKind::If(Box::new(call), then, None), ty: Ty::Unit, line })])
        }
        _ => (vec![], vec![Stmt::Expr(call)]),
    };
    stmts.extend(acc_stmts);
    stmts.push(Stmt::For { var: r, start: int(0), end: int(repeat), body: Block { stmts: body, tail: None, ty: Ty::Unit } });
    if let Some(acc) = locals.iter().position(|l| l.name == "acc") {
        let av = Expr { kind: ExprKind::Local(acc), ty: locals[acc].ty.clone(), line };
        stmts.push(Stmt::Expr(Expr { kind: ExprKind::Println(Box::new(av)), ty: Ty::Unit, line }));
    }
    let main = Func {
        name: "main".into(), params: vec![], ret: Ty::Unit, locals, sizes: vec![],
        body: Some(Block { stmts, tail: None, ty: Ty::Unit }), uses: vec![], asserts: vec![], line,
        reassigns: vec![],
    };
    let mut out = m.clone();
    match out.funcs.iter().position(|f| f.name == "main") {
        Some(i) => out.funcs[i] = main,
        None => out.funcs.push(main),
    }
    out
}

/// Least-squares slope of log y against log x.
pub fn slope(xs: &[f64], ys: &[f64]) -> f64 {
    let n = xs.len() as f64;
    if xs.len() < 2 { return f64::NAN; }
    let lx: Vec<f64> = xs.iter().map(|x| x.ln()).collect();
    let ly: Vec<f64> = ys.iter().map(|y| y.max(1.0).ln()).collect();
    let (mx, my) = (lx.iter().sum::<f64>() / n, ly.iter().sum::<f64>() / n);
    let num: f64 = lx.iter().zip(&ly).map(|(a, b)| (a - mx) * (b - my)).sum();
    let den: f64 = lx.iter().map(|a| (a - mx).powi(2)).sum();
    if den == 0.0 { f64::NAN } else { num / den }
}

/// Parse `perf stat -x,` output: the summed count of every line naming `event`.
pub fn perf_count(stderr: &str, event: &str) -> Option<u64> {
    let mut total = None;
    for line in stderr.lines() {
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() > 3 && parts[2].contains(event) {
            if let Ok(v) = parts[0].parse::<u64>() { *total.get_or_insert(0) += v; }
        }
    }
    total
}

/// Replace (or add) a function's line in a lockfile's text.
pub fn update_lock(lock: &str, name: &str, new_line: &str) -> String {
    let mut lines: Vec<String> = lock.lines().map(String::from).collect();
    let mut replaced = false;
    for l in &mut lines {
        if l.split_whitespace().next() == Some(name) { *l = new_line.to_string(); replaced = true; }
    }
    if !replaced { lines.push(new_line.to_string()); }
    let mut body: Vec<String> = lines.iter().filter(|l| !l.starts_with('#')).cloned().collect();
    body.sort();
    let mut out: Vec<String> = lines.iter().filter(|l| l.starts_with('#')).cloned().collect();
    out.extend(body);
    out.join("\n") + "\n"
}

//! A scan (docs/cost-model.md § A scan): a `while i < e` whose index only grows, by at least one
//! along every path through the body, runs at most `e − i₀` times. Two pieces live here: the walk
//! that finds the least growth of `i` over a body, and the per-function summary of what a function
//! guarantees about what it returns (`end ≥ start`), computed to a fixed point over the call graph.

use crate::ast::BinOp;
use crate::ir::*;
use super::size::Rat;
use std::collections::HashMap;

/// What a lower bound is relative to: the scanned index as it is now, a parameter of the function
/// the value comes from, or zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Base { Cur, Param(usize), Zero }

/// `result ≥ base + c`, or `result.field ≥ base + c`: one guarantee a function makes about what
/// it returns, `base` never `Cur`.
#[derive(Debug, Clone, PartialEq)]
pub struct Ge { pub field: Option<usize>, pub base: Base, pub c: Rat }

/// Lower bounds known of immutable locals: `x ≥ base + c`, `x.f ≥ base + c`.
type Facts = HashMap<LocalId, Vec<Ge>>;

struct Walk<'a> {
    f: &'a Func,
    summ: &'a [Vec<Ge>],
    /// the scanned index, or `None` in a summary's walk
    var: Option<LocalId>,
    /// a mutable local's lower bound, relative to what it was first bound to, for a summary
    facts: Facts,
    /// the program, to read the byte predicates a guard calls (§ A scan, a loop that runs once)
    m: Option<&'a Module>,
    /// immutable locals bound to `a[i]` while `i` has not moved this lap, by the array `a`
    bytes: HashMap<LocalId, LocalId>,
    /// the conditions of the `if`s the walk is inside, with the branch taken
    guards: Vec<(Expr, bool)>,
    /// whether `i` has not been assigned yet this lap, so the loop's own condition still holds
    fresh: bool,
    /// the condition of the loop whose lap is being walked
    outer: Option<Expr>,
}

impl<'a> Walk<'a> {
    /// Lower bounds of `e`: each `(base, c)` means `e ≥ base + c`.
    fn lower(&self, e: &Expr) -> Vec<(Base, Rat)> {
        match &e.kind {
            ExprKind::Int(c) => vec![(Base::Zero, Rat::int(*c as i128))],
            ExprKind::Local(l) if Some(*l) == self.var => vec![(Base::Cur, Rat::zero())],
            ExprKind::Local(l) => {
                if let Some(p) = self.f.params.iter().position(|q| q == l) {
                    if self.f.locals[*l].ty == Ty::I64 { return vec![(Base::Param(p), Rat::zero())]; }
                }
                self.facts.get(l).map(|gs| gs.iter().filter(|g| g.field.is_none()).map(|g| (g.base, g.c)).collect()).unwrap_or_default()
            }
            ExprKind::Field(inner, fi) => match &inner.kind {
                ExprKind::Local(x) => self.facts.get(x).map(|gs| gs.iter().filter(|g| g.field == Some(*fi)).map(|g| (g.base, g.c)).collect()).unwrap_or_default(),
                _ => vec![],
            },
            // a call's result, by the callee's summary with its arguments' bounds put in
            ExprKind::Call(g, args) => {
                let mut out = Vec::new();
                for s in self.summ[*g].iter().filter(|s| s.field.is_none()) {
                    match s.base {
                        Base::Zero => out.push((Base::Zero, s.c)),
                        Base::Param(p) => if let Some(a) = args.get(p) { for (b, d) in self.lower(a) { out.push((b, d.add(s.c))); } },
                        Base::Cur => {}
                    }
                }
                out
            }
            ExprKind::Binary(BinOp::Add, a, b) => match (&a.kind, &b.kind) {
                (_, ExprKind::Int(c)) => self.lower(a).into_iter().map(|(bs, d)| (bs, d.add(Rat::int(*c as i128)))).collect(),
                (ExprKind::Int(c), _) => self.lower(b).into_iter().map(|(bs, d)| (bs, d.add(Rat::int(*c as i128)))).collect(),
                _ => vec![],
            },
            ExprKind::Binary(BinOp::Sub, a, b) => match &b.kind {
                ExprKind::Int(c) => self.lower(a).into_iter().map(|(bs, d)| (bs, d.sub(Rat::int(*c as i128)))).collect(),
                _ => vec![],
            },
            _ => vec![],
        }
    }

    /// The facts a binding `let x = e` gives: `e`'s own bounds, or a callee's summary with its
    /// arguments' bounds put in.
    fn bind(&mut self, x: LocalId, e: &Expr) {
        if self.f.locals[x].mutable { return; }
        let mut out: Vec<Ge> = self.lower(e).into_iter().map(|(base, c)| Ge { field: None, base, c }).collect();
        if let ExprKind::Call(g, args) = &e.kind {
            for s in self.summ[*g].iter().filter(|s| s.field.is_some()) {
                match s.base {
                    Base::Zero => out.push(s.clone()),
                    Base::Param(p) => if let Some(a) = args.get(p) {
                        for (base, d) in self.lower(a) { out.push(Ge { field: s.field, base, c: d.add(s.c) }); }
                    },
                    Base::Cur => {}
                }
            }
        }
        if !out.is_empty() { self.facts.insert(x, out); }
    }

    /// An assignment made the index something new: what was known relative to it is not.
    fn forget_cur(&mut self) {
        for gs in self.facts.values_mut() { gs.retain(|g| g.base != Base::Cur); }
        self.bytes.clear();
        self.fresh = false;
    }

    /// Whether a nested `while` with condition `c`, reached here, surely runs at least one lap:
    /// every conjunct is one of the enclosing loop's own, which still holds since `i` has not moved,
    /// or a predicate of the byte `a[i]` that the guards on the path imply, for every one of the 256
    /// values the byte can take.
    fn runs_once(&self, c: &Expr) -> bool {
        let (Some(var), Some(outer)) = (self.var, &self.outer) else { return false };
        if !self.fresh { return false; }
        let outer_cs = conjuncts(outer);
        conjuncts(c).into_iter().all(|x| {
            outer_cs.iter().any(|o| expr_eq(x, o)) && locals_fixed(self.f, x, var)
                || self.byte_implied(x)
        })
    }

    fn byte_implied(&self, pred: &Expr) -> bool {
        let (Some(var), Some(m)) = (self.var, self.m) else { return false };
        let Some(arr) = byte_array(pred, var, &self.bytes) else { return false };
        let env = |v: i64| -> HashMap<LocalId, i64> { self.bytes.iter().filter(|(_, a)| **a == arr).map(|(l, _)| (*l, v)).collect() };
        let ev = |e: &Expr, v: i64| eval(m, self.f, &env(v), e, Some((arr, var, v)), 8);
        // the premises that can be read over the byte: a taken branch's conjuncts one by one, a
        // branch not taken only when its whole condition can be (the negation of a part says nothing)
        let mut prem: Vec<(&Expr, bool)> = Vec::new();
        for (g, taken) in &self.guards {
            if *taken { for x in conjuncts(g) { if ev(x, 0).is_some() { prem.push((x, true)); } } }
            else if ev(g, 0).is_some() { prem.push((g, false)); }
        }
        (0..256).all(|v| {
            let holds = prem.iter().all(|(e, t)| ev(e, v).map(|r| (r != 0) == *t).unwrap_or(false));
            !holds || ev(pred, v) == Some(1)
        })
    }

    /// The least growth of the index along every path through `b` that falls through:
    /// `Some(Some(d))`; `Some(None)` when every path leaves; `None` when the walk cannot follow.
    /// With `each`, every single assignment must grow it (by `0` or more), as for "only increased".
    fn block(&mut self, b: &Block, each: bool) -> Option<Option<Rat>> {
        let var = self.var?;
        let saved = self.facts.clone();
        let (saved_bytes, saved_guards, saved_fresh) = (self.bytes.clone(), self.guards.len(), self.fresh);
        let mut acc = Rat::zero();
        let tail = b.tail.as_ref().map(|t| Stmt::Expr((**t).clone()));
        let all: Vec<&Stmt> = b.stmts.iter().chain(tail.iter()).collect();
        let r = (|| {
            for (k, s) in all.iter().copied().enumerate() {
                match s {
                    Stmt::Break | Stmt::Return(_) => return Some(None),
                    Stmt::Assign(LValue::Var(v), op, e) if *v == var => {
                        let delta = match op {
                            Some(BinOp::Add) => match e.kind { ExprKind::Int(c) => Rat::int(c as i128), _ => return None },
                            Some(BinOp::Sub) => match e.kind { ExprKind::Int(c) => Rat::int(-(c as i128)), _ => return None },
                            Some(_) => return None,
                            None => {
                                let cur = self.lower(e).into_iter().filter(|(bs, _)| *bs == Base::Cur).map(|(_, d)| d);
                                cur.fold(None, |m: Option<Rat>, d| Some(match m { Some(m) if m > d => m, _ => d }))?
                            }
                        };
                        if each && delta < Rat::zero() { return None; }
                        acc = acc.add(delta);
                        self.forget_cur();
                    }
                    Stmt::Let(x, e) => {
                        if mentions_assign(e, var) { return None; }
                        self.bind(*x, e);
                        // `let c = a[i]` while `i` has not moved: `c` is the byte the lap is at
                        if !self.f.locals[*x].mutable && self.fresh {
                            if let ExprKind::Index(a, idx) = &e.kind {
                                if matches!(idx.kind, ExprKind::Local(l) if l == var) { self.bytes.insert(*x, *a); }
                            }
                        }
                        // `let mut j = e` that the rest of the block only ever increases: at least `e`
                        if self.f.locals[*x].mutable && self.f.locals[*x].ty == Ty::I64 && all[k + 1..].iter().all(|t| only_grows(t, *x)) {
                            let out: Vec<Ge> = self.lower(e).into_iter().map(|(base, c)| Ge { field: None, base, c }).collect();
                            if !out.is_empty() { self.facts.insert(*x, out); }
                        }
                    }
                    Stmt::Expr(Expr { kind: ExprKind::If(c, t, e), .. }) => {
                        if mentions_assign(c, var) { return None; }
                        self.guards.push(((**c).clone(), true));
                        let bt = self.block(t, each);
                        self.guards.pop();
                        let bt = bt?;
                        self.guards.push(((**c).clone(), false));
                        let be = match e { Some(e) => self.block(e, each), None => Some(Some(Rat::zero())) };
                        self.guards.pop();
                        let be = be?;
                        match (bt, be) {
                            (None, None) => return Some(None),
                            (Some(a), None) | (None, Some(a)) => acc = acc.add(a),
                            (Some(a), Some(b)) => acc = acc.add(if a < b { a } else { b }),
                        }
                        // a branch that assigned the index made what was known relative to it stale
                        if assigns_var(t, var) || e.as_ref().is_some_and(|e| assigns_var(e, var)) { self.forget_cur(); }
                    }
                    Stmt::For { body, .. } | Stmt::While { body, .. } => {
                        // a nested loop may run zero times: it helps only if it never hurts —
                        // unless it surely runs once, and then it grows `i` by one lap's growth
                        let once = match s { Stmt::While { cond, .. } => self.runs_once(cond), _ => false };
                        let (ob, oo, of) = (std::mem::take(&mut self.bytes), self.outer.clone(), self.fresh);
                        if let Stmt::While { cond, .. } = s { self.outer = Some(cond.clone()); }
                        self.fresh = true;
                        let inner = self.block(body, each);
                        (self.bytes, self.outer, self.fresh) = (ob, oo, of);
                        match inner {
                            Some(Some(d)) if d >= Rat::zero() => { if once { acc = acc.add(d); } }
                            Some(None) => {}
                            _ => return None,
                        }
                        if assigns_var(body, var) { self.forget_cur(); }
                    }
                    Stmt::Expr(e) | Stmt::LetRepeat(_, e, _) => { if mentions_assign(e, var) { return None; } }
                    Stmt::LetBuild { body, .. } => { if assigns_var(body, var) { return None; } }
                    Stmt::Assign(..) | Stmt::LetArray(..) | Stmt::Reassign(..) | Stmt::ParFor { .. } => {}
                }
            }
            Some(Some(acc))
        })();
        self.facts = saved;
        (self.bytes, self.fresh) = (saved_bytes, saved_fresh);
        self.guards.truncate(saved_guards);
        r
    }
}

/// Whether `e` contains a block that assigns `v` (an `if` or block expression).
fn mentions_assign(e: &Expr, v: LocalId) -> bool {
    match &e.kind {
        ExprKind::If(c, t, els) => mentions_assign(c, v) || assigns_var(t, v) || els.as_ref().is_some_and(|b| assigns_var(b, v)),
        ExprKind::Block(b) => assigns_var(b, v),
        _ => false,
    }
}

fn assigns_var(b: &Block, v: LocalId) -> bool {
    b.stmts.iter().any(|s| match s {
        Stmt::Assign(LValue::Var(w), _, e) => *w == v || mentions_assign(e, v),
        Stmt::Assign(_, _, e) | Stmt::Let(_, e) | Stmt::Expr(e) | Stmt::LetRepeat(_, e, _) => mentions_assign(e, v),
        Stmt::For { body, .. } | Stmt::While { body, .. } | Stmt::LetBuild { body, .. } | Stmt::ParFor { body, .. } => assigns_var(body, v),
        Stmt::Return(Some(e)) => mentions_assign(e, v),
        _ => false,
    }) || b.tail.as_ref().is_some_and(|t| mentions_assign(t, v))
}

fn has_return(b: &Block) -> bool {
    fn e_has(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::If(_, t, els) => has_return(t) || els.as_ref().is_some_and(has_return),
            ExprKind::Block(b) => has_return(b),
            _ => false,
        }
    }
    b.stmts.iter().any(|s| match s {
        Stmt::Return(_) => true,
        Stmt::For { body, .. } | Stmt::While { body, .. } | Stmt::LetBuild { body, .. } | Stmt::ParFor { body, .. } => has_return(body),
        Stmt::Expr(e) | Stmt::Let(_, e) | Stmt::Assign(_, _, e) => e_has(e),
        _ => false,
    }) || b.tail.as_ref().is_some_and(|t| e_has(t))
}

/// The least growth of `var` along every path through `body` (`None`: cannot follow; a path
/// that leaves is not counted), for the trip of a scan.
pub fn least_growth(f: &Func, summ: &[Vec<Ge>], body: &Block, var: LocalId) -> Option<Option<Rat>> {
    Walk { f, summ, var: Some(var), facts: HashMap::new(), m: None, bytes: HashMap::new(), guards: vec![], fresh: true, outer: None }.block(body, false)
}

/// `least_growth` for the lap of `while cond`, knowing the program: a nested `while` the path
/// surely enters counts one lap of its own growth (docs/cost-model.md § A scan, a loop that runs once).
pub fn least_growth_in(m: &Module, f: &Func, summ: &[Vec<Ge>], cond: &Expr, body: &Block, var: LocalId) -> Option<Option<Rat>> {
    Walk { f, summ, var: Some(var), facts: HashMap::new(), m: Some(m), bytes: HashMap::new(), guards: vec![], fresh: true, outer: Some(cond.clone()) }.block(body, false)
}

/// Whether every assignment to `var` anywhere in the function grows it, and the lower bounds of
/// what it was first bound to: then `var ≥` each of them wherever it is read.
pub fn only_increased(f: &Func, summ: &[Vec<Ge>], var: LocalId) -> Option<Vec<(Base, Rat)>> {
    let body = f.body.as_ref()?;
    // the one binding, at the top level of the body; one nested inside a block (a local of a
    // loop's lap, `let mut j = start`) is at least its initialiser's least when every assignment
    // to it anywhere in the function only grows it
    let Some((k, init)) = body.stmts.iter().enumerate().find_map(|(k, s)| match s { Stmt::Let(x, e) if *x == var => Some((k, e)), _ => None }) else {
        let init = binding(f, var)?;
        if !body.stmts.iter().all(|s| only_grows(s, var)) || !body.tail.as_ref().is_none_or(|t| !mentions_assign(t, var)) { return None; }
        let lb: Vec<(Base, Rat)> = least_in(f, summ, &init, 6).into_iter().filter(|(b, _)| *b != Base::Cur).collect();
        return if lb.is_empty() { None } else { Some(lb) };
    };
    let mut w = Walk { f, summ, var: Some(var), facts: HashMap::new(), m: None, bytes: HashMap::new(), guards: vec![], fresh: false, outer: None };
    for s in &body.stmts[..k] { if let Stmt::Let(x, e) = s { w.bind(*x, e); } }
    let init_lb: Vec<(Base, Rat)> = w.lower(init).into_iter().filter(|(b, _)| *b != Base::Cur).collect();
    let rest = Block { stmts: body.stmts[k + 1..].to_vec(), tail: body.tail.clone(), ty: body.ty.clone() };
    if assigns_var(&Block { stmts: body.stmts[..k].to_vec(), tail: None, ty: Ty::Unit }, var) { return None; }
    w.block(&rest, true)?;
    Some(init_lb)
}

/// Every function's guarantees about what it returns, to a fixed point over the call graph.
pub fn summaries(m: &Module) -> Vec<Vec<Ge>> {
    let mut summ: Vec<Vec<Ge>> = vec![Vec::new(); m.funcs.len()];
    // a function that returns its own result plus one (`f(a) = f(a) + 1`, which never returns)
    // would grow its constant every round: past a cap, whatever still changes says nothing,
    // which is always sound
    let mut frozen = vec![false; m.funcs.len()];
    for round in 0.. {
        let mut changed = false;
        for (fid, f) in m.funcs.iter().enumerate() {
            if frozen[fid] { continue; }
            let next = summarise(f, &summ);
            if next != summ[fid] {
                if round >= 32 { summ[fid] = Vec::new(); frozen[fid] = true; } else { summ[fid] = next; }
                changed = true;
            }
        }
        if !changed { break; }
    }
    summ
}

fn summarise(f: &Func, summ: &[Vec<Ge>]) -> Vec<Ge> {
    let Some(body) = &f.body else { return vec![] };
    if has_return(body) { return vec![]; }
    let Some(tail) = &body.tail else { return vec![] };
    // the immutable bindings of the top level, then the returned value's bounds
    let mut w = Walk { f, summ, var: None, facts: HashMap::new(), m: None, bytes: HashMap::new(), guards: vec![], fresh: false, outer: None };
    for s in &body.stmts { if let Stmt::Let(x, e) = s { w.bind(*x, e); } }
    let value = |e: &Expr, w: &Walk| -> Vec<(Base, Rat)> {
        match &e.kind {
            ExprKind::Local(l) if f.locals[*l].mutable && f.locals[*l].ty == Ty::I64 => only_increased(f, summ, *l).unwrap_or_default(),
            ExprKind::Binary(BinOp::Add | BinOp::Sub, a, c) if matches!(c.kind, ExprKind::Int(_)) && matches!(&a.kind, ExprKind::Local(l) if f.locals[*l].mutable) => {
                let ExprKind::Int(c) = c.kind else { unreachable!() };
                let c = if matches!(e.kind, ExprKind::Binary(BinOp::Sub, ..)) { -(c as i128) } else { c as i128 };
                let ExprKind::Local(l) = a.kind else { unreachable!() };
                only_increased(f, summ, l).unwrap_or_default().into_iter().map(|(b, d)| (b, d.add(Rat::int(c)))).collect()
            }
            _ => w.lower(e),
        }
    };
    let mut out = Vec::new();
    match &tail.kind {
        ExprKind::StructLit(_, vals) => {
            for (fi, v) in vals.iter().enumerate() {
                if v.ty != Ty::I64 { continue; }
                for (base, c) in value(v, &w) { if base != Base::Cur { out.push(Ge { field: Some(fi), base, c }); } }
            }
        }
        _ if tail.ty == Ty::I64 => {
            for (base, c) in value(tail, &w) { if base != Base::Cur { out.push(Ge { field: None, base, c }); } }
        }
        _ => {}
    }
    out
}

/// The least `e` can be anywhere it is read, as `(base, c)` with `base` a parameter or zero:
/// a constant; a mutable local only ever increased, by what it was first bound to; an immutable
/// local by its binding; a field of, or the result of, a call by the callee's summary with the
/// least of its arguments put in; `+ c` of any of these.
pub fn least(f: &Func, summ: &[Vec<Ge>], e: &Expr) -> Vec<(Base, Rat)> {
    least_in(f, summ, e, 8)
}

fn least_in(f: &Func, summ: &[Vec<Ge>], e: &Expr, depth: u32) -> Vec<(Base, Rat)> {
    if depth == 0 { return vec![]; }
    let call = |g: FuncId, args: &[Expr], field: Option<usize>| -> Vec<(Base, Rat)> {
        let mut out = Vec::new();
        for s in summ[g].iter().filter(|s| s.field == field) {
            match s.base {
                Base::Zero => out.push((Base::Zero, s.c)),
                Base::Param(p) => if let Some(a) = args.get(p) { for (b, d) in least_in(f, summ, a, depth - 1) { out.push((b, d.add(s.c))); } },
                Base::Cur => {}
            }
        }
        out
    };
    match &e.kind {
        ExprKind::Int(c) => vec![(Base::Zero, Rat::int(*c as i128))],
        ExprKind::Local(l) => {
            if let Some(p) = f.params.iter().position(|q| q == l) {
                return if f.locals[*l].ty == Ty::I64 { vec![(Base::Param(p), Rat::zero())] } else { vec![] };
            }
            if f.locals[*l].mutable { return only_increased(f, summ, *l).unwrap_or_default(); }
            match binding(f, *l) { Some(init) => least_in(f, summ, &init, depth - 1), None => vec![] }
        }
        ExprKind::Field(inner, fi) => match &inner.kind {
            ExprKind::Local(x) if !f.locals[*x].mutable => match binding(f, *x) {
                Some(Expr { kind: ExprKind::Call(g, args), .. }) => call(g, &args, Some(*fi)),
                _ => vec![],
            },
            _ => vec![],
        },
        ExprKind::Call(g, args) => call(*g, args, None),
        ExprKind::Binary(BinOp::Add, a, b) => match (&a.kind, &b.kind) {
            (_, ExprKind::Int(c)) => least_in(f, summ, a, depth).into_iter().map(|(bs, d)| (bs, d.add(Rat::int(*c as i128)))).collect(),
            (ExprKind::Int(c), _) => least_in(f, summ, b, depth).into_iter().map(|(bs, d)| (bs, d.add(Rat::int(*c as i128)))).collect(),
            _ => vec![],
        },
        _ => vec![],
    }
}

/// The expression a local is bound to by its one `let`.
fn binding(f: &Func, l: LocalId) -> Option<Expr> {
    fn in_block(b: &Block, l: LocalId) -> Option<Expr> {
        for s in &b.stmts {
            let found = match s {
                Stmt::Let(x, e) if *x == l => return Some(e.clone()),
                Stmt::For { body, .. } | Stmt::While { body, .. } | Stmt::LetBuild { body, .. } | Stmt::ParFor { body, .. } => in_block(body, l),
                Stmt::Let(_, e) | Stmt::Expr(e) | Stmt::Assign(_, _, e) => in_expr(e, l),
                _ => None,
            };
            if found.is_some() { return found; }
        }
        b.tail.as_ref().and_then(|t| in_expr(t, l))
    }
    fn in_expr(e: &Expr, l: LocalId) -> Option<Expr> {
        match &e.kind {
            ExprKind::If(_, t, els) => in_block(t, l).or_else(|| els.as_ref().and_then(|b| in_block(b, l))),
            ExprKind::Block(b) => in_block(b, l),
            _ => None,
        }
    }
    in_block(f.body.as_ref()?, l)
}

/// A function that advances an index through an array and returns where it stopped
/// (docs/cost-model.md § An amortised scan): it returns `i` (or a struct with `i` in field
/// `field`), `i` starts at its `i64` parameter `p`, and every assignment to `i` is `+= 1` made only
/// where `i < a.len()` holds, `a` its array parameter, and at most once there; every loop in it is a
/// `while` that scans `i`. Then what it returns is in `[p, max(p, a.len())]`, and each lap of each
/// loop moved `i` by at least one: the laps together are at most the distance it returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Advance { pub p: usize, pub a: usize, pub field: Option<usize> }

pub fn advance(f: &Func, summ: &[Vec<Ge>]) -> Option<Advance> {
    let body = f.body.as_ref()?;
    if has_return(body) { return None; }
    let tail = body.tail.as_ref()?;
    let scalar = |l: LocalId| f.locals[l].mutable && f.locals[l].ty == Ty::I64;
    let cands: Vec<(Option<usize>, LocalId)> = match &tail.kind {
        ExprKind::StructLit(_, vals) => vals.iter().enumerate()
            .filter_map(|(fi, v)| match v.kind { ExprKind::Local(l) if scalar(l) => Some((Some(fi), l)), _ => None }).collect(),
        ExprKind::Local(l) if scalar(*l) => vec![(None, *l)],
        _ => return None,
    };
    for (field, i) in cands {
        // `let mut i = p`, at the top level, `p` an `i64` parameter
        let Some(p) = body.stmts.iter().find_map(|s| match s {
            Stmt::Let(x, Expr { kind: ExprKind::Local(q), .. }) if *x == i => f.params.iter().position(|pp| pp == q).filter(|&k| f.locals[f.params[k]].ty == Ty::I64),
            _ => None,
        }) else { continue };
        let mut a: Option<LocalId> = None;
        if guarded(f, summ, body, false, i, &mut a).is_some() {
            if let Some(al) = a {
                if let Some(ak) = f.params.iter().position(|q| *q == al) {
                    return Some(Advance { p, a: ak, field });
                }
            }
        }
    }
    None
}

/// `i < a.len()` as the whole condition or its first conjunct, fixing `a` on first sight.
fn guard_of(c: &Expr, i: LocalId, a: &mut Option<LocalId>) -> bool {
    let mut c = c;
    while let ExprKind::Binary(BinOp::And, l, _) = &c.kind { c = l; }
    match &c.kind {
        ExprKind::Binary(BinOp::Lt, l, r) => match (&l.kind, &r.kind) {
            (ExprKind::Local(v), ExprKind::Len(x)) if *v == i => match a {
                Some(y) => y == x,
                None => { *a = Some(*x); true }
            },
            _ => false,
        },
        _ => false,
    }
}

/// Whether every assignment to `i` in `b` is one `+= 1` under a guard `i < a.len()` that nothing
/// before it in the same stretch has moved past; `Some(true)` when `b` assigned `i`.
fn guarded(f: &Func, summ: &[Vec<Ge>], b: &Block, guard: bool, i: LocalId, a: &mut Option<LocalId>) -> Option<bool> {
    let mut moved = false;
    let tail = b.tail.as_ref().map(|t| Stmt::Expr((**t).clone()));
    for s in b.stmts.iter().chain(tail.iter()) {
        match s {
            Stmt::Assign(LValue::Var(v), op, e) if *v == i => {
                let one = matches!(op, Some(BinOp::Add)) && matches!(e.kind, ExprKind::Int(1));
                if !one || !guard || moved { return None; }
                moved = true;
            }
            Stmt::While { cond, body, .. } => {
                if mentions_assign(cond, i) { return None; }
                if !guard_of(cond, i, a) { return None; }
                match least_growth(f, summ, body, i) { Some(Some(d)) if d >= Rat::one() => {} _ => return None }
                if guarded(f, summ, body, true, i, a)? { moved = true; }
            }
            Stmt::For { .. } | Stmt::ParFor { .. } | Stmt::LetBuild { .. } => return None,
            Stmt::Expr(Expr { kind: ExprKind::If(c, t, e), .. }) => {
                if mentions_assign(c, i) { return None; }
                let g = guard_of(c, i, a);
                let mt = guarded(f, summ, t, (guard && !moved) || g, i, a)?;
                let me = match e { Some(e) => guarded(f, summ, e, guard && !moved, i, a)?, None => false };
                if mt || me { moved = true; }
            }
            Stmt::Let(_, e) | Stmt::Expr(e) | Stmt::LetRepeat(_, e, _) | Stmt::Assign(_, _, e) => {
                if mentions_assign(e, i) || contains_loop(e) { return None; }
            }
            Stmt::Return(_) => return None,
            _ => {}
        }
    }
    Some(moved)
}

fn contains_loop(e: &Expr) -> bool {
    fn blk(b: &Block) -> bool {
        b.stmts.iter().any(|s| match s {
            Stmt::For { .. } | Stmt::While { .. } | Stmt::ParFor { .. } | Stmt::LetBuild { .. } => true,
            Stmt::Let(_, e) | Stmt::Expr(e) | Stmt::Assign(_, _, e) | Stmt::LetRepeat(_, e, _) => contains_loop(e),
            _ => false,
        }) || b.tail.as_ref().is_some_and(|t| contains_loop(t))
    }
    match &e.kind {
        ExprKind::If(_, t, els) => blk(t) || els.as_ref().is_some_and(blk),
        ExprKind::Block(b) => blk(b),
        _ => false,
    }
}

/// The conjuncts of `a && b && …`.
fn conjuncts(e: &Expr) -> Vec<&Expr> {
    match &e.kind {
        ExprKind::Binary(BinOp::And, l, r) => { let mut v = conjuncts(l); v.extend(conjuncts(r)); v }
        _ => vec![e],
    }
}

/// Structural equality of two expressions, lines aside, over the shapes a loop condition uses.
fn expr_eq(a: &Expr, b: &Expr) -> bool {
    match (&a.kind, &b.kind) {
        (ExprKind::Local(x), ExprKind::Local(y)) => x == y,
        (ExprKind::Int(x), ExprKind::Int(y)) => x == y,
        (ExprKind::Byte(x), ExprKind::Byte(y)) => x == y,
        (ExprKind::Len(x), ExprKind::Len(y)) => x == y,
        (ExprKind::Binary(o, l, r), ExprKind::Binary(p, m, n)) => o == p && expr_eq(l, m) && expr_eq(r, n),
        (ExprKind::Index(x, i), ExprKind::Index(y, j)) => x == y && expr_eq(i, j),
        _ => false,
    }
}

/// Whether every local `e` reads is immutable, or the index itself (which has not moved).
fn locals_fixed(f: &Func, e: &Expr, var: LocalId) -> bool {
    match &e.kind {
        ExprKind::Local(l) => *l == var || !f.locals[*l].mutable,
        ExprKind::Int(_) | ExprKind::Byte(_) | ExprKind::Len(_) => true,
        ExprKind::Binary(_, l, r) => locals_fixed(f, l, var) && locals_fixed(f, r, var),
        ExprKind::Index(_, i) => locals_fixed(f, i, var),
        _ => false,
    }
}

/// The array whose byte at the index a predicate reads, as `a[i]` or through a local bound to it.
fn byte_array(e: &Expr, var: LocalId, bytes: &HashMap<LocalId, LocalId>) -> Option<LocalId> {
    match &e.kind {
        ExprKind::Index(a, i) if matches!(i.kind, ExprKind::Local(l) if l == var) => Some(*a),
        ExprKind::Local(l) => bytes.get(l).copied(),
        ExprKind::Binary(_, l, r) => byte_array(l, var, bytes).or_else(|| byte_array(r, var, bytes)),
        ExprKind::Unary(_, x) | ExprKind::Cast(x, _) => byte_array(x, var, bytes),
        ExprKind::Call(_, args) => args.iter().find_map(|a| byte_array(a, var, bytes)),
        _ => None,
    }
}

/// The value of a byte predicate: literals, the locals in `env`, `a[i]` for the one array in
/// `at` (with the byte's value), comparisons, `&&`, `||`, `!`, `+`, `-`, casts, and calls to functions whose body is one
/// such expression of their parameters. `None` for anything else.
fn eval(m: &Module, f: &Func, env: &HashMap<LocalId, i64>, e: &Expr, at: Option<(LocalId, LocalId, i64)>, depth: u32) -> Option<i64> {
    let go = |x: &Expr| eval(m, f, env, x, at, depth);
    match &e.kind {
        ExprKind::Byte(b) => Some(*b as i64),
        ExprKind::Int(c) => Some(*c as i64),
        ExprKind::Bool(b) => Some(*b as i64),
        ExprKind::Local(l) => env.get(l).copied(),
        ExprKind::Index(a, i) => match (at, &i.kind) { (Some((arr, var, v)), ExprKind::Local(l)) if *a == arr && *l == var => Some(v), _ => None },
        ExprKind::Cast(x, _) => go(x),
        ExprKind::Unary(crate::ast::UnOp::Not, x) => go(x).map(|v| (v == 0) as i64),
        ExprKind::Binary(op, l, r) => {
            if matches!(op, BinOp::And) { let a = go(l)?; return if a == 0 { Some(0) } else { go(r).map(|b| (b != 0) as i64) }; }
            if matches!(op, BinOp::Or) { let a = go(l)?; return if a != 0 { Some(1) } else { go(r).map(|b| (b != 0) as i64) }; }
            let (a, b) = (go(l)?, go(r)?);
            Some(match op {
                BinOp::Eq => (a == b) as i64, BinOp::Ne => (a != b) as i64,
                BinOp::Lt => (a < b) as i64, BinOp::Le => (a <= b) as i64,
                BinOp::Gt => (a > b) as i64, BinOp::Ge => (a >= b) as i64,
                BinOp::Add => a.checked_add(b)?, BinOp::Sub => a.checked_sub(b)?,
                _ => return None,
            })
        }
        ExprKind::Call(g, args) if depth > 0 => {
            let gf = &m.funcs[*g];
            let body = gf.body.as_ref()?;
            if !body.stmts.is_empty() || args.len() != gf.params.len() { return None; }
            let mut genv = HashMap::new();
            for (p, a) in gf.params.iter().zip(args) { genv.insert(*p, go(a)?); }
            eval(m, gf, &genv, body.tail.as_ref()?, None, depth - 1)
        }
        _ => None,
    }
}

/// Whether a statement leaves `x` no smaller: no assignment to it but `x += c` with `c ≥ 0`,
/// nested blocks included.
fn only_grows(s: &Stmt, x: LocalId) -> bool {
    fn blk(b: &Block, x: LocalId) -> bool {
        b.stmts.iter().all(|s| only_grows(s, x)) && b.tail.as_ref().is_none_or(|t| ex(t, x))
    }
    fn ex(e: &Expr, x: LocalId) -> bool {
        match &e.kind {
            ExprKind::If(c, t, els) => ex(c, x) && blk(t, x) && els.as_ref().is_none_or(|b| blk(b, x)),
            ExprKind::Block(b) => blk(b, x),
            _ => true,
        }
    }
    match s {
        Stmt::Assign(LValue::Var(v), op, e) if *v == x => matches!(op, Some(BinOp::Add)) && matches!(e.kind, ExprKind::Int(c) if c >= 0),
        Stmt::Assign(_, _, e) | Stmt::Let(_, e) | Stmt::Expr(e) | Stmt::LetRepeat(_, e, _) | Stmt::Return(Some(e)) => ex(e, x),
        Stmt::For { body, .. } | Stmt::While { body, .. } | Stmt::ParFor { body, .. } | Stmt::LetBuild { body, .. } => blk(body, x),
        _ => true,
    }
}

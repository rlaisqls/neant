//! Rewrites the gap report can offer, applied to the typed IR of one function. Each one
//! preserves what the function computes; the cost calculus is then run on the result to say
//! what it bought. They are offered, never applied silently.
//!
//! Both work on the naive product shape, which is what the catalogue recognises:
//!
//!     for i in 0..ni { for j in 0..nj { let mut acc = 0; for k in 0..nk { acc += A[..] * B[..] } C[..] = acc } }

use crate::ast::BinOp;
use crate::ir::*;

use super::bounds::as_mac;

/// The naive shape, located: the three loops, the accumulator, and the store.
struct Naive {
    /// index of the outermost loop statement in its containing block
    path: Vec<usize>,
    i: LocalId,
    j: LocalId,
    k: LocalId,
    ni: Expr,
    nj: Expr,
    nk: Expr,
    acc: LocalId,
    zero: Expr,
    c: LocalId,
    ic: Expr,
    mac: Stmt,
}

fn find_naive(b: &Block, path: &mut Vec<usize>) -> Option<Naive> {
    for (n, s) in b.stmts.iter().enumerate() {
        path.push(n);
        if let Stmt::For { var: i, start: si, end: ni, body: bi } = s {
            if bi.stmts.len() == 1 && bi.tail.is_none() && is_zero(si) {
                if let Stmt::For { var: j, start: sj, end: nj, body: bj } = &bi.stmts[0] {
                    if bj.stmts.len() == 3 && bj.tail.is_none() && is_zero(sj) {
                        if let (Stmt::Let(acc, zero), Stmt::For { var: k, start: sk, end: nk, body: bk }, Stmt::Assign(LValue::Index(c, ic, _), None, store)) =
                            (&bj.stmts[0], &bj.stmts[1], &bj.stmts[2])
                        {
                            let stores_acc = matches!(&store.kind, ExprKind::Local(l) if l == acc);
                            if stores_acc && is_zero(sk) && bk.stmts.len() == 1 && bk.tail.is_none() {
                                if let Some(m) = as_mac(&bk.stmts[0]) {
                                    let acc_ok = matches!(&bk.stmts[0], Stmt::Assign(LValue::Var(l), _, _) if l == acc);
                                    let _ = m;
                                    if acc_ok {
                                        return Some(Naive {
                                            path: path.clone(), i: *i, j: *j, k: *k,
                                            ni: ni.clone(), nj: nj.clone(), nk: nk.clone(),
                                            acc: *acc, zero: zero.clone(), c: *c, ic: ic.clone(),
                                            mac: bk.stmts[0].clone(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if let Some(n) = find_naive(bi, path) { return Some(n); }
        }
        path.pop();
    }
    None
}

fn is_zero(e: &Expr) -> bool { matches!(e.kind, ExprKind::Int(0)) }

fn int(v: i64, line: u32) -> Expr { Expr { kind: ExprKind::Int(v), ty: Ty::I64, line } }
fn local(f: &Func, l: LocalId, line: u32) -> Expr { Expr { kind: ExprKind::Local(l), ty: f.locals[l].ty.clone(), line } }
fn bin(op: BinOp, a: Expr, b: Expr) -> Expr { let line = a.line; Expr { kind: ExprKind::Binary(op, Box::new(a), Box::new(b)), ty: Ty::I64, line } }
fn minx(a: Expr, b: Expr) -> Expr { let line = a.line; Expr { kind: ExprKind::MinMax(true, Box::new(a), Box::new(b)), ty: Ty::I64, line } }

fn new_local(f: &mut Func, name: &str, ty: Ty, mutable: bool) -> LocalId {
    f.locals.push(Local { name: name.to_string(), ty, mutable });
    f.locals.len() - 1
}

/// Replace the statement at `path` in `b` by `with`.
fn replace_at(b: &mut Block, path: &[usize], with: Vec<Stmt>) {
    if path.len() == 1 {
        b.stmts.splice(path[0]..path[0] + 1, with);
        return;
    }
    if let Stmt::For { body, .. } = &mut b.stmts[path[0]] {
        replace_at(body, &path[1..], with);
    }
}

/// Tile the three loops by `t`, accumulating into `C` across the `kk` tiles. `C` is cleared first,
/// so the function still computes the product rather than adding to `C`'s old contents.
pub fn tile(f: &Func, t: i64) -> Option<Func> {
    let mut path = Vec::new();
    let nv = find_naive(f.body.as_ref()?, &mut path)?;
    let mut g = f.clone();
    let line = nv.mac_line();
    let ii = new_local(&mut g, "ii", Ty::I64, false);
    let jj = new_local(&mut g, "jj", Ty::I64, false);
    let kk = new_local(&mut g, "kk", Ty::I64, false);
    let tiles = |n: &Expr| bin(BinOp::Div, bin(BinOp::Add, n.clone(), int(t - 1, line)), int(t, line));
    let lo = |v: LocalId| bin(BinOp::Mul, local(&g, v, line), int(t, line));
    let hi = |v: LocalId, n: &Expr| minx(bin(BinOp::Add, lo(v), int(t, line)), n.clone());
    let elem = g.locals[nv.acc].ty.clone();

    // clear C
    let clear = Stmt::For {
        var: nv.i, start: int(0, line), end: nv.ni.clone(),
        body: Block { stmts: vec![Stmt::For {
            var: nv.j, start: int(0, line), end: nv.nj.clone(),
            body: Block { stmts: vec![Stmt::Assign(LValue::Index(nv.c, nv.ic.clone(), line), None, nv.zero.clone())], tail: None, ty: Ty::Unit },
        }], tail: None, ty: Ty::Unit },
    };
    // the tile body: acc = C[..]; for k in tile { mac }; C[..] = acc
    let load_c = Expr { kind: ExprKind::Index(nv.c, Box::new(nv.ic.clone())), ty: elem.clone(), line };
    let inner = Block { stmts: vec![
        Stmt::Let(nv.acc, load_c),
        Stmt::For { var: nv.k, start: lo(kk), end: hi(kk, &nv.nk), body: Block { stmts: vec![nv.mac.clone()], tail: None, ty: Ty::Unit } },
        Stmt::Assign(LValue::Index(nv.c, nv.ic.clone(), line), None, local(&g, nv.acc, line)),
    ], tail: None, ty: Ty::Unit };
    let tiled = Stmt::For { var: ii, start: int(0, line), end: tiles(&nv.ni), body: Block { stmts: vec![
        Stmt::For { var: jj, start: int(0, line), end: tiles(&nv.nj), body: Block { stmts: vec![
            Stmt::For { var: kk, start: int(0, line), end: tiles(&nv.nk), body: Block { stmts: vec![
                Stmt::For { var: nv.i, start: lo(ii), end: hi(ii, &nv.ni), body: Block { stmts: vec![
                    Stmt::For { var: nv.j, start: lo(jj), end: hi(jj, &nv.nj), body: inner },
                ], tail: None, ty: Ty::Unit } },
            ], tail: None, ty: Ty::Unit } },
        ], tail: None, ty: Ty::Unit } },
    ], tail: None, ty: Ty::Unit } };
    replace_at(g.body.as_mut().unwrap(), &nv.path, vec![clear, tiled]);
    Some(g)
}

/// Transpose the operand that is walked down a column: the one whose index has the innermost
/// loop variable with a coefficient other than one. A transposed copy is built before the nest
/// and the inner loop reads it along a row.
pub fn transpose(f: &Func) -> Option<Func> {
    let mut path = Vec::new();
    let nv = find_naive(f.body.as_ref()?, &mut path)?;
    let mac = as_mac(&nv.mac)?;
    let line = mac.line;
    // which operand's index is `k·<coeff≠1> + j·1`? Coefficient of k is the row length.
    let (arr, idx) = if strided_in(mac.ib, nv.k) { (mac.b, mac.ib) } else if strided_in(mac.ia, nv.k) { (mac.a, mac.ia) } else { return None };
    let mut g = f.clone();
    let elem = g.locals[arr].ty.elem()?.clone();
    let name = format!("{}_t", g.locals[arr].name);
    let size_atom = match g.locals[arr].ty.size() { Some(s) => s.clone(), None => Size::Const(-1) };
    let bt = new_local(&mut g, &name, Ty::Array(Box::new(elem.clone()), size_atom), true);
    let p = new_local(&mut g, "p", Ty::I64, false);
    let q = new_local(&mut g, "q", Ty::I64, false);
    // the original index with k := p and the other loop variable := q, as the source; the copy
    // is indexed q·nk + p
    // the other loop variable in the index: the row of the transposed copy
    let mut vars = Vec::new();
    collect_locals(idx, &mut vars);
    let other = if vars.contains(&nv.j) { nv.j } else if vars.contains(&nv.i) { nv.i } else { return None };
    let src_idx = subst_locals(idx, &[(nv.k, p), (other, q)]);
    let dst_idx = |a: LocalId, b: LocalId| bin(BinOp::Add, bin(BinOp::Mul, local(&g, a, line), nv.nk.clone()), local(&g, b, line));
    let zero = Expr { kind: if elem == Ty::F64 { ExprKind::Float(0.0) } else { ExprKind::Int(0) }, ty: elem.clone(), line };
    let nother = if other == nv.j { nv.nj.clone() } else { nv.ni.clone() };
    let build = vec![
        Stmt::LetRepeat(bt, zero, bin(BinOp::Mul, nv.nk.clone(), nother)),
        Stmt::For { var: p, start: int(0, line), end: nv.nk.clone(), body: Block { stmts: vec![
            Stmt::For { var: q, start: int(0, line), end: if other == nv.j { nv.nj.clone() } else { nv.ni.clone() }, body: Block { stmts: vec![
                Stmt::Assign(LValue::Index(bt, dst_idx(q, p), line), None, Expr { kind: ExprKind::Index(arr, Box::new(src_idx)), ty: elem.clone(), line }),
            ], tail: None, ty: Ty::Unit } },
        ], tail: None, ty: Ty::Unit } },
    ];
    // rewrite the mac to read bt[other·nk + k]
    let new_read = Expr { kind: ExprKind::Index(bt, Box::new(dst_idx(other, nv.k))), ty: elem.clone(), line };
    let mut new_mac = nv.mac.clone();
    if let Stmt::Assign(_, _, e) = &mut new_mac {
        if let ExprKind::Binary(_, l, r) = &mut e.kind {
            if matches!(&l.kind, ExprKind::Index(a, _) if *a == arr) { **l = new_read.clone(); }
            else { **r = new_read; }
        }
    }
    // splice: the build before the nest, the nest with its mac replaced
    let mut nest = match stmt_at(g.body.as_ref()?, &nv.path) { Some(s) => s.clone(), None => return None };
    replace_mac(&mut nest, &new_mac);
    let mut with = build;
    with.push(nest);
    replace_at(g.body.as_mut().unwrap(), &nv.path, with);
    Some(g)
}

impl Naive {
    fn mac_line(&self) -> u32 { as_mac(&self.mac).map_or(0, |m| m.line) }
}

fn strided_in(idx: &Expr, k: LocalId) -> bool {
    // `k * something` appears somewhere in the index
    match &idx.kind {
        ExprKind::Binary(BinOp::Mul, a, b) => {
            matches!(&a.kind, ExprKind::Local(l) if *l == k) || matches!(&b.kind, ExprKind::Local(l) if *l == k)
        }
        ExprKind::Binary(_, a, b) => strided_in(a, k) || strided_in(b, k),
        _ => false,
    }
}

fn collect_locals(e: &Expr, out: &mut Vec<LocalId>) {
    match &e.kind {
        ExprKind::Local(l) => { if !out.contains(l) { out.push(*l); } }
        ExprKind::Binary(_, a, b) | ExprKind::MinMax(_, a, b) => { collect_locals(a, out); collect_locals(b, out); }
        ExprKind::Unary(_, a) | ExprKind::Cast(a, _) => collect_locals(a, out),
        _ => {}
    }
}

fn subst_locals(e: &Expr, map: &[(LocalId, LocalId)]) -> Expr {
    let mut r = e.clone();
    fn go(e: &mut Expr, map: &[(LocalId, LocalId)]) {
        match &mut e.kind {
            ExprKind::Local(l) => { if let Some((_, to)) = map.iter().find(|(from, _)| from == l) { *l = *to; } }
            ExprKind::Binary(_, a, b) | ExprKind::MinMax(_, a, b) => { go(a, map); go(b, map); }
            ExprKind::Unary(_, a) | ExprKind::Cast(a, _) => go(a, map),
            _ => {}
        }
    }
    go(&mut r, map);
    r
}

fn stmt_at<'a>(b: &'a Block, path: &[usize]) -> Option<&'a Stmt> {
    let s = b.stmts.get(path[0])?;
    if path.len() == 1 { return Some(s); }
    if let Stmt::For { body, .. } = s { stmt_at(body, &path[1..]) } else { None }
}

fn replace_mac(s: &mut Stmt, with: &Stmt) {
    if as_mac(s).is_some() { *s = with.clone(); return; }
    if let Stmt::For { body, .. } = s {
        for st in &mut body.stmts { replace_mac(st, with); }
    }
}

/// The tile side for a machine: the largest power of two with three `t×t` tiles of `w`-byte
/// elements strictly inside `M`.
pub fn tile_side(m_bytes: i128, elem_bytes: i128) -> i64 {
    let mut t: i128 = 8;
    while 3 * (2 * t) * (2 * t) * elem_bytes < m_bytes { t *= 2; }
    t as i64
}

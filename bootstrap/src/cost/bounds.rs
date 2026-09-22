//! Lower bounds the compiler derives itself, without a catalogue and without a tool.
//!
//! **HBL.** For a statement inside a loop nest whose array references are, after
//! delinearisation, injective affine maps on the loop variables — so that each behaves as a
//! coordinate projection `π_D` onto the loop variables `D` it mentions — the discrete
//! Brascamp–Lieb inequality (Bennett–Carbery–Christ–Tao; Christ, Demmel, Knight, Scanlon, Yelick
//! 2013, §6 for coordinate projections) says `|V| ≤ ∏_j |π_{D_j}(V)|^{s_j}` for every finite
//! `V` whenever, for every subset `S` of the loop variables, `|S| ≤ Σ_j s_j·|S ∩ D_j|`. Cut any
//! execution into segments of `S` words moved: within a segment each array has at most `2S`
//! accessible elements, so a segment runs at most `(2S)^σ` iterations with `σ = min Σ s_j`, and
//! the `|I|` iterations need at least `|I|/(2S)^σ` segments:
//!
//! ```text
//!   Q  ≥  |I| / (2^σ · S^(σ−1))  −  S      words        (S the cache in words, no recomputation)
//!      =  |I| · 4^σ · M^(1−σ)    −  M      bytes        (8-byte words, S = M/8)
//! ```
//!
//! The product has `σ = 3/2` and gets `8·N/√M − M`: Hong–Kung with Irony–Toledo–Tiskin's
//! constant, which the old hand entry wrote down. IOLB (`iolb.rs`) improves the constant where it
//! answers; this bound needs no tool and sees through a tiled nest, which IOLB does not.
//!
//! **Footprint.** Every distinct element of a parameter array that the function reads was in slow
//! memory when it was called and crosses at least once; every distinct element it writes crosses
//! back. The sum over parameter arrays of the largest injective image among their references is a
//! lower bound from a cold cache, tight on every streaming kernel.
//!
//! Both are lower bounds on *words* touched, and words move inside lines, so they bound the
//! calculus's line traffic too.

use crate::ast::BinOp;
use crate::ir::*;

use super::piece::dominates;
use super::size::{Atom, Poly, Rat};
use super::Machine;

#[derive(Debug, Clone)]
pub struct Bound {
    pub kind: String,
    pub citation: String,
    /// The bound on moves, in bytes, over the function's atoms.
    pub moves: Poly,
    pub line: u32,
    /// The bound assumes the operands start in slow memory (a footprint bound); such a bound is
    /// handed to a caller only for arrays the caller received the same way.
    pub cold: bool,
}

/// The shape of a multiply-accumulate statement, if `s` is one — for the operand notes.
pub struct Mac<'a> {
    pub a: LocalId,
    pub ia: &'a Expr,
    pub b: LocalId,
    pub ib: &'a Expr,
    pub line: u32,
}

pub fn as_mac(s: &Stmt) -> Option<Mac<'_>> {
    let e = match s {
        Stmt::Assign(LValue::Var(_), Some(BinOp::Add), e) | Stmt::Assign(LValue::Index(_, _, _), Some(BinOp::Add), e) => e,
        _ => return None,
    };
    if let ExprKind::Binary(BinOp::Mul, l, r) = &e.kind {
        if let (ExprKind::Index(a, ia), ExprKind::Index(b, ib)) = (&l.kind, &r.kind) {
            return Some(Mac { a: *a, ia, b: *b, ib, line: e.line });
        }
    }
    None
}

/// `σ = min Σ s_j` over `s ≥ 0` with `Σ_j s_j·|S ∩ D_j| ≥ |S|` for every nonempty subset `S` of
/// the `dims` loop variables; `refs[j]` is `D_j` as a bitmask. `None` when infeasible (a loop
/// variable no reference mentions) or when the enumeration would be too large.
pub fn hbl_sigma(dims: usize, refs: &[u32]) -> Option<Rat> {
    let mut refs: Vec<u32> = refs.iter().copied().filter(|&r| r != 0).collect();
    refs.sort();
    refs.dedup();
    let k = refs.len();
    if k == 0 || dims == 0 || dims > 12 { return None; }
    // rows: for each subset S, (coefficients |S ∩ D_j|, rhs |S|); then s_j ≥ 0
    let mut rows: Vec<(Vec<i128>, i128)> = Vec::new();
    for s in 1u32..(1 << dims) {
        rows.push((refs.iter().map(|d| (s & d).count_ones() as i128).collect(), s.count_ones() as i128));
    }
    for j in 0..k {
        let mut r = vec![0i128; k];
        r[j] = 1;
        rows.push((r, 0));
    }
    let m = rows.len();
    // the optimum sits at a vertex: k tight constraints
    let combos = binomial(m, k);
    if combos > 300_000 { return None; }
    let mut best: Option<(Rat, Vec<Rat>)> = None;
    let mut idx: Vec<usize> = (0..k).collect();
    loop {
        if let Some(sol) = solve(&idx.iter().map(|&i| &rows[i]).collect::<Vec<_>>(), k) {
            let feasible = rows.iter().all(|(a, b)| {
                let lhs = a.iter().zip(&sol).fold(Rat::zero(), |acc, (c, s)| acc.add(s.mul(Rat::int(*c))));
                lhs.sub(Rat::int(*b)).n >= 0
            });
            if feasible {
                let obj = sol.iter().fold(Rat::zero(), |acc, s| acc.add(*s));
                if best.as_ref().is_none_or(|(o, _)| obj < *o) { best = Some((obj, sol)); }
            }
        }
        // next combination
        let mut i = k;
        loop {
            if i == 0 { return best.map(|(o, _)| o); }
            i -= 1;
            if idx[i] < m - k + i { break; }
        }
        idx[i] += 1;
        for j in i + 1..k { idx[j] = idx[j - 1] + 1; }
    }
}

fn binomial(n: usize, k: usize) -> usize {
    let mut r: usize = 1;
    for i in 0..k { r = r.saturating_mul(n - i) / (i + 1); if r > 1 << 40 { return usize::MAX; } }
    r
}

/// Solve the square system `a·x = b` exactly; `None` when singular.
fn solve(rows: &[&(Vec<i128>, i128)], k: usize) -> Option<Vec<Rat>> {
    let mut a: Vec<Vec<Rat>> = rows.iter().map(|(r, b)| r.iter().map(|&c| Rat::int(c)).chain(std::iter::once(Rat::int(*b))).collect()).collect();
    for col in 0..k {
        let piv = (col..k).find(|&r| !a[r][col].is_zero())?;
        a.swap(col, piv);
        let p = a[col][col];
        for c in col..=k { a[col][c] = Rat::new(a[col][c].n * p.d, a[col][c].d * p.n); }
        for r in 0..k {
            if r == col || a[r][col].is_zero() { continue; }
            let f = a[r][col];
            for c in col..=k { let v = a[r][c].sub(f.mul(a[col][c])); a[r][c] = v; }
        }
    }
    Some((0..k).map(|r| a[r][k]).collect())
}

/// `|I| · 4^σ · M^(1−σ) − M` bytes.
pub fn hbl_bound(n_iters: &Poly, sigma: Rat) -> Poly {
    let coef = if sigma.d == 1 || sigma.d == 2 {
        // 4^(p/q) = 2^(2p/q), an integer when q | 2p
        Rat::int(1i128 << (2 * sigma.n / sigma.d))
    } else {
        Rat::new((4f64.powf(sigma.to_f64()) * 10000.0).round() as i128, 10000)
    };
    n_iters.scale(coef).mul_atom_pow(Atom::M, Rat::one().sub(sigma)).sub(&Poly::atom(Atom::M))
}

/// Strongest first: a bound that asymptotically dominates another goes before it; between
/// bounds neither of which dominates, the larger at the machine's `B` and `M` with every size at
/// a million. The report's gap for a rewrite is measured against the first.
pub fn strongest_first(bounds: &mut Vec<Bound>, m: &Machine) {
    bounds.dedup_by(|a, b| a.kind == b.kind && a.moves == b.moves);
    let at = |p: &Poly| p.eval(&|a| match a { Atom::B => Some(m.b_bytes as f64), Atom::M => Some(m.m_bytes as f64), Atom::P => Some(m.p_cores as f64), Atom::Var(_) => Some(1e6), Atom::Log(_) => None }).unwrap_or(0.0);
    // a bound that is a number and not positive at this machine says nothing: `216/√M − M`
    bounds.retain(|b| b.moves.has_vars() || at(&b.moves) > 0.0);
    bounds.sort_by(|a, b| {
        let ab = dominates(&a.moves, &b.moves);
        let ba = dominates(&b.moves, &a.moves);
        if ab && !ba { return std::cmp::Ordering::Less; }
        if ba && !ab { return std::cmp::Ordering::Greater; }
        at(&b.moves).partial_cmp(&at(&a.moves)).unwrap_or(std::cmp::Ordering::Equal)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_is_three_halves() {
        // a[i][k], b[k][j], c[i][j] over (i, j, k)
        let (i, j, k) = (1u32, 2u32, 4u32);
        assert_eq!(hbl_sigma(3, &[i | k, k | j, i | j]), Some(Rat::new(3, 2)));
    }

    #[test]
    fn tiled_product_is_still_three_halves() {
        // (ii, jj, kk, i, j, k): a on {ii, i, kk, k}, b on {kk, k, jj, j}, c on {ii, i, jj, j}
        let (ii, jj, kk, i, j, k) = (1u32, 2, 4, 8, 16, 32);
        assert_eq!(hbl_sigma(6, &[ii | i | kk | k, kk | k | jj | j, ii | i | jj | j]), Some(Rat::new(3, 2)));
    }

    #[test]
    fn streams_and_repetition() {
        assert_eq!(hbl_sigma(1, &[1]), Some(Rat::one()));            // x[i]
        assert_eq!(hbl_sigma(2, &[1, 2]), Some(Rat::int(2)));        // a[i]·a[j]
        assert_eq!(hbl_sigma(2, &[1]), None);                        // a repetition loop no reference sees
    }

    #[test]
    fn bytes() {
        let n3 = Poly::var(0).pow(3);
        let b = hbl_bound(&n3, Rat::new(3, 2));
        assert_eq!(b.display(&["n".to_string()]).to_string(), "8·n³/√M − M");
    }
}

//! A cost is a set of pieces, each a polynomial under a set of conditions. Its value at an
//! input is the **maximum** over the pieces whose conditions hold there. Two things make pieces:
//! an `if`, whose branches are alternatives with no condition the calculus can state; and a fit
//! test the calculus cannot decide, whose two outcomes are alternatives under `ws·B < M` and
//! `ws·B ≥ M`. Nothing is folded: every piece that can hold is kept, and the ones whose
//! conditions contradict each other are dropped (decisions §2, §3).

use std::fmt;

use super::analyze::Machine;
use super::size::{Atom, Poly, Rat};

/// `ws·B < M` when `fits`, `ws·B ≥ M` otherwise; `ws` is a number of cache lines.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Cond {
    pub ws: Poly,
    pub fits: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub conds: Vec<Cond>,
    pub poly: Poly,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cost {
    pub pieces: Vec<Piece>,
}

/// Does `q ≥ p` for every assignment of the size variables to values ≥ 1? True when `q − p`
/// has no negative coefficient, or when every term of `p` can be matched to a term of `q` with
/// at least its exponent in every variable and a coefficient budget at least as large — `B·n²`
/// covers `B·n` because `n ≥ 1`. Undecided cases are false: sound, not complete. Sizes of zero
/// are the boundary the model does not distinguish.
pub fn dominates(q: &Poly, p: &Poly) -> bool {
    if q.sub(p).terms.values().all(|c| c.n >= 0) { return true; }
    // budgets of q's terms, consumed by the terms of p they cover
    let mut budget: Vec<(&super::size::Mono, f64)> = q.terms.iter().filter(|(_, c)| c.n > 0).map(|(m, c)| (m, c.to_f64())).collect();
    let covers = |big: &super::size::Mono, small: &super::size::Mono| -> bool {
        // for every atom in either term, `big` has at least the exponent — so a `B⁻¹` in `big`
        // that `small` lacks disqualifies it: `8n²/B` does not cover `n`
        let zero = Rat::zero();
        big.factors.keys().chain(small.factors.keys()).all(|a| big.factors.get(a).unwrap_or(&zero) >= small.factors.get(a).unwrap_or(&zero))
    };
    let mut ps: Vec<(&super::size::Mono, f64)> = p.terms.iter().map(|(m, c)| (m, c.to_f64())).collect();
    // largest terms first, so they take the budget they need
    ps.sort_by(|a, b| b.0.cmp(a.0));
    for (pm, pc) in ps {
        if pc <= 0.0 { continue; }
        let mut need = pc;
        for (qm, qb) in budget.iter_mut() {
            if *qb <= 0.0 || !covers(qm, pm) { continue; }
            let take = need.min(*qb);
            *qb -= take;
            need -= take;
            if need <= 1e-12 { break; }
        }
        if need > 1e-12 { return false; }
    }
    true
}

/// Can these conditions hold together? A `fits` on `ws₁` and a `¬fits` on `ws₂ ≤ ws₁` cannot.
pub fn feasible(conds: &[Cond]) -> bool {
    for a in conds {
        if !a.fits { continue; }
        for b in conds {
            if b.fits { continue; }
            // b.ws ≥ M/B and a.ws < M/B contradict when b.ws ≤ a.ws
            if dominates(&a.ws, &b.ws) { return false; }
        }
    }
    true
}

/// Drop conditions implied by others: `fits ws₂` follows from `fits ws₁` when `ws₂ ≤ ws₁`, and
/// `¬fits ws₁` from `¬fits ws₂` likewise.
fn simplify(mut conds: Vec<Cond>) -> Vec<Cond> {
    conds.sort();
    conds.dedup();
    let keep: Vec<bool> = conds.iter().enumerate().map(|(i, c)| {
        !conds.iter().enumerate().any(|(j, d)| {
            i != j && c.fits == d.fits && c != d && (
                (c.fits && dominates(&d.ws, &c.ws)) ||       // d fits and is bigger: c is implied
                (!c.fits && dominates(&c.ws, &d.ws))         // d does not fit and is smaller: c is implied
            )
        })
    }).collect();
    conds.into_iter().zip(keep).filter(|(_, k)| *k).map(|(c, _)| c).collect()
}

impl Cost {
    pub fn zero() -> Cost { Cost::poly(Poly::zero()) }
    pub fn poly(p: Poly) -> Cost { Cost { pieces: vec![Piece { conds: vec![], poly: p }] } }
    pub fn constant(c: i128) -> Cost { Cost::poly(Poly::constant(c)) }

    /// The one unconditional polynomial, when that is all there is.
    pub fn single(&self) -> Option<&Poly> {
        if self.pieces.len() == 1 && self.pieces[0].conds.is_empty() { Some(&self.pieces[0].poly) } else { None }
    }
    pub fn is_zero(&self) -> bool { self.pieces.iter().all(|p| p.poly.is_zero()) }

    fn from_pieces(pieces: Vec<Piece>) -> Cost {
        let mut c = Cost { pieces };
        c.prune();
        c
    }

    /// Remove infeasible pieces, duplicate pieces, and pieces another piece dominates wherever
    /// they apply (its conditions are a subset, its polynomial is at least as large).
    pub fn prune(&mut self) {
        let mut ps: Vec<Piece> = std::mem::take(&mut self.pieces)
            .into_iter()
            .filter(|p| feasible(&p.conds))
            .map(|p| Piece { conds: simplify(p.conds), poly: p.poly })
            .collect();
        ps.sort_by(|a, b| a.conds.len().cmp(&b.conds.len()).then_with(|| a.poly.cmp(&b.poly)));
        ps.dedup();
        let keep: Vec<bool> = ps.iter().enumerate().map(|(i, a)| {
            !ps.iter().enumerate().any(|(j, b)| i != j && b.conds.iter().all(|c| a.conds.contains(c)) && dominates(&b.poly, &a.poly) && (b.conds.len() < a.conds.len() || b.poly != a.poly || j < i))
        }).collect();
        self.pieces = ps.into_iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| p).collect();
        if self.pieces.is_empty() { self.pieces.push(Piece { conds: vec![], poly: Poly::zero() }); }
    }

    pub fn add(&self, o: &Cost) -> Cost {
        let mut out = Vec::new();
        for a in &self.pieces {
            for b in &o.pieces {
                let mut conds = a.conds.clone();
                conds.extend(b.conds.iter().cloned());
                if !feasible(&conds) { continue; }
                out.push(Piece { conds, poly: a.poly.add(&b.poly) });
            }
        }
        Cost::from_pieces(out)
    }
    pub fn add_poly(&self, p: &Poly) -> Cost { self.map(|q| q.add(p)) }
    pub fn max(&self, o: &Cost) -> Cost {
        let mut ps = self.pieces.clone();
        ps.extend(o.pieces.iter().cloned());
        Cost::from_pieces(ps)
    }
    pub fn map(&self, f: impl Fn(&Poly) -> Poly) -> Cost {
        Cost::from_pieces(self.pieces.iter().map(|p| Piece { conds: p.conds.clone(), poly: f(&p.poly) }).collect())
    }
    pub fn mul_poly(&self, p: &Poly) -> Cost { self.map(|q| q.mul(p)) }
    pub fn scale(&self, r: Rat) -> Cost { self.map(|q| q.scale(r)) }
    pub fn sum_over(&self, atom: usize, lo: &Poly, step: i128, trip: &Poly) -> Cost {
        self.map(|q| q.sum_over(atom, lo, step, trip))
    }
    /// Substitution reaches into the conditions too.
    pub fn subst_many(&self, map: &[(usize, Poly)]) -> Cost {
        Cost::from_pieces(self.pieces.iter().map(|p| Piece {
            conds: p.conds.iter().map(|c| Cond { ws: c.ws.subst_many(map), fits: c.fits }).collect(),
            poly: p.poly.subst_many(map),
        }).collect())
    }
    pub fn subst(&self, var: usize, by: &Poly) -> Cost { self.subst_many(&[(var, by.clone())]) }
    /// Every piece's polynomial shifted by `d` (a credit is a negative `d`), under extra conditions.
    pub fn add_under(&self, conds: &[Cond], d: &Poly) -> Cost {
        let mut out = self.pieces.clone();
        for p in &mut out {
            let mut cs = p.conds.clone();
            cs.extend(conds.iter().cloned());
            if !feasible(&cs) { continue; }
            p.conds = cs;
            p.poly = p.poly.add(d);
        }
        // pieces where the extra conditions fail keep their old value
        let neg: Vec<Cond> = conds.iter().map(|c| Cond { ws: c.ws.clone(), fits: !c.fits }).collect();
        if !conds.is_empty() {
            for p in &self.pieces {
                for n in &neg {
                    let mut cs = p.conds.clone(); cs.push(n.clone());
                    if feasible(&cs) { out.push(Piece { conds: cs, poly: p.poly.clone() }); }
                }
            }
        }
        Cost::from_pieces(out)
    }

    /// Restrict to pieces compatible with `conds` and add `conds` to them.
    pub fn under(&self, conds: &[Cond]) -> Cost {
        let mut out = Vec::new();
        for p in &self.pieces {
            if !p.conds.iter().all(|c| conds.contains(c) || feasible(&[conds, std::slice::from_ref(c)].concat())) { continue; }
            let mut cs = conds.to_vec();
            cs.extend(p.conds.iter().cloned());
            if feasible(&cs) { out.push(Piece { conds: cs, poly: p.poly.clone() }); }
        }
        Cost::from_pieces(out)
    }
    /// Feasibility at the machine's `B` and `M`: when every condition of a piece mentions one
    /// size variable, each is an interval of it — `ws(n)·B < M` is `n < n₀` for the threshold
    /// `n₀`, found by bisection since a working set has nonnegative coefficients — and a piece
    /// whose intervals do not meet on `n ≥ 1` cannot hold. The pieces' conditions stay symbolic;
    /// only their feasibility is decided here, so another machine may keep more or fewer regimes.
    /// Conditions a piece's other conditions imply are dropped the same way.
    pub fn prune_at(&mut self, m: &Machine) {
        let at = |a: Atom| match a { Atom::B => Some(m.b_bytes as f64), Atom::M => Some(m.m_bytes as f64), _ => None };
        let threshold = |c: &Cond, v: usize| -> Option<f64> {
            // smallest n ≥ 1 with ws(n)·B ≥ M; None if ws is not monotone in n or never reaches M
            if !c.ws.terms.values().all(|k| k.n >= 0) { return None; }
            let f = |n: f64| c.ws.eval(&|a| if a == Atom::Var(v) { Some(n) } else { at(a) }).map(|w| w * m.b_bytes as f64);
            let target = m.m_bytes as f64;
            if f(1.0)? >= target { return Some(1.0); }
            let (mut lo, mut hi) = (1.0f64, 2.0f64);
            while f(hi)? < target { hi *= 2.0; if hi > 1e30 { return Some(f64::INFINITY); } }
            for _ in 0..200 { let mid = (lo + hi) / 2.0; if f(mid)? >= target { hi = mid; } else { lo = mid; } }
            Some(hi.ceil())
        };
        let mut kept: Vec<Piece> = Vec::new();
        'pieces: for mut p in std::mem::take(&mut self.pieces) {
            // a condition without size variables is a fact at this machine: drop it if true,
            // drop the piece if false
            let mut conds = Vec::new();
            for c in p.conds {
                if c.ws.has_vars() { conds.push(c); continue; }
                match c.ws.eval(&at) {
                    Some(w) => { if ((w * m.b_bytes as f64) < m.m_bytes as f64) != c.fits { continue 'pieces; } }
                    None => conds.push(c),
                }
            }
            p.conds = conds;
            let vars: Vec<usize> = p.conds.iter().flat_map(|c| c.ws.vars()).collect();
            let single = vars.first().copied().filter(|v| vars.iter().all(|x| x == v));
            let Some(v) = single else { kept.push(p); continue };
            // intervals: fits → n < n₀ (n ≤ n₀−1); ¬fits → n ≥ n₀
            let mut lo = 1.0f64; let mut hi = f64::INFINITY;
            let mut bounds: Vec<(usize, f64, bool)> = Vec::new(); // (cond index, threshold, fits)
            for (i, c) in p.conds.iter().enumerate() {
                let Some(t) = threshold(c, v) else { bounds.clear(); break };
                if c.fits { hi = hi.min(t - 1.0); } else { lo = lo.max(t); }
                bounds.push((i, t, c.fits));
            }
            if bounds.len() == p.conds.len() && lo > hi { continue; } // infeasible on n ≥ 1
            // drop conditions implied by tighter ones of the same kind
            if bounds.len() == p.conds.len() && !bounds.is_empty() {
                let tight_hi = bounds.iter().filter(|b| b.2).map(|b| b.1).fold(f64::INFINITY, f64::min);
                let tight_lo = bounds.iter().filter(|b| !b.2).map(|b| b.1).fold(0.0, f64::max);
                let conds: Vec<Cond> = p.conds.iter().enumerate().filter(|(i, _)| {
                    let b = bounds.iter().find(|b| b.0 == *i).unwrap();
                    if b.2 { b.1 <= tight_hi } else { b.1 >= tight_lo }
                }).map(|(_, c)| c.clone()).collect();
                kept.push(Piece { conds, poly: p.poly });
            } else {
                kept.push(p);
            }
        }
        self.pieces = kept;
        self.prune();
    }
    pub fn has_vars(&self) -> bool { self.pieces.iter().any(|p| p.poly.has_vars()) }
    pub fn mentions(&self, atom: usize) -> bool { self.pieces.iter().any(|p| p.poly.mentions(atom) || p.conds.iter().any(|c| c.ws.mentions(atom))) }
    pub fn max_var(&self) -> Option<usize> { self.pieces.iter().filter_map(|p| p.poly.max_var()).max() }

    /// Value at a full assignment: the conditions are decided with the machine's `B` and `M`,
    /// and the applicable pieces' maximum is taken.
    pub fn eval(&self, f: &dyn Fn(Atom) -> Option<f64>, m: &Machine) -> Option<f64> {
        let mut best: Option<f64> = None;
        for p in &self.pieces {
            let holds = p.conds.iter().all(|c| match c.ws.eval(f) {
                Some(ws) => ((ws * m.b_bytes as f64) < m.m_bytes as f64) == c.fits,
                None => false,
            });
            if !holds { continue; }
            let v = p.poly.eval(f)?;
            best = Some(best.map_or(v, |b: f64| b.max(v)));
        }
        best
    }

    pub fn display<'a>(&'a self, names: &'a [String]) -> CostDisplay<'a> { CostDisplay { c: self, names } }
}

pub struct CostDisplay<'a> { c: &'a Cost, names: &'a [String] }

impl Cond {
    pub fn display(&self, names: &[String]) -> String {
        let bytes = self.ws.mul_atom_pow(Atom::B, Rat::int(1));
        format!("{} {} M", bytes.display(names), if self.fits { "<" } else { "≥" })
    }
}

impl<'a> fmt::Display for CostDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(p) = self.c.single() { return write!(f, "{}", p.display(self.names)); }
        // several unconditional alternatives are a max; conditional ones are listed with their conditions
        let uncond: Vec<&Piece> = self.c.pieces.iter().filter(|p| p.conds.is_empty()).collect();
        let cond: Vec<&Piece> = self.c.pieces.iter().filter(|p| !p.conds.is_empty()).collect();
        let mut parts: Vec<String> = Vec::new();
        if uncond.len() > 1 {
            parts.push(format!("max({})", uncond.iter().map(|p| p.poly.display(self.names).to_string()).collect::<Vec<_>>().join(", ")));
        } else if let Some(p) = uncond.first() {
            parts.push(p.poly.display(self.names).to_string());
        }
        for p in cond {
            parts.push(format!("{} if {}", p.poly.display(self.names), p.conds.iter().map(|c| c.display(self.names)).collect::<Vec<_>>().join(" and ")));
        }
        write!(f, "{}", parts.join(" | "))
    }
}

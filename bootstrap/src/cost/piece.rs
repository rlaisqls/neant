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
    if dominates_as_written(q, p) { return true; }
    // an element is at most the most its array holds and at least the least: `p` with its
    // elements widened that way is no smaller, so `q` covering that covers `p`
    p.widen_reads().is_some_and(|r| dominates_as_written(q, &r))
}

fn dominates_as_written(q: &Poly, p: &Poly) -> bool {
    // the argument is about `q − p ≥ 0`: its positive terms are the budget and its negative terms
    // what the budget must cover. Budgeting `q`'s positive terms against `p`'s alone would drop
    // `q`'s negative ones, and call `32·i − 32 ≥ 32·i`
    let d = q.sub(p);
    if d.terms.values().all(|c| c.n >= 0) || univariate_nonneg(&d) { return true; }
    let mut budget: Vec<(&super::size::Mono, f64)> = d.terms.iter().filter(|(_, c)| c.n > 0).map(|(m, c)| (m, c.to_f64())).collect();
    let covers = |big: &super::size::Mono, small: &super::size::Mono| -> bool {
        // for every atom in either term, `big` has at least the exponent — so a `B⁻¹` in `big`
        // that `small` lacks disqualifies it: `8n²/B` does not cover `n`
        let zero = Rat::zero();
        let ge = |small: &super::size::Mono| big.factors.keys().chain(small.factors.keys()).all(|a| big.factors.get(a).unwrap_or(&zero) >= small.factors.get(a).unwrap_or(&zero));
        // and the least element is at most the most: `n·max(xs[_])` covers `n·min(xs[_])`
        ge(small) || small.factors.keys().any(|a| matches!(a, Atom::Read(r) if r.least && !r.walk)) && ge(&least_as_most(small))
    };
    let mut ps: Vec<(&super::size::Mono, f64)> = d.terms.iter().filter(|(_, c)| c.n < 0).map(|(m, c)| (m, -c.to_f64())).collect();
    // largest terms first, so they take the budget they need
    ps.sort_by(|a, b| b.0.cmp(a.0));
    for (pm, pc) in ps {
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

/// `m` with every least-element read replaced by the most-element read of the same array: a term
/// no smaller than `m`, since every atom is at least one.
fn least_as_most(m: &super::size::Mono) -> super::size::Mono {
    let mut out = super::size::Mono::default();
    for (a, e) in &m.factors {
        let a = match a { Atom::Read(r) if r.least && !r.walk => Atom::Read(Box::new(super::size::Read { least: false, ..(**r).clone() })), other => other.clone() };
        let cur = out.factors.get(&a).copied().unwrap_or(Rat::zero());
        out.factors.insert(a, cur.add(*e));
    }
    out
}

/// `d ≥ 0` for every integer value ≥ 1 of its one atom, decided exactly when `d` is a polynomial in
/// one integer-valued atom — a size, a read, `B` or `M` — with whole exponents: past the Cauchy
/// bound on its roots its sign is its leading coefficient's, and below it there are finitely many
/// integers to evaluate. `8·n² − 32·n + 32`, which is `8·(n − 2)²`, is out of reach of any term
/// budget and within reach of this.
fn univariate_nonneg(d: &Poly) -> bool {
    let mut atom: Option<&Atom> = None;
    let mut coef: std::collections::BTreeMap<i128, Rat> = std::collections::BTreeMap::new();
    for (m, c) in &d.terms {
        let k = match m.factors.len() {
            0 => 0,
            1 => {
                let (a, e) = m.factors.iter().next().unwrap();
                if !matches!(a, Atom::Var(_) | Atom::Read(_) | Atom::B | Atom::M) || e.d != 1 || e.n < 0 { return false; }
                if atom.is_some_and(|x| x != a) { return false; }
                atom = Some(a);
                e.n
            }
            _ => return false,
        };
        coef.insert(k, *c);
    }
    let Some((&deg, &lead)) = coef.iter().next_back() else { return true };
    if lead.n < 0 { return false; }
    // every real root is below 1 + max |aᵢ / a_deg|
    let mut bound = 1.0f64;
    for (&k, c) in &coef { if k != deg { bound = bound.max(1.0 + (c.to_f64() / lead.to_f64()).abs()); } }
    if !(bound <= 4096.0) { return false; }
    (1..=bound.ceil() as i128).all(|x| {
        let mut v = Rat::zero();
        for (&k, c) in &coef { v = v.add(c.mul(Rat::int(x.pow(k as u32)))); }
        v.n >= 0
    })
}

/// Does `q` grow at least as fast as `p`? `q`'s positive terms budgeted against `p`'s, every
/// negative term on either side dropped — so `n·(max(xs[_]) − min(xs[_])) ⪰ n/8` though the two
/// reads may be equal. Not an inequality and never used as one: it chooses between two bounds
/// that are both sound, which is what "the walk is longer than the arena" means in the region rule.
pub fn dominates_eventually(q: &Poly, p: &Poly) -> bool {
    let pos = |x: &Poly| Poly { terms: x.terms.iter().filter(|(_, c)| c.n > 0).map(|(m, c)| (m.clone(), *c)).collect() };
    let (q, p) = (pos(q), pos(p));
    if dominates_as_written(&q, &p) { return true; }
    // as the sizes grow: every term `q − p` subtracts is outgrown by one it adds, a term with at
    // least its exponent in every atom and more in some size — `n/3 − 1` is eventually positive,
    // whatever the coefficients
    let d = q.sub(&p);
    let zero = Rat::zero();
    d.terms.iter().filter(|(_, c)| c.n < 0).all(|(pm, _)| d.terms.iter().filter(|(_, c)| c.n > 0).any(|(qm, _)| {
        let atoms = || qm.factors.keys().chain(pm.factors.keys());
        atoms().all(|a| qm.factors.get(a).unwrap_or(&zero) >= pm.factors.get(a).unwrap_or(&zero))
            && atoms().any(|a| a.is_size() && qm.factors.get(a).unwrap_or(&zero) > pm.factors.get(a).unwrap_or(&zero))
    }))
}

/// The least value `p` takes over a few thousand samples in which every atom is a whole number
/// in 1..=40 — an array field's least element at most its most, and each element between them —
/// with `B` = 64, `M` = 2 MiB, and an unknown callee's cost in 0..=1000. Not a proof of anything:
/// a diagnostic for the pieces `dominates` cannot show are non-negative, which tells a prover too
/// weak for a true inequality (least ≥ 0) from a cost that is wrong (least < 0).
pub fn sample_least(p: &Poly) -> f64 {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    let seed = Cell::new(0x9e37_79b9_7f4a_7c15u64);
    let rnd = |lo: f64, hi: f64| {
        let mut x = seed.get();
        x ^= x << 13; x ^= x >> 7; x ^= x << 17;
        seed.set(x);
        lo + ((x % 1000) as f64 / 999.0 * (hi - lo)).round()
    };
    let mut least = f64::INFINITY;
    for _ in 0..3000 {
        let seen: RefCell<HashMap<String, f64>> = RefCell::default();
        let get = |k: String, lo: f64, hi: f64| -> f64 {
            if let Some(v) = seen.borrow().get(&k) { return *v; }
            let v = rnd(lo, hi);
            seen.borrow_mut().insert(k, v);
            v
        };
        let v = p.eval(&|a| Some(match &a {
            Atom::B => 64.0,
            Atom::M => 2097152.0,
            Atom::P => 8.0,
            Atom::Read(r) if !r.walk => {
                let key = format!("{:?}{:?}", r.root, r.field);
                let most = get(format!("max{key}"), 1.0, 40.0);
                let fewest = get(format!("min{key}"), 1.0, most);
                if r.index.is_none() { if r.least { fewest } else { most } } else { get(format!("{a:?}"), fewest, most) }
            }
            Atom::Opaque(_) => get(format!("{a:?}"), 0.0, 1000.0),
            other => get(format!("{other:?}"), 1.0, 40.0),
        }));
        if let Some(v) = v { least = least.min(v); }
    }
    least
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
        // one unconditional side shifts every piece of the other by the same polynomial, which
        // changes no piece's feasibility and no piece's dominance over another: what was pruned
        // stays pruned, and only the order, which is by polynomial, needs doing again
        if let Some((c, r)) = o.single().map(|r| (self, r)).or_else(|| self.single().map(|r| (o, r))) {
            let mut ps: Vec<Piece> = c.pieces.iter().map(|p| Piece { conds: p.conds.clone(), poly: p.poly.add(r) }).collect();
            ps.sort_by(|a, b| a.conds.len().cmp(&b.conds.len()).then_with(|| a.poly.cmp(&b.poly)));
            return Cost { pieces: ps };
        }
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
    pub fn add_poly(&self, p: &Poly) -> Cost { self.add(&Cost::poly(p.clone())) }
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
    /// A condition that reads an element at the atom is decided at the most that element holds:
    /// the atom is gone once summed, and a regime cannot be chosen per iteration.
    pub fn sum_over(&self, atom: usize, lo: &Poly, step: i128, trip: &Poly) -> Cost {
        self.sum_split(atom, lo, step, trip, &mut false)
    }
    /// `sum_over`, and a fit test in the atom itself — a working set that grows or shrinks with
    /// the loop's variable, the inner loop of a triangle (`B·i < M`) — decided over every lap,
    /// since after the sum there is no lap left to decide it in (`split_conds`): at the largest the
    /// working set is, exact where that fits and a bound where it does not. `straddled` is set
    /// when a bound is made.
    pub fn sum_split(&self, atom: usize, lo: &Poly, step: i128, trip: &Poly, straddled: &mut bool) -> Cost {
        let hide = |p: &Poly| p.mentions(atom);
        let last = lo.add(&trip.sub(&Poly::constant(1)).scale(Rat::int(step)));
        let mut out = Vec::new();
        for p in &self.pieces {
            let poly = p.poly.sum_over(atom, lo, step, trip);
            let hidden: Vec<Cond> = p.conds.iter().map(|c| Cond { ws: c.ws.hide_args(&hide), fits: c.fits }).collect();
            for (conds, s) in split_conds(&hidden, atom, lo, &last, step) {
                if s { *straddled = true; }
                out.push(Piece { conds, poly: poly.clone() });
            }
        }
        Cost::from_pieces(out)
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
    pub fn has_opaque(&self) -> bool { self.pieces.iter().any(|p| p.poly.has_opaque()) }
    pub fn has_loose_read(&self) -> bool { self.pieces.iter().any(|p| p.poly.has_loose_read()) }
    pub fn opaque_callees(&self, out: &mut Vec<String>) { for p in &self.pieces { p.poly.opaque_callees(out); } }
    pub fn hide_args(&self, hide: &dyn Fn(&Poly) -> bool) -> Cost { self.map(|q| q.hide_args(hide)) }
    pub fn rename_roots(&self, f: &dyn Fn(&super::size::Root) -> super::size::Root) -> Cost {
        Cost::from_pieces(self.pieces.iter().map(|p| Piece {
            conds: p.conds.iter().map(|c| Cond { ws: c.ws.rename_roots(f), fits: c.fits }).collect(),
            poly: p.poly.rename_roots(f),
        }).collect())
    }

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

/// Which way `ws` moves as `atom` grows: `Some(true)` when it is linear in the atom with every
/// coefficient positive (the rest of each term a product of sizes and `B`, never negative),
/// `Some(false)` when every one is negative, `None` when it does not mention the atom directly,
/// is not linear in it, mixes signs, or mentions it inside another atom (a read at it, a log).
fn direction(ws: &Poly, atom: usize) -> Option<bool> {
    let v = Atom::Var(atom);
    let mut sign: Option<bool> = None;
    for (m, c) in &ws.terms {
        let direct = m.factors.get(&v).copied();
        let inside = m.factors.iter().any(|(a, _)| *a != v && a.inner().iter().any(|q| q.mentions(atom)));
        if inside { return None; }
        let Some(e) = direct else { continue };
        if e != Rat::one() { return None; }
        let up = c.n > 0;
        if sign.is_some_and(|s| s != up) { return None; }
        sign = Some(up);
    }
    sign
}

/// Conditions in a loop's variable `atom`, running `lo` to `last` by `step`, decided over all its
/// laps (`Cost::sum_split`). A working set that moves one way with the atom is largest at one end:
/// a test that fits there fits in every lap, and the piece summed over them is exact; a test that
/// does not fit there fails in some laps and maybe not in the first ones, and the piece that
/// charges no reuse, summed over every lap, is at least what those laps cost — a bound, flagged
/// in the second of the pair. One condition becomes one, so the regimes are as many as before.
/// A condition not in the atom, or not moving one way with it, is kept as it is. The result is
/// empty when the conditions cannot hold together.
pub fn split_conds(conds: &[Cond], atom: usize, lo: &Poly, last: &Poly, step: i128) -> Vec<(Vec<Cond>, bool)> {
    let mut out: Vec<Cond> = Vec::new();
    let mut bound = false;
    for c in conds {
        let d = match direction(&c.ws, atom).map(|up| up == (step > 0)) {
            None => c.clone(),
            Some(grows) => {
                let most = c.ws.subst(atom, if grows { last } else { lo });
                if !c.fits { bound = true; }
                Cond { ws: most, fits: c.fits }
            }
        };
        if !out.contains(&d) { out.push(d); }
    }
    if feasible(&out) { vec![(out, bound)] } else { vec![] }
}

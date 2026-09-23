//! Symbolic sizes and costs. A `Poly` is a sum of terms, each a rational coefficient times a
//! product of atoms raised to rational powers. Atoms are the function's size variables and the
//! two machine parameters `B` (bytes per cache line) and `M` (bytes of cache). `√M` is `M^(1/2)`,
//! `n/B` is `n·B⁻¹`. Everything the calculus produces is one of these.

use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rat {
    pub n: i128,
    pub d: i128,
}

fn gcd(a: i128, b: i128) -> i128 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 { let t = a % b; a = b; b = t; }
    a
}

/// Numeric order: `d` is always positive, so cross-multiplication decides. (A derived order
/// would have compared numerators first and put `3/2` above `2`.)
impl PartialOrd for Rat {
    fn partial_cmp(&self, o: &Rat) -> Option<std::cmp::Ordering> { Some(self.cmp(o)) }
}
impl Ord for Rat {
    fn cmp(&self, o: &Rat) -> std::cmp::Ordering { (self.n * o.d).cmp(&(o.n * self.d)) }
}

impl Rat {
    pub fn new(n: i128, d: i128) -> Rat {
        assert!(d != 0);
        let g = gcd(n, d).max(1);
        let s = if d < 0 { -1 } else { 1 };
        Rat { n: s * n / g, d: s * d / g }
    }
    pub fn int(n: i128) -> Rat { Rat { n, d: 1 } }
    pub fn zero() -> Rat { Rat::int(0) }
    pub fn one() -> Rat { Rat::int(1) }
    pub fn is_zero(self) -> bool { self.n == 0 }
    pub fn is_int(self) -> bool { self.d == 1 }
    pub fn add(self, o: Rat) -> Rat { Rat::new(self.n * o.d + o.n * self.d, self.d * o.d) }
    pub fn mul(self, o: Rat) -> Rat { Rat::new(self.n * o.n, self.d * o.d) }
    pub fn neg(self) -> Rat { Rat { n: -self.n, d: self.d } }
    pub fn sub(self, o: Rat) -> Rat { self.add(o.neg()) }
    pub fn to_f64(self) -> f64 { self.n as f64 / self.d as f64 }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Atom {
    /// A size variable, indexed into the owning function's name table.
    Var(usize),
    B,
    M,
    /// Processors a `.par()` chain divides work over; only appears in the `T ≤ work/P + span`
    /// bound, never inside `work`, `moves` or `span` themselves.
    P,
    /// `log` of a size expression, from a recurrence, an asserted bound, or a `.par()` chain's
    /// reduction depth.
    Log(Box<Poly>),
    /// What one call to a function whose cost is unknown costs (stage D): its work, or its
    /// moves, at the arguments shown. It stands in the caller's cost where the callee's would, so
    /// the rest of the caller stays exact, and it is never given a value.
    Opaque(Box<Opaque>),
}

/// `work[f](n, _)`: the cost of one call to `f`, in terms of the arguments that are size
/// expressions. An argument that is not one is `_`, and the term is then the most one call costs
/// over every value that argument takes, so two calls that differ only there are the same term.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Opaque {
    pub callee: String,
    pub moves: bool,
    pub args: Vec<Option<Poly>>,
}

impl Opaque {
    fn map_args(&self, f: &dyn Fn(&Poly) -> Option<Poly>) -> Atom {
        Atom::Opaque(Box::new(Opaque { args: self.args.iter().map(|a| a.as_ref().and_then(|p| f(p))).collect(), ..self.clone() }))
    }
}

impl Atom {
    pub fn clone_ref(&self) -> Box<Atom> { Box::new(self.clone()) }
}

/// A product of atoms with nonzero exponents. The empty product is 1.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Mono {
    pub factors: BTreeMap<Atom, Rat>,
}

impl Mono {
    fn one() -> Mono { Mono::default() }
    fn atom(a: Atom) -> Mono {
        let mut f = BTreeMap::new();
        f.insert(a, Rat::one());
        Mono { factors: f }
    }
    pub fn has_atom(&self, a: &Atom) -> bool { self.factors.contains_key(a) }
    fn mul(&self, o: &Mono) -> Mono {
        let mut f = self.factors.clone();
        for (a, e) in &o.factors {
            let ne = f.get(a).map_or(*e, |x| x.add(*e));
            if ne.is_zero() { f.remove(a); } else { f.insert(a.clone(), ne); }
        }
        Mono { factors: f }
    }
    /// Total degree in size variables, for ordering terms. `B` and `M` do not count.
    fn degree(&self) -> Rat {
        self.factors.iter().filter(|(a, _)| matches!(a, Atom::Var(_))).fold(Rat::zero(), |acc, (_, e)| acc.add(*e))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Poly {
    pub terms: BTreeMap<Mono, Rat>,
}

impl Poly {
    pub fn zero() -> Poly { Poly::default() }
    pub fn constant(c: i128) -> Poly { Poly::from_rat(Rat::int(c)) }
    pub fn from_rat(c: Rat) -> Poly {
        let mut p = Poly::zero();
        if !c.is_zero() { p.terms.insert(Mono::one(), c); }
        p
    }
    pub fn atom(a: Atom) -> Poly {
        let mut p = Poly::zero();
        p.terms.insert(Mono::atom(a), Rat::one());
        p
    }
    pub fn var(i: usize) -> Poly { Poly::atom(Atom::Var(i)) }
    pub fn is_zero(&self) -> bool { self.terms.is_empty() }

    pub fn add(&self, o: &Poly) -> Poly {
        let mut t = self.terms.clone();
        for (m, c) in &o.terms {
            let nc = t.get(m).map_or(*c, |x| x.add(*c));
            if nc.is_zero() { t.remove(m); } else { t.insert(m.clone(), nc); }
        }
        Poly { terms: t }
    }
    pub fn sub(&self, o: &Poly) -> Poly { self.add(&o.scale(Rat::int(-1))) }
    pub fn scale(&self, c: Rat) -> Poly {
        if c.is_zero() { return Poly::zero(); }
        Poly { terms: self.terms.iter().map(|(m, x)| (m.clone(), x.mul(c))).collect() }
    }
    pub fn mul(&self, o: &Poly) -> Poly {
        let mut out = Poly::zero();
        for (m1, c1) in &self.terms {
            for (m2, c2) in &o.terms {
                let mut t = Poly::zero();
                t.terms.insert(m1.mul(m2), c1.mul(*c2));
                out = out.add(&t);
            }
        }
        out
    }
    pub fn mul_atom_pow(&self, a: Atom, e: Rat) -> Poly {
        let mut m = Mono::one();
        m.factors.insert(a, e);
        let mut p = Poly::zero();
        p.terms.insert(m, Rat::one());
        self.mul(&p)
    }
    /// Integer power, for substitution.
    pub fn pow(&self, k: i128) -> Poly {
        let mut r = Poly::constant(1);
        for _ in 0..k { r = r.mul(self); }
        r
    }

    /// The terms of highest total degree in the size variables: what the cost is asymptotically.
    /// An unknown callee's term has no degree anyone knows, so it is never dropped as lower order.
    pub fn leading(&self) -> Poly {
        let opaque = |m: &Mono| m.factors.keys().any(|a| matches!(a, Atom::Opaque(_)));
        let Some(top) = self.terms.keys().filter(|m| !opaque(m)).map(|m| m.degree()).max() else {
            return Poly { terms: self.terms.iter().filter(|(m, _)| opaque(m)).map(|(m, c)| (m.clone(), *c)).collect() };
        };
        Poly { terms: self.terms.iter().filter(|(m, _)| opaque(m) || m.degree() == top).map(|(m, c)| (m.clone(), *c)).collect() }
    }
    /// `self / other` when both are single terms; the exponents subtract.
    pub fn div_mono(&self, other: &Poly) -> Option<Poly> {
        if self.terms.len() != 1 || other.terms.len() != 1 { return None; }
        let (m1, c1) = self.terms.iter().next().unwrap();
        let (m2, c2) = other.terms.iter().next().unwrap();
        if c2.is_zero() { return None; }
        let mut f = m1.factors.clone();
        for (a, e) in &m2.factors {
            let ne = f.get(a).map_or(e.neg(), |x| x.add(e.neg()));
            if ne.is_zero() { f.remove(a); } else { f.insert(a.clone(), ne); }
        }
        let mut p = Poly::zero();
        p.terms.insert(Mono { factors: f }, Rat::new(c1.n * c2.d, c1.d * c2.n));
        Some(p)
    }

    /// The value as a rational, when there are no atoms at all.
    pub fn as_const(&self) -> Option<Rat> {
        match self.terms.len() {
            0 => Some(Rat::zero()),
            1 => self.terms.iter().next().and_then(|(m, c)| if m.factors.is_empty() { Some(*c) } else { None }),
            _ => None,
        }
    }
    pub fn has_vars(&self) -> bool {
        self.terms.keys().any(|m| m.factors.keys().any(|a| matches!(a, Atom::Var(_) | Atom::Log(_) | Atom::Opaque(_))))
    }
    /// Whether some term is an unknown callee's cost.
    pub fn has_opaque(&self) -> bool {
        self.terms.keys().any(|m| m.factors.keys().any(|a| matches!(a, Atom::Opaque(_))))
    }
    /// The callees whose unknown costs this polynomial names.
    pub fn opaque_callees(&self, out: &mut Vec<String>) {
        for m in self.terms.keys() {
            for a in m.factors.keys() {
                if let Atom::Opaque(o) = a { if !out.contains(&o.callee) { out.push(o.callee.clone()); } }
            }
        }
    }
    /// The same polynomial with every unknown callee's argument that `hide` picks shown as `_`:
    /// for an argument about to lose its meaning — a variable a caller cannot name, or a loop
    /// variable being summed away.
    pub fn hide_args(&self, hide: &dyn Fn(&Poly) -> bool) -> Poly {
        if !self.has_opaque() { return self.clone(); }
        let mut out = Poly::zero();
        for (m, c) in &self.terms {
            let mut f = BTreeMap::new();
            for (a, e) in &m.factors {
                let a = match a { Atom::Opaque(o) => o.map_args(&|p| if hide(p) { None } else { Some(p.clone()) }), other => other.clone() };
                f.insert(a, *e);
            }
            let mut t = Poly::zero();
            t.terms.insert(Mono { factors: f }, *c);
            out = out.add(&t);
        }
        out
    }
    /// Evaluate with every atom given a value. `None` if some atom is missing.
    pub fn eval(&self, f: &dyn Fn(Atom) -> Option<f64>) -> Option<f64> {
        let mut total = 0.0;
        for (m, c) in &self.terms {
            let mut v = c.to_f64();
            for (a, e) in &m.factors {
                let base = match a {
                    Atom::Log(inner) => inner.eval(f)?.max(1.0).ln(),
                    other => f(other.clone())?,
                };
                v *= base.powf(e.to_f64());
            }
            total += v;
        }
        Some(total)
    }
    /// Replace several variables at once, so a substitution can mention the variables it replaces.
    pub fn subst_many(&self, map: &[(usize, Poly)]) -> Poly {
        // route through fresh temporaries: shift every replaced variable to a high index first
        let shift = 1_000_000usize;
        let mut p = self.clone();
        for (i, (v, _)) in map.iter().enumerate() { p = p.subst(*v, &Poly::var(shift + i)); }
        for (i, (_, by)) in map.iter().enumerate() { p = p.subst(shift + i, by); }
        p
    }
    /// The size variables this polynomial mentions.
    pub fn vars(&self) -> Vec<usize> {
        let mut out = Vec::new();
        for m in self.terms.keys() {
            for a in m.factors.keys() {
                match a {
                    Atom::Var(i) => { if !out.contains(i) { out.push(*i); } }
                    Atom::Opaque(o) => for p in o.args.iter().flatten() { for i in p.vars() { if !out.contains(&i) { out.push(i); } } },
                    _ => {}
                }
            }
        }
        out
    }
    /// Highest total degree over the given variables.
    pub fn degree_in(&self, vars: &[usize]) -> Rat {
        self.terms.keys().map(|m| m.factors.iter().filter(|(a, _)| matches!(a, Atom::Var(i) if vars.contains(i))).fold(Rat::zero(), |acc, (_, e)| acc.add(*e))).max().unwrap_or(Rat::zero())
    }

    /// `Σ_{k=0}^{n−1} k^j` as a polynomial in `n`: Faulhaber's formula, by the identity
    /// `n^{j+1} = Σ_{i≤j} C(j+1, i)·S_i(n)`, which gives `S_j` from the `S_i` below it. Exact
    /// rationals throughout.
    pub fn faulhaber(j: usize, n: &Poly) -> Poly {
        fn binom(n: i128, k: i128) -> i128 {
            let mut r: i128 = 1;
            for i in 0..k { r = r * (n - i) / (i + 1); }
            r
        }
        let mut s: Vec<Poly> = Vec::with_capacity(j + 1);
        for jj in 0..=j {
            // S_jj(n) = (n^{jj+1} − Σ_{i<jj} C(jj+1, i)·S_i(n)) / (jj+1)
            let mut acc = n.pow(jj as i128 + 1);
            for i in 0..jj {
                acc = acc.sub(&s[i].scale(Rat::int(binom(jj as i128 + 1, i as i128))));
            }
            s.push(acc.scale(Rat::new(1, jj as i128 + 1)));
        }
        s.pop().unwrap()
    }

    /// `Σ` of this polynomial over `atom = lo, lo+step, …` for `trip` values: substitute
    /// `atom := lo + step·k`, then sum each `k^j` with Faulhaber. Exact when every power of the
    /// atom is a nonnegative integer, which is all the calculus produces. A polynomial that does
    /// not mention the atom sums to itself times `trip`.
    pub fn sum_over(&self, atom: usize, lo: &Poly, step: i128, trip: &Poly) -> Poly {
        const K: usize = usize::MAX / 2;
        // an unknown callee's cost cannot be summed over one of its arguments: that argument
        // becomes `_`, and the term the most any iteration's call costs
        let this = self.hide_args(&|p| p.mentions(atom));
        let this = &this;
        if !this.mentions(atom) { return this.mul(trip); }
        let shifted = this.subst(atom, &lo.add(&Poly::var(K).scale(Rat::int(step))));
        let mut out = Poly::zero();
        for (m, c) in &shifted.terms {
            let mut rest = m.clone();
            let j = rest.factors.remove(&Atom::Var(K)).map_or(0, |e| e.n as usize);
            let mut t = Poly::zero();
            t.terms.insert(rest, *c);
            out = out.add(&t.mul(&Poly::faulhaber(j, trip)));
        }
        out
    }
    pub fn mentions(&self, atom: usize) -> bool {
        self.terms.keys().any(|m| m.factors.keys().any(|a| match a {
            Atom::Var(i) => *i == atom,
            Atom::Log(inner) => inner.mentions(atom),
            Atom::Opaque(o) => o.args.iter().flatten().any(|p| p.mentions(atom)),
            _ => false,
        }))
    }
    /// Largest variable index used, for allocating fresh atoms above it.
    pub fn max_var(&self) -> Option<usize> {
        self.terms.keys().flat_map(|m| m.factors.keys()).filter_map(|a| match a {
            Atom::Var(i) => Some(*i),
            Atom::Opaque(o) => o.args.iter().flatten().filter_map(|p| p.max_var()).max(),
            _ => None,
        }).max()
    }

    /// Replace one size variable by a polynomial. The variable's exponents must be
    /// nonnegative integers; a fractional exponent on a variable never arises in the calculus.
    pub fn subst(&self, var: usize, by: &Poly) -> Poly {
        let mut out = Poly::zero();
        for (m, c) in &self.terms {
            let mut rest = Mono::one();
            let mut t = Poly::zero();
            let mut var_pow: Option<Rat> = None;
            for (a, e) in &m.factors {
                match a {
                    Atom::Var(v) if *v == var => var_pow = Some(*e),
                    // a log of an expression that mentions the variable: substitute inside
                    Atom::Log(inner) => { rest.factors.insert(Atom::Log(Box::new(inner.subst(var, by))), *e); }
                    Atom::Opaque(o) => { rest.factors.insert(o.map_args(&|p| Some(p.subst(var, by))), *e); }
                    other => { rest.factors.insert(other.clone(), *e); }
                }
            }
            t.terms.insert(rest, *c);
            if let Some(e) = var_pow {
                let k = if e.is_int() && e.n >= 0 { e.n } else { 0 };
                t = t.mul(&by.pow(k));
            }
            out = out.add(&t);
        }
        out
    }

    /// The same polynomial with `B` and `M` at their machine values: the form in which two
    /// costs can be compared as functions of the sizes alone. A fractional power of `M` leaves
    /// an irrational coefficient, kept to four decimals.
    pub fn at_machine(&self, b: i128, m: i128) -> Poly {
        let mut out = Poly::zero();
        for (mono, c) in &self.terms {
            let mut coef = *c;
            let mut rest = Mono::one();
            for (a, e) in &mono.factors {
                match a {
                    Atom::B | Atom::M => {
                        let base = if *a == Atom::B { b as f64 } else { m as f64 };
                        let v = base.powf(e.to_f64());
                        coef = coef.mul(Rat::new((v * 10000.0).round() as i128, 10000));
                    }
                    other => { rest.factors.insert(other.clone(), *e); }
                }
            }
            let mut t = Poly::zero();
            t.terms.insert(rest, coef);
            out = out.add(&t);
        }
        out
    }

    /// The one term of a single-term polynomial.
    pub fn as_mono(&self) -> Option<(&Mono, Rat)> {
        if self.terms.len() != 1 { return None; }
        self.terms.iter().next().map(|(m, c)| (m, *c))
    }
    /// `1/p` for a single positive term.
    pub fn inv_mono(&self) -> Option<Poly> {
        let (m, c) = self.as_mono()?;
        if c.n <= 0 { return None; }
        let mut out = Poly::zero();
        let mut f = m.clone();
        for e in f.factors.values_mut() { *e = e.neg(); }
        out.terms.insert(f, Rat::new(c.d, c.n));
        Some(out)
    }
    /// `p^(1/k)` for a single positive term; a coefficient without an exact root is kept to
    /// four decimals.
    pub fn root_mono(&self, k: i128) -> Option<Poly> {
        let (m, c) = self.as_mono()?;
        if c.n <= 0 || k <= 0 { return None; }
        let mut f = m.clone();
        for e in f.factors.values_mut() { *e = e.mul(Rat::new(1, k)); }
        let mut out = Poly::zero();
        out.terms.insert(f, rat_pow(c, Rat::new(1, k)));
        Some(out)
    }
    /// Substitute a single-term polynomial for a variable, at any rational power of it.
    pub fn subst_pow(&self, var: usize, by: &Poly) -> Poly {
        let Some((bm, bc)) = by.as_mono() else { return self.subst(var, by) };
        let mut out = Poly::zero();
        for (m, c) in &self.terms {
            let mut rest = Mono::one();
            let mut coef = *c;
            for (a, e) in &m.factors {
                match a {
                    Atom::Var(v) if *v == var => {
                        coef = coef.mul(rat_pow(bc, *e));
                        for (ba, be) in &bm.factors {
                            let ne = rest.factors.get(ba).map_or(be.mul(*e), |x| x.add(be.mul(*e)));
                            if ne.is_zero() { rest.factors.remove(ba); } else { rest.factors.insert(ba.clone(), ne); }
                        }
                    }
                    Atom::Log(inner) => { rest.factors.insert(Atom::Log(Box::new(inner.subst_pow(var, by))), *e); }
                    Atom::Opaque(o) => { rest.factors.insert(o.map_args(&|p| Some(p.subst_pow(var, by))), *e); }
                    other => {
                        let ne = rest.factors.get(other).map_or(*e, |x| x.add(*e));
                        if ne.is_zero() { rest.factors.remove(other); } else { rest.factors.insert(other.clone(), ne); }
                    }
                }
            }
            let mut t = Poly::zero();
            t.terms.insert(rest, coef);
            out = out.add(&t);
        }
        out
    }

    pub fn display<'a>(&'a self, names: &'a [String]) -> PolyDisplay<'a> {
        PolyDisplay { p: self, names }
    }
}

pub struct PolyDisplay<'a> {
    p: &'a Poly,
    names: &'a [String],
}

fn superscript(n: i128) -> String {
    let digits = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];
    let mut s = String::new();
    if n < 0 { s.push('⁻'); }
    for ch in n.abs().to_string().chars() {
        s.push(digits[ch.to_digit(10).unwrap() as usize]);
    }
    s
}

impl<'a> fmt::Display for PolyDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.p.terms.is_empty() {
            return write!(f, "0");
        }
        let name = |a: &Atom| -> String {
            match a {
                Atom::Var(i) => self.names.get(*i).cloned().unwrap_or_else(|| format!("?{i}")),
                Atom::B => "B".into(),
                Atom::M => "M".into(),
                Atom::P => "P".into(),
                Atom::Log(inner) => {
                    let s = inner.display(self.names).to_string();
                    if inner.terms.len() == 1 && !s.contains('·') { format!("log {s}") } else { format!("log({s})") }
                }
                Atom::Opaque(o) => {
                    let which = if o.moves { "moves" } else { "work" };
                    if o.args.iter().all(|a| a.is_none()) { return format!("{which}[{}]", o.callee); }
                    let args: Vec<String> = o.args.iter().map(|a| a.as_ref().map_or("_".into(), |p| p.display(self.names).to_string())).collect();
                    format!("{which}[{}]({})", o.callee, args.join(", "))
                }
            }
        };
        let atom_str = |a: &Atom, e: Rat| -> String {
            let n = name(a);
            if e == Rat::one() { n }
            else if e == Rat::new(1, 2) { format!("√{n}") }
            else if e.is_int() { format!("{n}{}", superscript(e.n)) }
            else { format!("{n}^({}/{})", e.n, e.d) }
        };
        // highest degree first, then by monomial for determinism
        let mut terms: Vec<(&Mono, &Rat)> = self.p.terms.iter().collect();
        // highest degree first; at equal degree the positive terms before the negative, so a
        // bound reads `8·n³/√M − M` and not `−M + 8·n³/√M`
        terms.sort_by(|(m1, c1), (m2, c2)| m2.degree().cmp(&m1.degree()).then_with(|| (c2.n > 0).cmp(&(c1.n > 0))).then_with(|| m2.cmp(m1)));
        for (i, (m, c)) in terms.iter().enumerate() {
            let neg = c.n < 0;
            let c = if neg { c.neg() } else { **c };
            if i > 0 { write!(f, "{}", if neg { " − " } else { " + " })?; }
            else if neg { write!(f, "−")?; }
            let mut ordered: Vec<(&Atom, &Rat)> = m.factors.iter().collect();
            ordered.sort_by_key(|(a, _)| match a { Atom::B => 0, Atom::M => 1, Atom::P => 2, Atom::Var(i) => 3 + *i, Atom::Log(_) => 1_000_000, Atom::Opaque(_) => 2_000_000 });
            let num: Vec<String> = ordered.iter().filter(|(_, e)| e.n > 0).map(|(a, e)| atom_str(a, **e)).collect();
            let den: Vec<String> = ordered.iter().filter(|(_, e)| e.n < 0).map(|(a, e)| atom_str(a, e.neg())).collect();
            // a rational with a big denominator (a tile count, a division by a constant) reads as
            // a decimal; small ones stay exact
            let decimal = c.d > 64 || (c.d > 1 && c.n.abs() > 1_000_000);
            let mut s = String::new();
            if decimal {
                let v = c.to_f64();
                s.push_str(&if v.abs() >= 1e6 || v.abs() < 1e-3 { format!("{v:.4e}") } else { format!("{v:.4}") });
                if !num.is_empty() { s.push('·'); }
            } else if c.n != 1 || num.is_empty() {
                s.push_str(&c.n.to_string());
                if !num.is_empty() { s.push('·'); }
            }
            if !num.is_empty() { s.push_str(&num.join("·")); }
            let mut d: Vec<String> = Vec::new();
            if c.d != 1 && !decimal { d.push(c.d.to_string()); }
            d.extend(den);
            if !d.is_empty() {
                s.push('/');
                if d.len() > 1 { s.push('('); }
                s.push_str(&d.join("·"));
                if d.len() > 1 { s.push(')'); }
            }
            write!(f, "{s}")?;
        }
        Ok(())
    }
}

/// `c^e` for a positive rational base: exact when `e` is an integer or the root is exact, else
/// to four decimals.
pub fn rat_pow(c: Rat, e: Rat) -> Rat {
    if e.is_int() {
        let mut r = Rat::one();
        for _ in 0..e.n.abs() { r = r.mul(c); }
        return if e.n < 0 { Rat::new(r.d, r.n) } else { r };
    }
    let exact = |x: i128| -> Option<i128> {
        let r = (x as f64).powf(1.0 / e.d as f64).round() as i128;
        let mut p = 1i128; for _ in 0..e.d { p *= r; }
        if p == x { Some(r) } else { None }
    };
    if let (Some(n), Some(d)) = (exact(c.n.abs()), exact(c.d)) {
        return rat_pow(Rat::new(n, d), Rat::int(e.n));
    }
    Rat::new((c.to_f64().powf(e.to_f64()) * 10000.0).round() as i128, 10000)
}


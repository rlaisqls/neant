//! Symbolic sizes and costs. A `Poly` is a sum of terms, each a rational coefficient times a
//! product of atoms raised to rational powers. Atoms are the function's size variables and the
//! two machine parameters `B` (bytes per cache line) and `M` (bytes of cache). `√M` is `M^(1/2)`,
//! `n/B` is `n·B⁻¹`. Everything the calculus produces is one of these.

use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rat {
    pub n: i128,
    pub d: i128,
}

fn gcd(a: i128, b: i128) -> i128 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 { let t = a % b; a = b; b = t; }
    a
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
    pub fn to_f64(self) -> f64 { self.n as f64 / self.d as f64 }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Atom {
    /// A size variable, indexed into the owning function's name table.
    Var(usize),
    B,
    M,
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
    fn mul(&self, o: &Mono) -> Mono {
        let mut f = self.factors.clone();
        for (a, e) in &o.factors {
            let ne = f.get(a).map_or(*e, |x| x.add(*e));
            if ne.is_zero() { f.remove(a); } else { f.insert(*a, ne); }
        }
        Mono { factors: f }
    }
    /// Total degree in size variables, for ordering terms. `B` and `M` do not count.
    fn degree(&self) -> Rat {
        self.factors.iter().filter(|(a, _)| matches!(a, Atom::Var(_))).fold(Rat::zero(), |acc, (_, e)| acc.add(*e))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
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
    fn pow(&self, k: i128) -> Poly {
        let mut r = Poly::constant(1);
        for _ in 0..k { r = r.mul(self); }
        r
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
        self.terms.keys().any(|m| m.factors.keys().any(|a| matches!(a, Atom::Var(_))))
    }
    /// Evaluate with every atom given a value. `None` if some atom is missing.
    pub fn eval(&self, f: &dyn Fn(Atom) -> Option<f64>) -> Option<f64> {
        let mut total = 0.0;
        for (m, c) in &self.terms {
            let mut v = c.to_f64();
            for (a, e) in &m.factors {
                v *= f(*a)?.powf(e.to_f64());
            }
            total += v;
        }
        Some(total)
    }
    /// Replace one size variable by a polynomial. The variable's exponents must be
    /// nonnegative integers; a fractional exponent on a variable never arises in the calculus.
    pub fn subst(&self, var: usize, by: &Poly) -> Poly {
        let mut out = Poly::zero();
        for (m, c) in &self.terms {
            let mut rest = m.clone();
            let e = rest.factors.remove(&Atom::Var(var));
            let mut t = Poly::zero();
            t.terms.insert(rest, *c);
            if let Some(e) = e {
                let k = if e.is_int() && e.n >= 0 { e.n } else { 0 };
                t = t.mul(&by.pow(k));
            }
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
        terms.sort_by(|(m1, _), (m2, _)| m2.degree().cmp(&m1.degree()).then_with(|| m2.cmp(m1)));
        for (i, (m, c)) in terms.iter().enumerate() {
            let neg = c.n < 0;
            let c = if neg { c.neg() } else { **c };
            if i > 0 { write!(f, "{}", if neg { " − " } else { " + " })?; }
            else if neg { write!(f, "−")?; }
            let mut ordered: Vec<(&Atom, &Rat)> = m.factors.iter().collect();
            ordered.sort_by_key(|(a, _)| match a { Atom::B => 0, Atom::M => 1, Atom::Var(i) => 2 + *i });
            let num: Vec<String> = ordered.iter().filter(|(_, e)| e.n > 0).map(|(a, e)| atom_str(a, **e)).collect();
            let den: Vec<String> = ordered.iter().filter(|(_, e)| e.n < 0).map(|(a, e)| atom_str(a, e.neg())).collect();
            let mut s = String::new();
            if c.n != 1 || num.is_empty() { s.push_str(&c.n.to_string()); }
            if !num.is_empty() {
                if c.n != 1 { s.push('·'); }
                s.push_str(&num.join("·"));
            }
            let mut d: Vec<String> = Vec::new();
            if c.d != 1 { d.push(c.d.to_string()); }
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

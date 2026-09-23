//! `#[cost(work_at_most = "n log n", moves_at_most = "16·n + B")]`: the bound's text, parsed
//! over the function's atoms, and the asymptotic-dominance check against what was inferred.
//!
//! The text grammar: terms joined by `+`; a term is factors joined by `·`, `*`, `/` or just
//! written next to each other; a factor is a number, a size name (`n`, `xs.len()`), `B`, `M`,
//! `log <factor>`, `√<factor>` or `sqrt(<expr>)`, a parenthesised expression, and any factor may
//! carry `^k` or a superscript.

use super::size::{Atom, Poly, Rat};

pub fn parse(text: &str, names: &[String]) -> Result<Poly, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut p = P { c: &chars, i: 0, names };
    let e = p.expr()?;
    p.ws();
    if p.i != chars.len() { return Err(format!("unexpected `{}` in cost bound", chars[p.i])); }
    Ok(e)
}

struct P<'a> { c: &'a [char], i: usize, names: &'a [String] }

impl<'a> P<'a> {
    fn ws(&mut self) { while self.i < self.c.len() && self.c[self.i].is_whitespace() { self.i += 1; } }
    fn peek(&mut self) -> Option<char> { self.ws(); self.c.get(self.i).copied() }
    fn eat(&mut self, ch: char) -> bool { if self.peek() == Some(ch) { self.i += 1; true } else { false } }

    fn expr(&mut self) -> Result<Poly, String> {
        let mut acc = self.term()?;
        loop {
            if self.eat('+') { acc = acc.add(&self.term()?); }
            else if self.eat('-') || self.eat('−') { acc = acc.sub(&self.term()?); }
            else { return Ok(acc); }
        }
    }
    fn term(&mut self) -> Result<Poly, String> {
        let mut acc = self.factor()?;
        loop {
            match self.peek() {
                Some('·') | Some('*') => { self.i += 1; acc = acc.mul(&self.factor()?); }
                Some('/') => {
                    self.i += 1;
                    let d = self.factor()?;
                    // division by a single atom or constant: negate exponents
                    if let Some(c) = d.as_const() {
                        if c.is_zero() { return Err("division by zero in cost bound".into()); }
                        acc = acc.scale(Rat::new(c.d, c.n));
                    } else if d.terms.len() == 1 {
                        let (m, c) = d.terms.iter().next().unwrap();
                        let mut inv = Poly::zero();
                        let mut f = m.clone();
                        for e in f.factors.values_mut() { *e = e.neg(); }
                        inv.terms.insert(f, Rat::new(c.d, c.n));
                        acc = acc.mul(&inv);
                    } else { return Err("a cost bound can only divide by a single term".into()); }
                }
                Some(ch) if ch.is_alphanumeric() || ch == '(' || ch == '√' => acc = acc.mul(&self.factor()?),
                _ => return Ok(acc),
            }
        }
    }
    fn factor(&mut self) -> Result<Poly, String> {
        let base = self.atom()?;
        // exponent
        if self.eat('^') {
            let k = self.number()?;
            return Ok(pow(&base, k));
        }
        let sup = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];
        if let Some(ch) = self.c.get(self.i).copied() {
            if let Some(k) = sup.iter().position(|s| *s == ch) {
                self.i += 1;
                return Ok(pow(&base, k as i128));
            }
        }
        Ok(base)
    }
    fn number(&mut self) -> Result<i128, String> {
        self.ws();
        let start = self.i;
        while self.i < self.c.len() && self.c[self.i].is_ascii_digit() { self.i += 1; }
        if start == self.i { return Err("expected a number".into()); }
        self.c[start..self.i].iter().collect::<String>().parse().map_err(|_| "bad number".to_string())
    }
    fn atom(&mut self) -> Result<Poly, String> {
        match self.peek() {
            Some('(') => { self.i += 1; let e = self.expr()?; if !self.eat(')') { return Err("expected `)`".into()); } Ok(e) }
            Some('√') => { self.i += 1; let e = self.atom()?; Ok(root(&e)?) }
            Some(ch) if ch.is_ascii_digit() => Ok(Poly::constant(self.number()?)),
            Some(ch) if ch.is_alphabetic() || ch == '_' => {
                let start = self.i;
                while self.i < self.c.len() && (self.c[self.i].is_alphanumeric() || self.c[self.i] == '_' || self.c[self.i] == '.') { self.i += 1; }
                let mut word: String = self.c[start..self.i].iter().collect();
                if word.ends_with('.') { self.i -= 1; word.pop(); }
                // `xs.len()`
                if self.c.get(self.i) == Some(&'(') && word.ends_with(".len") {
                    if self.c.get(self.i + 1) == Some(&')') { self.i += 2; word.push_str("()"); }
                }
                match word.as_str() {
                    "log" => { let e = self.atom()?; Ok(Poly::atom(Atom::Log(Box::new(e)))) }
                    "sqrt" => { if !self.eat('(') { return Err("sqrt(...)".into()); } let e = self.expr()?; if !self.eat(')') { return Err("expected `)`".into()); } root(&e) }
                    "B" => Ok(Poly::atom(Atom::B)),
                    "M" => Ok(Poly::atom(Atom::M)),
                    w => match self.names.iter().position(|n| n == w) {
                        Some(i) => Ok(Poly::var(i)),
                        None => Err(format!("`{w}` is not a size of this function; its sizes are {}", self.names.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", "))),
                    },
                }
            }
            Some(ch) => Err(format!("unexpected `{ch}` in cost bound")),
            None => Err("cost bound ends early".into()),
        }
    }
}

fn pow(p: &Poly, k: i128) -> Poly {
    if p.terms.len() == 1 {
        let (m, c) = p.terms.iter().next().unwrap();
        let mut f = m.clone();
        for e in f.factors.values_mut() { *e = e.mul(Rat::int(k)); }
        let mut out = Poly::zero();
        let mut coef = Rat::one();
        for _ in 0..k { coef = coef.mul(*c); }
        out.terms.insert(f, coef);
        return out;
    }
    let mut r = Poly::constant(1);
    for _ in 0..k { r = r.mul(p); }
    r
}

fn root(p: &Poly) -> Result<Poly, String> {
    if p.terms.len() != 1 { return Err("√ of a sum is not a cost the checker can compare".into()); }
    let (m, c) = p.terms.iter().next().unwrap();
    if *c != Rat::one() { return Err("√ of a coefficient is not supported; write the bound without it".into()); }
    let mut f = m.clone();
    for e in f.factors.values_mut() { *e = e.mul(Rat::new(1, 2)); }
    let mut out = Poly::zero();
    out.terms.insert(f, Rat::one());
    Ok(out)
}

/// Is every term of `inferred` dominated by some term of `asserted`? A term dominates another
/// when, for every size variable, its exponent is at least as large, and when equal on a
/// variable, its `log` power of that variable is at least as large. `B` and `M` are constants
/// and do not count. Coefficients do not count: this is asymptotic.
pub fn dominated(inferred: &Poly, asserted: &Poly) -> bool {
    inferred.terms.keys().all(|t| asserted.terms.keys().any(|u| dominates(u, t)))
}

fn dominates(u: &super::size::Mono, t: &super::size::Mono) -> bool {
    let exp = |m: &super::size::Mono, a: &Atom| m.factors.get(a).copied().unwrap_or(Rat::zero());
    let mut vars: Vec<Atom> = Vec::new();
    for a in u.factors.keys().chain(t.factors.keys()) {
        if matches!(a, Atom::Var(_)) && !vars.contains(a) { vars.push(*a.clone_ref()); }
    }
    for v in &vars {
        let (eu, et) = (exp(u, v), exp(t, v));
        if eu < et { return false; }
        if eu == et {
            // same power: compare logs of this variable
            let lv = Atom::Log(Box::new(Poly::atom(v.clone())));
            if exp(u, &lv) < exp(t, &lv) { return false; }
        }
    }
    // an unknown callee's cost is covered only by the same term, at least as often
    for (a, e) in &t.factors {
        if matches!(a, Atom::Opaque(_)) && exp(u, a) < *e { return false; }
    }
    // a log of something the asserted side does not have at all
    for a in t.factors.keys() {
        if let Atom::Log(inner) = a {
            if !u.factors.contains_key(a) {
                // fine only if u has a positive power of the variable inside
                let ok = inner.terms.keys().any(|m| m.factors.keys().any(|iv| matches!(iv, Atom::Var(_)) && exp(u, iv) > Rat::zero() && exp(u, iv) > exp(t, iv)));
                if !ok { return false; }
            }
        }
    }
    true
}

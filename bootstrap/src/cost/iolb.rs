//! Lower bounds from IOLB (Olivry, Langou, Pouchet, Sadayappan, Rastello, 2020): the exported SCoP
//! is handed to the external tool named by `NEANT_IOLB` — a command with `{file}` in it — and the
//! second-to-last line of its output, the asymptotic bound in words as a GiNaC expression such as
//! `2*n^3*S^(-1/2)`, is parsed into the function's atoms. `S` is the cache in words, `M/8` here;
//! the result is multiplied by 8 into bytes. The hand entry in `bounds.rs` is what this replaces
//! when the tool is present, and what stands in when it is not.

use std::process::Command;

use super::size::{Atom, Poly, Rat};

pub fn command() -> Option<String> { std::env::var("NEANT_IOLB").ok().filter(|s| !s.is_empty()) }

/// Run the tool on an exported SCoP; returns the asymptotic bound in **bytes** over `names`.
pub fn bound(scop_c: &str, names: &[String], workdir: &std::path::Path) -> Result<Poly, String> {
    let cmd = command().ok_or("NEANT_IOLB is not set")?;
    let file = workdir.join("scop.c");
    std::fs::write(&file, scop_c).map_err(|e| e.to_string())?;
    let full = cmd.replace("{file}", &file.to_string_lossy());
    let out = Command::new("sh").arg("-c").arg(&full).output().map_err(|e| format!("could not run `{full}`: {e}"))?;
    if !out.status.success() {
        return Err(match out.status.code() { Some(137) | None => "IOLB did not finish within IOLB_TIMEOUT".to_string(), Some(c) => format!("IOLB exited with {c}") });
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 3 {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("IOLB gave no bound: {}", err.lines().last().unwrap_or("(no output)")));
    }
    let asymptotic = lines[lines.len() - 2];
    Ok(to_bytes(&parse(asymptotic, names)?))
}

/// IOLB counts words in a cache of `S` words; the calculus counts bytes in `M` bytes. With 8-byte
/// words `S = M/8`, so `S^e = 8^(-e)·M^e`, and the count itself is ×8. `8^(1/2)` is irrational;
/// the coefficient is kept to four decimals.
fn to_bytes(words: &Poly) -> Poly {
    let mut out = Poly::zero();
    for (m, c) in &words.terms {
        let e = m.factors.get(&Atom::M).copied().unwrap_or(Rat::zero());
        let f = 8f64 * 8f64.powf(-e.to_f64());
        let coef = c.mul(Rat::new((f * 10000.0).round() as i128, 10000));
        let mut t = Poly::zero();
        t.terms.insert(m.clone(), coef);
        out = out.add(&t);
    }
    out
}

/// GiNaC's text: products with `*`, powers with `^` (integer or `(p/q)`), sums, parentheses.
fn parse(text: &str, names: &[String]) -> Result<Poly, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut p = P { c: &chars, i: 0, names };
    let e = p.expr()?;
    p.ws();
    if p.i != chars.len() { return Err(format!("unexpected `{}` in IOLB output `{text}`", chars[p.i])); }
    Ok(e)
}

struct P<'a> { c: &'a [char], i: usize, names: &'a [String] }

impl<'a> P<'a> {
    fn ws(&mut self) { while self.i < self.c.len() && self.c[self.i].is_whitespace() { self.i += 1; } }
    fn peek(&mut self) -> Option<char> { self.ws(); self.c.get(self.i).copied() }
    fn eat(&mut self, ch: char) -> bool { if self.peek() == Some(ch) { self.i += 1; true } else { false } }
    fn expr(&mut self) -> Result<Poly, String> {
        let mut acc = if self.eat('-') { self.term()?.scale(Rat::int(-1)) } else { self.term()? };
        loop {
            if self.eat('+') { acc = acc.add(&self.term()?); }
            else if self.eat('-') { acc = acc.sub(&self.term()?); }
            else { return Ok(acc); }
        }
    }
    fn term(&mut self) -> Result<Poly, String> {
        let mut acc = self.power()?;
        loop {
            if self.eat('*') { acc = acc.mul(&self.power()?); }
            else if self.eat('/') {
                let d = self.power()?;
                let c = d.as_const().ok_or("division by a non-constant in IOLB output")?;
                acc = acc.scale(Rat::new(c.d, c.n));
            } else { return Ok(acc); }
        }
    }
    fn power(&mut self) -> Result<Poly, String> {
        let base = self.atom()?;
        if !self.eat('^') { return Ok(base); }
        // exponent: an integer, or (p/q) possibly negative
        let paren = self.eat('(');
        let neg = self.eat('-');
        let num = self.number()?;
        let den = if self.eat('/') { self.number()? } else { 1 };
        if paren && !self.eat(')') { return Err("expected `)` in exponent".into()); }
        let e = Rat::new(if neg { -num } else { num }, den);
        // the base must be a single term to take a rational power
        if base.terms.len() != 1 { return Err("a power of a sum in IOLB output".into()); }
        let (m, c) = base.terms.iter().next().unwrap();
        if *c != Rat::one() && !e.is_int() { return Err("a fractional power of a coefficient in IOLB output".into()); }
        let mut mono = m.clone();
        for x in mono.factors.values_mut() { *x = x.mul(e); }
        let mut out = Poly::zero();
        let coef = if e.is_int() { let mut k = Rat::one(); for _ in 0..e.n.abs() { k = k.mul(*c); } if e.n < 0 { Rat::new(k.d, k.n) } else { k } } else { Rat::one() };
        out.terms.insert(mono, coef);
        Ok(out)
    }
    fn number(&mut self) -> Result<i128, String> {
        self.ws();
        let s = self.i;
        while self.i < self.c.len() && self.c[self.i].is_ascii_digit() { self.i += 1; }
        if s == self.i { return Err("expected a number in IOLB output".into()); }
        self.c[s..self.i].iter().collect::<String>().parse().map_err(|_| "bad number".to_string())
    }
    fn atom(&mut self) -> Result<Poly, String> {
        match self.peek() {
            Some('(') => { self.i += 1; let e = self.expr()?; if !self.eat(')') { return Err("expected `)`".into()); } Ok(e) }
            Some(ch) if ch.is_ascii_digit() => Ok(Poly::constant(self.number()?)),
            Some(ch) if ch.is_alphabetic() || ch == '_' => {
                let s = self.i;
                while self.i < self.c.len() && (self.c[self.i].is_alphanumeric() || self.c[self.i] == '_') { self.i += 1; }
                let w: String = self.c[s..self.i].iter().collect();
                if w == "S" { return Ok(Poly::atom(Atom::M)); } // words; converted in `to_bytes`
                if w == "max" {
                    return Err("the asymptotic line should not contain max".into());
                }
                // a parameter: our atom of the same name (`n`), or `x.len()` for `x_n`
                if let Some(i) = self.names.iter().position(|n| *n == w) { return Ok(Poly::var(i)); }
                if let Some(stem) = w.strip_suffix("_n") { if let Some(i) = self.names.iter().position(|n| *n == format!("{stem}.len()")) { return Ok(Poly::var(i)); } }
                if let Some(stem) = w.strip_suffix("_rows") { let _ = stem; return Err(format!("IOLB's bound mentions `{w}`, a row count the export introduced; the bound is not in the function's sizes")); }
                Err(format!("`{w}` in IOLB's output is not a size of this function"))
            }
            Some(ch) => Err(format!("unexpected `{ch}` in IOLB output")),
            None => Err("IOLB output ends early".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> { vec!["n".into(), "a.len()".into()] }

    #[test]
    fn gemm_bound_in_bytes() {
        // 2·n³/√S words, S = M/8: 16·√8·n³/√M ≈ 45.2548·n³/√M bytes
        let p = to_bytes(&parse("2*n^3*S^(-1/2)", &names()).unwrap());
        assert_eq!(p.display(&names()).to_string(), "45.2548·n³/√M");
    }

    #[test]
    fn linear_and_sums() {
        let p = to_bytes(&parse("2*n+a_n", &names()).unwrap());
        assert_eq!(p.display(&names()).to_string(), "8·a.len() + 16·n");
        let p = to_bytes(&parse("n^2*S^(-1)", &names()).unwrap());
        assert_eq!(p.display(&names()).to_string(), "64·n²/M");
    }

    #[test]
    fn unknown_name_is_refused() {
        assert!(parse("2*m", &names()).is_err());
        assert!(parse("2*a_rows", &names()).is_err());
    }
}

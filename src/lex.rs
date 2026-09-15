//! Tokens. Newline and `;` are the same separator. `1 2 3` is one vector token; `` `a`b `` one symbol vector.
//! `-` glued to a digit is a negative literal unless a noun precedes it: `1 -2` is a vector, `1 - 2` is subtraction.
//! Also: 101b bools, 0N 0W 0n 0w null/infinity, 2026.09.15 dates, 12:30:00 times, `\:` `/:` each-left/right, dotted names.
use crate::value::*;
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok { Num(Value), Str(Vec<char>), Syms(Value), Name(String), Verb(char), Adv(char), Punct(char) }

const VERBS: &str = "+-*%!&|<>=~,^#_$?@";

pub fn lex(src: &str) -> R<Vec<Tok>> { Ok(lex_lines(src)?.0) }

/// Tokens plus the 1-based line each one starts on (for error messages).
pub fn lex_lines(src: &str) -> R<(Vec<Tok>, Vec<u32>)> {
    let s: Vec<char> = src.chars().collect();
    let n = s.len();
    let mut out: Vec<(Tok, u32)> = Vec::new();
    let mut i = 0;
    let mut line: u32 = 1;
    while i < n {
        let c = s[i];
        let next = s.get(i + 1).copied().unwrap_or(' ');
        if c == ' ' || c == '\t' || c == '\r' { i += 1; continue; }
        if c == '/' && next == '/' { while i < n && s[i] != '\n' { i += 1; } continue; }
        if c == '\n' || c == ';' { out.push((Tok::Punct(';'), line)); if c == '\n' { line += 1; } i += 1; continue; }
        if c == '`' {
            let mut names: Vec<Rc<str>> = Vec::new();
            while i < n && s[i] == '`' {
                let st = i + 1; let mut j = st;
                while j < n && (s[j].is_alphanumeric() || s[j] == '_' || s[j] == '.') { j += 1; }
                names.push(Rc::from(s[st..j].iter().collect::<String>()));
                i = j;
            }
            out.push((Tok::Syms(if names.len() == 1 { Value::Symbol(names.pop().unwrap()) } else { syms(names) }), line));
            continue;
        }
        if c == '"' {
            let tl = line;
            let mut j = i + 1; let mut buf = Vec::new();
            while j < n && s[j] != '"' {
                if s[j] == '\\' && j + 1 < n {
                    buf.push(match s[j + 1] { 'n' => '\n', 't' => '\t', 'r' => '\r', '0' => '\0', o => o }); j += 2;
                } else { if s[j] == '\n' { line += 1; } buf.push(s[j]); j += 1; }
            }
            if j >= n { return err(format!("lex: unterminated string at line {tl}")); }
            out.push((Tok::Str(buf), tl)); i = j + 1; continue;
        }
        let prev_noun = matches!(out.last(), Some((Tok::Num(_) | Tok::Str(_) | Tok::Syms(_) | Tok::Name(_) | Tok::Punct(')' | ']' | '}'), _)));
        let neg = c == '-' && (next.is_ascii_digit() || next == '.') && !prev_noun;
        if c == '0' && next == 'x' {   // 0x0aff bytes; a single pair is a byte atom
            let mut j = i + 2;
            while j < n && s[j].is_ascii_hexdigit() { j += 1; }
            let hex: String = s[i + 2..j].iter().collect();
            if hex.len() % 2 == 1 { return err(format!("lex: odd hex digits in 0x{hex} at line {line}")); }
            let v: Vec<u8> = (0..hex.len() / 2).map(|k| u8::from_str_radix(&hex[2 * k..2 * k + 2], 16).unwrap()).collect();
            out.push((Tok::Num(if v.len() == 1 { Value::Byte(v[0]) } else { bytes(v) }), line)); i = j; continue;
        }
        if c.is_ascii_digit() || neg || (c == '.' && next.is_ascii_digit()) {
            if let Some((d, mut end)) = date_at(&s, i) {   // 2026.01.01 2026.01.02 is one date vector
                let mut ds = vec![d];
                loop {
                    let mut k = end;
                    while k < n && (s[k] == ' ' || s[k] == '\t') { k += 1; }
                    match if k > end { date_at(&s, k) } else { None } { Some((d2, e2)) => { ds.push(d2); end = e2; } None => break }
                }
                out.push((Tok::Num(if ds.len() == 1 { Value::Date(ds[0]) } else { dates(ds) }), line)); i = end; continue;
            }
            if let Some((t, end)) = time_at(&s, i) { out.push((Tok::Num(Value::Time(t)), line)); i = end; continue; }
            if let Some((b, end)) = bool_at(&s, i) { out.push((Tok::Num(b), line)); i = end; continue; }
            let mut nums: Vec<String> = Vec::new();
            while let Some((txt, end)) = num_at(&s, i) {
                nums.push(txt); i = end;
                let mut k = i;
                while k < n && (s[k] == ' ' || s[k] == '\t') { k += 1; }
                if k > i && num_at(&s, k).is_some() { i = k; } else { break; }
            }
            let isf = nums.iter().any(|t| t.contains(['.', 'e', 'E']) || t.ends_with(['n', 'w']));
            let v = if isf {
                let f: Vec<f64> = nums.iter().map(|t| parse_f(t)).collect::<R<_>>()?;
                if f.len() == 1 { Value::Float(f[0]) } else { floats(f) }
            } else {
                let x: Vec<i64> = nums.iter().map(|t| parse_i(t)).collect::<R<_>>()?;
                if x.len() == 1 { Value::Int(x[0]) } else { ints(x) }
            };
            out.push((Tok::Num(v), line)); continue;
        }
        if c.is_ascii_alphabetic() || (c == '.' && next.is_ascii_alphabetic()) {
            let st = i; i += 1;
            while i < n && (s[i].is_ascii_alphanumeric() || s[i] == '_' || s[i] == '.') { i += 1; }
            out.push((Tok::Name(s[st..i].iter().collect()), line)); continue;
        }
        if "()[]{}:".contains(c) { out.push((Tok::Punct(c), line)); }
        else if VERBS.contains(c) { out.push((Tok::Verb(c), line)); }
        else if (c == '\\' || c == '/') && next == ':' { out.push((Tok::Adv(if c == '\\' { 'L' } else { 'R' }), line)); i += 1; }
        else if c == '/' || c == '\\' || c == '\'' { out.push((Tok::Adv(c), line)); }
        else { return err(format!("lex: unexpected {c:?} at line {line}")); }
        i += 1;
    }
    Ok(out.into_iter().unzip())
}

fn parse_i(t: &str) -> R<i64> {
    match t { "0N" => Ok(NI), "0W" => Ok(WI), "-0W" => Ok(NWI), _ => t.parse().map_err(|_| NError(format!("lex: bad number {t}"))) }
}
fn parse_f(t: &str) -> R<f64> {
    match t {
        "0n" | "0N" => Ok(f64::NAN), "0w" | "0W" => Ok(f64::INFINITY), "-0w" | "-0W" => Ok(f64::NEG_INFINITY),
        _ => t.parse().map_err(|_| NError(format!("lex: bad number {t}"))),
    }
}

fn digits(s: &[char], mut i: usize, n: usize) -> Option<usize> {   // exactly n digits at i
    for _ in 0..n { if i < s.len() && s[i].is_ascii_digit() { i += 1; } else { return None; } }
    Some(i)
}
fn ends_number(s: &[char], i: usize) -> bool { !s.get(i).map_or(false, |c| c.is_ascii_alphanumeric() || *c == '.') }

/// yyyy.mm.dd
fn date_at(s: &[char], i: usize) -> Option<(i32, usize)> {
    let a = digits(s, i, 4)?; if s.get(a) != Some(&'.') { return None; }
    let b = digits(s, a + 1, 2)?; if s.get(b) != Some(&'.') { return None; }
    let c = digits(s, b + 1, 2)?; if !ends_number(s, c) { return None; }
    crate::prims::parse_date(&s[i..c].iter().collect::<String>()).ok().map(|d| (d, c))
}
/// hh:mm[:ss[.mmm]]
fn time_at(s: &[char], i: usize) -> Option<(i64, usize)> {
    let a = digits(s, i, 2).or_else(|| digits(s, i, 1))?; if s.get(a) != Some(&':') { return None; }
    let mut e = digits(s, a + 1, 2)?;
    if s.get(e) == Some(&':') { if let Some(f) = digits(s, e + 1, 2) {
        e = f;
        if s.get(e) == Some(&'.') { let mut g = e + 1; while g < s.len() && s[g].is_ascii_digit() && g - e <= 3 { g += 1; } if g > e + 1 { e = g; } }
    } }
    if !ends_number(s, e) { return None; }
    crate::prims::parse_time(&s[i..e].iter().collect::<String>()).ok().map(|t| (t, e))
}
/// 1b 010b
fn bool_at(s: &[char], i: usize) -> Option<(Value, usize)> {
    let mut j = i;
    while j < s.len() && (s[j] == '0' || s[j] == '1') { j += 1; }
    if j == i || s.get(j) != Some(&'b') || !ends_number(s, j + 1) { return None; }
    let bits: Vec<bool> = s[i..j].iter().map(|&c| c == '1').collect();
    Some((if bits.len() == 1 { Value::Bool(bits[0]) } else { bools(bits) }, j + 1))
}

fn num_at(s: &[char], start: usize) -> Option<(String, usize)> {
    let n = s.len(); let mut i = start;
    if i < n && s[i] == '-' { i += 1; }
    if i + 1 < n && s[i] == '0' && "NWnw".contains(s[i + 1]) && ends_number(s, i + 2) {   // 0N 0W 0n 0w
        return Some((s[start..i + 2].iter().collect(), i + 2));
    }
    let ds = i;
    while i < n && s[i].is_ascii_digit() { i += 1; }
    let mut digits = i > ds;
    if i < n && s[i] == '.' {
        i += 1; let fs = i;
        while i < n && s[i].is_ascii_digit() { i += 1; }
        digits = digits || i > fs;
    }
    if !digits { return None; }
    if i < n && (s[i] == 'e' || s[i] == 'E') {
        let save = i; i += 1;
        if i < n && (s[i] == '+' || s[i] == '-') { i += 1; }
        let es = i;
        while i < n && s[i].is_ascii_digit() { i += 1; }
        if i == es { i = save; }
    }
    Some((s[start..i].iter().collect(), i))
}

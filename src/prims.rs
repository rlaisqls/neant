//! Primitive verbs (single chars) and named builtins. All pure: they never call back into the VM.
use crate::value::*;
use std::collections::HashMap;
use std::rc::Rc;
use Value::*;

// ---- dyads
// int null (0N) propagates through + - *; floats get that for free from NaN
fn nadd(a: i64, b: i64) -> i64 { if a == NI || b == NI { NI } else { a.wrapping_add(b) } }
fn nsub(a: i64, b: i64) -> i64 { if a == NI || b == NI { NI } else { a.wrapping_sub(b) } }
fn nmul(a: i64, b: i64) -> i64 { if a == NI || b == NI { NI } else { a.wrapping_mul(b) } }
/// date±int -> date, time±int -> time, date-date / time-time -> int. None when neither side is temporal.
fn temporal(x: &Value, y: &Value, fi: fn(i64, i64) -> i64, sub: bool) -> Option<R<Value>> {
    let kind = |v: &Value| match v { Date(_) | Dates(_) => 1, Time(_) | Times(_) => 2, _ => 0 };
    let (kx, ky) = (kind(x), kind(y));
    if kx == 0 && ky == 0 { return None; }
    if kx != 0 && ky != 0 && (!sub || kx != ky) { return Some(err("type: temporal arithmetic")); }
    let r = match (sh_i(x), sh_i(y)) { (Some(a), Some(b)) => zip(a, b, fi), _ => return Some(err("type: temporal arithmetic")) };
    let r = match r { Ok(r) => r, Err(e) => return Some(Err(e)) };
    let k = if kx != 0 && ky != 0 { 0 } else { kx.max(ky) };
    Some(Ok(match (k, r) {
        (1, Out::A(a)) => Date(a as i32), (1, Out::V(v)) => dates(v.into_iter().map(|a| a as i32).collect()),
        (2, Out::A(a)) => Time(a), (2, Out::V(v)) => times(v),
        (_, r) => oi(r),
    }))
}
fn add(x: Value, y: Value) -> R<Value> { if let Some(r) = temporal(&x, &y, nadd, false) { return r; } arith(&x, &y, nadd, |a, b| a + b) }
fn sub(x: Value, y: Value) -> R<Value> { if let Some(r) = temporal(&x, &y, nsub, true) { return r; } arith(&x, &y, nsub, |a, b| a - b) }
fn mul(x: Value, y: Value) -> R<Value> { arith(&x, &y, nmul, |a, b| a * b) }
fn div(x: Value, y: Value) -> R<Value> {
    match (sh_f(&x), sh_f(&y)) { (Some(a), Some(b)) => Ok(of(zip(a, b, |p, q| p / q)?)), _ => err("type: % on non-numeric") }
}
fn is_bool(v: &Value) -> bool { matches!(v, Bool(_) | Bools(_)) }
// bool & bool stays bool (and/or); otherwise min/max
fn min2(x: Value, y: Value) -> R<Value> {
    if is_bool(&x) && is_bool(&y) { return compare(&x, &y, |a, b| a.min(b) != 0, |a, b| a.min(b) != 0.0); }
    arith(&x, &y, i64::min, f64::min)
}
fn max2(x: Value, y: Value) -> R<Value> {
    if is_bool(&x) && is_bool(&y) { return compare(&x, &y, |a, b| a.max(b) != 0, |a, b| a.max(b) != 0.0); }
    arith(&x, &y, i64::max, f64::max)
}
fn lt(x: Value, y: Value) -> R<Value> { compare(&x, &y, |a, b| a < b, |a, b| a < b) }
fn gt(x: Value, y: Value) -> R<Value> { compare(&x, &y, |a, b| a > b, |a, b| a > b) }
fn eq(x: Value, y: Value) -> R<Value> {
    if x.is_num() && y.is_num() { return compare(&x, &y, |a, b| a == b, |a, b| a == b); }
    if x.is_atom() && y.is_atom() { return Ok(Bool(x == y)); }
    let (xs, ys) = broadcast(&x, &y)?;
    Ok(bools(xs.iter().zip(&ys).map(|(a, b)| a == b).collect()))
}
fn matches(x: Value, y: Value) -> R<Value> { Ok(Bool(x == y)) }
fn pow(x: Value, y: Value) -> R<Value> {
    let int_exp = match sh_i(&y) { Some(Sh::A(b)) => b >= 0, Some(Sh::V(v)) => v.iter().all(|&b| b >= 0), None => false };
    if int_exp && !x.is_float() { return arith(&x, &y, |a, b| a.wrapping_pow(b as u32), f64::powf); }
    match (sh_f(&x), sh_f(&y)) { (Some(a), Some(b)) => Ok(of(zip(a, b, f64::powf)?)), _ => err("type: ^ on non-numeric") }
}
/// `x,: y` after the compiler's Take: x is uniquely owned, so append in place instead of rebuilding.
fn try_append(x: &mut Value, y: &Value) -> bool {
    macro_rules! push { ($a:expr, $b:expr) => { match Rc::get_mut($a) { Some(v) => { v.push($b); true } None => false } } }
    macro_rules! ext { ($a:expr, $b:expr) => { match Rc::get_mut($a) { Some(v) => { v.extend_from_slice($b); true } None => false } } }
    match (x, y) {
        (Ints(a), Int(b)) => push!(a, *b), (Ints(a), Ints(b)) => ext!(a, b),
        (Floats(a), Float(b)) => push!(a, *b), (Floats(a), Floats(b)) => ext!(a, b),
        (Bools(a), Bool(b)) => push!(a, *b), (Bools(a), Bools(b)) => ext!(a, b),
        (Chars(a), Char(b)) => push!(a, *b), (Chars(a), Chars(b)) => ext!(a, b),
        (Syms(a), Symbol(b)) => push!(a, b.clone()), (Syms(a), Syms(b)) => ext!(a, b),
        (Dates(a), Date(b)) => push!(a, *b), (Dates(a), Dates(b)) => ext!(a, b),
        (Times(a), Time(b)) => push!(a, *b), (Times(a), Times(b)) => ext!(a, b),
        (Bytes(a), Byte(b)) => push!(a, *b), (Bytes(a), Bytes(b)) => ext!(a, b),
        (List(a), b) => match Rc::get_mut(a) { Some(v) => { v.extend(b.seq()); true } None => false },
        _ => false,
    }
}
fn join(mut x: Value, y: Value) -> R<Value> {
    if try_append(&mut x, &y) { return Ok(x); }
    let mut a = x.seq(); a.extend(y.seq()); Ok(pack(a))
}
fn take(n: Value, x: Value) -> R<Value> {
    let n = int_of(&n)?; let s = x.seq(); let len = s.len() as i64;
    if len == 0 { return err("take from empty"); }
    let idx: Vec<i64> = if n >= 0 { (0..n).map(|i| i % len).collect() } else { (n..0).map(|i| (len + i) % len).collect() };
    Ok(pack(idx.into_iter().map(|i| s[i as usize].clone()).collect()))
}
fn drop(n: Value, x: Value) -> R<Value> {
    let n = int_of(&n)?; let s = x.seq(); let len = s.len() as i64;
    let (a, b) = if n >= 0 { (n.min(len), len) } else { (0, (len + n).max(0)) };
    Ok(pack(s[a as usize..b as usize].to_vec()))
}
fn dict(k: Value, v: Value) -> R<Value> {
    let vc = if is_table(&v) { v.item(0).map(|c| c.count()).unwrap_or(0) } else { v.count() };   // keyed table: one key per row
    if k.count() != vc { return err("length: dict"); }
    Ok(Dict(Rc::new(crate::value::Dict { keys: k, vals: v })))
}
/// Hashable form of an atom, so find/distinct/group are O(n). Lists and nested values fall back to a linear scan.
#[derive(Hash, PartialEq, Eq)]
enum Key { N, B(bool), I(i64), F(u64), C(char), S(Rc<str>), D(i32), T(i64), Y(u8) }
fn key_of(v: &Value) -> Option<Key> {
    Some(match v {
        Null => Key::N, Bool(b) => Key::B(*b), Int(i) => Key::I(*i), Char(c) => Key::C(*c), Symbol(s) => Key::S(s.clone()),
        Date(d) => Key::D(*d), Time(t) => Key::T(*t), Byte(b) => Key::Y(*b),
        Float(f) => Key::F(if f.is_nan() { u64::MAX } else if *f == 0.0 { 0 } else { f.to_bits() }),
        _ => return None,
    })
}
fn keys_of(xs: &[Value]) -> Option<Vec<Key>> { xs.iter().map(key_of).collect() }

/// Atom lookup in a typed vector: scan the raw elements instead of materializing x as a Vec<Value>.
/// This is the hot path — every `x in y` in the boot compiler's dispatch chains goes through it.
fn find_atom(x: &Value, i: &Value) -> Option<i64> {
    let at = |n: usize, hit: Option<usize>| hit.unwrap_or(n) as i64;
    Some(match (x, i) {
        (Syms(v), Symbol(s)) => at(v.len(), v.iter().position(|e| e == s)),
        (Ints(v), Int(a)) => at(v.len(), v.iter().position(|e| e == a)),
        (Chars(v), Char(c)) => at(v.len(), v.iter().position(|e| e == c)),
        (Bools(v), Bool(b)) => at(v.len(), v.iter().position(|e| e == b)),
        (Floats(v), Float(f)) => at(v.len(), v.iter().position(|e| e == f)),
        (Dates(v), Date(d)) => at(v.len(), v.iter().position(|e| e == d)),
        (Times(v), Time(t)) => at(v.len(), v.iter().position(|e| e == t)),
        (Bytes(v), Byte(b)) => at(v.len(), v.iter().position(|e| e == b)),
        _ => return None,
    })
}
fn find(x: Value, i: Value) -> R<Value> {
    if let Some(n) = find_atom(&x, &i) { return Ok(Int(n)); }
    let xs = x.seq();
    let n = xs.len() as i64;
    let small = i.is_atom() || i.count() * xs.len() < 1 << 14;   // few lookups: a scan beats building the table
    let pos: Box<dyn Fn(&Value) -> i64> = match if small { None } else { keys_of(&xs) } {
        Some(ks) => {
            let mut m: HashMap<Key, i64> = HashMap::with_capacity(ks.len());
            for (p, k) in ks.into_iter().enumerate() { m.entry(k).or_insert(p as i64); }
            Box::new(move |v: &Value| key_of(v).and_then(|k| m.get(&k).copied()).unwrap_or(n))
        }
        None => Box::new(move |v: &Value| xs.iter().position(|e| e == v).map_or(n, |p| p as i64)),
    };
    // in a general list the items are rows: (1 2;3 4)?3 4 finds the row; a list of rows looks up each
    let whole = i.is_atom() || (matches!(x, List(_)) && !matches!(i, List(_)));
    if whole { Ok(Int(pos(&i))) } else { Ok(ints(i.seq().iter().map(|v| pos(v)).collect())) }
}
/// A table is a dict of symbol keys over equal-length columns.
fn is_table(v: &Value) -> bool {
    let Dict(d) = v else { return false };
    let Syms(_) = &d.keys else { return false };
    let List(cols) = &d.vals else { return false };
    !cols.is_empty() && cols.iter().all(|c| !c.is_atom()) && cols.iter().all(|c| c.count() == cols[0].count())
}
/// Row i of a table as a dict.
fn row_at(t: &Value, i: usize) -> R<Value> {
    let Dict(d) = t else { unreachable!() };
    Ok(Dict(Rc::new(crate::value::Dict { keys: d.keys.clone(), vals: pack(d.vals.seq().iter().map(|c| c.item(i)).collect::<R<Vec<_>>>()?) })))
}
pub fn index_at(x: Value, i: Value) -> R<Value> {
    if let Dict(d) = &x {
        if is_table(&x) && matches!(i, Int(_) | Ints(_) | Bool(_) | Bools(_)) {   // t[2] row, t[0 2] rows
            let n = d.vals.item(0)?.count();
            if i.is_atom() { return row_at(&x, usize::try_from(int_of(&i)?).ok().filter(|&p| p < n).ok_or_else(|| NError("index".into()))?); }
            return Ok(Dict(Rc::new(crate::value::Dict { keys: d.keys.clone(), vals: list(d.vals.seq().into_iter().map(|c| index_at(c, i.clone())).collect::<R<Vec<_>>>()?) })));
        }
        let keys = d.keys.seq();
        let keyed = is_table(&d.vals);   // xkey: values are a table, lookups yield rows
        // multi-column key: kt[(1;2)] is one key row, not two lookups
        if keyed && !i.is_atom() && matches!(d.keys, List(_)) && keys.first().is_some_and(|k| k.count() == i.count()) {
            return match keys.iter().position(|e| *e == i) { Some(p) => row_at(&d.vals, p), None => Ok(Null) };
        }
        let one = |k: &Value| match keys.iter().position(|e| e == k) {
            Some(p) => if keyed { row_at(&d.vals, p) } else { d.vals.item(p) },
            None => Ok(Null),
        };
        return if i.is_atom() { one(&i) } else { Ok(pack(i.seq().iter().map(one).collect::<R<Vec<_>>>()?)) };
    }
    if i.is_atom() { return x.item(usize::try_from(int_of(&i)?).map_err(|_| NError("index".into()))?); }
    if i.count() == 0 { return Ok(empty_like(&x)); }   // x[()] keeps x's type: "" for strings, not an empty int vector
    Ok(pack(i.seq().iter().map(|j| index_at(x.clone(), j.clone())).collect::<R<Vec<_>>>()?))
}
fn empty_like(x: &Value) -> Value {
    match x {
        Chars(_) | Char(_) => chars(vec![]), Floats(_) => floats(vec![]), Bools(_) => bools(vec![]), Syms(_) => syms(vec![]),
        Dates(_) => dates(vec![]), Times(_) => times(vec![]), Bytes(_) | Byte(_) => bytes(vec![]), List(_) => list(vec![]), _ => ints(vec![]),
    }
}
/// `=x` group: distinct items -> indices where they occur, in first-seen order.
fn group(x: Value) -> R<Value> {
    let xs = x.seq();
    let (mut keys, mut idx): (Vec<Value>, Vec<Vec<i64>>) = (Vec::new(), Vec::new());
    match keys_of(&xs) {
        Some(ks) => {
            let mut at: HashMap<Key, usize> = HashMap::with_capacity(ks.len());
            for (i, (v, k)) in xs.into_iter().zip(ks).enumerate() {
                match at.get(&k) {
                    Some(&p) => idx[p].push(i as i64),
                    None => { at.insert(k, keys.len()); keys.push(v); idx.push(vec![i as i64]); }
                }
            }
        }
        None => for (i, v) in xs.into_iter().enumerate() {   // nested values (e.g. rows from flip): linear
            match keys.iter().position(|k| *k == v) {
                Some(p) => idx[p].push(i as i64),
                None => { keys.push(v); idx.push(vec![i as i64]); }
            }
        },
    }
    dict(pack(keys), list(idx.into_iter().map(ints).collect()))
}
fn broadcast(x: &Value, y: &Value) -> R<(Vec<Value>, Vec<Value>)> {
    let (mut xs, mut ys) = (x.seq(), y.seq());
    if xs.len() == 1 && ys.len() > 1 { xs = vec![xs[0].clone(); ys.len()]; }
    if ys.len() == 1 && xs.len() > 1 { ys = vec![ys[0].clone(); xs.len()]; }
    if xs.len() != ys.len() { return err("length"); }
    Ok((xs, ys))
}

// ---- monads
fn neg(x: Value) -> R<Value> { if x.is_float() { map_f(&x, |a| -a) } else { map_i(&x, |a| a.wrapping_neg()) } }
fn first(x: Value) -> R<Value> { if x.is_atom() { Ok(x) } else { x.item(0).or_else(|_| err("first of empty")) } }
fn recip(x: Value) -> R<Value> { map_f(&x, |a| 1.0 / a) }
fn where_(x: Value) -> R<Value> {
    match sh_i(&x) {
        Some(Sh::A(n)) => Ok(ints(vec![0; n.max(0) as usize])),
        Some(Sh::V(v)) => Ok(ints(v.iter().enumerate().flat_map(|(i, &n)| std::iter::repeat_n(i as i64, n.max(0) as usize)).collect())),
        None => err("type: where"),
    }
}
fn reverse(x: Value) -> R<Value> { let mut s = x.seq(); s.reverse(); Ok(pack(s)) }
fn grade(x: Value, desc: bool) -> R<Value> {
    let s = x.seq(); let mut idx: Vec<usize> = (0..s.len()).collect();
    idx.sort_by(|&a, &b| if desc { cmp_val(&s[b], &s[a]) } else { cmp_val(&s[a], &s[b]) });
    Ok(ints(idx.into_iter().map(|i| i as i64).collect()))
}
fn iasc(x: Value) -> R<Value> { grade(x, false) }
fn idesc(x: Value) -> R<Value> { grade(x, true) }
fn not(x: Value) -> R<Value> {
    match sh_f(&x) { Some(Sh::A(a)) => Ok(Bool(a == 0.0)), Some(Sh::V(v)) => Ok(bools(v.iter().map(|&a| a == 0.0).collect())), None => err("type: not") }
}
fn enlist(x: Value) -> R<Value> { Ok(pack(vec![x])) }
fn count(x: Value) -> R<Value> { Ok(Int(x.count() as i64)) }
fn floor(x: Value) -> R<Value> {
    match x {
        Float(f) => Ok(Int(f.floor() as i64)),
        Floats(v) => Ok(ints(v.iter().map(|f| f.floor() as i64).collect())),
        Int(_) | Ints(_) | Bool(_) | Bools(_) => Ok(x),
        _ => err("type: floor"),
    }
}
fn til(x: Value) -> R<Value> { Ok(ints((0..int_of(&x)?).collect())) }
fn distinct(x: Value) -> R<Value> {
    let xs = x.seq();
    let mut out: Vec<Value> = Vec::new();
    match keys_of(&xs) {
        Some(ks) => {
            let mut seen: std::collections::HashSet<Key> = std::collections::HashSet::with_capacity(ks.len());
            for (v, k) in xs.into_iter().zip(ks) { if seen.insert(k) { out.push(v); } }
        }
        None => for v in xs { if !out.contains(&v) { out.push(v); } },   // nested values: linear
    }
    Ok(pack(out))
}
fn type_(x: Value) -> R<Value> {
    Ok(Symbol(Rc::from(match x {
        Null => "null", Bool(_) => "bool", Int(_) => "int", Float(_) => "float", Char(_) => "char", Symbol(_) => "sym",
        Bools(_) => "bools", Ints(_) => "ints", Floats(_) => "floats", Chars(_) => "chars", Syms(_) => "syms",
        Date(_) => "date", Time(_) => "time", Dates(_) => "dates", Times(_) => "times", Byte(_) => "byte", Bytes(_) => "bytes",
        List(_) => "list", Dict(_) => "dict", Lambda(_) | Prim(_) | Adv(..) | Proj(..) | Closure(..) => "fn",
    })))
}
fn flip(x: Value) -> R<Value> {
    let rows = x.seq();
    let n = rows.first().map(|r| r.count()).unwrap_or(0);
    if rows.iter().any(|r| r.count() != n) { return err("length: flip"); }
    Ok(list((0..n).map(|j| pack(rows.iter().map(|r| if r.is_atom() { r.clone() } else { r.item(j).unwrap() }).collect())).collect()))
}
pub fn to_chars(x: &Value) -> Vec<char> {
    match x { Chars(v) => v.as_ref().clone(), Symbol(s) => s.chars().collect(), Char(c) => vec![*c], Null => vec![], _ => x.fmt().chars().collect() }
}
fn string(x: Value) -> R<Value> { Ok(chars(to_chars(&x))) }
fn sym(x: Value) -> R<Value> { Ok(Symbol(Rc::from(to_chars(&x).into_iter().collect::<String>()))) }
/// `tag$x`: `` `int `` `` `float `` `` `char `` `` `sym `` `` `string `` convert; chars parse; `` `code `` gives code points.
fn cast(t: Value, x: Value) -> R<Value> {
    let Symbol(tag) = &t else { return err("type: cast tag must be a symbol") };
    if let List(items) = &x { return Ok(pack(items.iter().map(|v| cast(t.clone(), v.clone())).collect::<R<Vec<_>>>()?)); }
    let text = || to_chars(&x).into_iter().collect::<String>();
    match &**tag {
        "" | "sym" => sym(x),
        "string" => string(x),
        "int" => match &x {
            Char(_) | Chars(_) => text().trim().parse::<i64>().map(Int).map_err(|_| NError(format!("parse: not an int: {:?}", text()))),
            _ => match sh_f(&x) { Some(Sh::A(a)) => Ok(Int(a.trunc() as i64)), Some(Sh::V(v)) => Ok(ints(v.iter().map(|a| a.trunc() as i64).collect())), None => err("type: `int$") },
        },
        "float" => match &x {
            Char(_) | Chars(_) => text().trim().parse::<f64>().map(Float).map_err(|_| NError(format!("parse: not a float: {:?}", text()))),
            _ => map_f(&x, |a| a),
        },
        "byte" => match &x {   // strings encode as UTF-8; ints are masked to a byte
            Char(_) | Chars(_) => Ok(bytes(text().into_bytes())),
            Byte(_) | Bytes(_) => Ok(x),
            _ => match sh_i(&x) { Some(Sh::A(a)) => Ok(Byte(a as u8)), Some(Sh::V(v)) => Ok(bytes(v.iter().map(|&a| a as u8).collect())), None => err("type: `byte$") },
        },
        "char" => match &x {
            Bytes(v) => Ok(chars(String::from_utf8_lossy(v).chars().collect())),   // UTF-8 decode
            Byte(b) => Ok(Char(*b as char)),
            _ => match sh_i(&x) {
            Some(Sh::A(a)) => Ok(Char(char::from_u32(a as u32).unwrap_or('?'))),
            Some(Sh::V(v)) => Ok(chars(v.iter().map(|&a| char::from_u32(a as u32).unwrap_or('?')).collect())),
            None => err("type: `char$"),
        } },
        "code" => match &x { Char(c) => Ok(Int(*c as i64)), Chars(v) => Ok(ints(v.iter().map(|&c| c as i64).collect())), _ => err("type: `code$") },
        "date" => match &x {
            Char(_) | Chars(_) => parse_date(&text()).map(Date),
            Date(_) | Dates(_) => Ok(x),
            _ => match sh_i(&x) { Some(Sh::A(a)) => Ok(Date(a as i32)), Some(Sh::V(v)) => Ok(dates(v.iter().map(|&a| a as i32).collect())), None => err("type: `date$") },
        },
        "time" => match &x {
            Char(_) | Chars(_) => parse_time(&text()).map(Time),
            Time(_) | Times(_) => Ok(x),
            _ => match sh_i(&x) { Some(Sh::A(a)) => Ok(Time(a)), Some(Sh::V(v)) => Ok(times(v.to_vec())), None => err("type: `time$") },
        },
        "year" | "month" | "day" => {
            let part = |d: i32| { let (y, m, dd) = civil(d); match &**tag { "year" => y, "month" => m as i64, _ => dd as i64 } };
            match &x { Date(d) => Ok(Int(part(*d))), Dates(v) => Ok(ints(v.iter().map(|&d| part(d)).collect())), _ => err(format!("type: `{tag}$ needs a date")) }
        }
        "hour" | "minute" | "second" => {
            let div = match &**tag { "hour" => 3_600_000, "minute" => 60_000, _ => 1000 };
            let part = |t: i64| (t / div) % if div == 3_600_000 { 24 } else { 60 };
            match &x { Time(t) => Ok(Int(part(*t))), Times(v) => Ok(ints(v.iter().map(|&t| part(t)).collect())), _ => err(format!("type: `{tag}$ needs a time")) }
        }
        _ => err(format!("type: unknown cast `{tag}")),
    }
}

// ---- strings and IO
fn text(x: &Value) -> String { to_chars(x).into_iter().collect() }
fn lines(s: String) -> Value { list(s.lines().map(|l| chars(l.chars().collect())).collect()) }
/// `read0 "path"` -> list of lines; `read0 0` reads stdin.
fn read0(x: Value) -> R<Value> {
    if let Int(0) = x {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s).map_err(|e| NError(e.to_string()))?;
        return Ok(lines(s));
    }
    let p = text(&x);
    Ok(lines(std::fs::read_to_string(&p).map_err(|e| NError(format!("{p}: {e}")))?))
}
// ---- TCP sockets. Handles are ints into a process-local table; the VM is single-threaded, so
// a thread_local is all the state these need and the prims stay plain fn(Value) -> R<Value>.
thread_local! {
    static SOCKS: std::cell::RefCell<(i64, HashMap<i64, std::net::TcpStream>)> =
        std::cell::RefCell::new((0, HashMap::new()));
}
fn sock<T>(h: &Value, f: impl FnOnce(&mut std::net::TcpStream) -> R<T>) -> R<T> {
    let h = int_of(h)?;
    SOCKS.with(|c| match c.borrow_mut().1.get_mut(&h) { Some(s) => f(s), None => err(format!("hsock: no handle {h}")) })
}
/// bytes to put on the wire: a byte vector as-is, a string as UTF-8 (what `` `byte$ `` would give).
fn wire(v: &Value) -> R<Vec<u8>> {
    match v {
        Bytes(b) => Ok(b.as_ref().clone()),
        Byte(b) => Ok(vec![*b]),
        Chars(_) | Char(_) | Symbol(_) => Ok(text(v).into_bytes()),
        _ => err("type: expected bytes or a string"),
    }
}
/// `hopen "host:port"` -> handle; `hopen ("host:port"; timeoutMs)` sets the connect and read timeout.
fn hopen(x: Value) -> R<Value> {
    use std::net::{TcpStream, ToSocketAddrs};
    let (addr, ms) = match &x {
        List(v) if v.len() == 2 => (text(&v[0]), int_of(&v[1])? as u64),
        _ => (text(&x), 30_000),
    };
    let to = std::time::Duration::from_millis(ms.max(1));
    // try every resolved address: the first is often IPv6 on a host that only routes IPv4
    let mut last = NError(format!("hopen {addr}: no address"));
    let mut sock = None;
    for sa in addr.to_socket_addrs().map_err(|e| NError(format!("hopen {addr}: {e}")))? {
        match TcpStream::connect_timeout(&sa, to) { Ok(s) => { sock = Some(s); break } Err(e) => last = NError(format!("hopen {addr}: {e}")) }
    }
    let s = sock.ok_or(last)?;
    s.set_read_timeout(Some(to)).ok();
    s.set_nodelay(true).ok();
    Ok(SOCKS.with(|c| { let mut c = c.borrow_mut(); c.0 += 1; let h = c.0; c.1.insert(h, s); Int(h) }))
}
fn hclose(x: Value) -> R<Value> {
    let h = int_of(&x)?;
    SOCKS.with(|c| c.borrow_mut().1.remove(&h));
    Ok(Null)
}
/// `hsend[h;x]` writes every byte of x; the count written.
fn hsend(h: Value, x: Value) -> R<Value> {
    let b = wire(&x)?;
    sock(&h, |s| std::io::Write::write_all(s, &b).map_err(|e| NError(format!("hsend: {e}"))))?;
    Ok(Int(b.len() as i64))
}
/// `hrecv[h;n]` reads once, up to n bytes. Empty means the peer closed.
fn hrecv(h: Value, n: Value) -> R<Value> {
    let n = int_of(&n)?.max(0) as usize;
    let mut buf = vec![0u8; n];
    let got = sock(&h, |s| std::io::Read::read(s, &mut buf).map_err(|e| NError(format!("hrecv: {e}"))))?;
    buf.truncate(got);
    Ok(bytes(buf))
}

fn write0(path: Value, x: Value) -> R<Value> {
    let p = text(&path);
    let body = match &x { List(items) => items.iter().map(text).collect::<Vec<_>>().join("\n") + "\n", _ => text(&x) };
    std::fs::write(&p, body).map_err(|e| NError(format!("{p}: {e}")))?;
    Ok(Null)
}
/// stdout without panicking on a closed pipe (`neant f.nt | head`)
pub fn out(s: &str) { use std::io::Write; let mut o = std::io::stdout().lock(); let _ = o.write_all(s.as_bytes()); let _ = o.write_all(b"\n"); }
fn print(x: Value) -> R<Value> { out(&text(&x)); Ok(Null) }
fn signal(x: Value) -> R<Value> { Err(NError(text(&x))) }

// ---- temporal and null
pub fn parse_date(s: &str) -> R<i32> {
    let p: Vec<i64> = s.trim().split('.').map(|t| t.parse().ok()).collect::<Option<_>>().unwrap_or_default();
    if p.len() != 3 || !(1..=12).contains(&p[1]) || !(1..=31).contains(&p[2]) { return err(format!("parse: not a date: {s:?}")); }
    let d = days_from_civil(p[0], p[1] as u32, p[2] as u32);
    if civil(d) != (p[0], p[1] as u32, p[2] as u32) { return err(format!("parse: not a date: {s:?}")); }
    Ok(d)
}
pub fn parse_time(s: &str) -> R<i64> {
    let bad = || NError(format!("parse: not a time: {s:?}"));
    let parts: Vec<&str> = s.trim().split(':').collect();
    if parts.len() < 2 || parts.len() > 3 { return Err(bad()); }
    let h: i64 = parts[0].parse().map_err(|_| bad())?;
    let m: i64 = parts[1].parse().map_err(|_| bad())?;
    let (sec, ms) = match parts.get(2) {
        None => (0, 0),
        Some(t) => {
            let (a, b) = t.split_once('.').unwrap_or((t, ""));
            let ms: i64 = if b.is_empty() { 0 } else { format!("{b:0<3}")[..3].parse().map_err(|_| bad())? };
            (a.parse().map_err(|_| bad())?, ms)
        }
    };
    Ok(((h * 60 + m) * 60 + sec) * 1000 + ms)
}
/// null per item: 0N 0n ` " " :: and null date/time
fn isnull(x: Value) -> R<Value> {
    fn one(v: &Value) -> bool {
        match v { Null => true, Int(i) => *i == NI, Float(f) => f.is_nan(), Symbol(s) => s.is_empty(), Char(c) => *c == ' ', Date(d) => *d == i32::MIN, Time(t) => *t == NI, _ => false }
    }
    if x.is_atom() { Ok(Bool(one(&x))) } else { Ok(bools(x.seq().iter().map(one).collect())) }
}
/// now`date / now`time: wall clock in local-agnostic UTC.
fn now(x: Value) -> R<Value> {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    match &x {
        Symbol(s) if &**s == "date" => Ok(Date((ms / 86_400_000 - DATE_EPOCH) as i32)),
        Symbol(s) if &**s == "time" => Ok(Time(ms % 86_400_000)),
        _ => err("type: now`date or now`time"),
    }
}

/// The Rust parser as neant data: each node is (`kind; ...) with identifiers as symbols. Oracle for boot/parse.nt.
fn ast_value(a: &crate::parse::Ast) -> Value {
    use crate::parse::Ast;
    let s = |n: &str| Symbol(Rc::from(n));
    let name = |n: &String| Symbol(Rc::from(n.as_str()));
    let many = |xs: &[Ast]| pack(xs.iter().map(ast_value).collect());
    pack(match a {
        Ast::Const(v) => vec![s("const"), v.clone()],
        Ast::Name(n) => vec![s("name"), name(n)],
        Ast::Verb(c) => vec![s("verb"), Char(*c)],
        Ast::Advb(c, x) => vec![s("advb"), Char(*c), ast_value(x)],
        Ast::Mo(f, x) => vec![s("mo"), ast_value(f), ast_value(x)],
        Ast::Dy(f, x, y) => vec![s("dy"), ast_value(f), ast_value(x), ast_value(y)],
        Ast::App(f, x) => vec![s("app"), ast_value(f), ast_value(x)],
        Ast::Call(f, args) => vec![s("call"), ast_value(f), many(args)],
        Ast::List(xs) => vec![s("list"), many(xs)],
        Ast::Assign(n, e) => vec![s("assign"), name(n), ast_value(e)],
        Ast::GAssign(n, e) => vec![s("gassign"), name(n), ast_value(e)],
        Ast::IndexAssign(n, i, e) => vec![s("iassign"), name(n), many(i), ast_value(e)],
        Ast::Do(c, b) => vec![s("do"), ast_value(c), many(b)],
        Ast::Break => vec![s("break")],
        Ast::Return(e) => vec![s("return"), ast_value(e)],
        Ast::Lambda(ps, body) => vec![s("fn"), pack(ps.iter().map(name).collect()), many(body)],
        Ast::If(c, b) => vec![s("if"), ast_value(c), many(b)],
        Ast::While(c, b) => vec![s("while"), ast_value(c), many(b)],
        Ast::Cond(xs) => vec![s("cond"), many(xs)],
        Ast::Noun(x) => vec![s("noun"), ast_value(x)],
        Ast::At(l, x) => vec![s("at"), Int(*l as i64), ast_value(x)],
    })
}
fn parse_oracle(x: Value) -> R<Value> {
    let (t, l) = crate::lex::lex_lines(&text(&x))?;
    let prog = crate::parse::Parser::with_lines(t, l).program()?;
    Ok(pack(prog.iter().map(ast_value).collect()))
}

/// The Rust lexer as neant data: a list of (`kind; value) pairs. Oracle for the self-hosted lexer in boot/lex.nt.
fn lex_oracle(x: Value) -> R<Value> {
    use crate::lex::Tok;
    let sym = |s: &str| Symbol(Rc::from(s));
    Ok(pack(crate::lex::lex(&text(&x))?.into_iter().map(|t| pack(match t {
        Tok::Num(v) => vec![sym("num"), v],
        Tok::Str(s) => vec![sym("str"), chars(s)],
        Tok::Syms(v) => vec![sym("sym"), v],
        Tok::Name(n) => vec![sym("name"), chars(n.chars().collect())],
        Tok::Verb(c) => vec![sym("verb"), Char(c)],
        Tok::Adv(c) => vec![sym("adv"), Char(c)],
        Tok::Punct(c) => vec![sym("punct"), Char(c)],
    })).collect()))
}

/// `x[i;j]:v`: amend along a path. All but the last index must be atoms.
pub fn amend_path(x: &mut Value, idx: &[Value], v: Value) -> R<()> {
    if idx.len() == 1 { return amend(x, idx[0].clone(), v); }
    if !idx[0].is_atom() { return err("nyi: deep amend needs atom indices before the last"); }
    let mut sub = index_at(x.clone(), idx[0].clone())?;   // ponytail: clones the sub-vector once per deep amend
    amend_path(&mut sub, &idx[1..], v)?;
    amend(x, idx[0].clone(), sub)
}
/// `x[i]:v` in place. Vector index amends each; a missing dict key appends.
pub fn amend(x: &mut Value, i: Value, v: Value) -> R<()> {
    if !i.is_atom() {
        let idx = i.seq();
        let vs = if v.is_atom() { vec![v; idx.len()] } else { v.seq() };
        if vs.len() != idx.len() { return err("length: amend"); }
        for (k, val) in idx.into_iter().zip(vs) { amend(x, k, val)?; }
        return Ok(());
    }
    if let Dict(d) = x {
        let dm = Rc::make_mut(d);
        return match dm.keys.seq().iter().position(|e| *e == i) {
            Some(p) => amend(&mut dm.vals, Int(p as i64), v),
            // enlist, not join: a vector value is one entry, so `d[`c]: 10 20` does not splice into vals
            None => { dm.keys = join(dm.keys.clone(), i)?; dm.vals = join(dm.vals.clone(), enlist(v)?)?; Ok(()) }
        };
    }
    let p = usize::try_from(int_of(&i)?).map_err(|_| NError("index".into()))?;
    if x.is_atom() || p >= x.count() { return err("index: amend out of range"); }
    let typed = match (&mut *x, &v) {
        (Ints(r), Int(a)) => { Rc::make_mut(r)[p] = *a; true }
        (Floats(r), Float(a)) => { Rc::make_mut(r)[p] = *a; true }
        (Floats(r), Int(a)) => { Rc::make_mut(r)[p] = *a as f64; true }
        (Bools(r), Bool(a)) => { Rc::make_mut(r)[p] = *a; true }
        (Chars(r), Char(a)) => { Rc::make_mut(r)[p] = *a; true }
        (Bytes(r), Byte(a)) => { Rc::make_mut(r)[p] = *a; true }
        (Syms(r), Symbol(a)) => { Rc::make_mut(r)[p] = a.clone(); true }
        (Dates(r), Date(a)) => { Rc::make_mut(r)[p] = *a; true }
        (Times(r), Time(a)) => { Rc::make_mut(r)[p] = *a; true }
        (List(r), _) => { Rc::make_mut(r)[p] = v.clone(); true }
        _ => false,
    };
    if !typed { let mut s = x.seq(); s[p] = v; *x = pack(s); }   // type widens to a general list
    Ok(())
}
fn show(x: Value) -> R<Value> { out(&x.fmt()); Ok(Null) }
fn key(x: Value) -> R<Value> { match x { Dict(d) => Ok(d.keys.clone()), _ => til(x) } }
fn value(x: Value) -> R<Value> { match x { Dict(d) => Ok(d.vals.clone()), _ => Ok(x) } }
fn sqrt(x: Value) -> R<Value> { map_f(&x, f64::sqrt) }
fn exp(x: Value) -> R<Value> { map_f(&x, f64::exp) }
fn log(x: Value) -> R<Value> { map_f(&x, f64::ln) }
fn sin(x: Value) -> R<Value> { map_f(&x, f64::sin) }
fn cos(x: Value) -> R<Value> { map_f(&x, f64::cos) }
fn tan(x: Value) -> R<Value> { map_f(&x, f64::tan) }
fn atan(x: Value) -> R<Value> { map_f(&x, f64::atan) }

// ---- bits: on ints (i64 as u64, logical shifts); two byte operands give bytes, so `key bxor data` stays bytes
fn bitop(x: Value, y: Value, f: fn(u64, u64) -> u64) -> R<Value> {
    let by = matches!(x, Byte(_) | Bytes(_)) && matches!(y, Byte(_) | Bytes(_));
    let r = match (sh_i(&x), sh_i(&y)) { (Some(a), Some(b)) => zip(a, b, |p, q| f(p as u64, q as u64) as i64)?, _ => return err("type: bit op on non-integer") };
    Ok(if by { match r { Out::A(v) => Byte(v as u8), Out::V(v) => bytes(v.iter().map(|&i| i as u8).collect()) } } else { oi(r) })
}
fn band(x: Value, y: Value) -> R<Value> { bitop(x, y, |a, b| a & b) }
fn bor(x: Value, y: Value) -> R<Value> { bitop(x, y, |a, b| a | b) }
fn bxor(x: Value, y: Value) -> R<Value> { bitop(x, y, |a, b| a ^ b) }
fn shl(x: Value, y: Value) -> R<Value> { bitop(x, y, |a, n| a.checked_shl(n as u32).unwrap_or(0)) }
fn shr(x: Value, y: Value) -> R<Value> { bitop(x, y, |a, n| a.checked_shr(n as u32).unwrap_or(0)) }
fn bnot(x: Value) -> R<Value> {
    match &x { Byte(b) => Ok(Byte(!b)), Bytes(v) => Ok(bytes(v.iter().map(|b| !b).collect())), _ => map_i(&x, |a| !a) }
}

// ---- random: xorshift64 seeded from the clock; `rseed n` makes a run reproducible
thread_local! { static RNG: std::cell::Cell<u64> = std::cell::Cell::new(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1); }
fn next_u64() -> u64 { RNG.with(|s| { let mut x = s.get(); x ^= x << 13; x ^= x >> 7; x ^= x << 17; s.set(x); x }) }
fn draw(m: &Value) -> R<Value> {
    Ok(match m {
        Int(k) if *k > 0 => Int((next_u64() % *k as u64) as i64),
        Float(f) => Float((next_u64() >> 11) as f64 / (1u64 << 53) as f64 * f),
        _ if m.len().is_some() => { let n = m.count(); if n == 0 { return err("rand: empty"); } m.item((next_u64() % n as u64) as usize)? }
        _ => return err("type: rand needs a positive int, a float, or a list"),
    })
}
/// `n rand m`: n draws from [0;m) (int or float m) or from the list m; `rand m` is one draw.
fn rand2(n: Value, m: Value) -> R<Value> { Ok(pack((0..int_of(&n)?).map(|_| draw(&m)).collect::<R<Vec<_>>>()?)) }
fn rand1(m: Value) -> R<Value> { draw(&m) }
fn rseed(x: Value) -> R<Value> { let s = int_of(&x)? as u64 | 1; RNG.with(|c| c.set(s)); Ok(Null) }
fn exit(x: Value) -> R<Value> { std::process::exit(int_of(&x)? as i32) }

/// Fused folds: `+/x` on a typed vector never touches the interpreter loop.
pub fn fold_fast(c: char, x: &Value) -> Option<Value> {
    match (c, x) {
        ('+', Ints(v)) => Some(Int(v.iter().fold(0i64, |a, &b| a.wrapping_add(b)))),
        ('+', Floats(v)) => Some(Float(v.iter().sum())),
        ('+', Bools(v)) => Some(Int(v.iter().filter(|&&b| b).count() as i64)),
        ('*', Ints(v)) => Some(Int(v.iter().fold(1i64, |a, &b| a.wrapping_mul(b)))),
        ('*', Floats(v)) => Some(Float(v.iter().product())),
        ('&', Ints(v)) => v.iter().min().map(|&m| Int(m)),
        ('&', Floats(v)) => v.iter().cloned().reduce(f64::min).map(Float),
        ('|', Ints(v)) => v.iter().max().map(|&m| Int(m)),
        ('|', Floats(v)) => v.iter().cloned().reduce(f64::max).map(Float),
        ('|', Bools(v)) => Some(Bool(v.iter().any(|&b| b))),   // any
        ('&', Bools(v)) => Some(Bool(v.iter().all(|&b| b))),   // all
        _ => None,
    }
}
pub fn scan_fast(c: char, x: &Value) -> Option<Value> {
    fn run<T: Copy>(v: &[T], f: impl Fn(T, T) -> T) -> Vec<T> {
        let mut out = Vec::with_capacity(v.len()); let mut acc = None;
        for &b in v { let a = match acc { None => b, Some(a) => f(a, b) }; acc = Some(a); out.push(a); }
        out
    }
    match (c, x) {
        ('+', Ints(v)) => Some(ints(run(v, |a, b| a.wrapping_add(b)))),
        ('+', Floats(v)) => Some(floats(run(v, |a, b| a + b))),
        ('*', Ints(v)) => Some(ints(run(v, |a, b| a.wrapping_mul(b)))),
        ('*', Floats(v)) => Some(floats(run(v, |a, b| a * b))),
        ('&', Ints(v)) => Some(ints(run(v, i64::min))),
        ('&', Floats(v)) => Some(floats(run(v, f64::min))),
        ('|', Ints(v)) => Some(ints(run(v, i64::max))),
        ('|', Floats(v)) => Some(floats(run(v, f64::max))),
        _ => None,
    }
}

macro_rules! p { ($n:literal, $m:expr, $d:expr) => { PrimDef { name: $n, m: $m, d: $d } } }
pub static PRIMS: &[PrimDef] = &[
    p!("+", Some(flip), Some(add)),
    p!("-", Some(neg), Some(sub)),
    p!("*", Some(first), Some(mul)),
    p!("%", Some(recip), Some(div)),
    p!("&", Some(where_), Some(min2)),
    p!("|", Some(reverse), Some(max2)),
    p!("<", Some(iasc), Some(lt)),
    p!(">", Some(idesc), Some(gt)),
    p!("=", Some(group), Some(eq)),
    p!("~", Some(not), Some(matches)),
    p!(",", Some(enlist), Some(join)),
    p!("#", Some(count), Some(take)),
    p!("_", Some(floor), Some(drop)),
    p!("!", Some(til), Some(dict)),
    p!("?", Some(distinct), Some(find)),
    p!("@", Some(type_), Some(index_at)),
    p!("^", Some(sqrt), Some(pow)),
    p!("$", Some(string), Some(cast)),
];
/// Only what needs Rust: IO, dict internals, transcendental math, VM-dispatched keywords, and the stage-0 oracles.
/// Everything expressible with the verbs lives in boot/prelude.nt (sum avg count first in mod vs upper ...).
pub static BUILTINS: &[PrimDef] = &[
    p!("exp", Some(exp), None), p!("log", Some(log), None), p!("sin", Some(sin), None), p!("cos", Some(cos), None), p!("tan", Some(tan), None), p!("atan", Some(atan), None),
    p!("rand", Some(rand1), Some(rand2)), p!("rseed", Some(rseed), None),
    p!("band", None, Some(band)), p!("bor", None, Some(bor)), p!("bxor", None, Some(bxor)), p!("shl", None, Some(shl)), p!("shr", None, Some(shr)), p!("bnot", Some(bnot), None),
    p!("key", Some(key), None), p!("value", Some(value), None), p!("group", Some(group), None),
    p!("isnull", Some(isnull), None), p!("now", Some(now), None),
    p!("show", Some(show), None), p!("print", Some(print), None), p!("signal", Some(signal), None), p!("exit", Some(exit), None),
    p!("read0", Some(read0), None), p!("write0", None, Some(write0)),
    p!("hopen", Some(hopen), None), p!("hclose", Some(hclose), None), p!("hsend", None, Some(hsend)), p!("hrecv", None, Some(hrecv)),
    p!("each", None, None), p!("over", None, None), p!("scan", None, None),   // adverb keywords, dispatched in the VM
    p!("lex", Some(lex_oracle), None), p!("parse", Some(parse_oracle), None), p!("compile", Some(compile_oracle), None),
    p!("exec", None, None),   // runs bytecode data; dispatched in the VM
    p!("elast", None, None),  // elast `line / `trace: where the last caught error came from; dispatched in the VM
];
/// Named dyads that parse infix like verbs: `7 mod 3`, `x in y`. Most are prelude lambdas; the parser only needs the names.
pub const INFIX: &[&str] = &["mod", "div", "xexp", "in", "within", "vs", "sv", "ss", "write0", "each", "over", "scan", "except", "inter", "union", "cross", "fill", "like",
    "rand", "msum", "mavg", "mmax", "mmin", "ema", "xbar", "bin", "cut", "rotate", "sublist",
    "band", "bor", "bxor", "shl", "shr"];

pub fn prim(c: char) -> &'static PrimDef { PRIMS.iter().find(|p| p.name.starts_with(c)).expect("known verb") }

// ---- bytecode as data
// unit  = (opcodes; args; consts; lines)     lambda code = (opcodes; args; consts; lines; params; nlocals)
// lines[i] is the source line op i came from (0 = synthetic), for runtime error positions.
// const = (`k;v) literal | (`g;`name) global name | (`p;"+") verb | (`a;"/";const) adverbed | (`f;code) lambda
fn code_data(ops: &[Op], consts: &[Value], lines: &[u32]) -> Vec<Value> {
    let mut is_name = vec![false; consts.len()];
    for op in ops { if let Op::LoadG(a) | Op::StoreG(a) | Op::TakeG(a) = op { is_name[*a as usize] = true; } }
    let (oc, oa): (Vec<i64>, Vec<i64>) = ops.iter().map(|o| o.encode()).unzip();
    vec![ints(oc), ints(oa), pack(consts.iter().zip(is_name).map(|(v, n)| const_data(v, n)).collect()),
         ints(lines.iter().map(|&l| l as i64).collect())]
}
fn const_data(v: &Value, is_name: bool) -> Value {
    let s = |n: &str| Symbol(Rc::from(n));
    pack(match v {
        _ if is_name => vec![s("g"), v.clone()],
        Prim(p) => vec![s("p"), Char(p.name.chars().next().unwrap())],
        Adv(c, f) => vec![s("a"), Char(*c), const_data(f, false)],
        Lambda(code) => {
            let mut d = code_data(&code.ops, &code.consts, &code.lines);
            d.push(pack(code.params.iter().map(|p| Symbol(Rc::from(p.as_str()))).collect()));
            d.push(Int(code.nlocals as i64));
            vec![s("f"), pack(d)]
        }
        _ => vec![s("k"), v.clone()],
    })
}
/// The Rust compiler as neant data: one unit per statement. Oracle for boot/compile.nt.
fn compile_oracle(x: Value) -> R<Value> {
    let (t, l) = crate::lex::lex_lines(&text(&x))?;
    let prog = crate::parse::Parser::with_lines(t, l).program()?;
    Ok(pack(prog.iter().map(|ast| {
        let (ops, consts, lines) = crate::compile::compile_stmt(ast)?;
        Ok(pack(code_data(&ops, &consts, &lines)))
    }).collect::<R<Vec<_>>>()?))
}
pub fn load_unit(u: &Value) -> R<(Vec<Op>, Vec<Value>, Vec<u32>)> {
    let (Ints(oc), Ints(oa)) = (u.item(0)?, u.item(1)?) else { return err("load: opcodes and args must be int vectors") };
    if oc.len() != oa.len() { return err("load: opcodes and args differ in length"); }
    let ops = oc.iter().zip(oa.iter()).map(|(&o, &a)| Op::decode(o, a)).collect::<R<Vec<_>>>()?;
    let consts = u.item(2)?.seq().iter().map(load_const).collect::<R<Vec<_>>>()?;
    // the line table is optional: hand-built bytecode passed to `exec` just gets no positions
    let lines = match u.item(3) { Ok(Ints(v)) => v.iter().map(|&l| l.max(0) as u32).collect(), _ => vec![] };
    Ok((ops, consts, lines))
}
fn load_const(e: &Value) -> R<Value> {
    let Symbol(tag) = e.item(0)? else { return err("load: const entry needs a tag") };
    let ch = |v: Value, ok: &str| match v { Char(c) if ok.contains(c) => Ok(c), _ => err("load: bad verb") };
    match &*tag {
        "k" | "g" => e.item(1),
        "p" => { let c = ch(e.item(1)?, VERBS_ALL)?; PRIMS.iter().find(|p| p.name.starts_with(c)).map(Prim).ok_or_else(|| NError("load: bad verb".into())) }
        "a" => Ok(Adv(ch(e.item(1)?, ADV_ALL)?, Rc::new(load_const(&e.item(2)?)?))),
        "f" => {
            let d = e.item(1)?;
            let (ops, consts, lines) = load_unit(&d)?;
            let params = d.item(4)?.seq().iter().map(|p| match p { Symbol(s) => Ok(s.to_string()), _ => err("load: param names must be symbols") }).collect::<R<Vec<_>>>()?;
            let nlocals = int_of(&d.item(5)?)? as usize;
            let nlocals = nlocals.max(params.len());
            Ok(Lambda(Rc::new(FnCode { ops, consts, lines, params, nlocals })))
        }
        _ => err(format!("load: unknown const tag `{tag}")),
    }
}
const VERBS_ALL: &str = "+-*%!&|<>=~,^#_$?@";
const ADV_ALL: &str = "/\\'LR";

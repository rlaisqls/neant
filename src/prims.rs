//! Primitive verbs (single chars) and named builtins. All pure: they never call back into the VM.
use crate::value::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
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
    macro_rules! push { ($a:expr, $b:expr) => { match Arc::get_mut($a) { Some(v) => { v.push($b); true } None => false } } }
    macro_rules! ext { ($a:expr, $b:expr) => { match Arc::get_mut($a) { Some(v) => { v.extend_from_slice($b); true } None => false } } }
    match (x, y) {
        (Ints(a), Int(b)) => push!(a, *b), (Ints(a), Ints(b)) => ext!(a, b),
        (Floats(a), Float(b)) => push!(a, *b), (Floats(a), Floats(b)) => ext!(a, b),
        (Bools(a), Bool(b)) => push!(a, *b), (Bools(a), Bools(b)) => ext!(a, b),
        (Chars(a), Char(b)) => push!(a, *b), (Chars(a), Chars(b)) => ext!(a, b),
        (Syms(a), Symbol(b)) => push!(a, b.clone()), (Syms(a), Syms(b)) => ext!(a, b),
        (Dates(a), Date(b)) => push!(a, *b), (Dates(a), Dates(b)) => ext!(a, b),
        (Times(a), Time(b)) => push!(a, *b), (Times(a), Times(b)) => ext!(a, b),
        (Bytes(a), Byte(b)) => push!(a, *b), (Bytes(a), Bytes(b)) => ext!(a, b),
        (List(a), b) => match Arc::get_mut(a) { Some(v) => { v.extend(b.seq()); true } None => false },
        _ => false,
    }
}
fn join(mut x: Value, y: Value) -> R<Value> {
    // `f ,x` parses as `f , x` — a name in front of a verb is that verb's left argument, so the
    // function gets joined into a two-element list and the mistake surfaces much later as a length
    // or index error. Nothing legitimate joins onto a function, so it is an error here instead.
    if x.is_fn() { return err("type: , has a function on its left — `f ,x` parses as `f , x`, so write `f (,x)`"); }
    if try_append(&mut x, &y) { return Ok(x); }
    // try_append only works when the left side's Arc is uniquely owned. When it is not — `a,a`, or
    // anything still held by a variable — the fallback used to box both sides into Vec<Value> and
    // let pack work out the type again. Two slice copies instead.
    macro_rules! cat2 { ($a:expr, $b:expr, $ctor:ident) => {{
        let mut v = Vec::with_capacity($a.len() + $b.len());
        v.extend_from_slice($a); v.extend_from_slice($b);
        return Ok($ctor(v));
    }}}
    macro_rules! cat1 { ($a:expr, $b:expr, $ctor:ident) => {{
        let mut v = Vec::with_capacity($a.len() + 1);
        v.extend_from_slice($a); v.push($b);
        return Ok($ctor(v));
    }}}
    // and the atom on the LEFT, which is not the mirror image of the one on the right: `v , atom`
    // can append in place when the Arc is unshared, `atom , v` never can, so without this arm
    // prelude's `prev` — (nullof x), -1_x — boxed the whole vector while `next` did not, and the
    // two were 100x apart for no reason a reader of either could see.
    macro_rules! cat0 { ($a:expr, $b:expr, $ctor:ident) => {{
        let mut v = Vec::with_capacity($b.len() + 1);
        v.push($a); v.extend_from_slice($b);
        return Ok($ctor(v));
    }}}
    match (&x, &y) {
        (Bool(a), Bools(b)) => cat0!(*a, b, bools),   (Int(a), Ints(b)) => cat0!(*a, b, ints),
        // a numeric atom against the other numeric vector: pack would promote these, so without
        // them `ratios` — (first x), (1_x) % -1_x, an Int in front of a Floats — took the slow road
        (Int(a), Floats(b)) => cat0!(*a as f64, b, floats),
        (Float(a), Ints(b)) => { let mut v = Vec::with_capacity(b.len() + 1); v.push(*a); v.extend(b.iter().map(|&e| e as f64)); return Ok(floats(v)); }
        (Float(a), Floats(b)) => cat0!(*a, b, floats), (Char(a), Chars(b)) => cat0!(*a, b, chars),
        (Symbol(a), Syms(b)) => cat0!(a.clone(), b, syms), (Date(a), Dates(b)) => cat0!(*a, b, dates),
        (Time(a), Times(b)) => cat0!(*a, b, times),   (Byte(a), Bytes(b)) => cat0!(*a, b, bytes),
        _ => {}
    }
    match (&x, &y) {
        (Bools(a), Bools(b)) => cat2!(a, b, bools),   (Bools(a), Bool(b)) => cat1!(a, *b, bools),
        (Ints(a), Ints(b)) => cat2!(a, b, ints),      (Ints(a), Int(b)) => cat1!(a, *b, ints),
        (Floats(a), Floats(b)) => cat2!(a, b, floats), (Floats(a), Float(b)) => cat1!(a, *b, floats),
        (Floats(a), Int(b)) => cat1!(a, *b as f64, floats),
        (Ints(a), Float(b)) => { let mut v: Vec<f64> = a.iter().map(|&e| e as f64).collect(); v.push(*b); return Ok(floats(v)); }
        (Ints(a), Floats(b)) => { let mut v: Vec<f64> = a.iter().map(|&e| e as f64).collect(); v.extend_from_slice(b); return Ok(floats(v)); }
        (Floats(a), Ints(b)) => { let mut v = a.as_ref().clone(); v.extend(b.iter().map(|&e| e as f64)); return Ok(floats(v)); }
        (Chars(a), Chars(b)) => cat2!(a, b, chars),   (Chars(a), Char(b)) => cat1!(a, *b, chars),
        (Syms(a), Syms(b)) => cat2!(a, b, syms),      (Syms(a), Symbol(b)) => cat1!(a, b.clone(), syms),
        (Dates(a), Dates(b)) => cat2!(a, b, dates),   (Dates(a), Date(b)) => cat1!(a, *b, dates),
        (Times(a), Times(b)) => cat2!(a, b, times),   (Times(a), Time(b)) => cat1!(a, *b, times),
        (Bytes(a), Bytes(b)) => cat2!(a, b, bytes),   (Bytes(a), Byte(b)) => cat1!(a, *b, bytes),
        _ => {}
    }
    let mut a = x.seq(); a.extend(y.seq()); Ok(pack(a))
}

/// The element order `n # x` wants: the first n cycling round from the front, or — for a negative n
/// — the last |n|, cycling round from the back. Whole-slice copies, never one index at a time.
fn cyc<T: Clone>(s: &[T], n: i64) -> Vec<T> {
    let ln = s.len();
    let cnt = n.unsigned_abs() as usize;
    let start = if n >= 0 { 0 } else { (ln - cnt % ln) % ln };   // where the window opens
    let mut out = Vec::with_capacity(cnt);
    if start != 0 { out.extend_from_slice(&s[start..start + (ln - start).min(cnt)]); }
    while out.len() + ln <= cnt { out.extend_from_slice(s); }
    let rest = cnt - out.len();
    out.extend_from_slice(&s[..rest]);
    out
}

/// `n # x`. A typed vector stays typed and the copy is a memcpy. This used to call `seq`, which
/// boxes every element into a Value, then build an index vector with an integer division per
/// element, then box again and let `pack` work out the type a third time — about 25x the cost, and
/// for a negative n larger than the length it indexed with a wrapped-around usize and panicked.
fn take(n: Value, x: Value) -> R<Value> {
    let n = int_of(&n)?;
    if n == 0 { return Ok(pack(vec![])); }   // 0 from anything, even empty, is just empty — no cycling needed
    // `n # atom` is a fill, and it is how every buffer in this tree is made — bnPad, the limb
    // kernels' scratch, every `k#0x00`. Cycling a one-element list through Value would box the atom,
    // clone it n times and let pack re-derive the type; `vec![b; n]` is a memset.
    macro_rules! rep { ($b:expr, $ctor:ident) => { return Ok($ctor(vec![$b; n.unsigned_abs() as usize])) } }
    match &x {
        Bool(b) => rep!(*b, bools),     Int(i) => rep!(*i, ints),   Float(f) => rep!(*f, floats),
        Char(c) => rep!(*c, chars),     Symbol(y) => rep!(y.clone(), syms), Date(d) => rep!(*d, dates),
        Time(t) => rep!(*t, times),     Byte(b) => rep!(*b, bytes),
        _ => {}
    }
    macro_rules! tk { ($v:expr, $ctor:ident) => {{
        if $v.is_empty() { return err("take from empty"); }
        return Ok($ctor(cyc($v, n)));
    }}}
    match &x {
        Bools(v) => tk!(v, bools), Ints(v) => tk!(v, ints),   Floats(v) => tk!(v, floats),
        Chars(v) => tk!(v, chars), Syms(v) => tk!(v, syms),   Dates(v) => tk!(v, dates),
        Times(v) => tk!(v, times), Bytes(v) => tk!(v, bytes), List(v) => tk!(v, list),
        _ => {}
    }
    let s = x.seq();
    if s.is_empty() { return err("take from empty"); }
    Ok(pack(cyc(&s, n)))
}

/// `n _ x`, the same way: a slice of the typed vector rather than a round trip through Value.
fn drop(n: Value, x: Value) -> R<Value> {
    let n = int_of(&n)?;
    macro_rules! dr { ($v:expr, $ctor:ident) => {{
        let len = $v.len() as i64;
        let (a, b) = if n >= 0 { (n.min(len), len) } else { (0, (len + n).max(0)) };
        return Ok($ctor($v[a as usize..b as usize].to_vec()));
    }}}
    match &x {
        Bools(v) => dr!(v, bools), Ints(v) => dr!(v, ints),   Floats(v) => dr!(v, floats),
        Chars(v) => dr!(v, chars), Syms(v) => dr!(v, syms),   Dates(v) => dr!(v, dates),
        Times(v) => dr!(v, times), Bytes(v) => dr!(v, bytes), List(v) => dr!(v, list),
        _ => {}
    }
    let s = x.seq(); let len = s.len() as i64;
    let (a, b) = if n >= 0 { (n.min(len), len) } else { (0, (len + n).max(0)) };
    Ok(pack(s[a as usize..b as usize].to_vec()))
}
fn dict(k: Value, v: Value) -> R<Value> {
    let vc = if is_table(&v) { v.item(0).map(|c| c.count()).unwrap_or(0) } else { v.count() };   // keyed table: one key per row
    if k.count() != vc { return err("length: dict"); }
    Ok(Dict(Arc::new(crate::value::Dict { keys: k, vals: v })))
}
/// Hashable form of an atom, so find/distinct/group are O(n). Lists and nested values fall back to a linear scan.
#[derive(Hash, PartialEq, Eq)]
enum Key { N, B(bool), I(i64), F(u64), C(char), S(Arc<str>), D(i32), T(i64), Y(u8) }
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
    // both sides the same kind of typed vector: look the raw elements up directly. The `small`
    // heuristic below is kept — for a handful of lookups a scan beats building a table at all.
    macro_rules! fnd { ($xv:expr, $iv:expr) => {{
        let n = $xv.len() as i64;
        if $iv.len() * $xv.len() < 1 << 14 {
            return Ok(ints($iv.iter().map(|e| $xv.iter().position(|q| q == e).map_or(n, |p| p as i64)).collect()));
        }
        let mut m: HashMap<_, i64> = HashMap::with_capacity($xv.len());
        for (p, e) in $xv.iter().enumerate() { m.entry(e.clone()).or_insert(p as i64); }
        return Ok(ints($iv.iter().map(|e| m.get(e).copied().unwrap_or(n)).collect()));
    }}}
    match (&x, &i) {
        (Ints(a), Ints(b)) => fnd!(a, b),     (Bools(a), Bools(b)) => fnd!(a, b),
        (Chars(a), Chars(b)) => fnd!(a, b),   (Syms(a), Syms(b)) => fnd!(a, b),
        (Dates(a), Dates(b)) => fnd!(a, b),   (Times(a), Times(b)) => fnd!(a, b),
        (Bytes(a), Bytes(b)) => fnd!(a, b),
        _ => {}
    }
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
    Ok(Dict(Arc::new(crate::value::Dict { keys: d.keys.clone(), vals: pack(d.vals.seq().iter().map(|c| c.item(i)).collect::<R<Vec<_>>>()?) })))
}
pub fn index_at(x: Value, i: Value) -> R<Value> {
    if let Dict(d) = &x {
        if is_table(&x) && matches!(i, Int(_) | Ints(_) | Bool(_) | Bools(_)) {   // t[2] row, t[0 2] rows
            let n = d.vals.item(0)?.count();
            if i.is_atom() { return row_at(&x, usize::try_from(int_of(&i)?).ok().filter(|&p| p < n).ok_or_else(|| NError("index".into()))?); }
            return Ok(Dict(Arc::new(crate::value::Dict { keys: d.keys.clone(), vals: list(d.vals.seq().into_iter().map(|c| index_at(c, i.clone())).collect::<R<Vec<_>>>()?) })));
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
    if let (Ints(j), false) = (&i, x.is_atom()) { if let Some(r) = gather(&x, j) { return r; } }
    Ok(pack(i.seq().iter().map(|j| index_at(x.clone(), j.clone())).collect::<R<Vec<_>>>()?))
}
/// `x[i]` for a typed vector indexed by an int vector: gather straight into the same typed vector.
/// The general path boxes every element into a `Value`, indexes it, then re-detects the type in `pack`;
/// this is the same answer without any of that. `List` still goes through `pack`, which may retype it.
fn gather(x: &Value, idx: &[i64]) -> Option<R<Value>> {
    macro_rules! g { ($v:expr, $ctor:expr) => {{
        let (src, n) = ($v, $v.len() as i64);
        let mut out = Vec::with_capacity(idx.len());
        for &j in idx {
            if j < 0 || j >= n { return Some(err("index")); }
            out.push(src[j as usize].clone());
        }
        Some(Ok($ctor(out)))
    }}}
    match x {
        Ints(v) => g!(v, ints), Floats(v) => g!(v, floats), Bools(v) => g!(v, bools), Chars(v) => g!(v, chars),
        Syms(v) => g!(v, syms), Dates(v) => g!(v, dates), Times(v) => g!(v, times), Bytes(v) => g!(v, bytes),
        List(v) => g!(v, pack),
        _ => None,
    }
}
fn empty_like(x: &Value) -> Value {
    match x {
        Chars(_) | Char(_) => chars(vec![]), Floats(_) => floats(vec![]), Bools(_) => bools(vec![]), Syms(_) => syms(vec![]),
        Dates(_) => dates(vec![]), Times(_) => times(vec![]), Bytes(_) | Byte(_) => bytes(vec![]), List(_) => list(vec![]), _ => ints(vec![]),
    }
}
/// `=x` group: distinct items -> indices where they occur, in first-seen order.
/// `group x` -> the distinct values against the indices where each occurs, the same way.
fn group(x: Value) -> R<Value> {
    macro_rules! grp { ($v:expr, $ctor:ident) => {{
        let mut at: HashMap<_, usize> = HashMap::with_capacity($v.len());
        let (mut keys, mut idx): (Vec<_>, Vec<Vec<i64>>) = (Vec::new(), Vec::new());
        for (i, e) in $v.iter().enumerate() {
            match at.get(e) {
                Some(&p) => idx[p].push(i as i64),
                None => { at.insert(e.clone(), keys.len()); keys.push(e.clone()); idx.push(vec![i as i64]); }
            }
        }
        return dict($ctor(keys), list(idx.into_iter().map(ints).collect()));
    }}}
    match &x {
        Ints(v) => grp!(v, ints), Bools(v) => grp!(v, bools), Chars(v) => grp!(v, chars),
        Syms(v) => grp!(v, syms), Dates(v) => grp!(v, dates), Times(v) => grp!(v, times),
        Bytes(v) => grp!(v, bytes),
        _ => {}
    }
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
/// Reversing a typed vector is a copy and a reverse in place, not a trip through Value: `seq` boxes
/// every element and `pack` then has to work the type out again, which cost about 17x.
fn reverse(x: Value) -> R<Value> {
    macro_rules! rv { ($v:expr, $ctor:ident) => {{ let mut v = $v.as_ref().clone(); v.reverse(); return Ok($ctor(v)); }} }
    match &x {
        Bools(v) => rv!(v, bools), Ints(v) => rv!(v, ints),   Floats(v) => rv!(v, floats),
        Chars(v) => rv!(v, chars), Syms(v) => rv!(v, syms),   Dates(v) => rv!(v, dates),
        Times(v) => rv!(v, times), Bytes(v) => rv!(v, bytes), List(v) => rv!(v, list),
        _ => {}
    }
    let mut s = x.seq(); s.reverse(); Ok(pack(s))
}
/// `<x` and `>x`, which `sort`, `asc` and `desc` are all written in terms of.
///
/// Going through Value boxes every element and compares with cmp_val, which promotes both sides to
/// f64 for anything numeric — an indirection and two conversions for each of the roughly seventeen
/// comparisons an element takes at 100k. The typed paths sort (value, index) pairs directly, which
/// also keeps the comparison in one cache-friendly array instead of chasing indices into another.
///
/// THE ORDER IS UNCHANGED in two respects that matter. The sort is still stable — an index
/// permutation whose ties do not keep their input order is a different function — which is why the
/// descending arm compares the value backwards and the index forwards rather than reversing. And a
/// null still sorts first: the int null is i64::MIN, which is least under Ord exactly as it was
/// least as an f64, and the float null is a NaN, handled by hand below as cmp_val handled it.
///
/// One thing does change, and it is a fix: two ints above 2^53 that differ used to compare EQUAL,
/// because the f64 they were promoted to was the same. They now sort apart.
fn grade(x: Value, desc: bool) -> R<Value> {
    macro_rules! gr { ($v:expr) => {{
        let mut p: Vec<(_, i64)> = $v.iter().enumerate().map(|(i, e)| (e.clone(), i as i64)).collect();
        if desc { p.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1))); } else { p.sort_unstable(); }
        return Ok(ints(p.into_iter().map(|(_, i)| i).collect()));
    }}}
    match &x {
        Ints(v) => gr!(v), Bools(v) => gr!(v), Chars(v) => gr!(v),
        Syms(v) => gr!(v), Dates(v) => gr!(v), Times(v) => gr!(v), Bytes(v) => gr!(v),
        Floats(v) => {
            let mut idx: Vec<i64> = (0..v.len() as i64).collect();
            let c = |a: f64, b: f64| a.partial_cmp(&b).unwrap_or_else(|| b.is_nan().cmp(&a.is_nan()));
            if desc { idx.sort_by(|&a, &b| c(v[b as usize], v[a as usize])); }
            else { idx.sort_by(|&a, &b| c(v[a as usize], v[b as usize])); }
            return Ok(ints(idx));
        }
        _ => {}
    }
    let s = x.seq(); let mut idx: Vec<usize> = (0..s.len()).collect();
    idx.sort_by(|&a, &b| if desc { cmp_val(&s[b], &s[a]) } else { cmp_val(&s[a], &s[b]) });
    Ok(ints(idx.into_iter().map(|i| i as i64).collect()))
}
fn iasc(x: Value) -> R<Value> { grade(x, false) }
fn idesc(x: Value) -> R<Value> { grade(x, true) }
/// `not x`. The integer shapes answer directly: sh_f would promote every i64 to an f64 first, which
/// is a whole extra pass and allocation to reach the same answer.
fn not(x: Value) -> R<Value> {
    match &x {
        Bools(v) => return Ok(bools(v.iter().map(|&b| !b).collect())),
        Ints(v) => return Ok(bools(v.iter().map(|&a| a == 0).collect())),
        Floats(v) => return Ok(bools(v.iter().map(|&a| a == 0.0).collect())),
        Bytes(v) => return Ok(bools(v.iter().map(|&b| b == 0).collect())),
        Bool(b) => return Ok(Bool(!*b)), Int(a) => return Ok(Bool(*a == 0)), Byte(b) => return Ok(Bool(*b == 0)),
        _ => {}
    }
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
/// `distinct x`. The typed paths hash the raw elements; the general one boxes each into a Value and
/// then builds a Key from it, which is two allocations' worth of work per element to answer a
/// question about an i64. `in`, `except`, `inter` and `union` in prelude.nt are all written in terms
/// of this and `?`, so they follow.
fn distinct(x: Value) -> R<Value> {
    macro_rules! dis { ($v:expr, $ctor:ident) => {{
        let mut seen = std::collections::HashSet::with_capacity($v.len());
        let mut out = Vec::new();
        for e in $v.iter() { if seen.insert(e.clone()) { out.push(e.clone()); } }
        return Ok($ctor(out));
    }}}
    match &x {
        Ints(v) => dis!(v, ints), Bools(v) => dis!(v, bools), Chars(v) => dis!(v, chars),
        Syms(v) => dis!(v, syms), Dates(v) => dis!(v, dates), Times(v) => dis!(v, times),
        Bytes(v) => dis!(v, bytes),
        _ => {}
    }
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
    Ok(Symbol(Arc::from(match x {
        Null => "null", Bool(_) => "bool", Int(_) => "int", Float(_) => "float", Char(_) => "char", Symbol(_) => "sym",
        Bools(_) => "bools", Ints(_) => "ints", Floats(_) => "floats", Chars(_) => "chars", Syms(_) => "syms",
        Date(_) => "date", Time(_) => "time", Dates(_) => "dates", Times(_) => "times", Byte(_) => "byte", Bytes(_) => "bytes",
        List(_) => "list", Dict(_) => "dict", Lambda(_) | Prim(_) | Adv(..) | Proj(..) | Closure(..) => "fn",
        Shared(_) => "shared", Thread(_) => "thread",
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
fn sym(x: Value) -> R<Value> { Ok(Symbol(Arc::from(to_chars(&x).into_iter().collect::<String>()))) }
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
/// `hrecv` for stdin: up to n bytes, as Bytes, empty at EOF. `read0 0` reads stdin to EOF and
/// splits it into lines, which cannot do the incremental framed reads a protocol speaking over
/// stdin needs (`lspServe`, src/neant/tools/lsp.nt) — that is the whole of why this exists.
fn read1(x: Value) -> R<Value> {
    let n = int_of(&x)?.max(0) as usize;
    let mut buf = vec![0u8; n];
    let got = std::io::Read::read(&mut std::io::stdin(), &mut buf).map_err(|e| NError(format!("read1: {e}")))?;
    buf.truncate(got);
    Ok(bytes(buf))
}
fn read0(x: Value) -> R<Value> {
    if let Int(0) = x {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s).map_err(|e| NError(e.to_string()))?;
        return Ok(lines(s));
    }
    let p = text(&x);
    Ok(lines(std::fs::read_to_string(&p).map_err(|e| NError(format!("{p}: {e}")))?))
}
// ---- TCP sockets. Handles are ints into a process-local table. One global table, not a
// thread_local: `accept`'s whole point is handing a connection handle to a `spawn`ed worker
// (src/vm.rs), which runs on a different OS thread and needs to resolve that same handle.
enum Sock { Stream(std::net::TcpStream), Listener(std::net::TcpListener) }
static SOCKS: std::sync::LazyLock<std::sync::Mutex<(i64, HashMap<i64, Sock>)>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new((0, HashMap::new())));
// `hsend`/`hrecv`/`accept` all block on real I/O; holding SOCKS locked across that would
// serialize every socket in the process behind whichever one happens to be waiting (exactly the
// concurrency an accept-loop-plus-spawn server needs not to have). So these clone the underlying
// fd (`try_clone`, a plain OS-level dup — reads/writes on the clone hit the same kernel socket)
// and block on the clone, holding the lock only for the lookup and the clone itself.
fn sock<T>(h: &Value, f: impl FnOnce(&mut std::net::TcpStream) -> R<T>) -> R<T> {
    let h = int_of(h)?;
    let mut s = match SOCKS.lock().unwrap().1.get(&h) {
        Some(Sock::Stream(s)) => s.try_clone().map_err(|e| NError(format!("hsock: {e}")))?,
        Some(Sock::Listener(_)) => return err(format!("hsock: {h} is a listener, not a connection")),
        None => return err(format!("hsock: no handle {h}")),
    };
    f(&mut s)
}
fn listener<T>(h: &Value, f: impl FnOnce(&std::net::TcpListener) -> R<T>) -> R<T> {
    let h = int_of(h)?;
    let l = match SOCKS.lock().unwrap().1.get(&h) {
        Some(Sock::Listener(l)) => l.try_clone().map_err(|e| NError(format!("hsock: {e}")))?,
        Some(Sock::Stream(_)) => return err(format!("hsock: {h} is a connection, not a listener")),
        None => return err(format!("hsock: no handle {h}")),
    };
    f(&l)
}
fn new_handle(s: Sock) -> Value {
    let mut c = SOCKS.lock().unwrap();
    c.0 += 1; let h = c.0; c.1.insert(h, s); Int(h)
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
    Ok(new_handle(Sock::Stream(s)))
}
/// `hlisten "host:port"` -> handle; `accept` blocks on it for the next inbound connection.
fn hlisten(x: Value) -> R<Value> {
    let addr = text(&x);
    let l = std::net::TcpListener::bind(&addr).map_err(|e| NError(format!("hlisten {addr}: {e}")))?;
    Ok(new_handle(Sock::Listener(l)))
}
/// `accept h` blocks for the next inbound connection on listener `h` and returns a connection
/// handle — from there it's an ordinary handle, same as one from `hopen`: `hsend`/`hrecv`/`hclose`
/// all just work, and it can be captured into a `spawn`ed closure to hand the connection off.
fn accept(x: Value) -> R<Value> {
    let (s, _) = listener(&x, |l| l.accept().map_err(|e| NError(format!("accept: {e}"))))?;
    s.set_nodelay(true).ok();
    Ok(new_handle(Sock::Stream(s)))
}
fn hclose(x: Value) -> R<Value> {
    let h = int_of(&x)?;
    SOCKS.lock().unwrap().1.remove(&h);
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

/// `shared x` wraps x in a mutable cell: the one value that is not lock-free COW, for state
/// that is genuinely meant to be shared and mutated across `spawn`ed threads. Everything else
/// stays a plain value, safe to pass to a spawned thread by ordinary (cheap, Arc-refcounted) clone.
fn shared(x: Value) -> R<Value> { Ok(Shared(Arc::new(Mutex::new(x)))) }
fn sget(x: Value) -> R<Value> {
    match x { Shared(c) => Ok(c.lock().unwrap().clone()), _ => err("type: sget expected a shared cell") }
}
/// `sset[s;v]` locks, overwrites, and returns v. A plain read-then-write built from `sget`/`sset`
/// in neant would race; `supd` (in the VM, since it calls back into a lambda) is the atomic one.
fn sset(s: Value, v: Value) -> R<Value> {
    match s { Shared(c) => { *c.lock().unwrap() = v.clone(); Ok(v) } _ => err("type: sset expected a shared cell") }
}

fn write0(path: Value, x: Value) -> R<Value> {
    let p = text(&path);
    let body = match &x { List(items) => items.iter().map(text).collect::<Vec<_>>().join("\n") + "\n", _ => text(&x) };
    std::fs::write(&p, body).map_err(|e| NError(format!("{p}: {e}")))?;
    Ok(Null)
}
/// `hsend` for stdout: the bytes exactly as given, no trailing newline, flushed. `print` appends a
/// newline and `write0 "/dev/stdout"` truncates on every call (so incremental writes overwrite each
/// other), neither of which a byte-counted protocol on stdout can use — see `read1`.
fn write1(x: Value) -> R<Value> {
    use std::io::Write;
    let b: Vec<u8> = match &x { Bytes(v) => v.to_vec(), _ => text(&x).into_bytes() };
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(&b); let _ = o.flush();
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
/// A float's raw IEEE bit pattern as an int — not a numeric cast (which truncates), a
/// reinterpretation, e.g. for the tracing JIT's codegen (`src/neant/jit/arm64.nt`) to load a
/// float constant's bits via the same integer-only `jitLoadConst` used everywhere else, then
/// `FMOV` them into a `D` register.
fn fbits(x: Value) -> R<Value> {
    match x { Float(f) => Ok(Int(f.to_bits() as i64)), _ => err("type: fbits on non-float") }
}
/// null per item: 0N 0n ` " " :: and null date/time
fn isnull(x: Value) -> R<Value> {
    fn one(v: &Value) -> bool {
        match v { Null => true, Int(i) => *i == NI, Float(f) => f.is_nan(), Symbol(s) => s.is_empty(), Char(c) => *c == ' ', Date(d) => *d == i32::MIN, Time(t) => *t == NI, _ => false }
    }
    // typed vectors compare the raw element against their own null; going through `one` means
    // boxing each one into a Value first, and `fill` and `fills` call this on every vector they see
    match &x {
        Ints(v) => return Ok(bools(v.iter().map(|&i| i == NI).collect())),
        Floats(v) => return Ok(bools(v.iter().map(|f| f.is_nan()).collect())),
        Syms(v) => return Ok(bools(v.iter().map(|s| s.is_empty()).collect())),
        Chars(v) => return Ok(bools(v.iter().map(|&c| c == ' ').collect())),
        Dates(v) => return Ok(bools(v.iter().map(|&d| d == i32::MIN).collect())),
        Times(v) => return Ok(bools(v.iter().map(|&t| t == NI).collect())),
        Bools(v) => return Ok(bools(vec![false; v.len()])),
        Bytes(v) => return Ok(bools(vec![false; v.len()])),
        _ => {}
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

/// `x[i;j]:v`: amend along a path. All but the last index must be atoms.
pub fn amend_path(x: &mut Value, idx: &[Value], v: Value) -> R<()> {
    if idx.len() == 1 { return amend(x, idx[0].clone(), v); }
    if !idx[0].is_atom() { return err("nyi: deep amend needs atom indices before the last"); }
    let mut sub = index_at(x.clone(), idx[0].clone())?;   // ponytail: clones the sub-vector once per deep amend
    amend_path(&mut sub, &idx[1..], v)?;
    amend(x, idx[0].clone(), sub)
}
/// `x[i]:v` in place. Vector index amends each; a missing dict key appends.
/// `x[i]: v` for a typed vector written through an int vector with values of its own type — the shape
/// `acc[i+til n] +: ...` that the field arithmetic in src/neant/crypto/crypto.nt is built out of. Writes in place
/// instead of boxing every index and value and recursing once per element. Returns false (no writes yet)
/// whenever anything does not line up, so the general path below still defines the semantics — including
/// the partial mutation it performs when an index is out of range.
fn scatter(x: &mut Value, idx: &[i64], v: &Value) -> bool {
    macro_rules! s { ($r:expr, $one:pat => $get:expr, $many:pat => $src:expr) => {{
        let n = $r.len() as i64;
        if idx.iter().any(|&j| j < 0 || j >= n) { return false; }
        match v {
            $one => { let a = $get; let t = Arc::make_mut($r); for &j in idx { t[j as usize] = a.clone(); } true }
            $many => {
                let src = $src;
                if src.len() != idx.len() { return false; }
                let t = Arc::make_mut($r);
                for (&j, a) in idx.iter().zip(src.iter()) { t[j as usize] = a.clone(); }
                true
            }
            _ => false,
        }
    }}}
    match x {
        Ints(r) => s!(r, Int(a) => *a, Ints(w) => w),
        Floats(r) => s!(r, Float(a) => *a, Floats(w) => w),
        Bytes(r) => s!(r, Byte(a) => *a, Bytes(w) => w),
        Bools(r) => s!(r, Bool(a) => *a, Bools(w) => w),
        Chars(r) => s!(r, Char(a) => *a, Chars(w) => w),
        _ => false,
    }
}
pub fn amend(x: &mut Value, i: Value, v: Value) -> R<()> {
    if !i.is_atom() {
        if let Ints(j) = &i { if scatter(x, j, &v) { return Ok(()); } }
        let idx = i.seq();
        let vs = if v.is_atom() { vec![v; idx.len()] } else { v.seq() };
        if vs.len() != idx.len() { return err("length: amend"); }
        for (k, val) in idx.into_iter().zip(vs) { amend(x, k, val)?; }
        return Ok(());
    }
    if let Dict(d) = x {
        let dm = Arc::make_mut(d);
        return match dm.keys.seq().iter().position(|e| *e == i) {
            Some(p) => amend(&mut dm.vals, Int(p as i64), v),
            // enlist, not join: a vector value is one entry, so `d[`c]: 10 20` does not splice into vals
            None => { dm.keys = join(dm.keys.clone(), i)?; dm.vals = join(dm.vals.clone(), enlist(v)?)?; Ok(()) }
        };
    }
    let p = usize::try_from(int_of(&i)?).map_err(|_| NError("index".into()))?;
    if x.is_atom() || p >= x.count() { return err("index: amend out of range"); }
    let typed = match (&mut *x, &v) {
        (Ints(r), Int(a)) => { Arc::make_mut(r)[p] = *a; true }
        (Floats(r), Float(a)) => { Arc::make_mut(r)[p] = *a; true }
        (Floats(r), Int(a)) => { Arc::make_mut(r)[p] = *a as f64; true }
        (Bools(r), Bool(a)) => { Arc::make_mut(r)[p] = *a; true }
        (Chars(r), Char(a)) => { Arc::make_mut(r)[p] = *a; true }
        (Bytes(r), Byte(a)) => { Arc::make_mut(r)[p] = *a; true }
        (Syms(r), Symbol(a)) => { Arc::make_mut(r)[p] = a.clone(); true }
        (Dates(r), Date(a)) => { Arc::make_mut(r)[p] = *a; true }
        (Times(r), Time(a)) => { Arc::make_mut(r)[p] = *a; true }
        (List(r), _) => { Arc::make_mut(r)[p] = v.clone(); true }
        _ => false,
    };
    if !typed { let mut s = x.seq(); s[p] = v; *x = pack(s); }   // type widens to a general list
    Ok(())
}
fn show(x: Value) -> R<Value> { out(&x.fmt()); Ok(Null) }
/// What `show` would print, as a string. The one thing `$` cannot give: `$` casts elementwise
/// ("," sv string 1 2 3), this is the whole value the way the REPL renders it — so tests/lang.nt
/// can pin display forms (2f, ,`a, 101b) from neant instead of from Rust.
fn repr(x: Value) -> R<Value> { Ok(chars(x.fmt().chars().collect())) }
/// The bytecode-as-data the JIT's neant codegen is handed for this lambda — `FnCode::jit_input`,
/// which until now only src/jit.rs could reach. It exists so that the constant-time checker in
/// src/neant/jit/arm64.nt can be pointed at a function from neant, in a test or at a REPL, rather
/// than only from inside the compile path; nothing else about it is new.
fn jitinput(x: Value) -> R<Value> {
    match &x { Lambda(c) => Ok(c.jit_input()), Closure(c, _) => Ok(c.jit_input()),
               _ => err("jitinput: not a lambda") }
}
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
// named so the VM can reach the same function for two int atoms (PrimDef::ib) as bitop does for vectors
pub fn ib_add(a: u64, b: u64) -> u64 { a.wrapping_add(b) }
pub fn ib_and(a: u64, b: u64) -> u64 { a & b }
pub fn ib_or(a: u64, b: u64) -> u64 { a | b }
pub fn ib_xor(a: u64, b: u64) -> u64 { a ^ b }
pub fn ib_shl(a: u64, n: u64) -> u64 { a.checked_shl(n as u32).unwrap_or(0) }
pub fn ib_shr(a: u64, n: u64) -> u64 { a.checked_shr(n as u32).unwrap_or(0) }
/// `+` on raw 64-bit patterns: no int-null special case, so a word may be any bit pattern —
/// what SHA-512 and anything else working in u64 needs, since `1 shl 63` is 0N to `+ - *`.
fn badd(x: Value, y: Value) -> R<Value> { bitop(x, y, ib_add) }
fn band(x: Value, y: Value) -> R<Value> { bitop(x, y, ib_and) }
fn bor(x: Value, y: Value) -> R<Value> { bitop(x, y, ib_or) }
fn bxor(x: Value, y: Value) -> R<Value> { bitop(x, y, ib_xor) }
fn shl(x: Value, y: Value) -> R<Value> { bitop(x, y, ib_shl) }
fn shr(x: Value, y: Value) -> R<Value> { bitop(x, y, ib_shr) }
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
/// `urand n`: n bytes from the OS. `rand` is a reproducible PRNG seeded from the clock — fine for
/// sampling, never for a key. /dev/urandom does not block once the pool is up, which on any system
/// that can open a socket it is.
fn urand(x: Value) -> R<Value> {
    use std::io::Read;
    let n = int_of(&x)?;
    if n < 0 { return err("urand: negative count"); }
    let mut buf = vec![0u8; n as usize];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .map_err(|e| NError(format!("urand: /dev/urandom: {e}")))?;
    Ok(bytes(buf))
}
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

macro_rules! p {
    ($n:literal, $m:expr, $d:expr) => { PrimDef { name: $n, m: $m, d: $d, ib: None } };
    ($n:literal, $m:expr, $d:expr, $b:expr) => { PrimDef { name: $n, m: $m, d: $d, ib: Some($b) } };
}
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
/// Only what needs Rust: IO, dict internals, transcendental math, and the VM-dispatched keywords.
/// Everything expressible with the verbs lives in src/neant/stdlib/prelude.nt (sum avg count first in mod vs upper ...).
pub static BUILTINS: &[PrimDef] = &[
    p!("exp", Some(exp), None), p!("log", Some(log), None), p!("sin", Some(sin), None), p!("cos", Some(cos), None), p!("tan", Some(tan), None), p!("atan", Some(atan), None),
    p!("rand", Some(rand1), Some(rand2)), p!("rseed", Some(rseed), None), p!("urand", Some(urand), None),
    p!("badd", None, Some(badd), ib_add), p!("band", None, Some(band), ib_and), p!("bor", None, Some(bor), ib_or), p!("bxor", None, Some(bxor), ib_xor),
    p!("shl", None, Some(shl), ib_shl), p!("shr", None, Some(shr), ib_shr), p!("bnot", Some(bnot), None),
    p!("key", Some(key), None), p!("value", Some(value), None), p!("group", Some(group), None),
    p!("isnull", Some(isnull), None), p!("now", Some(now), None), p!("fbits", Some(fbits), None),
    p!("show", Some(show), None), p!("repr", Some(repr), None), p!("jitinput", Some(jitinput), None), p!("print", Some(print), None), p!("signal", Some(signal), None), p!("exit", Some(exit), None),
    p!("read0", Some(read0), None), p!("read1", Some(read1), None), p!("write0", None, Some(write0)), p!("write1", Some(write1), None),
    p!("hopen", Some(hopen), None), p!("hclose", Some(hclose), None), p!("hsend", None, Some(hsend)), p!("hrecv", None, Some(hrecv)),
    p!("hlisten", Some(hlisten), None), p!("accept", Some(accept), None),
    p!("shared", Some(shared), None), p!("sget", Some(sget), None), p!("sset", None, Some(sset)),
    p!("each", None, None), p!("over", None, None), p!("scan", None, None),   // adverb keywords, dispatched in the VM
    p!("exec", None, None),   // runs bytecode data; dispatched in the VM
    p!("jitct", None, None),  // the constant-time check; dispatched in the VM, which owns the trampolines
    p!("elast", None, None),  // elast `line / `trace: where the last caught error came from; dispatched in the VM
    p!("spawn", None, None), p!("join", None, None), p!("supd", None, None),  // concurrency: dispatched in the VM (call back into user code)
];
// ---- bytecode as data
// unit  = (opcodes; args; consts; lines)     lambda code = (opcodes; args; consts; lines; params; nlocals)
// lines[i] is the source line op i came from (0 = synthetic), for runtime error positions.
// const = (`k;v) literal | (`g;`name) global name | (`p;"+") verb | (`a;"/";const) adverbed | (`f;code) lambda
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
        "a" => Ok(Adv(ch(e.item(1)?, ADV_ALL)?, Arc::new(load_const(&e.item(2)?)?))),
        "f" => {
            let d = e.item(1)?;
            let (ops, consts, lines) = load_unit(&d)?;
            let params = d.item(4)?.seq().iter().map(|p| match p { Symbol(s) => Ok(s.to_string()), _ => err("load: param names must be symbols") }).collect::<R<Vec<_>>>()?;
            let nlocals = int_of(&d.item(5)?)? as usize;
            let nlocals = nlocals.max(params.len());
            Ok(Lambda(Arc::new(FnCode::new(ops, consts, lines, params, nlocals))))
        }
        _ => err(format!("load: unknown const tag `{tag}")),
    }
}
const VERBS_ALL: &str = "+-*%!&|<>=~,^#_$?@";
const ADV_ALL: &str = "/\\'LR";

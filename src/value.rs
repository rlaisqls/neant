//! Values, bytecode, and the numeric kernel (typed vectors, broadcasting).
use std::borrow::Cow;
use std::cmp::Ordering;
use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub type R<T> = Result<T, NError>;

#[derive(Debug, Clone)]
pub struct NError(pub String);
pub fn err<T>(s: impl Into<String>) -> R<T> { Err(NError(s.into())) }

#[derive(Clone, Copy, Debug)]
pub enum Op {
    Push(u32), LoadG(u32), StoreG(u32), LoadL(u32), StoreL(u32),
    Monad(u32), Dyad(u32), Call(u32), MkAdv(char),
    Jmp(u32), Jmpf(u32), Pop, List(u32),
    TakeL(u32), TakeG(u32), Amend(u32),  // x[i;j]:v — Take moves the variable out so Amend(depth) can mutate in place
    Ret,
    MkClosure(u32),  // pops a lambda and n captured outer locals -> Closure (capture by value)
    Loop(u32),       // do[n;..]: counter on stack; >0 -> decrement and fall through, else pop and jump
}

impl Op {
    /// Bytecode-as-data form: (opcode; arg). This numbering is the loader's contract with
    /// boot/compile.nt's OPS table — the compiler emits these ints, `load_unit` turns them back into ops.
    pub fn decode(o: i64, a: i64) -> R<Op> {
        let u = a as u32;
        Ok(match o {
            0 => Op::Push(u), 1 => Op::LoadG(u), 2 => Op::StoreG(u), 3 => Op::LoadL(u), 4 => Op::StoreL(u),
            5 => Op::Monad(u), 6 => Op::Dyad(u), 7 => Op::Call(u), 8 => Op::MkAdv(char::from_u32(u).unwrap_or('?')),
            9 => Op::Jmp(u), 10 => Op::Jmpf(u), 11 => Op::Pop, 12 => Op::List(u), 13 => Op::TakeL(u), 14 => Op::TakeG(u),
            15 => Op::Amend(u), 16 => Op::Ret, 17 => Op::MkClosure(u), 18 => Op::Loop(u),
            _ => return err(format!("load: unknown opcode {o}")),
        })
    }
}

/// `lines[i]` is the source line op `i` came from (0 = synthetic), for runtime error positions.
pub struct FnCode {
    pub ops: Vec<Op>, pub consts: Vec<Value>, pub lines: Vec<u32>, pub params: Vec<String>, pub nlocals: usize,
    calls: AtomicU32, jit: std::sync::OnceLock<Option<Arc<crate::jit::Compiled>>>,
}
/// After this many calls, try once to compile — and remember whether that succeeded, so a
/// function that didn't qualify isn't re-walked on every later call.
const JIT_THRESHOLD: u32 = 64;

impl FnCode {
    pub fn new(ops: Vec<Op>, consts: Vec<Value>, lines: Vec<u32>, params: Vec<String>, nlocals: usize) -> FnCode {
        FnCode { ops, consts, lines, params, nlocals, calls: AtomicU32::new(0), jit: std::sync::OnceLock::new() }
    }
    /// The compiled version, attempting compilation once the call count crosses the threshold.
    /// `None` means "run it on the bytecode interpreter", whether because it's still cold, because
    /// it doesn't qualify (see src/jit.rs's compilability check), or because this arch has no backend.
    pub fn jitted(self: &Arc<FnCode>) -> Option<Arc<crate::jit::Compiled>> {
        if self.calls.fetch_add(1, AtomicOrdering::Relaxed) + 1 < JIT_THRESHOLD { return None; }
        self.jit_now()
    }
    /// Same cache as `jitted`, but without the call-count gate: a compiled function calling this
    /// one directly (src/jit.rs's `jit_call` trampoline) needs to know *immediately* whether the
    /// callee is equally pure, since that's what makes it safe to call at all (see README "Stage
    /// 2" and the trampoline's doc comment) — it can't wait for this callee's own count to warm up.
    pub(crate) fn jit_for_call(self: &Arc<FnCode>) -> Option<Arc<crate::jit::Compiled>> { self.jit_now() }
    /// Built once, read many times lock-free after that: `OnceLock` gives every call after the
    /// first a plain atomic load instead of a mutex lock — this is called on every single
    /// recursive step through `jit_call` (src/jit.rs), so that difference is the whole point.
    /// Compilation is still attempted at most once (whichever caller gets here first wins;
    /// `OnceLock` itself serializes a same-time race, so no attempt is ever wasted or repeated).
    fn jit_now(self: &Arc<FnCode>) -> Option<Arc<crate::jit::Compiled>> {
        self.jit.get_or_init(|| crate::jit::compile(self).map(Arc::new)).clone()
    }
}

pub struct PrimDef {
    pub name: &'static str,
    pub m: Option<fn(Value) -> R<Value>>,
    pub d: Option<fn(Value, Value) -> R<Value>>,
    /// Two int atoms, straight through. The bit verbs work on the raw 64-bit pattern (nulls and all,
    /// like `prims::bitop`), so the VM can answer them without the shape/broadcast machinery.
    pub ib: Option<fn(u64, u64) -> u64>,
}

#[derive(Clone)]
pub struct Dict { pub keys: Value, pub vals: Value }

/// Int null and infinities, like q: 0N is the smallest int, 0W the largest, -0W next to null.
pub const NI: i64 = i64::MIN;
pub const WI: i64 = i64::MAX;
pub const NWI: i64 = i64::MIN + 1;
/// Days from 1970.01.01 to 2000.01.01 — dates count from 2000.
pub const DATE_EPOCH: i64 = 10957;

/// Atoms and homogeneous vectors are separate variants: a vector of ints is a `Vec<i64>`,
/// never a `Vec<Value>`, so the primitives run tight typed loops.
#[derive(Clone)]
pub enum Value {
    Null,
    Bool(bool), Int(i64), Float(f64), Char(char), Symbol(Arc<str>), Byte(u8),
    Date(i32), Time(i64),   // days since 2000.01.01; milliseconds since midnight
    Bools(Arc<Vec<bool>>), Ints(Arc<Vec<i64>>), Floats(Arc<Vec<f64>>), Chars(Arc<Vec<char>>), Syms(Arc<Vec<Arc<str>>>),
    Dates(Arc<Vec<i32>>), Times(Arc<Vec<i64>>), Bytes(Arc<Vec<u8>>),   // 0x0aff: raw bytes, shown as hex; arithmetic promotes them to ints
    List(Arc<Vec<Value>>), Dict(Arc<Dict>),
    Lambda(Arc<FnCode>), Prim(&'static PrimDef), Adv(char, Arc<Value>),
    Proj(Arc<Value>, Arc<Vec<Value>>),          // partial application; Null marks an open slot
    Closure(Arc<FnCode>, Arc<Vec<Value>>),      // lambda plus captured outer locals (appended after its own locals)
    Shared(Arc<Mutex<Value>>),                  // `shared x`: an opt-in mutable cell; every other value is lock-free COW
    Thread(Arc<Mutex<Option<JoinHandle<R<Value>>>>>),   // `spawn f`; the Option lets `join` take the handle so joining twice errors cleanly
}
use Value::*;

pub fn ints(v: Vec<i64>) -> Value { Ints(Arc::new(v)) }
pub fn floats(v: Vec<f64>) -> Value { Floats(Arc::new(v)) }
pub fn bools(v: Vec<bool>) -> Value { Bools(Arc::new(v)) }
pub fn chars(v: Vec<char>) -> Value { Chars(Arc::new(v)) }
pub fn syms(v: Vec<Arc<str>>) -> Value { Syms(Arc::new(v)) }
pub fn dates(v: Vec<i32>) -> Value { Dates(Arc::new(v)) }
pub fn times(v: Vec<i64>) -> Value { Times(Arc::new(v)) }
pub fn list(v: Vec<Value>) -> Value { List(Arc::new(v)) }
pub fn bytes(v: Vec<u8>) -> Value { Bytes(Arc::new(v)) }

impl Value {
    pub fn len(&self) -> Option<usize> {
        Some(match self {
            Bools(v) => v.len(), Ints(v) => v.len(), Floats(v) => v.len(),
            Chars(v) => v.len(), Syms(v) => v.len(), List(v) => v.len(),
            Dates(v) => v.len(), Times(v) => v.len(), Bytes(v) => v.len(),
            Dict(d) => d.keys.count(),
            _ => return None,
        })
    }
    pub fn is_atom(&self) -> bool { self.len().is_none() }
    pub fn count(&self) -> usize { self.len().unwrap_or(1) }
    pub fn is_num(&self) -> bool { matches!(self, Bool(_) | Int(_) | Float(_) | Bools(_) | Ints(_) | Floats(_)) }
    pub fn is_fn(&self) -> bool { matches!(self, Lambda(_) | Prim(_) | Adv(..) | Proj(..) | Closure(..)) }
    pub fn is_float(&self) -> bool { matches!(self, Float(_) | Floats(_)) }

    /// Items of a vector as atoms (an atom is its own single item).
    pub fn seq(&self) -> Vec<Value> {
        match self {
            Bools(v) => v.iter().map(|&b| Bool(b)).collect(),
            Ints(v) => v.iter().map(|&i| Int(i)).collect(),
            Floats(v) => v.iter().map(|&f| Float(f)).collect(),
            Chars(v) => v.iter().map(|&c| Char(c)).collect(),
            Syms(v) => v.iter().map(|s| Symbol(s.clone())).collect(),
            Dates(v) => v.iter().map(|&d| Date(d)).collect(),
            Times(v) => v.iter().map(|&t| Time(t)).collect(),
            Bytes(v) => v.iter().map(|&b| Byte(b)).collect(),
            List(v) => v.as_ref().clone(),
            Dict(d) => d.vals.seq(),
            _ => vec![self.clone()],
        }
    }
    pub fn item(&self, i: usize) -> R<Value> {
        if self.is_atom() { return Ok(self.clone()); }   // like q: an atom indexes as itself, so "x"[0] is "x"
        if i >= self.count() { return err("index"); }
        Ok(match self {
            Bools(v) => Bool(v[i]), Ints(v) => Int(v[i]), Floats(v) => Float(v[i]),
            Chars(v) => Char(v[i]), Syms(v) => Symbol(v[i].clone()), List(v) => v[i].clone(),
            Dates(v) => Date(v[i]), Times(v) => Time(v[i]), Bytes(v) => Byte(v[i]),
            Dict(d) => d.vals.item(i)?,
            _ => unreachable!(),
        })
    }
    /// Conditions look at the first item, like q.
    pub fn truthy(&self) -> bool {
        match self {
            Null => false, Bool(b) => *b, Int(i) => *i != 0, Float(f) => *f != 0.0,
            Char(c) => *c != '\0', Byte(b) => *b != 0, Symbol(s) => !s.is_empty(), Date(d) => *d != 0, Time(t) => *t != 0,
            _ => self.item(0).map(|v| v.truthy()).unwrap_or(false),
        }
    }
}

/// Rebuild a vector from items: homogeneous -> typed vector, otherwise general list.
#[allow(unused_variables)]
pub fn pack(items: Vec<Value>) -> Value {
    if items.is_empty() { return ints(vec![]); }
    macro_rules! all { ($p:pat => $e:expr, $ctor:ident) => {
        if items.iter().all(|v| matches!(v, $p)) {
            return $ctor(items.iter().map(|v| match v { $p => $e, _ => unreachable!() }).collect());
        }
    }}
    all!(Int(i) => *i, ints);
    all!(Bool(b) => *b, bools);
    all!(Char(c) => *c, chars);
    all!(Symbol(s) => s.clone(), syms);
    all!(Date(d) => *d, dates);
    all!(Time(t) => *t, times);
    all!(Byte(b) => *b, bytes);
    if items.iter().all(|v| matches!(v, Int(_) | Float(_))) {
        return floats(items.iter().map(|v| match v { Int(i) => *i as f64, Float(f) => *f, _ => unreachable!() }).collect());
    }
    list(items)
}

// ---------------------------------------------------------------- numeric kernel
/// A numeric operand: atom or borrowed/promoted vector.
pub enum Sh<'a, T: Clone> { A(T), V(Cow<'a, [T]>) }
pub enum Out<T> { A(T), V(Vec<T>) }

pub fn sh_i(v: &Value) -> Option<Sh<'_, i64>> {
    Some(match v {
        Bool(b) => Sh::A(*b as i64), Int(i) => Sh::A(*i), Byte(b) => Sh::A(*b as i64), Date(d) => Sh::A(*d as i64), Time(t) => Sh::A(*t),
        Bools(b) => Sh::V(Cow::Owned(b.iter().map(|&x| x as i64).collect())),  // ponytail: promotion copies; add a Bools fast path if it shows up
        Ints(x) | Times(x) => Sh::V(Cow::Borrowed(x)),
        Dates(x) => Sh::V(Cow::Owned(x.iter().map(|&d| d as i64).collect())),
        Bytes(x) => Sh::V(Cow::Owned(x.iter().map(|&b| b as i64).collect())),
        _ => return None,
    })
}
fn i2f(i: i64) -> f64 { if i == NI { f64::NAN } else if i == WI { f64::INFINITY } else if i == NWI { f64::NEG_INFINITY } else { i as f64 } }
pub fn sh_f(v: &Value) -> Option<Sh<'_, f64>> {
    Some(match v {
        Float(f) => Sh::A(*f),
        Floats(x) => Sh::V(Cow::Borrowed(x)),
        _ => match sh_i(v)? { Sh::A(i) => Sh::A(i2f(i)), Sh::V(x) => Sh::V(Cow::Owned(x.iter().map(|&i| i2f(i)).collect())) },
    })
}
pub fn zip<T: Copy, U>(a: Sh<T>, b: Sh<T>, f: impl Fn(T, T) -> U) -> R<Out<U>> {
    Ok(match (a, b) {
        (Sh::A(x), Sh::A(y)) => Out::A(f(x, y)),
        (Sh::V(x), Sh::A(y)) => Out::V(x.iter().map(|&v| f(v, y)).collect()),
        (Sh::A(x), Sh::V(y)) => Out::V(y.iter().map(|&v| f(x, v)).collect()),
        (Sh::V(x), Sh::V(y)) => {
            if x.len() != y.len() { return err("length"); }
            Out::V(x.iter().zip(y.iter()).map(|(&p, &q)| f(p, q)).collect())
        }
    })
}
pub fn oi(o: Out<i64>) -> Value { match o { Out::A(x) => Int(x), Out::V(v) => ints(v) } }
pub fn of(o: Out<f64>) -> Value { match o { Out::A(x) => Float(x), Out::V(v) => floats(v) } }
pub fn ob(o: Out<bool>) -> Value { match o { Out::A(x) => Bool(x), Out::V(v) => bools(v) } }

/// Int op if both sides integral, else float op.
pub fn arith(x: &Value, y: &Value, fi: fn(i64, i64) -> i64, ff: fn(f64, f64) -> f64) -> R<Value> {
    if !x.is_float() && !y.is_float() {
        if let (Some(a), Some(b)) = (sh_i(x), sh_i(y)) { return Ok(oi(zip(a, b, fi)?)); }
    }
    match (sh_f(x), sh_f(y)) { (Some(a), Some(b)) => Ok(of(zip(a, b, ff)?)), _ => err("type: arithmetic on non-numeric") }
}
pub fn compare(x: &Value, y: &Value, fi: fn(i64, i64) -> bool, ff: fn(f64, f64) -> bool) -> R<Value> {
    if !x.is_float() && !y.is_float() {
        if let (Some(a), Some(b)) = (sh_i(x), sh_i(y)) { return Ok(ob(zip(a, b, fi)?)); }
    }
    match (sh_f(x), sh_f(y)) { (Some(a), Some(b)) => Ok(ob(zip(a, b, ff)?)), _ => err("type: comparison on non-numeric") }
}
pub fn map_f(x: &Value, f: fn(f64) -> f64) -> R<Value> {
    match sh_f(x) {
        Some(Sh::A(a)) => Ok(Float(f(a))),
        Some(Sh::V(v)) => Ok(floats(v.iter().map(|&a| f(a)).collect())),
        None => err("type: expected number"),
    }
}
pub fn map_i(x: &Value, f: fn(i64) -> i64) -> R<Value> {
    match sh_i(x) {
        Some(Sh::A(a)) => Ok(Int(f(a))),
        Some(Sh::V(v)) => Ok(ints(v.iter().map(|&a| f(a)).collect())),
        None => err("type: expected integer"),
    }
}
pub fn int_of(v: &Value) -> R<i64> {
    match v {
        Int(i) => Ok(*i), Bool(b) => Ok(*b as i64),
        Float(f) if *f == f.trunc() => Ok(*f as i64),
        _ => err("type: expected integer atom"),
    }
}

impl PartialEq for Value {
    fn eq(&self, o: &Value) -> bool {
        match (self, o) {
            (Null, Null) => true,
            (Bool(a), Bool(b)) => a == b, (Int(a), Int(b)) => a == b, (Float(a), Float(b)) => a == b || (a.is_nan() && b.is_nan()),
            (Char(a), Char(b)) => a == b, (Symbol(a), Symbol(b)) => a == b, (Date(a), Date(b)) => a == b, (Time(a), Time(b)) => a == b,
            (Bools(a), Bools(b)) => a == b, (Ints(a), Ints(b)) => a == b, (Chars(a), Chars(b)) => a == b, (Syms(a), Syms(b)) => a == b,
            (Dates(a), Dates(b)) => a == b, (Times(a), Times(b)) => a == b, (Byte(a), Byte(b)) => a == b, (Bytes(a), Bytes(b)) => a == b,
            (Floats(a), Floats(b)) => a.len() == b.len() && a.iter().zip(b.iter()).all(|(p, q)| p == q || (p.is_nan() && q.is_nan())),
            (List(a), List(b)) => a == b,
            (Dict(a), Dict(b)) => a.keys == b.keys && a.vals == b.vals,
            (Lambda(a), Lambda(b)) => Arc::ptr_eq(a, b),
            (Closure(a, x), Closure(b, y)) => Arc::ptr_eq(a, b) && x == y,
            (Prim(a), Prim(b)) => a.name == b.name,
            (Adv(c, a), Adv(d, b)) => c == d && a == b,
            (Proj(f, a), Proj(g, b)) => f == g && a == b,
            (Shared(a), Shared(b)) => Arc::ptr_eq(a, b),
            (Thread(a), Thread(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

pub fn cmp_val(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Char(x), Char(y)) => x.cmp(y),
        (Symbol(x), Symbol(y)) => x.cmp(y),
        _ if !a.is_atom() && !b.is_atom() => {   // vectors and rows compare lexicographically: strings sort, xasc on several columns
            let (xa, xb) = (a.seq(), b.seq());
            for (p, q) in xa.iter().zip(&xb) { let c = cmp_val(p, q); if c != Ordering::Equal { return c; } }
            xa.len().cmp(&xb.len())
        }
        _ => match (sh_f(a), sh_f(b)) {
            (Some(Sh::A(x)), Some(Sh::A(y))) => x.partial_cmp(&y).unwrap_or_else(|| y.is_nan().cmp(&x.is_nan())),   // null sorts first
            _ => Ordering::Equal,
        },
    }
}

// ---------------------------------------------------------------- display
fn numf(f: f64) -> String {
    if f.is_nan() { "0n".into() }
    else if f.is_infinite() { (if f > 0.0 { "0w" } else { "-0w" }).into() }
    else if f == f.trunc() && f.abs() < 1e15 { format!("{}f", f as i64) } else { format!("{}", f) }
}
fn intf(i: i64) -> String {
    match i { NI => "0N".into(), WI => "0W".into(), NWI => "-0W".into(), _ => i.to_string() }
}
/// Proleptic Gregorian (y, m, d) from days since 2000.01.01.
pub fn civil(days: i32) -> (i64, u32, u32) {
    let z = days as i64 + DATE_EPOCH + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}
/// Days since 2000.01.01 from a civil date.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i32 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146097 + doe - 719468 - DATE_EPOCH) as i32
}
pub fn datef(d: i32) -> String {
    if d == i32::MIN { return "0Nd".into(); }
    let (y, m, dd) = civil(d);
    format!("{y:04}.{m:02}.{dd:02}")
}
pub fn timef(t: i64) -> String {
    if t == NI { return "0Nt".into(); }
    let (sign, t) = if t < 0 { ("-", -t) } else { ("", t) };
    format!("{sign}{:02}:{:02}:{:02}.{:03}", t / 3_600_000, (t / 60_000) % 60, (t / 1000) % 60, t % 1000)
}
fn pre(n: usize) -> &'static str { if n == 1 { "," } else { "" } }
fn joined<T>(v: &[T], f: impl Fn(&T) -> String) -> String { v.iter().map(f).collect::<Vec<_>>().join(" ") }

impl Value {
    pub fn fmt(&self) -> String {
        match self {
            Null => "::".into(),
            Bool(b) => (if *b { "1b" } else { "0b" }).into(),
            Int(i) => intf(*i), Float(f) => numf(*f),
            Char(c) => format!("\"{}\"", c), Symbol(s) => format!("`{}", s),
            Date(d) => datef(*d), Time(t) => timef(*t),
            Byte(b) => format!("0x{b:02x}"), Bytes(v) => format!("{}0x{}", pre(v.len()), v.iter().map(|b| format!("{b:02x}")).collect::<String>()),
            Bools(v) => format!("{}{}b", pre(v.len()), v.iter().map(|&b| if b { '1' } else { '0' }).collect::<String>()),
            Ints(v) if v.is_empty() => "()".into(),
            Ints(v) => format!("{}{}", pre(v.len()), joined(v, |i| intf(*i))),
            Floats(v) => format!("{}{}", pre(v.len()), joined(v, |f| numf(*f))),
            Dates(v) => format!("{}{}", pre(v.len()), joined(v, |d| datef(*d))),
            Times(v) => format!("{}{}", pre(v.len()), joined(v, |t| timef(*t))),
            Chars(v) => format!("\"{}\"", v.iter().collect::<String>()),
            Syms(v) => format!("{}{}", pre(v.len()), v.iter().map(|s| format!("`{}", s)).collect::<String>()),
            List(v) => format!("({})", v.iter().map(|x| x.fmt()).collect::<Vec<_>>().join(";")),
            Dict(d) => format!("{}!{}", d.keys.fmt(), d.vals.fmt()),
            Lambda(f) | Closure(f, _) => format!("{{[{}]...}}", f.params.join(";")),
            Prim(p) => p.name.into(),
            Proj(f, held) => format!("{}[{}]", f.fmt(), held.iter().map(|a| if matches!(a, Null) { String::new() } else { a.fmt() }).collect::<Vec<_>>().join(";")),
            Adv(c, f) => format!("{}{}", f.fmt(), advf(*c)),
            Shared(_) => "<shared>".into(),
            Thread(_) => "<thread>".into(),
        }
    }
}
/// Adverb spelling; each-left and each-right are two characters, stored as 'L' and 'R'.
pub fn advf(c: char) -> &'static str { match c { 'L' => "\\:", 'R' => "/:", '/' => "/", '\\' => "\\", _ => "'" } }
impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&Value::fmt(self)) }
}

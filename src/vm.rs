//! Stack VM. Adverbs live here because they call back into user functions.
use crate::compile::compile_stmt;
use crate::lex::lex_lines;
use crate::parse::{Ast, Parser};
use crate::prims::{amend_path, fold_fast, index_at, load_unit, scan_fast, BUILTINS};
use crate::value::*;
use std::collections::HashMap;
use std::rc::Rc;
use Value::*;

/// Globals live in a slot vector; names are interned once when code is loaded, so LoadG is an index, not a hash.
pub struct Vm { vals: Vec<Option<Value>>, names: HashMap<Rc<str>, u32>, slot_names: Vec<Rc<str>>, depth: usize, pool: Vec<Vec<Value>> }

impl Vm {
    pub fn new() -> Vm {
        let mut vm = Vm { vals: vec![], names: HashMap::new(), slot_names: vec![], depth: 0, pool: vec![] };
        for p in BUILTINS { vm.set(p.name, Prim(p)); }
        vm
    }
    fn slot(&mut self, name: &str) -> u32 {
        if let Some(&s) = self.names.get(name) { return s; }
        let s = self.vals.len() as u32;
        let rc: Rc<str> = Rc::from(name);
        self.vals.push(None); self.names.insert(rc.clone(), s); self.slot_names.push(rc);
        s
    }
    pub fn set(&mut self, name: &str, v: Value) { let s = self.slot(name) as usize; self.vals[s] = Some(v); }
    pub fn get(&self, name: &str) -> Option<Value> { self.names.get(name).and_then(|&s| self.vals[s as usize].clone()) }
    /// All global values, for save/restore around test cases (slots only grow, so a snapshot stays valid).
    #[allow(dead_code)]
    pub fn snapshot(&self) -> Vec<Option<Value>> { self.vals.clone() }
    #[allow(dead_code)]
    pub fn restore(&mut self, snap: &[Option<Value>]) {
        for (i, v) in self.vals.iter_mut().enumerate() { *v = snap.get(i).cloned().flatten(); }
    }
    /// Replace global-name consts with slot indices, recursively through lambda consts.
    fn intern(&mut self, ops: &[Op], consts: &mut [Value]) {
        for op in ops {
            if let Op::LoadG(a) | Op::StoreG(a) | Op::TakeG(a) = op {
                if let Symbol(n) = &consts[*a as usize] { let s = self.slot(n); consts[*a as usize] = Int(s as i64); }
            }
        }
        for c in consts.iter_mut() {
            if let Lambda(code) = c {
                if let Some(code) = Rc::get_mut(code) { let FnCode { ops, consts, .. } = code; self.intern(ops, consts); }
            }
        }
    }
    fn gidx(&mut self, c: &Value) -> usize {
        match c { Int(s) => *s as usize, Symbol(n) => self.slot(n) as usize, _ => unreachable!() }
    }

    /// Run a program; value of the last statement (assignments yield Null so the REPL stays quiet).
    pub fn run(&mut self, src: &str) -> R<Value> {
        let mut last = Null;
        let (toks, lines) = lex_lines(src)?;
        let multi = lines.last().is_some_and(|&l| l > 1);   // one-liners (the REPL) get no "at line 1" noise
        let mut p = Parser::with_lines(toks, lines);
        let asts = p.program()?;
        for (i, ast) in asts.iter().enumerate() {
            let (ops, mut k) = compile_stmt(ast)?;
            self.intern(&ops, &mut k);
            let v = self.execute(&ops, &k, &mut Vec::new())
                .map_err(|e| if multi { NError(format!("{} at line {}", e.0, p.stmt_lines()[i])) } else { e })?;
            last = if matches!(ast, Ast::Assign(..) | Ast::GAssign(..) | Ast::IndexAssign(..)) { Null } else { v };
        }
        Ok(last)
    }

    fn execute(&mut self, ops: &[Op], k: &[Value], loc: &mut Vec<Value>) -> R<Value> {
        let mut st: Vec<Value> = self.pool.pop().unwrap_or_default();
        let mut ip = 0;
        while ip < ops.len() {
            let op = ops[ip]; ip += 1;
            match op {
                Op::Push(a) => st.push(k[a as usize].clone()),
                Op::LoadL(a) => st.push(loc[a as usize].clone()),
                Op::LoadG(a) => {
                    let s = self.gidx(&k[a as usize]);
                    match &self.vals[s] { Some(v) => st.push(v.clone()), None => return err(format!("undefined: {}", self.slot_names[s])) }
                }
                Op::StoreL(a) => loc[a as usize] = st.last().unwrap().clone(),
                Op::StoreG(a) => { let s = self.gidx(&k[a as usize]); self.vals[s] = Some(st.last().unwrap().clone()); }
                Op::TakeL(a) => st.push(std::mem::replace(&mut loc[a as usize], Null)),
                Op::TakeG(a) => {
                    let s = self.gidx(&k[a as usize]);
                    match self.vals[s].take() { Some(v) => st.push(v), None => return err(format!("undefined: {}", self.slot_names[s])) }
                }
                Op::Amend(n) => {
                    // x stays on the stack even on error so the following Store puts it back untouched
                    let mut x = st.pop().unwrap();
                    let idx: Vec<Value> = (0..n).map(|_| st.pop().unwrap()).collect();
                    let v = st.pop().unwrap();
                    let r = amend_path(&mut x, &idx, v); st.push(x); r?;
                }
                Op::MkClosure(n) => {
                    let Lambda(code) = st.pop().unwrap() else { return err("closure: not a lambda") };
                    let mut caps: Vec<Value> = (0..n).map(|_| st.pop().unwrap()).collect(); caps.reverse();
                    st.push(Closure(code, Rc::new(caps)));
                }
                Op::Loop(t) => {
                    let n = int_of(st.last().unwrap())?;
                    if n > 0 { *st.last_mut().unwrap() = Int(n - 1); } else { st.pop(); ip = t as usize; }
                }
                Op::Monad(a) => { let x = st.pop().unwrap(); let r = self.monad(&k[a as usize], x)?; st.push(r); }
                Op::Dyad(a) => {
                    let x = st.pop().unwrap(); let y = st.pop().unwrap();
                    let r = self.dyad(&k[a as usize], x, y)?; st.push(r);
                }
                Op::Call(n) => {
                    let f = st.pop().unwrap();
                    let mut args = self.pool.pop().unwrap_or_default();   // becomes the callee's locals; returned to the pool after
                    for _ in 0..n { args.push(st.pop().unwrap()); }
                    let r = self.call(&f, args)?; st.push(r);
                }
                Op::MkAdv(c) => { let f = st.pop().unwrap(); st.push(Adv(c, Rc::new(f))); }
                Op::Jmp(t) => ip = t as usize,
                Op::Jmpf(t) => if !st.pop().unwrap().truthy() { ip = t as usize },
                Op::Pop => { st.pop(); }
                Op::Ret => { let v = st.pop().unwrap_or(Null); st.clear(); self.pool.push(st); return Ok(v); }
                Op::List(n) => { let items = (0..n).map(|_| st.pop().unwrap()).collect(); st.push(pack(items)); }
            }
        }
        let v = st.pop().unwrap_or(Null);
        st.clear(); self.pool.push(st);
        Ok(v)
    }

    /// Run bytecode data: one unit `(opcodes; args; consts)` or a list of units (a boot image). Value of the last unit.
    pub fn exec(&mut self, data: &Value) -> R<Value> {
        let units = if matches!(data.item(0), Ok(Ints(_))) { vec![data.clone()] } else { data.seq() };
        let mut last = Null;
        for u in units { let (ops, mut k) = load_unit(&u)?; self.intern(&ops, &mut k); last = self.execute(&ops, &k, &mut Vec::new())?; }
        Ok(last)
    }

    /// Lambda or closure call: args, then its own locals, then captured values (slots the compiler assigned).
    fn call_code(&mut self, code: &Rc<FnCode>, caps: &[Value], f: &Value, args: Vec<Value>) -> R<Value> {
        let arity = code.params.len();
        if args.len() > arity { return err(format!("rank: expected {arity} args, got {}", args.len())); }
        let empty_unary = args.is_empty() && arity == 1;   // f[] calls a unary with null, like q
        if !empty_unary && (args.len() < arity || args.iter().any(|a| matches!(a, Null))) {   // projection
            let mut held = args; held.resize(arity, Null);
            return Ok(Proj(Rc::new(f.clone()), Rc::new(held)));
        }
        if self.depth > 2000 { return err("stack: recursion too deep"); }
        let mut loc = args; loc.resize(code.nlocals.max(arity), Null); loc.extend_from_slice(caps);
        self.depth += 1;
        let r = self.execute(&code.ops, &code.consts, &mut loc);
        self.depth -= 1;
        loc.clear(); self.pool.push(loc);
        r
    }

    pub fn call(&mut self, f: &Value, args: Vec<Value>) -> R<Value> {
        match f {
            Lambda(code) => self.call_code(code, &[], f, args),
            Closure(code, caps) => self.call_code(code, caps, f, args),
            Proj(g, held) => {
                let mut filled = held.as_ref().clone();
                let mut it = args.into_iter();
                for slot in filled.iter_mut() {
                    if matches!(slot, Null) { match it.next() { Some(a) => *slot = a, None => break } }
                }
                if it.next().is_some() { return err("rank: too many args for projection"); }
                let g = g.as_ref().clone();
                self.call(&g, filled)
            }
            Prim(p) if p.name == "@" && args.len() == 3 => {   // @[f;x;handler]: protected call
                let mut it = args.into_iter();
                let (g, x, h) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
                match self.call(&g, vec![x]) {
                    Ok(v) => Ok(v),
                    Err(e) => if h.is_fn() { self.call(&h, vec![chars(e.0.chars().collect())]) } else { Ok(h) },
                }
            }
            Prim(_) | Adv(..) => {
                if args.len() == 2 && args.iter().any(|a| matches!(a, Null)) { return Ok(Proj(Rc::new(f.clone()), Rc::new(args))); }
                let mut it = args.into_iter();
                match (it.next(), it.next(), it.next()) {
                    (Some(x), None, _) => self.monad(f, x),
                    (Some(x), Some(y), None) => self.dyad(f, x, y),
                    _ => err("rank: primitives take 1 or 2 args"),
                }
            }
            _ if args.len() == 1 => index_at(f.clone(), args.into_iter().next().unwrap()),
            _ => err("type: not callable"),
        }
    }

    fn monad(&mut self, f: &Value, x: Value) -> R<Value> {
        match f {
            Prim(p) => match (p.m, p.name) {
                (Some(m), _) => m(x),
                (None, "exec") => self.exec(&x),
                _ => err(format!("rank: {} has no monadic form", p.name)),
            },
            Adv(c, g) => self.adv1(*c, g, x),
            _ => self.call(f, vec![x]),
        }
    }
    fn dyad(&mut self, f: &Value, x: Value, y: Value) -> R<Value> {
        match f {
            Prim(p) => match (p.d, p.name) {
                (Some(d), _) => d(x, y),
                (None, "each") => self.adv1('\'', &x, y),   // f each x
                (None, "over") => self.adv1('/', &x, y),
                (None, "scan") => self.adv1('\\', &x, y),
                _ => err(format!("rank: {} has no dyadic form", p.name)),
            },
            Adv(c, g) => self.adv2(*c, g, x, y),
            _ => self.call(f, vec![x, y]),
        }
    }

    fn adv1(&mut self, c: char, g: &Value, x: Value) -> R<Value> {
        let verb = if let Prim(p) = g { p.name.chars().next().unwrap() } else { '\0' };
        match c {
            '/' => {
                if let Some(v) = fold_fast(verb, &x) { return Ok(v); }
                let mut it = x.seq().into_iter();
                let mut acc = it.next().ok_or_else(|| NError("fold over empty".into()))?;
                for v in it { acc = self.dyad(g, acc, v)?; }
                Ok(acc)
            }
            '\\' => {
                if let Some(v) = scan_fast(verb, &x) { return Ok(v); }
                let mut out = Vec::new(); let mut acc: Option<Value> = None;
                for v in x.seq() {
                    let a = match acc { None => v, Some(a) => self.dyad(g, a, v)? };
                    out.push(a.clone()); acc = Some(a);
                }
                Ok(pack(out))
            }
            'L' | 'R' => err("rank: each-left/each-right take two arguments"),
            _ => {
                let out = pack(x.seq().into_iter().map(|v| self.monad(g, v)).collect::<R<Vec<_>>>()?);
                match x {   // each over a dict maps the values and keeps the keys
                    Dict(d) => Ok(Dict(Rc::new(crate::value::Dict { keys: d.keys.clone(), vals: out }))),
                    _ => Ok(out),
                }
            }
        }
    }
    fn adv2(&mut self, c: char, g: &Value, x: Value, y: Value) -> R<Value> {
        match c {
            '/' => { let mut acc = x; for v in y.seq() { acc = self.dyad(g, acc, v)?; } Ok(acc) }
            '\\' => {
                let mut out = Vec::new(); let mut acc = x;
                for v in y.seq() { acc = self.dyad(g, acc, v)?; out.push(acc.clone()); }
                Ok(pack(out))
            }
            'L' => Ok(pack(x.seq().into_iter().map(|a| self.dyad(g, a, y.clone())).collect::<R<Vec<_>>>()?)),   // x f\: y — each x_i with all of y
            'R' => Ok(pack(y.seq().into_iter().map(|b| self.dyad(g, x.clone(), b)).collect::<R<Vec<_>>>()?)),   // x f/: y — all of x with each y_j
            _ => {
                let (mut xs, mut ys) = (x.seq(), y.seq());
                if xs.len() == 1 && ys.len() > 1 { xs = vec![xs[0].clone(); ys.len()]; }
                if ys.len() == 1 && xs.len() > 1 { ys = vec![ys[0].clone(); xs.len()]; }
                if xs.len() != ys.len() { return err("length: each"); }
                Ok(pack(xs.into_iter().zip(ys).map(|(a, b)| self.dyad(g, a, b)).collect::<R<Vec<_>>>()?))
            }
        }
    }
}

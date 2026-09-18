//! Stack VM. Adverbs live here because they call back into user functions.
//! There is no front end here: source becomes bytecode in src/neant/core/{lex,parse,compile}.nt, which run on this VM.
use crate::prims::{amend_path, fold_fast, index_at, load_unit, scan_fast, BUILTINS};
use crate::value::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use Value::*;

/// Two int atoms through the common verbs, skipping the shape/broadcast machinery in value.rs.
/// Nulls and every other verb fall through to the general path, which is what defines the semantics.
fn int_dyad(name: &str, a: i64, b: i64) -> Option<Value> {
    if name.len() != 1 || a == NI || b == NI { return None; }
    Some(match name.as_bytes()[0] {
        b'+' => Int(a.wrapping_add(b)),
        b'-' => Int(a.wrapping_sub(b)),
        b'*' => Int(a.wrapping_mul(b)),
        b'&' => Int(a.min(b)),
        b'|' => Int(a.max(b)),
        b'<' => Bool(a < b),
        b'>' => Bool(a > b),
        b'=' => Bool(a == b),
        _ => return None,
    })
}

/// The value `n` down from the top of a stack, for peeking at a callee before popping it.
fn f_at(st: &[Value], n: usize) -> &Value { &st[st.len() - 1 - n] }

/// How deep interpreted calls may nest — see `call_code`'s guard for the calibration. Shared with
/// the JIT's `jit_call` (src/jit.rs), whose compiled-to-compiled chain sits *on top of* whatever
/// interpreted depth it was entered from and so has to count against the same budget.
pub(crate) const MAX_DEPTH: usize = 1000;

/// One call-stack frame of an error: the function's name (None when it is not a plain global call) and
/// the source line it was on. A frame is named by its *caller*, which knows the global it loaded to call it.
type Frame = (Option<Arc<str>>, u32);

/// Globals live in a slot vector; names are interned once when code is loaded, so LoadG is an index, not a hash.
/// trace: frames an in-flight error is unwinding through, innermost first.
/// last_trace: the same, kept for `elast` after @[f;x;h] catches.
/// recorder: the tracing JIT's in-flight recording (src/trace.rs), if any. On the VM rather than in
/// `run_ops`'s own frame because a recording follows calls *into* their frames — `call_code` and
/// the nested `run_ops` both need to reach it.
pub struct Vm { vals: Vec<Option<Value>>, names: HashMap<Arc<str>, u32>, slot_names: Vec<Arc<str>>, depth: usize, pool: Vec<Vec<Value>>, trace: Vec<Frame>, last_trace: Vec<Frame>, recorder: Option<crate::trace::Recorder> }

impl Vm {
    pub fn new() -> Vm {
        let mut vm = Vm { vals: vec![], names: HashMap::new(), slot_names: vec![], depth: 0, pool: vec![], trace: vec![], last_trace: vec![], recorder: None };
        for p in BUILTINS { vm.set(p.name, Prim(p)); }
        vm
    }
    /// A fresh VM for a `spawn`ed thread: the parent's globals and name→slot map (so the child
    /// resolves `LoadG`/`StoreG` in already-compiled bytecode identically), a clean call stack.
    fn forked(vals: Vec<Option<Value>>, names: HashMap<Arc<str>, u32>, slot_names: Vec<Arc<str>>) -> Vm {
        Vm { vals, names, slot_names, depth: 0, pool: vec![], trace: vec![], last_trace: vec![], recorder: None }
    }
    fn slot(&mut self, name: &str) -> u32 {
        if let Some(&s) = self.names.get(name) { return s; }
        let s = self.vals.len() as u32;
        let rc: Arc<str> = Arc::from(name);
        self.vals.push(None); self.names.insert(rc.clone(), s); self.slot_names.push(rc);
        s
    }
    pub fn set(&mut self, name: &str, v: Value) { let s = self.slot(name) as usize; self.vals[s] = Some(v); }
    pub fn get(&self, name: &str) -> Option<Value> { self.names.get(name).and_then(|&s| self.vals[s as usize].clone()) }
    /// A global by its already-interned slot index (see `intern`/`gidx`) — what the JIT's `jit_call`
    /// trampoline (src/jit.rs) uses to resolve a compiled call site's callee, the same lookup
    /// `Op::LoadG` does in `run_ops` below.
    pub(crate) fn global_at(&self, slot: usize) -> Option<Value> { self.vals.get(slot).cloned().flatten() }
    /// The slot a name is already interned at, without interning it if it isn't — the JIT
    /// (src/jit.rs) needs to recognise a `LoadG` of `band`/`shr`/... by slot number, and asking
    /// for a name nothing has ever mentioned should not create one.
    pub(crate) fn slot_of(&self, name: &str) -> Option<usize> { self.names.get(name).map(|&s| s as usize) }
    /// The current interpreted call depth (`call_code`), for `jit_call`'s combined depth guard.
    pub(crate) fn depth(&self) -> usize { self.depth }
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
                if let Some(code) = Arc::get_mut(code) { let FnCode { ops, consts, .. } = code; self.intern(ops, consts); }
            }
        }
    }
    fn gidx(&mut self, c: &Value) -> usize {
        match c { Int(s) => *s as usize, Symbol(n) => self.slot(n) as usize, _ => unreachable!() }
    }

    /// Run source through the self-hosted front end: `nrun` from the boot image (src/neant/core/compile.nt).
    /// Value of the last statement; a trailing assignment yields Null so the REPL stays quiet.
    pub fn eval(&mut self, src: &str) -> R<Value> {
        let f = self.get("nrun").ok_or_else(|| NError("nrun: no boot image loaded".into()))?;
        self.call(&f, vec![chars(src.chars().collect())])
    }

    /// Run a frame; on error record the line the failing op came from, so the trace grows innermost-first.
    /// If this frame failed inside a Call, the op before it loaded the callee — that names the frame below.
    /// `code`: `Some` when this frame belongs to an actual `FnCode` (a lambda/closure call,
    /// `Vm::call_code`) — the only case `Op::Jmp`/`Op::Loop` backward jumps can trigger the
    /// tracing JIT's per-loop-header hotness counting (`FnCode::loop_action`); `None` for top-level
    /// unit execution (`Vm::exec`), which has no `FnCode` to key that counter on and isn't the
    /// tracing JIT's target anyway.
    fn execute(&mut self, code: Option<&Arc<FnCode>>, ops: &[Op], k: &[Value], lines: &[u32], loc: &mut Vec<Value>) -> R<Value> {
        let mut ip = 0;
        let r = self.run_ops(code, ops, k, loc, &mut ip);
        if r.is_err() {
            let at = ip.saturating_sub(1);
            if let (Some(Op::Call(_)), Some(&Op::LoadG(a))) = (ops.get(at), at.checked_sub(1).and_then(|i| ops.get(i))) {
                let s = self.gidx(&k[a as usize]);
                let nm = self.slot_names[s].clone();
                if let Some(f) = self.trace.last_mut() { if f.0.is_none() { f.0 = Some(nm); } }
            }
            self.trace.push((None, lines.get(at).copied().unwrap_or(0)));
        }
        r
    }

    fn run_ops(&mut self, code: Option<&Arc<FnCode>>, ops: &[Op], k: &[Value], loc: &mut Vec<Value>, ipc: &mut usize) -> R<Value> {
        let mut st: Vec<Value> = self.pool.pop().unwrap_or_default();
        let mut ip = 0;
        while ip < ops.len() {
            let op = ops[ip]; ip += 1; *ipc = ip;   // where the error reporter looks when an op fails
            let ip_before_op = ip as u32;
            let was_recording = self.recorder.is_some();
            // A backward `Jmp` is a loop header being reached again — both `while` (jumps back to
            // its own condition check) and `do` (jumps back to the `Op::Loop` that decrements its
            // counter) emit this shape; `Op::Loop`'s own target is always *forward* (the loop's
            // exit), never a header, so it's not checked here. Only meaningful with a real `FnCode`
            // to key the per-header counter on (see `execute`'s doc comment). What the operand
            // stack holds at this point — nothing for a `while`, its counter for a `do`, the
            // counters of enclosing `do` loops for either — is part of the trace (`entry_tags`;
            // see src/trace.rs's module doc comment): it has to be plain ints, or this pass of the
            // loop is silently left interpreted.
            if !was_recording {
                if let Op::Jmp(t) = op {
                    if (t as usize) < ip {
                        if let Some(code) = code {
                            match code.loop_action(t) {
                                // Already compiled: run it instead of interpreting this iteration.
                                // `run` checks the stack against what the trace was recorded on,
                                // and on a bail leaves `st` holding what the interpreter would have
                                // had at `bail_ip`. A refusal falls through to the ordinary
                                // `Op::Jmp` below: a type mismatch is the same "just don't use the
                                // compiled version this time" fallback as everywhere else in the
                                // JIT; a callee global that has been reassigned since the trace
                                // was recorded is for good, so that trace is retired and the loop
                                // counted afresh (`FnCode::retrace`).
                                LoopAction::Run(trace) => match trace.run(loc, &mut st, self) {
                                    crate::jit::TraceRun::Bailed(bail_ip) => { ip = bail_ip; *ipc = ip; continue; }
                                    crate::jit::TraceRun::TypeMismatch => {}
                                    crate::jit::TraceRun::StaleCallee => code.retrace(t),
                                },
                                LoopAction::StartRecording => {
                                    if let Some(entry) = crate::trace::TraceTy::entry_tags(&st) {
                                        self.recorder = Some(crate::trace::Recorder::start(t, loc.len() as u32, code.clone(), entry));
                                    }
                                }
                                LoopAction::None => {}
                            }
                        }
                    }
                }
            }
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
                    st.push(Closure(code, Arc::new(caps)));
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
                // Anything that is not a user function takes its arguments straight off the stack: no args vector,
                // so no allocation. The general path's vector becomes the callee's locals and returns to the
                // pool, but `call` consumes and drops it for a primitive or an index. Lambdas go below.
                Op::Call(n @ (1 | 2)) if !matches!(f_at(&st, 0), Lambda(_) | Closure(..) | Proj(..)) => {
                    let f = st.pop().unwrap();
                    let x = st.pop().unwrap();
                    let prim = matches!(f, Prim(_) | Adv(..));
                    let r = if n == 1 {
                        if prim { self.monad(&f, x)? } else { index_at(f, x)? }   // x[i] on a vector or dict
                    } else {
                        let y = st.pop().unwrap();
                        if prim && (matches!(x, Null) || matches!(y, Null)) { Proj(Arc::new(f), Arc::new(vec![x, y])) }
                        else { self.dyad(&f, x, y)? }   // a non-callable here still errors, in `call`
                    };
                    st.push(r);
                }
                Op::Call(n) => {
                    let f = st.pop().unwrap();
                    let mut args = self.pool.pop().unwrap_or_default();   // becomes the callee's locals; returned to the pool after
                    for _ in 0..n { args.push(st.pop().unwrap()); }
                    let r = self.call(&f, args)?; st.push(r);
                }
                Op::MkAdv(c) => { let f = st.pop().unwrap(); st.push(Adv(c, Arc::new(f))); }
                Op::Jmp(t) => ip = t as usize,
                Op::Jmpf(t) => if !st.pop().unwrap().truthy() { ip = t as usize },
                Op::Pop => { st.pop(); }
                Op::Ret => { let v = st.pop().unwrap_or(Null); st.clear(); self.pool.push(st); return Ok(v); }
                Op::List(n) => { let items = (0..n).map(|_| st.pop().unwrap()).collect(); st.push(pack(items)); }
            }
            // `was_recording`, not a fresh check: a recording that *started* on this very op (the
            // backward `Jmp` above) begins with the next one. The outcome goes to the recorder's
            // own owner, not this frame's `code` — a recording can end inside an inlined callee,
            // where `code` is the callee's and its loop table has nothing to do with the header.
            if was_recording {
                if let Some(rec) = self.recorder.as_mut() {
                    if rec.step(op, ip_before_op, ip as u32, &st, k) {
                        let rec = self.recorder.take().unwrap();
                        let owner = rec.owner().clone();
                        let t = rec.finish();
                        match crate::jit::compile_trace(&t, self) {
                            Some(compiled) => owner.set_trace_compiled(t.header, Arc::new(compiled)),
                            None => owner.set_trace_rejected(t.header),
                        }
                    } else if rec.failed() {
                        let rec = self.recorder.take().unwrap();
                        rec.owner().set_trace_rejected(rec.header());
                    }
                }
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
        for u in units {
            let (ops, mut k, lines) = load_unit(&u)?;
            self.intern(&ops, &mut k);
            self.trace.clear();
            last = self.execute(None, &ops, &k, &lines, &mut Vec::new())?;
        }
        Ok(last)
    }

    /// Lambda or closure call: args, then its own locals, then captured values (slots the compiler assigned).
    fn call_code(&mut self, code: &Arc<FnCode>, caps: &[Value], f: &Value, args: Vec<Value>) -> R<Value> {
        let arity = code.params.len();
        if args.len() > arity { return err(format!("rank: expected {arity} args, got {}", args.len())); }
        let empty_unary = args.is_empty() && arity == 1;   // f[] calls a unary with null, like q
        if !empty_unary && (args.len() < arity || args.iter().any(|a| matches!(a, Null))) {   // projection
            let mut held = args; held.resize(arity, Null);
            return Ok(Proj(Arc::new(f.clone()), Arc::new(held)));
        }
        // Lower than it looks: each level is a real Rust stack frame, and this needs to be safe on
        // the smallest stack this can run on, not just the main thread's — a `spawn`ed thread
        // (src/prims.rs) defaults to a 2MiB OS stack. 1800 levels of plain recursion overflows one;
        // this leaves real margin. src/jit.rs's `MAX_CALL_DEPTH` mirrors this for the same reason.
        if self.depth > MAX_DEPTH { return err("stack: recursion too deep"); }
        let nargs = args.len();
        let mut loc = args; loc.resize(code.nlocals.max(arity), Null); loc.extend_from_slice(caps);
        self.depth += 1;
        // A hot, integer-only function may have a compiled native version (src/jit.rs); its entry
        // guard and its own deopt path both just mean "run it on the interpreter instead", so a
        // `None` here always falls straight through to the same `execute` that runs everything else.
        // Not while a trace is being recorded, though: the recording has to *see* this frame's ops
        // to inline them (src/trace.rs), and a compiled version would run them where it can't. The
        // recording is one loop iteration long, so this only ever defers a tier-up, never skips it.
        let compiled = if self.recorder.is_some() { None } else { code.jitted(self) };
        let jit_result = match &compiled { Some(c) => c.try_run(&loc, self), None => None };
        let r = match jit_result {
            Some(v) => Ok(v),
            None => {
                if let Some(rec) = self.recorder.as_mut() { rec.enter_frame(code, nargs, &loc); }
                let r = self.execute(Some(code), &code.ops, &code.consts, &code.lines, &mut loc);
                if let Some(rec) = self.recorder.as_mut() { if r.is_ok() { rec.exit_frame() } else { rec.abort() } }
                r
            }
        };
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
                    Err(e) => {
                        self.last_trace = std::mem::take(&mut self.trace);
                        if h.is_fn() { self.call(&h, vec![chars(e.0.chars().collect())]) } else { Ok(h) }
                    }
                }
            }
            Prim(_) | Adv(..) => {
                if args.len() == 2 && args.iter().any(|a| matches!(a, Null)) { return Ok(Proj(Arc::new(f.clone()), Arc::new(args))); }
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

    /// `spawn f`: runs niladic f on a new OS thread with its own VM, forked from this one's
    /// globals (a snapshot: an Arc-bump clone of the slot table, not a shared one — the new
    /// thread sees today's globals and nothing this thread assigns afterwards). No lock is
    /// taken; ordinary values are safe to hand across because they are lock-free COW.
    fn spawn(&mut self, f: Value) -> R<Value> {
        let (vals, names, slot_names) = (self.vals.clone(), self.names.clone(), self.slot_names.clone());
        let handle = std::thread::spawn(move || Vm::forked(vals, names, slot_names).call(&f, vec![]));
        Ok(Thread(Arc::new(Mutex::new(Some(handle)))))
    }
    /// `join h` blocks for the spawned thread's result; a runtime error raised inside it
    /// surfaces here rather than being lost, and joining twice is a clean error, not a panic.
    fn join(&mut self, h: Value) -> R<Value> {
        match h {
            Thread(cell) => match cell.lock().unwrap().take() {
                Some(h) => h.join().unwrap_or_else(|_| err("thread: panicked")),
                None => err("thread: already joined"),
            }
            _ => err("type: join expected a thread handle"),
        }
    }
    /// `supd[s;f]` is the one atomic read-modify-write: it holds s's lock for the whole call to
    /// f, so concurrent `supd`s on the same cell serialize instead of losing an update the way a
    /// bare `sset[s; f sget s]` would if two threads interleaved between the get and the set.
    fn supd(&mut self, s: Value, f: Value) -> R<Value> {
        match s {
            Shared(cell) => {
                let mut guard = cell.lock().unwrap();
                let r = self.call(&f, vec![guard.clone()])?;
                *guard = r.clone();
                Ok(r)
            }
            _ => err("type: supd expected a shared cell"),
        }
    }

    fn monad(&mut self, f: &Value, x: Value) -> R<Value> {
        match f {
            Prim(p) => match (p.m, p.name) {
                (Some(m), _) => m(x),
                (None, "exec") => self.exec(&x),
                (None, "jitct") => crate::jit::ct_why(self, &x),
                (None, "elast") => match &x {
                    Symbol(s) if &**s == "line" => Ok(Int(self.last_trace.first().map_or(0, |f| f.1) as i64)),
                    Symbol(s) if &**s == "trace" => Ok(pack(self.last_trace.iter()
                        .map(|(n, l)| pack(vec![Symbol(n.clone().unwrap_or_else(|| Arc::from(""))), Int(*l as i64)])).collect())),
                    _ => err("type: elast `line or elast `trace"),
                },
                (None, "spawn") => self.spawn(x),
                (None, "join") => self.join(x),
                _ => err(format!("rank: {} has no monadic form", p.name)),
            },
            Adv(c, g) => self.adv1(*c, g, x),
            _ => { let mut a = self.pool.pop().unwrap_or_default(); a.push(x); self.call(f, a) }
        }
    }
    fn dyad(&mut self, f: &Value, x: Value, y: Value) -> R<Value> {
        match f {
            Prim(p) => {
                if let (Int(a), Int(b)) = (&x, &y) {
                    if let Some(f) = p.ib { return Ok(Int(f(*a as u64, *b as u64) as i64)); }
                    if let Some(v) = int_dyad(p.name, *a, *b) { return Ok(v); }
                }
                match (p.d, p.name) {
                (Some(d), _) => d(x, y),
                (None, "each") => self.adv1('\'', &x, y),   // f each x
                (None, "over") => self.adv1('/', &x, y),
                (None, "scan") => self.adv1('\\', &x, y),
                (None, "supd") => self.supd(x, y),
                _ => err(format!("rank: {} has no dyadic form", p.name)),
                }
            }
            Adv(c, g) => self.adv2(*c, g, x, y),
            _ => { let mut a = self.pool.pop().unwrap_or_default(); a.push(x); a.push(y); self.call(f, a) }
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
                // the whole loop in compiled code, when the shapes allow it
                if let Some(r) = self.each_ints_jit(g, &x) { return r; }
                let xs = x.seq();
                let out = pack(match self.each_lambda(g, &xs) {
                    Some(r) => r?,
                    None => xs.into_iter().map(|v| self.monad(g, v)).collect::<R<Vec<_>>>()?,
                });
                match x {   // each over a dict maps the values and keeps the keys
                    Dict(d) => Ok(Dict(Arc::new(crate::value::Dict { keys: d.keys.clone(), vals: out }))),
                    _ => Ok(out),
                }
            }
        }
    }
    /// `x f' y` over two int vectors, the two-argument twin of `each_ints_jit` below. An atom on
    /// either side is the broadcast the general path does, materialised here so the compiled loop
    /// sees two equal-length slices.
    fn each2_ints_jit(&mut self, g: &Value, x: &Value, y: &Value) -> Option<R<Value>> {
        let (code, caps): (Arc<crate::value::FnCode>, &[Value]) = match g {
            Lambda(c) => (c.clone(), &[]),
            Closure(c, caps) => (c.clone(), caps.as_slice()),
            _ => return None,
        };
        if code.params.len() != 2 || !caps.is_empty() || self.recorder.is_some() { return None; }
        let (xs, ys): (Vec<i64>, Vec<i64>) = match (x, y) {
            (Ints(a), Ints(b)) if a.len() == b.len() => (a.as_ref().clone(), b.as_ref().clone()),
            (Int(a), Ints(b)) => (vec![*a; b.len()], b.as_ref().clone()),
            (Ints(a), Int(b)) => (a.as_ref().clone(), vec![*b; a.len()]),
            _ => return None,
        };
        if xs.is_empty() { return None; }
        let compiled = code.jitted_n(self, xs.len())?;
        compiled.try_run_each2_int(&xs, &ys, self).map(Ok)
    }

    /// `f each x` where x is an int vector and the method JIT has compiled f to scalar code.
    ///
    /// `each_lambda` below took the per-element cost of entering the interpreter out of the adverb;
    /// this takes the interpreter out of it altogether, running the compiled body over the raw
    /// elements with none of the boxing `seq` does on the way in or the type scan `pack` does on
    /// the way out. It answers None for every shape it does not handle — a capture, a vector slot,
    /// a float, a null — and the paths below pick those up unchanged.
    fn each_ints_jit(&mut self, g: &Value, x: &Value) -> Option<R<Value>> {
        let Ints(xs) = x else { return None };
        let (code, caps): (Arc<crate::value::FnCode>, &[Value]) = match g {
            Lambda(c) => (c.clone(), &[]),
            Closure(c, caps) => (c.clone(), caps.as_slice()),
            _ => return None,
        };
        if code.params.len() != 1 || !caps.is_empty() || self.recorder.is_some() { return None; }
        if xs.is_empty() { return None; }
        let compiled = code.jitted_n(self, xs.len())?;
        let xs = xs.clone();   // `self` is about to be borrowed mutably; the Arc bump is once, not per element
        compiled.try_run_each_int(&xs, self).map(Ok)
    }

    /// `f each x` where f is a one-parameter lambda, which is most of them.
    ///
    /// The general path calls `monad` per element and so reaches `call_code`, which redoes for
    /// every single element work that cannot change between them: the arity and projection checks,
    /// the JIT cache lookup with its atomic increment, and a round trip through the locals pool.
    /// Measured on a 100k vector, that machinery was about 40ns an element, against roughly 8 for
    /// the adverb itself and 18 for the body of `{x+1}` — the call, not the work. `(neg) each x`,
    /// which is a primitive and never enters any of it, was 8.7 where `{x} each x` was 49.
    ///
    /// So: resolve the callee once, keep one locals buffer, and loop. Returns None for anything not
    /// of this shape and the caller falls back to the general path, which stays the definition of
    /// what this has to agree with.
    fn each_lambda(&mut self, g: &Value, xs: &[Value]) -> Option<R<Vec<Value>>> {
        let (code, caps): (Arc<crate::value::FnCode>, &[Value]) = match g {
            Lambda(c) => (c.clone(), &[]),
            Closure(c, caps) => (c.clone(), caps.as_slice()),
            _ => return None,
        };
        if code.params.len() != 1 { return None; }
        // while a trace is being recorded the recorder has to see each frame, which is exactly what
        // this skips (see call_code), and a Null element means call_code would build a projection
        if self.recorder.is_some() || xs.iter().any(|v| matches!(v, Null)) { return None; }
        if self.depth > MAX_DEPTH { return Some(err("stack: recursion too deep")); }
        let compiled = code.jitted_n(self, xs.len());
        let nloc = code.nlocals.max(1);
        let mut out: Vec<Value> = Vec::with_capacity(xs.len());
        let mut loc: Vec<Value> = self.pool.pop().unwrap_or_default();
        self.depth += 1;
        let mut fail = None;
        for v in xs {
            loc.clear();
            loc.push(v.clone());
            loc.resize(nloc, Null);
            loc.extend_from_slice(caps);
            // a compiled version that bails out means "run it on the interpreter instead", the same
            // as it does in call_code
            let r = match compiled.as_ref().and_then(|c| c.try_run(&loc, self)) {
                Some(v) => Ok(v),
                None => self.execute(Some(&code), &code.ops, &code.consts, &code.lines, &mut loc),
            };
            match r { Ok(v) => out.push(v), Err(e) => { fail = Some(e); break; } }
        }
        self.depth -= 1;
        loc.clear(); self.pool.push(loc);
        Some(match fail { Some(e) => Err(e), None => Ok(out) })
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
                // the whole loop in compiled code, when the shapes allow it
                if let Some(r) = self.each2_ints_jit(g, &x, &y) { return r; }
                let (mut xs, mut ys) = (x.seq(), y.seq());
                if xs.len() == 1 && ys.len() > 1 { xs = vec![xs[0].clone(); ys.len()]; }
                if ys.len() == 1 && xs.len() > 1 { ys = vec![ys[0].clone(); xs.len()]; }
                if xs.len() != ys.len() { return err("length: each"); }
                Ok(pack(xs.into_iter().zip(ys).map(|(a, b)| self.dyad(g, a, b)).collect::<R<Vec<_>>>()?))
            }
        }
    }
}

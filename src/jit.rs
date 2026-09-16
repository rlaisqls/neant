//! A conservative, hand-rolled AArch64 baseline JIT for integer-only hot loops and calls.
//!
//! Scope (see README "Stage 2"): compiles a whole `FnCode` to native code only if every op in it
//! is provably a pure integer computation over its own locals — arithmetic, comparison,
//! `Jmp`/`Jmpf`/`Loop` control flow (`while`, `if`, `do`), and a call to another function that is
//! *itself* provably pure the same way (see `jit_call` below). Anything else is rejected once,
//! permanently, for that function. A compiled function is guarded at entry (every arg and capture
//! must be a plain non-null `Int`) and can still bail out mid-run (`deopt`) the couple of places a
//! plain integer operation can't just trust things blindly (a wrapped-to-null coincidence, an
//! impure or arity-mismatched callee, runaway recursion). On any doubt, the caller
//! (`FnCode::jitted`, src/value.rs) just falls back to the bytecode interpreter, which is always
//! correct and always present.
//!
//! **Codegen lives in neant, not here** (`src/neant/jit/arm64.nt`) — the compilability walk and
//! the instruction encoders are pure computation over bytecode-as-data, the same kind of job
//! `src/neant/core/compile.nt` already does, so it's self-hosted the same way. What's left here is
//! only what genuinely needs the host: executable memory (`mmap`/`mprotect`/`__clear_cache`,
//! declared directly as `extern "C"` — already linked in via libc/libgcc, so `Cargo.toml` stays
//! empty), owning it (`Compiled`), and the trampoline a compiled call reaches through `blr` to get
//! back into the interpreter. `compile()` below is the one place these two halves meet: it calls
//! the neant codegen function (`FnCode::jit_now`, src/value.rs, does the actual call — this module
//! only turns whatever `Bytes` comes back into executable memory).
//!
//! **Calling another compiled function.** A compiled function that calls something is no longer
//! pure by inspection alone — it's pure only if the callee is too, and if it isn't, calling it and
//! then later deopting (re-running the *caller* from scratch on the interpreter) would invoke the
//! callee a second time, corrupting any real side effect it had. So `jit_call` (the one fixed
//! trampoline every compiled call site reaches via `blr`) proves the callee is equally pure —
//! by literally attempting to compile it too (`FnCode::jit_for_call`, unlocked by the same
//! compilability check as everything else) — *before* making the call at all. If that fails, no
//! call happens and the compiled caller deopts immediately, and the interpreter makes that one
//! real call itself, correctly and exactly once. Compiling that callee now means running the neant
//! codegen function, which needs `&mut Vm` — this trampoline holds `*mut Vm` (not `*const Vm`) for
//! exactly that reason; see `FnCode::jit_now`'s doc comment for the aliasing argument.

use crate::value::{FnCode, Value};
use crate::vm::Vm;
use std::sync::Arc;

#[cfg(target_arch = "aarch64")]
mod arm64 {
    use super::*;

    /// One compiled function: owns its executable memory and knows how to call into it. Dropping
    /// it unmaps the memory — safe because nothing calls in once the owning `FnCode`'s cached
    /// `Arc<Compiled>` (src/value.rs) has been dropped, since that's the only handle to it.
    pub struct Compiled {
        mem: *mut u8,
        len: usize,
        arity: usize,
        nlocals: usize,
        /// Param/capture slots `src/neant/jit/arm64.nt`'s `jitClassifySlots` proved are only ever
        /// read via `x[i]` or written via `x[i]:v` — see `try_run`/`try_run_raw` below for how
        /// these get a vector pointer instead of a plain int at entry.
        vec_slots: Vec<usize>,
        entry: unsafe extern "C" fn(*mut i64, *mut i64, *mut Vm) -> i64,
    }
    unsafe impl Send for Compiled {}
    unsafe impl Sync for Compiled {}
    impl Drop for Compiled {
        fn drop(&mut self) { unsafe { munmap(self.mem as *mut std::ffi::c_void, self.len); } }
    }

    unsafe extern "C" {
        fn mmap(addr: *mut std::ffi::c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut std::ffi::c_void;
        fn munmap(addr: *mut std::ffi::c_void, len: usize) -> i32;
        fn mprotect(addr: *mut std::ffi::c_void, len: usize, prot: i32) -> i32;
        fn __clear_cache(start: *mut std::ffi::c_char, end: *mut std::ffi::c_char);
    }
    const PROT_READ: i32 = 1;
    const PROT_WRITE: i32 = 2;
    const PROT_EXEC: i32 = 4;
    const MAP_PRIVATE: i32 = 0x02;
    const MAP_ANONYMOUS: i32 = 0x20;

    /// Locals buffers stay on the native stack, not the heap — real functions have a handful of
    /// locals/captures, and a fixed cap here is what lets both entry points below skip allocating
    /// at all; `jit_call` (the recursive-call path) runs this once per call, so this is the
    /// difference between "a loop iteration's cost" and "a loop iteration's cost plus a malloc".
    const MAX_SLOTS: usize = 64;
    /// A vector-classified local's slot in `buf` holds a pointer into this side table instead of
    /// a plain int (see `try_run`/`try_run_raw`) — capped separately from `MAX_SLOTS` since real
    /// functions have at most a couple of vector params/captures, never dozens.
    const MAX_VEC_SLOTS: usize = 8;

    impl Compiled {
        /// `None` means the entry guard failed (some arg/capture isn't the type its slot was
        /// classified as — plain non-null int, or `Ints` for a `vec_slots` entry), the compiled
        /// body deopted partway through, or there were more locals/captures than `MAX_SLOTS`/
        /// `MAX_VEC_SLOTS` (generous for anything realistic) — all of which just mean "run the
        /// interpreter instead". Nothing observable has happened yet *by this function*:
        /// everything it calls is independently proven pure the same way (see the module doc
        /// comment), so re-running from scratch is always safe.
        pub fn try_run(&self, loc: &[Value], vm: &mut Vm) -> Option<Value> {
            if loc.len() > MAX_SLOTS || self.vec_slots.len() > MAX_VEC_SLOTS { return None; }
            // args (0..arity) and captures (nlocals..) must already be the right type; the slots
            // in between are the interpreter's own Null-until-first-StoreL scratch locals, which
            // the compilability check guarantees are always written before they're read (and,
            // separately, never classified as vector slots — see jitClassifySlots's own comment).
            let mut buf = [0i64; MAX_SLOTS];
            // Cloned (Arc bump) `Ints` values a vector-classified slot's pointer aims at, kept
            // alive here for exactly as long as `buf`'s pointers into it need to be — this same
            // stack frame, for the whole duration of `run` below.
            let mut vecbuf: [Value; MAX_VEC_SLOTS] = std::array::from_fn(|_| Value::Null);
            let mut vk = 0usize;
            for (i, v) in loc.iter().enumerate() {
                if i < self.arity || i >= self.nlocals {
                    if self.vec_slots.contains(&i) {
                        let Value::Ints(_) = v else { return None };
                        vecbuf[vk] = v.clone();
                        buf[i] = &mut vecbuf[vk] as *mut Value as i64;
                        vk += 1;
                    } else {
                        match v { Value::Int(n) if *n != crate::value::NI => buf[i] = *n, _ => return None }
                    }
                }
            }
            self.run(buf.as_mut_ptr(), vm).map(Value::Int)
        }
        /// The direct recursive-call path (`jit_call` below): args are already known to be plain
        /// ints (they came from another compiled function's own int-typed registers, per the same
        /// invariant that makes any of this sound), so this skips `try_run`'s general `Value`
        /// boxing/unboxing and the heap allocation a `Vec` would otherwise need — the args and the
        /// captures it still has to validate go straight into a stack buffer. A callee whose own
        /// *parameter* is vector-classified can't be entered this way at all — `jit_call` only
        /// ever has plain ints for `arg0`/`arg1`, never a vector pointer to hand over, since this
        /// slot's classification is a fact about the callee's own body, invisible to whichever
        /// other compiled function is calling it — so that one call just deopts instead, same as
        /// any other guarded point. Captures, unlike args, are real `Value`s here (from the
        /// callee's own closure) and get the same vector-slot treatment `try_run` gives them.
        fn try_run_raw(&self, argc: usize, arg0: i64, arg1: i64, caps: &[Value], vm: &mut Vm) -> Option<i64> {
            let total = self.nlocals.max(self.arity) + caps.len();
            if total > MAX_SLOTS || self.vec_slots.len() > MAX_VEC_SLOTS { return None; }
            if self.vec_slots.iter().any(|&s| s < argc) { return None; }
            let mut buf = [0i64; MAX_SLOTS];
            let mut vecbuf: [Value; MAX_VEC_SLOTS] = std::array::from_fn(|_| Value::Null);
            let mut vk = 0usize;
            if argc >= 1 { buf[0] = arg0; }
            if argc >= 2 { buf[1] = arg1; }
            let capbase = self.nlocals.max(self.arity);
            for (i, v) in caps.iter().enumerate() {
                let slot = capbase + i;
                if self.vec_slots.contains(&slot) {
                    let Value::Ints(_) = v else { return None };
                    vecbuf[vk] = v.clone();
                    buf[slot] = &mut vecbuf[vk] as *mut Value as i64;
                    vk += 1;
                } else {
                    match v { Value::Int(n) if *n != crate::value::NI => buf[slot] = *n, _ => return None }
                }
            }
            self.run(buf.as_mut_ptr(), vm)
        }
        fn run(&self, buf: *mut i64, vm: &mut Vm) -> Option<i64> {
            let mut ok: i64 = 0;
            let val = unsafe { (self.entry)(buf, &mut ok as *mut i64, vm as *mut Vm) };
            if ok != 0 { Some(val) } else { None }
        }
    }

    thread_local! { static CALL_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; }
    /// Compiled-to-compiled calls go through `blr`, not `Vm::call_code`, so they'd otherwise blow
    /// the real machine stack (a hard crash) instead of failing like every other kind of runaway
    /// recursion in this language does — needs its own limit for the same reason `call_code`'s
    /// `self.depth` guard (src/vm.rs) needs one, and a lower one: `try_run_raw`'s `[i64; MAX_SLOTS]`
    /// stack buffer makes each level here heavier than a plain interpreted call. Calibrated the
    /// same way — empirically, against the smallest stack this can run on (a `spawn`ed thread
    /// defaults to 2MiB) — with real margin below where it actually overflows.
    const MAX_CALL_DEPTH: u32 = 500;

    /// The one fixed trampoline every compiled call site reaches via `blr` (see the module doc
    /// comment for why proving the callee pure *before* calling it is the safety argument here,
    /// and why `vm` is `*mut` — compiling an unseen callee here runs the neant codegen function,
    /// which needs `&mut Vm`). `vm`/`slot` resolve the callee exactly like `Op::LoadG`; `argc`/
    /// `arg0`/`arg1` are its already int-typed arguments (this language caps calls at two); `ok`
    /// is a scratch flag the caller reads right after the `blr` returns.
    unsafe extern "C" fn jit_call(vm: *mut Vm, slot: i64, argc: i64, arg0: i64, arg1: i64, ok: *mut i64) -> i64 {
        let depth = CALL_DEPTH.with(|d| { let n = d.get() + 1; d.set(n); n });
        struct Guard;
        impl Drop for Guard { fn drop(&mut self) { CALL_DEPTH.with(|d| d.set(d.get() - 1)); } }
        let _guard = Guard;
        let fail = |ok: *mut i64| unsafe { *ok = 0; 0 };
        if depth > MAX_CALL_DEPTH { return fail(ok); }
        let vm = unsafe { &mut *vm };
        let Some(f) = vm.global_at(slot as usize) else { return fail(ok) };
        let (code, caps): (&Arc<FnCode>, &[Value]) = match &f {
            Value::Lambda(c) => (c, &[]),
            Value::Closure(c, caps) => (c, caps),
            _ => return fail(ok), // calling a primitive from compiled code stays out of scope
        };
        if code.params.len() != argc as usize { return fail(ok); }
        let Some(compiled) = code.jit_for_call(vm) else { return fail(ok) };
        match compiled.try_run_raw(argc as usize, arg0, arg1, caps, vm) {
            Some(v) => unsafe { *ok = 1; v }
            None => fail(ok),
        }
    }

    /// `x[i]` from compiled code (`jitOpVecGet`, `src/neant/jit/arm64.nt`): `vp` is a pointer into
    /// the `vecbuf` side table `try_run`/`try_run_raw` populated at entry, live for the whole
    /// compiled call. All `Arc`/COW handling stays here in Rust rather than being inlined as
    /// hand-rolled AArch64 pointer arithmetic — see README "Stage 2" for why. Bounds-checked; out
    /// of range, or (shouldn't happen given the entry guard, but checked anyway) not actually
    /// `Ints`, both deopt like any other guarded point in a compiled function.
    unsafe extern "C" fn jit_vec_get(vp: *mut Value, idx: i64, ok: *mut i64) -> i64 {
        let v = unsafe { &*vp };
        let Value::Ints(items) = v else { unsafe { *ok = 0 }; return 0 };
        if idx < 0 || idx as usize >= items.len() { unsafe { *ok = 0 }; return 0 }
        unsafe { *ok = 1 };
        items[idx as usize]
    }

    /// `x[i]:v` from compiled code (`jitOpVecSet`): the same copy-on-write discipline the
    /// interpreter's own `Op::Amend`/`scatter` (`src/prims.rs`) use — `Arc::make_mut` clones only
    /// if this vector isn't uniquely owned, so a second live reference to the same vector never
    /// observes the write.
    unsafe extern "C" fn jit_vec_set(vp: *mut Value, idx: i64, val: i64, ok: *mut i64) {
        let v = unsafe { &mut *vp };
        let Value::Ints(items) = v else { unsafe { *ok = 0 }; return };
        if idx < 0 || idx as usize >= items.len() { unsafe { *ok = 0 }; return }
        Arc::make_mut(items)[idx as usize] = val;
        unsafe { *ok = 1 };
    }

    thread_local! { static COMPILING: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; }
    /// True while a `jitCompile` call is already on the Rust stack (see `compile` below) — checked
    /// by `FnCode::jit_now` (src/value.rs) before it will even attempt a new one.
    pub(crate) fn already_compiling() -> bool { COMPILING.with(|c| c.get() > 0) }

    pub fn compile(code: &Arc<FnCode>, vm: &mut Vm) -> Option<super::Compiled> {
        let f = vm.get("jitCompile")?;
        // neant has no way to take a Rust function's address itself — hand it over explicitly, the
        // same way the old Rust encoder used to compute `jit_call`'s address inline.
        let trampolines = Value::List(Arc::new(vec![
            Value::Int(jit_call as *const () as i64),
            Value::Int(jit_vec_get as *const () as i64),
            Value::Int(jit_vec_set as *const () as i64),
        ]));
        // `jitCompile` and its own helpers (jitOpDyad, ...) are neant functions too, and their own
        // bytecode contains the very op kinds they exist to handle — a Dyad inside `jitOpDyad`'s own
        // body, for instance. So compiling *any* of them, once its own call count crosses the
        // threshold, means *calling* it as part of walking its own bytecode — reentering its own
        // `OnceLock` from inside that same `OnceLock`'s initializer. `already_compiling` (checked in
        // `jit_now`) shuts that off for the whole nested call, not just the exact function that
        // would recurse: the JIT's own implementation is never a JIT target, full stop — it only
        // ever affects how long one-time compilation takes, never a compiled function's own speed.
        COMPILING.with(|c| c.set(c.get() + 1));
        let result = vm.call(&f, vec![code.jit_input(), trampolines]);
        COMPILING.with(|c| c.set(c.get() - 1));
        // Success is `(bytes; vecSlots)` — the classified param/capture slots (`jitClassifySlots`)
        // ride along so the entry guard below knows which ones need `Ints`, not a plain int.
        let (bytes, vec_slots) = match result {
            Ok(Value::List(items)) if items.len() == 2 => {
                let bytes = match &items[0] { Value::Bytes(b) => b.clone(), _ => return None };
                let vec_slots: Vec<usize> = match &items[1] {
                    Value::Ints(v) => v.iter().map(|&n| n as usize).collect(),
                    _ => return None,
                };
                (bytes, vec_slots)
            }
            _ => return None, // Null (not compilable) or a runtime error in the codegen itself
        };
        emit(&bytes, code.params.len(), code.nlocals, vec_slots)
    }

    fn emit(bytes: &[u8], arity: usize, nlocals: usize, vec_slots: Vec<usize>) -> Option<super::Compiled> {
        if vec_slots.len() > MAX_VEC_SLOTS { return None; }
        let page = 4096usize;
        let len = bytes.len().div_ceil(page) * page;
        unsafe {
            let mem = mmap(std::ptr::null_mut(), len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            if mem as isize == -1 { return None; }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), mem as *mut u8, bytes.len());
            if mprotect(mem, len, PROT_READ | PROT_EXEC) != 0 { munmap(mem, len); return None; }
            __clear_cache(mem as *mut std::ffi::c_char, (mem as *mut u8).add(len) as *mut std::ffi::c_char);
            let entry: unsafe extern "C" fn(*mut i64, *mut i64, *mut Vm) -> i64 = std::mem::transmute(mem);
            Some(super::Compiled { mem: mem as *mut u8, len, arity, nlocals, vec_slots, entry })
        }
    }

    /// A compiled trace (tracing JIT Milestone 1, `src/trace.rs`): a hot `while` loop's body,
    /// running natively until a guard disagrees with what was recorded. Unlike `Compiled` above,
    /// there's no "return" — every exit is a bail, handing back exactly where in the original
    /// bytecode to resume interpreting and the current value of every local the trace touched, so
    /// `Vm::run_ops` can just splice them into its own `loc`/`ip` and keep going, indistinguishable
    /// from having interpreted the whole time. Sound because recording only ever *observes* —
    /// nothing here can make an already-correct program produce a different result, only run some
    /// of it faster.
    pub struct CompiledTrace {
        mem: *mut u8,
        len: usize,
        /// Every local this trace reads or writes, in the order codegen laid them out in the
        /// buffer — `run()` marshals exactly these, in this order, both in and out. Taken from
        /// `jitCompileTrace`'s own result rather than recomputed here: the layout has to mean the
        /// same thing on both sides, and one side deciding it is what guarantees that. A slot at
        /// or past `real_upto` belongs to an inlined callee's frame (src/trace.rs) and is skipped
        /// both ways: it has no value before the loop and none the interpreter wants after it.
        touched: Vec<(usize, crate::trace::TraceTy)>,
        real_upto: usize,
        /// Every global an inlined callee was loaded from, and the lambda it held when the trace
        /// was recorded. Checked at entry, once: nothing a compiled trace runs can assign a global.
        callees: Vec<(usize, Arc<FnCode>)>,
        entry: unsafe extern "C" fn(*mut i64, *mut i64),
    }
    unsafe impl Send for CompiledTrace {}
    unsafe impl Sync for CompiledTrace {}
    impl Drop for CompiledTrace {
        fn drop(&mut self) { unsafe { munmap(self.mem as *mut std::ffi::c_void, self.len); } }
    }
    /// Generous but fixed, same choice `MAX_SLOTS`/`MAX_VEC_SLOTS` already make: a loop body with
    /// more than this many distinct locals live in it just doesn't get traced. Mirrored as
    /// `jitTrMAXTOUCHED` (src/neant/jit/arm64.nt), which rejects the trace before emitting any
    /// code that would index past the buffer.
    const MAX_TOUCHED: usize = 16;

    impl CompiledTrace {
        /// Runs the trace until a guard bails, writes every touched local's new value back into
        /// `loc`, and returns the bytecode `ip` to resume interpreting from (`Bailed`). The two
        /// refusals are the entry guards: `TypeMismatch` means the *current* values in `loc` don't
        /// match the types this trace was compiled assuming (the same kind of guard `try_run`'s
        /// entry check already makes for the method-JIT) — same fallback as everywhere else here,
        /// just don't use the compiled version this time. `StaleCallee` means a global this trace
        /// inlined a callee from has been reassigned since it was recorded, which no later entry
        /// can undo — the caller retires the trace (`FnCode::retrace`, src/value.rs).
        pub fn run(&self, loc: &mut [Value], vm: &Vm) -> TraceRun {
            for (slot, code) in &self.callees {
                match vm.global_at(*slot) { Some(Value::Lambda(c)) if Arc::ptr_eq(&c, code) => {}, _ => return TraceRun::StaleCallee }
            }
            // Only how the locals get in and out: the compiled code keeps them in registers for
            // the whole loop and touches this again just once per iteration, to leave behind the
            // values the iteration started with for its one mid-iteration deopt to resume from.
            let mut buf = [0i64; MAX_TOUCHED];
            for (i, (slot, ty)) in self.touched.iter().enumerate() {
                if *slot >= self.real_upto { continue; }
                let Some(v) = loc.get(*slot) else { return TraceRun::TypeMismatch };
                buf[i] = match (ty, v) {
                    // A null int is rejected, not passed through: the interpreter propagates it
                    // through arithmetic and compiled code does plain wrapping arithmetic — the
                    // same entry guard `try_run` makes, for the same reason.
                    (crate::trace::TraceTy::Int, Value::Int(n)) if *n != crate::value::NI => *n,
                    (crate::trace::TraceTy::Float, Value::Float(f)) => f.to_bits() as i64,
                    _ => return TraceRun::TypeMismatch,
                };
            }
            let mut bail_ip: i64 = 0;
            unsafe { (self.entry)(buf.as_mut_ptr(), &mut bail_ip as *mut i64); }
            for (i, (slot, ty)) in self.touched.iter().enumerate() {
                if *slot >= self.real_upto { continue; }
                loc[*slot] = match ty {
                    crate::trace::TraceTy::Int => Value::Int(buf[i]),
                    crate::trace::TraceTy::Float => Value::Float(f64::from_bits(buf[i] as u64)),
                };
            }
            TraceRun::Bailed(bail_ip as usize)
        }
    }

    pub fn compile_trace(trace: &crate::trace::Trace, vm: &mut Vm) -> Option<CompiledTrace> {
        let f = vm.get("jitCompileTrace")?;
        let (kinds, args, tys) = trace.to_neant_input();
        let input = Value::List(Arc::new(vec![
            crate::value::ints(kinds), crate::value::ints(args), crate::value::ints(tys),
            crate::value::list(trace.consts.clone()), Value::Int(trace.header as i64),
            Value::Int(trace.real_upto as i64),
        ]));
        // Same reentrancy guard `compile` above uses: `jitCompileTrace`'s own helpers are neant
        // functions too and could cross the JIT threshold while compiling themselves.
        COMPILING.with(|c| c.set(c.get() + 1));
        let result = vm.call(&f, vec![input]);
        COMPILING.with(|c| c.set(c.get() - 1));
        // Success is `(bytes; slots; tys)` — the buffer layout rides along with the code that was
        // built around it (see `CompiledTrace::touched`). Null (not compilable) or an error in the
        // codegen itself both just mean this loop stays interpreted.
        let Ok(Value::List(items)) = result else { return None };
        if items.len() != 3 { return None; }
        let Value::Bytes(bytes) = &items[0] else { return None };
        let slots = int_vec(&items[1])?;
        let tys = int_vec(&items[2])?;
        if slots.len() != tys.len() || slots.len() > MAX_TOUCHED { return None; }
        let touched = slots.iter().zip(&tys)
            .map(|(&s, &t)| (s as usize, if t == 1 { crate::trace::TraceTy::Float } else { crate::trace::TraceTy::Int }))
            .collect();
        emit_trace(bytes, touched, trace.real_upto as usize, trace.callees.clone())
    }

    /// An `Ints` result as a `Vec<i64>`. A neant vector that happens to be empty comes back as an
    /// empty general list, not an empty `Ints`, so that case is spelled out rather than rejected.
    fn int_vec(v: &Value) -> Option<Vec<i64>> {
        match v {
            Value::Ints(xs) => Some(xs.to_vec()),
            Value::List(xs) if xs.is_empty() => Some(Vec::new()),
            _ => None,
        }
    }

    fn emit_trace(bytes: &[u8], touched: Vec<(usize, crate::trace::TraceTy)>, real_upto: usize, callees: Vec<(usize, Arc<FnCode>)>) -> Option<CompiledTrace> {
        let page = 4096usize;
        let len = bytes.len().div_ceil(page) * page;
        unsafe {
            let mem = mmap(std::ptr::null_mut(), len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            if mem as isize == -1 { return None; }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), mem as *mut u8, bytes.len());
            if mprotect(mem, len, PROT_READ | PROT_EXEC) != 0 { munmap(mem, len); return None; }
            __clear_cache(mem as *mut std::ffi::c_char, (mem as *mut u8).add(len) as *mut std::ffi::c_char);
            let entry: unsafe extern "C" fn(*mut i64, *mut i64) = std::mem::transmute(mem);
            Some(CompiledTrace { mem: mem as *mut u8, len, touched, real_upto, callees, entry })
        }
    }
}

#[cfg(target_arch = "aarch64")]
pub(crate) use arm64::already_compiling;
#[cfg(target_arch = "aarch64")]
pub use arm64::{compile as compile_arm64, compile_trace as compile_trace_arm64, Compiled, CompiledTrace};

/// What `CompiledTrace::run` came back with — see its doc comment.
pub enum TraceRun { Bailed(usize), TypeMismatch, StaleCallee }

#[cfg(target_arch = "aarch64")]
pub fn compile(code: &Arc<FnCode>, vm: &mut Vm) -> Option<Compiled> { compile_arm64(code, vm) }
#[cfg(target_arch = "aarch64")]
pub fn compile_trace(trace: &crate::trace::Trace, vm: &mut Vm) -> Option<CompiledTrace> {
    compile_trace_arm64(trace, vm)
}

#[cfg(not(target_arch = "aarch64"))]
pub(crate) fn already_compiling() -> bool { false }
#[cfg(not(target_arch = "aarch64"))]
pub struct Compiled(std::convert::Infallible);
#[cfg(not(target_arch = "aarch64"))]
impl Compiled {
    pub fn try_run(&self, _loc: &[Value], _vm: &mut Vm) -> Option<Value> { match self.0 {} }
}
#[cfg(not(target_arch = "aarch64"))]
pub fn compile(_code: &Arc<FnCode>, _vm: &mut Vm) -> Option<Compiled> { None }
#[cfg(not(target_arch = "aarch64"))]
pub struct CompiledTrace(std::convert::Infallible);
#[cfg(not(target_arch = "aarch64"))]
impl CompiledTrace {
    pub fn run(&self, _loc: &mut [Value], _vm: &Vm) -> TraceRun { match self.0 {} }
}
#[cfg(not(target_arch = "aarch64"))]
pub fn compile_trace(_trace: &crate::trace::Trace, _vm: &mut Vm) -> Option<CompiledTrace> { None }

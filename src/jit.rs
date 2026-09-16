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

    impl Compiled {
        /// `None` means the entry guard failed (some arg/capture isn't a plain non-null int), the
        /// compiled body deopted partway through, or there were more locals/captures than
        /// `MAX_SLOTS` (generous for anything realistic) — all of which just mean "run the
        /// interpreter instead". Nothing observable has happened yet *by this function*:
        /// everything it calls is independently proven pure the same way (see the module doc
        /// comment), so re-running from scratch is always safe.
        pub fn try_run(&self, loc: &[Value], vm: &mut Vm) -> Option<Value> {
            if loc.len() > MAX_SLOTS { return None; }
            // args (0..arity) and captures (nlocals..) must already be plain, non-null ints; the
            // slots in between are the interpreter's own Null-until-first-StoreL scratch locals,
            // which the compilability check guarantees are always written before they're read.
            let mut buf = [0i64; MAX_SLOTS];
            for (i, v) in loc.iter().enumerate() {
                if i < self.arity || i >= self.nlocals {
                    match v { Value::Int(n) if *n != crate::value::NI => buf[i] = *n, _ => return None }
                }
            }
            self.run(buf.as_mut_ptr(), vm).map(Value::Int)
        }
        /// The direct recursive-call path (`jit_call` below): args are already known to be plain
        /// ints (they came from another compiled function's own int-typed registers, per the same
        /// invariant that makes any of this sound), so this skips `try_run`'s general `Value`
        /// boxing/unboxing and the heap allocation a `Vec` would otherwise need — the args and the
        /// captures it still has to validate go straight into a stack buffer.
        fn try_run_raw(&self, argc: usize, arg0: i64, arg1: i64, caps: &[Value], vm: &mut Vm) -> Option<i64> {
            let total = self.nlocals.max(self.arity) + caps.len();
            if total > MAX_SLOTS { return None; }
            let mut buf = [0i64; MAX_SLOTS];
            if argc >= 1 { buf[0] = arg0; }
            if argc >= 2 { buf[1] = arg1; }
            for (i, v) in caps.iter().enumerate() {
                match v { Value::Int(n) if *n != crate::value::NI => buf[self.nlocals.max(self.arity) + i] = *n, _ => return None }
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

    thread_local! { static COMPILING: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; }
    /// True while a `jitCompile` call is already on the Rust stack (see `compile` below) — checked
    /// by `FnCode::jit_now` (src/value.rs) before it will even attempt a new one.
    pub(crate) fn already_compiling() -> bool { COMPILING.with(|c| c.get() > 0) }

    pub fn compile(code: &Arc<FnCode>, vm: &mut Vm) -> Option<super::Compiled> {
        let f = vm.get("jitCompile")?;
        // neant has no way to take a Rust function's address itself — hand it over explicitly, the
        // same value the old Rust encoder used to compute inline (`jit_call as *const () as i64`).
        let trampoline = Value::Int(jit_call as *const () as i64);
        // `jitCompile` and its own helpers (jitOpDyad, ...) are neant functions too, and their own
        // bytecode contains the very op kinds they exist to handle — a Dyad inside `jitOpDyad`'s own
        // body, for instance. So compiling *any* of them, once its own call count crosses the
        // threshold, means *calling* it as part of walking its own bytecode — reentering its own
        // `OnceLock` from inside that same `OnceLock`'s initializer. `already_compiling` (checked in
        // `jit_now`) shuts that off for the whole nested call, not just the exact function that
        // would recurse: the JIT's own implementation is never a JIT target, full stop — it only
        // ever affects how long one-time compilation takes, never a compiled function's own speed.
        COMPILING.with(|c| c.set(c.get() + 1));
        let result = vm.call(&f, vec![code.jit_input(), trampoline]);
        COMPILING.with(|c| c.set(c.get() - 1));
        let bytes = match result {
            Ok(Value::Bytes(b)) => b,
            _ => return None, // Null (not compilable) or a runtime error in the codegen itself
        };
        emit(&bytes, code.params.len(), code.nlocals)
    }

    fn emit(bytes: &[u8], arity: usize, nlocals: usize) -> Option<super::Compiled> {
        let page = 4096usize;
        let len = bytes.len().div_ceil(page) * page;
        unsafe {
            let mem = mmap(std::ptr::null_mut(), len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            if mem as isize == -1 { return None; }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), mem as *mut u8, bytes.len());
            if mprotect(mem, len, PROT_READ | PROT_EXEC) != 0 { munmap(mem, len); return None; }
            __clear_cache(mem as *mut std::ffi::c_char, (mem as *mut u8).add(len) as *mut std::ffi::c_char);
            let entry: unsafe extern "C" fn(*mut i64, *mut i64, *mut Vm) -> i64 = std::mem::transmute(mem);
            Some(super::Compiled { mem: mem as *mut u8, len, arity, nlocals, entry })
        }
    }
}

#[cfg(target_arch = "aarch64")]
pub(crate) use arm64::already_compiling;
#[cfg(target_arch = "aarch64")]
pub use arm64::{compile as compile_arm64, Compiled};

#[cfg(target_arch = "aarch64")]
pub fn compile(code: &Arc<FnCode>, vm: &mut Vm) -> Option<Compiled> { compile_arm64(code, vm) }

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

//! A conservative, hand-rolled baseline JIT for integer-only hot loops and calls, on AArch64 and
//! x86-64.
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
//! **Codegen lives in neant, not here** — `src/neant/jit/arm64.nt` for AArch64 and
//! `src/neant/jit/x86.nt` for x86-64. The compilability walk and the instruction encoders are pure
//! computation over bytecode-as-data, the same kind of job `src/neant/core/compile.nt` already
//! does, so it's self-hosted the same way; the walk itself is literally the same code for both
//! targets (the x86-64 file calls arm64.nt's), so the two backends accept and reject exactly the
//! same functions and traces. What's left here is only what genuinely needs the host: executable
//! memory (`mmap`/`mprotect`, plus `__clear_cache` where the architecture needs it, declared
//! directly as `extern "C"` — already linked in via libc/libgcc, so `Cargo.toml` stays empty),
//! owning it (`Compiled`), and the trampoline a compiled call reaches through an indirect call
//! (`blr` on AArch64, `call` on x86-64) to get back into the interpreter. `compile()` below is the
//! one place these two halves meet: it calls this target's neant codegen function (`CODEGEN`;
//! `FnCode::jit_now`, src/value.rs, does the actual call — this module only turns whatever `Bytes`
//! comes back into executable memory).
//!
//! **Only the AArch64 backend has ever executed.** Both are compiled and both are covered by tests
//! that check what can be checked without running (README "Stage 2", x86-64 subsection): the
//! development machine is AArch64, so the x86-64 encoders are verified against `nasm`'s bytes and
//! the glue here against `cargo check --target x86_64-unknown-linux-gnu`, not against a running
//! program. Everything below is fail-closed either way — a backend that returns `None`, or a
//! codegen function that isn't in the boot image, just leaves the interpreter to do the work.
//!
//! **Calling another compiled function.** A compiled function that calls something is no longer
//! pure by inspection alone — it's pure only if the callee is too, and if it isn't, calling it and
//! then later deopting (re-running the *caller* from scratch on the interpreter) would invoke the
//! callee a second time, corrupting any real side effect it had. So `jit_call` (the one fixed
//! trampoline every compiled call site reaches through an indirect call) proves the callee is
//! equally pure —
//! by literally attempting to compile it too (`FnCode::jit_for_call`, unlocked by the same
//! compilability check as everything else) — *before* making the call at all. If that fails, no
//! call happens and the compiled caller deopts immediately, and the interpreter makes that one
//! real call itself, correctly and exactly once. Compiling that callee now means running the neant
//! codegen function, which needs `&mut Vm` — this trampoline holds `*mut Vm` (not `*const Vm`) for
//! exactly that reason; see `FnCode::jit_now`'s doc comment for the aliasing argument.

use crate::value::{FnCode, Value};
use crate::vm::Vm;
use std::sync::Arc;

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
mod native {
    use super::*;

    /// This target's two neant codegen entry points, `(whole function, trace)`. Same contract on
    /// both — bytecode as data in, a `Bytes` of machine code (plus the layout the other half has
    /// to agree on) or null out — so this is the only place in the module that the target
    /// architecture is named at all, apart from the cache flush in `emit`/`emit_trace`.
    #[cfg(target_arch = "aarch64")]
    const CODEGEN: (&str, &str) = ("jitCompile", "jitCompileTrace");
    #[cfg(target_arch = "x86_64")]
    const CODEGEN: (&str, &str) = ("jitCompileX86", "jitCompileTraceX86");

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
        /// The subset of `vec_slots` the body writes (`x[i]:v`). A written slot is made uniquely
        /// owned at entry (`Arc::make_mut` on our own clone) so the inlined store below can write
        /// straight into it: copy-on-write happens once, here, instead of on the first write from
        /// inside the loop, and the data pointer then cannot move for the rest of the call.
        vec_writes: Vec<usize>,
        /// `Some(k)`: this function returns `vec_slots[k]`'s vector rather than an int — the
        /// compiled code's x0 is a placeholder and the `Value` in `vecbuf` is the real result.
        /// That is what lets a compiled function fill a vector at all, since nothing else it does
        /// is visible to its caller (locals are never written back; see `run`).
        ret_vec: Option<usize>,
        /// `(global slot; the `&'static PrimDef` that slot held when this was compiled)` for every
        /// bit builtin the body inlined as a machine instruction instead of a `jit_call`
        /// (`band`/`bor`/`bxor`/`badd`/`shl`/`shr`). Those names are ordinary globals and can be
        /// reassigned, so entry re-checks each one is still the same primitive; a rebound `band`
        /// just means this function runs interpreted from then on.
        prim_guards: Vec<(usize, i64)>,
        /// True when the codegen used the inlined-vector-access layout: a vector slot's word in
        /// `buf` is the element data pointer (not a `*mut Value`) and its length is in the
        /// descriptor area at `buf[MAX_SLOTS + k]`. False for a backend that still calls the
        /// `jit_vec_get`/`jit_vec_set` trampolines, which want the `*mut Value` there instead.
        inline_vec: bool,
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
        /// Only declared where it is needed: see the call sites in `emit`/`emit_trace`.
        #[cfg(target_arch = "aarch64")]
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
    /// `buf` is `MAX_SLOTS` local words followed by one *descriptor* word per vector slot — its
    /// element count, at `buf[MAX_SLOTS + k]` for the k'th entry of `vec_slots`. That is the whole
    /// of what an inlined `x[i]` needs beyond the data pointer already in the slot's own word: a
    /// bounds check and a scaled load, with no call. The codegen names this same base
    /// (`jitVECBASE`, src/neant/jit/arm64.nt) and the two have to agree.
    const BUFLEN: usize = MAX_SLOTS + MAX_VEC_SLOTS;

    /// The six bit builtins a compiled body can inline as one machine instruction instead of a
    /// `jit_call` — `x band y` and friends are ordinary two-argument calls to a global holding a
    /// `Value::Prim`, so without this every limb mask and every shift in a compiled loop is a
    /// trampoline call. Order is the kind code the codegen switches on; `compile` below resolves
    /// each name to its global slot and hands the six slots over, and every function that inlines
    /// one carries an entry guard that the slot still holds that exact primitive.
    const BIT_PRIMS: [&str; 6] = ["badd", "band", "bor", "bxor", "shl", "shr"];

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
            if !self.prims_intact(vm) { return None; }
            // args (0..arity) and captures (nlocals..) must already be the right type; the slots
            // in between are the interpreter's own Null-until-first-StoreL scratch locals, which
            // the compilability check guarantees are always written before they're read (and,
            // separately, never classified as vector slots — see jitClassifySlots's own comment).
            let mut buf = [0i64; BUFLEN];
            // Cloned (Arc bump) `Ints` values a vector-classified slot's pointer aims at, kept
            // alive here for exactly as long as `buf`'s pointers into it need to be — this same
            // stack frame, for the whole duration of `run` below.
            let mut vecbuf: [Value; MAX_VEC_SLOTS] = std::array::from_fn(|_| Value::Null);
            for (i, v) in loc.iter().enumerate() {
                if i < self.arity || i >= self.nlocals {
                    if let Some(k) = self.vec_slots.iter().position(|&s| s == i) {
                        let Value::Ints(_) = v else { return None };
                        vecbuf[k] = v.clone();
                        buf[i] = self.bind_vec(&mut vecbuf[k], i, &mut buf[MAX_SLOTS + k]);
                    } else {
                        match v { Value::Int(n) if *n != crate::value::NI => buf[i] = *n, _ => return None }
                    }
                }
            }
            let out = self.run(buf.as_mut_ptr(), vm)?;
            // A vector-returning body's x0 is a placeholder: the answer is the (now filled in)
            // `Value` the slot was bound to, taken out of `vecbuf` before it is dropped.
            Some(match self.ret_vec {
                Some(k) => std::mem::replace(&mut vecbuf[k], Value::Null),
                None => Value::Int(out),
            })
        }

        /// What a vector slot's word in `buf` holds, and the descriptor word beside it. Under the
        /// inlined layout that is the element data pointer and the element count: for a slot the
        /// body writes, `Arc::make_mut` first — our clone shares with the caller's value, so this
        /// is the one copy-on-write this call makes, and after it the buffer is uniquely ours and
        /// cannot be reallocated under the inlined stores. For a read-only slot nothing is copied
        /// and the `Arc` we hold is what keeps the elements from moving. A backend still using the
        /// trampolines gets the `*mut Value` it has always had instead.
        fn bind_vec(&self, v: &mut Value, slot: usize, desc: &mut i64) -> i64 {
            if !self.inline_vec { return v as *mut Value as i64; }
            let Value::Ints(items) = v else { return 0 };
            *desc = items.len() as i64;
            if self.vec_writes.contains(&slot) { Arc::make_mut(items).as_mut_ptr() as i64 }
            else { items.as_ptr() as i64 }
        }

        /// Every bit builtin this body inlined still resolves to the same primitive it did at
        /// compile time. Empty for a function that inlined none, which is the common case, so this
        /// is a length check on the hot path.
        fn prims_intact(&self, vm: &Vm) -> bool {
            self.prim_guards.iter().all(|&(slot, want)| {
                matches!(vm.global_at(slot), Some(Value::Prim(p)) if p as *const crate::value::PrimDef as i64 == want)
            })
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
            // A vector-returning callee has nothing to hand back through this path, whose whole
            // point is that a result is a plain int in a register: that call deopts instead.
            if self.ret_vec.is_some() { return None; }
            if !self.prims_intact(vm) { return None; }
            let mut buf = [0i64; BUFLEN];
            let mut vecbuf: [Value; MAX_VEC_SLOTS] = std::array::from_fn(|_| Value::Null);
            if argc >= 1 { buf[0] = arg0; }
            if argc >= 2 { buf[1] = arg1; }
            let capbase = self.nlocals.max(self.arity);
            for (i, v) in caps.iter().enumerate() {
                let slot = capbase + i;
                if let Some(k) = self.vec_slots.iter().position(|&s| s == slot) {
                    let Value::Ints(_) = v else { return None };
                    vecbuf[k] = v.clone();
                    buf[slot] = self.bind_vec(&mut vecbuf[k], slot, &mut buf[MAX_SLOTS + k]);
                } else {
                    match v { Value::Int(n) if *n != crate::value::NI => buf[slot] = *n, _ => return None }
                }
            }
            self.run(buf.as_mut_ptr(), vm)
        }
        /// Only the returned value and `ok` come back out. The compiled code loads its locals from
        /// `buf` into registers once in its prologue and never writes them back (see the
        /// locals-in-registers comment in this target's codegen file), which is sound precisely
        /// because nothing here — or in either entry point above — reads `buf` after this call.
        /// That's a fact the codegen relies on, not an accident: keep it true.
        fn run(&self, buf: *mut i64, vm: &mut Vm) -> Option<i64> {
            let mut ok: i64 = 0;
            let val = unsafe { (self.entry)(buf, &mut ok as *mut i64, vm as *mut Vm) };
            if ok != 0 { Some(val) } else { None }
        }
    }

    thread_local! { static CALL_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; }
    /// Compiled-to-compiled calls go through a native indirect call, not `Vm::call_code`, so
    /// they'd otherwise blow
    /// the real machine stack (a hard crash) instead of failing like every other kind of runaway
    /// recursion in this language does — needs its own limit for the same reason `call_code`'s
    /// `self.depth` guard (src/vm.rs) needs one, and a lower one: `try_run_raw`'s `[i64; MAX_SLOTS]`
    /// stack buffer makes each level here heavier than a plain interpreted call. Calibrated the
    /// same way — empirically, against the smallest stack this can run on (a `spawn`ed thread
    /// defaults to 2MiB) — with real margin below where it actually overflows. Re-measured after
    /// the locals moved into registers (`jitLREGS`, src/neant/jit/arm64.nt), which grew the
    /// compiled frame from 112 to 192 bytes for the callee-saved saves and the spill area. (The
    /// x86-64 frame is smaller — six pushes and a 72-byte frame, 128 bytes with the return address
    /// — so the same limit is if anything more conservative there; it has not been re-measured on
    /// x86-64 hardware, because nothing that backend emits has ever run.) With
    /// this guard lifted, `{[x] $[x<1; 0; 1+h[x-1]]}` on a 2MiB thread now overflows between 1800
    /// and 1900 levels (2000–2100 before), so 500 still leaves a >3x margin and stays.
    ///
    /// That margin is for the chain *alone*. A chain is entered from some interpreted depth and
    /// sits on top of it — and when the chain deopts at this limit, the interpreter takes one more
    /// level and the next call starts a fresh 500-deep chain, so near the interpreter's own limit
    /// the stack holds ~1000 interpreted frames *plus* a full chain. That sum is what actually
    /// overflowed a 2MiB thread once both tiers had grown a little (the register-locals frame here,
    /// the trace tier's `run_ops` changes there — each fine alone). So `jit_call` also charges the
    /// chain against the interpreter's budget (`vm::MAX_DEPTH`): the total never exceeds what 1000
    /// interpreted levels already fit, and a compiled level is the lighter of the two.
    const MAX_CALL_DEPTH: u32 = 500;

    /// The one fixed trampoline every compiled call site reaches through an indirect call —
    /// `blr x15` on AArch64, `call rax` on x86-64 (see the module doc
    /// comment for why proving the callee pure *before* calling it is the safety argument here,
    /// and why `vm` is `*mut` — compiling an unseen callee here runs the neant codegen function,
    /// which needs `&mut Vm`). `vm`/`slot` resolve the callee exactly like `Op::LoadG`; `argc`/
    /// `arg0`/`arg1` are its already int-typed arguments (this language caps calls at two); `ok`
    /// is a scratch flag the caller reads right after the call returns.
    unsafe extern "C" fn jit_call(vm: *mut Vm, slot: i64, argc: i64, arg0: i64, arg1: i64, ok: *mut i64) -> i64 {
        let depth = CALL_DEPTH.with(|d| { let n = d.get() + 1; d.set(n); n });
        struct Guard;
        impl Drop for Guard { fn drop(&mut self) { CALL_DEPTH.with(|d| d.set(d.get() - 1)); } }
        let _guard = Guard;
        let fail = |ok: *mut i64| unsafe { *ok = 0; 0 };
        let vm = unsafe { &mut *vm };
        if depth > MAX_CALL_DEPTH || vm.depth() + depth as usize > crate::vm::MAX_DEPTH { return fail(ok); }
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

    /// `x[i]` from compiled code (`jitOpVecGet` / `jitxOpVecGet`): `vp` is a pointer into
    /// the `vecbuf` side table `try_run`/`try_run_raw` populated at entry, live for the whole
    /// compiled call. All `Arc`/COW handling stays here in Rust rather than being inlined as
    /// hand-rolled pointer arithmetic — see README "Stage 2" for why. Bounds-checked; out
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
        let f = vm.get(CODEGEN.0)?;
        // neant has no way to take a Rust function's address itself — hand it over explicitly, the
        // same way the old Rust encoder used to compute `jit_call`'s address inline.
        // The global slots of the six bit builtins, in `BIT_PRIMS` order, so the codegen can turn
        // a `Call` to one of them into the instruction it is; -1 for a name that is not currently
        // a global holding that very primitive, which simply means it is not inlinable here.
        // `bit_now` is the same lookup, kept to pair each inlined slot with the identity the entry
        // guard re-checks.
        let bit_now: Vec<(i64, i64)> = BIT_PRIMS.iter().map(|n| match (vm.slot_of(n), vm.get(n)) {
            (Some(s), Some(Value::Prim(p))) if p.name == *n => (s as i64, p as *const crate::value::PrimDef as i64),
            _ => (-1, 0),
        }).collect();
        let trampolines = Value::List(Arc::new(vec![
            Value::Int(jit_call as *const () as i64),
            Value::Int(jit_vec_get as *const () as i64),
            Value::Int(jit_vec_set as *const () as i64),
            crate::value::ints(bit_now.iter().map(|&(s, _)| s).collect()),
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
        // ride along so the entry guard below knows which ones need `Ints`, not a plain int — or,
        // from a backend that inlines vector access, the five-element form that also says which of
        // those slots are written, which one (if any) is the returned vector, and which global
        // slots were inlined as bit instructions. A two-element result keeps the old layout, where
        // a vector slot's word is a `*mut Value` for the trampolines; that is what makes this one
        // glue path serve a backend that has the inlining and one that does not.
        let ints_of = |v: &Value| -> Option<Vec<usize>> {
            match v { Value::Ints(v) => Some(v.iter().map(|&n| n as usize).collect()), _ => None }
        };
        let (bytes, vec_slots, vec_writes, ret_vec, prim_slots, inline_vec) = match result {
            Ok(Value::List(items)) if items.len() == 2 || items.len() == 5 => {
                let bytes = match &items[0] { Value::Bytes(b) => b.clone(), _ => return None };
                let vec_slots = ints_of(&items[1])?;
                if items.len() == 2 { (bytes, vec_slots, Vec::new(), None, Vec::new(), false) } else {
                    let vec_writes = ints_of(&items[2])?;
                    let ret_vec = match &items[3] { Value::Int(n) if *n >= 0 => Some(*n as usize), Value::Int(_) => None, _ => return None };
                    if ret_vec.is_some_and(|k| k >= vec_slots.len()) { return None; }
                    let prim_slots = ints_of(&items[4])?;
                    (bytes, vec_slots, vec_writes, ret_vec, prim_slots, true)
                }
            }
            _ => return None, // Null (not compilable) or a runtime error in the codegen itself
        };
        // Pair each inlined bit-builtin slot with the primitive it resolved to just now; that pair
        // is what `prims_intact` re-checks at every entry.
        let mut prim_guards = Vec::new();
        for slot in prim_slots {
            let want = bit_now.iter().find(|&&(s, _)| s == slot as i64)?.1;
            prim_guards.push((slot, want));
        }
        emit(&bytes, code.params.len(), code.nlocals, vec_slots, vec_writes, ret_vec, prim_guards, inline_vec)
    }

    #[allow(clippy::too_many_arguments)]
    fn emit(bytes: &[u8], arity: usize, nlocals: usize, vec_slots: Vec<usize>, vec_writes: Vec<usize>,
            ret_vec: Option<usize>, prim_guards: Vec<(usize, i64)>, inline_vec: bool) -> Option<super::Compiled> {
        if vec_slots.len() > MAX_VEC_SLOTS { return None; }
        let page = 4096usize;
        let len = bytes.len().div_ceil(page) * page;
        unsafe {
            let mem = mmap(std::ptr::null_mut(), len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            if mem as isize == -1 { return None; }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), mem as *mut u8, bytes.len());
            if mprotect(mem, len, PROT_READ | PROT_EXEC) != 0 { munmap(mem, len); return None; }
            // Freshly written bytes have to be visible to instruction fetch before anything jumps
            // into them. On AArch64 the data and instruction caches are not coherent, so that is a
            // real operation. On x86-64 they are coherent — the architecture guarantees a store
            // followed (here) by an `mprotect` is seen by a later fetch — so there is nothing to
            // do at all, which is why this is a `cfg` on the call rather than a call to a helper
            // that would be empty on one target: there is no operation to skip.
            #[cfg(target_arch = "aarch64")]
            __clear_cache(mem as *mut std::ffi::c_char, (mem as *mut u8).add(len) as *mut std::ffi::c_char);
            let entry: unsafe extern "C" fn(*mut i64, *mut i64, *mut Vm) -> i64 = std::mem::transmute(mem);
            Some(super::Compiled { mem: mem as *mut u8, len, arity, nlocals, vec_slots, vec_writes, ret_vec, prim_guards, inline_vec, entry })
        }
    }

    /// A compiled trace (`src/trace.rs`): a hot loop's body, running natively until a guard
    /// disagrees with what was recorded. Unlike `Compiled` above, there's no "return" — every exit
    /// is a bail, handing back exactly where in the original bytecode to resume interpreting, the
    /// current value of every local the trace touched, and the operand stack the interpreter
    /// would have had at that point, so `Vm::run_ops` can just splice them into its own
    /// `loc`/`st`/`ip` and keep going, indistinguishable from having interpreted the whole time.
    /// Sound because recording only ever *observes* — nothing here can make an already-correct
    /// program produce a different result, only run some of it faster.
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
        /// The operand stack this trace was recorded on (`Trace::entry`, src/trace.rs) — a `do`
        /// counter, or the counters of the `do` loops the header sits inside — and what `run`
        /// must find on the interpreter's stack to enter: those values go into the buffer's stack
        /// area and are loop-carried from there.
        entry: Vec<crate::trace::TraceTy>,
        /// Where in the buffer the stack area starts (right after the locals — but codegen's
        /// number, read back, not this side's assumption), and, per exit, the ip to resume at and
        /// the tags of the values the exit left in that area, bottom first. The compiled code hands
        /// back an *index* into this table, not an ip: an exit is an ip *and* a stack now.
        stack_base: usize,
        exits: Vec<(usize, Vec<crate::trace::TraceTy>)>,
        entry_fn: unsafe extern "C" fn(*mut i64, *mut i64),
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
    /// The most operand-stack values an exit can hand back: the trace's int and float stacks are
    /// each `jitMAXDEPTH` (6) registers deep (src/neant/jit/arm64.nt), and a null placeholder
    /// holds an int position. The stack area of the buffer is sized for this, and `compile_trace`
    /// refuses any exits table that would index past it — codegen respects its own depth cap, but
    /// the buffer is this side's, so this side checks.
    const MAX_STACK: usize = 12;
    const BUF_WORDS: usize = MAX_TOUCHED + MAX_STACK;

    impl CompiledTrace {
        /// Runs the trace until a guard bails, writes every touched local's new value back into
        /// `loc`, replaces the entry values on `st` with the operand stack the exit handed back,
        /// and returns the bytecode `ip` to resume interpreting from (`Bailed`). The two refusals
        /// are the entry guards: `TypeMismatch` means the *current* values in `loc`/`st` don't
        /// match the types this trace was compiled assuming (the same kind of guard `try_run`'s
        /// entry check already makes for the method-JIT) — same fallback as everywhere else here,
        /// just don't use the compiled version this time. `StaleCallee` means a global this trace
        /// inlined a callee from has been reassigned since it was recorded, which no later entry
        /// can undo — the caller retires the trace (`FnCode::retrace`, src/value.rs).
        pub fn run(&self, loc: &mut [Value], st: &mut Vec<Value>, vm: &Vm) -> TraceRun {
            use crate::trace::TraceTy;
            for (slot, code) in &self.callees {
                match vm.global_at(*slot) { Some(Value::Lambda(c)) if Arc::ptr_eq(&c, code) => {}, _ => return TraceRun::StaleCallee }
            }
            // The loop-carried stack: exactly the entry values, and — same guard as a local — a
            // plain non-null int each (`TraceTy::entry_tags` recorded nothing else).
            if st.len() != self.entry.len() { return TraceRun::TypeMismatch; }
            let mut buf = [0i64; BUF_WORDS];
            for (j, (v, ty)) in st.iter().zip(&self.entry).enumerate() {
                buf[self.stack_base + j] = match (ty, v) {
                    (TraceTy::Int, Value::Int(n)) if *n != crate::value::NI => *n,
                    _ => return TraceRun::TypeMismatch,
                };
            }
            // Only how the locals get in and out: the compiled code keeps them in registers for
            // the whole loop and touches this again only on the way out (or, if the trace has a
            // rewind in it, once per iteration — see jitCompileTrace's section comment).
            for (i, (slot, ty)) in self.touched.iter().enumerate() {
                if *slot >= self.real_upto { continue; }
                let Some(v) = loc.get(*slot) else { return TraceRun::TypeMismatch };
                buf[i] = match (ty, v) {
                    // A null int is rejected, not passed through: the interpreter propagates it
                    // through arithmetic and compiled code does plain wrapping arithmetic — the
                    // same entry guard `try_run` makes, for the same reason.
                    (TraceTy::Int, Value::Int(n)) if *n != crate::value::NI => *n,
                    (TraceTy::Float, Value::Float(f)) => f.to_bits() as i64,
                    _ => return TraceRun::TypeMismatch,
                };
            }
            let mut exit_idx: i64 = 0;
            unsafe { (self.entry_fn)(buf.as_mut_ptr(), &mut exit_idx as *mut i64); }
            for (i, (slot, ty)) in self.touched.iter().enumerate() {
                if *slot >= self.real_upto { continue; }
                loc[*slot] = match ty {
                    TraceTy::Float => Value::Float(f64::from_bits(buf[i] as u64)),
                    _ => Value::Int(buf[i]),
                };
            }
            // `compile_trace` checked every index the code can hand back is in the table.
            let (resume_ip, tags) = &self.exits[exit_idx as usize];
            st.truncate(st.len() - self.entry.len());
            for (j, ty) in tags.iter().enumerate() {
                let bits = buf[self.stack_base + j];
                st.push(match ty {
                    TraceTy::Int => Value::Int(bits),
                    TraceTy::Float => Value::Float(f64::from_bits(bits as u64)),
                    TraceTy::Bool => Value::Bool(bits != 0),
                    TraceTy::Null => Value::Null,
                });
            }
            TraceRun::Bailed(*resume_ip)
        }
    }

    pub fn compile_trace(trace: &crate::trace::Trace, vm: &mut Vm) -> Option<CompiledTrace> {
        use crate::trace::TraceTy;
        let f = vm.get(CODEGEN.1)?;
        let (kinds, args, tys, ips) = trace.to_neant_input();
        let input = Value::List(Arc::new(vec![
            crate::value::ints(kinds), crate::value::ints(args), crate::value::ints(tys), crate::value::ints(ips),
            crate::value::list(trace.consts.clone()), Value::Int(trace.header as i64),
            Value::Int(trace.real_upto as i64),
            crate::value::ints(trace.entry.iter().map(|t| t.code()).collect()),
        ]));
        // Same reentrancy guard `compile` above uses: `jitCompileTrace`'s own helpers are neant
        // functions too and could cross the JIT threshold while compiling themselves.
        COMPILING.with(|c| c.set(c.get() + 1));
        let result = vm.call(&f, vec![input]);
        COMPILING.with(|c| c.set(c.get() - 1));
        // Success is `(bytes; slots; tys; stackBase; exits)` — the buffer layout and the exits
        // table ride along with the code that was built around them (see `CompiledTrace`). Null
        // (not compilable) or an error in the codegen itself both just mean this loop stays
        // interpreted.
        let Ok(Value::List(items)) = result else { return None };
        if items.len() != 5 { return None; }
        let Value::Bytes(bytes) = &items[0] else { return None };
        let slots = int_vec(&items[1])?;
        let tys = int_vec(&items[2])?;
        let Value::Int(stack_base) = &items[3] else { return None };
        let stack_base = *stack_base as usize;
        if slots.len() != tys.len() || slots.len() > MAX_TOUCHED || stack_base > MAX_TOUCHED { return None; }
        let touched = slots.iter().zip(&tys)
            .map(|(&s, &t)| TraceTy::from_code(t).map(|t| (s as usize, t)))
            .collect::<Option<Vec<_>>>()?;
        if touched.iter().any(|(_, t)| !matches!(t, TraceTy::Int | TraceTy::Float)) { return None; }
        // Every index the code can hand back must name an entry, and every entry's stack — like
        // the entry stack itself — must fit the buffer's stack area: the one bound codegen can't
        // check for this side.
        let fits = |tags: &[TraceTy]| stack_base + tags.len() <= BUF_WORDS;
        let mut exits = Vec::new();
        for e in items[4].seq() {
            let Value::List(pair) = &e else { return None };
            if pair.len() != 2 { return None; }
            let Value::Int(ip) = &pair[0] else { return None };
            let tags = int_vec(&pair[1])?.into_iter().map(TraceTy::from_code).collect::<Option<Vec<_>>>()?;
            if !fits(&tags) { return None; }
            exits.push((*ip as usize, tags));
        }
        if exits.is_empty() || !fits(&trace.entry) { return None; }
        emit_trace(bytes, touched, trace.real_upto as usize, trace.callees.clone(), trace.entry.clone(), stack_base, exits)
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

    fn emit_trace(
        bytes: &[u8], touched: Vec<(usize, crate::trace::TraceTy)>, real_upto: usize, callees: Vec<(usize, Arc<FnCode>)>,
        entry: Vec<crate::trace::TraceTy>, stack_base: usize, exits: Vec<(usize, Vec<crate::trace::TraceTy>)>,
    ) -> Option<CompiledTrace> {
        let page = 4096usize;
        let len = bytes.len().div_ceil(page) * page;
        unsafe {
            let mem = mmap(std::ptr::null_mut(), len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            if mem as isize == -1 { return None; }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), mem as *mut u8, bytes.len());
            if mprotect(mem, len, PROT_READ | PROT_EXEC) != 0 { munmap(mem, len); return None; }
            // Freshly written bytes have to be visible to instruction fetch before anything jumps
            // into them. On AArch64 the data and instruction caches are not coherent, so that is a
            // real operation. On x86-64 they are coherent — the architecture guarantees a store
            // followed (here) by an `mprotect` is seen by a later fetch — so there is nothing to
            // do at all, which is why this is a `cfg` on the call rather than a call to a helper
            // that would be empty on one target: there is no operation to skip.
            #[cfg(target_arch = "aarch64")]
            __clear_cache(mem as *mut std::ffi::c_char, (mem as *mut u8).add(len) as *mut std::ffi::c_char);
            let entry_fn: unsafe extern "C" fn(*mut i64, *mut i64) = std::mem::transmute(mem);
            Some(CompiledTrace { mem: mem as *mut u8, len, touched, real_upto, callees, entry, stack_base, exits, entry_fn })
        }
    }
}

/// Everything above is per-architecture only in its codegen function and its cache flush, so both
/// backends export the same names from the same module.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(crate) use native::already_compiling;
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub use native::{compile, compile_trace, Compiled, CompiledTrace};

/// What `CompiledTrace::run` came back with — see its doc comment.
pub enum TraceRun { Bailed(usize), TypeMismatch, StaleCallee }

// Any other target has no backend at all: uninhabited stand-ins, so the call sites in
// src/value.rs and src/vm.rs keep compiling and every one of them takes the interpreter's path.
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
pub(crate) fn already_compiling() -> bool { false }
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
pub struct Compiled(std::convert::Infallible);
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
impl Compiled {
    pub fn try_run(&self, _loc: &[Value], _vm: &mut Vm) -> Option<Value> { match self.0 {} }
}
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
pub fn compile(_code: &Arc<FnCode>, _vm: &mut Vm) -> Option<Compiled> { None }
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
pub struct CompiledTrace(std::convert::Infallible);
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
impl CompiledTrace {
    pub fn run(&self, _loc: &mut [Value], _st: &mut Vec<Value>, _vm: &Vm) -> TraceRun { match self.0 {} }
}
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
pub fn compile_trace(_trace: &crate::trace::Trace, _vm: &mut Vm) -> Option<CompiledTrace> { None }

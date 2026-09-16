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
//! No new Cargo dependency: executable memory comes from `mmap`/`mprotect`, the instruction cache
//! is flushed with `__clear_cache`, both declared directly as `extern "C"` — already linked in via
//! libc/libgcc on this glibc target, so `Cargo.toml` stays empty.
//!
//! **Calling another compiled function.** A compiled function that calls something is no longer
//! pure by inspection alone — it's pure only if the callee is too, and if it isn't, calling it and
//! then later deopting (re-running the *caller* from scratch on the interpreter) would invoke the
//! callee a second time, corrupting any real side effect it had. So `jit_call` (the one fixed
//! trampoline every compiled call site reaches via `blr`) proves the callee is equally pure —
//! by literally attempting to compile it too (`FnCode::jit_for_call`, unlocked by the same
//! compilability check as everything else) — *before* making the call at all. If that fails, no
//! call happens and the compiled caller deopts immediately, and the interpreter makes that one
//! real call itself, correctly and exactly once.

#[cfg(target_arch = "aarch64")]
mod arm64 {
    use crate::value::{Op, FnCode, Value, NI};
    use crate::vm::Vm;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// One compiled function: owns its executable memory and knows how to call into it. Dropping
    /// it unmaps the memory — safe because nothing calls in once the owning `FnCode`'s `Arc<Compiled>`
    /// (src/value.rs `JitState`) has been dropped, since that's the only handle to it.
    pub struct Compiled {
        mem: *mut u8,
        len: usize,
        arity: usize,
        nlocals: usize,
        entry: unsafe extern "C" fn(*mut i64, *mut i64, *const Vm) -> i64,
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
        pub fn try_run(&self, loc: &[Value], vm: &Vm) -> Option<Value> {
            if loc.len() > MAX_SLOTS { return None; }
            // args (0..arity) and captures (nlocals..) must already be plain, non-null ints; the
            // slots in between are the interpreter's own Null-until-first-StoreL scratch locals,
            // which the compilability check guarantees are always written before they're read.
            let mut buf = [0i64; MAX_SLOTS];
            for (i, v) in loc.iter().enumerate() {
                if i < self.arity || i >= self.nlocals {
                    match v { Value::Int(n) if *n != NI => buf[i] = *n, _ => return None }
                }
            }
            self.run(buf.as_mut_ptr(), vm).map(Value::Int)
        }
        /// The direct recursive-call path (`jit_call` below): args are already known to be plain
        /// ints (they came from another compiled function's own int-typed registers, per the same
        /// invariant that makes any of this sound), so this skips `try_run`'s general `Value`
        /// boxing/unboxing and the heap allocation a `Vec` would otherwise need — the args and the
        /// captures it still has to validate go straight into a stack buffer.
        fn try_run_raw(&self, argc: usize, arg0: i64, arg1: i64, caps: &[Value], vm: &Vm) -> Option<i64> {
            let total = self.nlocals.max(self.arity) + caps.len();
            if total > MAX_SLOTS { return None; }
            let mut buf = [0i64; MAX_SLOTS];
            if argc >= 1 { buf[0] = arg0; }
            if argc >= 2 { buf[1] = arg1; }
            for (i, v) in caps.iter().enumerate() {
                match v { Value::Int(n) if *n != NI => buf[self.nlocals.max(self.arity) + i] = *n, _ => return None }
            }
            self.run(buf.as_mut_ptr(), vm)
        }
        fn run(&self, buf: *mut i64, vm: &Vm) -> Option<i64> {
            let mut ok: i64 = 0;
            let val = unsafe { (self.entry)(buf, &mut ok as *mut i64, vm as *const Vm) };
            if ok != 0 { Some(val) } else { None }
        }
    }

    thread_local! { static CALL_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; }
    /// Same limit `Vm::call_code` (src/vm.rs) enforces for ordinary recursion — compiled-to-compiled
    /// calls go through `blr`, not `call_code`, so they'd otherwise blow the real machine stack
    /// (a hard crash) instead of failing like every other kind of runaway recursion in this
    /// language does.
    const MAX_CALL_DEPTH: u32 = 1000;

    /// The one fixed trampoline every compiled call site reaches via `blr` (see the module doc
    /// comment for why proving the callee pure *before* calling it is the safety argument here).
    /// `vm`/`slot` resolve the callee exactly like `Op::LoadG`; `argc`/`arg0`/`arg1` are its already
    /// int-typed arguments (this language caps calls at two); `ok` is a scratch flag the caller
    /// reads right after the `blr` returns.
    unsafe extern "C" fn jit_call(vm: *const Vm, slot: i64, argc: i64, arg0: i64, arg1: i64, ok: *mut i64) -> i64 {
        let depth = CALL_DEPTH.with(|d| { let n = d.get() + 1; d.set(n); n });
        struct Guard;
        impl Drop for Guard { fn drop(&mut self) { CALL_DEPTH.with(|d| d.set(d.get() - 1)); } }
        let _guard = Guard;
        let fail = |ok: *mut i64| unsafe { *ok = 0; 0 };
        if depth > MAX_CALL_DEPTH { return fail(ok); }
        let vm = unsafe { &*vm };
        let Some(f) = vm.global_at(slot as usize) else { return fail(ok) };
        let (code, caps): (&Arc<FnCode>, &[Value]) = match &f {
            Value::Lambda(c) => (c, &[]),
            Value::Closure(c, caps) => (c, caps),
            _ => return fail(ok), // calling a primitive from compiled code stays out of scope
        };
        if code.params.len() != argc as usize { return fail(ok); }
        let Some(compiled) = code.jit_for_call() else { return fail(ok) };
        match compiled.try_run_raw(argc as usize, arg0, arg1, caps, vm) {
            Some(v) => unsafe { *ok = 1; v }
            None => fail(ok),
        }
    }

    // ---- AArch64 A64 instruction encoding.
    //
    // Registers: x19 = loc pointer, x20 = the `NI` constant, x21 = the `vm` context pointer, x22 =
    // this call's `ok`-out pointer — all callee-saved, so they survive any number of nested `blr`s
    // for free instead of needing to be spilled around every call. x9..x14 are the interpreter's
    // operand stack (6 deep — enough for the expressions this subset allows; deeper is rejected at
    // compile time), caller-saved, so whichever of them are still live below a call's own arguments
    // are spilled to the frame first and reloaded after. x15 is a dedicated call scratch register,
    // never used for stack values, so argument marshalling never has to worry about clobbering one.
    //
    // Every new encoding here (`stp`/`ldp` pre/post/signed-offset, `blr`, `add`-immediate) was
    // cross-checked against `objdump -d` on a tiny snippet assembled with this box's own `as`/`gcc`
    // before being trusted, the same way `str`/`ldr`/`sub`-immediate already were.
    const LOC: u32 = 19;
    const NIREG: u32 = 20;
    const VMREG: u32 = 21;
    const OKREG: u32 = 22;
    const CALLSCRATCH: u32 = 15;
    const STACK_BASE: u32 = 9;
    const MAX_DEPTH: usize = 6;
    const SP: u32 = 31;
    /// Frame layout (bytes from SP): [0,16) x29/x30, [16,32) x19/x20, [32,48) x21/x22,
    /// [48,48+6*8) the operand-stack spill area, then an 8-byte scratch slot for a call's `ok`
    /// flag, rounded up to the 16-byte SP alignment AAPCS64 requires.
    const SPILL_BASE: u32 = 48;
    const CALL_OK_SLOT: u32 = SPILL_BASE + (MAX_DEPTH as u32) * 8;
    const FRAME_SIZE: u32 = (CALL_OK_SLOT + 8).div_ceil(16) * 16;

    fn movz(rd: u32, imm16: u16, hw: u32) -> u32 { 0xD2800000 | (hw << 21) | ((imm16 as u32) << 5) | rd }
    fn movk(rd: u32, imm16: u16, hw: u32) -> u32 { 0xF2800000 | (hw << 21) | ((imm16 as u32) << 5) | rd }
    fn add_r(rd: u32, rn: u32, rm: u32) -> u32 { 0x8B000000 | (rm << 16) | (rn << 5) | rd }
    fn sub_r(rd: u32, rn: u32, rm: u32) -> u32 { 0xCB000000 | (rm << 16) | (rn << 5) | rd }
    fn mul_r(rd: u32, rn: u32, rm: u32) -> u32 { 0x9B007C00 | (rm << 16) | (rn << 5) | rd }
    fn cmp_r(rn: u32, rm: u32) -> u32 { 0xEB00001F | (rm << 16) | (rn << 5) }
    fn csel(rd: u32, rn: u32, rm: u32, cond: u32) -> u32 { 0x9A800000 | (rm << 16) | (cond << 12) | (rn << 5) | rd }
    fn cset(rd: u32, cond: u32) -> u32 { 0x9A9F07E0 | ((cond ^ 1) << 12) | rd } // CSINC Xd,XZR,XZR,invert(cond)
    fn ldr_imm(rt: u32, rn: u32, byte_off: u32) -> u32 { 0xF9400000 | ((byte_off / 8) << 10) | (rn << 5) | rt }
    fn str_imm(rt: u32, rn: u32, byte_off: u32) -> u32 { 0xF9000000 | ((byte_off / 8) << 10) | (rn << 5) | rt }
    fn mov_r(rd: u32, rn: u32) -> u32 { 0xAA0003E0 | (rn << 16) | rd } // ORR Xd,XZR,Xn
    fn ret() -> u32 { 0xD65F03C0 }
    fn blr(rn: u32) -> u32 { 0xD63F0000 | (rn << 5) }
    fn b(imm26: i32) -> u32 { 0x14000000 | (imm26 as u32 & 0x03FF_FFFF) }
    fn cbz(rt: u32, imm19: i32) -> u32 { 0xB4000000 | ((imm19 as u32 & 0x7FFFF) << 5) | rt }
    fn bcond(cond: u32, imm19: i32) -> u32 { 0x54000000 | ((imm19 as u32 & 0x7FFFF) << 5) | cond }
    fn cmp_imm(rn: u32, imm12: u32) -> u32 { 0xF1000000 | (imm12 << 10) | (rn << 5) | 31 }
    fn sub_imm(rd: u32, rn: u32, imm12: u32) -> u32 { 0xD1000000 | (imm12 << 10) | (rn << 5) | rd }
    fn add_imm(rd: u32, rn: u32, imm12: u32) -> u32 { 0x91000000 | (imm12 << 10) | (rn << 5) | rd }
    // Pair loads/stores: pre-index allocates the frame (`sp -= n`, then stores), signed-offset and
    // post-index address within/deallocate it. Confirmed against `objdump -d` output for
    // `stp x29,x30,[sp,#-32]!`, `stp x19,x20,[sp,#16]`, `ldp x19,x20,[sp,#16]` and
    // `ldp x29,x30,[sp],#32` respectively.
    fn stp_pre(rt1: u32, rt2: u32, rn: u32, imm_bytes: i32) -> u32 { 0xA9800000 | (((imm_bytes / 8) as u32 & 0x7F) << 15) | (rt2 << 10) | (rn << 5) | rt1 }
    fn stp_off(rt1: u32, rt2: u32, rn: u32, byte_off: u32) -> u32 { 0xA9000000 | (((byte_off / 8) & 0x7F) << 15) | (rt2 << 10) | (rn << 5) | rt1 }
    fn ldp_off(rt1: u32, rt2: u32, rn: u32, byte_off: u32) -> u32 { 0xA9400000 | (((byte_off / 8) & 0x7F) << 15) | (rt2 << 10) | (rn << 5) | rt1 }
    fn ldp_post(rt1: u32, rt2: u32, rn: u32, byte_off: u32) -> u32 { 0xA8C00000 | (((byte_off / 8) & 0x7F) << 15) | (rt2 << 10) | (rn << 5) | rt1 }

    const COND_LT: u32 = 11;
    const COND_GT: u32 = 12;
    const COND_EQ: u32 = 0;

    fn load_const(out: &mut Vec<u32>, rd: u32, v: i64) {
        let u = v as u64;
        out.push(movz(rd, u as u16, 0));
        for hw in 1..4 {
            let chunk = (u >> (hw * 16)) as u16;
            if chunk != 0 { out.push(movk(rd, chunk, hw as u32)); }
        }
    }
    fn emit_prologue(asm: &mut Vec<u32>) {
        asm.push(stp_pre(29, 30, SP, -(FRAME_SIZE as i32)));
        asm.push(stp_off(19, 20, SP, 16));
        asm.push(stp_off(21, 22, SP, 32));
        asm.push(mov_r(LOC, 0));   // x19 = loc ptr (arg0)
        asm.push(mov_r(VMREG, 2)); // x21 = vm ptr (arg2)
        asm.push(mov_r(OKREG, 1)); // x22 = our own ok-out ptr (arg1)
        load_const(asm, NIREG, NI);
    }
    fn emit_epilogue(asm: &mut Vec<u32>) {
        asm.push(ldp_off(21, 22, SP, 32));
        asm.push(ldp_off(19, 20, SP, 16));
        asm.push(ldp_post(29, 30, SP, FRAME_SIZE));
        asm.push(ret());
    }
    fn emit_return(asm: &mut Vec<u32>, valreg: u32) {
        asm.push(mov_r(0, valreg));
        load_const(asm, 5, 1);
        asm.push(str_imm(5, OKREG, 0));
        emit_epilogue(asm);
    }
    fn emit_deopt(asm: &mut Vec<u32>) {
        load_const(asm, 5, 0);
        asm.push(str_imm(5, OKREG, 0));
        emit_epilogue(asm);
    }
    /// After a wrapping add/sub/mul, the one case a plain integer op can't just trust: two ordinary
    /// values wrapping to exactly the null sentinel by coincidence (see the crate-level doc comment
    /// and README "Stage 2"). Emits a placeholder branch to be patched to the shared deopt exit.
    fn check_ni(asm: &mut Vec<u32>, reg: u32) -> (usize, Option<u32>) {
        asm.push(cmp_r(reg, NIREG));
        let at = asm.len();
        asm.push(0); // placeholder B.EQ, patched to the deopt exit once its address is known
        (at, None)
    }

    /// Which dyadic verbs this backend inlines directly — exactly `int_dyad`'s set (src/vm.rs).
    fn dyad_kind(p_name: &str) -> Option<u8> {
        match p_name { "+" => Some(0), "-" => Some(1), "*" => Some(2), "&" => Some(3), "|" => Some(4), "<" => Some(5), ">" => Some(6), "=" => Some(7), _ => None }
    }

    pub fn compile(code: &Arc<FnCode>) -> Option<super::Compiled> {
        let arity = code.params.len();
        let mut asm: Vec<u32> = Vec::new();
        let mut ip_word: Vec<usize> = vec![0; code.ops.len() + 1];
        let mut fixups: Vec<(usize, u32, bool)> = Vec::new(); // (word idx of branch, target ip, is_cbz(reg carried separately))
        let mut cbz_regs: HashMap<usize, u32> = HashMap::new();
        // (word idx, None = flags-based B.EQ, Some(reg) = CBZ reg), both patched to the deopt exit.
        let mut deopt_fixups: Vec<(usize, Option<u32>)> = Vec::new();
        // Every op but `Loop` has one depth delta that holds on both the fallthrough and any jump
        // edge out of it, so accumulating linearly through array order already gives the right
        // depth wherever a later ip is reached — a well-formedness guarantee the plain interpreter
        // relies on too. `Loop` is the one exception (see below), so its exit target's depth is
        // recorded here and used to override the naive accumulation when the scan reaches it.
        let mut depth_at: HashMap<usize, i32> = HashMap::new();
        // A `LoadG` is only ever accepted immediately before the `Call` it feeds (see the module
        // doc comment) — this carries its already-interned slot index the one step from one to the
        // other, the same pairing trick as the `Push(Null)`-before-`Pop` case below.
        let mut pending_call_slot: Option<i64> = None;

        emit_prologue(&mut asm);

        let mut depth: i32 = 0;
        for (ip, op) in code.ops.iter().enumerate() {
            if let Some(&d) = depth_at.get(&ip) { depth = d; }
            ip_word[ip] = asm.len();
            if depth < 0 || depth as usize > MAX_DEPTH { return None; }
            if pending_call_slot.is_some() && !matches!(op, Op::Call(1) | Op::Call(2)) { return None; }
            match *op {
                // `if`/`while`/`do` push a `Null` as their statement value (boot/compile.nt's
                // `pushNull`), always immediately discarded by the `Pop` that follows every
                // statement in a block. Nothing is ever read back from it, so it's safe to compile
                // as long as that pairing holds — skip emitting anything for the push itself, and
                // let the matching `Pop` below account for it. Anywhere else a `Null` reaches
                // (stored, returned, used in arithmetic) isn't provably int-only — reject.
                Op::Push(a) if matches!(code.consts.get(a as usize), Some(Value::Null)) => {
                    if !matches!(code.ops.get(ip + 1), Some(Op::Pop)) { return None; }
                    depth += 1;
                }
                Op::Push(a) => {
                    let Value::Int(v) = code.consts.get(a as usize)? else { return None };
                    if *v == NI { return None; }
                    load_const(&mut asm, STACK_BASE + depth as u32, *v);
                    depth += 1;
                }
                Op::LoadL(a) => {
                    if a as usize >= 4096 { return None; }
                    asm.push(ldr_imm(STACK_BASE + depth as u32, LOC, a * 8));
                    depth += 1;
                }
                Op::StoreL(a) => {
                    if depth < 1 || a as usize >= 4096 { return None; }
                    asm.push(str_imm(STACK_BASE + (depth - 1) as u32, LOC, a * 8));
                }
                Op::Pop => { if depth < 1 { return None; } depth -= 1; }
                Op::Ret => {
                    if depth < 1 { return None; }
                    emit_return(&mut asm, STACK_BASE + (depth - 1) as u32);
                }
                Op::Dyad(a) => {
                    if depth < 2 { return None; }
                    let Value::Prim(p) = code.consts.get(a as usize)? else { return None };
                    let kind = dyad_kind(p.name)?;
                    let x = STACK_BASE + (depth - 1) as u32; // left operand (popped first)
                    let y = STACK_BASE + (depth - 2) as u32; // right operand
                    let dst = y; // reuse the lower slot as the new top
                    match kind {
                        0 => { asm.push(add_r(dst, x, y)); deopt_fixups.push(check_ni(&mut asm, dst)); }
                        1 => { asm.push(sub_r(dst, x, y)); deopt_fixups.push(check_ni(&mut asm, dst)); }
                        2 => { asm.push(mul_r(dst, x, y)); deopt_fixups.push(check_ni(&mut asm, dst)); }
                        3 => { asm.push(cmp_r(x, y)); asm.push(csel(dst, x, y, COND_LT)); } // min
                        4 => { asm.push(cmp_r(x, y)); asm.push(csel(dst, x, y, COND_GT)); } // max
                        5 => { asm.push(cmp_r(x, y)); asm.push(cset(dst, COND_LT)); }
                        6 => { asm.push(cmp_r(x, y)); asm.push(cset(dst, COND_GT)); }
                        7 => { asm.push(cmp_r(x, y)); asm.push(cset(dst, COND_EQ)); }
                        _ => unreachable!(),
                    }
                    depth -= 1;
                }
                Op::Jmp(t) => { depth_at.insert(t as usize, depth); fixups.push((asm.len(), t, false)); asm.push(0); }
                Op::Jmpf(t) => {
                    if depth < 1 { return None; }
                    let r = STACK_BASE + (depth - 1) as u32;
                    depth -= 1;
                    depth_at.insert(t as usize, depth);
                    cbz_regs.insert(asm.len(), r);
                    fixups.push((asm.len(), t, true));
                    asm.push(0);
                }
                // `do[n;..]`: >0 -> decrement in place and fall through to the body (depth
                // unchanged, same as the interpreter's `*st.last_mut()=Int(n-1)`); else pop and
                // jump to the exit (depth-1, since that pop is the one thing that differs between
                // the two edges — recorded in `depth_at` since the naive linear scan can't see it).
                Op::Loop(t) => {
                    if depth < 1 { return None; }
                    let r = STACK_BASE + (depth - 1) as u32;
                    asm.push(cmp_imm(r, 0));
                    asm.push(bcond(COND_GT, 2)); // n>0: skip the exit branch, fall into the decrement
                    depth_at.insert(t as usize, depth - 1);
                    fixups.push((asm.len(), t, false));
                    asm.push(0); // n<=0: exit branch, patched to t
                    asm.push(sub_imm(r, r, 1));
                }
                // A global loaded only to be called immediately (see `pending_call_slot` above) —
                // nothing to emit for the load itself, the call site below resolves it fresh every
                // time via the trampoline, same as the interpreter re-reading the global each call.
                Op::LoadG(a) if matches!(code.ops.get(ip + 1), Some(Op::Call(1)) | Some(Op::Call(2))) => {
                    let Value::Int(slot) = code.consts.get(a as usize)? else { return None };
                    pending_call_slot = Some(*slot);
                }
                Op::Call(n @ (1 | 2)) if pending_call_slot.is_some() => {
                    let slot = pending_call_slot.take().unwrap();
                    let argc = n as i32;
                    if depth < argc { return None; }
                    let live = depth - argc; // slots below the args, which must survive the call
                    for i in 0..live { asm.push(str_imm(STACK_BASE + i as u32, SP, SPILL_BASE + (i as u32) * 8)); }
                    asm.push(mov_r(3, STACK_BASE + (depth - 1) as u32)); // arg0: left/first-popped operand
                    if argc == 2 { asm.push(mov_r(4, STACK_BASE + (depth - 2) as u32)); } // arg1
                    asm.push(mov_r(0, VMREG));
                    load_const(&mut asm, 1, slot);
                    load_const(&mut asm, 2, argc as i64);
                    asm.push(add_imm(5, SP, CALL_OK_SLOT));
                    load_const(&mut asm, CALLSCRATCH, jit_call as *const () as i64);
                    asm.push(blr(CALLSCRATCH));
                    asm.push(ldr_imm(CALLSCRATCH, SP, CALL_OK_SLOT));
                    deopt_fixups.push((asm.len(), Some(CALLSCRATCH)));
                    asm.push(0); // placeholder CBZ CALLSCRATCH -> deopt (ok==0)
                    asm.push(mov_r(STACK_BASE + live as u32, 0)); // call result -> new top of stack
                    for i in 0..live { asm.push(ldr_imm(STACK_BASE + i as u32, SP, SPILL_BASE + (i as u32) * 8)); }
                    depth = live + 1;
                }
                // Monad, MkClosure, MkAdv, Amend, List, TakeL/TakeG, StoreG, a bare LoadG/Call not
                // in the paired shape above: none of these are provably pure integer arithmetic
                // over locals (or a call to something that is) — reject.
                _ => return None,
            }
        }
        if pending_call_slot.is_some() { return None; } // a trailing LoadG with no Call to pair it
        ip_word[code.ops.len()] = asm.len();
        // fell off the end without an explicit Ret: return whatever's left on top, like the interpreter does
        if !matches!(code.ops.last(), Some(Op::Ret)) {
            if depth < 1 { return None; }
            emit_return(&mut asm, STACK_BASE + (depth - 1) as u32);
        }

        let deopt_at = asm.len();
        emit_deopt(&mut asm);

        for (word, target, is_cbz) in fixups {
            let target_word = ip_word[target as usize];
            let delta = (target_word as i64 - word as i64) as i32;
            asm[word] = if is_cbz { cbz(cbz_regs[&word], delta) } else { b(delta) };
        }
        for (word, kind) in deopt_fixups {
            let delta = (deopt_at as i64 - word as i64) as i32;
            asm[word] = match kind { Some(reg) => cbz(reg, delta), None => bcond(COND_EQ, delta) };
        }

        emit(&asm, arity, code.nlocals)
    }

    fn emit(words: &[u32], arity: usize, nlocals: usize) -> Option<super::Compiled> {
        let bytes = words.len() * 4;
        let page = 4096usize;
        let len = bytes.div_ceil(page) * page;
        unsafe {
            let mem = mmap(std::ptr::null_mut(), len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            if mem as isize == -1 { return None; }
            std::ptr::copy_nonoverlapping(words.as_ptr() as *const u8, mem as *mut u8, bytes);
            if mprotect(mem, len, PROT_READ | PROT_EXEC) != 0 { munmap(mem, len); return None; }
            __clear_cache(mem as *mut std::ffi::c_char, (mem as *mut u8).add(len) as *mut std::ffi::c_char);
            let entry: unsafe extern "C" fn(*mut i64, *mut i64, *const Vm) -> i64 = std::mem::transmute(mem);
            Some(super::Compiled { mem: mem as *mut u8, len, arity, nlocals, entry })
        }
    }
}

#[cfg(target_arch = "aarch64")]
pub use arm64::{compile as compile_arm64, Compiled};

#[cfg(target_arch = "aarch64")]
pub fn compile(code: &std::sync::Arc<crate::value::FnCode>) -> Option<Compiled> { compile_arm64(code) }

#[cfg(not(target_arch = "aarch64"))]
pub struct Compiled(std::convert::Infallible);
#[cfg(not(target_arch = "aarch64"))]
impl Compiled {
    pub fn try_run(&self, _loc: &[crate::value::Value], _vm: &crate::vm::Vm) -> Option<crate::value::Value> { match self.0 {} }
}
#[cfg(not(target_arch = "aarch64"))]
pub fn compile(_code: &std::sync::Arc<crate::value::FnCode>) -> Option<Compiled> { None }

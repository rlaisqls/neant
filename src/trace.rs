//! The tracing JIT's recording side: record one concrete pass through a hot `while` loop as a
//! flat, branch-free op sequence with observed (not inferred) types, for `src/jit.rs`/
//! `src/neant/jit/arm64.nt` to compile (`jitCompileTrace`). This module is purely the *recording*
//! side — `Vm::run_ops` (src/vm.rs) drives it by feeding it every op it executes; recording never
//! influences what actually runs, only observes it, so a trace failing to record (or never being
//! attempted at all) can never change a program's result, only whether it gets to run faster.
//!
//! Scope, deliberately narrow: `while` loops only (a loop header must have an *empty* operand
//! stack — true for `while`, false for `do[n;..]`, whose backward jump lands back on the
//! still-live counter; `Vm::run_ops` checks this before ever starting a recording), no
//! `Op::Loop`, and `Push`/`LoadL`/`StoreL`/`Dyad`/`Pop`/`Jmpf`/`Jmp` plus a call to a plain lambda
//! held by a global (`f x`, `f[a;b]`, nested however deep) in the body. Anything else — a
//! closure, a primitive, a projection, a global that isn't the function being called, a `Ret`
//! out of the loop's own frame — just aborts the attempt: same fail-closed default the
//! method-JIT's own compilability check already uses.
//!
//! A trace is *linear*: one concrete path, with every branch it took pinned by a guard, not a
//! control-flow graph. That's why an unconditional `Jmp` needs no representation at all (the ops
//! it skipped simply aren't in the recording) while every `Jmpf` becomes one, and why an `if` or a
//! `$[..]` inside the loop costs nothing until the day its condition actually flips.
//!
//! **Calls are inlined, not called.** A recording follows the interpreter straight into a callee's
//! frame (`Vm::call_code` drives that through `enter_frame`/`exit_frame`) and keeps going, so what
//! comes out is one flat sequence with no call in it at all. Three things fall out of that:
//!
//! - The callee's locals are the caller's locals' neighbours. Every frame gets a disjoint range of
//!   *trace-local* slots, and only the range the loop's own frame occupies (`real_upto`) means
//!   anything to the interpreter; the rest are *virtual* — they exist for the duration of the
//!   compiled loop and never go back anywhere. Binding an argument is a `StoreL` into one of
//!   them, so it needs no representation of its own either; only the frame's edges do
//!   (`FramePush`/`FrameEnd`), so the codegen knows where on the operand stack its result belongs.
//! - Which function a global holds is a guard, not an assumption — but one that can be checked
//!   once on the way in (`callees`), since nothing a compiled trace runs can assign a global. A
//!   trace whose callee *has* been reassigned is retired and the loop recorded again against the
//!   new definition (`FnCode::retrace`, src/value.rs), a bounded number of times.
//! - A branch inside a callee can't bail the way one in the loop's own frame does: the `ip` it
//!   would resume at belongs to another frame's bytecode, and the interpreter is not in that frame
//!   any more. Those become `GuardRewind` — throw the half-finished iteration away and re-enter the
//!   interpreter at the loop header, which is sound for exactly the reason the int-null deopt is
//!   (nothing in the traceable subset can be observed from outside the loop).

use crate::value::*;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceTy { Int, Float }

impl TraceTy {
    /// `Bool` counts as `Int` — comparison results (`Op::Dyad`'s `<`/`>`/`=`) are `Value::Bool`,
    /// but represented identically to a plain 0/1 int by every op that might consume one
    /// afterwards (`Jmpf`'s `truthy()`, arithmetic via the general — not fast — path). Anything
    /// else (a vector, a symbol, `Null`, ...) is out of scope for this pass.
    pub fn of(v: &Value) -> Option<TraceTy> {
        match v {
            Value::Int(_) | Value::Bool(_) => Some(TraceTy::Int),
            Value::Float(_) => Some(TraceTy::Float),
            _ => None,
        }
    }

    /// The same, for a value that a *local* holds rather than one passing through the operand
    /// stack — stricter by exactly one case: `Bool` is rejected. A trace's locals round-trip
    /// through a buffer of raw 64-bit words (`CompiledTrace::run`, src/jit.rs), so whatever type
    /// they're rebuilt as on the way out is the type they have afterwards; letting a `Bool` in
    /// would silently turn `b: i<n` into an `Int` local, which `type`/`show`/`string` can all see.
    /// An int null is rejected for the same reason the compiled code checks for one at all (see
    /// `jitTrDyad`, src/neant/jit/arm64.nt): the interpreter propagates it through arithmetic and
    /// compiled code does not.
    fn of_local(v: &Value) -> Option<TraceTy> {
        match v {
            Value::Int(n) if *n != crate::value::NI => Some(TraceTy::Int),
            Value::Float(_) => Some(TraceTy::Float),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum TraceOp {
    Push { k: u32, ty: TraceTy },
    /// `if`/`while`/`do`'s own statement-value convention — always immediately consumed by a
    /// `Pop`, same as the method-JIT's `jitOpPush` already special-cases (`src/neant/jit/
    /// arm64.nt`) — a true no-op, not a value that needs a register at all.
    PushNull,
    LoadL { slot: u32, ty: TraceTy },
    StoreL { slot: u32, ty: TraceTy },
    Dyad { k: u32, ty: TraceTy },
    Pop,
    /// The recorded direction of a branch this trace took, and where a later replay should resume
    /// *interpretation* if it disagrees — the other destination this exact `Jmpf` could have gone
    /// to (the loop's own condition check included — recorded the same as any other `Jmpf`).
    Guard { taken: bool, bail_ip: u32 },
    /// The same, for a branch inside an inlined callee, whose `ip` means nothing to the frame the
    /// interpreter is actually in: resume the loop header instead and let the iteration run again
    /// interpreted (see this module's header comment).
    GuardRewind { taken: bool },
    /// An inlined callee's frame starts here, with its `nargs` arguments the top of the operand
    /// stack (parameter 0 uppermost — `Op::Call` pops them in that order) and nothing of the
    /// callee's own there: the `LoadG` that fetched it is not a value to a trace (see `step`),
    /// and the interpreter has popped it by the time the frame runs. The `StoreL`+`Pop` pairs
    /// that follow bind the arguments into the frame's own slots; the depth under them is where
    /// the frame's result ends up.
    FramePush { nargs: u32 },
    /// ...and ends here, with its result the one value it leaves behind.
    FrameEnd,
}

pub struct Trace {
    pub header: u32,
    pub steps: Vec<TraceOp>,
    /// The values `Push`/`Dyad` steps name, collected here rather than indexed into some frame's
    /// const table — with callees inlined there is no one frame to index into any more.
    pub consts: Vec<Value>,
    /// Trace-local slots below this are the loop's own frame, and are the ones (and the only ones)
    /// that mean something to the interpreter on the way in and out.
    pub real_upto: u32,
    /// `(global slot, the lambda it must still hold)` for every inlined callee — checked once at
    /// entry (`CompiledTrace::run`, src/jit.rs), which is enough: nothing a compiled trace runs
    /// can assign a global, so within one run they cannot change under it.
    pub callees: Vec<(usize, Arc<FnCode>)>,
}

impl Trace {
    /// `(kinds;args;tys)` — what `src/neant/jit/arm64.nt`'s `jitCompileTrace` reads. One entry
    /// per step, in order; `kinds` follows `TraceOp`'s own numbering (see the match arms below),
    /// `args` is whichever int that step carries (a const/slot index, or a bail `ip`), `tys`
    /// is `0` for `Int`/untyped steps and `1` for `Float`.
    pub fn to_neant_input(&self) -> (Vec<i64>, Vec<i64>, Vec<i64>) {
        let mut kinds = Vec::with_capacity(self.steps.len());
        let mut args = Vec::with_capacity(self.steps.len());
        let mut tys = Vec::with_capacity(self.steps.len());
        let ty_bit = |t: TraceTy| if t == TraceTy::Float { 1 } else { 0 };
        for step in &self.steps {
            let (kind, arg, ty) = match *step {
                TraceOp::Push { k, ty } => (0, k, ty_bit(ty)),
                TraceOp::PushNull => (1, 0, 0),
                TraceOp::LoadL { slot, ty } => (2, slot, ty_bit(ty)),
                TraceOp::StoreL { slot, ty } => (3, slot, ty_bit(ty)),
                TraceOp::Dyad { k, ty } => (4, k, ty_bit(ty)),
                TraceOp::Pop => (5, 0, 0),
                TraceOp::Guard { taken: true, bail_ip } => (6, bail_ip, 0),
                TraceOp::Guard { taken: false, bail_ip } => (7, bail_ip, 0),
                TraceOp::GuardRewind { taken: true } => (8, 0, 0),
                TraceOp::GuardRewind { taken: false } => (9, 0, 0),
                TraceOp::FramePush { nargs } => (10, nargs, 0),
                TraceOp::FrameEnd => (11, 0, 0),
            };
            kinds.push(kind); args.push(arg as i64); tys.push(ty);
        }
        (kinds, args, tys)
    }
}

/// Caps a recording attempt that never finds its way back to the header (e.g. the loop actually
/// exited on this pass) at a fixed size, rather than adding separate exit detection — it will
/// always hit an out-of-scope op before too long in practice, this just bounds the wasted work.
/// Also what stops a recursive or deeply nested call from inlining forever.
const MAX_TRACE_STEPS: usize = 512;
/// A trace-local slot range per inlined frame, so this caps how much inlining one trace can carry
/// before the slots stop fitting. The register files (`jitTrLOCALS`, src/neant/jit/arm64.nt) are a
/// tighter limit in practice; this just keeps the numbers small enough to reason about.
const MAX_TRACE_LOCALS: u32 = 64;

pub struct Recorder {
    header: u32,
    steps: Vec<TraceOp>,
    consts: Vec<Value>,
    callees: Vec<(usize, Arc<FnCode>)>,
    /// The global a just-recorded `Op::LoadG` held a plain lambda in, until the `Op::Call` that
    /// must come next enters its frame (`enter_frame`) and takes it. Only ever one: the compiler
    /// emits a callee's `LoadG` directly before its `Call` (`callee`/`gen`, src/neant/core/
    /// compile.nt), arguments first, so even `f[g[x]]` never has two loaded at once — and any
    /// other op arriving while this is set is a `LoadG` that wasn't a call, which fails the
    /// recording rather than let a lambda pass as a value.
    pending: Option<(usize, Arc<FnCode>)>,
    /// One per frame currently being inlined: the trace-local base its slots are offset by.
    frames: Vec<u32>,
    real_upto: u32,
    next_base: u32,
    /// The function whose loop this is — where the outcome is filed (`FnCode::set_trace_compiled`/
    /// `set_trace_rejected`), whichever frame the recording happens to end in.
    owner: Arc<FnCode>,
    /// Set by `exit_frame`, cleared by the `Op::Call` that caused it — a `Call` that arrives
    /// without it is one that never became an inlined frame (a primitive, a projection, an
    /// already-compiled callee), and aborts the recording.
    returned: bool,
    failed: bool,
}

impl Recorder {
    pub fn start(header: u32, nlocals: u32, owner: Arc<FnCode>) -> Recorder {
        Recorder {
            header, steps: Vec::new(), consts: Vec::new(), callees: Vec::new(), pending: None,
            frames: Vec::new(), real_upto: nlocals, next_base: nlocals, owner, returned: false, failed: false,
        }
    }

    /// Called by `run_ops` right after executing `op` — its effect on `st` is already visible —
    /// for every op *after* the one whose backward jump started this recording, in whichever frame
    /// is currently executing. `k` is that frame's const table, read here and copied into the
    /// trace's own so a step means the same thing once the frames are gone. `ip_before`/`ip_after`
    /// are `run_ops`'s own `ip` right before and right after dispatching `op` (so for a `Jmpf`,
    /// `ip_before` is the natural fallthrough position and `ip_after` is whichever of
    /// `ip_before`/the jump target it actually went to). Returns `true` once this op closed the
    /// loop back to `header` — one full extra iteration successfully recorded.
    pub fn step(&mut self, op: Op, ip_before: u32, ip_after: u32, st: &[Value], k: &[Value]) -> bool {
        if self.failed { return false; }
        if self.steps.len() >= MAX_TRACE_STEPS { self.failed = true; return false; }
        // A frame just ended: the only op that can legitimately arrive now is the `Call` that
        // opened it, still finishing in the caller.
        let returned = std::mem::take(&mut self.returned);
        if returned && !matches!(op, Op::Call(_)) { self.failed = true; return false; }
        // Likewise a callee was just loaded: only its `Call` may follow (see `pending`).
        if self.pending.is_some() && !matches!(op, Op::Call(_)) { self.failed = true; return false; }
        let base = self.frames.last().copied().unwrap_or(0);
        match op {
            Op::Push(a) => match st.last() {
                Some(Value::Null) => self.steps.push(TraceOp::PushNull),
                _ => { let c = self.konst(k, a); self.push_typed(st, |ty| TraceOp::Push { k: c, ty }) }
            },
            // A local's value, not just a stack one — `of_local`'s stricter check (see it for why).
            // Both ops leave the value on the stack: `LoadL` pushed it, `StoreL` only peeks.
            Op::LoadL(a) => self.push_local_typed(st, |ty| TraceOp::LoadL { slot: base + a, ty }),
            Op::StoreL(a) => self.push_local_typed(st, |ty| TraceOp::StoreL { slot: base + a, ty }),
            Op::Dyad(a) => { let c = self.konst(k, a); self.push_typed(st, |ty| TraceOp::Dyad { k: c, ty }) }
            Op::Pop => self.steps.push(TraceOp::Pop),
            // Only a plain lambda about to be called, and only by the global it was loaded from —
            // that pair is what the entry guard rechecks. Anything else on the stack here (a
            // closure, a primitive, an ordinary global value) isn't something this can inline.
            // Nothing is recorded for it: the lambda is no value to the trace, and the `Call`
            // that follows pops it before the callee's frame — the part that is recorded — runs.
            Op::LoadG(a) => match (st.last(), &k[a as usize]) {
                (Some(Value::Lambda(code)), Value::Int(slot)) => self.pending = Some((*slot as usize, code.clone())),
                _ => self.failed = true,
            },
            // The frame it named has already been recorded by now (`enter_frame`/`exit_frame` run
            // inside it, before this op finishes); all that is left is to confirm it happened. A
            // `Call` that arrives without it never became a frame — a primitive, a projection, a
            // vector being indexed — and none of those is something this can inline.
            Op::Call(_) => if !returned { self.failed = true },
            Op::Jmpf(t) => {
                let taken = ip_after == t;   // Jmpf jumps (to t) exactly when the condition was false
                if self.frames.is_empty() {
                    let bail_ip = if taken { ip_before } else { t };
                    self.steps.push(TraceOp::Guard { taken, bail_ip });
                } else {
                    self.steps.push(TraceOp::GuardRewind { taken });
                }
            }
            // A trace is the ops that actually ran, in the order they ran, so an *unconditional*
            // jump contributes nothing to it: the ops it skipped simply aren't in the recording,
            // and the branch that chose this path is the `Jmpf` guard right before it. Only the
            // one back to `header` is special — that's a full iteration recorded, so stop there,
            // and only in the frame the loop is actually in.
            Op::Jmp(t) => return t == self.header && self.frames.is_empty(),
            // Leaving an inlined frame early: the ops it skipped simply aren't recorded, same as
            // any other jump. `exit_frame` is what closes the frame; the value `Ret` returns is the
            // top of the stack, which is where `FrameEnd` looks for it.
            Op::Ret if !self.frames.is_empty() => {}
            _ => self.failed = true,   // out of scope — Loop, Monad, StoreG, MkClosure, ...
        }
        false
    }

    /// `Vm::call_code`, once it has decided to actually interpret a frame (not project, not run an
    /// already-compiled version), for the callee the pending `Op::LoadG` named. `loc` is the
    /// frame's locals with its arguments already in place, so binding them is just a `StoreL` each
    /// — the same step an assignment in the loop body records, into slots that happen to belong to
    /// the callee rather than the caller.
    pub fn enter_frame(&mut self, code: &Arc<FnCode>, nargs: usize, loc: &[Value]) {
        if self.failed { return; }
        let ok = matches!(&self.pending, Some((_, c)) if Arc::ptr_eq(c, code));
        if !ok || self.next_base as usize + loc.len() > MAX_TRACE_LOCALS as usize {
            self.failed = true; return;
        }
        let (slot, code) = self.pending.take().unwrap();
        // The same global can be inlined at several call sites; one entry is enough for all.
        if !self.callees.iter().any(|(s, c)| *s == slot && Arc::ptr_eq(c, &code)) {
            self.callees.push((slot, code));
        }
        let base = self.next_base;
        self.next_base += loc.len() as u32;
        self.steps.push(TraceOp::FramePush { nargs: nargs as u32 });
        // Arguments come off the stack in parameter order — the first is on top (`Op::Call` pops
        // the callee, then argument 0) — so this pops exactly as many as it binds.
        for (i, v) in loc.iter().take(nargs).enumerate() {
            match TraceTy::of_local(v) {
                Some(ty) => {
                    self.steps.push(TraceOp::StoreL { slot: base + i as u32, ty });
                    self.steps.push(TraceOp::Pop);
                }
                None => { self.failed = true; return }
            }
        }
        self.frames.push(base);
    }

    /// The frame is done and its result is the value it left on the stack.
    pub fn exit_frame(&mut self) {
        if self.failed { return; }
        if self.frames.pop().is_none() { self.failed = true; return; }
        self.steps.push(TraceOp::FrameEnd);
        self.returned = true;
    }

    /// An error unwound out of a frame, or anything else that makes what was recorded stop
    /// matching what ran.
    pub fn abort(&mut self) { self.failed = true; }

    fn konst(&mut self, k: &[Value], a: u32) -> u32 {
        self.consts.push(k[a as usize].clone());
        (self.consts.len() - 1) as u32
    }

    fn push_typed(&mut self, st: &[Value], make: impl FnOnce(TraceTy) -> TraceOp) {
        match st.last().and_then(TraceTy::of) {
            Some(ty) => self.steps.push(make(ty)),
            None => self.failed = true,
        }
    }

    fn push_local_typed(&mut self, st: &[Value], make: impl FnOnce(TraceTy) -> TraceOp) {
        match st.last().and_then(TraceTy::of_local) {
            Some(ty) => self.steps.push(make(ty)),
            None => self.failed = true,
        }
    }

    pub fn failed(&self) -> bool { self.failed }
    pub fn header(&self) -> u32 { self.header }
    pub fn owner(&self) -> &Arc<FnCode> { &self.owner }
    pub fn finish(self) -> Trace {
        Trace {
            header: self.header, steps: self.steps, consts: self.consts,
            real_upto: self.real_upto, callees: self.callees,
        }
    }
}

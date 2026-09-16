//! Milestone 1 of the tracing JIT: record one concrete pass through a hot `while` loop as a flat,
//! branch-free op sequence with observed (not inferred) types, for `src/jit.rs`/
//! `src/neant/jit/arm64.nt` to compile (`jitCompileTrace`). This module is purely the *recording*
//! side — `Vm::run_ops` (src/vm.rs) drives it by feeding it every op it executes; recording never
//! influences what actually runs, only observes it, so a trace failing to record (or never being
//! attempted at all) can never change a program's result, only whether it gets to run faster.
//!
//! Scope, deliberately narrow for a first pass: `while` loops only (a loop header must have an
//! *empty* operand stack — true for `while`, false for `do[n;..]`, whose backward jump lands back
//! on the still-live counter; `Vm::run_ops` checks this before ever starting a recording), no
//! calls, no `Op::Loop`, no early `Ret`, only `Push`/`LoadL`/`StoreL`/`Dyad`/`Pop`/`Jmpf`/`Jmp`
//! inside the loop body. Anything else just aborts the attempt — same fail-closed default the
//! method-JIT's own compilability check already uses.
//!
//! A trace is *linear*: one concrete path, with every branch it took pinned by a guard, not a
//! control-flow graph. That's why an unconditional `Jmp` needs no representation at all (the ops
//! it skipped simply aren't in the recording) while every `Jmpf` becomes one, and why an `if` or a
//! `$[..]` inside the loop costs nothing until the day its condition actually flips.

use crate::value::*;

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
    Push { const_idx: u32, ty: TraceTy },
    /// `if`/`while`/`do`'s own statement-value convention — always immediately consumed by a
    /// `Pop`, same as the method-JIT's `jitOpPush` already special-cases (`src/neant/jit/
    /// arm64.nt`) — a true no-op, not a value that needs a register at all.
    PushNull,
    LoadL { slot: u32, ty: TraceTy },
    StoreL { slot: u32, ty: TraceTy },
    Dyad { verb_idx: u32, ty: TraceTy },
    Pop,
    /// The recorded direction of a branch this trace took, and where a later replay should resume
    /// *interpretation* if it disagrees — the other destination this exact `Jmpf` could have gone
    /// to (the loop's own condition check included — recorded the same as any other `Jmpf`).
    Guard { taken: bool, bail_ip: u32 },
}

#[derive(Debug)]
pub struct Trace {
    pub header: u32,
    pub steps: Vec<TraceOp>,
}

impl Trace {
    /// `(kinds;args;tys)` — what `src/neant/jit/arm64.nt`'s `jitCompileTrace` reads. One entry
    /// per step, in order; `kinds` follows `TraceOp`'s own numbering (see the match arms below),
    /// `args` is whichever int that step carries (a const/slot/verb index, or a bail `ip`), `tys`
    /// is `0` for `Int`/untyped steps and `1` for `Float`.
    pub fn to_neant_input(&self) -> (Vec<i64>, Vec<i64>, Vec<i64>) {
        let mut kinds = Vec::with_capacity(self.steps.len());
        let mut args = Vec::with_capacity(self.steps.len());
        let mut tys = Vec::with_capacity(self.steps.len());
        let ty_bit = |t: TraceTy| if t == TraceTy::Float { 1 } else { 0 };
        for step in &self.steps {
            let (kind, arg, ty) = match *step {
                TraceOp::Push { const_idx, ty } => (0, const_idx, ty_bit(ty)),
                TraceOp::PushNull => (1, 0, 0),
                TraceOp::LoadL { slot, ty } => (2, slot, ty_bit(ty)),
                TraceOp::StoreL { slot, ty } => (3, slot, ty_bit(ty)),
                TraceOp::Dyad { verb_idx, ty } => (4, verb_idx, ty_bit(ty)),
                TraceOp::Pop => (5, 0, 0),
                TraceOp::Guard { taken: true, bail_ip } => (6, bail_ip, 0),
                TraceOp::Guard { taken: false, bail_ip } => (7, bail_ip, 0),
            };
            kinds.push(kind); args.push(arg as i64); tys.push(ty);
        }
        (kinds, args, tys)
    }
}

/// Caps a recording attempt that never finds its way back to the header (e.g. the loop actually
/// exited on this pass) at a fixed size, rather than adding separate exit detection — it will
/// always hit an out-of-scope op before too long in practice, this just bounds the wasted work.
const MAX_TRACE_STEPS: usize = 256;

pub struct Recorder {
    header: u32,
    steps: Vec<TraceOp>,
    failed: bool,
}

impl Recorder {
    pub fn start(header: u32) -> Recorder { Recorder { header, steps: Vec::new(), failed: false } }

    /// Called by `run_ops` right after executing `op` — its effect on `st` is already visible —
    /// for every op *after* the one whose backward jump started this recording. `ip_before`/
    /// `ip_after` are `run_ops`'s own `ip` right before and right after dispatching `op` (so for a
    /// `Jmpf`, `ip_before` is the natural fallthrough position and `ip_after` is whichever of
    /// `ip_before`/the jump target it actually went to). Returns `true` once this op closed the
    /// loop back to `header` — one full extra iteration successfully recorded.
    pub fn step(&mut self, op: Op, ip_before: u32, ip_after: u32, st: &[Value]) -> bool {
        if self.failed { return false; }
        if self.steps.len() >= MAX_TRACE_STEPS { self.failed = true; return false; }
        match op {
            Op::Push(a) => match st.last() {
                Some(Value::Null) => self.steps.push(TraceOp::PushNull),
                _ => self.push_typed(st, |ty| TraceOp::Push { const_idx: a, ty }),
            },
            // A local's value, not just a stack one — `of_local`'s stricter check (see it for why).
            // Both ops leave the value on the stack: `LoadL` pushed it, `StoreL` only peeks.
            Op::LoadL(a) => self.push_local_typed(st, |ty| TraceOp::LoadL { slot: a, ty }),
            Op::StoreL(a) => self.push_local_typed(st, |ty| TraceOp::StoreL { slot: a, ty }),
            Op::Dyad(a) => self.push_typed(st, |ty| TraceOp::Dyad { verb_idx: a, ty }),
            Op::Pop => self.steps.push(TraceOp::Pop),
            Op::Jmpf(t) => {
                let taken = ip_after == t;   // Jmpf jumps (to t) exactly when the condition was false
                let bail_ip = if taken { ip_before } else { t };
                self.steps.push(TraceOp::Guard { taken, bail_ip });
            }
            // A trace is the ops that actually ran, in the order they ran, so an *unconditional*
            // jump contributes nothing to it: the ops it skipped simply aren't in the recording,
            // and the branch that chose this path is the `Jmpf` guard right before it. Only the
            // one back to `header` is special — that's a full iteration recorded, so stop there.
            Op::Jmp(t) => return t == self.header,
            _ => self.failed = true,   // out of Milestone 1's scope — Call, Loop, Ret, Monad, ...
        }
        false
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
    pub fn finish(self) -> Trace { Trace { header: self.header, steps: self.steps } }
}

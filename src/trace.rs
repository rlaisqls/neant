//! The tracing JIT's recording side: record one concrete pass through a hot loop as a
//! flat, branch-free op sequence with observed (not inferred) types, for `src/jit.rs`/
//! `src/neant/jit/arm64.nt` to compile (`jitCompileTrace`). This module is purely the *recording*
//! side — `Vm::run_ops` (src/vm.rs) drives it by feeding it every op it executes; recording never
//! influences what actually runs, only observes it, so a trace failing to record (or never being
//! attempted at all) can never change a program's result, only whether it gets to run faster.
//!
//! Scope, deliberately narrow: `while` and `do[n;..]` loops, `Push`/`LoadL`/`StoreL`/`Dyad`/
//! `Pop`/`Jmpf`/`Jmp`/`Loop` plus a call to a plain lambda held by a global (`f x`, `f[a;b]`,
//! nested however deep) in the body. Anything else — a closure, a primitive, a projection, a
//! global that isn't the function being called, a `Ret` out of the loop's own frame, an
//! `Op::Loop` inside a callee — just aborts the attempt: same fail-closed default the method-JIT's
//! own compilability check already uses.
//!
//! **The operand stack is part of the trace.** A `do` header is reached with the loop counter
//! live on the operand stack (the backward jump lands on the `Op::Loop` that decrements it), and
//! a loop nested inside a `do` has the outer counter under it the whole time. So a recording
//! starts with whatever plain ints the stack holds at the header (`entry`, checked by
//! `Vm::run_ops` — nothing else is ever live there in practice) and treats them as loop-carried
//! values: on the trace's virtual stack from step 0, back on it at the back edge, and handed back
//! to the interpreter at every exit. Which is also how *every* exit works now: a bail writes the
//! locals back, writes whatever the trace's stack holds at that point into the same buffer, and
//! the interpreter resumes at the bail's `ip` with exactly that stack (`CompiledTrace::run`,
//! src/jit.rs, rebuilds the `Value`s by the tags codegen recorded per exit). A `Jmpf` inside an
//! expression, an `Op::Loop`'s exit edge, an int-null collision in the middle of `n: (i*i)+(m*m)`
//! — each just names the ip the interpreter would have been at and what would have been on its
//! stack. `Dyad` steps record their own `resume_ip` for exactly that: the interpreter resumes at
//! the op *after* the colliding one with the null result already on the stack, and propagates it
//! from there the way it always would have.
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
//!   interpreter at the loop header with the locals and the operand stack the iteration started
//!   with, which is sound because nothing in the traceable subset can be observed from outside
//!   the loop. An int-null collision inside a callee rewinds the same way, for the same reason
//!   (`jitTrDyad`, src/neant/jit/arm64.nt); one in the loop's own frame hands off instead.

use crate::value::*;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceTy { Int, Float, Bool, Null }

impl TraceTy {
    /// What a value passing through the operand stack is. `Bool` is its own tag, not an `Int`:
    /// a comparison result (`Op::Dyad`'s `<`/`>`/`=`) is `Value::Bool`, and since an exit can now
    /// hand the live stack back to the interpreter, one that is handed back has to come back a
    /// `Bool` — `type`/`show` can see the difference, and so can `&`/`|`, whose result type
    /// depends on it (`1b&0b` is a bool, `2&1b` an int). In registers it is a plain 0/1 like
    /// any int, so `+ - *` and the comparisons treat it as one (`jitTrDyad`, src/neant/jit/
    /// arm64.nt). Anything else (a vector, a symbol, `Null`, ...) is out of scope for this pass.
    pub fn of(v: &Value) -> Option<TraceTy> {
        match v {
            Value::Int(_) => Some(TraceTy::Int),
            Value::Bool(_) => Some(TraceTy::Bool),
            Value::Float(_) => Some(TraceTy::Float),
            _ => None,
        }
    }

    /// The wire encoding shared with `jitCompileTrace` (src/neant/jit/arm64.nt) — per-step types
    /// on the way in, per-exit stack tags on the way back out. `Null` only ever comes *back*: it
    /// is the tag codegen gives a `PushNull` placeholder (`if`/`while`/`do`'s statement value)
    /// that happens to be live on the stack at an exit, so the interpreter gets its `Null` back
    /// rather than whatever the never-written register held; `of` never produces it (a `Null`
    /// being pushed is a `PushNull` step, not a typed `Push`).
    pub fn code(self) -> i64 { match self { TraceTy::Int => 0, TraceTy::Float => 1, TraceTy::Bool => 2, TraceTy::Null => 3 } }
    pub fn from_code(c: i64) -> Option<TraceTy> {
        match c { 0 => Some(TraceTy::Int), 1 => Some(TraceTy::Float), 2 => Some(TraceTy::Bool), 3 => Some(TraceTy::Null), _ => None }
    }

    /// The loop-carried operand stack a recording may start on (see the module doc comment): a
    /// `do` counter, or the counters of the `do` loops this one is nested in. Plain non-null
    /// ints only — a `Bool`/`Float` counter (`do[1b;..]` is legal — `int_of` accepts both) would
    /// be handed back to the interpreter as an `Int`, and the null is the one int a trace never
    /// takes for granted. `None` leaves this pass of the loop interpreted, exactly as before.
    pub fn entry_tags(st: &[Value]) -> Option<Vec<TraceTy>> {
        if st.len() > MAX_ENTRY_STACK { return None; }
        st.iter().map(|v| match TraceTy::of_local(v) { Some(TraceTy::Int) => Some(TraceTy::Int), _ => None }).collect()
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
    /// `resume_ip` is the op after this one: where the interpreter picks up if the result wraps
    /// to exactly the int-null sentinel, with that null on the stack where the result would be.
    Dyad { k: u32, ty: TraceTy, resume_ip: u32 },
    Pop,
    /// `do[n;..]`'s `Op::Loop`, as the direction this pass took. Not exited: the counter on top of
    /// the stack was positive and was decremented in place — a guard that bails to the loop's exit
    /// `t` with the counter popped if it ever isn't (that is exactly the interpreter's other edge).
    /// Exited (only a `do` *nested inside* the traced loop can record this — the loop's own
    /// header exiting ends the recording): the counter was `<= 0` and was popped — a guard that
    /// bails to the `Loop` op itself, counter still on the stack, if it is ever positive.
    Loop { exited: bool, bail_ip: u32 },
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
    /// The operand stack the recording started on (see the module doc comment) — what a compiled
    /// trace must find on the interpreter's stack to be entered, and what its own virtual stack
    /// holds at step 0 and again at the back edge.
    pub entry: Vec<TraceTy>,
}

impl Trace {
    /// `(kinds;args;tys;ips)` — what `src/neant/jit/arm64.nt`'s `jitCompileTrace` reads. One entry
    /// per step, in order; `kinds` follows `TraceOp`'s own numbering (see the match arms below),
    /// `args` is whichever int that step carries (a const/slot index, or a bail `ip`), `tys` is
    /// `TraceTy::code` (`0` for `Int`/untyped steps), `ips` is a `Dyad`'s `resume_ip` (`0` for
    /// every other step — a second int only that one kind of step needs).
    pub fn to_neant_input(&self) -> (Vec<i64>, Vec<i64>, Vec<i64>, Vec<i64>) {
        let mut kinds = Vec::with_capacity(self.steps.len());
        let mut args = Vec::with_capacity(self.steps.len());
        let mut tys = Vec::with_capacity(self.steps.len());
        let mut ips = Vec::with_capacity(self.steps.len());
        for step in &self.steps {
            let (kind, arg, ty, ip) = match *step {
                TraceOp::Push { k, ty } => (0, k, ty.code(), 0),
                TraceOp::PushNull => (1, 0, 0, 0),
                TraceOp::LoadL { slot, ty } => (2, slot, ty.code(), 0),
                TraceOp::StoreL { slot, ty } => (3, slot, ty.code(), 0),
                TraceOp::Dyad { k, ty, resume_ip } => (4, k, ty.code(), resume_ip),
                TraceOp::Pop => (5, 0, 0, 0),
                TraceOp::Guard { taken: true, bail_ip } => (6, bail_ip, 0, 0),
                TraceOp::Guard { taken: false, bail_ip } => (7, bail_ip, 0, 0),
                TraceOp::GuardRewind { taken: true } => (8, 0, 0, 0),
                TraceOp::GuardRewind { taken: false } => (9, 0, 0, 0),
                TraceOp::FramePush { nargs } => (10, nargs, 0, 0),
                TraceOp::FrameEnd => (11, 0, 0, 0),
                TraceOp::Loop { exited: false, bail_ip } => (12, bail_ip, 0, 0),
                TraceOp::Loop { exited: true, bail_ip } => (13, bail_ip, 0, 0),
            };
            kinds.push(kind); args.push(arg as i64); tys.push(ty); ips.push(ip as i64);
        }
        (kinds, args, tys, ips)
    }
}

/// How deep an operand stack a recording may start on (`TraceTy::entry_tags`) — one `do` counter
/// per level of `do` nesting around the header, so this is a nesting depth, and a generous one.
/// The compiled trace's operand-stack registers (`jitMAXDEPTH`, src/neant/jit/arm64.nt) are the
/// real limit, and codegen enforces that one itself; this just keeps the entry check cheap.
const MAX_ENTRY_STACK: usize = 4;

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
    entry: Vec<TraceTy>,
    /// Set by `exit_frame`, cleared by the `Op::Call` that caused it — a `Call` that arrives
    /// without it is one that never became an inlined frame (a primitive, a projection, an
    /// already-compiled callee), and aborts the recording.
    returned: bool,
    failed: bool,
}

impl Recorder {
    /// `entry` is what the interpreter's operand stack held at the header (`TraceTy::entry_tags`).
    pub fn start(header: u32, nlocals: u32, owner: Arc<FnCode>, entry: Vec<TraceTy>) -> Recorder {
        Recorder {
            header, steps: Vec::new(), consts: Vec::new(), callees: Vec::new(), pending: None,
            frames: Vec::new(), real_upto: nlocals, next_base: nlocals, owner, entry, returned: false, failed: false,
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
            // `ip_after` is the op after this one — where the interpreter resumes if this very op's
            // result turns out to be the int null on some later pass (see `TraceOp::Dyad`).
            Op::Dyad(a) => { let c = self.konst(k, a); self.push_typed(st, |ty| TraceOp::Dyad { k: c, ty, resume_ip: ip_after }) }
            Op::Pop => self.steps.push(TraceOp::Pop),
            // `do[n;..]`'s counter test — see `TraceOp::Loop`. `ip_before` is the op *after* the
            // `Loop` (run_ops has already advanced), so `ip_before - 1` is the `Loop` itself: where
            // the exited direction bails to, counter and all, if the counter is ever positive
            // again. Only in the loop's own frame: an `Op::Loop` in an inlined callee would need a
            // bail into the callee's bytecode, which the interpreter is not in — rejected, the
            // same way any other unsupported shape is, rather than rewound: a rewind is only
            // sound for a *branch*, and this op also mutates the stack. The traced loop's own
            // header exiting ends the recording outright: the ops that follow are not the loop.
            // Nothing about the counter's type is checked here — the interpreter has already
            // replaced it with a plain `Int` by now whatever it was (`int_of` takes a bool or a
            // whole float too), so the recorder can't see it; the tag it carries on the trace's
            // own virtual stack can, and `jitTrLoop` (src/neant/jit/arm64.nt) rejects anything
            // but a plain int there.
            Op::Loop(t) => {
                let exited = ip_after == t;
                let loop_ip = ip_before - 1;
                if !self.frames.is_empty() || (exited && loop_ip == self.header) { self.failed = true; }
                else { self.steps.push(TraceOp::Loop { exited, bail_ip: if exited { loop_ip } else { t } }); }
            }
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
            _ => self.failed = true,   // out of scope — Monad, StoreG, MkClosure, ...
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
            real_upto: self.real_upto, callees: self.callees, entry: self.entry,
        }
    }
}

# The compiler

How it is built, what it compiles, and how that is tested. Summary in [../README.md](../README.md).

### Stage 0 (done)

Lexer, parser, compiler and VM in Rust.

### Stage 1 (done)

lex/parse/compile rewritten in neant and running on that VM, PyPy-style; the Rust VM and primitives
stay as the runtime. The Rust front end has been deleted — `src/` is `{vm, prims, value, image, jit,
trace, main}.rs` plus `src/neant/` (the self-hosted sources, below, and the compiled image), and **source
never reaches Rust**.

- `src/neant/core/lex.nt` — the lexer.
- `src/neant/core/parse.nt` — the parser. Nodes are ``(`kind; ...)`` lists with identifiers as symbols.
- `src/neant/core/compile.nt` — the compiler. It emits bytecode as data: a unit is
  `(opcodes; args; consts; lines)`, where `lines[i]` is the source line op `i` came from (0 for
  synthetic ops). Consts are tagged ``(`k;v)`` ``(`g;`name)`` ``(`p;"+")`` ``(`a;"/";f)`` ``(`f;code)``.
  `exec` loads and runs it.
- `nrun src` is the whole pipeline; `load "f.nt"` is `nrun` over a file. The REPL and the file runner
  are both one `nrun` call.

`--build-boot` compiles `src/neant/{core,stdlib,crypto}/*.nt` **with the compiler already in the image** and serializes the
bytecode into `src/neant/image.nb` (`src/image.rs`), embedded in the binary by `include_bytes!`. Rust is the
VM plus the primitives, and nothing else.

`src/neant/` is grouped by what a file is for, not by whether it ends up in the image — `core/`
(lex/parse/compile, above) and `stdlib/` (prelude/table/json/encode/regex/test) do; `crypto/` is split between what's
in it (`crypto.nt`) and what's loaded on demand (`ed25519.nt`, `tls.nt` — see their own sections);
`net/` (`http.nt`) is loadable only. `load "f.nt"` doesn't care which directory a file is under.

### Stage 2 (started): a baseline JIT for integer loops and calls

The codegen is self-hosted the same way Stage 1 is: `src/neant/jit/arm64.nt` is neant, and its entry
points — `jitCompile` here, `jitCompileTrace` for [Stage 2b](#stage-2b-started-a-tracing-jit-for-hot-loops)
— take bytecode as data and return either a `Bytes` value (the encoded AArch64) or `::` for "not
compilable", the null-signals-failure convention used elsewhere. Only what genuinely needs the host
stays in Rust (`src/jit.rs`, AArch64 only, no crate dependency): `mmap`/`mprotect`/`__clear_cache`
declared directly as `extern "C"` (already linked in via libc/libgcc), ownership of the executable
memory, and the trampolines compiled code reaches through `blr`.

Both tiers are fail-closed the same way, and the rest of this section and Stage 2b assume it:
whatever fails the check is rejected once and runs interpreted forever after, and compiled code is
guarded at entry and can still bail out mid-run (`deopt`) back to the interpreter, which is always
correct and always present. That is safe because the compilable subset cannot observe anything
outside its own locals, so re-running a compiled stretch from scratch — or abandoning one halfway —
is never visible.

One wrinkle is unique to a JIT that compiles itself: the codegen's own bytecode contains the very op
kinds it exists to handle, so once one of its functions goes hot, compiling it would mean *calling*
it to walk its own bytecode, reentering its own `OnceLock` from inside that lock's initializer. A
thread-local guard (`already_compiling`) shuts compilation off for the whole nested call — the JIT's
own implementation is never a JIT target, which only affects how long one-time compilation takes.
(Compiling at all means running neant code, so it needs `&mut Vm` even from inside a trampoline
holding a raw `*mut Vm`; that reborrow is sound under the discipline every reentrant
interpreter-calls-host-calls-interpreter path already relies on, adverbs included — single-threaded,
strictly nested, the outer call's own `&mut self` untouched while the nested one runs.)

**This tier compiles a whole `FnCode`, after 64 calls**, if every op in it is provably pure integer
arithmetic over its own locals — `+ - * & | < > =`, `Push`/`LoadL`/`StoreL`/`Pop`/`Ret`,
`Jmp`/`Jmpf`/`Loop` (`while`/`if`/`do` control flow), and a call to another function that is
*itself* provably pure the same way. Anything else (a global that isn't an immediately-called
function, a closure, a float, a call to something impure) is rejected. The result is cached behind a
`OnceLock` (`FnCode::jitted`, `src/value.rs`) — built complete, then stored once, never edited in
place, so two threads racing to compile the same hot function waste one's work instead of racing on
it: the atomic-swap-in discipline [Concurrency](#concurrency) already commits to.

`Loop` (`do[n;..]`) is the one op whose two edges leave the interpreter's stack at different depths
— decrementing in place and falling through leaves it unchanged, but exiting also pops — so it is
the one place the compiler cannot just accumulate stack depth linearly through the bytecode; the
exit edge's depth is recorded and overrides that accumulation when the scan reaches it, which is
also what makes nested `do` loops compile.

Entry demands a plain non-null int for every argument and capture. After that, the deopt points are:
two ordinary ints wrapping to exactly the null sentinel (`0W+1`), which the interpreter would
propagate as a null and compiled code would not; a call whose arity doesn't match or whose callee
turns out not to be a plain function; and runaway recursion, since compiled-to-compiled calls go
through `blr` rather than `Vm::call_code`'s own depth guard. That last one has a limit of its own
(`MAX_CALL_DEPTH`) *and* counts against the interpreter's (`vm::MAX_DEPTH`), because a compiled
chain is entered from some interpreted depth and sits on top of it — both calibrated against the
smallest stack this can run on, a `spawn`ed thread's 2MiB, not the main thread's.

Calling another compiled function goes through one fixed trampoline (`jit_call`): it resolves the
callee exactly like `Op::LoadG` would, proves it pure *before* making the call at all — the only way
a later deopt of the caller cannot fire a real side effect twice — then recurses through compiled
code directly, never dropping back into the interpreter unless something deopts. The callee's
compiled version is cached behind its own `OnceLock` (`jit_for_call`), read with a plain atomic load
on every recursive step, and its locals go on a fixed-size native stack buffer (`try_run_raw`)
rather than a heap `Vec` — so a hot recursive call pays neither a lock nor an allocation.

**Locals live in registers.** That buffer is only how a call's locals get *in*: every local a
compiled function touches — params, scratch and captures alike, in first-seen bytecode order
(`jitLocalSlots`) — gets one register for the whole function, loaded once in the prologue, and
`LoadL`/`StoreL` become register moves that are never written back. Sound because `Compiled::run`
reads only the returned value and the ok word, and neither entry point looks at the buffer again, so
a return or a deopt simply abandons the registers. Unlike the tracing tier's version of the same
idea, a compiled function makes calls and its locals have to survive them: the first six registers
handed out (`jitLREGS`) are callee-saved (x23..x28), preserved by the trampolines for free at one
`stp`/`ldp` per pair actually used; the next five are caller-saved (x6..x8, x16, x17) and spilled
around every `blr` the way the live operand stack already was; a twelfth local and beyond stays in
the buffer, reached through x19 exactly as every local used to be, so no function is rejected for
being too wide. A vector-classified slot (below) is a register too — its value is a pointer, a plain
64-bit word only ever handed to the vector trampolines. The frame grew from 112 to 192 bytes, which
moved where compiled recursion overflows a 2MiB stack from ~2000 levels to ~1800.

**Vector indexing inside a compiled loop.** `x[i]` and `x[i]:v` compile for a scalar int index on an
`Ints` **parameter or capture** — not a scratch local, since nothing in the compilable subset can
construct a vector. This language has no indexing opcode: a vector applied to an int just *is*
indexing (`compile.nt`'s `app` node), through the same `Op::Call` a plain application emits. So
`jitClassifySlots` walks the bytecode once and accepts a slot only if *every* appearance of it is
one of exactly three shapes — `LoadL(s)` immediately consumed by `Call(1)` (a read), the literal
3-op run `TakeL(s); Amend(1); StoreL(s)` that `iassign` always emits back to back (a write), or a
bare `LoadL(s)` at the very end of the body or immediately before a `Ret` (the vector is the
result, below). Any other appearance disqualifies it; there is no partial typing.

**The access is inlined — no call.** `buf` (`src/jit.rs`) is the locals area followed by one
descriptor word per classified slot, its element count; the slot's own word is the element *data
pointer*. A read is then a load of that count, one unsigned compare that catches a negative index
and an out-of-range one together, and a scaled `ldr` off the slot's own register; a write is the
same with an `str`. Nothing about `Arc`'s layout is assumed, because Rust still does all of it — at
entry `bind_vec` clones the value into the `vecbuf` side table and, for a slot the body *writes*,
calls `Arc::make_mut` on that clone straight away. That is the same copy-on-write
`Op::Amend`/`scatter` (`src/prims.rs`) perform and the same one `jit_vec_set` used to perform on the
first write, only hoisted to entry, which is what makes the elements' address stable for the rest of
the call: a read-only slot is an `Arc` this frame holds a reference to and nothing may mutate, a
written one is uniquely ours and cannot be reallocated. A second live reference to an amended vector
still never sees the write, which `tests/jit.nt` asserts. The trampolines stay in `src/jit.rs` for
the x86-64 backend, which has not been taught this yet.

**A compiled function can return the vector it filled.** Locals are never written back, so until
this everything a compiled body did to a vector was invisible to its caller and the only worthwhile
shape was one that reduces to a scalar — which is why `src/neant/crypto/bignum.nt` could not put a
whole schoolbook product inside one compiled call. Now a body whose last op is a bare `LoadL` of a
classified slot returns that slot's vector: the machine code's x0 is a placeholder and `try_run`
hands back the `Value` the slot was bound to, which the inlined stores have been filling all along.
Every return has to agree — a function returning an int down one path and the vector down another
is refused outright, since there would be no single answer for `src/jit.rs` to believe — and a
vector-returning callee cannot be entered through `jit_call`, whose whole point is that a result is
an int in a register, so that one call deopts.

**The bit builtins are instructions, not calls.** `x band y` is an ordinary two-argument call to a
global holding a `Value::Prim`, so without this every limb mask and every shift inside a compiled
loop was a `blr` through `jit_call` — and limb arithmetic is nothing but masks and shifts.
`src/jit.rs` resolves `badd band bor bxor shl shr` to their global slots, hands them to the codegen,
and records which primitive each slot held; a body that inlined one carries an entry guard that the
slot still holds that exact primitive, because these are ordinary globals and rebinding `band` is
legal. The two shifts deopt unless the count is in [0;64): AArch64 reads it mod 64 where `ib_shl`/
`ib_shr` truncate to u32 and answer 0, so outside that range the machine and the interpreter
genuinely differ. All six work on the raw 64-bit pattern and so may *produce* the int-null sentinel
where `+ - *` may not, and the result is checked for it exactly as an arithmetic one is.

**The virtual operand stack.** A value's physical register used to be its depth — entry *i* lived in
x9+*i*, so a `LoadL` of a local that already had a register of its own still emitted a `mov`, and a
literal always cost a `movz`. Each entry now records where the value actually *is*: a register, or a
constant not yet loaded into one. So a local feeds an add out of its own register, `i+1` is one
`add` with a 12-bit immediate, and a comparison whose result is immediately consumed by a `Jmpf` is
one `cmp` and one conditional branch instead of a `cmp`, a `cset` and a `cbz`. What makes it safe is
that an entry is *materialised* — moved into x9+*i*, where it would always have been — whenever
anything could invalidate it: before every branch and at every branch target, so both halves of a
merge agree where a value lives; before a local's register is written, since an entry aliasing that
register holds the old value; and before a `do` counter is decremented in place. Materialising is a
`mov` the old scheme emitted unconditionally, so the worst case is what it used to cost. On the
measured scalar loop this removes eight instructions of twenty-one and changes nothing at all, which
is itself the finding: that loop is bound by its four branches, not by its instructions. Numbers for
both tiers are together at the end of Stage 2b.

#### The x86-64 backend (compiled, never executed)

Both tiers have a second backend, `src/neant/jit/x86.nt`, for x86-64 System V. Its entry points are
`jitCompileX86` and `jitCompileTraceX86` — the same contracts as the AArch64 pair, named apart so
both files can be in the boot image at once — and `src/jit.rs` picks the pair its target
architecture needs (`CODEGEN`) and is otherwise the same code for both. Everything that decides
*whether* something compiles is called out of `arm64.nt` rather than copied (`jitClassifySlots`,
`jitLocalSlots`, `jitDyadKind`, `jitTraceTouched`, `jitTrPos`, ...), so the compilability walk, the
slot classification and the deopt/guard points are literally the same code and the two backends
accept and reject the same functions and traces. What differs is the encoders, the register
assignment, and how branches are resolved.

The roles the AArch64 file documents map onto this ABI with less room. The long-lived pointers have
to survive the calls a compiled function makes, so they take callee-saved registers: `rbx` the
locals buffer, `rbp` the int-null sentinel, `r12` the `Vm`, `r13` the ok word. The operand stack is
`rsi`, `rdi`, `r8`–`r11` — caller-saved and spilled around every call, as `x9..x14` are — and `rax`
carries the trampoline address for `call rax` and then its result. That leaves `r14`/`r15` for
locals-in-registers (against AArch64's eleven), with the third local and beyond staying in `buf`
exactly as the AArch64 overflow slots do, so no function is rejected for being wide. Because this
ABI's six argument registers *are* four of the operand-stack registers, a call spills the whole
live stack and then loads its arguments back out of those spill slots: there is no order in which
the register moves alone are safe, and this removes the class of bug entirely for a handful of
memory operations on a path that is already making a call. The tracing tier, unlike its AArch64
twin, does need a prologue — six `push`es and six `pop`s at the one tail every exit funnels through
— because nine caller-saved registers cannot hold a buffer pointer, an out pointer, a sentinel, a
scratch, six operand-stack slots and a register per local.

An x86 instruction is variable length, so **every** branch is emitted in its `rel32` form and never
the short `rel8` one, every memory operand uses a full `disp32`, and every constant the 10-byte
`mov r64, imm64`: an instruction's length then depends on its form alone and never on its operands'
values, which is what makes the standard two-pass resolution exact. Pass one emits the instruction
in full with a zero displacement and records the byte offset of that four-byte field; pass two
subtracts and writes four bytes, moving nothing. (The AArch64 file can re-encode a whole branch
word at patch time; here the opcode — and the `test` that sets the flags a conditional branch reads
— is chosen at emission time and only the displacement is left.)

Two places this backend is deliberately narrower, both refusals, so the affected loop just stays
interpreted: **`&`/`|` on floats are not compiled**, because SSE2's `minsd`/`maxsd` return their
second operand when either is a NaN, which is not the IEEE minNum/maxNum that `f64::min`/`max`
(and therefore the interpreter) implement — AArch64 has `FMINNM`/`FMAXNM` and this does not, and
emulating it is a compare, two branches and a NaN case for an operation no measured loop performs.
And the tracing tier has five int local registers against AArch64's eight, so a wider loop body is
not traced. Float `+ - *` and the float comparisons do compile; the comparisons go through
`ucomisd` and the *unsigned* condition codes, because NaN sets ZF, CF and PF together — `x<y` is
`ucomisd y, x` plus `seta` (operands swapped rather than the condition inverted), and `=` needs
`sete` and `setnp` and'ed, which is the one place the integer `&`/`|` verbs being min/max leaves
`and` with a job to do.

In `src/jit.rs` the only architecture-specific parts left are that codegen name and the instruction
cache: `__clear_cache` is declared and called under `#[cfg(target_arch = "aarch64")]` only, because
on x86-64 the caches are coherent and the `mprotect` already orders the write — there is nothing to
do, which is why that is a `cfg` on the call rather than a call to a helper that would be empty on
one target. `MAX_CALL_DEPTH` keeps its AArch64 calibration; the x86-64 frame is smaller (six pushes
and a 72-byte frame, 128 bytes with the return address, against 192), so the same limit is if
anything more conservative there.

**What is verified.** Every encoder is asserted byte for byte against `nasm -f bin` output for the
same mnemonic and operands (`tests/x86.nt`, which says how to re-derive each expectation), and the
emitted code was read back with `ndisasm -b 64` and `llvm-mc --disassemble --triple=x86_64` — the
`llvm-objdump` in this image does not take `-b binary`. The two-pass branch resolution is checked by
computing where a displacement has to land from the encoders' own lengths and reading the four bytes
that were patched in, for a forward jump, a backward jump and a deopt branch. Compilability is
asserted against the AArch64 backend on 25 sources and 10 recorded traces — the accept/reject
verdict, the classified vector slots, and for a trace the whole buffer layout and exits table that
`src/jit.rs` reads back — and the same input twice is required to give byte-identical output.
`cargo check --release --target x86_64-unknown-linux-gnu` passes with no warnings, where before this
work that target compiled 17 dead-code warnings' worth of stubbed-out JIT; that clean build is the
proof the `cfg` work is right.

**What is not.** Nothing this backend emits has ever executed. The development machine is AArch64,
so there is no evidence that the code runs, that the System V details are right in practice (stack
alignment at a `call`, what the trampolines actually preserve), or that a compiled function returns
what the interpreter would. That last one matters most: a wrong register here is a *wrong answer*,
not a crash, and the fail-closed design of both tiers does not help with it — it only guarantees
that what fails to compile falls back. So the first thing to run on an x86-64 box is
`cargo test --release`, whose JIT tests compare every compiled path against the interpreter
(["What the tests check"](#what-the-tests-check)); expect to debug — and note that the generated
differential suite below would run there too, which is a better first hour than reading the
disassembly. Every performance number in this section and the next is AArch64's.

### `f each x`, in compiled code

`each` applied a lambda by calling it, once per element, through `Vm::call_code` — which redid for
every element work that cannot change between them: the arity and projection checks, the JIT cache
lookup and its atomic, and a round trip through the locals pool. Measured on a 100k int vector, that
machinery was about 40ns an element against roughly 8 for the adverb itself and 18 for the body of
`{x+1}`. The call, not the work. `(neg) each x`, a primitive that enters none of it, was 8.7ns where
`{x} each x` — an empty body — was 49.

There are now two steps below that. `each_lambda` (src/vm.rs) resolves the callee once for the whole
vector and keeps one locals buffer. `each_ints_jit` goes further when the input is an int vector and
the method JIT has compiled the body: `try_run_each_int` (src/jit.rs) checks once what `try_run`
checks per call, then runs the compiled body straight over the raw elements — no `seq` boxing on the
way in, no `pack` type scan on the way out.

```
{x+1} each x        67.7 ns/elem  ->  2.3      {x} each x     49.0  ->  2.3
```

Which shapes reach it, measured one per process because a benchmark harness's own hot loop perturbs
this: `{x}`, `{x+1}`, `{x bxor 3}`, `{x shr 2}`, `{[x] y: x+1; y*2}`, `{[x] y: x+1; y}`,
`{$[x>5; x; 0-x]}`, `{x>3}`, `{x&3}` all run fused. A body containing a call (163ns) or a loop
(35-39ns) does not, and falls back to the path above, which stays the definition of what the fused
one has to agree with — `tests/jit.nt` asserts that agreement element by element over a vector that
includes the int null and both ends of the range.

**A bug this turned up.** Compiled code hands back a bare `i64` and the caller has to put a type
back on it; it always said `Int`. For a function whose answer is a comparison that is wrong:

```
f: {x>3}
f 5                       1b
do[200; f 5]; f 5         1                 <- same function, now hot
```

`jitRetKind` (src/neant/jit/arm64.nt) is what tells it which, from the bytecode: a returned value
produced by an unfused comparison is a boolean; a body in which no comparison escapes a branch, no
boolean constant appears and nothing unmodelled happens can only produce an integer. Anything else
is "cannot say", which keeps the older behaviour — so `try_run` is strictly better than it was, and
the fused `each` declines the fast path entirely rather than guess. x86.nt still returns the
five-element shape without this field, and is no worse off than before.

`x f' y` has the same pair of paths (`each2_ints_jit`, `try_run_each2_int`), which is what `prior`
is written in terms of:

```
a {x+y}' b          73.0 ns/elem  ->  3.5      prior[{x-y};a]   56.3  ->  4.0
```

**A second bug, and a worse one.** Writing the two-argument suite turned up a compiled function that
answered differently from the interpreter — the one failure the fail-closed design cannot catch,
since a wrong register is a wrong *answer* and not a bail:

```
f: {[x] $[x>3; x; 99]}
f 9                       9
do[300; f 9]; f 9         99                <- same function, now hot
```

Every `$[c; a; b]` whose arms end the function was affected, at any arity, for as long as the method
tier has existed. The position after the last op is a jump target — the then-arm jumps there — and
the linear walk stops at the last op, so it never visited it. The arm that fell through was still
holding its value as a pending constant when the epilogue materialised it, and that store is emitted
*after* the two paths join: it ran on both and overwrote what the other arm had left. An arm ending
in anything but a bare constant was fine, which is how it survived — `{$[x>5; x; 0-x]}` was always
right. The fix brings that position to the canonical layout like every other target.
`tests/jit.nt` now takes the interpreted answer, warms the function past the threshold, and demands
the same answer back, for nineteen shapes; eleven of them fail on the code before the fix.

### A buffer the function makes for itself

A compiled body cannot construct a vector: nothing in the op subset builds one. So a vector slot
could only ever be a parameter or a capture, which the runtime fills at entry — and a loop that
starts `r: n#0` fell out of the compiled path entirely, which is how anyone writes a loop that fills
a buffer.

It needs no new opcode. `n#0` is the five ops `Push(0) LoadL(n) Dyad(#) StoreL(r) Pop`, and the
runtime can do exactly that at entry from the length already sitting in the parameter slot, the same
way it binds a caller's vector. So `jitVecAllocs` (src/neant/jit/arm64.nt) finds those runs, the
codegen emits nothing for them, and `try_run` makes the buffer.

```
{[n] r: n#0; i: 0; while[i<n; r[i]: (i*2); i: i+1]; r}     1.2x  ->  3.9x
```

That is the same 3.9x a parameter buffer already got, which is the point: it is now the same path.
A literal length (`r: 64#0`) works too.

Every condition on it is about the buffer being there before anything looks at it — the fill is the
integer `0`, the length is a parameter that is never reassigned or a literal, the slot is a scratch
local, and this is the slot's first appearance. The last one is a correctness trap rather than a
convenience: **the runtime allocates unconditionally**, so an allocation the interpreter might skip
would be a divergence, not an optimisation. `$[c; r: n#0; 0]` followed by a read of `r` answers 0
compiled and signals interpreted, so an allocation with a branch before it is not one.

**Ints only.** A vector slot holds an `Ints` and nothing else, so `64#0x00` is still not a buffer
this can make, and the byte-building loops named above are still interpreted for that reason rather
than the one the previous paragraph used to give.

### Generated differential testing

Both of the wrong answers above were found by accident, while measuring something else. That is the
problem, not the bugs: a wrong register is a wrong *answer* and not a bail, so the fail-closed design
that makes everything else here safe does nothing for this one class, and a hand-written list only
ever covers the shapes somebody thought of.

`tests/jitdiff.nt` generates them instead — about 2200 function bodies a run, from the compilable
subset: arithmetic, comparisons, `$[..]`, the bit builtins, loops, an early `:` return, values
carried through locals. Each body is built twice, once as written and once with a single integer
literal wrapped in `(- - n)`. The pair has the same value for every input and differs only in that
the second can never be compiled, because a monadic minus pair is outside the subset either tier
takes. That twin is the only honest interpreter baseline available: since the tracing tier landed, a
function's own first call is not one, because a loop inside it can be taken over partway through.
The seed is fixed, so a failure prints the body that caused it and reproduces exactly. It costs 7
seconds.

It found a third wrong answer within minutes of working, in a shape nobody had written down:

```
f: {[x;y] (($[x>y; 8; 16]) + 1)}
f[0;0]                    17
do[300; f[0;0]]; f[0;0]   18                <- same function, now hot
```

A fused comparison leaves its flags for the branch to read, so that branch cannot materialise
anything itself — and only the comparison's own two operands were being put in their canonical
registers first. Everything *under* them stayed wherever it happened to be, and the path that takes
the branch arrives at a target whose state has been reset to "every slot is canonical". Operands are
pushed right first, so in `($[c; a; b]) + 1` the `1` sits under the comparison the whole way across
it as a pending constant that was never stored. `1 + $[c; a; b]` has nothing underneath and was
always right, which is how it survived. The fix materialises below the operands before the `cmp`,
where there are no flags to clobber yet.

Three wrong answers in the method tier, all in branch handling, all found in one sitting once
something was looking. The generator is the part worth keeping.

**The tracing tier, aimed at properly.** A trace is one recorded path kept behind guards, so what
has to hold is that a guard which stops agreeing hands control back at exactly the right bytecode
position, with every local and the whole operand stack as the interpreter would have had them. Two
loop templates only stressed that incidentally. There are now ten more: trip counts that straddle
the threshold, a branch that flips at a chosen iteration, three loop-carried locals, a stack that is
not empty under the branch, a loop inside a loop, an early `:` return out of a loop, a `break`, a
vector slot written and one read, an index that walks off the end, and `+` from `0W-7` so the result
reaches the int null mid-loop. About 4200 bodies a run, 17 seconds.

That the templates have teeth is checked rather than assumed: at 200 iterations a generated loop
runs 9x faster than its twin after four calls, and 6x at 64, so it is being taken over. At 8 it is
not, which is the point of straddling.

**They found nothing.** Ten shapes, some 2800 generated loop bodies, no disagreement. That is
evidence and not proof — `break` and the vector templates only run about 1.2x faster than their
twins, so they are mostly exercising the refusal path rather than a compiled one — but after three
wrong answers in the other tier it is worth saying plainly that this one was looked at.

### Stage 2b (started): a tracing JIT for hot loops

The tier above tiers up whole *functions*, after 64 calls. That misses the shape this language is
most often written in: one call that loops a million times. So a second tier records **traces** —
`src/trace.rs` does the recording, inside the VM's own dispatch loop, and `jitCompileTrace` the
codegen.

A loop header is counted every time a backward `Jmp` reaches it (`FnCode::loop_action`) — per
header, not per function, so a loop goes hot inside a single call. At 64 the VM records the *next*
iteration: every op it actually executes, in order, each tagged with the type it was actually
observed to hold. What comes out is one straight line with no control flow in it at all. Where the
iteration branched, the trace keeps a **guard** — the direction taken, plus the bytecode `ip` the
other direction would have gone to — and an unconditional `Jmp` leaves nothing behind, since the ops
it skipped never ran. So an `if` or a `$[..]` inside the loop costs nothing until the day its
condition actually flips. Recording is pure observation: it cannot change what a program computes,
only whether some of it gets to run faster. Scope is `while` and `do[n;..]` loops,
`Push`/`LoadL`/`StoreL`/`Dyad`/`Pop`/`Jmpf`/`Jmp`/`Loop` in the body, and calls to plain lambdas
held by globals (both below).

What a trace buys over the method tier is types. That tier has to *prove* every op integer from the
bytecode alone; a trace just writes down what the values were, so **floats compile too** — a second
operand stack in `d16..d21` beside the integer one in `x9..x14`, with a per-value type tracked at
codegen time (`tstack`) rather than a single depth counter. `&` and `|` on floats are
`FMINNM`/`FMAXNM`, not `FMIN`/`FMAX`: Rust's `f64::min`/`max` propagate the non-NaN side and the
plain forms don't. Locals get a register each here too (`jitTrLOCALS`: x2..x8/x16 for ints, d0..d7
for floats), and the buffer they pass through (`CompiledTrace`, `src/jit.rs`) is indexed by a layout
`jitCompileTrace` decides and `src/jit.rs` reads back out of its result rather than recomputing —
both halves have to agree what a position means, and one side deciding is what guarantees they do.

**Every exit hands the interpreter its operand stack.** A guard's stub writes the locals back, then
whatever the trace's own virtual stack holds at that point into the same buffer past the locals, and
returns an *index* into an exits table `jitCompileTrace` returns alongside the code — one
`(resume ip; stack tags)` per exit. `CompiledTrace::run` rebuilds those values by tag, `Vm::run_ops`
pushes them and resumes at that `ip`, indistinguishable from having interpreted the whole time. So a
guard is legal anywhere, not only where the stack happens to be empty: a `Jmpf` inside an expression
hands back that expression's operands, and the one deopt that fires *mid*-iteration — `0W+1`
wrapping to the null sentinel — resumes at the op *after* the colliding one with the null result on
top, which is exactly the `Int` the interpreter would have produced and propagates from there. A
trace with no rewind exit (below) therefore has **no memory operation in its body at all**.

Tags are why `Bool` is its own type on the virtual stack: a comparison result handed back has to
come back a `Value::Bool` — `type` sees the difference, and so does `&`/`|`, whose result is a bool
exactly when both operands are (`1b&0b`, not `2&1b`), a rule codegen reproduces and the recorder's
observed result type double-checks. In registers a bool is a 0/1 like any int. A `Bool` *local* is
still refused: locals round-trip through the buffer as raw words of their slot's one type, so
`b: i<n` would come back an `Int`.

**`do[n;..]` loops.** A `do` header is the `Op::Loop` that tests and decrements its counter, and it
is reached with that counter live on the operand stack — which is what kept `do` from tracing. Now
the entry stack is part of the trace: recording starts (and a compiled trace is entered) when the
stack holds only plain non-null ints (`Trace::entry`), and those are loop-carried values — on the
virtual stack from step 0, back in the same registers at the back edge, handed back at every exit
like anything else on it. `Op::Loop` is a step of its own: on the edge the recording took, a guard
that bails to the loop's exit with the counter popped if it is ever `<= 0` — which *is* the
interpreter's other edge, stack and all — plus a decrement in place. A `do` nested inside records
unrolled, its exit edge a guard in the other direction bailing to the `Loop` op itself; a `while`
inside a `do` traces with the outer counter under it the whole time; `do` inside `do` traces with
two counters on its entry stack. An `Op::Loop` inside an inlined callee is rejected rather than
rewound — its bail would be into the callee's bytecode and, unlike a callee's branch, it mutates the
stack — and a counter that isn't a plain int (`do[1b;..]` and `do[2.0;..]` are legal, `int_of` takes
both) is refused at the entry check or, nested, by its tag at codegen.

**Calls are inlined, not called.** The recorder lives on the `Vm` rather than in one `run_ops`
frame, so when the loop body calls a plain lambda it follows the interpreter into the callee's frame
(`Recorder::enter_frame`/`exit_frame`) and keeps writing down what runs. What comes out is still one
flat sequence with no call in it: a `FramePush` remembering how deep the operand stack stood under
the arguments (that is where the result has to end up), a `StoreL`+`Pop` per argument binding it
into a slot of the callee's own, the body's ops exactly as in the loop, and a `FrameEnd` moving the
one value the frame leaves behind to where the caller expects it. Each inlined frame gets a disjoint
range of trace-local slots past the loop's own (`real_upto`), and those are **virtual**: a register
like any other local, but never loaded, stored or written back — they have no value before the loop
and nobody wants one after. So `f[a;b]`, `f[g[x]]`, a callee that calls another lambda, the same
lambda at two call sites, a callee with a scratch local or an early `:x` all inline, and a bounded
recursion inlines as far as it actually recursed. The method tier is bypassed for a callee while a
recording is in progress — the recorder has to *see* the callee's ops — which only defers a
tier-up, the recording being one iteration long.

Two kinds of guard fall out of that. **Which function the global holds** is checked once per entry —
enough, since nothing a compiled trace runs can assign a global — and a trace whose callee was
reassigned is retired rather than refused forever: the header counts afresh and is recorded again
against the new definition, up to `MAX_RETRACE` (4) times (`FnCode::retrace`). Reassigning a
function at the REPL between runs is ordinary; a loop whose callee changes on every run is not worth
a compile each time. **A branch inside a callee** cannot bail the way one in the loop's own frame
does, because the `ip` it would resume at is in a frame the interpreter is not in. Those are
`GuardRewind`s, the one exit left that does not hand off: it throws the half-finished iteration away
and resumes at the loop header from the values the buffer says the iteration started with — which is
why a trace that contains one stores its written locals and entry stack there at the top of every
iteration. An int-null collision inside a callee rewinds the same way. If a callee's condition flips
for good, every later iteration enters the trace, rewinds and is interpreted; measured, that costs
nothing over interpreting alone (496ms against 510ms for 2M such iterations), so there is no cliff
to fall off. Everything that is *not* a plain lambda in a global — a closure, a primitive, a
projection (an arity mismatch is one), a vector being indexed, a callee that reads a global, a `Ret`
out of the loop's own frame — fails the recording and leaves the loop interpreted, never
miscompiled: a `Call` that didn't become an inlined frame arrives at the recorder without one having
`returned`, and that is the whole check.

**Measured** on this machine, `cargo test --release -- --ignored --nocapture`. Every ratio is
against the same body with `- -` (two monadic negations, an identity) spliced in: `Op::Monad` is
rejected by both tiers, so the twin is guaranteed interpreted. It does marginally more work per
iteration than the original, so each number is a few percent optimistic.

- **~190x** — a scalar `while` loop on the method tier (`manual_perf_measurement`): 0.95ms against
  185ms, 1M iterations.
- **~160x** — the same shape on the tracing tier (`manual_trace_perf_measurement`): 5.5ms against
  890ms, 5M iterations. No branch and no call in the body, so the trace has no rewind exit and
  therefore no memory operation at all; that removed store was worth ~5%.
- **~195x** — a loop calling `{x*2}` every iteration (`manual_trace_call_perf_measurement`): 5.8ms
  against 1.14s, 5M iterations, the twin paying a real `Op::Call` → `call_code` → `execute` each
  time. The same speed as the loop with no call in it, which is what inlining should mean.
- **~112x** — a `do` loop (`manual_trace_do_perf_measurement`): 6.4ms against 725ms, 5M iterations.
  One compare-branch-decrement per iteration more than the `while` form, which shows.
- **~200x** — `x[i]` inside a compiled loop (`manual_vector_perf_measurement`). Two reads, a
  multiply and an accumulate per iteration now run at 0.9–1.0ns against 2.7ns when each access was
  a `blr` plus the operand-stack spill around it: level with the scalar loop, which is what an
  inlined bounds check and a scaled load should mean.
- **~3.5x** — recursive calls, `fib 27` (`manual_recursive_perf_measurement`). A call costs far more
  than a loop iteration even with the lock and the allocation gone: marshalling arguments, the depth
  guard, resolving the callee.

### What the tests check

There is no external oracle left, so the front end is pinned by fixpoints and by behaviour:

- **Generation 2.** Recompile every boot file through the pipeline it defines, then require it to lex,
  parse and compile the corpora to byte-identical output and still pass every `tests/*.nt`. A compiler
  that does not reproduce its own output when rebuilt by itself fails here — this is what the Rust
  oracle used to catch.
- **The image is a fixpoint.** Compiling the current sources with the embedded image reproduces
  that image exactly. Catches both a stale `image.nb` and a compiler change rebuilt only once.
- **The language cases.** 363 source/result pairs — semantics, error messages, error line numbers and
  call stacks — every one through `nrun`. They live in `tests/lang.nt`, in neant: a case is
  `("1+2"; "3")`, run with `repr join spawn {nrun s}`, where the `spawn` gives it the fresh globals
  the Rust harness used to get from `snapshot`/`restore`.
- **Front-end errors.** Lexer and parser messages and their line numbers, asserted literally.
- **The JIT against the interpreter.** Every compiled path is checked by running the same loop
  twice — once so it tiers up, once with `- -` spliced in so it provably never can — and requiring
  the two to agree, at sizes that straddle the tier-up threshold. The guards and deopts get the same
  treatment: a branch that flips after the trace was recorded, `0W+1` wrapping to the null sentinel
  mid-loop under an operand stack of every shape, a bool that has to come back a bool, an
  out-of-range index, a second live reference to an amended vector, a callee's global reassigned
  between runs. So does every way a shape can *refuse* to compile — a `do` inside a callee, a
  non-int counter, a closure or primitive or projection callee, an arity mismatch, a global read,
  deep recursion, too many locals — since what matters there is that it be rejected rather than
  miscompiled. These are `tests/jit.nt`, in neant: a case is a source string and the text the REPL
  prints for it, which needs no Rust.
- **The JIT against the interpreter, on bodies nobody wrote.** The list above covers the shapes
  someone thought of, and the three wrong answers the method tier has produced were all found by
  accident. `tests/jitdiff.nt` generates about 2200 bodies a run from the compilable subset and
  requires each to agree with a twin that has `(- - n)` spliced in and therefore can never compile.
  Nineteen templates: whole functions for the method tier, and for the tracing tier loops with
  guards that flip, loop-carried locals, nesting, `break`, an early return, vector slots, an index
  off the end and arithmetic that reaches the int null. Fixed seed, so a failure prints the body and
  reproduces. See
  ["Generated differential testing"](#generated-differential-testing). What stays in Rust is only what has to look at the host — the
  two wall-clock bounds that catch the JIT silently compiling nothing at all, the `FnCode` flag that
  says a function really was compiled, and the deopt cases that need two separate VMs so one provably
  never tiers up. `tests/bench.nt` is the ratio measurement, run by hand.
- RFC vectors for the crypto and the TLS key schedule (`tests/crypto.nt`); the record layer
  round-trips offline.

What this gives up relative to the oracle: a bug that the compiler introduces *and* reproduces
consistently is no longer caught by construction — it is caught only if a language case exercises it.

#### What the null-sentinel checks cost, measured

Every `+ - *` on two ints emits `cmp` against the null sentinel and a branch, because two ordinary
ints can wrap to exactly it and the interpreter would then propagate a null. In the inner loop of a
schoolbook multiply that is four checks of about thirty instructions. Two ways to make them cheaper
were built and measured against the same loop:

- **One branch per basic block instead of one per op.** Each check increments x4 with `cinc` (no
  branch, no flags left behind) and a single `cbnz` at the end of the straight-line stretch acts on
  the lot, flushed before every branch, call and return so a poisoned value cannot escape the block
  it was made in. Correct, and **12% slower**: the accumulator is a loop-carried dependency where
  the branches were free, being never taken and perfectly predicted. Reverted.
- **Removing them entirely** (unsound — measured only as a ceiling): the multiply-accumulate loop
  goes 0.96ns → 0.80ns per limb product and an RSA-2048 verification 0.50ms → 0.46ms. So perfect
  elimination is worth 17% of the inner loop and **8% of the verification**, which is the budget any
  range analysis on this has to fit inside. It is not where the remaining distance to OpenSSL is.


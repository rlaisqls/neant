# Implementation plan

The front door is [../README.md](../README.md). This is the order things get built in, what each
step has to prove before the next one starts, and what would stop the project. Dates are not in
it: the ones an earlier version carried were extrapolated from M0–M3, a subset cut to be
tractable, onto stages that are each team-scale problems, and that was not honest.

The governing rule: **a claim the compiler makes is a scientific claim before it is a feature.**
The cost model says a static number predicts what the hardware does; the signature says a
function's cost can be known without its body; the language says being a language buys scope
that a DSL cannot. Each is falsifiable, each stage below carries the test that would falsify it,
and a failed test stops the stage rather than being explained away. M1 was run this way and its
first rule set failed ([experiments.md](experiments.md)).

## Decisions taken up front

**Rust first, self-hosted later, two seeds forever.** The compiler in `bootstrap/` is Rust,
builds with `cargo build` from a clean checkout, and is never deleted: once a compiler in neant
exists it becomes seed one and stops growing, and the compiler's own C output, committed as
`bootstrap/neant.c`, is seed two. The fixpoint from both roads is a CI test. Self-hosting is a
goal the author holds, not a proof of anything: the compiler is the program this language is worst
at, and it comes after M4 (decisions §4).

**Emit C, do not write a backend.** Cost inference is static and needs no backend; validating it
needs the program to run on real hardware, and `clang`/`gcc -O2` on generated C is that. Layout,
fusion, tiling and loop order are decided before the C is written, so the cost model owns what it
cares about. An own backend is a stage M7 question, as a search space rather than a hand-written
optimiser (§ M7).

**Costs are exact where exactness is finite, and loud where it is not** (decisions §3). No hull
where a sum is available, no trusted measure that can be checked, no lid on regimes that folds
silently.

## Done: M0–M3, the analyser

**M0** — lexer, parser, checker, typed IR with size variables, C emitter; a dozen programs run.
**M1** — `work` and `moves` for the exact tier, `costs.lock`, and the experiment: six kernels,
size sweeps, predicted bytes against `l2d_cache_refill × 64` on one pinned core. Passed on the
second rule set — the data forced two rules, that access sites compete for the cache and that
fitting is strictly less than `M`. **M2** — chains and comprehensions desugared to single loops;
the Hong–Kung bound for contractions, the gap, and `--apply tile|transpose` as costed rewrites:
tiling removed 53× of measured traffic against 39× predicted and beat the hand-written tile by
1.75×. **M3** — `while` with inferred or declared measures, `break`, `u8`, solved self-recursion,
`io` inferred, `#[cost]` by asymptotic dominance, `neant measure`, and a four-program corpus.
Then the exactness pass: verified measures, reaching definitions, loop sums by Faulhaber, exact
recurrence constants, and piecewise costs with `if` as max and fit tests that fork. After the
literature review (decisions §5, §6): lower bounds derived by the compiler — the HBL exponent by
an exact LP, the footprint from cold — with IOLB asked for the constant through `emit --scop`
(untiled, delinearised) and `cost --iolb`; the tile side read off the model's own cost of the
tiled program with the side symbolic, then corrected by measurement.

What M0–M3 established is an analyser. Everything in it could have been built over a Rust subset
or as an MLIR dialect. The coverage it reached on ordinary code is the number that governs what
follows: **6 of 11 functions exact in the M3 corpus, 2 of them only by a declared measure.**

## Stage A — a composable arrow

**Claim.** A function's cost can be written in its signature so that a caller never re-analyses
the body. Today it cannot: rules two and three of the calculus (call-site specialisation,
inherited loops) re-analyse the callee at every call, because a scalar polynomial cannot say what
the callee leaves in the cache.

**Do.** The cost object becomes four things: `work`; **footprint** — the set of (root, byte
range) the function touches, expressed in its parameters (the view roots and ranges from the
exactness pass are already this data structure); **moves** from a cold cache; and **residue** —
what is resident when it returns, bounded by `min(footprint, M)`, piecewise like the rest.
Composition is one rule: for `f` then `g`, `g`'s moves are reduced by `f.residue ∩ g.footprint`.
A loop is its body composed with itself, so the fit test and the two slide rules become instances
of the rule instead of separate machinery. Regions in M4 become instances too: a region's size is
its footprint, and "fits a cache level → loaded once whatever the access order" is the residue
rule.

**Exit.** Rules two and three deleted. Every `.cost` golden reproduced from signatures alone —
in particular, a call scanning an array twenty times inside a loop costs one scan when the array
fits, because the residue of iteration `k` covers the footprint of iteration `k+1`. The M1 and M2
numbers reproduced as a by-product.

**Kill.** If the goldens cannot be reproduced from signatures, moves is not a composable quantity.
Then "cost lives in the signature" is withdrawn, the README is rewritten around a whole-program
analysis tool, and M4 is reconsidered from there.

**Status: passed, 2026-09-22.** Rules two and three deleted; the signature carries footprint and
residue; `main` calling `matmul(1984, …)` costs the same bytes from the signature as the
re-analysis did; twenty scans of a fitting array cost one; kernel predictions unchanged
([experiments.md](experiments.md), Stage A).

## Stage B — declarations first

**Claim.** A caller can be checked against a callee's declaration alone.

**Do.** Three changes to `#[cost]`. Budgets: coefficients count, checked as concrete numbers under
given `M`, `B` and declared size bounds (`moves_at_most = "4096"` with `n ≤ 512`), beside the
asymptotic check that exists. Ownership: a declaration is the lockfile line, and the inferred cost
is what gets checked against it — today it is the other way round. Modularity: a caller reads the
callee's declaration and nothing else, which stage A makes possible. `dyn` inverts with it — an
interface carries a budget and implementations are checked against it, instead of the call being
charged the maximum over implementations.

**Exit.** Delete a callee's body, keep its declaration: the caller's check still passes. One test,
and it captures what makes separate compilation, binary distribution and interface budgets
possible.

**Status: passed, 2026-09-22.** `sizes` budgets in real units; the declaration is the lockfile
line; a declared callee is composed through its declaration only; `extern fn` is a bodiless
declaration naming the C symbol. `tests/golden/decl.nt` and `decl_extern.nt` are the exit test:
the same caller, the same declaration, one callee with a body and one without, both check. `dyn`
does not exist in the language yet, so its inversion waits for it.

## Stage C — the boundary

**Claim.** Scope — the reason this is a language — survives the call into C, as a number rather
than a hole.

**Do.** `extern fn` carries a declared cost and effects. `neant measure` confirms the declaration
on the machine and records the range it was measured on. Every line in `costs.lock` names what it
rests on: the machine model, an external declaration, a person's assumption (`decreasing`, a
`#[cost]` on an `extern`). The tier column becomes an auditable chain.

**Exit.** A program that calls a C library keeps every non-boundary function exact, and the
lockfile shows what each line rests on. Then the M3 corpus tier count is taken again — and the
same for a corpus chosen from the domain where these constraints are assets (control loops,
kernels, real-time code) — and how far it moved is the measured size of the language's reason to
exist.

**Status: passed on its own terms, 2026-09-22.** `extern fn` carries a declared cost and effects;
`neant measure --fn labs` confirms a declaration per call against a baseline driver without the
call, within the counter's known factors, and `--lock` records `measured over n = …: confirmed`,
which `neant lock` preserves; every line names what it rests on, transitively (`main rests on total
(declared, checked); labs (declared, extern)`). The re-count is honest and unchanged: the M3 corpus
calls no C, so it is still 6 of 11, and the domain corpus that would move the number does not exist
yet — that is the first thing M4's work should be measured on.

## Done: M4 — structs, layout, arenas, owned returns

Built, in this order (the design ordered moves before SoA; the order here puts the gate first, so
the kill condition is tested as early as possible): struct types with scalar fields and struct
values by copy; field reads and writes on values and on elements; AoS emission; cost sites that
record what they touch and how far they step, and that merge when they touch one address; SoA
emission behind `#[layout(soa)]`; the layout chosen by the compiler from the module's own moves,
reported and locked; the region rule for arenas; owned array returns whose size the signature
carries. Goldens: `structs`, `arena`, `owned`, and the corpus `particles`, `stencil`, `tree`,
`ring`.

**The gate held.** `sum_x` over an array of three-field structs, built under both layouts and
measured past L2: predicted `24·n` against `8·n`, measured 3.7–4.0× (docs/experiments.md). The
region rule was measured too: where the arena does not fit, a chase over an LCG permutation pays
what the model says within 1.2–1.4×; where it fits, the model is conservative by the number of
walks, because a site whose index is loaded from memory claims no residue, and that is stated.

**Deferred, with the reason.** Real moves (`let ys = xs`, arrays by value) were M5 (done there —
see M5 below); at M4's close an array could not be bound to another name at all, which was
*stricter* than a move checker, not unsound, and nothing in the M4 corpus needed to hand an array
over except by returning it. Per-array layout needs monomorphisation (design §9). Owned returns of
struct arrays are refused, since a SoA return is a
tuple of pointers.

**Coverage.** M4's corpus is 9 of 9 exact and the M3 corpus is unchanged at 6 of 11, so 15 of 20
together — with the caveat the README carries: those nine were written after the rules that cost
them. The two holes are untouched: a worklist whose trip count is not a size expression, and
mutual recursion with a measure that reads memory.

## M4, narrowed: no element/field views, arenas instead of region inference

docs/m4-design.md §1 and §6 cut this from the plan above and said why. A view of one element
(`&ps[i]`, `&p.field`) would need a fat representation and nothing in the exit tests needs it; the
projections that exist (`v.f`, `xs[i].f`) already return values, never addresses, which is the
mechanism claim. And there is nothing to infer about where a pointer-linked structure lives: a
list or a tree is a struct array — an **arena** — whose links are indices the programmer already
wrote, so Tofte–Talpin has no unknown to find. What M4 built instead is the **region rule**: a
non-affine site's root has a footprint, and once an arena that fits `M` has been fully touched
nothing more is fetched, whatever the order — the same residue rule the rest of the calculus uses,
applied inside one loop. Typed arena indices (`Idx<Node>`, an `i64` that can only index one arena)
are a later convenience, not inference.

**Exit, as delivered.** `sum_list` and a tree walk over an arena get the two-piece bound (the
arena's footprint where it fits, `n` lines where it does not), measured within 1.2–1.4× on both
sides of `M`; `sum_x` under AoS vs SoA measured 3.7–4.0× against a predicted 3×. **This was the
project's first real gate** — the point at which being a language, rather than an analyser, bought
something — and it held.

## M5 — span and in-place reuse

`span` joins the cost; `T ≤ W/P + O(S)` becomes a statement about a parallel loop. Uniqueness in
the Perceus style: `ys = xs; ys[3] = 9` is written one way and the cost line says `1` or `n`.

**Real moves, done.** `let ys = xs;` moves an array local: the source is dead after, a walk over
the structured control flow rejects a further use with the line of the move (`xs[0]` after, a
branch that only moves it on one side and then uses it outside, the goldens `moves`,
`err_move_use`, `err_move_if`), and a move inside a loop of a local born outside it is rejected
outright — not deferred to a per-iteration check, since the same statement would move it again on
the next lap (`err_move_loop`). A local born inside the loop body may be moved freely lap to lap,
since lexically it is a fresh binding each time. What m4-design.md §2 also named — passing an
array by value (`f(xs)`, a parameter of type `[T]`) — is not done: parameters are still views
only, so the only move source is a `let`. Next: uniqueness, which needs this to have a subject —
designed in [m5-design.md](m5-design.md).

**Uniqueness and in-place reuse, done.** `ys = xs;` — an existing `let mut` array reassigned from
a plain variable of the same array type and size — moves `xs` (dead after, either way) and decides
once, from the checked body, whether `ys` takes `xs`'s buffer (`moves 0`) or a fresh copy is
written into `ys`'s own (`moves ys.len()·sizeof(elem)`): a backward scan finds the latest line, if
any, at which a view rooted at `xs` is still read, and that alone forces the copy — not a piece,
not a regime, a fact about the text (m5-design.md §2). The report names the line that forced it
(`` `ys = xs` (line 5) copies: a view of `xs` is still read at line 8 ``) or says it reused in
place. Inside a loop the copy is forced unconditionally when either name predates it, honestly,
not solved (goldens `reassign_inplace`, `reassign_copy`, `reassign_loop`). The gap m5-design.md §4
found in part 1 is closed the same way: `let ys = xs;` now rejects a move that leaves a view of the
source alive (`err_move_view`) — `restrict` and the call-argument disjointness check depended on
that being impossible, and until this it was not checked.

**Measured, exit tests 1–2 passed** (docs/experiments.md, M5 — `tests/kernels/reassign_inplace.nt.in`,
`reassign_copy.nt.in`, `perf` past L2 on the pinned core): in place adds nothing to the measured
traffic beyond building and summing the pair; forced to copy, the machine pays for it, and the
copy's own marginal cost — the copy kernel's bytes minus the in-place kernel's, isolating the
reassignment from what both share — scales with `n` at slope 1.00 against `8·n`, at the model's
usual ~3× counter gap (M1 §3's read-stream pairing, plus a fresh-`malloc`-every-repeat page-fault
effect `sum`'s own sweep does not have). A rough edge found doing this, not part of the feature and
**fixed the same day**: `[e; n]` allocated a fresh size atom per use, so two arrays built from
literally the same local `n` did not type-match for `ys = xs` — real code naming a size once and
building several arrays from it would have hit this immediately. An immutable local used as a
count is now given its size atom once and reused at every `[e; n]`; a mutable one still gets a
fresh atom each time, since it may have changed between them; `[e; xs.len()]` takes `xs`'s own size
rather than minting one that merely agrees with it. Goldens `reassign_named_size`,
`reassign_len_size`, `err_reassign_mutable_size`; the measurement kernels rewritten to the natural
`let n = @N@;` form, same predictions and outputs as before.

**Span, structurally built**, designed in [m5-span-design.md](m5-span-design.md). `.par()` on a
chain whose terminal combines associatively — `sum`, `count`, `any`, `all` (`max`/`min` deferred:
their "first element seen wins" has no OpenMP-native identity, refused rather than approximated;
`fold` refused, its closure not known associative); `.par()` must come first, right after the
source, since fusion's existing closure-purity check (`in_closure` forbids assignment) is already
the soundness argument parallel execution needs. `span: Cost` mirrors `work` through every site
that composes it (sequential add, `if`'s max, a call's substitution) except a `.par()` loop's own
leaving, which is one iteration's cost plus `O(log n)` for the reduction tree instead of `Σ_v`; not
threaded through self-recursion, which falls back to `work` (a safe over-approximation). `P` joins
`M`/`B` as a machine parameter (`-P`, defaults to `available_parallelism()`). Emission: `#pragma omp
parallel for reduction(op:acc)`, the loop's own end bound hoisted out of the init-clause first
(OpenMP's canonical form allows one declarator, unlike the sequential `for`'s `i = 0, end = e`);
`-fopenmp` added only to a build whose C actually contains `#pragma omp` — confirmed by `ldd`, a
program with no `.par()` links no `libgomp`. The report gets a `span ... T ≤ work/P + O(span)` line
only when span differs from work, so every existing golden is unchanged (exit test 1) and the
structural shape (`O(log n)`, composed correctly across a call) is checked directly in the report
(exit test 2, goldens `par`, `par_span`, `err_par_max`, `err_par_fold`, `err_par_order`).

**Measured, exit tests 3–4 decided against the bound as it stands** (docs/experiments.md M5,
decisions §7; `tests/kernels/par_compute.nt.in`, `par_memory.nt.in`, `par_sweep.py`, wall-clock on
this machine's ten-core big cluster — all Cortex-X925, checked directly via `midr_el1`, not a
big.LITTLE mix as an earlier pass of this measurement wrongly assumed). A compute-bound `.par()`
chain's efficiency stays above a memory-bound one's (`.par().sum()`) at every `P` past 2 — 86% vs
74% at `P = 4`, 69% vs 53% at `P = 8`, 46% vs 38% at `P = 10` — and the model predicts the identical
curve for both, having nothing in it that could tell them apart. **`T ≤ work/P + O(span)` needs a
second, `moves/BW` term taken as a `max` with the first, a roofline** — decided, not yet built: `BW`
itself was not fit, and part of the `P = 6..10` range ran while another session shared the machine,
noisier than a quiet rerun would be. Next: fit `BW` on a quiet machine, or move on and record the
bound as qualified rather than exact.

## Self-hosting

After M4 (M5 done too now), as a goal. The compiler in neant, the Rust compiler frozen as seed one,
`bootstrap/neant.c` as seed two, the two-seed fixpoint in CI, and `compiler/costs.lock` as the
compiler's own stated complexity. Not the coverage proof; the corpus for that is chosen in stage C.

**Started, designed in [self-hosting-design.md](self-hosting-design.md).** The apparent conflict
with "Not scheduled: strings and I/O beyond the harness" resolves narrower than it looks: every
stage (lex, parse, types, ir, emit) is expressible with what M0–M5 already built — byte arrays as
"strings," pre-sized arrays with a tracked count instead of growth (every pass has a known upper
bound on what it produces), and M4's arena pattern (a flat array of scalar-field structs, children
by index) for the tree, since structs still cannot nest or hold arrays. The one real gap is
`extern fn` file I/O: an extern cannot return an owned array (the size-inference that lets a
function-with-a-body do it never runs for one without), and `neant build`/`run` link only the one
generated `.c` file. Fix, not a language feature: a small hand-written `bootstrap/rt.c` whose one
function matches this language's own pointer+length calling convention exactly (`read_file(path:
&[u8], buf: &mut [u8]) -> i64`, the caller pre-allocating `buf` and reading the real length off the
return value), linked in by a fixed convention rather than a new flag. **First milestone done:
`compiler/lex.nt`, the lexer alone**, exit test passed on every one of the 69 `tests/golden` files,
not a sample — `neant lexdump` (a debug command added for this) against `bootstrap/src/lex.rs`'s
own tokenisation, compared kind by kind (`bootstrap/tests/self_host_lex.rs`). `neant cost` on `lex`
itself — the first data point for what the compiler costs, by its own tool — not yet taken.

**Second milestone done: `compiler/parse.nt`**, designed in
[self-hosting-parser-design.md](self-hosting-parser-design.md) — one arena of uniform scalar-field
`Node`s, children chained by a `next` index (the design's flat child-list arena did not survive
contact: a nested block's statements interleave with the outer block's), each precedence level its
own inlined function since there are no function values to parameterise one with. Exit test passed:
of the 69 golden files, the 13 inside the first slice's grammar all produce an identical
depth-first node-kind sequence to `bootstrap/src/parse.rs` (`neant parsedump`,
`bootstrap/tests/self_host_parse.rs`), and the negative goldens are required to be rejected by
both. Two things it found: `parse.rs` checks "block tail" before "`if` needs no `;`" and the order
matters; and the C emitter put user functions and its own runtime helpers in one namespace, so a
neant function named `alloc` collided with `nt_alloc` — user functions are now `ntu_`-prefixed.
**Third milestone done: `compiler/check.nt`**, designed in
[self-hosting-checker-design.md](self-hosting-checker-design.md) — the first slice of `types.rs`
(964 lines): name resolution and type checking, no size variables, no chain desugaring, no
moves/layout, each left out because it feeds a stage that does not exist self-hosted yet. A scope
is the symbol table's saved length; names compare by the source bytes their tokens span, since
`lex.nt` kept spans instead of copying identifiers. Exit test passed in two halves: verdict parity
with `neant check` on every in-slice golden — including the six negative ones, each firing a
different rule — plus twenty hand-written probes, each a broken form and its fix, whose verdicts
must flip (they include the `&mut [T]`/`&[T]` argument direction both ways); and `fib.nt`'s
per-expression type codes pinned after hand-checking each one against the source. Building it
produced the start of a typed IR ahead of schedule: the exit test needed each node's type, so the
checker records one. **Fourth milestone done, and the chain is end-to-end: `compiler/emit.nt`**, designed in
[self-hosting-emitter-design.md](self-hosting-emitter-design.md). A neant program now reads a
`.nt` file, lexes, parses, type-checks and writes C, and `cc` compiles it: for the seven golden
programs inside the slice, **its output equals `neant run`'s byte for byte**
(`bootstrap/tests/self_host_emit.rs`) — the first test here that compares behaviour rather than a
representation. `bootstrap/rt.c` gained `write_file` for it, whose first signature dropped the
length argument because a neant view passes as two C arguments — the same convention mismatch that
motivated writing the shim in the first place. Locals are emitted by their source spelling, so
same-block shadowing fails in `cc` rather than silently; fixing it is the next thing the checker
should record (design §3).

**The slice now covers structs, and the front end reads its own source**, written up in
[self-hosting-structs.md](self-hosting-structs.md). Every stage stopped at its first `struct` — the
arena pattern means every stage opens with one — so structs went in across parser, checker and
emitter at once: a pre-scan laying a marker per `struct <Ident>` name (the one-pass parser's answer
to telling `S { … }` from a block), type kind 7 over a flat struct/field table, and C compound
literals with designated initialisers. `tests/golden/structval.nt` goes through the whole chain and
prints what `neant run` prints. Two findings worth the name: the self-hosted lexer had, since the
day it was written, read past an **escaped quote** — `b'\''`, which `compiler/lex.nt` itself
contains and no golden program does, so the lexer's corpus is now the goldens *and* `compiler/*.nt`;
and `P { x: 1, x: 2 }` type-checked, because "every initialiser names a field, and the count
matches" forbids neither a repeat nor the omission that goes with it.

**Arrays and slices went in next, and the compiler now compiles itself**, designed in
[self-hosting-arrays-design.md](self-hosting-arrays-design.md). The measurement that opened it was
unusually precise: `emit.nt` needed exactly `.len()` (once) and `b"…"` bound by `let` (79 times).
What that pulled in was the array representation itself — every array or view is two C variables,
`x_p` and `x_n`, which is not a choice because `rt.c` already works that way — plus `nt_alloc`,
`nt_idx` and the `&x`-is-two-arguments convention. Two tests now pin the fixpoint from both sides:
`the_self_hosted_front_end_checks_its_own_source` parses and type-checks all four stages (~2000
lines), and `the_self_hosted_emitter_emits_its_own_source` runs the whole chain over them, writes
160 KB of C, and `cc` compiles it — 92 functions, no diagnostic. End-to-end behavioural comparison
went from 7 golden programs to 18.

`extern fn` and brace-list array literals closed the last gap, so a driver is in the slice too and
the stages are a program. **The fixpoint is reached**, written up in
[self-hosting-fixpoint.md](self-hosting-fixpoint.md): stage1 (the stages plus a driver, interpreted
by the Rust compiler) compiles its own source to `stage2.c`; `cc` builds that with `rt.c`; and
stage2 compiles the same source to **byte-identical C**, in 42 ms where the interpreted pass takes
about ninety seconds. `the_self_hosted_compiler_reaches_its_fixpoint` is the test, and it is the
only one here with no Rust compiler in the comparison. 36 golden programs also go through the whole
self-hosted chain with output identical to `neant run`.

What the fixpoint is *of* matters as much: a front end and a C backend, not the compiler the plan
describes. **The cost calculus's first slice is now self-hosted too** —
[self-hosting-cost-design.md](self-hosting-cost-design.md), `compiler/poly.nt` and
`compiler/cost.nt`: `work` as a polynomial over size atoms with rational coefficients, walked over
the parse tree (the work table in `analyze.rs` is per-node and local, so no IR is needed), loops
summed by Faulhaber, `if` by dominance, `while` by the two ways `analyze.rs` finds a trip
count — the programmer's `decreasing` measure, whose promise is checked rather than taken, or an
induction variable the body steps by a constant exactly once — and self-recursion with one call per
invocation, by finding a measure that shrinks and then unrolling it or, when it halves, taking a
logarithm. **71 golden functions' `work` columns come out string for
string identical to `neant cost`'s**; exactly one it declines — `msum`, the only recursion with two
calls per invocation, which wants the master theorem — and 4 differ *on purpose*, because `ys = xs` costs 1 when
in place and the array's length when copied — the self-hosted emitter has no uniqueness proof and
always copies, so its cost says so. Charging 1 for parity would have been a cost report for code
this compiler does not emit. **`moves`'s first slice landed too**: 51 columns exact for functions that call nothing, by the rule
`t·s + B` for a contiguous site and `t·B` for a strided one, with 7 differing because the
self-hosted emitter always lays an array of structs out as AoS while the Rust compiler chooses —
the same principle as the `ys = xs` divergences, arrived at independently, and predicted in the
design before a line was written. The cost pass is now **inside the fixpoint**: `self_host_cost.rs` compiles the reporter with the
committed seed instead of interpreting it, which is 17× faster (67 s → 3.9 s) and proves the cost
calculus survives being compiled by the compiler it is part of. Pointed at `compiler/*.nt` itself
it reports 31 functions' work and 28 functions' moves in 20 ms — and produced the project's first
**cost-model finding about the model**, measured and written up in experiments.md: `emit_bytes`
was charged a cache line per byte because the one-element-array idiom hides the cursor, the
rewrite the model advised cut the predicted traffic 32×, and the machine measured 0.65% fewer L2
refills and no wall-clock change. The estimate improved; the program did not. **Regimes arrived with it**: a cost may fork once, so a scattered walk costs the array's footprint
when it fits in `M` and a line per touch when it does not, and all five single-condition piecewise
goldens come out exact — the language's distinctive feature, in the self-hosted compiler, for the
first time. Multi-condition regimes, recurrences, span are still out — nor are M5's moves and uniqueness (the emitter takes the safe branch and copies),
M4's layout, chains, closures or comprehensions. The cost pass is tested and the native
self-hosted compiler compiles it, but it is not in `bootstrap/neant.c`: `compiler/main.nt` is a
filter and there is no argv to ask for a cost report with. **Seed two is checked in**: `bootstrap/neant.c`, 182,390 bytes, generated by `compiler/build.sh` from
`compiler/*.nt` and compared against the live compiler by the fixpoint test, so it cannot silently
stop matching its source. It took `compiler/main.nt`, a committed driver that is a filter
(`./neant-self < x.nt > x.c`) because the language has no argv, plus `read_stdin`/`write_stdout`
and a one-line `quit` shim in `rt.c` — libc's `exit` cannot be named by an `extern fn`, since
neant's `i64` is `int64_t` where libc's parameter is `int`. Also found along the way: **size atoms are not only the cost model's** — whole-array
reassignment cannot be type-checked without them, which the checker design had said the opposite
of.

## M7 — the constant factor

Prove the asymptote, search the constant. A micro-architectural cost line (what llvm-mca and uiCA
compute for a block, as default output, applicable because the type system knows what may be
reassociated); schedules separate from algorithms; search over schedules pruned by the cost model
and decided by measurement, persisted in `costs.lock`. An own backend is justified here and only
here — as the search space LLVM does not expose, not as better heuristics.

## Not scheduled

Zero-copy persistence (unrelated to cost; out of the argument); generics beyond what the stages
need; strings and I/O beyond the harness; compile-time performance of the compiler.

An option, not scheduled: an export of a rectangular fully-permutable nest to IOUB's DSL (loop
dimensions, access functions, reuse directions, cache sizes) for the multi-level-cache tile
recommendation. The single-level side is now read off the model itself (cost-model § Rewrites);
the affine analysis already has every field the DSL asks for.

## Who switches, and why

Nobody has to, and the plan does not assume anyone will. This is a general-purpose language judged
on its own merits, chosen knowingly over the two positions with an adoption story: a kernel DSL
that inherits its host's ecosystem, and the safety-critical / real-time niche where dynamic
allocation bans, restricted subsets and strange toolchains are what certification already demands
and worst-case tools are disliked (decisions §4). The reason a language and not an embedded IR is
scope; the thing that attacks scope is the C boundary; stage C turns that into a number. If, after
stage C, the exact tier does not move on ordinary code, the honest positions are the two above, and
the choice is reopened with the number in hand.

## Layout of the repository

```
bootstrap/              everything that builds the compiler from nothing
  Cargo.toml, src/      the Rust compiler. Seed one. Frozen after self-hosting, never deleted.
    lex.rs  parse.rs  ast.rs  types.rs  ir.rs  emit_c.rs  main.rs
    cost/
      size.rs           symbolic sizes and costs: rational polynomials over atoms, B and M
      piece.rs          piecewise costs: conditions, max, feasibility
      analyze.rs        the calculus, one walk (docs/cost-model.md is its spec)
      bounds.rs         native lower bounds: the HBL exponent by an exact LP, the footprint from cold
      scop.rs           export of an affine function as a SCoP for IOLB, indices delinearised, tiles undone
      iolb.rs           runs IOLB, parses its bound into bytes over M
      rewrite.rs        tile and transpose
      assert.rs         #[cost] parsing and dominance
      measure.rs        the measured tier
      lock.rs           costs.lock and the report
  neant.c               compiler/ compiled by itself. Seed two. (self-hosting)
compiler/               the compiler in neant. Empty until self-hosting.
tests/
  golden/               .nt programs with expected output (.out, .exit), rejection (.err), cost report (.cost)
  kernels/              the M1/M2 experiments: kernel templates and sweep.py, the perf harness; iolb.sh runs IOLB in docker
docs/
  plan.md               this file
  cost-model.md         the calculus, as implemented
  experiments.md        what was measured against what prediction, and what it changed
  decisions.md          decisions with the reasoning that produced them
```

## Validation harness notes

`perf stat -e l2d_cache_refill`, pinned to one core with `taskset`, minimum of several runs per
size. The development machine is heterogeneous (Cortex-X925 on CPUs 5–9 and 15–19, L2 2 MiB
private, L3 16 MiB; Cortex-A725 elsewhere); pinning to a big core is not optional, and the harness
records which core it ran on. Sizes step by factors of two from below L1 to well past L3 and avoid
powers of two; the slope is fitted on the region past the cache where the I/O model is meant to
hold. `kernel.perf_event_paranoid` must be 2 or lower.

## Risks and what is done about them

| risk | mitigation |
|---|---|
| moves is not composable (stage A fails) | The kill condition: withdraw the signature claim, reposition as whole-program analysis |
| the boundary swallows the program (most lines rest on `extern` declarations) | Stage C measures it; if the exact share does not move, the positioning is reopened (§ Who switches) |
| the moves model does not predict hardware | M1's experiment and kill criterion, passed; re-run at every rule change |
| symbolic sizes or regimes explode | Exact by default, feasibility pruning, and an explosion is reported, not folded (decisions §3) |
| region inference and cost-driven layout are research problems | M4 built the arena region rule and the layout choice on stage A instead of inference; the explicit layout attribute is the fallback that always works |
| self-hosting recreates the bootstrap trap | Two seeds, both in CI; the Rust compiler is frozen, not deleted; seed two is C, not an image |
| the generated-C path cannot express a layout the model wants | Discovered in M4, where the backend decision is revisited with evidence |

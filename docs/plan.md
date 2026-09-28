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
logarithm. **72 golden functions' `work` columns come out string for
string identical to `neant cost`'s, and it declines none of them** — the last was `msum`, whose two
calls halve the measure, which is the master theorem rather than `fib`'s exponential; 4 differ
*on purpose*, because `ys = xs` costs 1 when
in place and the array's length when copied — the self-hosted emitter has no uniqueness proof and
always copies, so its cost says so. Charging 1 for parity would have been a cost report for code
this compiler does not emit. **`moves`'s first slice landed too**: 64 columns exact for functions that call nothing, by the rule
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
refills and no wall-clock change. The estimate improved; the program did not. The **layout choice** is designed in
[self-hosting-layout-design.md](self-hosting-layout-design.md) and is the first piece of
self-hosting work that changes what the compiler *emits* rather than what it computes: the Rust
compiler picks AoS or SoA per struct from the cost model and the self-hosted emitter always emits
AoS, which is nine of the thirteen recorded differences. The arrays design chose that deliberately,
because the layout decision belongs to a cost calculus that was not self-hosted — a premise that
has now expired. **Done**: the self-hosted compiler now chooses AoS or SoA per struct as the Rust one does, emits
the layout it chose, and the five golden programs with struct arrays print exactly what `neant run`
prints. The cost pass is part of the compiler now rather than only of the reporter, so the code it
emits and the cost it reports describe the same program; the seed doubled to 401 KB and the
fixpoint still holds.

**Regimes arrived with it**: a cost may fork once, so a scattered walk costs the array's footprint
when it fits in `M` and a line per touch when it does not, and all five single-condition piecewise
goldens come out exact — the language's distinctive feature, in the self-hosted compiler, for the
first time. **Multi-condition regimes and nested loops landed after it**, and `moves` is now
**72 columns exact with none declined**. A cost became a list of pieces rather than one fork, and then the
sites stopped being settled where they stand: a level's working set is the sum over every site
inside it — they compete for the same cache — so it cannot be computed one site at a time, which is
what had held this to one site per loop and what made two earlier attempts at nested loops fail.
`settle_moves` walks the levels innermost outwards over a table of alternatives, and `matmul`,
`stencil` and `tri`'s `pairs` come out regime for regime, including the order the report lists them
in. **`moves` then finished**: a footprint became a **range** `[lo, hi)` rather than the whole array
or nothing, which is what a callee touching two fields of a four-field particle needs, and the last
three declines went with it. **72 columns exact and nothing declined** — every column in the corpus
is reproduced or differs for the one recorded reason, that this emitter copies a whole-array
assignment the Rust proves is in place. Five of the six things that then had to be right were invisible until a column carried two
conditions — among them that a condition is carried in **lines** and not bytes, because `dominates`
matches monomials exponent by exponent and that is not invariant under scaling both sides by `B`,
and that `prune_at` reads conditions as **intervals on a size variable**, each a number found by
bisection, which is strictly stronger than comparing them symbolically. **That paragraph is now out of date in every clause, and the corrected version is the milestone:
the whole golden corpus is inside the self-hosted slice.** Every golden parses, checks, emits and
costs, and four columns are reproduced string for string — **work 98, moves 98, the footprint lower
bound 109, the footprint 111, with no differences of any kind.** Recurrences, M5's moves and
uniqueness (the checker proves in-place now, so the emitter takes the buffer), M4's layout, chains,
closures, comprehensions, `#[cost]` declarations and assertions, and owned array returns are all in.
What is left is **span** — `.par()` is the one chain stage still refused — and the parts of the
*report* beyond those four columns: the HBL bound and the `gap` that depends on it, `rests on`
provenance, the `inferred` and `✗` lines, and the rewrite suggestions. The cost pass is tested and the native
self-hosted compiler compiles it, but it is not in `bootstrap/neant.c`: `compiler/main.nt` is a
filter and there is no argv to ask for a cost report with. **Seed two is checked in**: `bootstrap/neant.c`, 182,390 bytes, generated by `compiler/build.sh` from
`compiler/*.nt` and compared against the live compiler by the fixpoint test, so it cannot silently
stop matching its source. It took `compiler/main.nt`, a committed driver that is a filter
(`./neant-self < x.nt > x.c`) because the language has no argv, plus `read_stdin`/`write_stdout`
and a one-line `quit` shim in `rt.c` — libc's `exit` cannot be named by an `extern fn`, since
neant's `i64` is `int64_t` where libc's parameter is `int`. Also found along the way: **size atoms are not only the cost model's** — whole-array
reassignment cannot be type-checked without them, which the checker design had said the opposite
of.

## Stage D — ordinary code

**Why this comes before the rest of self-hosting.** Self-hosting is, by this document's own words,
not a proof of anything, and the parity work left — span, the report lines — proves nothing more.
What the project has to prove is the governing number: that the exact tier moves on ordinary code
(§ Who switches). And self-hosting produced, as a by-product, the largest ordinary program this
language has — one not written to suit the rules, since it was written to compile itself. Measured
2026-09-23, `neant cost` on `compiler/*.nt` concatenated as `build.sh` does: **81 of 276 functions
exact, 29%.** The 195 unknowns split in two:

| cause | functions |
|---|---|
| calls a callee whose cost is unknown | 95 |
| a loop bound is not a size expression | 55 |
| a `while` with no measure the compiler can find | 22 |
| the compared variable is not stepped by a constant exactly once | 16 |
| recursion with no shrinking argument, an entry value set in an earlier loop, `unbounded`, an `extern` | 7 |

So half the unknowns are contagion, not ignorance, and most of the rest are one shape: a trip count
read from memory. `tok_text_eq` walks `i < toks[a].len`, a length stored in a field — a size, but
not one the calculus can name, because sizes today are only array lengths and immutable locals.
These are the two holes M3 named, a worklist whose trip count is not a size expression and a
measure that reads memory, at the scale of a real program.

**Claim.** The exact tier grows on code that was not written for the rules, by extending what a
size is — not by asking the programmer for more measures.

**Do.**
1. **Unknowns stop propagating.** A call to a callee with no cost contributes a named term `c(f)`
   to the caller, in the callee's argument sizes where they are size expressions, and the rest of
   the caller's cost stays exact. The caller's tier becomes *exact modulo* the named callees, the
   report and the lockfile say which, and `rests on` carries them like an `extern` declaration. A
   caller of an unknown is then exactly as informative as a caller of a declared callee, which it
   already is.
2. **Sizes read from data.** A field of integer type read at a loop bound becomes a size atom of
   its own (`toks[a].len`), minted at the read, so a loop over it is exact in that atom. Where the
   value was written from an array length or a size expression the checker can see, the atom is
   that expression; otherwise it stays free and is reported as such.
3. **Amortised trip counts.** A worklist loop — pop until empty, pushes inside — is bounded by the
   total pushes, charged at the push sites (the potential method, as RAML does for OCaml). The
   total is a size expression when every push site is inside a loop that is one.
4. **The compiler's coverage is a tracked number.** `compiler/costs.lock` is committed and the
   exact count out of 276 is recorded here at every change to the calculus, beside the M3/M4
   corpus's 15 of 20.

**Exit.** Measured on the compiler, whose text is not changed to help: the contagion row goes to
zero by construction (1); the other rows fall by (2) and (3); and the exact share, counting *exact
modulo* separately, is reported. The M3 corpus is re-counted with it, and its two holes are either
closed or named as what (2) and (3) do not reach.

**Kill.** If, with (1)–(3), the compiler is still mostly unknown for reasons that are not the
rows above, the calculus does not reach ordinary code, and § Who switches is reopened with the
number in hand — the positioning question stage C was meant to answer and could not, for want of
a corpus.

**(1) done, 2026-09-23** (cost-model § An unknown callee is a term; golden `modulo`). An unknown
callee is `work[f](…)` / `moves[f](…)` in its caller, the caller's tier is `modulo`, and it rests
on `f (unknown)`. Measured on the compiler again, its text unchanged: **81 exact, 33 modulo, 162
unknown**, from 81 and 195. The contagion row did not go to zero, and the reason is the finding:
of the 95 functions that were unknown only by what they called, 33 are now `modulo` and the other
62 had **causes of their own that contagion was hiding** — the walk used to stop at the first
unknown call and never reached them. The unknowns now, every one for its own reason:

| cause | functions |
|---|---|
| a loop bound is not a size expression | 59 |
| a `while` with no measure the compiler can find | 30 |
| the compared variable is not stepped by a constant exactly once | 22 |
| calls a callee in its own cycle of calls (mutual recursion, left unknown on purpose) | 22 |
| an exact callee's cost depends on an argument that is not a size expression | 15 |
| recursion with no shrinking argument, an entry value set in an earlier loop | 10 |
| `unbounded`, an `extern` | 4 |

So the table this stage opened with undercounted what (2) and (3) have to reach: 111 functions
(the first three rows) are trip counts the calculus cannot name, not 93. Mutual recursion, which
M3 already named, is 22 more and is not in (1)–(3); the fifteen whose argument is not a size
could become `modulo` too, with the argument as `_`, but only by giving an exact callee the same
treatment as an unknown one, which throws away a cost the compiler has — worth doing only if (2)
does not make those arguments sizes first. Next is (2).

**(2) done, 2026-09-23** (cost-model § A size read from memory; golden `bfs`). An `i64` read
from an array where a size is needed is an atom of its own, `toks[a].len` or `offsets[u]`, the
same atom for the same text until the array is written, and no size inside a loop that writes it;
a read at an index that is not a size is `max(xs[_])` (or `min`, bounding from below) and makes
the line `bound`, an upper bound that says so. Two smaller changes rode along because the
compiler's loops needed them: `while a && b` takes its trip from the first conjunct that has one,
and the condition's memory is charged every iteration. Measured on the compiler, text unchanged:
**92 exact, 37 modulo, 28 bound, 119 unknown**, from 81, 33, 0 and 162. The unknowns now:

| cause | functions |
|---|---|
| the compared variable is not stepped by a constant exactly once | 29 |
| calls a callee in its own cycle of calls | 27 |
| a `while` bound is not a size expression | 19 |
| a `while` with no measure the compiler can find | 17 |
| an exact callee's cost depends on an argument that is not a size expression | 14 |
| recursion with no shrinking argument, an entry value set in an earlier loop | 9 |
| `unbounded`, an `extern` | 4 |

The three trip-count rows are 65, from 111; the `for` whose bound is not a size is gone as a row.
Two things this did not do. The atom is not yet followed back to the store that wrote it, which
the step's own wording asked for where the checker can see one — so `bfs`, one of M3's two holes,
is now a bound, `n·(max(offsets[_]) − min(offsets[_]))`, and not the `edges.len()` a
potential-method count of its pushes would give; that is (3). And the self-hosted pass has no read
atoms: it declines where it declined before, so the parity test lists `bfs` as a footprint it
states more narrowly (three arrays of four), which is the one new entry. And it costs compile
time: `neant check` on the compiler takes 5.5 s where it took 1.1 s, all of it in choosing layouts,
which costs the whole program once per struct and layout. Each read atom that sizes a window is a
fit condition of its own, so the regimes multiply — `pol_nth` has 16 — where parameters alone kept
them few; as first written, before a read that loses its element was made one atom and the
conditions were summed with the cost, it was 81 s and 64 regimes. Folding regimes that no machine
can tell apart is the fix, and is not done. Next is (3).

**(3) done, 2026-09-24, as a walk down a list rather than a worklist** (cost-model § A walk down
a list; golden `walk`). The step as written, a pop-until-empty loop bounded by its pushes, turned
out to reach nothing: the compiler has no such loop, and `bfs`, the one M3 named, pushes inside
the loop it bounds, so its total is its own trip count — it ends because of the `dist[v] < 0`
guard, which a count of pushes cannot see. What the compiler does have, 26 times among the
functions unknown for their own reason, is `while s >= 0 { …; s = nodes[s].next }` down a list
in an arena. Such a loop now runs at most `walk(nodes[_].next)`, the longest walk along the link,
when nothing in the body writes `next` — decided by a summary, per function and parameter, of the
fields it may store into, itself or through its callees. The line is `bound`: the atom is at most
`nodes.len()` only if no list is a cycle, which is `arena.nt`'s `decreasing` promise and not
something the compiler checks. Three things rode along. The same summary replaces "a call handed
the array writable" wherever a read asks whether its array was written, so a view passed to a
function that only reads it no longer makes every read of it stale. Dominance knows that an
element is at most its array's `max`, so regimes that contradict it are dropped. And an exact
callee whose footprint depends on an argument the caller cannot name makes the footprint inexact
instead of making the caller unknown.

Measured on the compiler, text unchanged: **92 exact, 47 modulo, 32 bound, 105 unknown**, from 92,
37, 28 and 119. The walk itself reached 24 of the 26; the other two, `check_block` and
`check_program`, call a checker that splices desugared statements into the list it is walking,
and are refused, rightly. The unknowns now:

| cause | functions |
|---|---|
| calls a callee in its own cycle of calls | 35 |
| a `while` with no measure the compiler can find | 20 |
| a `while` bound is not a size expression | 16 |
| recursion with no shrinking argument, recursive calls that depend on the input, an entry value set in an earlier loop | 13 |
| an exact callee's cost depends on an argument that is not a size expression | 12 |
| `unbounded`, an `extern` | 4 |
| the compared variable is not stepped by a constant exactly once | 3 |
| a walk down a list the body may write | 2 |

The "not stepped" row, 29 in (2), was the walk. As with (1), most of what it reached had a
second cause behind the first: mutual recursion is now the largest row (27 → 35), because walks
over statement lists are how the compiler's recursive descent is written, and the walk only moves
the verdict on to the recursion. That row and the recursion row, 48 functions together, are
M3's second hole, recursion with a measure that reads memory: a tree in an arena, walked by
recursing over each node's children. That is the potential method, a node count as the potential,
and it is what is left of this stage besides (4), the committed `compiler/costs.lock`.

It costs compile time again, and most of that was paid back. `written_roots` made more reads
survive, which let `pol_cmp_ord` and `piece_before` be costed for the first time, with 128 regimes
each, and `neant check` on the compiler went to 113 s. Three changes that leave every report byte
for byte the same brought it to 7.2 s, against 5.5 s before: adding a polynomial to every piece of
a cost no longer prunes it again, since a shift changes no piece's feasibility or dominance;
substitution accumulates in place and skips variables a polynomial does not mention; and layout
choice analyses, per struct and layout, only the functions that touch the struct, with their
callees as they are reached, instead of the whole program 39 times. Folding regimes that no
machine can tell apart is still not done.

**(4) done, 2026-09-24.** `compiler/costs.lock` is committed, and `self_host_lock.rs` fails when it
is not what `neant lock` writes today for the concatenation `build.sh` compiles — so a change to
the calculus or to the compiler's text that moves any of the 276 lines has to show up in review as
a change to that file. `compiler/lock.sh` regenerates it and prints the count, which is read from
the lockfile and nowhere else: **92 exact, 47 modulo, 32 bound, 105 unknown**. (The 46 recorded for
(3) was a miscount; the four did not add to 276.) Two things the lockfile had to learn first. It
kept the report's `(line N)` on every unknown, and in a concatenation of seven files every edit
moves the line of every unknown below it, a hundred lines of diff for none of substance; the
lockfile now leaves the line out. And `neant lock --check` keyed its diff by a line's first word,
so every `layout` line shared one key and a change of layout to any struct but the last went
unseen; they are keyed by struct now.

**What the lockfile showed as soon as it was read (2026-09-24).** Reading the compiler's
report line by line turned up costs no machine can have: the SoA alternative the layout choice
weighed for `Token` in `collect_structs` was `≈ −8·toks.len()·walk(nodes[_].next)²`, and
`sym_find` claimed its symbols resident under `−32·cst[1] < M`. Four faults, each also in every
program shaped like it, and each labelled `exact` or claimed as residue while wrong (golden
`descend`, which reproduces all four on the old code):

1. **Dominance ignored the larger side's negative terms.** `q ≥ p` budgeted `q`'s positive terms
   against `p`'s and dropped the rest, so `32·i − 32 ≥ 32·i` held — and a resident range from the
   last iteration was taken to contain this iteration's. The argument is now about `q − p`. Two
   rules keep what the old one got right by accident: `max(xs[_])` covers `min(xs[_])`, and a
   difference in one integer size is decided exactly (`8·(n − 2)² ≥ 0` in `stencil`). The region
   rule's "the walk is longer than the arena", which chooses between two sound bounds, is a growth
   comparison of its own, `dominates_eventually`, not an inequality.
2. **The second walk of a loop started from this iteration's residue**, not the last one's, so a
   call at `xs[i]` was credited for re-reading `xs[i]`. The residue is shifted back one step.
3. **A footprint that was a hull was exact**: two fields of one element under SoA became one range
   with the `8·n` bytes between them, and ranges with an end at `max`/`min` of an array were
   treated as read byte for byte. Only ranges that overlap or touch merge; a loose end is inexact.
4. **A loop counting down had its range inverted, and `<=`/`>=` lost their last lap.** The end a
   loop extends is the sign of coefficient times step, and an inclusive bound runs `+1`. The
   self-hosted pass had copied both rules and is fixed with them; seed two is rebuilt.

The count does not move — **92 exact, 47 modulo, 32 bound, 105 unknown** — and 22 lines of the
lockfile do: the `− 10·max(toks[_].len)` in every downward symbol search was the lost lap, and
`cand_measure` and `solve_recurrence` shed a credit larger than what they were charged. What is
left open: `asym_dominated` has `− 32·max(terms[_].n_fac)·min(terms[_].n_fac)·pols[inferred].n_term`
that none of its positive terms covers, which `asserts_hold` inherits as a negative leading term;
some range inside it is a `max − min` hull multiplied through, and it is not yet found. A test
that every cost the compiler prints is non-negative by `dominates` would have caught all of the
above, and is the next thing to add before the potential method.

**The sign audit (2026-09-26).** `NEANT_SIGNS=1 neant cost` lists every cost piece `dominates`
cannot show is non-negative, with the least value a few thousand samples find — every atom a
whole number, an array's least element at most its most and each element between them. Least
below zero is a cost that is wrong; at or above it, a prover too weak for a true inequality. On
the compiler it listed twelve functions, and the one found wrong was the `asym_dominated` term
left open above: **a rewrite that maps two atoms of a term to one kept the last exponent instead
of adding them**, so `pols[a].n_term · pols[b].n_term`, both losing their elements in a caller,
became `max(pols[_].n_term)` and not its square — a factor dropped from an upper bound. The same
overwrite was in renaming a callee's arrays (`f(xs, xs)`) and in substitution (`xs[j]` at
`j = i`); all four sites now add (golden `widen`, whose `pairs` was bounded by 81 on the old code
and costs 135). Five lockfile lines move, the count does not.

`widen` then found the same class in the self-hosted pass, through the parity test. Its
`pol_dominates` was the Rust's old rule, dropping `q`'s negative terms, and now argues about
`q − p` with the one-atom exact check beside it; the arena rule's "the walk is longer" is its own
growth comparison, `pol_grows_as`, as in the Rust. Its footprint merge still took the span of two
disjoint ranges, and takes their union only when they meet. And a walk that stopped at something
it could not cost stated what it had seen as exact and resident — `grid`'s `xs[a]` alone, having
stopped at `0..xs[a]` before `xs[b]` — and now states whole arrays with no residue; three
functions it stated exactly by luck (`twice`, `atom`, `pairs`) are listed as narrower for it.
Parity: work 99, moves 99, footprint 117, bound 123 (from 120, the new goldens' columns). The
compiler has two functions more, `pol_grows_as` (bound) and `univariate_nonneg` (unknown: its
loop runs to a bound in `f64`): **92 exact, 47 modulo, 33 bound, 106 unknown, of 278.** What the audit lists now:

| functions | least sampled | why |
|---|---|---|
| `lpe_add`, `used_after`; in the corpus `recur`'s `sum_from` and `msum`, `stencil` | negative | a trip count or measure that is negative where the loop does not run |
| `conds_feasible`, `asym_dominated`, `size_atom`, `check_func`, `pol_cmp_ord`, `piece_before`, `pol_nth`, `asserts_hold` | ≥ 0 | true, beyond term budgets: `m·(n − 1)²` with `m` an unknown callee's cost, `5·B·w − 40·w`, which needs `B ≥ 8`, and correlations between an element and its array's extremes |

The first row is a decision the calculus made without saying so: **a cost holds where every loop
it counts runs a non-negative number of times.** `for i in 1..n − 1` counts `n − 2`, and `while t <
e` from `from` counts `e − from`, with no `max(·, 0)`; where the range is empty the formula can be
negative and the real cost is the loop's absence. The regimes can carry conditions only on
working sets, so the true trip is not expressible yet. Written down here and in cost-model
§ Loops; the audit stays a diagnostic, not a test, until that row can be closed or stated as a
precondition the report prints.

With the loop work of 2026-09-26 merged beside it (a size bound once, the scan rule) the compiler is
**92 exact, 49 modulo, 34 bound, 103 unknown, of 278**, and the parity counts are work 101, moves 101,
footprint 128, bound 137.

**Not in this stage, but what makes the number meaningful afterwards.** The roofline term M5
decided (`moves/BW` taken as a `max` with `work/P`), so the prediction reaches time; argv, modules
and arrays by value, without which a domain corpus (stage C's control loops and kernels) cannot be
written; and the editor surface the README describes, which did not exist.

**The editor surface, 2026-09-26.** `neant hints f.nt` prints the report as one JSON document:
each function with the line of its `fn`, its tier, the grey text (`work …   moves …   exact`, the
README's wording, from the same `brief` the report uses), its work and moves as the lockfile
states them, its regimes, bounds, footprint, what it rests on, and for an unknown the cause and
the line the report names; lex, parse and type errors and broken `#[cost]` bounds are diagnostics
with a line and a column instead of a message on stderr. It computes nothing: every field is a
line of `neant cost` or of `costs.lock` taken apart (bootstrap/src/hints.rs), and on the
concatenated compiler it counts what `lock.sh` counts, 92 exact, 47 modulo, 32 bound, 105 unknown
of 276. `editors/vscode` is a VS Code extension in plain JavaScript that runs it on open and on
save, prints each function's cost in grey after its signature with the report in the hover, and
puts the errors in the Problems panel; its grammar is the lexer's keyword and operator set.
`tests/hints/` pins the output on `dot` (exact), `fib` (unknown, recurrence, modulo), a type error
and a broken budget. What is left: the hints are for the saved file — an edit that moves lines
clears them until the next save, since the compiler reads the file and not the buffer — and the
analysis is whole-file, 65 s on the compiler under a debug build, so an editor wants a release
build and, later, a cache keyed by function. The `✓` the README draws after `exact` is not
printed: nothing in the report says what it would check.

**Modules, 2026-09-26** (docs/modules-design.md). A program can span files in seed one: a top-level
`use "path.nt";` resolved from the naming file's directory, each file loaded once, a cycle
rejected with its chain, one flat namespace with a name defined twice rejected naming both files
and lines. The files share one line space, numbered through the concatenation, so the checker,
the cost pass and the emitter are unchanged; the driver maps every line that leaves the compiler
back to `path:N`, including the emitted bounds check. A single-file program takes the old path
and no golden moved; eight new cases in `tests/golden/modules/`. The lockfile is one per program
and keyed by name, so it does not say which file a function is in. Left: the self-hosted side
(the loader in `compiler/*.nt` and `build.sh`'s `cat` replaced by a root file that `use`s the
others), and a file index in positions so that no text is rewritten.

**Program input, 2026-09-26.** argv is done, with a named file (decisions.md §9): `arg(k)` and
`read_file(&path)` return an owned `[u8]`, `arg_count()` and `file_size(&path)` an `i64` (−1 is
the error value), all four `extern`s with a declared cost in a prelude the compiler adds when a
program calls one. What it measured is a cost line, not a speed: a loop over an argument's bytes
is exact in that argument's own atom, `a.len()`, free as a parameter's length is, because an
extern returning an array may name its result's length and a caller mints an atom of its own
for it (`tests/golden/input_args.cost`, `input_file.cost`); reading `n` bytes is priced as a
sequential write of `n`. `neant run f.nt -- args` and the built binary were already the same
program and are tested as such (`.args` files in `tests/golden`). What is left: input read inside
a loop makes the caller unknown rather than a sum over the laps; the declarations are not yet
confirmed by `neant measure`; the self-hosted compiler has none of it.

**Input in loops, and the input declarations measured, 2026-09-26** (cost-model.md § Program
input). `arg_count()` is one atom for the run, so `for k in 0..arg_count()` is exact
(`input_argc`); `arg(k)` read per lap is at most the longest argument, `max(arg[_].len())`, and
the line is a bound (`input_perlap`); `read_file` per lap stays unknown and says why
(`input_perlap_file`). `neant measure` now takes the four builtins, on a real argument or file of
`n` bytes: `arg` was confirmed as declared, and the other three were exceeded by a constant their
declarations lacked — 12.5 instructions for a call to `arg_count`, about 2000 for opening a file.
With `10` and `+ 2500` added all four are confirmed; that changes the constant term of
`input_args.cost`, `input_file.cost` and `modules/input/main.cost` and nothing else in them. A
declaration the sweep cannot evaluate is no longer reported as confirmed. What is left: a function
that returns an argument, called per lap, is unknown, because a result's atom does not carry where
it came from; the kernel's side of a read is not in the counter.

**Arrays by value, 2026-09-26** (docs/arrays-by-value-design.md). What was missing was not a
second kind of buffer but a small fixed-size aggregate that is a value: a struct field may now be
`[T; k]`, `k` a literal, read and written by element (`s.x[i]`, `s.x.len()` the literal `k`), built
by `[a, b, …]` or `[e; k]` in the literal, and copied with the struct — passed, returned, `let`.
The calculus charges writing such a field as it charges `let xs = [a, b, c]`, `k` stores and
`k·elem` bytes, and a copy the same, at `let t = s`, `t = s` and every by-value argument; an
element is a load with no bytes. Two goldens pin it: a state vector stepped by value, and a 2×2
control loop `x = apply(m, x, u)` whose `run` is exact at work `56·n + 2`, moves `64·n + 16` — the
48 bytes a lap are the two argument copies. An array of such a struct is rejected with its
reason (a third layout); a bare `[T; k]` parameter, `==` on arrays, nested fixed-size arrays and
the self-hosted side are left.

**The domain corpus, 2026-09-26** (docs/corpus.md). With argv, modules and arrays by value in, the
count stage C asked for was taken on six programs of its domain — a PID loop over a trajectory
file, a Jacobi stencil and a dense `matmul` sized by arguments, a CSV aggregator, a BFS over an
edge-list file, a ring-buffer filter — written as one would write them, in `tests/corpus/` and
pinned by `corpus.rs`. **14 of 24 functions exact, 58%**, against 33% on the compiler: every kernel
and every controller step is exact. **0 of 6 `main`s are.** The eight unknowns are two shapes: a
size parsed out of text, which is an `i64` returned by a call and so not a size (five `main`s), and
a scan that advances by what it read (`next_int`, `count_ints`, the CSV row loop); with the parsed
size replaced by an argument's length, the stencil's and `matmul`'s programs become exact, the
filter's and the controller's stop at the parser loop, and BFS's at its worklist. What moves the
number next is therefore an integer read from input as an atom for the run, as `arg_count()` is.
Four shapes the corpus had to be written around are kept as rejected cases: string output, a grid
of rows, an array of structs with array fields, a bare `[T; k]` parameter.

**Arrays by value, part two, 2026-09-26** (arrays-by-value-design.md §§ 7–9). A bare `[T; k]` is a
parameter and a return type, and it is **moved**, not copied: `let b = a` was a move for every
array and stays one, free, so a by-value argument is moved in the same way and the callee owns the
buffer; `-> [T; k]` gives the caller the literal length, and `s = f(…, s, …)` rebinds a local to
what comes back, so a control loop over a bare two-element state is exact at `14·n + 5` /
`2·B·n + 2·B + 16`. `==` and `!=` compare a fixed-size array held in a variable, or any struct,
element by element with no early exit — `3·k` work and `2·k·elem` bytes for an array, a compare
per scalar field and three per array-field element with no bytes for a struct. An array of
structs that hold an array is accepted and laid out AoS only; the chooser does not weigh SoA for
it and says so, and `xs[i].p[j]` is a site on the whole field of element `i`, so an update of every
element of every `p` is at its lower bound, `32·n`. It found a bug: an owned-array return of a
literal pointed into its own stack frame; a literal whose buffer leaves the function is now built
on the heap. `err_value_array`, which rejected the array of holders, now rejects `#[layout(soa)]`
on one — the one golden whose expected output changed, because what it pinned is now accepted.
Left: nested fixed-size arrays, a by-value array of structs, SoA for holders, and the self-hosted
side of all of it.

**What the corpus could not write, 2026-09-26** (decisions §§ 10–11, corpus.md). Three of the
corpus's rejected cases. A by-value array passed twice, `dot(a, a)`, is a double move and now says
so — "argument 2 moves `a` into `dot`, which argument 1 already moved" — where it spoke of views
and `&mut`, and a use after a move into a call names the call. Text output is a literal: a string
is the argument of `print` or `println` and nothing else, written byte for byte, costing one call
and its `n` bytes, a constant, so `rejected_string` is `corpus/report`, a labelled report, exact.
A grid of rows is the row-major idiom written by the checker rather than a nested type:
`[[e; n]; m]` is a flat buffer, `g[i][j]` is `g[i·n + j]` with the row checked, and it costs what
the flat idiom costs term for term — `corpus/rows`, a plate relaxed as rows, exact. Left: a string
value, a nested array type, and the self-hosted side of all three.

**A size bound once, 2026-09-26** (cost-model § A size bound once). The first of the corpus's two
gaps is closed. An `i64` bound to an immutable local outside every loop, from anything the
calculus cannot name, is now an atom of its own named after the local. A caller gets it at the
call as `callee.local`. Inside a caller's loop it is refused when used as a size, and widened to
`max(xs[_])` when it only indexes a read. `bound_once.nt` pins the rule and `bound_once_loop.nt`
the two refusals: bound in a loop, and `let mut`.

On the corpus, 4 of the 6 `main`s gain a cost and become modulo: `heat` `≈ 31·n²·steps`, `matmul`
`≈ 10·n³`, and `pid` and `fir` linear in the sample count. Each rests on the parser's
`next_int`, whose loop has no measure. BFS's `main` now stops at its worklist. The share of
exact functions is unchanged at 14 of 24.

On the compiler, `compiler/costs.lock` goes from 92 exact, 47 modulo, 32 bound, 105 unknown to
**92, 49, 32, 103**. `site_top` and `w_func` move from unknown to modulo, and eight more move within
their tier. A handle bound once (`nb = pol_scale(..)`) makes `pols[nb].n_term` an element, not
`max(pols[_].n_term)`. Without the widening at a caller's loop, three functions fell from modulo
to unknown; with it, none falls. The self-hosted cost pass does not have the rule. On the two new
goldens it declines `squares` and the `main`s rather than disagreeing, and `self_host_cost.rs`'s
counts grow only by the functions it matches: work and moves 99 → 101, footprint 117 → 124,
bound 120 → 127. Left: the scan that steps by what it read, the worklist, and the rule on the
self-hosted side.

**A standard library, and text as a value, 2026-09-26** (modules-design § 8, decisions § 12).
Every corpus program carried its own number parsing and math; now `use "std/text.nt";` and `use
"std/math.nt";` find the library wherever the program is (`$NEANT_STD`, else the repository's
`std/`), and it prints as `std/…`. Every function in it is exact or declared: the parsers and
formatters walk a fixed 18 or 19 places, so `parse_int` is 333 and `format_int` 406, constants;
`skip_space` scans by what it reads and is charged the rest of the text; libm's `sqrt`, `exp`, `log`,
`sin`, `cos`, `pow`, `floor`, `ceil` are externs with declared, unmeasured bounds. A string literal
is now also a value: `let s = "…"` is a `[u8; n]` as `b"…"` is, and a literal passed where `&[u8]`
is taken is bound before the call — `n` stores and `n` bytes, a constant. Still not a string: no
type, no concatenation, no growth, no printing of a `[u8]` as text. Left: the corpus adopting std,
printing a buffer, and the self-hosted loader.

**A scan, 2026-09-26** (cost-model § A scan). The corpus's second gap: a `while i < e` whose index
grows by at least `d` on every path through the body, with `i₀` a lower bound at entry, runs at
most `(e − i₀)/d` times.
- The growth is proved by a walk over the body, like the check on a `decreasing` measure.
- A call's result is read through a per-function summary of what it returns (`next_int`: `end ≥
  start`; `after_header`: `≥ 1`), computed to a fixed point from nothing over the call graph.
- `i₀` is the entry value, or, when every assignment to `i` in the function grows it, what `i`
  was first bound to.
- A callee whose cost only falls as an `i64` argument grows is charged with the argument at its
  least.

Such lines are `bound`, with a `scan:` note, and a caller says `rests on f (bound, a scan)`.
`scan.nt` pins the rule and `scan_refused.nt` a body with a path that does not grow the index.

On the corpus, now eight programs, **27 of 28 functions have a cost and 7 of 8 `main`s do**. Only
BFS's worklist is unknown, and nothing is modulo. The bound is loose where a scan calls a scan:
`count_ints` is `9·xs.len()²`, where the calls together read the text once. That is the
amortised scan, left next. On the compiler, `compiler/costs.lock` goes from 92/49/32/103 to
**92, 49, 33, 102**: `bytes_len`, an escape-skipping loop, is bound. `tok_text_eq` did
not move, having already had a constant step. Nor did the lexer, for two reasons the walk states
plainly:
- `i = j + 1` goes through a mutable `j`, and the walk follows immutable bindings only.
- The comment branch grows `i` only through a nested `while`, which the walk must count as
  possibly running zero times.

The second could be closed by the loop's own condition, `src[i] == '/'` holding on entry; the
first by tracking a mutable local that is only increased. The self-hosted cost pass has neither rule. On the two new goldens its footprint is
narrower where a loop is `while i < xs.len() && …`, which it declines before recording the site:
`word_end` twice and `trimmed`, listed in `self_host_cost.rs` with that reason. Its matched counts
grow by the functions it matches: footprint 124 → 128, bound 127 → 134. Work and moves do not
change.

**Printing bytes, the declarations measured, hints as you type, 2026-09-26** (decisions § 13,
experiments.md § The standard library's declarations, editors/README.md). `print_bytes(&s, n)`
writes a view's first `n` bytes, a builtin extern declared in the view's length so that printing
what a formatter produced stays exact: `modules/std`'s `main` went from unknown to exact. Measuring
the declarations found the driver measuring nothing — it passed `1.0`, and gcc folded every libm
call — so an `f64` argument now varies per call; then `sin` and `cos` were under-declared (119 and
123 against 100), the others over-declared up to 5×, and `print_bytes` about 140 a call against
60, and each is redeclared at its measurement and confirmed on the X925. `neant hints --stdin
<path>` reads an unsaved buffer and resolves `use` from `<path>`, and the extension sends the live
buffer after a pause in typing, keeping save as the fallback; the hints were for the saved file,
and are now for what is on the screen. Left: the whole-file analysis on every pause, which on a
large file wants a cache keyed by function.

**A language server, 2026-09-27** (editors/README.md). `neant lsp` is the grey text over the
Language Server Protocol, so it is not tied to one editor: JSON-RPC on stdio with a hand-written
JSON reader and writer (bootstrap/src/lsp.rs, no dependency), full-text sync, and five answers —
diagnostics (lex, parse and type errors and broken `#[cost]` bounds, each published to the file
of a multi-file program it belongs to, and cleared when it is fixed or its buffer closes), an
inlay hint per function after its signature, a hover with the function's report, and a
definition of a function or a struct in whichever `use`d file holds it. It computes nothing:
each analysis is `hints::run` on the buffer, as `hints --stdin` does, its document read back, and
a definition is the module loader's line. `editors/vscode` now talks to it through a small client
of its own, keeping `neant hints` as `neant.mode: hints`; editors/README.md configures Neovim and
Helix. `tests/lsp/` pins a scripted session on a two-file program, 15 messages in 10 ms. Left:
analysis is synchronous and whole-program on every change the editor sends, so a request waits
behind it (the VS Code client debounces; Neovim and Helix send every change); a `use`d file is
read from disk even when its buffer is open and edited; and a definition knows functions and
structs, not locals, fields or parameters.

**An amortised scan, 2026-09-27** (cost-model § An amortised scan; golden `amortised`). The scan
rule charged a parser called once a lap its whole-range cost every lap, so `count_ints` was
`9·xs.len()²` where the truth is linear. A function that advances an index through an array —
returns it, starts it at a parameter, steps it by one under `i < a.len()` and loops only as scans of
it — has its cost read as `α·(a.len() − start) + β`, and a caller that calls it once a lap and moves
its own index on through what it returned pays `β` a lap and `α·(a.len() − v₀)` once: the distances
telescope. Work and moves are amortised column by column. In the corpus the parse's work is linear
(`count_ints` `44·xs.len() + 1`; `fir`'s and `pid`'s `main`s linear in the text); its moves are not,
because while the text fits in memory each call is charged a cold read of the whole array. The
compiler's count does not move (92/49/34/103; the compiler has no call of this shape), and the
parity counts become work 102, moves 102, footprint 133, bound 143. Left: the moves (a warm walk's
residue), bfs's worklist, and the lexer's two scan shapes.

**A bounded worklist, 2026-09-27** (cost-model § A bounded worklist; golden `worklist`). M3's first
hole, and the corpus's last unknown `main`, is closed by the bounds check rather than a count of
pushes: in `while h < t` a tail that only grows by one straight after a write `a[t] = …` stays at
most `t₀ + a.len()`, so the loop runs at most `(t₀ + a.len() − h₀)/step` laps. `bfs` has a cost,
and **every program in the corpus has one: 18 exact, 10 bound, 0 unknown of 28**. The parity test
then found the self-hosted pass wrong where it should have declined: its `assigns_any`, which asks
whether the body assigns a loop's bound, never looked at an `if` in a block's tail, so a worklist
whose push sits in a trailing `if` was costed as a single pass — `drain` at `xs.len() + 8`. Fixed in
`compiler/cost.nt`, seed two rebuilt; the compiler's count does not move (92/49/34/103), and the
parity counts become work 102, moves 102, footprint 135, bound 146.

**The roofline, 2026-09-27** (cost-model § Time; experiments.md § The roofline). `neant cost --eval`
prints a time, `max(span·τ, work·τ/P, moves/BW)`, and which term bound it, with `τ` and `BW`
fitted once on this machine (0.0176 ns per unit of work, 20.8 GB/s) by
`tests/kernels/roofline.py`. Held fixed across the other kernels, the two constants predict
wall-clock within about 30% where a stream or a tiled block dominates — `sum` past the cache,
`matmul_tiled` at every size, `horner`, `matmul_naive` past the cache — and miss by named terms the
model lacks elsewhere: latency (`arena`, 5–31×, growing with the arena), a second cache level and
page faults (small sizes, up to 6×), the TLB (`transpose`, 2–3×), and bandwidth that grows with the
number of streams (`dot`, 0.64). Next is the corpus, program by program.

**The corpus against the clock, 2026-09-27** (experiments.md, the two corpus tables;
`tests/corpus/timing.py`). Timed with the fitted roofline, the kernels carried over (`matmul`
1.3–2.6×) and the text readers did not, 10³–10⁶ too high: a scan's accesses were charged a line
each and its footprint was the whole array, so the parse's moves were quadratic. A scan's index now
has an affine form for access sites only (cost-model § A scan's accesses) — sizes, lower bounds and
residue never read it — and a lap's chain of parser calls is amortised as one. Every corpus program
is now predicted within 0.10–2.7 of its measured time, geometric mean 0.41, erring high as a bound
should; what is left is listed where it is measured (`3·B` a call at a parse's ends, the stencil's
shared rows, `τ` for code that vectorises worse).

**A latency term, 2026-09-27** (cost-model § Time, latency). The lines an access fetches when its
index is a local the loop loads — a pointer chase — are counted as `chase`, composed like work and
read only by a time: `(moves − chase)/BW + chase/B · L`, `L = 112 ns` fitted on a 64 MB chase. `arena`
goes from 5–31× too fast to 0.28–1.00, the kernels' geometric mean from 1.56 to 1.14; no bound, tier or
report line moved. The same day: calls in both branches of an `if` had their moves added where work
took the larger branch; they now take the larger when it is cheap to know and the sum otherwise
(`heat` 8× → 4× high; five compiler lock lines drop, the count does not move). Then neighbouring
sites (cost-model § Neighbouring sites): sites on one array whose offsets are whole laps of a loop
share lines when `span + 1` laps fit, so the stencil is three streams, not five (`24·n²` against the
lower bound's `16·n²`), `heat` goes to 0.50, measured refills stay below the prediction, one compiler
lock line moves and the self-hosted pass, without the rule, is listed as charging more
(`NEIGHBOURS_INSTEAD`). A second cache level for the time (`--M3`, the analysis run again at
`M` = L3, BW₂ and L₃ fitted by differences) was measured and left off by default: it moves the error
between kernels (streams better, strided access worse) and the geometric mean from 1.14 to 1.16.
A TLB charge per line of a page-strided access (`--tlb`, 9 ns fitted on transpose) was measured the
same way and is off too: transpose 2–3× → 0.85–1.28, naive matmul 1.0 → 0.24–0.30.

**The lexer's shapes, 2026-09-27** (cost-model § A scan, a loop that surely runs; golden `lexer`). A
nested `while` counts one lap of growth when the path surely enters it — its guards imply its
condition over all 256 values of the byte at the index — and a lap-local `let mut j = i + 1` only
increased is at least its initialiser. `lex` is a bound (quadratic: its string branches are not
amortised inline), the compiler is 92/49/35/102, and parity is 105/103/140/152. Then a misreading
of `while` conditions (cost-model § Loops without a range, reading the condition): `j > start` with
both sides locals was a loop in `start`, and `i + 1 < n` had no variable alone; fixed, the compiler is
**94/51/35/98**, parity 105/103/141/154 (golden `whileshapes`). Two callers of the newly exact callees
fall to unknown for an argument they cannot name; charging such callees as terms was tried and made
costing the compiler take minutes, so it is left.

**Programs the project did not write, 2026-09-28** (experiments.md § The Benchmarks Game;
`tests/bench`). n-body, spectral-norm, mandelbrot, fannkuch-redux and binary-trees, ported with their
published structure, print the reference outputs; 11 of 15 functions are exact, and the three with a
cost time at 0.15–4.3 of the predicted. The two constants' failures are named: work's latency
(mandelbrot's dependent loop 4×) and a triangular loop's moves summed where they share a hull
(n-body 6.7× high). The second is half fixed the same day: a triangle's laps are charged their hull
once while they fit (cost-model § Moves, a triangle), n-body 0.15 → 0.27, golden `tri`'s `pairs`
`4·n²` → `32·n + …`; one compiler lock line moves, the count does not. And the first: work in a loop
that carries a scalar through a multiply is `serial`, charged `τ_s = 0.155 ns` fitted on a logistic map
(cost-model § Time, serial work); mandelbrot 4.2× → 0.50, the benchmarks within 0.26–1.93. Then
divisions counted apart and charged `τ_div = 0.148 ns` more (a sum of reciprocals): spectral-norm
1.9 → 1.27, the benchmarks within 0.26–1.28. Then footprints of several exact ranges per parameter,
widened over outer laps and clamped to the site's field, and a call on a resident footprint moving
nothing (cost-model § Moves): n-body's per-step moves go to zero, which its refills confirm, and it is
2.3× too fast instead of 3.7× too slow; thirteen goldens' `main`s drop a constant, the self-hosted pass
is listed as charging those calls (`RESIDENT_INSTEAD`), and the compiler's count does not move. A
store chained through memory at a fixed index is a serial unit a lap: n-body 2.3 → 1.76, the Benchmarks
Game programs within 0.50–1.77. And the roofline M5 decided and did not fit: `BW(P) = min(P·BW,
BW_max)`, `BW_max = 65.6 GB/s` on ten cores; the M5 kernels' measured / predicted is flat in `P`
(parallel sum 1.44–1.49, compute map 2.0–3.1), so a memory-bound chain's flattening is predicted.
A read goes stale only where a loop may write its slot or its field, not anywhere in its array
(cost-model § A size read from memory, by slot): the compiler is 94/51/36/97 (golden `slots`).
A recursion on the children of the node it is handed, `f(xs, xs[t].l)`, `f(xs, xs[t].r)`, is at most
`k·xs.len() + 1` invocations over an arena that is a tree — the promise a walk makes, and a bound
(cost-model § Recursion, a tree): binary-trees' `check` gets a cost, 12 of 15, and the compiler is
94/50/38/96, `same_ty` among them (golden `arena_tree`); `resolve_ty` goes down `.a` from three
`if`s in a row, which the count of calls reads as three an invocation, and is refused. Then the
same for a component of mutual recursion, a path at a time (cost-model § A tree in an arena, and a
forest; `forest.rs`, golden `forest`): a group of calls on one node goes down each path of links at
most once, so over a tree at most `L·xs.len() + 1` groups; the one-function rule is its case of
one, which `resolve_ty` now passes. The compiler is 94/49/42/93. The cost walkers (`w_*`, `m_*`)
have the shape and are unknown for one member's `while` with no measure; the checker's and emitter's (`check_*`,
`emit_*`) call back in once a lap of a counted loop over stages or fields, which the rule refuses.
A `while` that reads `xs[i]` in its condition and steps `i` up once a lap stops by the array's end
at the latest (cost-model § A scan to a sentinel; golden `sentinel`): `call_arr_len` and
`used_after` get a cost, 94/50/42/92, and with it the cost walkers' members are costed through and
refused for the reason that is theirs — `m_expr`'s cost depends on its node, `w_expr`'s on a size
bound once — which their lines now say. Costing them through doubled the lock's time, 9 s to 19 s.
Then an invocation's cost is taken apart from its node (cost-model § A tree in an arena, and a
forest): a read at an index that moves is its array's most, an unknown callee's moving argument is
`_`, a regime is dropped for the sum of its pieces — and every array the cost still reads must be
one the component never writes, which the composition had not checked. `expr_same`, `idx_coef`
and `emit_type` get a bound, 94/52/43/89 (golden `forest`'s `spell` and `grow`), and the walkers are
refused for reading the state they write. A `max` of the regimes instead of their sum took a
walker's moves, 5216 terms, 94 s to settle, and the lock six minutes; the sum takes 16 s. And the
layout pass, which costs the program twice per struct, tried each refused component every time:
a refusal does not depend on a layout, so it is tried once, and compiling the compiler is back to
8.5 s from 18.
A cursor in a slot, `while ds[0] < ds[1] { …; ds[0] += 1 }`, counts as a variable does (cost-model
§ A size read from memory, a cursor in a slot; golden `cursor`), and writing its test found a hole
in the committed calculus: a callee's size read from an array its caller's loop writes was the
first lap's value for every lap. Such calls are unknown now, and the compiler is 95/43/44/96 — nine
`modulo` lines were wrong, one line more exact and one more bound from the cursor. A stale read
that is only an unknown callee's argument is `_` there instead (95/45/44/94: the compiler's
`pol_close(…, pst[1])` everywhere), and a resident call no longer zeroes an unknown callee's moves.
Back to time (evaluation, next): an amortised call's line ends are charged once for the loop, the
region rule's "longer than the array" is asked asymptotically, and serial work and divisions are
amortised as work is — the corpus from 0.56 to 1.26, `csv` 0.10–0.22 to 0.81–0.95 (experiments.md,
a parse's ends and its serial work); no report line changes but the moves of eight parse loops.
And an `f64` a short lap carries through an add is a unit of serial work (cost-model § Time, through
an add): `sum` in L1 1.2× from 3.6×; with no bound on the lap's work it broke `horner` and a tiled
`matmul`, which overlap the add's wait (experiments.md, a carried add in a short lap).
Then the memory side (cost-model § Time, streams and write-backs): two streams move at twice one's
rate and a stored line's write-back is charged — together, since either alone made the kernels
worse — and the kernels' root-mean-square log error goes from 0.61 to 0.53, the Benchmarks Game to
a geometric mean of 1.00.

**Next for reach: a recursion over a cursor (designed, not built).** The parser — `or_level` down to
`primary`, `block`, `type_expr`, and the cost attribute's `c_*` reader — is the largest block of the
compiler left unknown, and it is a mutual recursion over a token cursor, not a tree: `st[0]` only
grows, `bump` moves it one, and every loop reads `toks[st[0]]` before it bumps. The argument that
bounds it, and what each piece must check:

- *the cursor*: a slot `st[k]` of an `i64` array every member hands on, only ever increased,
  directly or through callees (a per-function summary: never writes it, only increases it, always
  increases it by at least one on every path, or increases it or sets an error slot);
- *own bumps*: an increase the invocation itself makes, directly or through an always-increasing
  helper, after a read of `toks[st[k]]` in the same invocation — so each happens at a cursor below
  `toks.len()`, and there are at most `toks.len()` of them in the run;
- *calls*: every call into the component but an invocation's first is preceded, since its last
  such call, by an own bump; the first calls, made at the entry position, form an acyclic graph of
  depth `D` (`or_level → and_level → … → primary`, nine). An advance a callee made does not count:
  it would let every frame on the stack call again, and the count is no longer linear;
- *laps*: a loop that calls into the component, or has no other measure, bumps in every lap before
  its calls, and is charged one lap, its laps counted with the bumps;
- *the error slot*: an advance-or-flag helper (`expect`) counts as a bump only where the loop or the
  next call is guarded by the flag being clear, and a member entered with the flag set returns
  without calling in — the part of the proof that is easiest to get wrong, and why this is written
  down before it is built.

Then the invocations are at most `D·(toks.len() + 1)` and the laps `toks.len()`, and the component
costs at most `(D + 1)·(toks.len() + 1)` times its members' own costs, settled as a forest's are.

*Checked against the parser the same day, before building: it does not hold.* `block`'s loop
calls `item` once a lap and never bumps itself — the statement it parses is what advances the
cursor, inside the callee — and so do the argument and field loops of `primary` (`expr`, then
`eat` of a comma). Counting a callee's advance is what the rule refuses on purpose: an advance deep
in the stack would let every frame above it call again, and laps of loops in nested frames can end
on the same token (`a || b && c`: the `&&` lap and the `||` lap end at `c`). A linear bound needs
what the grammar guarantees and the rule cannot see — that nested laps are separated by a token of
their own (`)`, `}`, `;`). What would hold without it is quadratic, invocations at most the tokens
times the deepest stack, and the argument for even that needs the error path made precise. The
parser stays unknown; a declared cost on it would be an assumption the lockfile names, not a proof.

## M7 — the constant factor

Prove the asymptote, search the constant. A micro-architectural cost line (what llvm-mca and uiCA
compute for a block, as default output, applicable because the type system knows what may be
reassociated); schedules separate from algorithms; search over schedules pruned by the cost model
and decided by measurement, persisted in `costs.lock`. An own backend is justified here and only
here — as the search space LLVM does not expose, not as better heuristics.

## Stage E — evidence for a reader outside

**Why.** Every number in evaluation.md was taken by the people who wrote the rules it tests, on
programs they chose or wrote, on one core of one machine, and every constant and most rules were
added after a measurement showed they were missing. That makes a record of how the model was
built, not evidence for it. The four questions (evaluation § The claim: bytes, time, reach, trust)
are the right ones; what a reader outside needs is each answered on code, a machine and a
threshold the model was not fitted to, beside what the tools that already exist would answer.
Stage E does not wait on M7, and M7 does not need it; it does need the end of stage D, since reach
is measured with the calculus frozen.

**Claim.** On programs the project did not write and a machine it did not fit, the cost the
compiler infers (1) predicts bytes and time within stated factors, (2) ranks alternative versions
of a program the way the machine does, better than a count of operations, (3) reaches more of a
program than a bound analyser reaches on the same program written in C, and (4) is never wrong
where it says `exact`, on programs generated to find the case where it is.

**Do.**

1. **Thresholds before numbers.** docs/heldout.md is written, and committed, before any held-out
   program is costed or timed: for each question the number that counts as holding, the number
   that counts as failing, and what is reported in between. The roofline's constants and the
   calculus are frozen at a commit named there; a rule added afterwards is measured on the
   held-out set as a separate row, never folded into the first.
2. **A held-out corpus.** Ported with its published structure, the port committed before it is
   costed, its text never edited to suit the rules, every refusal kept as a rejected case with its
   cause, as `tests/corpus` does:
   - **PolyBench/C** (30 affine kernels): the suite IOLB, IOUB and Bao's exact count report on, so
     the moves can be put beside theirs kernel by kernel;
   - **the rest of the Benchmarks Game** (fasta, k-nucleotide, reverse-complement,
     pidigits, regex-redux): the text- and hash-heavy half; those the language cannot express yet
     count as refusals of the language, not skipped;
   - **a few kernels from outside numerics**: a sort, a hash join, a trie or B-tree search, an LZ
     style compressor — ordinary code shaped the way the compiler's own is, but not written here.
   Reach is reported on this set by cause, as stage D reports the compiler; bytes and time on
   every program that gets a cost.
3. **Baselines, on the same programs.**
   - *Bytes:* measured refills (as now), IOLB's lower bound (`--iolb`), and a cache simulator at
     `M` with the machine's associativity, so a gap is split into the model's error and the
     ideal-cache model's. On PolyBench, beside Bao's exact count where it is published.
   - *Reach:* the same programs as C, the port's own emitted C and the upstream C, through a C
     cost analyser (KoAT2 or Loopus, whichever runs on the emitted C), and the functional ones
     through RAML where they translate. This is the scope argument (decisions §4) as a number: what
     being a language buys over analysing its output.
   - *Time:* the roofline against three smaller models — `work·τ` alone, `moves/BW` alone, and
     the roofline without the latency, serial and division terms — so each term shows what it
     earns on programs it was not fitted on.
4. **Ablation of the calculus.** Each rule stage D added (read atoms, list walks, the scan rule,
   worklists, tree and forest recursion, neighbouring sites, amortised calls) behind a switch
   that turns it off, and the held-out reach and time re-measured with each off. A rule that
   moves nothing on the held-out set is reported as fitted to the corpus it was found on.
5. **A second and a third machine.** One x86 server core and one other ARM core (an Apple M-series
   or a Neoverse). The constants refitted there by the kernels each was fitted on here and
   nothing else, then every table re-taken: the claim is "two constants per machine", and a
   second machine is where it is tested. The byte counter differs per machine
   (`l2d_cache_refill` here, LLC or L2 misses on x86); which one is used, and why, is written
   down before the sweep. Past one
   core: the M5 kernels and the `.par()` programs at P = 1…cores on both.
6. **Ranking, the claim a user acts on.** For each corpus and held-out program, two to four
   versions a person would plausibly write (AoS and SoA, tiled and not, a copy and in place, a
   worklist and a recursion, a different loop order). The compiler's predicted order is compared
   with the measured order per program (Kendall τ) and against the work-only baseline. A cost that
   is 2× off but always orders versions right is useful; one within 1.3× that orders them wrong is
   not. Pairs whose measured times differ by less than the run-to-run noise are reported apart.
7. **Trust by construction, not by reading.** Two parts:
   - *A counting emitter* (new): `neant emit --count` instruments the C with the calculus's own
     work units and every array access, run through the cache simulator at `M` and `B`, so a
     program's actual work and moves in the model's own units are a number the run prints.
   - *A generator* of programs in the fragment the calculus claims: nests of counted and
     `while` loops, calls with footprints and residues, arenas walked as lists and trees, scans
     that step by what they read, sizes read from memory. Every line the compiler marks `exact`
     is checked equal to the count at random sizes; every `bound` checked not below it. The
     generator runs in CI at a small budget and at a large one before each count in evaluation.md.
     The two-compiler parity check stays; this one does not depend on either compiler being right.
   A mechanised proof of the whole calculus is not attempted; one of the composition rule alone
   (stage A's four-part object: `g`'s moves reduced by `f.residue ∩ g.footprint`, loops as the
   body composed with itself) over a small core, in Lean, is — it is the part the signature claim
   rests on and the literature does not have (decisions §5).
8. **The lockfile in review, replayed.** For every commit of the project's history that changed
   `compiler/*.nt` or a corpus program, the `costs.lock` diff against the measured change in time
   on the harness: how often a line that moved named a change the machine saw, and how often the
   machine saw a change no line named. That is the claim that the lockfile belongs in code review,
   taken on history rather than asserted.
9. **One command per table.** `tests/eval/run.sh` rebuilds every table in evaluation.md from a
   clean checkout, records machine, core, kernel, compiler versions and the frozen commit, and
   writes the numbers the document quotes; the document's tables are generated, not typed. A
   container for the parts that do not need the counters (reach, trust, simulator bytes), so they
   run anywhere.

**Exit.** evaluation.md re-written on the held-out set with the thresholds of (1) beside every
number: each question holds, fails, or sits between, said in those words; each baseline's column
beside the compiler's; the ablation table; the second machine's tables beside the first's.

**Kill**, each on its own claim:
- *Time.* If the constants refitted on the second machine do not bring its held-out programs
  within the thresholds, the time claim is withdrawn to "on the machine it was fitted to", and the
  bytes model is what remains.
- *Ranking.* If the ranking is no better than the work-only baseline's, moves adds nothing a
  user can act on, whatever its accuracy in bytes, and the README's first fact is not shown.
- *Reach.* If the C analyser, run on the emitted C, reaches what the compiler reaches, being a
  language buys no reach, and the positioning question (§ Who switches) is reopened with that
  number, towards a whole-program analysis of C or the kernel DSL.
- *Trust.* A wrong `exact` from the generator is fixed with a golden, as now; if they keep coming
  at a rate that does not fall across three rounds, the tier is renamed `estimate`, the lockfile
  stops claiming it, and budgets are checked against bounds only.

**Not in it.** A user study: there is one author, and a study of one is an anecdote. If the
ranking and the lockfile replay hold, a small one (people choosing between versions with and
without the grey text) is where it would go next.

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
    modules.rs          `use "file.nt"`: loads a program's files into one line space, maps lines back
    input.rs            the input builtins (arg, read_file, …) as externs with declared costs
    hints.rs            `neant hints`: the report as JSON, for an editor
    lsp.rs              `neant lsp`: the same as a Language Server on stdio
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
std/                    the standard library, `use "std/…"`: text.nt (numbers in text), math.nt
editors/                the grey text in an editor: vscode/ (grammar, a client of `neant lsp`); README.md, with Neovim and Helix
tests/
  golden/               .nt programs with expected output (.out, .exit), rejection (.err), cost report (.cost)
    modules/            multi-file programs, one directory each, rooted at main.nt
  hints/                what `neant hints` prints for some of golden/, one .json each
  lsp/                  a two-file program and the pinned `neant lsp` session on it (session.out)
  corpus/               the domain corpus (docs/corpus.md): one program per directory, rooted at main.nt, lib/ shared by `use`
  kernels/              the M1/M2 experiments: kernel templates and sweep.py, the perf harness; iolb.sh runs IOLB in docker
docs/
  plan.md               this file
  cost-model.md         the calculus, as implemented
  experiments.md        what was measured against what prediction, and what it changed
  evaluation.md         the claim, and every measurement that bears on it, side by side
  decisions.md          decisions with the reasoning that produced them
  corpus.md             the domain corpus's tiers and what blocks the rest
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

# Implementation plan

The front door is [../README.md](../README.md). This is the order things get built in, what each
step has to prove before the next one starts, and what would stop the project.

The governing rule: **the cost model is a scientific claim before it is a compiler feature.** It
says a static number predicts what the hardware does. If that is false, nothing else here matters,
so the plan front-loads the experiment that could falsify it and puts a kill criterion on it.

## Decisions taken up front

**Rust first, self-hosted later, two seeds forever.** The compiler will be written in neant. Not
first, and not by deleting the thing that came before it — the previous language's single worst
structural problem was the bootstrap: an image that could only be rebuilt by a binary that already
carried a working one, because the Rust front end had been deleted the moment the self-hosted one
worked. So:

- **The bootstrap compiler is Rust and is never deleted.** It builds with `cargo build` from a clean checkout. Once
  the self-hosted compiler exists, it stops growing — it only has to compile the subset the
  compiler is written in — but it stays in the tree and CI builds `compiler/` with it on every
  commit.
- **The second seed is the compiler's own C output**, committed as `bootstrap/neant.c`. Because the
  backend emits C, the self-hosted compiler compiled by itself *is* a C file, and anyone with clang
  can rebuild the tree from it. This is what the old image should have been: a seed that is
  readable, diffable, and buildable with a tool everyone already has.
- **The fixpoint is a test.** `clang bootstrap/neant.c → stage1; stage1 compiler/ → stage2;
  stage2 compiler/ → stage3; stage2 == stage3`, and separately `bootstrap compiler/ → stage2'` with
  `stage2' == stage2`. Two independent roads to the same binary, checked in CI.

Self-hosting is M6, not M0, for a reason given there. Zero dependencies is a goal, not a rule; a
parser combinator or an SMT binding in the bootstrap compiler is acceptable if it saves a month.

**Emit C, do not write a backend.** Cost inference is a static analysis; it needs no backend at
all. What it needs is a way to *run* the analysed program on real hardware to check the prediction,
and `clang -O2` on generated C is that way in a week rather than a quarter. It is also a defensible
long-term architecture (Nim, Chicken, Futhark's C target): clang owns instruction selection and
register allocation, this compiler owns everything the cost model cares about — layout, fusion,
tiling, loop order — because those are decided before the C is written. An own backend is
reconsidered only if layout control turns out to need it.

**Source files stay `.nt`.** Same name, same extension.

**Sizes are symbolic, costs are expressions.** A cost is a normalised expression over size
variables (`n`, `m`, from slice lengths), the two cache parameters (`M` lines of capacity, `B` bytes
per line) and the usual functions (`log`, `√`). Two costs compare by asymptotic dominance. When the
normal form would blow up, the function degrades to *measured*; it never fails to get a line.

## Milestones

Each milestone has an exit criterion that is a test, not a feeling.

### M0 — A program runs

Lexer, parser, name resolution, a typed IR with size variables attached to every array-typed
value, and a C emitter. The subset: `i64 f64 bool`, fixed arrays `[T; n]` and slices `&[T]`,
`let`, arithmetic, `if`, `for i in a..b`, first-order functions, `println` for the harness.

`neant build f.nt` produces a binary via clang. No cost anywhere yet.

**Exit:** a dozen small programs compile and produce the right output.

### M1 — The exact tier, and the experiment

Add iterator chains (`iter map filter sum fold zip`) and comprehensions to the subset. Infer, for
every function, **work** (primitive operations, symbolic in sizes) and **moves** (bytes across the
cache boundary, in the I/O model). Print both in the shape the README shows. Write `costs.lock`.

The moves rule set for M1 is deliberately coarse and written down in `docs/cost-model.md` as it is
built: every array access in a loop nest is classified as *sequential* (`n/B`), *strided* (`n`)
or *reused-within-cache* (`0` after the first load, when the touched footprint is shown to fit
`M`); tiling is recognised when a loop nest is split by a constant that the footprint check
accepts. The polyhedral model is the eventual replacement; it is not needed to run the experiment.

**Exit, part 1 — golden tests.** `tests/golden/*.nt` each carry the expected `work` and `moves`
line; they pass.

**Exit, part 2 — the experiment.** Six kernels: `sum`, `dot`, `saxpy`, `transpose`, naive
`matmul`, tiled `matmul`. For each, over a size sweep from L1-sized to several times L3-sized, on
one pinned core, compare predicted `moves` against measured `LLC-load-misses × 64` from `perf
stat`. Two things must hold:

- the **slope** in `n` is right — the model says `n³` for naive matmul and `n³/√M` for tiled, and
  the measured misses scale the same way;
- the **ratio** predicted/measured is stable within a factor of ~3 across the sweep for a given
  kernel. The constant is allowed to be wrong; it is not allowed to drift.

**Kill criterion.** If the model cannot separate naive from tiled matmul in measured misses, or if
the ratio wanders by an order of magnitude across sizes, the moves model as specified does not
predict the machine. Stop, and do not build M2 on top of it. Either the rule set is fixed until the
experiment passes, or the project's central claim is withdrawn.

**Status: passed, 2026-09-22, on the second rule set.** Slopes within 0.1 on the five fixed-pattern
kernels; naive/tiled separated 30× measured against 28× predicted; ratios stable per kernel. The
first rule set failed on the tiled product and was changed twice — access sites now compete for
the cache, and fitting is strictly less than `M`. The record, with the numbers and the three
things the model does not see, is [experiments.md](experiments.md).

### M2 — Guaranteed fusion, one lower bound, one gap report

Two things the README promises that M1 does not yet deliver.

**Fusion as a guarantee.** An iterator chain that cannot be fused into a single loop — because a
stage needs the whole intermediate (`sort`, `reverse`), or because a closure captures a mutable
that a later stage also touches — is a **type error** naming the stage, not a slower program. The
compiler proves the fused form has the moves cost it reported.

**The lower-bound catalogue, entry one.** Recognise matrix multiply in the IR (a triple loop nest
with the characteristic access pattern, or a call to a `matmul` intrinsic), attach the Hong–Kung
bound `n³/√M`, and compute the gap. Offer the two closing transformations — tiling by `√(M/3)` and
transposing the column-accessed operand — as rewrites the compiler can apply, and report the new
cost after applying.

**Exit:** the matmul report in the README is real output on real code, and the `[apply]` rewrites
are verified in the M1 experiment harness to move the measured misses the way the model says.

### M3 — The dial: nobody is silent

Take the language from "the exact subset" to "a language", and make sure every function still
gets a line.

- **`while`.** Infer the termination measure for induction-variable loops (`i < n` with `i += k`).
  When inference fails, the error asks for one — `while c decreasing m` — exactly as Rust asks for
  a lifetime, and only then.
- **Recursion.** Extract a recurrence from structural recursion on lists and from divide-and-conquer
  on slices; solve the master-theorem shapes. Anything else is `unbounded`.
- **`unbounded` and `io` as effects** in the `uses` position. They propagate through calls; a
  function that calls an unbounded function is unbounded.
- **The measured tier.** For any function the static analysis leaves unbounded, the compiler can run
  it over a size sweep, fit `~n^k`, and write that into `costs.lock` marked *measured* with the
  range it was measured on. A function that falls from *exact* to *measured* is a lockfile diff.
- **`#[cost(...)]`.** `work_at_most`, `moves_at_most`. Checked by asymptotic dominance; a failure is
  a build error that shows the inferred cost next to the asserted one.
- **Higher-order costs.** `map f` costs `n · cost(f)`; costs are parametric in callback costs and
  instantiated at the call site when `f` is known.

**Exit:** a corpus of ordinary programs — string processing, a small interpreter, a graph search,
a parser — every function of which has a `costs.lock` line, none of which says nothing, and a
golden test that each line is the expected *kind* (exact / parametric / recurrence / measured).

### M4 — Views, layout, regions

The part of the design that makes the moves model apply to programs with structure in them.

- **Structs and views.** `&xs[i]` and `&p.field` denote (collection, index) or (collection,
  index, field) and never a location. Nothing in the language observes an address.
- **Compiler-owned layout.** For each struct type, choose AoS or SoA (later AoSoA, hot/cold
  splitting) per type, driven by which fields each loop touches and what the moves model says each
  choice costs. Report the choice.
- **Region inference.** Tofte–Talpin style: pointer-linked structures (`List`, `Tree`) live in a
  region the compiler infers from escape behaviour; the region has a known size; a traversal is
  bounded by `|region| / B` regardless of access order, and by `0` after first touch when the
  region fits a cache level. `Arena::with_capacity` is the explicit override.
- **Layout evolution.** Layout is a deterministic function of the type definition, so a
  position-independent value written by one build reads back under any build of the same
  definition. Type changes are out of scope until there is a use for them.

**Exit:** a linked-list and a tree traversal get a region-granular moves bound; switching a struct
from AoS to SoA on a benchmark changes measured misses in the direction and magnitude the model
predicts.

### M5 — Span, in-place reuse, the editor

- **Span** joins work and moves in every cost; `T ≤ W/P + O(S)` becomes a statement the compiler
  can make about a `par` loop or a parallel iterator.
- **In-place reuse.** Uniqueness analysis in the Perceus style: `ys = xs; ys[3] = 9` is written the
  same way whether it copies or not, and the cost line says `1` or `n` and, in the `n` case, names
  the line that keeps `xs` alive.
- **An LSP** that serves the cost line as an inlay hint after the signature, and the M2 gap report
  as a code action.

### M6 — Self-hosting

Rewrite the compiler in neant. This waits for M4 because a compiler is exactly the kind of program
the early language is worst at: tree-shaped (the AST needs recursive types and therefore regions),
string-heavy, hash-map-heavy, and full of `while` loops over tokens. Before M3 it cannot be
expressed; before M4 it cannot be expressed comfortably. After M4 it is an ordinary program.

The port is of the whole compiler, cost inference included. The Rust cost module written for
M1–M3 is written a second time here; that is the price of validating the model early instead of
waiting until the language could express its own analysis, and it is a bounded price — a few
thousand lines, in a language whose shape is settled by then. The bootstrap compiler is then frozen: it keeps
whatever it has, gains nothing, and is only ever touched to keep compiling `compiler/`.

Two things fall out of self-hosting *this* language that do not fall out of self-hosting in
general:

- **The compiler is the M3 corpus.** "Every function gets a line and nobody is silent" is tested on
  the largest real program in the tree.
- **`compiler/costs.lock` is the compiler's own complexity, stated.** Which pass is superlinear in
  the size of the input, which one is `n · d` in nesting depth, which one is measured because it
  recurses on the AST — a self-hosted compiler usually proves the language works; this one would
  also state what its own compile time costs and why.

**Exit:** the two-seed fixpoint passes in CI; `compiler/costs.lock` is committed; `bootstrap/` is marked
frozen in its README.

### M7 — The constant factor

Everything before this is asymptotic: the cost model proves the shape of a function's cost and
leaves the constant to clang. The constant is where the last 1.2–2× on a hot kernel lives —
latency chains, vector width, unroll factors, port pressure, prefetch distance — and it is where
LLVM's heuristics stop and a person with an assembly listing starts. This milestone takes that
work over, not by writing a better heuristic backend than LLVM (a solo project cannot) but by three
things this language is unusually placed to do.

**Prove the asymptote, search the constant.**

- **A micro-architectural cost line.** What the matmul report does for moves, done for latency and
  throughput: `dot` is reported as latency-bound on a 4-cycle FMA chain against a 0.5-cycle
  throughput bound, with the fix — four independent accumulators — offered as `[apply]`. This is
  what llvm-mca and uiCA compute for a basic block, made the default output for every function, and
  made applicable because the type system knows whether the reduction may be reassociated (always
  for integers; for `f64` only where the region allows `reassoc`).
- **Schedules, separate from algorithms.** Halide's separation: `schedule matmul for <target> {
  tile ..; vectorize ..; unroll ..; prefetch ..; }`. The algorithm fixes meaning and asymptotic
  cost; the schedule moves only the constant, and cannot break either. Hand-tuning stops meaning
  rewriting the algorithm and hoping.
- **Search where heuristics stop.** For a hot kernel on a specific machine, enumerate schedules,
  prune by the cost model (a schedule with worse `moves` is never run), measure the survivors,
  keep the best, and persist it in `costs.lock` so it is still there tomorrow. This is the shape
  in which search has actually beaten hand-tuned code — TVM/Ansor over cuDNN, Halide's
  autoscheduler over hand schedules, CryptOpt over hand-written assembly — and the language makes
  the search unusually cheap: no aliasing means every reordering is legal without analysis, known
  sizes mean specialisation is free, and the cost model is the pruning function.

**Where an own backend becomes justified.** Not to write better instruction selection than LLVM by
hand, but to expose instruction selection and scheduling as a *search space* that LLVM does not
offer — a backend that need not be good in general because it solves one kernel on one machine
with time to spare. That is CryptOpt's position and the only one from which a small backend beats
a large one. It also restores the property the previous language had and this one gave up by
emitting C: that the bytes that run are bytes the compiler can be asked about.

**Exit:** on a small set of kernels that are already at their moves bound after M2, the searched
schedule beats `clang -O3` on the same C by a measured margin, the micro line predicted the
bottleneck the search fixed, and the result survives a rebuild via `costs.lock`.

The micro model is an approximation on out-of-order cores; uiCA's error against hardware is
nonzero and this one's will be larger. It is therefore used only to prune, never to choose — the
final choice is always a measurement.

### Not scheduled

Generics beyond what the milestones need; strings and I/O beyond the harness; `dyn` dispatch
costing; zero-copy persistence as a feature rather than a consequence; compile-time performance of
the compiler itself.

## Layout of the repository

```
bootstrap/              everything that builds the compiler from nothing
  Cargo.toml, src/      the Rust compiler. Seed one. Frozen after M6, never deleted.
    lex.rs  parse.rs  ast.rs  resolve.rs  types.rs
    ir.rs               typed IR, every array value carries a size variable
    cost/
      size.rs           symbolic sizes and costs: rational polynomials over atoms, B and M
      analyze.rs        the work and moves calculi, one walk (docs/cost-model.md is its spec)
      recur.rs          recurrence extraction and solving           (M3)
      bounds.rs         the lower-bound catalogue                    (M2)
      lock.rs           costs.lock read/write/diff
    emit_c.rs
    main.rs             neant build | cost | lock | measure | validate
  neant.c               compiler/ compiled by itself. Seed two. Regenerated at each release. (M6)
compiler/               the compiler in neant. Empty until M6, then the one that grows.
  *.nt
  costs.lock
tests/
  golden/               .nt programs with expected output (.out, .exit), rejection (.err), cost report (.cost)
  kernels/              the M1 experiment: six kernel templates and sweep.py, the perf harness
  bootstrap.sh          the two-seed fixpoint
docs/
  plan.md               this file
  cost-model.md         the calculus, as implemented
  experiments.md        what was measured against what prediction, and what it changed
```

## Validation harness notes

`perf stat -e LLC-loads,LLC-load-misses`, pinned to one core with `taskset`, minimum of several
runs per size. The development machine is heterogeneous (big.LITTLE); pinning to a big core is not
optional, and the harness must record which core it ran on. Sizes step by factors of 2 from below
L1 to well past L3; the slope is fitted on the region past L3 where the I/O model is meant to hold.

## Risks and what is done about them

| risk | mitigation |
|---|---|
| The moves model does not predict hardware | The M1 experiment, with a kill criterion, before anything is built on it |
| Symbolic sizes explode | A small normal form; when it fails, the function degrades to *measured* rather than the compiler failing |
| Recurrence solving is a research project | Master-theorem shapes only in M3; everything else is `unbounded` and measured |
| Region inference is a research project | M4, not earlier; the explicit `Arena` is the fallback that always works |
| "Measured" becomes the tier everyone lives in | The error that accompanies a fall to *measured* says exactly what would bring the function back to *exact*, and `costs.lock` makes the fall visible in review |
| The generated-C path cannot express a layout the model wants | Discovered in M4, where the own-backend decision is revisited with evidence |
| Self-hosting recreates the bootstrap trap | Two seeds, both exercised in CI on every commit; the bootstrap compiler is frozen, not deleted; the second seed is C, not an image |
| Two compilers to maintain | The bootstrap compiler stops growing at M6 and only has to compile `compiler/`; the cost module is written twice, once, and that is the whole overlap |

## Rough shape of the calendar

M0 one to two weeks. M1 three to four, half of it the experiment. M2 three. M3 four to six. M4
six to eight. M5 four. M6 — the port — six to eight. M7 is open-ended and starts with the micro
cost line, which is the cheap part. That is roughly a quarter to M3, the point at which the
language exists and the thesis is either standing or not; two more months to M4; self-hosted
somewhere around month eight or nine; and the constant factor after that, for as long as it keeps
paying. Solo pace; the numbers are for ordering, not for promising.

# Implementation plan

The front door is [../README.md](../README.md). This is the order things get built in, what each
step has to prove before the next one starts, and what would stop the project.

The governing rule: **the cost model is a scientific claim before it is a compiler feature.** It
says a static number predicts what the hardware does. If that is false, nothing else here matters,
so the plan front-loads the experiment that could falsify it and puts a kill criterion on it.

## Decisions taken up front

**Rust, not self-hosted.** The previous language's single worst structural problem was the
bootstrap: an image that could only be rebuilt by a binary that already carried a working one.
This compiler is written in Rust, builds from source with `cargo build`, and stays that way. Zero
dependencies is a goal, not a rule; a parser combinator or an SMT binding is acceptable if it saves
a month.

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

### Not scheduled

An own backend; generics beyond what the milestones need; strings and I/O beyond the harness;
`dyn` dispatch costing; zero-copy persistence as a feature rather than a consequence; compile-time
performance of the compiler itself.

## Layout of the repository

```
Cargo.toml
src/
  lex.rs  parse.rs  ast.rs  resolve.rs  types.rs
  ir.rs                 typed IR, every array value carries a size variable
  cost/
    size.rs             symbolic sizes and their normal form
    work.rs             the work calculus
    moves.rs            the I/O-model calculus (docs/cost-model.md is its spec)
    recur.rs            recurrence extraction and solving           (M3)
    bounds.rs           the lower-bound catalogue                    (M2)
    lock.rs             costs.lock read/write/diff
  emit_c.rs
  main.rs               neant build | cost | lock | measure | validate
tests/
  golden/               .nt files with expected cost lines
  kernels/              the M1 experiment: kernels, sweep driver, perf harness
docs/
  plan.md               this file
  cost-model.md         the calculus, written as M1 builds it
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

## Rough shape of the calendar

M0 one to two weeks. M1 three to four, half of it the experiment. M2 three. M3 four to six. M4
six to eight. That is roughly a quarter to M3 — the point at which the language exists and the
thesis is either standing or not — and another two months to M4. Solo pace; the numbers are for
ordering, not for promising.

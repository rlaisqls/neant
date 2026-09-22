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
literature review (decisions §5): lower bounds from IOLB through `emit --scop` and `cost --iolb`,
the hand catalogue reduced to a fallback.

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

## M4 — views, layout, regions

Stands on stage A. Structs and views (`&xs[i]`, `&p.field` as (collection, index[, field]),
never an address; projections do not return addresses, which is the one mechanism argument for
being a language). Compiler-owned representation per type, chosen by the footprint and moves of
the loops that touch it, reported. Region inference (Tofte–Talpin) for pointer-linked structures,
with the region's footprint and the residue rule giving the traversal bound. Arrays as values that
move and return. Layout as a deterministic function of the type definition.

**Exit.** A linked-list and a tree traversal get a bound from the residue rule; switching a struct
from rows to columns on a benchmark changes measured refills in the direction and magnitude the
model predicts. **This is the project's first real gate**: the point at which being a language,
rather than an analyser, has bought something.

## M5 — span and in-place reuse

`span` joins the cost; `T ≤ W/P + O(S)` becomes a statement about a parallel loop. Uniqueness in
the Perceus style: `ys = xs; ys[3] = 9` is written one way and the cost line says `1` or `n`.

## Self-hosting

After M4, as a goal. The compiler in neant, the Rust compiler frozen as seed one, `bootstrap/neant.c`
as seed two, the two-seed fixpoint in CI, and `compiler/costs.lock` as the compiler's own stated
complexity. Not the coverage proof; the corpus for that is chosen in stage C.

## M7 — the constant factor

Prove the asymptote, search the constant. A micro-architectural cost line (what llvm-mca and uiCA
compute for a block, as default output, applicable because the type system knows what may be
reassociated); schedules separate from algorithms; search over schedules pruned by the cost model
and decided by measurement, persisted in `costs.lock`. An own backend is justified here and only
here — as the search space LLVM does not expose, not as better heuristics.

## Not scheduled

Zero-copy persistence (unrelated to cost; out of the argument); generics beyond what the stages
need; strings and I/O beyond the harness; compile-time performance of the compiler.

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
      bounds.rs         the one hand lower bound, the fallback
      scop.rs           export of an affine function as a SCoP for IOLB, indices delinearised
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
| region inference and cost-driven layout are research problems | M4 waits for stage A; the explicit `Arena` and an explicit layout attribute are the fallbacks that always work |
| self-hosting recreates the bootstrap trap | Two seeds, both in CI; the Rust compiler is frozen, not deleted; seed two is C, not an image |
| the generated-C path cannot express a layout the model wants | Discovered in M4, where the backend decision is revisited with evidence |

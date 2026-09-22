# Decisions

Design decisions with the reasoning that produced them, so the reasoning is not lost when the
code is. Newest first. A decision is recorded when it was argued over, when it rejected
alternatives worth remembering, or when a future reader would otherwise ask "why on earth".

## 7 — `T ≤ work/P + O(span)` is missing a term, on the machine's own evidence

**Decided 2026-09-22, after `tests/kernels/par_sweep.py` measured a compute-bound and a
memory-bound `.par()` chain across `P` on five same-type cores (experiments.md, M5).**

m5-span-design.md §6 named the risk before building anything: the classical work-span bound has no
memory-bandwidth term, and M1 had already found `sum`/`dot` bandwidth-bound on one core. The
question was whether that stayed true across several cores sharing one path to memory, or whether
enough parallel slack absorbed it. Measured: a compute-bound `.par()` chain holds 84% efficiency at
`P = 5`; a memory-bound one (`.par().sum()`) holds 97%, then breaks — 75% at `P = 4`, 61% at
`P = 5`, speedup barely moving from `P = 4` to `P = 5` while compute keeps climbing. The model
predicts the same curve for both, because `work/P + span` has nothing in it that could tell a
compute-bound chain from a memory-bound one.

**Accepted:** the bound needs a second term, `moves/BW` for some aggregate bandwidth `BW`, taken as
a `max` with `work/P` — a roofline, the same shape the fit test already uses `max` for elsewhere in
this calculus, not a new kind of machinery. **Not decided:** what `BW` is, or how it is measured —
five points on five cores show the shape breaking, not enough to fit a constant. Also not settled:
the mixed-cluster case. A supplementary run across this machine's two core types (X925 and A725, a
big.LITTLE split) in the same session showed a real loss from adding the slower cluster late — a
genuine effect, but a scheduling question (OpenMP's static split assumes same-speed cores) distinct
from the bandwidth question this decision is about, and left open.

## 6 — Improving on the tools' logic where the compiler knows more, not where they know more

**Decided 2026-09-22, after the question whether IOLB and IOUB should be reimplemented.**

Reimplementing them was weighed in three layers and answered per layer. The exponent of a lower
bound is an LP over the statement's array references and costs a few hundred lines on the
existing rational arithmetic; the constants are the papers' lemmas, weeks of work whose failure
mode is an unsound bound printed as fact, for a constant factor on a gap number; the full
polyhedral machinery is a project. IOUB's upper bound is a cost formula in the tile side
minimised numerically, and the model already has a better cost formula for the tiled program.

Done, because the compiler has what the tools lack:

- **Bounds are derived, not catalogued.** The Brascamp–Lieb exponent by an exact LP over
  injective references, the exact iteration count by summation, the footprint from a cold cache
  (`bounds.rs`, cost-model § Lower bounds). The one hand entry is now a special case that falls
  out. IOLB stays for the constant where it is installed, `5.66×` tighter on the product.
- **The schedule is erased before the tool is asked.** A lower bound is a property of the
  computation; IOLB works on the loop nest and gave the data size for a tiled product. The
  export untiles, with the divisibility assumption carried on the bound. The native bound never
  needed it.
- **The tile side is read off the model, not searched.** The tiled program is analysed with its
  side symbolic; the side is the boundary of the fit condition of the cheapest regime, in closed
  form; the integer recommended is checked against the exact working set. Where IOUB solves an
  I/O formula numerically at fixed sizes, this is the model's own exact cost, symbolic in `M`.
  What the machine then said about the choice is in § What the machine said and in
  docs/experiments.md, and it changed the rule.

### What the machine said

The model's tile side for the product, `T < √(M/8)` = 510, moved thirty times what the model
predicted on the machine; every side with all tiles inside `M` moved what it predicted; the side
at which everything fits in half the cache, 181, moved the least (docs/experiments.md). The
ideal cache's regime "one tile resident, two streaming" is optimal replacement and does not exist
on an LRU-like machine. Two rules follow and are in the compiler: fits for a *choice* are decided
at `M/2` (Sleator–Tarjan's factor, the octave M1 measured), and model ties go to the smaller
side. The lesson is the M1 lesson again — a choice the model makes at the edge of its own
approximation is the one to measure first — and it is the reason the tile choice was measured
before it was written up as an improvement.

Not done, and why: IOLB's constants (a lost race against a team that has spent a decade, for a
constant), the full polyhedral layer (a project unrelated to the thesis), IOUB's DSL (its
strength is multi-level caches and bandwidths, which is M7's problem, not a DSL's). A
layout-aware line-granularity bound for pointer-chasing code under a layout the compiler chose
is recorded as research for after M4: the theorem that a general schedule cannot use the rest of
a fetched line is not written.

Corrected in passing: this file and docs/experiments.md had called IOLB's bound on a triangular
reduction weak; it was the parser reading the asymptotic line and not the full one. The footprint
bound now states what that line stated.

## 5 — Against the literature: the bound side is IOLB's, the upper side and the composition are not in it

**Decided 2026-09-22, after reading Olivry et al. (IOLB, 2020) and Elango et al. (POPL 2015) in
full, and Bao et al. (POPL 2018) from its abstract and the author's description; the Bao paper
itself was not obtainable without library access.**

- **The lower-bound catalogue is folded.** IOLB derives parametric, non-asymptotic data-movement
  lower bounds for arbitrary affine programs, reproduces Hong–Kung on the product (the catalogue's
  one entry), improves on every other published hand bound across PolyBench, and proves two
  kernels untileable — a result the catalogue could never give. Extending a hand catalogue against
  that is a lost race. `bounds.rs` keeps its one entry as the fallback when IOLB is not installed.
  Done the same day: `neant emit --scop` exports an affine function to IOLB's input (C with
  `#pragma scop`, PET's front end; flat parametric indexing delinearised, since `i·n + k` is not
  affine in the polyhedral sense) and `neant cost --iolb` parses the bound back into the gap
  report. What the tool then said (docs/experiments.md): on the product its bound is `4·√2 ≈ 5.7×`
  tighter than the hand entry's constant, so the hand entry had been understating the bound all
  along; on `dot`, `saxpy` and `sum` the bound equals the function's own moves to the leading
  term (gap 1×); on the tiled product it returns only the data size, so the hand entry stays as
  the stronger statement there and both are reported.
- **The program as written is what this compiler owns; the complexity's two ends are not.**
  Three questions are distinct. What must any schedule move: IOLB, from below. What does the best
  tiled schedule move: IOUB (IOOpt, Olivry, Iooss, Tollenaere, Rountev, Sadayappan, Rastello,
  PLDI 2021), from above, for a perfectly nested rectangular fully-permutable band described by
  hand in a YAML DSL with its reuse directions, for multi-level caches with bandwidths, solved
  numerically for fixed sizes, with the permutation and tile sizes recommended. What does *this*
  program move: Bao 2018, exactly, inside affine nests in set-associative caches, and nobody
  outside them. The gap needs the first and the third. The moves rules here are an approximation
  of Bao's count where the nest is affine, and cover what it does not — calls, `while`, recursion,
  data-dependent access, and a whole function as one composable object. The plan holds a place for
  replacing the rules by the exact polyhedral count inside affine nests. (This bullet first read
  "the upper side is what this compiler owns", which IOUB's existence made ambiguous; corrected the
  same day.)
- **The tiling rewrite is a costed transformation, not a search.** `--apply tile` earns its place
  by being computed in the model, locked and measured on the machine, not by finding the optimum;
  IOUB chooses tile sizes better, over more cache levels. Where a nest is a rectangular band the
  affine analysis already knows its reuse directions, so an export to IOUB's DSL is a small step,
  recorded in the plan as an option.
- **Composition is not in the literature.** Elango's "composition" assembles a program's lower
  bound from its sub-computations' bounds; nobody composes a function's cost from a signature
  with a footprint and a residue, checks callers against declarations alone, or audits the
  boundary. Stages A, B and C stand.
- **Hardware validation is a differentiator and moves to the front.** IOLB validates achieved
  operational intensity with the Dinero cache simulator; this project counted `l2d_cache_refill`
  on the core, which is how it found that read streams register at half and write streams not
  at all. A simulator would never have shown either. The earlier judgement that the M1 sweep was
  over-described is reversed for this part.

## 4 — What four reviews changed: composition is an effect, the language's reason is scope, and the boundary is the measure of it

**Decided 2026-09-22, after four written reviews of the tree at M3.**

Accepted, and written into README, plan and this file:

- **Cost composes like an effect, not like a type.** The cache is a shared resource, so what `g`
  costs after `f` depends on what `f` left resident. The call-site re-analysis of the first
  calculus (specialisation, inherited loops) was whole-program analysis wearing a signature's
  clothes, and the symbolic line — the one in the lockfile — was the least predictive one until
  regimes were added. The fix is structural: a cost object of four parts, `work`, `footprint`
  (roots and byte ranges), `moves` from a cold cache, and `residue` (what is resident after,
  bounded by `min(footprint, M)`), composed by one rule — `g`'s moves are reduced by
  `f.residue ∩ g.footprint` — with loops as the body composed with itself, so the fit test and
  the slide rules become instances of the rule. Stage A. Its kill condition: if the goldens cannot
  be reproduced from signatures alone, moves is not a composable quantity, and "cost lives in the
  signature" is withdrawn in favour of "whole-program analysis tool".
- **The reason to be a language is scope, not mechanism.** Aliasing is a lint on safe Rust;
  `repr(Rust)` is known to rustc; position independence is unrelated to cost — all three were
  withdrawn from the argument. The one mechanism argument that stands is narrower: projections
  return addresses in Rust (`Index` → `&T`, `Vec<T>` contiguous by definition), so the compiler
  cannot choose representation without redefining references. The real competitor is the embedded
  IR (MLIR, Halide, TVM, Triton, Exo), which owns layout without being a general-purpose language.
  What only a language buys is that the analysis covers everything and the tier of `main` means
  something.
- **That reason and its strongest objection are the same fact**: scope ends at the C boundary.
  Stage C makes the boundary declared (`extern` with a cost), measured (`neant measure` confirms
  it), and audited (every lockfile line names what it rests on). The share of a program's cost
  resting on the boundary becomes a number — the size of the language's reason.
- **Declarations first** (stage B): budgets in real units checked under given `M`, `B` and size
  bounds, the declaration as the lockfile line and the inference as what is checked against it,
  callers seeing only callee declarations. Exit test: delete a callee's body, keep its
  declaration, and the caller still checks. `dyn` inverts: the interface carries the budget.
- **Coverage is a number and it is low.** M3 corpus, 11 functions: 6 exact, 2 of them by a
  declared measure, 5 unknown. Kept in the README; re-measured after stage C.
- **Dates are gone from the plan.** The M0–M3 pace came from a subset cut to be tractable; region
  inference, cost-driven layout, uniqueness analysis and schedule search are each team-scale
  problems, and extrapolating the easy part's speed onto them was not honest.
- **Self-hosting is a goal, not the coverage proof.** The compiler is the program this language is
  worst at (tree-shaped, string-heavy, `while` over tokens) and so the worst corpus to prove
  coverage on. It moves after M4 and stays a goal because the author wants it.
- **Zero-copy persistence leaves the argument** and stays unscheduled.

Not accepted as stated: that the pre-regime symbolic line was "wrong by 8×". It was an upper bound
under a stated assumption. That the lockfile carried the least predictive bound is accepted, and
was the reason conditional costs were pulled forward.

The safety-critical / real-time market, where every constraint this language accepts is something
certification already demands, was proposed early in the design discussion and set aside in favour
of a general-purpose language judged on its own merits. It is recorded here as the one adoption
story that was ever plausible, so that if the coverage number after stage C does not move, the
choice can be revisited with the number in hand.

## 3 — Exactness first: an exact answer that is finite is always taken; an exponential one is taken until it explodes, and then it says so

**Decided 2026-09-22. Governs everything below and everything after.**

### The principle

When the calculus can give an exact answer with finite work, it gives the exact answer, even
where a looser bound would have been cheaper to implement or faster to compute. Where the exact
answer is exponential in the input, it is still computed exactly by default; if it explodes,
the compiler **says so and stops**, and folding toward a bound is something the user asks for
(`--regimes N`), never something the compiler does silently. A cost line the compiler prints is
either exact under its stated model, an upper bound it calls an upper bound, or an honest
"unknown, because". It is never a quietly loose number.

The reason is the thesis. "The compiler answers for cost" is worth less than nothing if the
answer carries a silent factor of two (a triangular loop counted on its rectangular hull) or a
trusted promise (a `decreasing` measure nobody checked): the reader cannot tell the honest lines
from the lazy ones. Compile time was weighed and judged the cheaper thing to spend.

### What this changes, item by item

Finite and cheap — no reason but effort was ever against these:

| where the calculus was loose | the exact answer |
|---|---|
| a triangular loop `for j in i..n` charged as its hull `n` (2× loose) | symbolic summation over the loop variable: `Σᵢ (n−i) = n(n+1)/2`, Faulhaber's formula for polynomial bodies |
| a linear recurrence `T(m) = T(m−c) + f(m)` solved as `f·(m/c+1)` (exact only for constant `f`) | the same summation: `Σₖ f(k·c)` |
| divide-and-conquer solved to `Θ` with a rounded constant | the geometric series per monomial of `f`, exact rational constants |
| a `decreasing` measure taken on trust | verified: along every path through the body, the affine update of the locals in `m` must give `m' ≤ m − 1`; unverifiable → unknown, said so |
| a loop variable's entry value found as "the last assignment before the loop" | reaching definitions, the standard dataflow |
| aliasing checked by root array at call sites | views carry their range as size expressions and disjointness is proved with the polynomial arithmetic; refused only when unprovable |

Exponential — exact by default, loud on explosion:

| where a lid was planned | the exact answer |
|---|---|
| conditional costs capped at `K` regimes, folded pessimistically (§2 as first written) | every regime kept; the infeasible ones — whose conditions contradict (`n·B ≥ M` and `8n² < M` cannot both hold) — eliminated by deciding the conjunction: interval arithmetic when conditions are monomial, rational LP when linear, a sound "cannot decide, keep both" otherwise. No fold unless asked |
| `if` charged as the sum of both branches | `max`: the cost algebra becomes (max, +, ×) with normal form "a max of polynomials", dominated members dropped; the special case that counted recursive calls per path disappears into the rule |

Unchanged, because the exact answer is not a cost model but a case split on residues: a tiled
nest's `⌈n/T⌉·T` stays the bound `n + T − 1`. And the ideal cache — fully associative, a step at
`M` — is the model, not an approximation of one; M1 measured what it misses.

### The price, stated

Summation, recurrence constants, dataflow, measure verification and disjointness proofs are
polynomial in the program and change compile time imperceptibly. Regimes and `max` are
exponential in the number of independent thresholds and undominated branches; feasibility
pruning and dominance remove most of it in real code, and the rest is reported rather than
hidden. When a function's cost splits forty-seven ways the message is that it does, with
`--regimes 4` and `#[cost(..., when = "…")]` as the two ways to ask for less.

## 2 — Conditional costs: when the fit test cannot be decided, report both sides

**Decided 2026-09-22; implemented the same day with §3's rules (no lid, feasibility pruning,
`if` as max).** The naive product's own line came out in exactly the three regimes below, with
the thresholds `B·n < M` and `8·n² < M`; every concrete `main` kept its numbers to the byte.

### The problem

The moves rule turns on one test: a loop level reuses the lines its inner levels touched when
their working set — a number of bytes — is strictly less than `M`. In a `main` where sizes are
numbers the test has an answer, and M1 measured that answer against the machine. In a function
analysed on its own the working set is an expression in the size variables, and

    n·B < M ?

has no answer: it depends on `n`. That is not a weakness of the analysis. **The cost is
genuinely piecewise in `n`.** For the naive product, the column of `b` is `n` lines: below
`n = M/B = 32768` it stays in cache and consecutive `j` iterations share each line, so `b`
costs `8·n³` bytes; above it, every access is a fresh line and `b` costs `B·n³`.

The rule in force at the time was "a symbolic working set does not fit". Its consequence: the
function's own line — the one in `costs.lock`, the one the gap was computed against (13033×) —
described a regime that no feasible `n` is in. A 32768² matrix of doubles is 8 GB. Every call
of `matmul` that will ever happen is in the other regime, where the line is wrong by a factor of
eight and the concrete `main` (gap 1453×) was right.

### Alternatives rejected

- **Assume it fits.** Then at the `i` level the whole of `b` (8·n² bytes) is assumed resident
  and `b` is charged once. False for every `n > 512`. Each fixed assumption is wrong in some
  regime, because there are *two* thresholds (`n·B < M`, `8·n² < M`) and one assumption cannot
  agree with both.
- **A degree heuristic** — "a working set of lower degree than the input fits". It needs to know
  that `a.len() = n²`, which the atoms do not say; and it is a fixed assumption in disguise,
  silently choosing a regime.
- **Leave the function line conservative and let `--eval` and `measure` supply the realistic
  numbers.** Zero implementation, but the lockfile — the artefact reviewers actually look at —
  would record the most pessimistic regime forever.

### The decision

When a fit test does not reduce to a number, **fork**: carry the computation on under both
outcomes, each tagged with its condition, and report a piecewise cost:

    matmul   moves   8·n³ + 8·n² + B·n²     if  n·B < M          (n < 32768 at M = 2 MiB)
                     B·n³ + 8·n³ + B·n²     otherwise

The model states what it knows — the thresholds and the cost on each side — and assumes
nothing. The gap is reported per regime. `costs.lock` records the pieces, with conditions kept
symbolic (`n·B < M`) so a change of machine does not change the file; the report may print the
threshold as a number for the machine it is on.

This is also what M4 needs. The traversal of a linked list of `n` nodes in a region costs
`|region|` bytes if the region fits a cache level and `B·n` if not, and `|region|` is symbolic.
Under the old rule "fits" could never be said; under this one the M4 promise is a line of output.

### The worst case, and the lid on it

Counted naively, every symbolic fit test doubles the number of regimes: a nest of depth `d` gives
`2^d`, and `k` such nests in one function give `(2^d)^k`. Two facts make it smaller:

1. **Monotonicity.** An outer level's working set contains the inner level's. If the inner does
   not fit, the outer does not either. So a nest's regimes are "fits up to level `j`" for
   `j = 0..d`: at most `d + 1`, not `2^d`. The product has three, because its innermost working
   set is one line and never forks.
2. **One size variable means intervals.** Every condition is then `n < c_k` for some threshold,
   and the regimes are the intervals between thresholds: their number is linear in the number of
   distinct thresholds, however many nests there are.

What genuinely multiplies: several size variables tested by different nests (`n·B < M` and
`m·B < M` are independent, and `k` nests give up to `(d+1)^k`), and a callee's conditional cost
substituted into a caller's, where the conditions do not coincide.

The lid first planned here — `K = 4` regimes, folded pessimistically — was withdrawn by §3 the
same day. Regimes are kept exactly; the infeasible combinations are removed by deciding whether
their conditions can hold together; folding happens only when the user asks (`--regimes N`), and
an explosion is reported, not hidden. Folding everything would reproduce the old rule, so the old
behaviour remains the floor the user can choose.

### Consequences to carry

- `#[cost]` must hold in **every** regime. That makes a bound effectively a bound on the worst
  regime; to assert the realistic one, a `when = "n·B < M"` clause will be needed. Start
  without it.
- The report gains lines. Two per function is the budget for the summary; every feasible regime
  is in `costs.lock` and in `neant cost --regimes all`.
- Regime order in `costs.lock` must be canonical (by threshold) or diffs become noise.

## 1 — Work approximates emitted instructions

**Decided and implemented 2026-09-22.**

Work had been "primitive operations" by a rule table (`let` = 1, index = 1, `op=` = 2, …). Its
shape was right and its constant meant nothing: `dot` said 6 per element and the machine ran
4.5, and the 6 did not correspond to anything one could name. Three options were weighed —
leave it as a shape indicator, redefine it toward instruction count, or drop it from the
headline in favour of moves alone.

Redefined. What lives in a register — literals, variable reads, `let`, `.len()`, `&x` — is
free; every arithmetic, load, store and branch is one; `for` costs two per iteration, `while`
one plus its condition, a call two. The reason is `neant measure`, which prints predicted work
beside `perf stat -e instructions`: the two must be in the same unit for the ratio to carry
information ("gcc fused the multiply-add" rather than "the rule table is odd"). Dropping work
was rejected because an exponential recursion like `fib` touches no memory — moves is 0 — and
only work shows it.

A side effect worth keeping: a chain now costs exactly what the hand-written loop costs, where
before every `let` the desugaring introduced added one.

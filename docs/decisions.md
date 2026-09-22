# Decisions

Design decisions with the reasoning that produced them, so the reasoning is not lost when the
code is. Newest first. A decision is recorded when it was argued over, when it rejected
alternatives worth remembering, or when a future reader would otherwise ask "why on earth".

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

**Decided 2026-09-22. Not yet implemented; it goes in before M4, which depends on it.**

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

# Decisions

Design decisions with the reasoning that produced them, so the reasoning is not lost when the
code is. Newest first. A decision is recorded when it was argued over, when it rejected
alternatives worth remembering, or when a future reader would otherwise ask "why on earth".

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

So there is a **lid**: at most `K` regimes per function (`K = 4` to start). Beyond it, adjacent
regimes are **folded toward the pessimistic side** — merged, taking the larger cost. A folded
cost is still an upper bound, and folding everything reproduces exactly the old rule. The old
behaviour is therefore this design's worst-case fallback, and nothing can be worse than today.
The report shows two regimes and says how many were folded; the lockfile keeps up to `K`;
analysis time grows by at most `K`.

### Consequences to carry

- `#[cost]` must hold in **every** regime. That makes a bound effectively a bound on the worst
  regime; to assert the realistic one, a `when = "n·B < M"` clause will be needed. Start
  without it.
- The report gains lines. Two per function is the budget; the rest folds.
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

# The cost model, as implemented

What `neant cost` computes, rule by rule. `bootstrap/src/cost/analyze.rs` implements exactly this;
when the two disagree the code is wrong. The plan for what this grows into is in
[plan.md](plan.md); this file is only what stands today.

Every function gets two polynomials over its size variables and the machine parameters `B`
(bytes per cache line) and `M` (bytes of cache):

- **work** — primitive operations executed;
- **moves** — bytes that cross the cache boundary, in the I/O model.

Or it gets one sentence saying why not, with a line number. It never gets nothing.

## Size variables

A function's atoms are its parameters, in order: a slice parameter `a: &[T]` contributes
`a.len()`, an `i64` parameter `n` contributes `n`. There are no other atoms — every size inside
the body must reduce to these and constants, or the function's cost is unknown.

An `i64` expression is a **size expression** when it is built from integer literals, size
atoms, `.len()` of a local whose size is known, immutable `let`s bound to size expressions, and
`+ − *`, or `/` by a literal. Inside a loop, a loop variable standing in a bound is replaced by
its own bound — the end for an upper bound, the start for a lower — so a triangular loop
`for j in i..n` is charged as its rectangular hull `n`. When the loop variables cancel between
the two bounds (`ii*T .. ii*T + T`) the trip count is exact (`T`).

Machine parameters default to `M = 2 MiB`, `B = 64` and are set with `-M` and `-B`. They are
kept symbolic in the reported polynomial and made numeric only for the two decisions below.

## Work

| construct | work |
|---|---|
| literal, variable read, `&x` | 0 |
| `a op b`, `-a`, `!a`, `x as T`, `.len()`, `println` | 1 + operands |
| `x[i]` (read) | 1 + index |
| `let`, `x = e` | 1 + value; `op=` counts 2 |
| `x[i] = e` | 2 + index + value |
| `for i in a..b { body }` | bounds, once; then `trip × (1 + body)` |
| `if c { t } else { e }` | 1 + c + t + e — **both branches are charged**; an upper bound, tight when one is empty |
| call | 1 + arguments + the callee's work |
| `[e; n]` | n; `[a, b, c]` | 3 |

Everything inside a loop is multiplied by the product of the enclosing trip counts.

## Moves

Moves are counted per **access site** — each `x[i]` read or write in the source — as the number
of distinct cache lines it touches over the whole loop nest it sits in, times `B`.

The index is analysed as an **affine** function of the enclosing loop variables with size-
polynomial coefficients: `a[i*n + k]` is `n·i + 1·k`. A loop variable whose range starts at an
outer loop variable is read as that start plus a local offset, so `k` in `for k in kk*T..kk*T+T`
contributes `T·kk` to the index's dependence on `kk`. An index that is not affine — a product of
two loop variables, a mutable accumulator, a value loaded from memory — is treated as moving by a
whole line every iteration of every loop.

Then, from the innermost loop outward, with `t` the loop's trip count and `s` the number of
bytes the access moves per iteration of that loop (its coefficient times the element size):

```
for every access site:  lines = 1
for each loop, innermost first, for every site inside it:
    ws = Σ over the sites inside this loop of their lines      -- they share the cache
    if ws·B is not a known number < M:          lines ×= t            -- the working set does not fit
    else if s = 0:                              lines ×= 1            -- same lines every iteration
    else if s ≥ B (or s is symbolic):           lines ×= t            -- a fresh line every iteration
    else:                                       lines ×= t·s/B        -- consecutive iterations share lines
                                                (but never below 1)
moves += Σ lines · B
```

The **fit test** is the whole model: a level reuses the lines its inner levels touched only when
the working set of *every* access site inside that loop, added together, is a known number of
bytes strictly less than `M`. The sum is what the M1 experiment forced: a tiled matrix product
whose `a`-panel alone fits was measured re-reading it, because the `b` tiles streaming through
the same loop level brought the total to exactly `M`. Strictly less, because a cache is never
empty of everything else. A working set that still contains a size variable is assumed not to
fit. This is why `matmul(n, ...)` analysed on its own reports
`B·n³ + 8n³ + 8n²` — a conservative bound in which nothing is reused — while the same function
called from a `main` where `n = 1792` reports `8n³ + …`, because there the column of `b` is
`1792` lines, that is a number, it is under `M`, and the walk down consecutive columns is seen to
share lines.

That is the second rule: **a call whose argument sizes are all known at the call site is analysed
again for that call site**, with the parameters bound to the caller's polynomials, so every fit
and stride decision inside is made with the caller's numbers. The function's own line in the
report and in `costs.lock` is still its symbolic, conservative one.

And the third: when such a call sits inside the caller's loops and **none of its arguments moves
with those loops** — `for r in 0..20 { sum(&xs) }` — the callee is analysed *inside* them. The
caller's loops appear to the callee as levels with no variable, so every access in the callee
sees them as stride-0 levels and reuses across them when its working set fits. A twenty-fold
repeat over an array that fits `M` is then charged one pass, not twenty. When an argument does
depend on a caller loop, nothing is inherited and the call is charged in full per iteration.

Other moves:

| construct | moves |
|---|---|
| `[e; n]`, `[a, b, c]` | a sequential write of the array: `n × elem_bytes` |
| call, arguments loop-invariant | the callee's moves analysed inside the caller's loops (rule three) |
| call, an argument moves with a loop | the callee's moves, times the enclosing trip counts — no reuse across calls |

## What the model does not see

Each of these rounds up, so the reported cost stays an upper bound; each is a place where the
measured number can come in under the prediction.

- **The fit test is a step.** Below `M` everything is reused, at `M` nothing is; a real cache
  with LRU and prefetch traffic degrades gradually from well under `M`. The tiled product at
  `n = 1600` (working set 85% of `M`) measures between the two.
- **Associativity.** The cache is ideal — fully associative, optimal replacement. A stride that
  is a multiple of a large power of two maps many lines to the same set, and the real cache
  evicts what the model keeps. The experiment avoids power-of-two sizes for this reason, and
  the discrepancy at such sizes is a known, expected failure of the ideal-cache assumption.
- **Overlap between iterations at a stride ≥ B.** Two accesses in consecutive iterations that
  land in the same line by accident (a sliding window wider than a line) are counted twice.
- **The prefetcher**, which fetches lines the program has not asked for yet and so raises the
  measured count on strided access above the model's.
- **Element alignment.** An element is assumed not to straddle two lines.
- **Recursion.** A recursive function is *unknown* until recurrences are solved (M3).
- **Branches** are summed, not maxed.

## Lower bounds

For some computations the catalogue knows what *any* program must move, and the report says how
far the function is from it.

**Entry one: the matrix product.** A statement `acc += A[ia] * B[ib]` — or `C[ic] += …` —
inside a loop nest, whose two indices are affine in the loop variables, share at least one of
them (the reduction) and each have one the other lacks, is a contraction. Whatever the loop
order or tiling around it, the number of multiply-adds `N` is the product of the enclosing trip
counts, and a cache of `M` bytes must move at least

```
8·N / √M   bytes          (Hong–Kung 1981, constant per Irony–Toledo–Tiskin, 8-byte elements)
```

The report prints the bound under the function's line, and the **gap** — the ratio of the
function's leading moves term to the bound's, at the machine's `B` and `M` — when the size
variables cancel, or the plain ratio when both are numbers. A specialised call hands its bounds
up to the caller, scaled by how often it is called, so a concrete `main` gets a gap too.

Under the bound, the report says what each operand does in the innermost loop when it moves by
a whole line or more per iteration: that access is the one paying for the gap.

## Rewrites

A function with a recognised bound has two rewrites tried on it; each is costed by running the
calculus on the rewritten IR, and the result is printed as a suggestion with the `--apply` flag
that performs it. Nothing is applied silently. Both work on the naive shape the catalogue
recognises — `for i { for j { let mut acc = 0; for k { acc += A·B } C = acc } }` — and are
refused, with a sentence, on anything else.

- **`name:tile`** — the three loops are split into tiles of side `T`, the largest power of two
  with three `T×T` tiles of 8-byte elements strictly inside `M` (256 at 2 MiB). The tile loops
  accumulate into `C` across the `kk` tiles, so `C` is cleared first; the result is the same
  product. Tile edges use `min(ii·T + T, n)`, which the calculus reads as `T`.
- **`name:transpose`** — the operand walked down a column (the one whose index carries the
  innermost variable times a row length) is copied transposed before the nest, and the inner
  loop reads the copy along a row. The copy costs a transpose and `8·nk·nj` bytes of memory.

A tiled nest's `N` is counted on its rectangular hull — `(n+T−1)/T` tiles of `T` — so the bound
printed for a rewritten function is over by `(1+T/n)³`. The suggestion lines take the gap against
the original function's bound, which is exact.

What the two buy is a matter of the regime. In the symbolic report, where nothing is assumed to
fit, transposing turns `B·n³` into `8·n³` — a factor of `B/8`. In a concrete program where the
column of `b` is a known number of lines that fits `M`, the naive walk already shares each line
across eight consecutive `j` and the transpose buys nothing while costing a copy; the report
says so, because it is computed, not looked up. Tiling wins in both regimes.

## Reading a line

```
matmul           work 10·n³ + 6·n² + n             moves B·n³ + 8·n³ + B·n²           exact
                 lower bound      moves 8·n³/√M    (matrix product, Hong–Kung 1981)   gap 11585× at M = 2 MiB, B = 64
                 `b` moves by 8·n bytes per iteration of the innermost loop: a new line every time (line 7)
                 tile by 256      work ≈ 10.05·n³  moves ≈ n³/8                        [--apply matmul:tile]
fib              unknown: calls `fib`, whose cost is unknown (recursive; recurrences are not solved yet) (line 3)
```

`exact` means both polynomials were derived by the rules above with no unknown. It does not mean
the machine will agree to the byte; it means the shape is proven and the constant is the rules'.
`--eval n=1792,B=64` substitutes and prints numbers.

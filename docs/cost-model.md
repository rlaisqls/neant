# The cost model, as implemented

What `neant cost` computes, rule by rule. `stage0/src/cost/analyze.rs` implements exactly this;
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
lines = 1
for each loop level, innermost first:
    if lines·B is not a known number ≤ M:      lines ×= t            -- inner working set does not fit
    else if s = 0:                              lines ×= 1            -- same lines every iteration
    else if s ≥ B (or s is symbolic):           lines ×= t            -- a fresh line every iteration
    else:                                       lines ×= t·s/B        -- consecutive iterations share lines
                                                (but never below 1)
moves += lines · B
```

The **fit test** is the whole model: a level reuses the lines its inner levels touched only when
that working set is a known number of bytes at most `M`. A working set that still contains a size
variable is assumed not to fit. This is why `matmul(n, ...)` analysed on its own reports
`B·n³ + 8n³ + 8n²` — a conservative bound in which nothing is reused — while the same function
called from a `main` where `n = 1792` reports `8n³ + …`, because there the column of `b` is
`1792` lines, that is a number, it is under `M`, and the walk down consecutive columns is seen to
share lines.

That is the second rule: **a call whose argument sizes are all known at the call site is analysed
again for that call site**, with the parameters bound to the caller's polynomials, so every fit
and stride decision inside is made with the caller's numbers. The function's own line in the
report and in `costs.lock` is still its symbolic, conservative one.

Other moves:

| construct | moves |
|---|---|
| `[e; n]`, `[a, b, c]` | a sequential write of the array: `n × elem_bytes` |
| call | the callee's moves, times the enclosing trip counts — **no reuse across calls** |
| a repeated call in a loop | charged in full every iteration, even when the data would still be in cache |

## What the model does not see

Each of these rounds up, so the reported cost stays an upper bound; each is a place where the
measured number can come in under the prediction.

- **Accesses do not compete.** The fit test looks at one access site's working set as if it had
  the cache to itself. Three arrays each fitting `M` do not necessarily fit together.
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

## Reading a line

```
matmul           work 10·n³ + 6·n² + n             moves B·n³ + 8·n³ + 8·n²           exact
main             work 5.760e+10                    moves 4.622e+10                    exact
fib              unknown: calls `fib`, whose cost is unknown (recursive; recurrences are not solved yet) (line 3)
```

`exact` means both polynomials were derived by the rules above with no unknown. It does not mean
the machine will agree to the byte; it means the shape is proven and the constant is the rules'.
`--eval n=1792,B=64` substitutes and prints numbers.

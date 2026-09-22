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
atoms, `.len()` of a local whose size is known, immutable `let`s bound to size expressions, loop
variables of the loops that are open, and `+ − *`, or `/` by a literal. A loop variable is its
own atom while its loop is open; a bound `i..n` gives the exact trip `n − i`, and the cost of the
body, a polynomial in `i`, is summed over `i` when the outer loop is left.

Machine parameters default to `M = 2 MiB`, `B = 64` and are set with `-M` and `-B`. They are
kept symbolic in the reported polynomial and made numeric only for the two decisions below.

## Work

Work approximates the **instructions the C compiler will emit**. It is not exact — the compiler
fuses, strength-reduces and unrolls — but it is in the same unit as `perf stat -e instructions`,
so `neant measure` can print the two side by side and the ratio means something (a hand `dot`
predicts 6 per element and measures 4.5: gcc fused the multiply-add).

| construct | work |
|---|---|
| literal, variable read, `&x`, `.len()`, `let x = e` | 0 + e — a register |
| `a op b`, `-a`, `!a`, `x as T` | 1 + operands |
| `min(a, b)` | 2 + operands — compare, select |
| `x[i]` (read) | 1 + index — a load; the index arithmetic is counted as written |
| `x = e` | 0 + e; `x op= e` | 1 + e |
| `x[i] = e` | 1 + index + e — a store; `x[i] op= e` | 3 — load, op, store |
| `for i in a..b { body }` | bounds once; then `Σ_{i=a}^{b−1} (2 + body)` — increment, compare-and-branch; exact for triangular bounds |
| `while c { body }` | `trip × (1 + c + body)` |
| `if c { t } else { e }` | 1 + c + max(t, e) — the branches are alternatives |
| call | 2 + arguments + the callee's work — call and return |
| `println` | 1, and the function is `io`; the library call behind it is not modelled |
| `[e; n]`, `[e for x in xs]` | one store per element, plus the loop |

Everything inside a loop is summed over the enclosing loop variables — the product of the trip
counts when the bounds are independent, the exact polynomial when an inner bound mentions an outer
variable. A chain desugars to the same loop a hand-written one would be, so it costs the same.

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
bytes the access moves per iteration of that loop (its coefficient times the element size),
each site's lines are computed by **summing over the loop variable** rather than multiplying
by `t`: the loop variable is a size atom while the loop is open, a cost accumulated in the body
may mention it, and leaving the loop applies `Σ_{v=lo}^{hi−1}` exactly (Faulhaber's formula).
For a rectangular loop the sum is the product; for `for j in i..n` it is `Σᵢ (n−i) = n(n+1)/2`,
not the hull `n²`.

```
for every access site:  lines = 1, contiguous = true
for each loop, innermost first, for every site inside it:
    ws = Σ over the sites inside this loop of their lines      -- they share the cache
    if ws·B is not a known number < M:          lines = Σ_v lines         -- the working set does not fit
    else if s = 0:                              lines = lines             -- same lines every iteration
    else if s ≥ B (or s is symbolic):           lines = Σ_v lines         -- a fresh line every iteration; not contiguous
    else if contiguous:                         lines = lines + Σ_v s/B   -- a contiguous set slides by s per iteration
    else:                                       lines = lines × Σ_v s/B   -- each separate line slides
                                                (a slide of under one line counts as one)
moves += Σ lines · B
```

A working set that varies with the loop's own variable is tested at both ends of the range and
must fit at the larger. A set that changes with the variable is not credited for overlap when the
stride is zero: it is summed, an upper bound.

The two slide rules are the model's geometry. After sequential inner loops a site's lines form one
contiguous region; an outer loop that moves it by less than a line grows the region by the slide,
so a row of `n` doubles read `t` times at an offset of 8 bytes costs `n·8/B + t·8/B` lines, once
plus the slide. After a strided inner loop the lines are separate — a column — and each slides on
its own: `n × t·8/B`. The second is the case M1 measured on the naive product; the first is what a
tiled row does.

The **fit test** is the whole model: a level reuses the lines its inner levels touched only when
the working set of *every* access site inside that loop, added together, is a known number of
bytes strictly less than `M`. The sum is what the M1 experiment forced: a tiled matrix product
whose `a`-panel alone fits was measured re-reading it, because the `b` tiles streaming through
the same loop level brought the total to exactly `M`. Strictly less, because a cache is never
empty of everything else.

When the working set still contains a size variable the test **cannot be decided, and the cost
forks**: the level is computed both ways, and each result carries its condition — `ws·B < M` on
one side, `ws·B ≥ M` on the other. The function's cost is then **piecewise**: a set of pieces,
each a polynomial under a set of conditions, whose value at an input is the maximum over the
pieces whose conditions hold there. Pieces whose conditions contradict each other are dropped —
symbolically when one working set dominates another term by term (sizes are at least one, so
`B·n²` covers `B·n`), and at the machine's `B` and `M` when every condition of a piece is in one
size variable: each is then an interval of that variable, and an empty intersection is an
impossible regime. The conditions themselves stay symbolic; only their feasibility is decided
for the machine, so another cache size may keep more or fewer regimes. Nothing is folded
([decisions.md](decisions.md) §2, §3). The naive product analysed on its own comes out in three
regimes:

```
matmul   moves  24·n² + …    if  8·n² < M                              everything fits
                8·n³ + …     if  B·n + 8·n < M  and  8·n² ≥ M          the column of b fits, the matrix does not
                B·n³ + …     if  B·n + 8·n ≥ M                          nothing fits
```

with the gap to the Hong–Kung bound per regime: 1448× where the column fits, 13033× where it
does not. A concrete `main` decides every test with numbers and has one piece.

`if` is the other source of pieces: its two branches are alternatives, and the cost of the
statement is the **larger** of the two, not their sum — a binary search is one recursive call per
level, a stack machine's dispatch costs its most expensive opcode. Access sites on the two sides
of an `if` are alternatives too, and their lines combine by max in the final total. (Inside a
loop, the working-set sum still counts the sites of both branches: a branch-dependent working
set is bounded by the sum.)

That is the second rule: **a call whose argument sizes are all known at the call site is analysed
again for that call site**, with the parameters bound to the caller's polynomials, so every fit
and stride decision inside is made with the caller's numbers. The function's own line in the
report and in `costs.lock` is its symbolic, piecewise one.

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

## Loops without a range

A `while` gets a trip count in one of two ways, or none.

- **An induction variable.** `while i < e { … i += c … }` runs at most `(e − i₀)/c` times when
  `i` is a mutable `i64` stepped by the constant `c` exactly once in the body and nowhere else,
  `e` is a size expression that the body does not assign, and `i₀` — the last thing assigned to
  `i` before the loop — is one too. `i > e` with `i -= c` is the mirror. The variable then acts
  as a loop variable for the stride rule, so `xs[i]` inside is a sequential access.
- **A declared measure.** `while cond decreasing m { … }` runs at most `m` times, where `m` is
  read *at entry*: mutable locals in it stand for what they were last assigned. `decreasing
  j − i` after `let mut i = 0; let mut j = s.len() − 1` is `s.len() − 1`. The programmer is
  promising `m` goes down by at least one per iteration; the compiler does not check it.
- **Neither**, or a measure that reads memory (`decreasing s.len() − pos[0]`): the function's
  cost is unknown, the message says which, and `neant measure` is the way to a number.

`break` changes nothing: every bound here is an upper bound.

## Recursion

A function that calls itself is a recurrence. The body's own cost `f` is computed with the
self-calls charged nothing; then some **measure** must shrink at every self-call — an `i64`
parameter `p`, `xs.len() − p` for a slice and an index, or `hi − lo` for two indices. The
candidates are tried in that order over every call site, with the arguments substituted for the
parameters, and the first that shrinks everywhere is used:

| every call shrinks `m` … | recurrence | solved as |
|---|---|---|
| by a constant `c`, one call | `T(m) = T(m−c) + f(m)` | unrolled and summed exactly: `Σ_{j=0}^{m/c} f` with every parameter of `m` advanced `j` steps along its shift (Faulhaber) |
| by a constant, two or more calls | `T(m) = a·T(m−c) + f` | **refused**: exponential |
| to `m/b`, `a` calls | `T(m) = a·T(m/b) + f(m)` | per monomial `g` of `f` with degree `d` in `m`, the geometric series over levels: `a < b^d` → `g·b^d/(b^d − a)`; `a = b^d` → `g·(log_b m + 1)`; `a > b^d` → `g·((a/b^d)·m^(log_b a − d) − 1)/(a/b^d − 1)` — exact rationals; `log` is base 2 and `log_b` is exact when `b` is a power of two |

`msum` (two calls on halves, constant body 13) comes out as `13·(2m − 1)`, which is the exact
solution at powers of two; binary search as `17·log(hi − lo) + 17`.

Recursive calls under `if` are counted along the heavier branch, not summed — a binary search is
one call per level, not two. The function's line says `recurrence` instead of `exact`. Mutual
recursion is not solved; the message says so. A recursive callee is never specialised at a call
site: its cost is the solved recurrence in its own parameters, substituted.

## Effects

`io` is inferred: a function that prints, or calls one that does, carries `, io` on its line.
Nothing is declared.

## `#[cost(...)]`

```
#[cost(work_at_most = "a.len()", moves_at_most = "16 a.len()")]
fn dot(a: &[f64], b: &[f64]) -> f64 { … }
```

The bound is written over the function's own size names — `a.len()`, `n` — with `B`, `M`,
`log x`, `√M`, `^k` or superscripts, `·` or juxtaposition for products, `/` for division by one
term. It is checked by **asymptotic dominance**: every term of the inferred cost must be
dominated by some term of the bound — exponent by exponent on every size variable, with a
higher `log` power breaking ties. Coefficients do not count. A bound that fails, or that is
asserted on a function whose cost is unknown, is a build error on every command, with both
polynomials in the message.

## The measured tier

`neant measure f.nt --fn name [--sizes …] [--shape p=n*n,…] [--repeat k] [--cpu c] [--lock]`

For a function the calculus cannot bound, the compiler writes a `main` that builds every
parameter at size `n` — slices filled with `i`, `i as f64`, `i % 251`; integers set to `n`; the
shape of each parameter in `n` given by `--shape` — calls the function, and prints something
derived from the result so nothing is optimised away. It builds that at each size, runs it under
`perf stat` for instructions and L2 refills, and fits `~n^k` to each over the upper half of the
sweep. `--lock` writes the line into `costs.lock`:

```
bfs              work ~n^0.99                     moves ~n^0.26                     measured over n = 10000..2560000
```

A measured line is a fit, not a proof, and says so. When the function does have a static cost,
the table prints the prediction beside every measurement, so `measure` doubles as a check on
the calculus for that one function.

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
matmul           work 10·n³ + 5·n² + 2·n           moves B·n³ + 8·n³ + B·n²           exact
                 lower bound      moves 8·n³/√M    (matrix product, Hong–Kung 1981)   gap 11585× at M = 2 MiB, B = 64
                 `b` moves by 8·n bytes per iteration of the innermost loop: a new line every time (line 7)
                 tile by 256      work ≈ 10.0509·n³  moves ≈ n³/8                        [--apply matmul:tile]
fib              unknown: calls `fib`, whose cost is unknown (recursive; recurrences are not solved yet) (line 3)
```

`exact` means both costs were derived by the rules above with no unknown. It does not mean the
machine will agree to the byte; it means the shape is proven and the constant is the rules'. A
piecewise cost prints its regimes on the lines under the function; `costs.lock` keeps every piece
exactly on the function's one line. `--eval n=1792,B=64` decides the conditions at the machine's
`B` and `M` and prints the applicable piece's numbers. `#[cost]` must hold in every piece.

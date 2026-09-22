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

That is the second rule, and it is about **calls**. A function's signature carries, besides its
work and moves, its **footprint** — for each array parameter, the byte range it touches, computed
exactly from the affine indices and loop ranges when it can be, and marked as the whole array when
it cannot — and its **residue**: the condition under which that footprint is still resident when
it returns (its total working set, including internal arrays, under `M`). A range that is not exact
forfeits the residue: the footprint may be over-approximated for a fit test, never for a credit.

A call composes the callee's signature: the callee's cost, with this call's argument sizes
substituted for its atoms, and its footprint mapped onto this function's arrays through the
views passed. What the callee will read that is **already resident** — left there by the previous
call or loop nest, under the conditions that made it resident — is credited: the callee's moves
are reduced by the overlap, as a conditional piece. After the call, what it left resident replaces
what was. The callee is never re-analysed; `matmul` called with `n = 1984` from a `main` costs, to
the byte, what a re-analysis with `n = 1984` cost before this rule replaced it.

A loop that calls is walked twice: once as written, which costs the first iteration with whatever
was resident at entry, and once more with the residue its own body leaves, which costs every
iteration after the first — only the calls are recounted in the second walk. `for r in 0..20 {
sum(&xs) }` over an array that fits `M` is then one scan and nineteen line-touches, not twenty
scans; over an array that does not fit, twenty.

Other moves:

| construct | moves |
|---|---|
| `[e; n]`, `[a, b, c]` | a sequential write of the array: `n × elem_bytes` |
| call | the callee's moves with its atoms substituted, less what is already resident of its footprint (rule two) |

## Structs, layout and arenas

A `struct S { f: T, … }` with scalar fields is a **value**: passed, returned and assigned by copy,
living in registers, costing nothing to move. What costs is an array of them, and the shape of
that array is the compiler's, because nothing in the language can hold an address into it — there
is no `&ps[i]` and no `&p.x`, only `ps[i]` and `p.x`, which are values.

**A site touches one thing and steps by another.** An access site records the bytes it reads or
writes (`es`) and the bytes its address moves per unit of the index (`stride`). For a scalar array
they are the same number. For one field of a struct array they are not, and the difference is
what a layout costs:

| | `es` | `stride` | a loop over `ps[i].x` |
|---|---|---|---|
| array of structs (AoS) | the field | the whole element | `24·n` bytes: every line fetched, a third of it wanted |
| one array per field (SoA) | the field | the field | `8·n` bytes: one stream |

The slide rule then charges the difference by itself: a stride-24 walk over `n` elements covers
`24·n` bytes and so `24·n/B` lines, whatever it reads from each. Under SoA the field arrays are
modelled as laid end to end, so field `f`'s addresses start at the size of the fields before it;
their ranges are disjoint, and every rule about ranges — footprint, residue, disjointness — holds
with no special case. A whole element read under SoA is a gather: one site per field.

**Two accesses to one address are one site.** `ps[i].x` and `ps[i].y` in the same statement, or
`a[i]` twice in one expression, touch the same lines; the second is a hit. Sites on the same array
inside the same loops merge when their indices are equal — term by term when affine, and as
expressions when not, so that `nodes[i].val` and `nodes[i].next` in a pointer chase are one
element. Without this an array of structs would be charged once per field read and no loop reading
a whole element could ever prefer that layout.

**The layout is chosen, not declared.** For each struct type the program has one layout, and it is
the one under which the program moves fewer bytes: the module is analysed twice per type, every
function that touches the type is costed under each, and the totals are compared at a reference
point (this machine, every size a million). Types are decided in declaration order, each with the
others at their current choice. `#[layout(aos)]` / `#[layout(soa)]` fixes a type and the model is
not asked. A tie is AoS, and the report says the model did not decide. The choice heads the report
and the lockfile, because every moves line below it rests on the choice:

```
struct Particle  layout SoA  decided by step (32·n + 4·B, AoS 32·n + B), kinetic (16·n + 2·B, AoS 32·n + B), …
```

**Arenas and the region rule.** A linked structure here is a struct array whose links are indices
into it. There is nothing to infer about where a node lives: it lives in the arena. What the model
adds is a bound on the walk. A site whose index is not an affine function of the loops — `i` loaded
from memory — costs a fresh line per step, but never more than the arena itself, because once every
line of the arena has been touched nothing more is fetched, whatever the order:

```
sum_list   moves 16·nodes.len()     if 16·nodes.len() < M      -- the arena, once
           moves B·nodes.len()      if 16·nodes.len() ≥ M      -- a line per step
```

The two pieces are the ordinary fork on a fit test, under the arena's own condition, and several
sites walking one arena pay for it once. The rule applies when the walk is at least as long as the
arena, which is a comparison between a trip count and a number of lines and so is decided at this
machine's `B`. When the trip count and the arena are different sizes — a ring buffer written
`xs.len()` times into `buf` — the rule cannot fire, because a condition in this calculus compares a
working set with the cache and not two size expressions with each other. The model says the walk
costs a line per step, and says it rather than guessing.

**Owned arrays.** `[T]` as a return type is an array the caller owns. The callee's signature
carries the size of what it returns, over the callee's own size atoms; a caller binds the result,
becomes its root, and costs its loops with that size substituted through the call's arguments —
so `doubled(&xs)` hands back something of `xs.len()` elements and the loop over it is exact. A
size the callee cannot express leaves the caller's array unknown, and loops over it go to the
measured tier, as everywhere else.

## Where these rules stand

For affine loop nests there is an exact answer to the question these rules approximate: Bao,
Krishnamoorthy, Pouchet and Sadayappan (POPL 2018) count cache misses in closed form for
polyhedral programs by computing, for every access, the set of distinct lines touched since the
last touch of the same line — the reuse distance — as a count of integer points in a polyhedron,
parametric in the problem sizes and the cache size, for set-associative caches. The slide rule, the
contiguity flag and the fit test here are a rule-based approximation of that computation, exact in
the cases M1 measured and coarser elsewhere (a slide over a region that is not contiguous, two
accesses whose lines overlap by accident). Where a nest is affine, the honest replacement for these
rules is that computation, and the plan holds a place for it. What the rules cover that the
polyhedral count does not is everything around the nest: calls with footprints and residues,
`while` with a measure, recursion, data-dependent access under the region rule, and a whole
function's cost as one object a caller composes. A third quantity is not what these rules compute
at all: IOUB (Olivry et al., PLDI 2021) bounds the I/O complexity from above with the best tiled
schedule of a rectangular band, which is a statement about the computation, not about the program
as written. The rules here answer for the program as written.

On the other side of the gap, lower bounds: IOLB (Olivry, Langou, Pouchet, Sadayappan, Rastello,
2020) derives them automatically for any affine program, and `neant cost --iolb` obtains them
from it (§ Lower bounds). The one hand entry in `bounds.rs` is the fallback when the tool is
absent, and the stronger statement where the tool does not see through a tiled nest.

## What the model does not see

**Partial fits.** The model is the ideal cache: optimal replacement, a step at `M`. Where a
working set fits entirely it agrees with the machine to within the constants M1 calibrated.
Where only part of a level's working set fits — one tile resident while two stream — the ideal
cache keeps the resident part and the model's regime says so; an LRU-like machine does not keep
it, and the tile-side sweep measured the difference as an order of magnitude (docs/experiments.md).
The regimes the model prints in that situation are correct for the ideal cache and optimistic for
the machine; the tile choice compensates by deciding fits at `M/2` and preferring the side with
the smaller working set, and nothing else in the calculus does yet.

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

## `#[cost(...)]` — declarations first

```
#[cost(work_at_most = "12 xs.len()", moves_at_most = "8 xs.len() + 2 B")]
fn total(xs: &[i64]) -> i64 { … }

#[cost(moves_at_most = "4096", sizes = "xs.len() <= 256")]
fn small_sum(xs: &[f64]) -> f64 { … }

#[cost(work_at_most = "4", moves_at_most = "0")]
extern fn labs(x: i64) -> i64;
```

A bound is written over the function's own size names — `a.len()`, `n` — with `B`, `M`, `log x`,
`√M`, `^k` or superscripts, `·` or juxtaposition for products, `/` for division by one term.

**The declaration is the function's line.** When a function declares its cost, that declaration is
what `costs.lock` records and what the report shows first; the inferred cost is printed under it
as `inferred … within the declaration` or `outside`, and outside is a build error on every
command. A function without a declaration has its inferred cost as its line, as before.

**Two ways to check.** Without `sizes`, by **asymptotic dominance** in every regime: each piece of
the inferred cost must be dominated term by term by the bound, `log` powers breaking ties,
coefficients ignored. With `sizes = "n <= 512, a.len() <= 4096"`, as **numbers**: the inferred
cost is evaluated at those size bounds and the machine's `B` and `M` — regime conditions decided,
the applicable pieces' maximum taken — and must not exceed the bound evaluated the same way. This
is a budget in real units (`moves_at_most = "4096"`), which the asymptotic check cannot express.

**A caller sees only the declaration.** When a callee is declared, the caller composes the
declared work and moves and nothing else: no footprint, no residue, no credit. Delete the callee's
body and keep its declaration — `extern fn` is exactly that — and every caller checks the same.
That is the property that makes separate compilation, binary distribution and interface budgets
possible, and it is a golden test (`decl.nt` against `decl_extern.nt`).

**`extern fn`** has no body. Its cost is its declaration; without one it is unknown unless declared
`uses unbounded`. It names the C symbol itself (`labs` is libc's), carries its effects as `uses io,
unbounded`, and is the boundary stage C measures and audits.

## What a line rests on

A line's tier says how its numbers were obtained: `exact` and `recurrence` by the calculus,
`declared` by a person, `measured` by the machine. A function that composes a declared callee
rests on that declaration, and says so — `rests on labs (declared, extern)`, or `(declared,
checked)` when the callee has a body the compiler verified against its declaration — transitively
up the call graph. The tier column is therefore an audit chain: every number in `costs.lock` is
traceable to the model, to a declaration someone wrote, or to a measurement someone ran, and
nothing rests on nothing.

`neant measure --fn labs` on a declared function does not fit a curve; it **confirms the
declaration**. The driver is built twice, with the call and with a constant in its place, and the
difference per call — instructions and refills — is compared to the declared work and moves at
each size, within the counter's known factors — work up to 1.5× + 2, moves up to 2× + one line
per call, because reads pair, prefetch overfetches and process noise divides by the repeats. The
verdict, `confirmed` or `EXCEEDED`, and the range it was measured over go into the lockfile with
`--lock`, and `neant lock` keeps that annotation when it regenerates the file.

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

The report says how far a function is from what *any* program computing the same thing must
move. Three bounds are derived, two by the compiler itself and one by an external tool; all are
lower bounds on words touched, and words move inside lines, so each bounds the calculus's line
traffic. The report prints them strongest first — a bound that dominates another asymptotically
goes before it, and between two that do not, the larger at this machine with every size at a
million — and the gap of a rewrite is measured against the first. A bound that is a number and
not positive at this machine (`216/√M − M` for a 3×3 product) says nothing and is not printed.

**Footprint (native).** Every distinct element of a parameter array the function reads was in
slow memory when the function was called and crosses at least once; every distinct element it
writes crosses back. For each array reference whose index is an injective affine map of the loop
variables (below), the size of its image is the count of the loop variables it mentions, exact by
summation when their bounds mention no other loop; per array the largest image among its
references is taken, and the sum over parameter arrays, in bytes, is the bound. It is tight on
every streaming kernel (`sum`, `dot`, `saxpy`: gap 1×) and on the product where everything fits.
Arrays born inside the function do not count: they need never cross.

**HBL (native).** For a statement inside a loop nest, take its array references as maps from
the loop variables to array elements. When each is injective on the loop ranges it behaves as the
coordinate projection `π_D` onto the loop variables `D` it mentions, and the discrete
Brascamp–Lieb inequality (Bennett–Carbery–Christ–Tao; Christ, Demmel, Knight, Scanlon, Yelick
2013, §6 for coordinate projections) gives `|V| ≤ ∏_j |π_{D_j}(V)|^{s_j}` for every finite set
of iterations `V`, whenever `|S| ≤ Σ_j s_j·|S ∩ D_j|` for every subset `S` of the loop variables.
Cut any execution into segments of `S` words moved: within a segment each array has at most `2S`
accessible elements, so a segment runs at most `(2S)^σ` iterations with `σ = min Σ s_j`, and the
`|I|` iterations need at least `|I|/(2S)^σ` segments.

```
  Q  ≥  |I| / (2^σ · S^(σ−1))  −  S     words      (S the cache in words; no recomputation)
     =  |I| · 4^σ · M^(1−σ)    −  M     bytes      (8-byte words, S = M/8)
```

`σ` is found by an exact rational LP, the optimum enumerated over its vertices; `|I|` is the
exact iteration count by summation, so a triangular nest is counted as a triangle. The product
has `σ = 3/2` and gets `8·N/√M − M`: Hong–Kung with Irony–Toledo–Tiskin's constant, derived
rather than written down. Details that make it work on code as written:

- *Injectivity* is decided by the mixed-radix test: order the loop variables by the span each
  contributes to the index (`|coefficient·step|`), and each coefficient must reach past the whole
  span of the smaller ones, for all sizes ≥ 1 — `i·n + k` with `k < n` passes, `i + j` does not.
  A reference that fails leaves its statement without an HBL bound; `a[i + j]` is IOLB's.
- *A scalar accumulator is the element it is stored into.* `let mut acc = 0; for k { acc += … };
  c[i·n + j] = acc` is `c[i][j] += …` with the element kept in a register, the same computation;
  a scalar stored to or loaded from an array element by a statement of the enclosing block stands
  for that element inside the block, and a statement using it references the element. Without
  this the product's inner statement sees only `a` and `b` and gets `σ = 2`.
- *Repetition loops* — loops no reference mentions and no inner bound depends on — are left out
  of `|I|`; a loop an inner bound depends on stays in, and if no reference mentions it the LP is
  infeasible and there is no HBL bound, which is right: repeating a computation proves nothing
  about what the repetitions must move.
- *Handing up.* A bound on a callee is a bound on the caller, once — repeated calls are not
  multiplied, since a bound on any schedule of a part says nothing about what repetitions may
  share. A footprint bound travels to a caller only for arrays the caller itself received.
- The bound sees through a tiled nest: the six loop variables of the tiled product project onto
  the three arrays with the same `σ = 3/2`.

**IOLB (tool).** IOLB (Olivry, Langou, Pouchet, Sadayappan, Rastello, PLDI 2020) derives
lower bounds for affine programs with better constants — Smith–van de Geijn's `2·N/√S` for the
product, `4·√2 ≈ 5.66×` the HBL constant above — and handles references the native bound
refuses. `neant emit --scop f` writes `f` as the C its front end (PET) reads: a loop nest with
affine bounds and indices between `#pragma scop` and `#pragma endscop`. A flat row-major index
`i·n + k` is not affine in the polyhedral sense, so an array every one of whose indices has the
form `v·row + w` is **delinearised** into a two-dimensional parameter `double a[a_rows][row]`.
Immutable scalars bound to a literal are written as the literal. A **tiled nest is untiled**
first — a lower bound is a property of the computation, not of the loop order, and IOLB does not
see through the tiles — with the divisibility assumption (`64 | n`) carried on the bound, because
a floor in a loop bound makes IOLB's search run for hours. A call, a `while`, an array born
inside the function or a data-dependent index refuses the export with the reason, which the
report carries as a note. `neant cost --iolb` runs the command in `NEANT_IOLB` (`{file}` standing
for the export; `tests/kernels/iolb.sh {file}` runs the docker image from a checkout in
`IOLB_DIR`, under a wall-clock limit `IOLB_TIMEOUT`), takes the asymptotic bound from the
second-to-last line of its output — a GiNaC expression in the parameters and `S` — and converts
words to bytes with `S = M/8`, keeping an irrational coefficient to four decimals:

```
2·n³/√S words  =  16·√8·n³/√M  ≈  45.2548·n³/√M bytes
```

**The gap** is the ratio of the function's leading moves term to the bound's, at the machine's
`B` and `M`, when the size variables cancel, or the plain ratio when both are numbers, per
regime. Under the bounds, the report says what each operand of a multiply-accumulate does in
the innermost loop when it moves by a whole line or more per iteration: that access is the one
paying for the gap.

## Rewrites

A function with an HBL bound (an exponent above one: reuse to be had) has two rewrites tried on it; each is costed by running the
calculus on the rewritten IR, and the result is printed as a suggestion with the `--apply` flag
that performs it. Nothing is applied silently. Both work on the naive shape the catalogue
recognises — `for i { for j { let mut acc = 0; for k { acc += A·B } C = acc } }` — and are
refused, with a sentence, on anything else.

- **`name:tile`**, **`name:tile=T`** — the three loops are split into tiles of side `T`. The
  tile loops accumulate into `C` across the `kk` tiles, so `C` is cleared first; the result is
  the same product. Tile edges use `min(ii·T + T, n)`, which the calculus reads as `T`. Without
  `=T` the side is the one the model chooses (below), or, when it cannot, the largest power of
  two with three `T×T` tiles strictly inside `M`.
- **`name:transpose`** — the operand walked down a column (the one whose index carries the
  innermost variable times a row length) is copied transposed before the nest, and the inner
  loop reads the copy along a row. The copy costs a transpose and `8·nk·nj` bytes of memory.

**The tile side is read off the model.** The tiled program is analysed once more with its side
`T` as a size variable — a fresh `i64` parameter, the tile count `n / T`, full tiles — and its
moves come out piecewise in `T`: fifty regimes for the product, each under fit conditions some
of which bound `T` from above (`8·T² < M`: a tile fits; `32·T² < M`: all of them do). In a piece
whose moves fall as `T` grows — every power of `T` non-positive — the best side is the largest
the conditions allow, so each such condition is solved for `T` at its boundary and substituted;
the piece's other conditions, substituted too, must stay feasible and must hold at the reference
point (this machine, every size a million: tiling is for large sizes). Among the candidates the
smallest moves at the reference point wins, and among candidates within five percent of each
other the **smaller side**. Three rules narrow this, and all three came from the machine rather
than from the model (docs/experiments.md, the tile-side sweep):

1. **A partial fit is not a candidate.** A regime that depends on something *not* fitting — one
   tile resident while the other two operands stream — is one the ideal cache computes and an
   LRU machine does not deliver. Two working sets of the same degree in `T` belong to the same
   loop level, so if one of them does not fit, the side is a partial fit and is refused.
2. **The boundary is half the cache.** A fit the ideal cache decides at `M` is not one an LRU
   cache of `M` honours; one decided at `M/2` is (Sleator–Tarjan), and the octave-wide transition
   M1 measured is the same fact.
3. **Ties go to the smaller side.**

The model's first answer for the product was `T < √(M/8)`, 510, the one-tile regime, which in the
ideal cache ties the regime where everything fits (`90.5·n³/√M` either way); the machine moved
thirty times the prediction at 510, matched the prediction at every side with all tiles inside
`M`, and moved the least at 181, the side at which everything fits in half of `M`. The expression
is printed from the working set's leading term in `T` when there are edge terms, and says so; the
integer recommended is the largest for which the working set, edge lines included, is strictly
below `M/2`. The report then shows the tiled cost at
that side, symbolically, and the concrete `tile by T` line re-analysed at the integer, with the
gap of each against the strongest bound. This is the upper side of the I/O question — what the
best tiling of this loop order moves — from the model's own exact cost rather than a separate
cost formula, in closed form where IOUB solves numerically, and corrected by the counters.

For the product at `M = 2 MiB` the answer is the side at which all three tiles fit half the
cache, `T < 0.1443·√M`, which is 206 once the edge lines are counted. It moves `110.9·n³/√M`:
`14×` above the HBL bound, `2.5×` above IOLB's, and on the machine `19%` fewer bytes than the
square of 256 a rule of thumb picks and `6%` more than the best of the seven sides measured.

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

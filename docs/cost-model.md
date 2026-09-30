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
`a.len()`, an `i64` parameter `n` contributes `n`. The other kinds of atom are a value read from
memory (§ A size read from memory), a value bound once to an immutable local (§ A size bound
once), and program input (§ Program input) — every size inside the body must reduce to these and
constants, or the function's cost is unknown.

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
impossible regime. Term by term means the argument is about `q − p ≥ 0`: its positive terms are
a budget, and every negative term must be covered by one with at least its exponents — `q`'s own
negative terms included, which is what makes `32·i − 32 ≥ 32·i` false. The least element of an
array is at most its most, so `n·max(xs[_])` covers `n·min(xs[_])`; and a difference in one
integer size is decided exactly, below the bound on its roots by evaluation and above it by its
leading sign, which is how `8·n² − 32·n + 32 ≥ 0` is known. The conditions themselves stay symbolic; only their feasibility is decided
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
Exact means every byte of the range was read. Two ranges on one parameter are one exact range
only when they overlap or touch — two fields of one element under SoA are `8·n` bytes apart, and
the span between them was never read — and a range with an end at the most or least an array
holds is a hull over elements, not a range, so it is not exact either. A loop's range extends
from its first iteration toward the sign of `coefficient · step`: a loop counting down reaches
lower addresses.

A call composes the callee's signature: the callee's cost, with this call's argument sizes
substituted for its atoms, and its footprint mapped onto this function's arrays through the
views passed. What the callee will read that is **already resident** — left there by the previous
call or loop nest, under the conditions that made it resident — is credited: the callee's moves
are reduced by the overlap, as a conditional piece. After the call, what it left resident replaces
what was. The callee is never re-analysed; `matmul` called with `n = 1984` from a `main` costs, to
the byte, what a re-analysis with `n = 1984` cost before this rule replaced it.

A loop that calls is walked twice: once as written, which costs the first iteration with whatever
was resident at entry, and once more with the residue its own body leaves, which costs every
iteration after the first — only the calls are recounted in the second walk. What the second
walk starts from is the residue of the iteration before, so it is shifted back one step in the
loop's variable: a call that reads `xs[i]` finds `xs[i − 1]` resident, not `xs[i]`. `for r in 0..20 {
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

A field may be a fixed-size array `[T; k]` (docs/arrays-by-value-design.md), and then the value
is not all registers: writing the field — `[a, b, …]` or `[e; k]` in a literal — costs `k` stores
and `k·elem` bytes, as `let xs = [a, b, c]` does, and so does every copy of the struct where the
program names a second place for it (`let t = s`, `t = s`, a by-value argument); `s.x[i]` is a
load with no bytes, the value being resident. A struct with an array field is never an array
element.

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

**A cost holds where every range it counts is non-empty.** `for i in a..b` counts `b − a` and a
`while` counts its distance to the bound, with no `max(·, 0)`: where the range is empty the formula
may read negative and the loop in fact costs nothing. Conditions on regimes are working sets only,
so the clamp is not expressible yet (plan § Stage D, the sign audit).

A `while` gets a trip count in one of two ways, or none.

- **An induction variable.** `while i < e { … i += c … }` runs at most `(e − i₀)/c` times when
  `i` is a mutable `i64` stepped by the constant `c` exactly once in the body and nowhere else,
  `e` is a size expression that the body does not assign, and `i₀` — the last thing assigned to
  `i` before the loop — is one too. `i > e` with `i -= c` is the mirror. `i <= e` and `i >= e`
  run the bound too, `(e − i₀)/c + 1`: `i >= 0` from `n − 1` is `n` laps. The variable then acts
  as a loop variable for the stride rule, so `xs[i]` inside is a sequential access.
- **A declared measure.** `while cond decreasing m { … }` runs at most `m` times, where `m` is
  read *at entry*: mutable locals in it stand for what they were last assigned. `decreasing
  j − i` after `let mut i = 0; let mut j = s.len() − 1` is `s.len() − 1`. The programmer is
  promising `m` goes down by at least one per iteration; the compiler does not check it.
- **A walk down a list.** `while s >= 0 { …; s = xs[s].f }` follows links, not a count, and runs
  at most the longest walk along them, `walk(xs[_].f)` (§ A walk down a list).
- **Neither**, or a measure that reads an array the body writes (`decreasing s.len() − pos[0]`
  with `pos[0] += 1` inside): the function's cost is unknown, the message says which, and `neant
  measure` is the way to a number.

`break` changes nothing: every bound here is an upper bound. Nor does `&&`: `while a && b` stops
no later than either conjunct would, so the first conjunct that has a trip count bounds the loop.
The condition is evaluated once more than the body runs, and the memory it reads is charged each
time; its work is the loop's compare-and-branch, as it always was.

**Reading the condition (2026-09-27).** A `while` condition is read as a variable against a
bound. When both sides were locals the reading took the right-hand one whenever the operator was
`>`, so `while j > start { …; j -= 1; }` was a loop in `start`, which is not mutable, and was refused;
and `while i + 1 < n` had no variable alone on either side. Now the variable is the local the body
assigns, and `i + c` against `e` is `i` against `e − c`. On the compiler, 92/49/35/102 becomes
94/51/35/98 — insertion sorts and `i + 1 <` scans — and two callers of the newly exact
`mono_mul` and `mono_with` become unknown in turn, because those callees' costs depend on an
argument the callers pass as a read inside a loop that writes it (`terms[ta].n_fac`). Charging such a
callee as a term over `_`, as an unknown one is, was tried and costs the compiler minutes instead of
seconds — terms nest along every chain of calls — so it is not done (golden `whileshapes`).

## A scan

**Written 2026-09-26, before the code.** Text is read by a loop whose index advances by what it
read:

```neant
while i < xs.len() {
    let r = next_int(xs, i);      // r.end is at least i
    …
    i = r.end + 1;                // so i grows by at least 1 a lap
}
```

and by a loop that steps by one but starts where an earlier loop left off, the second loop of
`next_int`. Neither is an induction variable (§ Loops without a range). The first is not stepped by
a constant, and the second has no known entry value. Both are bounded all the same, by the same
argument a `decreasing` measure is.

**The rule.** `while i < e` or `while i <= e` runs at most `(e − i₀)/d` times (`+ 1` for `<=`)
when three things hold:
- `e` is a size expression the body does not assign;
- along every path through the body that comes back to the condition, `i` grows by at least `d`,
  with `d ≥ 1`;
- `i₀` is a lower bound on `i` at entry.

It is the `decreasing e − i` argument, proved rather than promised. If `i₀` is `i`'s one entry
value, it is that. If not, and every assignment to `i` in the whole function is an increase, `i` is
at least what it was first bound to, and that is `i₀`: `let mut i = start` gives `start`.

**What "grows by at least `d`" means.** The body is walked once, path by path, as the check on a
`decreasing` measure is. What the walk accepts:
- `i += c` and `i = i + c` grow `i` by `c`. `i -= c` shrinks it.
- `i = x + c` grows it by `c + k` when `x` is known to be at least `i + k` at that point.
- An `if` takes its smaller branch. A `break` or `return` leaves, and a path that leaves need not
  grow `i`.
- A nested loop may run zero times, so it counts `0` and must not shrink `i`.
- Any other assignment to `i`, or one inside an expression the walk does not open, and the rule
  does not apply.

"`x` is at least `i + k`" comes from:
- `let x = e`, immutable, with `e` built from `i`, constants and `+`;
- a field `r.f` of `let r = g(…)`, from `g`'s summary.

A fact about `i` stops holding at the next assignment to `i`.

**The summary.** For every function it lists what it guarantees about what it returns: the
result, or a field of the struct it returns, is at least one of its `i64` parameters plus a
constant, or at least a constant. It is read off the returned expression, the body's tail, in a
function with no `return`:
- a parameter, a constant, or `+ c` of either;
- a mutable local whose every assignment is an increase, which is at least what it was bound to;
- a field of a callee's result, by the callee's summary.

`next_int` returns `Num { …, end: i }` with `let mut i = start` and every assignment to `i` a `+= 1`,
so its summary is `end ≥ start`. `after_header` returns `i + 1` from `let mut i = 0`, so it
returns at least 1. Summaries depend on callees' summaries, so they are computed to a fixed point
over the call graph, from nothing, as the fields a call may write are (§ A walk down a list).
A cycle only ever adds facts that hold.

**What the report says.** The loop's trip is an upper bound reached by an argument about values,
not a count. So the line is `bound`, not `exact`, and a note says which loop and why: ``scan: `i` grows by at least 1 a lap, so the `while` at line 41 runs at most xs.len() times``. A caller whose
cost rests on such a line is `bound` too, and its line says `rests on next_int (bound, a scan)`.
The scan's index is not an induction variable, so its accesses are charged as not affine: a line
each (§ Moves). That is an upper bound too.

**Where it refuses.**
- `!=`, where a step larger than one can jump the bound.
- A descending scan.
- A bound the body assigns.
- An assignment to `i` the walk cannot follow: `i = xs[k]`, `i = f(i)` with no summary.
- A path that can come back without growing `i`: "`i` does not grow on every path through the
  body".
- An entry value that is neither known nor bounded below because some assignment to `i` shrinks
  it: "`i`'s entry value is not known and `i` is not only increased".

**A loop that surely runs (2026-09-27).** A lexer moves its index only inside the nested loops of
its branches — `if is_alpha(c) { while i < n && is_alnum(xs[i]) { i += 1; } }` — and a nested loop
may run zero times, so the walk above counted it as growing `i` by nothing and refused. It now counts
one lap of a nested `while` when the path surely enters it: `i` has not moved this lap, so every
conjunct the enclosing loop's condition shares still holds, and every other conjunct is a predicate
of the byte `xs[i]` (directly or through `c = xs[i]`) that the guards on the path imply for each of
the 256 values the byte can take — the guards and the predicate read by evaluating them, calls to
byte predicates like `is_alnum` included. A guard not taken is used only when all of it can be read,
since the negation of a part says nothing. And a mutable local bound inside the lap and only ever
increased, `let mut j = i + 1`, is at least its initialiser, so `i = j + 1` grows `i` by 2; one bound
anywhere in the function and only increased is at least its initialiser's least. The compiler's own
`lex` is a bound for the first time (`6·n² + …`: its string branches' inner loops are charged the
rest of the text a lap, an inline amortisation not done); golden `lexer`, with `stuck` refused — its
guard, a digit, does not imply the letter its loop wants.

## Neighbouring sites

**Written 2026-09-27, for the stencil.** § Moves adds up the lines of every site. A five-point
stencil reads `src[(i − 1)·n + j]`, `src[i·n + j]`, `src[(i + 1)·n + j]`, `src[i·n + j − 1]` and
`src[i·n + j + 1]` — five sites, five streams, where the machine reads three rows, each once, while
they are near each other in cache. The lower bound said so: `16·n²` against `48·n²` charged.

**The rule.** At a loop, sites on one array — the same field, nest and branch, the same coefficient
on every loop variable — whose constant parts differ by whole laps of this loop are a group: the
one ahead reads now what the others read one or two laps later. When the working set of `span + 1`
laps of this loop fits in `M` — the loop's own working set per lap, as § Moves computes it, times
the laps between the first member and the last — the group costs one member in full plus the
lines of `span` laps, once, and the others nothing. Where the window does not fit, each member is
its own stream, as before; the report forks on the condition like any other.

Two guards keep a line from being saved twice: a group is formed at the loop whose laps its offsets
are, and only if (where both are numbers) the offsets are less than one lap of the loop outside;
and a site already charged less by a group further in joins no other. The stencil's `j ± 1` group
at `j` and its `i ± 1` group at `i` are two streams, and `dst` the third: `24·n²` against the lower
bound's `16·n²`. The centre read, grouped at `j`, is not also counted as sharing the rows at `i`,
which is where the remaining factor of 1.5 is.

Measured on `heat` (the corpus's stencil) the prediction stays above the machine's refills, at
0.05 and 0.34 of it at `n` = 300 and 900, and its time goes from 0.26 to 0.50 of the predicted.

**A triangle (2026-09-28).** A site that does not move with a loop re-reads the same lines every lap,
and § Moves charges them once. When its inner range moves with that loop — `for j in i + 1..n` —
each lap re-reads a subset of the lines of the range's hull over every lap, and they were summed
lap by lap: n-body's `advance` over five bodies was `≈ 28·n²` bytes in the regime where the whole
array fits. Where the loop's working set fits, such a site is now charged the lines of that hull
once, the range taken at the loop's first and last lap and the one that dominates chosen at each
end; where it does not, or neither end dominates, lap by lap as before. `pairs` in golden `tri` goes
from `4·n²` to `32·n + 2·B − 8` in cache, against the lower bound's `8·n`; the self-hosted pass
still sums (`TRIANGLES_INSTEAD`).

**A triangle's fit test (2026-09-29).** The inner loop of a triangle, `for j in 0..i + 1`, has a
working set in `i` — a row of `i + 1` elements, `B·i + 8·i < M` — and its fit test was made there
and carried outward as it was: once `i` was summed away its regimes still named it, so a caller
had an atom `kernel_syrk.i` in its conditions that no `--eval` could give. Eight of PolyBench's 30
kernels were exact in form and had no prediction for it (evaluation § 5). The test is now decided
over the laps when the loop of `i` is summed (`piece::split_conds`, in `Cost::sum_split` and in
`settle_moves`' combination of an inner loop's alternatives): a working set linear in `i` is
largest at one end, so a test that fits there fits in every lap and that piece's sum is exact;
one that does not fit there fails in the last laps and perhaps not in the first, and the piece
that charges no reuse, summed over every lap, is at least what they cost, a bound. The function
says `regime: …` and is `bound`, and a caller rests on it `(bound, a regime)`. One condition
becomes one, so the regimes are as many as before; a first attempt that kept both ends as
conditions and a third piece for the switch multiplied them (cholesky 4 → 566, symm past 300 s).
Golden `triangle_regime`.

Two older faults that the conditions in `i` had hidden came out once those regimes could be
evaluated, and are fixed with it. A triangle's hull (§ A triangle) is the lines of every lap of
its loop, the slide included, and was slid again: a column read under `j in 0..i` was `n²·n²`,
lu's and ludcmp's `64·n⁴/(3·B)` in the regime where the row fits, and the sum the hull falls back
to when its ends cannot be ordered was slid the same way (nussinov's `n⁴`). Neither is slid now.
And the hull was taken with the ranges of the loops inside it still in their own variables — `k in
0..j` leaves `j` in the range at the loop of `i` — so a caller was handed `first.j`; each inner
variable is now taken at its own extreme first, innermost out. `pairs` in golden `tri` goes from
`32·a.len()` to `24·a.len()` in cache, against the lower bound's `8·a.len()`. n-body's `advance` goes from
`272·bs.len()` to `216·bs.len()` and `energy` from `160·bs.len()` to `128·bs.len()` where the bodies fit,
against `56·bs.len()`; the layout chosen is still SoA.

**A triangle's hull, in the loops' own values (2026-09-30).** nussinov's inner walk,
`t[(k + 1)·n + j]` for `k in i + 1..j` inside `j in i + 1..n` inside `i` counting down, was
charged lap by lap wherever a column fit, `B·n³/6`, five times its counter. Four faults, each
found by the one before it. The hull was taken from `site_range`, which adds each loop's span
apart and so counts `j` twice when it is also the trip of the loop inside (a hull `8·n² + 8·n·j`
for what is at most `8·n·j`); it is now the index in the loops' own values (`Site::raw`), each
variable taken at its own extreme, innermost out. An end was put at its extreme only when the two
laps' values could be ordered, which `8·n·j` against `8·n·i` cannot be without `j ≥ i`; an end
linear in the variable now goes by its coefficient's sign (`piece::direction`). A step between laps
that is a size (`(n + 1)·8`) sent the site to the lap-by-lap sum before any hull was tried; a
triangle's laps now try the hull whatever the step. And the fit test of a loop counting down took
its working set at the first lap as if it were the least, which for `i` from `n − 1` is the most:
a set that shrinks with `i` was tested at its smallest, `−16·n/B + 13`, found to fit, and charged
as if the table fit — 124 times under the counter for the moment it stood. The least and the most
are now the step's. A hull is also no more than the laps' own sum where that is less at this
machine's `B` (a triangle's bounding rows `8·n²` against the rows it reads, `4·n²`). nussinov goes
to 1.63 / 0.41 / 0.78 of its predicted time and 0.50 of its bytes at EXTRALARGE; `tri`'s `pairs`
from 24 to 16·a.len() in cache, n-body's `advance` from 216 to 160·bs.len(), `whileshapes`'
insertion sort from quadratic in its range to linear, and the compiler's `mono_mul` and `mono_with`
lose their quadratic moves (95 exact, 45 modulo, 44 bound, 94 unknown). Golden
`modules/triangle_down`, among the modules because the self-hosted pass costs a loop counting down
its own way.

**Footprints of several ranges, and a resident call (2026-09-28).** A parameter's footprint was one
range, so two SoA fields of one element — ranges `8·n` apart — fell back to the whole array, inexact,
and no caller was credited for either. A parameter now keeps its disjoint exact ranges apart; a range
whose ends move with an outer loop is widened to its hull over that loop's laps, each end at the lap
where it is furthest out, and clamped to the region the site can touch (its field's under SoA) —
without the clamp the two ends, taken at different laps, overran into the next field. And a call
whose every footprint range is already resident, to a callee with no array of its own, moves nothing
where those residues hold: what the credit leaves above zero is set to zero, while a credit already at
or below zero — the cross product cancelling an earlier call's charge, `dot(&xs, &xs)` — stands. On
n-body the steps loop's calls to `advance` move nothing per step, which the machine confirms: its L2
refills are the same at 10³ and 10⁶ steps. A square root is counted as a division.

## A scan's accesses

**Written 2026-09-27, after the corpus was timed.** § A scan bounds a scan's *trip*, but its index
was still no induction variable, so every `xs[i]` in its body was not affine and § Moves charged it a
line: 64 bytes for one byte of text, a line per access and never the stream it is. And since a
non-affine site's footprint is the whole array, a parser handed a start was charged a cold read of
all of the text at every call. In the corpus that made the text readers' moves quadratic and their
predicted time 10³ to 10⁶ too high.

**The rule.** A `while` bounded as a scan, with an integer least growth `d`, gives its index an
affine form *for access sites only*: at lap `k` the index is at least `i₀ + d·k` and below the
bound. Because it only grows, the lines it visits are visited in order, each once while it is being
passed; so the lines the site touches over the loop are at most those of a stream at stride `d`
over `[i₀, e)`, which is what § Moves computes from that form — `es·(e − i₀)/B`, or one a lap when a
lap moves a line or more. A jump makes fewer laps, not more lines. Three places never read the form:
- **Sizes.** The index's value is not `i₀ + d·k` but at least that, and a cost that grows with it
  would be under-charged; `size_of` ignores the loop, as it did.
- **Lower bounds.** The trip is an upper bound; an HBL or footprint *lower* bound built on it would be
  inflated, so a scan's loop is left out of the nest they are counted over.
- **Residue and exactness.** A jump skips elements, so the hull `[i₀, e)` is a range the scan stays
  in, not one every byte of which was read: the footprint is not exact, no caller is credited for it,
  and the residue a lap leaves is not assumed to be one step back.

Scanners nest `while`s on one index; an access reads the innermost loop that moves it. `next_int`'s
moves become `2·(xs.len() − start) + 3·B` in one regime, which has the distance form § An amortised
scan needs, so a loop of calls is charged its text once in moves too.

## An amortised scan

**Written 2026-09-27.** A scan bounds a loop that calls a parser once a lap, but it charges each
call the parser's whole-range cost: `next_int(xs, i)` costs `9·(xs.len() − start) + 29`, the
caller's `i` is taken at its least, `0`, and `count_ints` came out at `9·xs.len()²`. The truth is
linear: each call moves `i` on, the next starts where it stopped, and the distances add up to the
length once. That is the potential method, with the index as the potential.

**The callee: a function that advances.** `g` advances an index through its array parameter `a`
when it returns an `i64` local `i` (or a struct with `i` in a field `f`), bound as `let mut i = p`
from its `i64` parameter `p`, and:
- every assignment to `i` is `i += 1`, made where `i < a.len()` holds — inside a `while` whose
  condition is `i < a.len()` or starts with it, or an `if` whose condition does — and at most once
  there before the guard is tested again;
- every loop in `g` is such a `while`, and a scan of `i` (§ A scan): a lap grows `i` by at least one.
  There is no `for`.

Then `p ≤ result ≤ max(p, a.len())`, and every lap of every loop moved `i` by at least one, so the
laps together are at most `result − p`. When `g`'s cost has the form `α·(a.len() − p) + β` piece by
piece, with `α` and `β` free of `a.len()` and `p` and `α ≥ 0` (`α` may be `B + 1`, bytes a line),
its cost is at most `α·(result − p) + β`: `α` was the laps' rate and `a.len() − p` their trip.
`bootstrap/src/cost/scan.rs` checks the shape (`advance`); the form is read off `g`'s cost at the call.

**The caller.** In a `while` body, at its top level, `let r = g(.., A, .., v, ..)` with `v` a mutable
`i64` in `g`'s parameter `p` and an array `A` in `a`, and later in the body `v = r.f + c` with
`c ≥ 0`, the only assignment to `v` in the body. Lap `k` calls at `v_k` and gets `r_k.f ≤ v_{k+1}`;
and `r_k.f ≤ A.len()` whenever `v_k < A.len()`, while `r_k.f = v_k` otherwise. So the distances
telescope:

    Σ_k (r_k.f − v_k) ≤ A.len() − v₀

with `v₀` what `v` is at entry, or at least (§ A scan). A lap is charged `β`, and the loop is
charged `α·(A.len() − v₀)` once, after it. The loop's own trip comes from whatever bounds it: a
scan of `v` (`count_ints`, `i = r.end + 1`) or anything else (`read_ints`, which runs to
`out.len()` and moves `i` alongside).

Each column on its own: work, span and moves are each amortised when their distance term has the
form, and charged a lap at a time otherwise. `next_int`'s moves while `xs` fits in memory are
`2·xs.len() − start + 2·B` — a cold read of the whole array at every call, not a distance — and so
stay per lap: the corpus's parser moves are still quadratic. That is the residue a warm walk would
credit, not something this rule reaches.

**What the report says.** The line is `bound`, as a scan's is, and a note names the call:
``scan: the calls to `next_int` at line 42 are amortised: `i` only moves on through where `next_int`
stopped in `xs`, so together they are charged 9·xs.len() once, not a scan of `xs` a lap``.

**Where it refuses** — the calls are then charged as a scan charges them, a lap at a time:
- a callee that may step past its guard (`i += 2`, or two increments before the guard is tested
  again), whose result can pass `a.len()`;
- a callee with a loop that is not a scan of its index, or a `for`;
- a caller that moves `v` other than through what the call returned (`v += 1`), or assigns it twice;
- a call not at the top level of the loop body, so perhaps more than once a lap;
- `A`'s length or `v`'s entry not known where the loop starts.

**A chain.** A lap may call several advancing functions in a row, each where the last stopped —
`s = next_int(t, i)`, `t2 = next_int(t, s.end + 1)`, `v = next_int(t, t2.end + 1)`, `i = v.end + 1`, a
CSV row. Each argument is at least the end before it, so the distances of all the calls of all the
laps still telescope to `A.len() − v₀`; the chain is charged its calls' largest rate once for that
distance, not once per call.

Golden `amortised` has both sides: `fields` (`19·xs.len() + 1`, from quadratic) and `sum_fields`
(`14·out.len() + 4·xs.len() + 1`), `hops` and `restart` refused and quadratic. In golden `scan`,
`words` goes from `2·xs.len()² + 11·xs.len() + 1` to `13·xs.len() + 1`, moves too.


**The ends of the stretch, and serial work (2026-09-28).** A call's moves past its distance — the
partly used lines at the two ends of what it reads, `3·B` for `next_int` — are the chain's: the
next call starts where this one stopped. Where the callee touches the scanned array alone and has
no array of its own, the constant `k·B` of its moves is taken out of each lap and charged once for
the loop, the chain's largest. And the callee's serial work and divisions (§ Time) are amortised as
work is — without it they were in a distance the call cannot name, and dropped: the parse loop's
`v = v·10 + d` chain was charged at `τ`, not `τ_s`.
## A scan to a sentinel

`while xs[i].f >= 0 { …; i += c }` stops at a sentinel the data holds, and nothing in the program
says where. But the condition reads `xs[i]` every time it is evaluated, and an index past the end
stops the program (the bounds check), so the loop runs at most `(xs.len() − i₀)/c` times, `i₀` the
least `i` is on entry (§ A scan). It applies when the condition indexes the array at `i` outside
any `&&` or `||` that may skip it — a conjunct of an `&&` counts, since the loop goes on only when
every conjunct held — and `i` is a mutable `i64` stepped by a constant `c > 0` once, at the top level
of the body, and nowhere else: a step inside an `if` may not happen, and then the loop never
reaches the end. The line is a bound, since the sentinel may come first, and its note says which
variable and array. The compiler's look-up of a signature by name, `while sigs[i].name >= 0`, is
this shape (`call_arr_len`, `used_after`); golden `sentinel` has `find`, `skip` (a step of two) and
`stuck`, refused. The self-hosted pass does not have the rule.

## A bounded worklist

**Written 2026-09-27.** Breadth-first search keeps its frontier in an array: `while head < tail`,
`head` stepped by one, and `tail` pushed inside the body — `queue[tail] = v; tail += 1;`. Neither
rule above bounds it, since the bound is assigned inside the body, and M3 named it as the worklist
a count of pushes would bound. A count of pushes does not reach it: the pushes are guarded by
`dist[v] < 0`, which only a count of vertices sees. What does reach it is the bounds check.

**The rule.** `while h < t`, `h` stepped by a constant `step ≥ 1`, runs at most
`(t₀ + a.len() − h₀)/step` times when every assignment to `t` in the body, nested blocks included,
is `t += 1` straight after a write `a[t] = …` in the same block, `a` one array throughout, and
`t₀ ≥ 0`. The write is bounds-checked, so it happens only if `t < a.len()`, and then `t` becomes at
most `a.len()`; before any push `t` is `t₀`. So `t ≤ max(t₀, a.len()) ≤ t₀ + a.len()`, and `h`,
growing by `step` a lap from `h₀`, meets it within that many laps. `h₀` and `t₀` are the entry
values; `a`'s length a size. The line is `bound`, with a note:
``worklist: `tail` only grows by one straight after a checked write `queue[tail]`, so it stays at most
queue.len() + 1 and the `while` at line 42 runs at most … times as `head` meets it``, and a caller
rests on it as on a scan.

**Where it refuses.** A tail moved by more than one after a write (`t += 2`), moved without a write
just before it, or written through two arrays; an entry value of either index not known; a tail
that may start negative. `h` must be stepped by a constant: a head that jumps is a scan's business.

Golden `worklist`: `reach` is `14·next.len() + 15`; `drain`, whose tail moves by two, stays unknown.
In the corpus `bfs`'s `main` has a cost for the first time, `≈ 11·n·(max(start[_]) − min(start[_]))`:
the worklist's `n + 1` laps, each charged its row at the widest a row can be. The truth is `2·m` over
all rows — the rows partition `adj` — and that is an amortisation over the reads of `start`, not
something this rule does.

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
recursion is not solved as a recurrence; over a tree in an arena it is bounded (below), and
otherwise the message says so. A recursive callee is never specialised at a call
site: its cost is the solved recurrence in its own parameters, substituted.

### A tree in an arena, and a forest

When no measure shrinks, one more shape is tried, for one function or for a component of the call
graph — functions that call one another however indirectly, which neither can be costed without
the other: the members walk a tree threaded through one array `xs` by index, the arena idiom of
§ A walk down a list with more than one link. The compiler's own walkers are this shape, a
statement walker calling an expression walker calling a block walker. Each member has an `i64` node
and the array as parameters, and every call inside the component hands on the array and, for the
node, the caller's own node, one below it — `xs[t].f`, `xs[t]` of an `[i64]`, or a name bound to
one, `let c = xs[t].b; … xs[c].a` — or the variable of a walk down a list,
`while s >= 0 { …; s = xs[s].next }`, which is read as the recursion `W(s) = body(s) + W(xs[s].next)`
(`forest.rs`).

An invocation together with the calls it makes on its own node, and theirs, is a **group**. Every
path through a group is enumerated — one branch of each `if` at a time, an early `return` ending
it — and on each, no chain of calls on one node comes back to where it began, and no path of links
is a prefix of another, the same link twice included. Then over an arena that is a tree each node
is entered by at most one group, the one on the ancestor its path of links starts from, and every
group but the first is entered from a group on a node of the array: at most `L·xs.len() + 1`
groups, `L` the most paths of links one group goes down. Each member `G` is costed on its own, the
calls into the component charged their call only and a walk that calls into it charged one lap,
its laps being invocations; a group costs at most `Σ m_G·cost(G)`, `m_G` the most invocations of `G`
in one group; and each member's line is the product, tier `bound`, with the promise stated as a
walk states it. A cycle in the links breaks it as it breaks a walk; so does a DAG, where a shared
node is entered once per parent.

It applies only where an invocation's own cost does not depend on which one it is, as the member's
cost at the start stands for every invocation's. What moves from one invocation to the next — the
node, a parameter some call does not hand on unchanged, a size an invocation binds once — is taken
out first: an element read at an index that moves is the most its array holds (`ws[xs[t].kind]` is
`max(ws[_])`), an unknown callee's argument that moves is `_`, and each regime of the cost is
dropped for the sum of its pieces, since which one holds is an invocation's own. What is left may
name none of them, and every array it reads must be one the component never writes: a read is a
value at the start only if nothing between the start and the invocation changed it.
`f(xs, xs[t].l) + f(xs, xs[t].l)` enters each left child twice and doubles at every level, and is
refused; three calls down `.a` in three branches of a dispatch on the node's kind are one a path,
and are not. The footprint of such a recursion is the whole arena, with no residue claimed: the
body's own sites are one node, and what is in the cache after a walk is whatever it read last.

`binary-trees`' `check` is `22·ns.len() + 11`. Golden `arena_tree` has `sum` and `same` (a loop to a
depth handed down unchanged) and three refused, `chase` (`ns[t].l + 1` is below no node), `deep`
(it hands down `d + 1`, so its invocations differ) and `twice` (down `l` twice); golden `forest` has
`has`/`has_list`, `size`/`size_list` and `spell` (a loop to an element at the node), and four
refused, `dup` (two members going down `.a` between them), `lap` (a call on the member's own node
once a lap of a counted loop), `deep`, and `grow` (a loop to what the recursion writes).
In the compiler, `same_ty`, `resolve_ty`, `expr_same`, `idx_coef`, `emit_type`, `has_assign` and
`has_assign_list` get a bound. The cost walkers `w_*` and `m_*` have the shape, but their costs read
the state they write (`wst`, `pst`), and their lines say so instead of "mutually recursive"; the checker's and the emitter's do not, since a member calls into the
component once a lap of a counted loop, over a chain's stages or a struct's fields. The self-hosted pass does not have the rule and declines all of these; where
its footprint of one then differs from the Rust's, nothing uses it (`DECLINED_FOOT`).

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
`modulo` by the calculus up to the unknown callees it names, `declared` by a person, `measured`
by the machine. A function that composes a declared callee
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

## An unknown callee is a term

A call to a function whose cost is unknown does not make the caller unknown. The callee's cost
enters the caller as a named term, `work[f](…)` and `moves[f](…)`, over the callee's parameters
that carry a size — an array's length, an `i64`'s value — with each argument the caller can state
as a size expression written in, and `_` for one it cannot. A term with a `_` is the most one call
costs over every value that argument takes, so two calls that differ only there are the same term;
a loop summing a call whose argument is its own variable does the same, since a term cannot be
summed over an argument (`for k in 0..n { walk(k) }` is `n·work[walk]`). The rest of the caller
is costed as before and its tier is `modulo`, with `rests on f (unknown)` naming the callee, which
carries its own reason on its own line. The term substitutes through callers like any size, is
never given a value, is never dropped as lower order, and is covered by a `#[cost]` bound only by
the same term — so a declaration never holds of a cost with an unknown in it.

The callee is seen the way a declared one is: no footprint and no residue, so nothing it reads is
credited to what follows and nothing is claimed resident after it. Three things stay unknown:
a callee declared `unbounded`, whose term would be infinite; a callee in the same cycle of calls
as the caller, whose cost is a system of recurrences in which neither can be a term of the other;
and an exact callee whose cost depends on an argument that is not a size expression.

## A size read from memory

An `i64` read from an array — an element `xs[e]` of an `[i64]`, or a field `xs[e].f` of type
`i64` — is a size where a size is needed: a loop bound, a `while` bound, an immutable `let`, the
entry value of an induction variable. It is an atom of its own, minted at the read and named by
its text, `offsets[u]` or `pols[p].n_term`; nothing is known of its value, so a cost in it is
exact in it the way a cost in a parameter is exact in the parameter. Where `e` is itself a size
expression the atom is that element. Where it is not, the atom is `max(xs[_])` — the most any
element holds — or `min(xs[_])` where the size bounds something from below, as the start of a
range does, and the line's tier is `bound`, not `exact`: it is an upper bound and says so. The
same happens to an element whose index a loop sums away, in the cost and in the fit conditions
between its regimes alike. `max(xs[_])` is one atom however many reads produced it — the most the
array holds over the run — so two of them multiply to a square and do not fork a regime apiece.
A footprint lower bound that is still in a closed loop's variable is dropped rather than
widened, since a `max` would make a lower bound larger.

A read names a value, not a place. Two reads of the same text are one atom until something writes
the array — a store into it, `ys = xs` onto it, or passing it to a call that may write it — handed
over `&mut` or owned, to a callee that stores into it, itself or further down (§ A walk down a
list). Inside a loop whose body writes the array, a read of it is no size at all,
since it differs from one iteration to the next. Through a call the callee's reads come with its
cost: a read of a parameter becomes a read of the caller's argument, and a read of the callee's
own array stays as `f.xs[…]`, a value the caller cannot name any better.

What the atom is not yet: the expression the value was written from. `let k = n; xs[0] = k` then
a loop to `xs[0]` is a loop to the atom `xs[0]`, not to `n` — following a store to its load is
what plan § Stage D (2) left for later.

**By slot and by field (2026-09-28).** A read was stale inside any loop that wrote its array at
all. A store to `a[k]`, `k` a literal and `a` an array of scalars, is now recorded as a write to slot
`k` only — the compiler keeps its state in such arrays, `wst[10]`, `cst[12]` — and a store to
`a[j].f` as one to field `f`, both carried through calls by the same fixed point the fields a call
may write already were (§ A walk down a list). A read of `a[k]` is stale where a loop around it may
write slot `k`, or any slot at an index it computes; a read of `a[i].f` where it may write field `f`
or a whole element. The compiler moves by one (97 unknown, from 98); golden `slots`.

**Through a call, in a loop (2026-09-28, a fix).** A callee's cost that reads a size from an array
it is handed names the value at the call. In a loop that writes that field or slot, every lap's
call reads a value of its own, and one atom stood for them all: `many` calling `upto`, which loops
to `xs[0]`, ten times while adding 100 to `xs[0]`, was costed `30·xs[0] + 90`, the first lap's ten
times. Such a call is now unknown, and says which array — unless the read is only an unknown
callee's argument, where it is `_`, as an argument a call cannot name is: `pass` hands `xs[0]` to
`wander`, and `repeat`, calling `pass` while it writes `xs[0]`, is `4·work[wander] + 48`. The
compiler loses seven `modulo` lines, each through a callee that reads a size from state its loop
writes (`pols`, `wst`, `cks`); golden `cursor`'s `many`, `twice` and `repeat`.

A call whose every access is resident moves nothing (§ Moves) — but not one that calls an unknown
callee, which may have arrays of its own: `repeat`'s moves were one `moves[wander]` for four calls.

**A cursor in a slot.** `while ds[0] < ds[1] { …; ds[0] = ds[0] + c }` counts as an induction
variable does, the slot `ds[k]` of an array of scalars standing for the variable — a parser keeps
its cursor so, to hand it to callees. It applies when the slot is stepped by a constant `c > 0` once,
at the top level of the body, and written nowhere else in it, itself or through a callee (the slot
writes above); and when the bound is a size nothing in the body writes. The trip is
`(bound − ds[k])/c`, both read at entry to the loop, before its own writes. `c_space`, the cost
attribute's reader skipping blanks, is exact; golden `cursor`'s `skip_sp`, and `via_call` (stepped in
a callee) and `moving_end` (the end moves) refused.

## A size bound once

**Added 2026-09-26.** An `i64` bound by `let n = e` — immutable, outside every loop — where `e` is
nothing the calculus can name (a call's result, `parse_int(&a)`, `count_ints(&text)`, a division
by a variable) is an atom of its own for the rest of the function, named after the local: `n`.
Nothing is known of its value, so a cost in it is exact in it the way a cost in a parameter is.
An array `[e; n]` has it as its length, and a loop to it has it as its trip
(`bound_once.cost`: `squares  work 9·n + 5  exact`).

**Why it is sound.** The local cannot be assigned, so it names one value for the rest of the
run. That is all a parameter is to the calculus: a number fixed on entry and never known.
The atom is minted where the `let` is and stands only for what follows it.

**Where it is refused.**
- **Bound inside a loop**: it is a new value every lap and stays no size (`bound_once_loop.cost`:
  "the length of `t` is not a size expression").
- **`let mut`**: it may change after it is bound, so it stays no size, as a mutable local always
  has ("loop bound is not a size expression").
- A value the calculus *can* name is left as it was: a size expression, an element read
  (§ A size read from memory), an argument's or a file's length (§ Program input).

**A negative value** is not ruled out, any more than for an `i64` parameter. A line is read for
the atom at `0` or more. A negative `n` as a length never gets past the allocation, which exits
101 ("negative array length"). A loop `0..n` with `n < 0` runs no lap, so it costs less than the
line's constant.

**At a call** the callee's atom is a new quantity at every call, so the caller gets an atom of its
own for it, named `callee.local`: `main  work 9·squares.n + 11`. This is the rule § Program input
gave `first_len.a.len()`. Inside a loop of the caller, a callee's atom used as a size is refused
("calls `f` in a loop: `n`, a size it binds once, is a new value at every iteration"). One that
only indexes an element the callee reads, `pols[nb].n_term`, is widened as a variable summed away
is, to `max(pols[_].n_term)`, and the line is a bound. An unknown callee's argument is shown as `_`.

## Program input

The input builtins (decisions.md §9) are externs with declared costs, so a caller rests on them
like on any declaration. What the calculus adds is the size of what they return, in three cases:

- **`arg_count()` is exact.** The number of arguments is fixed for the run, so every call returns
  the same value: one atom, `arg_count()`, minted once per function when the program has the
  builtin, and the callee's `arg_count()` is its caller's own at a call rather than a new one. A
  loop to it, directly or through an immutable `let`, is exact in it, as a loop to a parameter
  is (`input_argc.cost`: `main  work 6·arg_count() + 28`).
- **An array returned outside a loop is exact.** `let a = arg(0)` or `let d = read_file(&p)`: an
  extern that returns `[T]` names its result's length `result.len()` in its declaration, and the
  caller gets an atom of its own for it at the call, named after the local, `a.len()`
  (`input_args.cost`). It is free the way a parameter's length is. Through a function that reads
  and returns, the caller gets a new atom again (`first_len.a.len()` in `modules/input`).
- **`arg(k)` inside a loop is a bound.** Each lap's argument has a length of its own, an atom that
  would not survive the lap. What does survive is the longest argument: the lap's array is taken
  at `max(arg[_].len())`, a read atom with no element (§ A size read from memory), shared by every
  function, so the line is `bound` and a loop over all of them reads `arg_count()·max(arg[_].len())`
  (`input_perlap.cost`). As for an array born in a loop by `[e; n]`, the lap's array is one local
  to the calculus: where it fits, its read after the first lap is credited as resident.
- **Anything else read inside a loop stays unknown**, with the call named: `read_file` per lap
  (`input_perlap_file.cost`: "calls `read_file` in a loop: what it reads is a size of its own at
  every iteration"). No whole bounds it: the files are not known to the run the way its arguments
  are. So does a function that returns an argument it read, called per lap: the provenance of its
  result's atom does not travel with it.

**The declarations, measured.** `neant measure --fn arg_count|arg|read_file|file_size` runs each
builtin on a real input of `n` bytes, an argument of `n` bytes or a file of `n` bytes, because the
ordinary driver cannot build either and has no value for `result.len()`. Before this, a
declaration the sweep could not evaluate was reported as confirmed; it is now reported as not.
On the machine (`--cpu 5`, 10000 calls per size, n = 1000..32000) the first declarations failed
for three of the four, and in the same place: they had no constant. `arg_count` measured 12.5
instructions a call against a declared 1 — the call into `rt.c`, which the compiler cannot
inline; opening a file measured about 2000 user-space instructions for `file_size` and about 2260
for `read_file` at small `n`, against a declared `path.len()`. `arg` was confirmed as declared:
it measured about `n/3 + 300` instructions and a few lines of traffic, well under `n` and `n`.
The declarations now carry the constants, `10` and `+ 2500`, and all four are confirmed. The
kernel's share of a read, the copy into the buffer among it, is not in the counter (it counts
user space), so `moves` is confirmed here only as not exceeded; a file read streams through the
page cache, which the model does not see.

## A walk down a list

`while s >= 0 { …; s = xs[s].f }` walks a list threaded through the array `xs` by the `i64` field
`f` — the arena idiom, where a link is an index and `−1` ends the list. It runs at most as many
times as **the longest walk along `f`**, the atom `walk(xs[_].f)`: the most steps `s = xs[s].f`
takes, from any element, to reach a negative index, over the run. `xs[s]` of an `[i64]` is the
same with no field, `walk(xs[_])`. Nothing is known of the atom's value, as nothing is of
`max(xs[_])`, so the line's tier is `bound`; if no walk is a cycle it is at most `xs.len()`, and if
one is, a loop that follows it and ends did so by another way out — a `break`, or another conjunct
of the condition. `arena.nt` states that promise itself, `decreasing nodes.len() − steps`, and is
exact in `nodes.len()`; the atom is what a loop gets that promises nothing.

It applies when `s` is a mutable `i64` compared as `s >= 0` (or `0 <= s`, or as one conjunct of
an `&&`), assigned `xs[s].f` exactly once, at the top level of the body, and nowhere else in it;
and when **nothing in the body writes `f`** — no store to `xs[_].f` or to a whole element, no
`ys = xs` on it, and no call that may write `f` in what it is handed. Which fields a call may write
is each function's own summary, per parameter, to a fixed point over the call graph: the fields it
stores into, and what the callees it passes the array on to write; an `extern` handed an array
writable may write any. So a body that writes `val` while it walks `next`, itself or through a
callee, keeps its bound, and one that splices the list as it walks it is unknown and says why:
the walk may visit what it just inserted, and does not end.

The same summary is what *reads* consult (§ A size read from memory): a call counts as writing an
array only where the callee may store into it, so handing a view to a function that only reads it
no longer makes the reads of it stale, after the call or anywhere in a loop around it.

Two consequences in the calculus around it. An element read is at most the most its array holds,
`xs[e] ≤ max(xs[_])`, and dominance knows it, so a fit condition on the element is implied by one
on the maximum and the regimes that contradict that are dropped. And an exact callee whose
footprint ends depend on an argument the call cannot name no longer makes the caller unknown: the
footprint is kept inexact, so nothing is credited from it and nothing claimed resident after, a
lower bound in that argument is not handed up, and the cost goes through as before — the argument
`_` in a read's element, which makes it the `max`.

## Time

**Written 2026-09-27, the roofline M5 decided (decisions §7).** `work` counts operations and
`moves` bytes across the `M` boundary; neither is a time. With `--eval`, a line now also says

    time = max(span·τ, work·τ/P, moves/BW)

and which term bound it: `τ` nanoseconds per unit of work on one core, `BW` bytes per nanosecond
across `M`, and in sequential code `span = work`, so it is `max(work·τ, moves/BW)`. Both constants
are the machine's, fitted by `tests/kernels/roofline.py fit` on a pinned big core — `τ` from a
Horner polynomial over an array in L1 (work dominates by three orders), `BW` from a streaming sum
over 100 MB (moves dominate by five times) — and set with `--tau` and `--bw`. On this machine
(Cortex-X925, CPU 5): **τ = 0.0176 ns, BW = 20.8 GB/s**. `τ` is small because a unit of work is an
operation of the source and the C compiler vectorises and fuses them: it is a rate for this
compiler's code, not a cycle.

**Latency (2026-09-27).** A pointer chase fetches one line, waits for it, and only then knows the
next address: its lines do not overlap, and `moves/BW` charges them as if they streamed. So the
lines fetched by an access whose index is a local the loop assigned from a load — `i = nodes[i].next;
… nodes[i]` — are also counted as `chase`, a part of `moves`, composed through calls and loops as work
is, and the memory term becomes

    (moves − chase)/BW + chase/B · L

with `L` the nanoseconds a chased line waits, fitted on a chase over a 64 MB arena in a random
cycle: **L = 112 ns**. `chase` is only ever read by a time: no bound, tier or report line changes,
and `--eval` prints it when it is not zero.

**Cores (2026-09-28).** A `.par()` chain's time is `max(span·τ, work·τ/P, moves/BW(P))`, and `BW(P)`
is not one core's: `P` cores each pull a core's bandwidth until memory runs out, `BW(P) = min(P·BW,
BW_max)` — the two-line roofline M5 decided and did not fit. `BW_max = 65.6 GB/s`, fitted on the
parallel sum over all ten big cores (`--bwmax`); sequential code keeps one core's `BW`.

**Serial work (2026-09-28).** `τ` is a throughput: the rate of a loop whose laps overlap in the
pipelines, fitted on a polynomial over an array. A loop whose body carries a scalar from one lap to
the next through a multiply or a divide — `x ← r·x·(1 − x)`, mandelbrot's `z ← z² + c`, a digit
loop's `v ← 10·v + d` — cannot overlap its laps: each waits for the last one's multiply. Such a loop's
work (its whole body's, nested loops' included) is counted as `serial`, composed through calls as work
is, and the compute term becomes `(work − serial)·τ + serial·τ_s`, with `τ_s` fitted on the logistic
map: **τ_s = 0.155 ns**, nine times `τ`. A reduction, `s += f(x[i])`, carries only an add and is not
serial — unless its lap is short (below). Mandelbrot goes from 4.2× too fast to 0.50, the half an upper bound's: its 50 laps are the most
a point can take. Only a time reads `serial`.

*Through memory.* A store to an array element of a value read from that same element, at an index
the loop does not move — `bs[i].vx = bs[i].vx − dx·m` in a loop over `j` — makes each lap's load wait
for the last lap's store, whatever the operation. Such a store is one unit of serial work a lap (only
the chained operation waits; the rest of the lap overlaps), where a scalar multiply chain makes the
whole loop serial. Charging the whole loop for a memory chain was tried first and took n-body from
2.3× too fast to 2.7× too slow.

*Around a loop inside (2026-09-30).* "Its whole body's, nested loops' included" charged a chain
carried at a loop's own level for the loops inside it too: durbin's `beta = (1 − α²)·β` each lap of
`k`, around three loops over `i` that run between one link and the next, made all of durbin serial,
predicted 3.8 times its time. The loops inside do not wait on that chain (removing it measures the
same; splitting their `sum` four ways does not, which is their own add chain), so the lap is now
charged its own work — the work the frame has less what its nested loops added (`inner_work`) —
and the nested loops' own serial work, where it was the whole lap. An innermost loop has none
inside, so mandelbrot, the logistic map and seidel-2d keep their charge. durbin goes from 0.2495 to
1.60 of its predicted time at EXTRALARGE and from 0.26 to 1.68 at LARGE. Found by a read-only look
from another session, which measured the two variants. Golden `outer_chain`.

*A store that reads the last lap's (2026-09-30).* A lap that reads the element the lap before it
stored — Gauss–Seidel's `a[i·n + j] = (… + a[i·n + j − 1] + …) / 9` over `j`, a prefix sum's
`a[j] = a[j] + a[j − 1]` — waits for that store: a recurrence through memory at a moving index,
which neither rule above saw (the store to a fixed element needs an index the loop does not move).
The read's index is the store's less the store's coefficient in the loop variable times the step,
compared as polynomials in the locals (`lap_chain`). Charged as a carried scalar is: the whole lap
serial when the value read reaches the store through a multiply or a divide, one unit a lap when
only through adds. PolyBench's seidel-2d goes from 5.3–5.8 times slower than predicted to 0.78–0.81
at all three sizes, with `τ_s` as it was (evaluation § 5). Golden `lap_chain`, whose `.eval` pins
the `serial` column the plain report does not print.

*Through an add (2026-09-28).* An `f64` a lap carries through an add or a subtract — `s += xs[i]`,
`e = e − d` — waits for the add, whose latency a unit of work does not have: measured, the `sum`
kernel in L1 takes 0.25 ns a lap against 0.07 predicted. The core runs a lap's other work while the
add waits, so the add binds only a lap with less work than one unit of serial work takes,
`work·τ < τ_s`; such a lap's carried `f64` is one unit of serial work each (`sum` in L1: 1.2× from
3.6×). Charged whatever the lap, it took `horner`, eight multiplies a lap, to 0.70 and a tiled
`matmul`, whose next `j` starts the next chain, to 0.51, and brought spectral-norm, a division a
lap, from 1.26 to 0.96: with the rule as it is, spectral-norm stays at 1.26, a lap with more work
than the add that still waits on something the model does not name.

**Streams and write-backs (2026-09-28).** One core reads one stream at about 30 GB/s and two or
more at about 60 (experiments.md, bandwidth by the number of streams), and a stored line goes back
to memory when it leaves the cache, bytes `moves` does not count. Two time-only columns carry this.
`conc` is the part of `moves` whose site streams in its innermost loop — an address that moves with
it by less than a line a lap — in a loop with two such streams or more, one per array and a field
under SoA its own; those bytes move at `min(2·BW, BW_max)`. `wback` is the lines of the stored
sites, at one stream's rate alone and half their bytes in a loop of two streams or more. A call
whose lines the caller already holds — credited, or moving nothing because all of it is resident —
takes nothing back: its stores stay in the cache with it. So the memory term of a sequential
program is `(moves − chase − conc)/BW + conc/min(2·BW, BW_max) + wback/BW` and the chase's latency.
`BW` is fitted on one stream, and stands. Alone, the stream term made the kernels worse: `dot`
waits on its multiply-add's latency as much as on the memory, and `saxpy`'s write-backs had covered
for its missing stream (experiments.md).

**M7's line (2026-09-29).** `neant cost --eval … --m7` adds `m7 T s`: the C the compiler emits is
compiled to assembly (`cc -O2 -g -S`, unchecked, a `#line` before each loop), each innermost loop
of it is the calculus's loop at its line, and its cycles on a model of this core (`src/m7.rs`:
latencies and pipes measured by `tests/kernels/*.c`, a short loop's entries simulated inside the
loop around it with an instruction window, a missed exit an entry where the trip varies) times its
laps and entries make the compute; `T` is the longer of that and the memory term above. It is an
estimate of the constant, next to the cost and never in it; experiments.md, M7. Where a loop's laps
are only a bound — a `break`, a condition of several parts, a scan — and that loop is more than a
hundredth of the compute, the line is `m7 ≤ T`; where a callee's laps were left out, a size the call
cannot name (an amortised scan's), it says M7 does not apply.

**Divisions (2026-09-28).** An `f64` division is one unit of work to the calculus and several to
the machine: its throughput is a fraction of an add's. Divisions are counted apart as `divs`,
composed as work is, and charged `τ_div = 0.148 ns` each beyond their unit, fitted on a sum of
reciprocals over an array in L1. Spectral-norm, whose inner loop divides, goes from 1.9 to 1.27.
Only a time reads `divs`.

**Translations (2026-09-30).** An access whose address moves a page or more a lap of its innermost
loop — a column walk, `data[k·m + i]` over `k` with `8·m ≥ 4096` — needs a page of its own every
lap, and when one run of that loop touches more pages than the second-level TLB maps (2048 of 4 KiB,
`T` = 8 MiB) every lap waits for a page walk, whether or not its line is in cache. Measured by
`tests/kernels/pagewalk.nt` on this core: an access to a line in cache costs 0.13 ns while its pages
fit the first-level TLB, 0.3–0.7 ns while they fit the second, and 1.5–2.1 ns past it (at 3072 to
8192 pages, a stride of 20.8 KB); `τ_tlb` = **1.6 ns** is the last less the second, fitted on the
probe and on no program. Such an access is counted as `tlb`, per lap and not per line fetched —
under the two conditions that its step is a page or more (when the step is a size) and that the
loop's accesses that move so, times its trip at its largest, are more pages than `T` — composed
through calls and loops as `divs` is, and charged `τ_tlb` each on the compute side of a time.
Both conditions are written as fit tests on `M` at this machine's ratio of `T` (or a page) to `M`.
Two things tell it apart from the per-line charge tried below: it counts accesses, so a column that
stays in cache still pays, and it asks whether the pages fit, so `matmul_naive`'s column walk, 832
pages at 832, pays nothing — measured 0.37 ns a lap there, where every lap is a new page. A stride
of a power of two is slower again (1.0 ns a lap at 4 KiB and 8 KiB within the TLB's reach, the lines
meeting in one set of the cache), which is not in the model. PolyBench's correlation, covariance and
gramschmidt at EXTRALARGE go from 8.7–9.0 times slower than predicted to 0.96–0.98; at LARGE,
where their pages are 1.4 times the reach and the walk costs less than at four, correlation and
covariance are overcharged (0.41–0.42). Golden `tlb_column`.

**Pages, tried (2026-09-27).** Lines fetched by an access that moves a page or more a lap of its
innermost loop are counted as `paged`, and `--tlb ns` charges each of them a TLB walk. Fitted on a
transpose it is 9 ns; held fixed, it over-charges naive matmul fourfold, whose column walk reuses
the same pages column after column and whose misses hide the walk. So it is off by default
(experiments.md § The roofline, pages): a page walk costs something only where the line itself
does not, which a count of paged lines cannot see.

**A second level, tried (2026-09-27).** With `--M3 bytes` the whole analysis runs a second time with
`M` the outer cache's size — a size known at analysis time decides its regime there, so a cost at
one `M` cannot be re-read at another — and what crosses `M` but not the outer boundary is charged
at that cache's bandwidth and latency, `BW₂ = 30 GB/s` and `L₃ = 23 ns`, fitted by differences so
that the model's first touch does not enter. Measured, it moves the error between kernels rather than
shrinking it, so it is not the default (experiments.md § The roofline, a second level).

What the model is not, and the measurements say so (experiments.md § The roofline): it has one
cache level, so data that fits in `M` moves nothing and its time is all work, while L2 and page
faults are real, and a chase over an arena that fits in L3 waits for L3, not memory; and it charges a
strided sweep's lines at the streaming rate, while a stride that misses the TLB costs more. Each of
those is a named term the model could grow, not a constant to tune.

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
exactly on the function's one line, and leaves out the `(line N)` the report ends an unknown with:
the lockfile is read in review, where a number that moves because something above it grew is noise
on every unknown below it. `--eval n=1792,B=64` decides the conditions at the machine's
`B` and `M` and prints the applicable piece's numbers. `#[cost]` must hold in every piece.

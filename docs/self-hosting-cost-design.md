# Self-hosting: the cost calculus, first slice

The fixpoint is reached and **the cost calculus is not in it**
([self-hosting-fixpoint.md](self-hosting-fixpoint.md) §2). That is the largest omission by far,
because it is the language's whole point: a neant compiler that does not infer cost is a C
frontend with an unusual syntax.

`bootstrap/src/cost/` is 4503 lines — nearly twice the whole self-hosted compiler. So this is
explicitly a **slice**, chosen the way every previous one was: by measuring what the corpus
actually needs, not by porting a file.

## 1. The measurement

Of the 98 functions in golden programs the self-hosted compiler can already read, `neant cost`
reports a **plain polynomial** for `work` on **63**. The other 35 need recurrences (`fib`),
regimes (a fit condition on `M`), or report `unknown`.

The distinct work shapes across those 63:

```
41          1024        10·n        25·n²       10·n³ + 5·n
19·fuel + 7 21·n/4 + 1  12·s.len()  10·ps.len() + 1
```

Constants, linear, squares, cubes, **rational coefficients** (`n/4`), and size variables that come
from an array's length. That is the whole data structure requirement: polynomials over size atoms,
rational coefficients, non-negative integer exponents.

Rational *exponents* — `M^(1−σ)` — appear only in `bounds.rs`, which is out of this slice. So the
self-hosted `Poly` is simpler than the Rust one, and honestly so.

## 2. Scope: `work`, and nothing else

**In:** `work` as a polynomial, for straight-line code, `if`, `for` with affine bounds, and calls
by substitution.

**Out**, each with a reason that is not "it is hard":

- **`moves`.** Needs footprints, cache lines, `B`, residue, the site/key machinery that makes two
  reads of one address merge — a second engine the size of this one. `work` alone is a complete,
  checkable claim; `moves` is the next slice, not a corner of this one.
- **Regimes and piecewise costs.** A fit condition compares a working set with `M`, and a working
  set is a footprint. Falls out with `moves`.
- **Recurrences.** `while` with a `decreasing` measure, and self-recursion. The Rust pass solves
  these into closed forms; this slice reports **unknown** for them, which is what the Rust pass
  does for `fib` too — just more often. *(`while` came in afterwards: §10.)*
- **`span`, bounds, IOLB, layout choice, `#[cost]` dominance, `costs.lock`.** Each is a consumer of
  the cost object, not part of computing one.

The exit test measures the slice rather than describing it: **63 of 98**, and the number is in the
test.

## 3. It runs on the parse tree, not on an IR

The Rust cost pass walks `ir::Block`, after desugaring. The self-hosted compiler **has no IR** —
`emit.nt` walks the parse tree directly — and this pass will too.

That is affordable here for one reason worth checking rather than assuming: the work table is
**per-node and local**. From `analyze.rs`: 1 for a binary operator, 1 for a unary or a cast, 2 for
`min`/`max` (compare, select), 1 for `println`, 2 for a call (call and return; arguments are
register moves), 3 for a `for`'s setup (store, increment, compare-and-branch), 2 per iteration,
1 for an index, 0 or 1 for an assignment depending on whether it is compound. Nothing in that table
asks a question the parse tree cannot answer.

What the IR *would* have given is desugaring: a chain becomes a loop before it is costed. Chains
are outside the parser's slice anyway, so the absence costs nothing today and will be the first
thing to hurt when they arrive. Stated here so it is not a surprise then.

## 4. The size atoms are already there

A cost polynomial's variables are **size atoms**: `n` in `[0; n]`, `xs.len()`, a parameter's
length. The self-hosted checker already mints them — `Ty.size`, three disjoint ranges (a
declaration's token, a literal's value, a fresh counter) — because whole-array assignment cannot
be type-checked without them (arrays design §7).

They were built for the type checker and they are exactly what the cost pass needs. `Ty.size` *is*
the atom id; no second mechanism. This is the one place where the slices happened to line up, and
it is worth saying that it was luck rather than foresight: the checker design had said size
variables were only the cost model's business and left them out, and then type checking turned out
to need them first.

A literal-valued atom (range `1000000 + v`) is not a variable at all — it is the constant `v`, and
the cost pass substitutes it rather than carrying it. That is what makes `main`'s work a number
where `sum_to`'s is `3·n`.

## 5. The representation

The arena pattern, three levels, all flat arrays with tracked counts:

```neant
struct Fac  { atom: i64, exp: i64 }                  // one factor of a monomial
struct Term { fac: i64, n_fac: i64, cn: i64, cd: i64 }  // coefficient n/d over a run of factors
struct Pol  { term: i64, n_term: i64 }               // a run of terms
```

Polynomials are **append-only and immutable once built**, which is what makes an arena work here:
`add`, `mul` and `sum_over` each write a new `Pol` at the end rather than editing one.

**Canonical form is the whole correctness argument.** Two polynomials are equal exactly when their
arenas agree term by term, so every constructor must produce factors sorted by atom, terms sorted
by monomial, zero coefficients dropped, and rationals reduced by gcd. Insertion sort over the flat
run — there are no generics and no comparison functions to pass, so each sort is written where it
is used.

Rationals are a pair of `i64`. The Rust one is `i128`; `neant` has no `i128`, and `14116` and
`1600007` in the corpus are nowhere near the edge. Overflow is not detected, which is a real
narrowing and is recorded rather than defended — the first slice's job is parity on a corpus, and
the corpus does not overflow.

## 6. Loops: Faulhaber, and why nothing else is needed

A `for i in lo..hi` whose body costs `c(i)` costs `Σ c(i)`. When `c` is a polynomial in `i`, the
sum is a polynomial in the trip count, by Faulhaber's formula — `Σ_{k<n} k` is `n²/2 − n/2`,
`Σ_{k<n} k²` is `n³/3 − n²/2 + n/6`, and so on. That is where the rational coefficients come from,
and it is why `Rat` is not optional.

`size.rs`'s `faulhaber(j, n)` is 20 lines and this is a direct port. A body whose cost does not
mention the loop variable is the easy case — multiply by the trip count — and falls out of the same
formula at `j = 0`.

## 7. `if` is the max of its branches, and that needs dominance

`analyze.rs` takes an `if` to cost the larger of its two branches. Two polynomials are not
generally comparable, so the Rust pass asks `dominates(q, p)`: is every term of `p` matched by a
term of `q` with a coefficient at least as large and an exponent at least as large, given every
size is ≥ 1. When neither dominates, the Rust pass forks into **regimes** — which are out of this
slice, so here a non-comparable `if` reports **unknown**.

That is a narrowing the exit test will measure rather than hide: if it costs more than a function
or two of the 63, the slice was drawn wrong.

## 8. Exit test

`neant cost <file>` prints a `work` column per function. The self-hosted pass prints the same
column, and for every in-slice golden function where the Rust compiler reports a plain polynomial,
**the two strings must be identical** — the same printing, the same rational reduction, the same
term order. Where the Rust compiler reports a recurrence, a regime or `unknown`, the self-hosted
pass must report `unknown` rather than a number: a cost calculus that guesses is worse than one
that declines.

String equality is deliberately strict. It catches `n²/2 − n/2` printed as `0.5·n² − 0.5·n`, and a
term order that happens to agree on the corpus and not in general.

The count goes in the test, so widening the slice means changing a number that someone has to
look at.

## 9. What building it changed

**Passed**, with a number the estimate did not predict: **53 work columns exact**, 19 declined, and
4 that differ *on purpose*. Every one of the 19 is in a function with a `while` or a self-recursive
call — the recurrences §2 put out of the slice — or calls one, so the boundary the design drew is
exactly the boundary the corpus found.

- **Four functions disagree because the two compilers emit different code.** `ys = xs` on whole
  arrays costs 1 when the compiler can prove the assignment is in place and the array's length
  when it cannot. The proof is M5's uniqueness analysis; the self-hosted compiler has none, so its
  emitter always copies, and its cost says so — `2400006` where the Rust compiler says `1600007`.
  It would have been easy to charge 1 and call it parity. That would have been a cost report for
  code this compiler does not emit, which is the one thing a cost calculus may not do. The four are
  listed by name in the test, and deleting a line from that list is what growing move checking
  would look like.
- **`n / 4` is a size expression, and the corpus said so twice.** The first walk refused division,
  on the reasoning that a quotient is not a polynomial. It is, when the divisor is a constant —
  `for ii in 0..n / 4` is how a tiled loop is written, and `let m = xs.len() / 10` how a derived
  length is. That is also where a rational coefficient enters from the *source* rather than from
  Faulhaber, which is a second reason `Rat` is not optional.
- **A byte-string literal is an array literal.** `let s = b"hello"` costs five, like `[0; 5]`, and
  carries a length that `s.len()` reads back. Missing it cost exactly 5 in `arrayview.nt` and was
  invisible until the total was 6 out — 5 for the literal and 1 for the reassignment above.
- **Byte-string literals hold ASCII only.** `bootstrap/src/lex.rs` refuses a non-ASCII byte
  literal, and every character this printer needs that the Rust one writes as a `char` — `·`, the
  minus sign `−`, the superscript digits — is multi-byte UTF-8. They go in by number, with the
  code points named in a comment, because a reader cannot check `0xC2 0xB7` by eye against a `·`
  that is not there. This is the first place the self-hosted compiler has had to write text it
  cannot spell.
- **The `if` comment in `analyze.rs` is wrong.** Three lines above `wt.max(&we)` it says "both
  branches are charged". They are not; the cost is the larger. Reading the comment and not the code
  would have produced a walker that agreed with nothing.

**Not integrated into the compiler binary.** `compiler/main.nt` is a filter — source in, C out —
and there is no argv to select a mode with, so `poly.nt` and `cost.nt` are not in the concatenation
`compiler/build.sh` builds and are not in `bootstrap/neant.c`. They are tested, and the native
self-hosted compiler compiles them and computes identical polynomials, so nothing about them is
outside the slice. What is missing is a way to *ask* for a cost report, and that is a question
about the command line, not about the calculus.


## 10. `while`, added after the first measurement

The first slice left every `while` unknown, and the measurement said that was 14 of the 19 — too
many to leave. So `while` came in next, and self-recursion did not: the two are called "recurrences"
together but only one of them actually needs a recurrence solved.

A `for` says its own trip count. A `while` does not, and `analyze.rs` has exactly two ways to find
one, both of which are now here:

- **The programmer's measure.** `while i < j decreasing j - i` — the trip count is the measure's
  value at entry. The promise is **checked, not taken**: `min_dec` walks the body and asks how far
  the measure falls along the path that falls least, taking the smaller side of an `if`, treating
  `break` and `return` as leaving, and refusing a nested loop that could make the measure rise. A
  measure that is not shown to fall by at least one every lap is not a bound, it is a wish.
- **An induction variable.** `while i < n` with `i += 1` in the body — the condition names a
  variable, the body steps it by a constant *exactly once*, the bound holds still while the loop
  runs, and the entry value is known. The trip count is the distance over the step.

**67 work columns exact**, up from 53. The five that remain are self-recursion — `fact`, `sum_from`,
`msum`, `bsearch` — and those want a recurrence *solved*, which is a different machine from a trip
count found.

What the build changed:

- **The measure is an expression and costs its own work.** `is_palindrome` came out `7·s.len() − 5`
  against `7·s.len() − 4`: exactly one unit, the `j - i` in `decreasing j - i`. Its caller was then
  out by two, because it calls it twice — which is the pleasant property of an exact model, that an
  error shows up multiplied rather than smeared.
- **A block's tail is a statement to this analysis.** An `if` in tail position still runs, and
  `min_dec` has to walk it or a measure that falls only in the tail reads as not falling at all.

## 11. `moves`, the next slice

`work` counts instructions. `moves` counts **cache-line traffic in the I/O model**, and it is the
half of the cost object this language exists for — a compiler that reports only `work` is an
instruction counter with an unusual syntax.

### The measurement

Of the 76 golden functions with a `moves` column: **19 are `moves 0`**, 49 are a plain polynomial,
8 need regimes. Splitting them the other way, **44 call nothing else in their module** and 31 of
those have non-zero moves. The commonest shapes:

```
8·xs.len() + B     16·a.len() + 2·B    9·cs.len() + 2·B    3·B    7·B + 56
```

A polynomial in the size atoms **and in `B`**, the cache line.

### The rule, which is simpler than the engine that produces it

For a site walking an array with stride `s` bytes over `t` iterations:

```
moves = t·s + B      when s < B   (contiguous: every byte crosses, plus the partial line at each end)
      = t·B          when s ≥ B   (strided: a whole line per touch)
```

and a function's moves are the sum over its sites. That reproduces `dot`'s two arrays as
`16·a.len() + 2·B` and `total`'s one as `8·xs.len() + B`, exactly.

`bootstrap/src/cost/analyze.rs`'s `settle_moves` computes the same thing the general way — lines
per loop level, innermost out, with the working set at each level tested against `M` — and that
generality is where **regimes** come from, and where a nested loop's reuse is found. Both are out.

### Scope

**In:** a function that calls nothing, whose array accesses are affine in the enclosing loop
variables. Each distinct `(array, index shape)` inside a loop is one site.

**Out:**

- **Calls.** A callee's moves depend on what is already resident, which is the footprint machinery
  — `repeat.nt`'s `main` calls `total` three times over one array and pays for it **once**, because
  after the first call the array is in `M`. Reporting the sum would be wrong by 16 000. So a call
  makes moves unknown, and 24 functions go with it.
- **Regimes.** A working set whose fit in `M` is symbolic forks, and the fork is the piecewise
  machinery §2 already excluded. 8 functions.
- **Nested-loop reuse.** `matmul`'s inner loop re-reads a row that may still be resident. That is
  `settle_moves`'s whole point and it is not approximable by the rule above.

### AoS, and the second deliberate divergence

The Rust compiler **chooses** AoS or SoA per struct from the cost model, and the choice changes
moves: `arrayview.nt`'s `tagged` reads `9·cs.len() + 2·B` under the SoA the Rust compiler picked
and would read `32·cs.len() + 2·B` under AoS. **The self-hosted emitter always emits AoS**
(self-hosting-arrays-design.md §4), so its moves are AoS moves.

This is the same principle as the four `ys = xs` divergences the `work` slice found, arrived at
independently: a cost is a claim about the code the compiler emits, so a compiler that emits AoS
must report AoS traffic. The affected functions are listed by name in the test, as those four are.

### Exit test

The `moves` column, string for string, for every leaf function — and `moves 0` must be *earned*:
reported because the pass walked the body and found no site, not because it did not look. A
function with a call must say unknown.

### What building it changed

**26 `moves` columns exact**, 35 declined, and 7 differing — every one of them the AoS/SoA
divergence §11 predicted before a line was written, which is the first time a prediction in one of
these documents has survived contact unchanged.

- **A site is per *branch*, not per address.** `parse.nt`'s `peek` reads `pos[0]` in an `if`'s
  condition and again in its then-arm, and those are **two** sites: only one of them happens, and
  the other's line may be gone by then. Merging them reported `2·B` against `3·B`.
- **Every assumption rounds up, and two of them were rounding down.** A `while` with a `decreasing`
  measure has no loop variable at all, so nothing in it is known to move contiguously and every
  site in it costs a whole line per lap — the first version charged one line for the whole loop. And
  a variable in an index that the *body assigns* — `buf[head]` with `head` stepped inside the loop —
  is not a constant offset; the first version read it as one, and `ring.nt`'s `push_all` lost its
  whole `B·xs.len()` term.
- **The checker was not recording what it resolved, again.** An assignment target's array
  (`xs[i] = v`) is resolved by `sym_find` rather than through `check_expr`, so its type was never
  written into the node table and the cost pass could not ask what its elements weigh. The same
  omission, in the same place, as the one the `work` slice found for a target that is a plain
  variable.

### The one difference that was mine

`vm.nt`'s `run` — a bytecode interpreter dispatching through an `if`/`else if` chain — reported
`3·B·fuel + B` from `neant cost` and `10·B·fuel + B` here, and the two readings of site identity
that could explain it contradicted each other: three is what ignoring the branch gives, and `peek`
needs the branch counted.

It was listed as unexplained rather than excused, and then settled by **instrumenting the Rust
compiler** — `NEANT_DEBUG_SITES=1` prints the site table — instead of reading it again. The Rust
has **eleven** sites for `run`, not three. The three comes from somewhere else entirely:

> **The arms of an `if` are alternatives, so their traffic is the larger and not the sum.**

The same rule `work` follows, which had been in this file for two slices. Sites still carry a
branch tag — two reads of one address on opposite sides of an `if` are two sites, which is what
`peek` needs — but their *costs* combine with `max`, not `+`. Collecting sites into a flat list
and settling them at the end cannot express that, so the moves walk now returns a polynomial per
node and composes exactly as the work walk does.

The lesson is about the method, not the rule: a difference that two readings of the source both
fail to explain is a signal to *measure the source*, and a three-line `eprintln!` behind an
environment variable answered in one run what an hour of reading had not.

## 12. Self-recursion, one call per invocation

`while` left five `work` unknowns, all of them self-recursive. Four are now solved and one is not,
and the line between them is the number of recursive calls per invocation.

`T(m) = f + T(m′)` needs a **measure** `m` the compiler finds rather than reads. The candidates are
`analyze.rs`'s: each scalar parameter, then `xs.len() − i` for an array and a scalar, then a
difference of two scalars — and the first that shrinks at the recursive call wins. Shrinkage is
either `m − m′ = c` for a positive integer (unroll: the answer is `f·(m/c + 1)`) or `b·m′ ≤ m` for
a small integer (halve: the answer is `f·(log m + 1)`, and the polynomial layer grows a `log`
atom whose argument is itself a polynomial, kept in a side table so an atom stays an `i64`).

**Out: two or more recursive calls.** That is `fib` — exponential when the measure falls by a
constant — and the master theorem when it halves. `recur.nt`'s `msum` is the one golden that wants
it, and it stays unknown.

Also out: an `f` that is not a number. All four of the corpus's recurrences have a constant body
cost; a body whose own cost varies with the measure needs the summation `analyze.rs` does, with
each parameter advanced along its own shift.

**71 work columns exact**, 4 differing by design, 1 declined.

### What building it changed

- **A recursive call inside an `if` counts along the heavier arm only.** `bsearch` calls itself on
  each side of a branch and only one of them happens; counting both reads as two calls per level
  and comes out linear instead of logarithmic. `analyze.rs` keeps the arm with more calls and, on a
  tie, the else arm's arguments — which is what makes `bsearch`'s measure `hi − lo` and not
  something that fails to shrink.
- **A shared scratch buffer aliased between a print and its own nested call.** `pol_print` sorts a
  polynomial's terms into an `order` array and then walks it — and printing `log(hi − lo)` calls
  `pol_print` again, *in the middle of that walk*, on the same array. The argument's sort
  overwrote the caller's, and its second term came out as the caller's own:
  `15·log(hi − lo) − lo` where the answer is `15·log(hi − lo) + 15`. The fix is an offset, so a
  nested print writes above the caller's region. The first genuinely re-entrant thing in the
  self-hosted compiler, and it aliased on the first try.

## 13. Regimes: a cost that forks once

The eight golden functions with a piecewise `moves` are the language's distinctive feature made
concrete — decisions §2, "an undecidable fit test forks into regimes" — and five of them have the
same shape:

```
arena.nt  sum_list      moves 16·nodes.len()   if 16·nodes.len() < M
                        moves B·nodes.len()    if 16·nodes.len() ≥ M
words.nt  is_palindrome moves s.len()          if s.len() < M
                        moves 2·B·s.len() − 2·B if s.len() ≥ M
```

A **scattered walk over one array**: an index the compiler cannot follow, or a loop with no
variable at all. This slice already recognises that case and already rounds it up to "a whole line
per touch" (§11). The fork is the other half of the same rule, and it is the half that carries the
information:

> If the array fits in `M`, every distinct element crosses **once** — the footprint. If it does
> not, every touch crosses a line.

`settle_moves` reaches this through a working set computed per loop level, innermost out, and that
generality is what produces `matmul`'s two pieces with two *different* conditions. Here the slice
is narrower and says so.

### The slice: one condition

A cost may **fork at most once**. It is either a single polynomial, or a pair of polynomials under
one condition `ws < M` / `ws ≥ M`. Two costs combine when they are both single, or both forked on
the same `ws`, or one of each; **two independent conditions make it unknown**.

That covers the five single-condition functions above and excludes `matmul`, `stencil` and
`tri.nt`'s `pairs`, which are nested-loop and already outside §11's one-loop boundary. The
narrowing is stated rather than discovered: a fuller piecewise algebra is `piece.rs`'s 304 lines,
with feasibility, pruning by dominance and a cross product on every `add`, and none of that earns
its place until a second condition does.

### The footprint

`ws` for a scattered walk is the array's own size: `elem_bytes · length`, in bytes — which is what
the report prints (`16·nodes.len() < M`), while `analyze.rs` carries it in lines and multiplies by
`B` to display it. This slice carries bytes and compares against `M` directly, because it never
needs the line count for anything else.

### Exit test

The same `moves` column, string for string, now including the pieces and their conditions. The
count in the test goes up by five; a function whose cost forks twice must say unknown.

### What building it changed

**31 `moves` columns exact**, up from 26 — all five single-condition regimes, with no regression
among the twenty-six that already matched. Three things the build changed, and the third is the
interesting one.

- **A cost arena must not be reset per function.** `Ck` indices are stored per function and printed
  at the end, so resetting the arena between functions left every earlier function pointing at the
  last one's costs. `upcase` read `0`. The polynomial arenas are append-only for exactly this
  reason and the new one had to be too.
- **Sites on the same array share its footprint.** Two sites walking one array scattered cost the
  array *once* when it fits, not once per site — `settle_moves` says the same of an arena, "an
  arena's lines are the arena's, however many sites walk it". `is_palindrome` read `2·s.len()`
  where the answer is `s.len()`. On the fitting side forked costs combine with `max`; on the other
  side they still add.
- **The fork is not for every scatter — only when the walk is longer than the array.** Bounding a
  scatter by the array it scatters over buys nothing until lines are revisited, so `analyze.rs`
  forks only when the touches dominate the array's line count, with `B` substituted. Forking
  unconditionally made `ring.nt`'s `push_all` and `vm.nt`'s `run` *worse* — both went from exact to
  unknown, because their walks are bounded by something unrelated to the array they scatter over
  (`xs.len()` touches of `buf`, `fuel` touches of `code`) and the comparison rightly fails.

  That comparison needed the half of `piece::dominates` §7 had left out: the **budget argument**,
  where each positive term of `q` is a budget the terms of `p` consume from monomials that cover
  them. `64·s.len() − 64` dominates `s.len()` only through it. The arithmetic is `f64` here because
  it is `f64` there, and this has to agree rather than merely be right.

## 14. Calls, and what is already in the cache

24 of the 38 declined `moves` columns are functions that call something. The reason they are
declined is one line in `analyze.rs`:

```rust
// credit: what the callee reads that is already resident here pays nothing
```

`repeat.nt`'s `main` builds one array and calls `total` over it **three times**, and pays for the
traversal **once** — after the first call the array is in `M` and the rest is free. Adding the
callees' costs would be wrong by 16 000, which is the whole answer.

So a call needs three things the slice does not have:

- **A signature footprint.** Per array parameter, the byte range the callee touches. For a whole
  walk that is `[0, elem·len)`, which is all this slice will compute: a callee whose sites do not
  cover an array it receives makes the call unknown.
- **A resident set.** What the caller knows is in `M` at this point. It is set *only by a call* —
  `analyze.rs` clears it and refills it from the callee's footprints — so building an array does
  not make it resident, which is why `repeat.nt`'s first call still pays.
- **Credit.** The callee's moves, substituted into the caller's atoms, minus the footprint of every
  argument already resident.

Residency is conditional: the callee leaves its footprint resident only if it fits in `M`. Where
the sizes are concrete — which is every `main` in the corpus — that condition is decided and
nothing forks. Where they are symbolic it forks, and a second, independent fork is unknown (§13).

### Not in this slice

The Rust's credit compares *ranges* with `dominates` in both directions, so a callee reading
`[0, n/2)` of an array the caller left resident over `[0, n)` gets partial credit. Here a footprint
is the whole array or nothing, and a partial overlap makes the call unknown. `saxpy` and `stencil`
are where that will first bite.

### The first attempt, why it was withdrawn, and what the instrumentation said

*(The account below stands as written. What follows it is what happened when the next step it
named was actually taken.)*



Implemented as described — a footprint flag per array parameter, a resident set replaced at every
call, credit subtracted for arguments already resident — it took `moves` from 31 exact to 37, and
got the case §14 opens with **exactly right**: `repeat.nt`'s `main`, `4·B + 16824`, credit and all.

It also got six other `main`s wrong, and the two answers cannot both be produced by any residency
rule this author could construct:

| | `neant cost` | this |
|---|---|---|
| `repeat.nt main` | `4·B + 16824` | `4·B + 16824` |
| `dot.nt main` | `9·B + 896` | `9·B + 4096` |
| `vm.nt main` | `615·B + 248` | `615·B − 56` |

`repeat.nt`'s number requires that **building an array does not make it resident** — otherwise its
first traversal would be free and the total short by 8 000. `dot.nt`'s requires that it **does** —
its `896` is exactly the two small builds, the first `dot`, and the `xs` build, with every later
traversal free, and `xs` is only ever made resident by its build. A rule cannot do both.

`vm.nt`'s negative constant is the same disagreement with the sign flipped: the credit taken
exceeded the traffic there was to credit.

So it was **withdrawn**, not shipped with six differences listed as known. The distinction matters:
`AOS_INSTEAD` and `COPIES_INSTEAD` are differences whose cause is understood and whose direction is
chosen; these are six numbers nobody can account for, and a cost calculus that ships those has
given up the only property that makes it worth having. `moves` stays at 31.

**What to do next is known**, and it is what settled the last difference of this kind: instrument
`analyze.rs` — print the resident set and the credit at every call, the way `NEANT_DEBUG_SITES`
prints the site table — and read the answer off a run instead of constructing rules that fit two
data points and fail on the third.

### Read off a run

`NEANT_DEBUG_CALLS=1` prints `analyze.rs`'s resident set, the callee's footprints and the credit at
every call. One run dissolved the contradiction: **there was none.** `repeat.nt` and `dot.nt` obey
the same rule, and four things had been missing.

1. **A view has a root.** `let v = &xs` makes `v` an alias of `xs`, and `dot(v, v)` is credited
   against what `dot(&xs, &xs)` left resident. Matching residency by *name* rather than root was
   the whole of `dot.nt`'s 3 200-byte error, and the whole of the apparent contradiction: without
   roots, `dot.nt`'s last call looked as if it needed builds to make arrays resident.
2. **The credit is a cross product.** Every footprint against every resident range. `dot(&xs, &xs)`
   has two of each, so the credit is four times the array — and the intermediate really does go
   negative, which is right, because the traffic it cancels was charged at an earlier call.
3. **A walk shorter than a line still crosses one.** `arrays.nt`'s five-element loop covers 40
   bytes; the slide floors at one line, so it costs `B`, not `40`.
4. **An inexact footprint leaves nothing resident.** `ring.nt`'s `push_all` writes `buf[head]` — a
   scatter — so the compiler cannot say what it brought in, and the following `sum_ring(&buf)` pays
   in full. `vm.nt`'s negative constant was this: credit taken for a residency no one had
   established.

And one thing that had nothing to do with calls: **an array built inside a loop is built every
lap.** A site's cost already carries its loop's trip count; a build is not a site, and was the one
contribution the walk had to multiply itself.

**43 `moves` columns exact**, up from 31. Every remaining difference is the AoS/SoA divergence —
the seven leaves and the two `main`s that call them — and there are no unexplained ones.

The method is now twice-proven and worth stating plainly: when two readings of the source
contradict each other, stop reading and make the source say what it does. Both times a few lines of
`eprintln!` behind an environment variable answered in one run what hours of reasoning had not, and
both times the reasoning had produced something confident and wrong.

### Self-recursion for `moves`, almost free

The `work` column's recurrence solver turned out to apply to `moves` unchanged, once one assumption
was dropped: **`f` need not be a number, only independent of the measure.** `B` is a perfectly good
body cost for a recurrence over `xs.len() − i`, and it is exactly what a self-recursive walk's
traffic looks like:

```
sum_from   moves B·xs.len() − B·i + B
bsearch    moves 2·B·log(hi − lo) + 2·B
```

The only other thing needed was the rule the `work` walk already had and the `moves` walk did not:
**recursive calls count along the heavier arm only**. `bsearch` calls itself on each side of an
`if`, and counting both made it two calls per invocation and so out of slice.

**47 `moves` columns exact**, up from 43. Every remaining difference is still AoS/SoA, and the 20
declined are nested loops, calls inside loops, and whole-array reassignment.

### A callee's regime travels to its caller

`moves` 47 → 51, and both steps were about refusals that had outlived their reason.

- **A forked callee hands its condition up.** `call_moves` refused any callee whose own cost was
  piecewise, on the grounds that the caller would inherit a condition and one is all there is. But
  a caller with no condition of its own can simply *take* the callee's, and `ck_add` already
  refuses later if it turns out to have had a different one. Every regime in this corpus reaches a
  `main` this way — `sum_list`, `stencil` and `is_palindrome` are all piecewise and all called — so
  `arena.nt`'s and `tree.nt`'s `main`s came back for free. Where the condition's truth is already
  known at this machine's `M`, it is decided rather than carried, so no report grows a fork it does
  not need.
- **`ys = xs` streams one array.** The moves walk had refused it while the emitter had long since
  decided to copy. The cost is the copy's, multiplied by the trip count when it is inside a loop —
  `reassign_loop.nt`'s `main` is two laps of a three-element copy and comes out exactly. The three
  `main`s where the Rust *proves* the assignment is in place now differ for the same recorded
  reason their `work` columns already did, and the test lets a `moves` difference be excused by
  either list.

**51 exact, 13 differing in two understood families, 12 declined** — and the twelve are three
things: nested loops (`matmul`, `stencil`, `tri`), a call inside a loop (`words`), and `msum`.

### Two more refusals that were doing no work

`moves` 51 → 54, and the boundary is now clean: the nine declines are **nested loops with an array
access in them**, and `msum`.

- **A call inside a loop, into a callee that touches nothing.** The replay `leave_loop` does exists
  to weigh a callee's traffic on the first lap against the later ones. A callee with no traffic has
  none to weigh, and `words.nt`'s `count_words` calls `is_space` on every byte and was declined for
  it.
- **A nested loop with no sites in it.** Refusing every second loop was refusing arithmetic:
  `forsum.nt`'s `main` counts pairs in a double loop and touches no array at all. The depth check
  belongs on the *site*, not on the loop, and moving it there also required the moves walk to bind
  a loop variable to an atom the way the work walk already did — `for j in i..4` has no bound
  otherwise.

What remains is one mechanism, honestly: the per-level working set, which is what `settle_moves` is
for and what `matmul`, `stencil` and `tri` need. Everything cheaper than it is done.

## 15. Two calls per invocation, and the `work` column closes

`msum` was the last `work` decline: `T(m) = 2·T(m/2) + f`, a divide-and-conquer whose two calls
each halve the measure. §12 had put every two-call recursion out on the grounds that it is either
`fib` or the master theorem. It is both, and the two are told apart by *how* the measure shrinks:

- **falls by a constant, two calls** — `fib`. Exponential. Still out, and `neant cost` declines it
  too, so the exit test requires this pass to decline it as well.
- **halves, `a` calls with `a = b`** — each level pays `f` per node, the ratio is `a` and the depth
  is `log_a m`, so the levels sum to `(a·m − 1)/(a − 1)`. `msum` is `a = b = 2` and comes to
  `f·(2m − 1)`: `20·hi − 20·lo − 10` for `work` and `2·B·hi − 2·B·lo − B` for `moves`, both exact.

`a ≠ b` lifts the measure to a fractional power and is out; so is any `f` that varies with the
measure. And **every call must shrink the measure the same way** — `msum` recurses on both halves,
and both are checked, where before only the first was looked at.

**72 `work` columns exact, four differing by design, and no declines at all.** Every in-slice
golden function's `work` is either reproduced string for string or differs for one of the two
recorded reasons. `moves` is at 55, and its eight declines are all one thing: a site inside two
loops.

### The cold/warm replay

`warm.nt` is the golden that exists for one question: what does calling the same function over the
same array twenty times cost? Its `main` does exactly that, once over 1.6 MB and once over 32 MB.

The first lap of a loop finds nothing of the callee's resident and pays in full; every later lap
finds what the previous one left. So the loop costs `cold + (t − 1)·warm`, where `warm` is `cold`
less the callee's own footprints, each credited only where it fits in `M`. The two halves of
`warm.nt` are the two sides of that: the 1.6 MB array is paid for **once** and the 32 MB array
**twenty times**, and `40·B + 675200000` comes out exactly.

**56 `moves` columns exact.** The seven declines are one thing — a site inside two loops — and
`matmul`, `stencil` and `tri` are all of it.

## 16. Nested loops: the per-level working set

Seven declines remain and they are one mechanism. `settle_moves` computes, for each site and each
loop it sits in, the lines that site touches over one full run of that loop — innermost outwards:

```
lines(inner of innermost) = 1
lines(level) = inner × t          if the working set at this level does not fit M
             = inner              if the access does not move with this loop
             = inner × t          if it moves by a whole line or more
             = inner + t·s/B      if contiguous and it moves by s < B per iteration
```

and a site's moves are `lines × B`. Worked by hand for `tri.nt`'s `tiles`, whose answer is
`8·n + 2·B`:

```
for ii in 0..n/4 { for i in ii*4 .. ii*4+4 { s += a[i]; } }

inner (i):   stride 1·8 = 8 < B;  slide = 4·8/B = 0.5, floored to one line
             lines = 1 + 1 = 2
outer (ii):  stride 4·8 = 32 < B; slide = (n/4)·32/B = n/8
             lines = 2 + n/8
moves = lines·B = 2·B + 8·n            ✓
```

The rule is not the hard part. The hard part is `stride` **with respect to each enclosing loop**,
which for the outer level needs the inner loop variable's own bounds expressed in the outer atom —
`i` runs `ii*4 .. ii*4+4`, so `a[i]` moves by `4` elements per lap of `ii`. This slice tracks a
single loop variable and a single trip count; it needs an index as an **affine form in every open
loop atom**, which is `analyze.rs`'s `Affine`.

### `≈` is display, not analysis

Worth recording because it looked like a third piece of machinery and is not. `lock.rs`:

```rust
if p.terms.len() <= 3 { p.display(names) } else { format!("≈ {}", p.leading().display(names)) }
```

A polynomial with more than three terms prints its **highest-degree terms** with `≈` in front; a
condition prints `≈` when its working set has more than one term and only the leading one is shown.
Nothing is approximated — the cost object is exact and the report is short. `stencil`'s two regimes
print the same `≈ 40·n²` because they agree on the leading term and differ below it.

### Multi-condition regimes

`matmul` reports two pieces under two *different* conditions:

```
24·n² + 2·B·n            if ≈ 8·n² < M
B·n³ + 8·n³ + 2·B·n²     if ≈ B·n + 8·n ≥ M
```

which the one-fork `Ck` of §13 cannot carry — it holds one condition and refuses a second. A
general piecewise cost is `piece.rs`: a list of (conditions, polynomial), with feasibility, pruning
by dominance, and a cross product on every `add`. That is the one place in this port where the
Rust's full generality would have to be reproduced rather than narrowed.

### The order to do it in

Per-level lines first, and it alone finishes `tri.nt`'s `tiles` — the only one of the seven whose
answer is a plain polynomial. Then `≈`, which is twenty lines of printing. Multi-condition regimes
last, and only if the two before it do not already show the shape of what `matmul` needs.

### Per-level lines, built

`tri.nt`'s `tiles` comes out `8·n + 2·B`, exactly as §16 worked it by hand, and `moves` reaches 64.
The loop state is now per depth — the variable, the trip, and the **lower bound's node** — because
a site's stride against an *outer* loop is read from the inner loop's bound: `for i in ii*4 ..
ii*4+4` moves `a[i]` four elements a lap of `ii`, and the coefficient chains through.

Two things in the polynomial layer had to be right for the arithmetic to come out, and neither was:

- **The slide divides by the atom `B`, not by the machine's 64.** Dividing by 64 gives `B·n/8`
  where the answer is `8·n` — the `× B` that turns lines back into bytes has to cancel it. The
  *test* (is the slide less than one line?) is still done at this machine's 64; only the value
  carries the atom.
- **A monomial must drop an exponent that cancelled to zero.** `B · B⁻¹` is 1, and leaving `B⁰` in
  the run printed `8·B⁰·n` and, worse, made two equal monomials compare unequal. That is a bug in
  `mono_mul` that nothing had reached before, because until now no polynomial here had ever had a
  negative exponent to cancel.

The nine that remain are `matmul`, `stencil` and `tri.nt`'s `pairs`, whose working sets are
symbolic and fork on a second condition; the `main`s that call them; and the three `main`s where a
callee under SoA leaves a **per-field** footprint resident, which this slice tracks per array.

### A cost as a list of pieces

`Ck` is now `piece.rs`'s shape rather than §13's single fork: a cost is a run of **pieces**, each a
run of **conditions** and a polynomial. `add` is a cross product with infeasible combinations
dropped, `max` is the union, and both prune. **Behaviour-preserving**: 64 exact, 3 differing, 9
declined, exactly as before — which is the point, because the ceiling it removes is not reached
until the level test forks.

Three things the refactor needed that the single fork had hidden:

- **`arenas_taken` belongs at the site, not in the arithmetic.** §13 expressed "an arena's lines
  are the arena's, however many sites walk it" as a `max` on the fitting side of `add`. A general
  piece algebra cannot: `add` there really does add. So the second scattered site on an array now
  contributes **zero** on the fitting side, which is what `analyze.rs` does and what §13 was
  approximating.
- **A condition this machine has already decided is not a regime.** `prune_at`: `128 < M` is a
  fact, and carrying it made `arena.nt`'s `main` read `B + 512 if 128 < M | …`.
- **The prune's tie-break.** A piece is covered when another says at least as much under no more
  conditions — and when the two polynomials *differ*, not only when the conditions do. Getting that
  wrong left both `0` and `B` standing as alternatives, which made every self-recursive function's
  moves look piecewise and so undecidable.

And the compiler outgrew its own arenas: at 267 KB of source it no longer fitted the 256 KiB input
buffer, and `build.sh` exited 5. The pre-sized-array discipline meeting its own limit, in the one
program guaranteed to keep growing.

### The level fork, attempted and not landed

With the piece list in place, the remaining step looked like one change: where the per-level test
finds a **symbolic** working set, fork instead of declining. It was built, and it did not converge.
§16 had estimated one mechanism; it is at least four, and each was found only by the previous one
being fixed:

1. **A condition's working set is in bytes**, and the comparison against `M` must not scale it by a
   line. That one is independently right and is kept — with `pol_num_at_b`, because `2·B` is a
   number on this machine even though it is not a bare constant, and `pol_as_f64` called it
   symbolic and forked on it.
2. **A site that does not move with the inner loop** touches one line however many laps run —
   `a[i]` inside `for j` is the same address every time. The first version handled only the
   contiguous case and declined this one, which is `tri.nt`'s `pairs`.
3. **The outer level sums, it does not multiply.** `for j in i..a.len()` runs a different number of
   laps for each `i`, so the inner level's line count depends on the outer variable and multiplying
   leaves that variable standing in the answer — visibly, as a term printed with no name.
4. **And the condition must be taken at the loop's extreme.** Even summed, the *working set* still
   mentions the outer variable, and `analyze.rs` substitutes its largest value before testing.
   `pairs` still printed a loop atom in its condition after (3).

Four is where it was reverted, with `pairs`' shape still wrong. The state kept is the one that was
verified — 64 exact, 3 differing, 9 declined — plus (1).

**What this says about the estimate.** §16 worked `tiles` by hand and concluded the rule was not
the hard part. That was right about `tiles`, whose inner trip is the constant 4, and wrong about
everything else: a constant inner trip hides (3) and (4) completely. Choosing the simplest example
to validate a design made the design look finished.

`matmul` needs a fifth thing regardless — **symbolic strides**. Its index `i*n + j` has a stride of
`n` elements against `i`, and `idx_coef` returns integers. The Rust's `Affine` carries polynomial
coefficients. That is not a layer on top of the four above; it is a different representation of an
index, and it should be designed rather than discovered.

## 17. Symbolic strides, measured first — and they are not a representation

The previous section ended by saying `matmul`'s `i * n` needs the Rust's `Affine`, a map from loop
variable to **polynomial** coefficient, and that this is "a different representation of an index
rather than a layer on top". That was an estimate, made from the shape of the Rust type and not
from what the Rust does with it. Measured, it is wrong, and wrong in the cheap direction.

### What the Rust actually does with a symbolic stride

`analyze.rs:1285` decides a level from the site's stride, and there are three arms:

```rust
Some(sb) if sb.abs() >= m.b_bytes => (summed, false),   // a whole line or more
Some(sb)                          => (…slide…, contig), // less than a line
None                              => (summed, false),   // SYMBOLIC
```

**A symbolic stride is never compared with `B`, and never forks.** It falls in with "a whole line
or more" — the assumption the section's own doc comment states, that every assumption rounds up.
So a coefficient's *value* is needed only when it is a number; when it is not, the only thing asked
of it is that it is not zero.

The polynomial coefficient is therefore never used as a polynomial. It is used as a three-way tag.

### Checked against the printed answer, by hand

`matmul`'s fitting regime is `24·n² + 2·B·n`, and it decomposes exactly, loops `i`, `j`, `k`:

| site | vs `k` | vs `j` | vs `i` | lines | × B |
|---|---|---|---|---|---|
| `a[i*n+k]` | coef 1 → 8 B, slide `8n/B` | coef 0 → reuse | coef `n` **symbolic** → summed | `n + 8n²/B` | `B·n + 8n²` |
| `b[k*n+j]` | coef `n` **symbolic** → summed | coef 1 → 8 B, slide | coef 0 → reuse | `8n²/B` | `8n²` |
| `c[i*n+j]` | — (not in `k`) | coef 1 → 8 B, slide | coef `n` **symbolic** → summed | `n + 8n²/B` | `B·n + 8n²` |

Sum: `2·B·n + 24·n²`. That is the printed regime, term for term. Every appearance of a symbolic
coefficient in the derivation is a `summed`, and no appearance of one is an arithmetic operand.

### So what `idx_coef` becomes

Not an `Affine`. Today it returns an `i64` and raises one flag, `wst[14]`, meaning "not affine".
That flag is carrying **two different verdicts** at once, and separating them is the whole change:

| verdict | today | wanted | what it costs |
|---|---|---|---|
| a known integer coefficient | returns it | unchanged | compare with `B`: slide, or a line |
| **affine, coefficient not a number** | `wst[14] = 1` | a *second* flag | a line per lap — the `stride ≥ 64` arm, **no region rule** |
| not affine at all | `wst[14] = 1` | unchanged | the region rule: fork against the array fitting |

The distinction matters because the region rule is attached to the wrong one of them. `a[i*n+k]`
against `i` is perfectly affine; bounding it by the array it walks, as a non-affine arena walk is
bounded, would be a different and larger claim than the Rust's.

The rules for a product, which is the only place a symbolic coefficient is born — where `c(e)` is
the coefficient of the variable being asked about:

- either factor not affine → not affine
- `c(a) = 0` and `c(b) = 0` → `0`. **This is a fix, not an extension**: `i*n` asked about `j` is
  constant, and today it falls off the end of `idx_coef` and is called non-affine.
- exactly one factor moves → symbolic
- both factors move → not affine, since the index is quadratic in that variable

One subtlety the Rust gets by construction and this would not. `affine()` returns `None` for the
**whole index** when it is non-affine in *any* variable — `pa.is_const()` is const in all loops at
once — so `a[i*j + k]` is region-ruled at every level, including against `k` where it looks linear.
Asking per variable, as `idx_coef` does, would answer `k` more precisely than the Rust and diverge.
The non-affine verdict has to be **pooled across the open variables** and settled once for the site.
Nothing in the corpus writes `i*j`; that is why it must be written down rather than relied on.

### What it opens, which is nothing on its own

Honest accounting, since §16 was over-optimistic about exactly this:

- `stencil` is depth 2 and needs symbolic strides **and** the outer level's `sum_over` — layer (3).
- `matmul` is depth 3. `site_add` declines at `wst[11] > 2`, and the per-depth loop state is packed
  two slots apart (`wst[30..31]` the variable, `wst[32..33]` the trip, `wst[34..35]` the bound), so
  a third depth collides with the next field. The slots have to be re-laid before the depth rises.
- `tri`'s `pairs` needs **no** symbolic stride at all — its coefficients are 1 and 0. It needs
  layers (2), (3) and (4) and nothing from this section.

So symbolic strides close no column by themselves. They are the cheapest of the five things and
they are a prerequisite for two of the nine declines, which is worth knowing before ordering the
work — and the order that falls out is: layers (2)–(4) first, because `pairs` alone pays for them,
then the slot re-lay, then this, which by then is the small one.

## 18. The working set is per level and across sites, which is why the fork kept failing

`pairs` derived by hand, the way §16 derived `tiles`, against both printed regimes. Loops `i`
(trip `N`, lo 0) and `j` (trip `N − i`, lo `i`); sites `a[i]` and `a[j]`, both inside both loops.

| site | at `j` | at `i` | × B |
|---|---|---|---|
| `a[j]` | coef 1, 8 B → `1 + 8(N−i)/B`, contiguous | coef 0 → but the lines **mention `i`**, so summed: `N + 4N²/B + 4N/B` | `B·N + 4N² + 4N` |
| `a[i]` | coef 0, lines `1` do not mention `j` → `1` | coef 1, 8 B, contiguous → `1 + 8N/B` | `B + 8N` |

That is the **fitting** regime. For the other, the level `i` does not fit, every site is summed,
`a[j]` gives `B·N + 4N² + 4N` and `a[i]` gives `B·N`, totalling `4N² + 2·B·N + 4N` — which is the
printed `4·a.len()² + 2·B·a.len() + 4·a.len()`, term for term.

### The thing the four "layers" were symptoms of

The condition separating those regimes is the working set at level `i`, and it is

```
ws = 1  +  (1 + 8(N−i)/B)        the sum over *both* sites of what each touches per lap
```

substituted at `i`'s extreme — the `i` terms are all negative, so at `lo = 0` — giving `2 + 8N/B`,
or `2·B + 8·n` in bytes, printed `≈ 8·a.len()`. **A working set is a property of a level, not of a
site.** The sites in a loop compete for the same cache, so the test is over their sum.

`matmul` proves this and `pairs` cannot, because `pairs`' two sites happen to share a leading term.
At `matmul`'s level `j` the three sites contribute `1 + 8n/B`, `n` and `1`; the sum is `2 + 8n/B + n`,
which is the printed `≈ B·n + 8·n`. Taken one site at a time they would give `≈ 8·n`, `≈ B·n` and
`≈ B` — three conditions, none of them the one that is printed, and the report prints exactly one.

`site_add` settles a site's moves **at the site**, the moment it is seen, and its comment says why:

> settled here rather than in a pass at the end, so that the moves of an `if`'s two arms can be
> compared: sites in different arms are *alternatives*, and only one of them happens

So it computes the working set from the one site in hand. That is correct wherever a loop holds one
site, which is every column this slice gets exact today — `tiles` has one, and that is why §16
worked and generalised wrongly from it. It cannot be correct where a loop holds two.

**This is what the reverted attempt was actually hitting.** Each of the four "layers" — a site that
does not move with the inner loop, the outer level summing rather than multiplying, the condition
at the loop's extreme — is a rule about *a level*, being fitted one site at a time into a place that
only ever holds one site. They did not converge because the fourth fix cannot be made in that place.

### So the change is one change, not four

Defer the settle. `sites[]` already records everything a later pass needs — `arr`, `idx`, `inloop`,
`coef`, `stride`, `bad`, `trip`, `branch`, `field` — which is not an accident: it is what the Rust's
`Site` records for exactly this pass. What is missing is per-depth loop state kept for the whole
function rather than while the loop is open: the atom, `lo`, the step, the trip. `LoopRec` in the
Rust, four arrays here.

Then, innermost level outwards, for each level:

1. the working set is the **sum** over the sites in that level of the lines each touches per lap
2. substituted at the level's extreme — at `last()` when the variable's terms are all positive, at
   `lo` when all negative, and untested (assume it does not fit) when mixed
3. numeric → decided; symbolic → **fork**, which is where `piece.rs`'s generality earns its place
4. per site: not fitting → summed; coefficient 0 → the same set, itself summed if it mentions the
   level's atom; a numeric stride under a line → the slide, added when contiguous and multiplied
   when not; a numeric stride over a line, or a symbolic one (§17) → summed

and the `if`-arm objection is answered the way the Rust answers it, by grouping on `branch` at the
end and combining arms with `max` — which the sites already carry and which is the Rust's `group`.

`pol_sum_over`, `pol_subst`, `pol_faulhaber` and `mono_exp` all exist. The pass needs no new
polynomial machinery, which is the one thing §16 got right.

### What it closes, stated before building it

`pairs`, `stencil` — the latter also needing §17 — and the `main`s that call them. `matmul` needs
depth 3 as well. Six of the nine declines; the other three are the SoA per-field residency ones and
are unrelated. If it closes fewer than that, the measurement above is wrong somewhere and the place
to look is which sites a level collects.

## 19. What building the settle pass changed

**Done. `moves` goes 64 → 70 exact and the declines 9 → 3**, and the three differences left are the
`main`s where the emitter copies a whole-array assignment the Rust proves is in place — the group
that has differed by design since the start. `matmul`, `stencil`, `tri`'s `pairs` and the `main`s
that call them all settle, regime for regime, including the order the report lists the regimes in.

§18 was right about the shape: the four "layers" were one change, `sites[]` was already recording
what the pass needed, and no new polynomial machinery was required. What it did not predict is that
**the pass is the smaller half of the work**. Six further things had to be right, and five of them
were invisible until a column with two conditions existed to expose them.

### A condition is carried in lines, and this is a correction

The addendum to §16, committed this morning, said a condition's working set is carried in **bytes**,
because bytes are what the report prints. That is now reversed: conditions are in **lines**, as
`analyze.rs` carries them, and `costdump.nt` multiplies by `B` when it prints — which is exactly
what `brief_cond` in `lock.rs` does and what should have been copied in the first place.

The reason is not taste. `dominates` decides whether one working set is at least another by matching
each monomial of the smaller to one of the larger that **covers it exponent by exponent**, and that
relation is *not* invariant under multiplying both sides by `B`:

```
lines:  2·n + n² + 8·n²/B   dominates   2 + 8·n/B + n      ✓
bytes:  2·B·n + B·n² + 8·n²  dominates  2·B + 8·n + B·n     ✗
```

The same two working sets, the same question, two answers. In lines the comparison rules out the
regime where the inner level overflows `M` but the outer one does not; in bytes it does not, and
`matmul` came out with a fourth regime that cannot occur. A unit is not a presentation choice when
the comparison is exponent-wise.

This also exposed a latent bug: `pol_eval` raised a factor to its exponent with `while e < exp`,
which silently does nothing for a negative one, so `8·n²/B` evaluated as `8·n²`. Nothing had ever
asked it to evaluate a polynomial with a negative exponent until conditions were in lines.

### Three things `piece.rs` does to conditions that this had never done

- **`simplify`**: a condition implied by another is not a condition. If the bigger working set fits
  then so does the smaller; if the smaller does not fit then neither does the bigger.
- **`prune_at`'s threshold reading**, which is the one that mattered. When every condition of a
  piece is written in the same size variable, each is a **number**: the smallest `n` at which that
  working set reaches `M`, found by bisection. The conditions are then intervals on `n` — `fits` is
  `n < t`, `≥ M` is `n ≥ t` — of two of the same kind only the tighter says anything, and a piece
  whose interval is empty cannot happen. This is strictly stronger than the symbolic `dominates`,
  and it is what leaves `matmul`'s fitting regime saying `8·n² + 16·n + 2·B < M` **alone** rather
  than beside the looser `B·n + 8·n + 2·B < M`. This compiler had only the variable-free half of
  `prune_at`, which decides `128 < M` and nothing else.
- **The order pieces are listed in**: fewest conditions first, then by polynomial. The polynomial
  order is a `BTreeMap<Mono, Rat>` comparison, and reproducing it meant reproducing `Atom`'s own
  order, in which **`Var` comes before `B`** — where this compiler numbers `B` as 0 and a size
  variable as `3 + i`. `atom_rank` puts them back. `pairs` had the right two regimes in the wrong
  order for an hour before that.

### And two about what a site says

- **Leading terms count size variables only.** `analyze.rs`'s `Mono::degree` filters to `Atom::Var`,
  so `B`, `M` and the `log` atoms contribute nothing, and `4·n² + B·n + 12·n + B` leads with `4·n²`
  alone. This compiler's `mono_degree` summed every exponent, which had never mattered because no
  matching column had a `B`-bearing term below its leading degree. `mono_var_degree` is the fix and
  the ordinary `mono_degree` still orders terms for display.
- **A symbolic stride makes a footprint inexact.** Once `i*n` was affine rather than unreadable,
  `stencil`'s sites looked like walks over a known range, so the call claimed residue and `main`
  credited 512 bytes it should not have. This slice computes a footprint's range from *integer*
  coefficients; a range it cannot state is not exact. `analyze.rs` reaches the same verdict for
  `stencil`'s `src` and prints it: **no residue claimed**.

### The test was comparing against a truncated expectation

`rust_moves` split a regime on `"  if "`, two spaces. The report pads a polynomial into a column, so
a polynomial long enough to fill it leaves **one** space — and that regime was silently dropped.
`pairs`' first regime is 41 characters and was never compared. The harness now splits on `" if "`,
and `pairs` counts as the exact match it was.

That is worth stating plainly: for the length of this work the test could not have failed on a piece
it never read. A test that skips what it cannot parse fails open.

### What is still declined

The three SoA per-field residency `main`s — `arrayview`, `particles`, `structs` — where a callee
leaves a footprint per *field* resident and this slice tracks one per array. Unrelated to nested
loops, and the next thing.

## 20. A footprint is a range, and `moves` has no declines left

**`moves` is 72 columns exact and declines nothing.** The four that still differ are the
`COPIES_INSTEAD` group, where the emitter copies a whole-array assignment the Rust proves is in
place — a divergence of the *code*, not of the calculus, and the only one recorded since the start.
Every column in the corpus is now either reproduced exactly or differs for that one stated reason.

What closed the last three — the `main`s of `arrayview`, `particles` and `structs` — was one line
being deleted:

```rust
if ffoot[si * 16 + p] != 0 && is_soa(types, slay, t) { wst[1] = 1; };
```

The cost pass declined outright whenever a callee touched a struct array the module had laid out as
SoA. It had to, because a footprint here was **the whole array or nothing**: a byte count, matched
to a resident set by array name. Under SoA that is wrong in the expensive direction. `kinetic` reads
`vx` and `vy` of a four-field particle and touches half the array; crediting a later call over the
other half against it would report traffic that does not happen.

### The model that replaces it

The one `analyze.rs` already had, and the one the layout design already described without building:
**the field arrays are laid end to end**, so a field's base is the size of the fields before it, two
fields' ranges are disjoint by construction, and footprint, residue and overlap are all ranges with
no special case for SoA anywhere. A parameter's footprint becomes `[lo, hi)` in bytes per element,
scaled by the argument's length at the call:

```
kinetic     ps: [16·ps.len(), 32·ps.len())    vx and vy
centroid_x  ps: [0, 8·ps.len())               x
sum_all     ps: [0, 24·ps.len())              all three fields of a Point
tagged      cs: [0, 9·cs.len())               an i64 and a u8, 9 bytes per element and no padding
```

and a credit is the **smaller of two ranges, and only when one contains the other**. Two ranges that
merely overlap say nothing this slice can use. That is what makes `centroid_x` pay in full after
`kinetic`: `[0, 8·n)` and `[16·n, 32·n)` are disjoint, so nothing carries over.

### The rule that is order-dependent, and is meant to be

Merging several sites' ranges into one parameter's accepts two cases — the same range twice, or two
ranges where one lies entirely after the other, which becomes the span. Anything else falls back to
the whole array and is **not exact**, and an inexact footprint forfeits the residue for the whole
call.

`particles`' `step` is the case that makes this visible. It touches all four fields, so its range is
the whole array either way — but it meets them in source order as `vx, x, vy, y`, that is fields
2, 0, 3, 1. By the time field 1 arrives the span is already `[0, 32·n)`, which is neither equal to
`[8·n, 16·n)` nor disjoint from it, so the merge fails and `step` claims no residue. `analyze.rs`
prints exactly that: `ps: [0, 32·ps.len()) (whole array)   no residue claimed`.

It would have been easy to "fix" this by testing containment as well, and the answer would then have
stopped matching. The rule is conservative in the safe direction — it can only *refuse* to credit —
and reproducing it mattered more than improving it.

### What is approximated, stated plainly

A site's range here is its **field's whole array**, where `analyze.rs` computes the range the index
actually reaches from the affine form and the loop bounds. On this corpus every walk is
`for i in 0..xs.len()` and the two agree, and the pre-existing AoS path made the same assumption
before this change. A function that walked half an array would have its footprint overstated and its
caller over-credited. That is the next thing this would need if the corpus grew a partial walk, and
it is `site_range` in `analyze.rs` — which is now within reach, since the settle pass already records
each site's coefficient against every enclosing loop and each loop's first and last value.

## 21. Both columns agree exactly, and the divergence list is empty

**76 `work` columns and 76 `moves` columns, string for string, with nothing declined and nothing
differing.** `COPIES_INSTEAD` is empty; so is `AOS_INSTEAD`, deleted earlier; so is the checker's
`NO_MOVES_YET`. The self-hosted cost pass now reproduces `neant cost` on the whole corpus with no
exceptions of any kind.

The last four columns closed by the self-hosted checker learning to make a proof it had been
refusing to guess at. `ys = xs` on whole arrays takes `xs`'s buffer unless something can still
observe that the two became one, and two things force the copy — the same two `types.rs` uses:

- **a loop either name was declared before**, because one lexical assignment stands for every lap
  and the second would read a buffer the first gave away;
- **a view of the source read after the assignment**, which would otherwise be looking at what is
  now the target's.

The verdict is recorded at the statement as `ntys[n] = −2`, a value no type index can be, and the
emitter and the cost pass both read it. That is the point: **a cost is a claim about the code this
compiler emits**, and the only way to keep that true through a change like this is for the claim and
the code to come from the same decision rather than from two that agree.

### Why the list is emptied rather than deleted

Every deliberate divergence recorded in this project has now been closed:

| list | what it held | closed by |
|---|---|---|
| `AOS_INSTEAD` | the emitter always laid struct arrays out as AoS | the compiler choosing its own layout (§ layout design) |
| `COPIES_INSTEAD` | the emitter always copied `ys = xs` | the checker proving in-place |
| `NO_MOVES_YET` | five move and aliasing rules the checker did not have | the checker's move rules |

Each was argued as a *narrowing* rather than a disagreement when it was recorded, and each turned
out to be exactly that. The lists stay as empty constants because the next such narrowing should
land in one and be argued, not absorbed silently into a number.

### The check that mattered

This change makes the compiler emit a **pointer assignment where it used to emit a `memcpy`**. A
wrong decision here does not make a report wrong, it makes a program wrong. The safety net is the
same one the layout choice used: the golden programs go through the whole self-hosted chain and are
run, and `reassign_inplace`, `reassign_copy`, `reassign_loop` and `arrayview` print exactly what
`neant run` prints — including `reassign_copy`, which is in the corpus precisely to catch an emitter
that takes the buffer when a view is still watching.

## 22. The footprint lower bound, designed before building

With both cost columns exact, the largest block of the report still missing is the **lower bound**
lines. Counted over the corpus: **40 footprint bounds, 1 HBL, 4 others.** The footprint one is
almost all of it, needs no syntax this compiler lacks, and rests on machinery the settle pass just
built — so it is next, and the HBL bound (a rational LP over the loop nest) is not.

### What the bound is

`analyze.rs`: *every distinct element of a parameter array crosses once from cold.* Per reference:

1. **`injective_dims`** — the loop variables the index is injective on, by a mixed-radix argument.
   Sort them ascending by unit stride; each unit must be at least `covered + 1`, where `covered` is
   the span the smaller ones already reach. Digits that do not overlap cannot collide.
2. **`image_size`** — how many distinct values that is: summed over each injective loop in turn,
   innermost outwards, exactly `pol_sum_over`.
3. × the bytes one touch covers — the *field's* size for a field access, the element's otherwise.

Keyed by `(root array, field)`, keeping the dominating one where a loop nest is walked twice, and
summed over the parameters. Two references to different fields of one struct array are two entries,
which is why `kinetic` bounds at `16·ps.len()` and `centroid_x` at `8·ps.len()`.

### What the corpus asks for, measured

| shape | bound | needs |
|---|---|---|
| one loop, stride 1, whole array | `8·xs.len()`, `s.len()`, `2·s.len()` | nothing new |
| several arrays or fields | `16·a.len()`, `24·ps.len()`, `32·ps.len()` | keying by (root, field) |
| a non-affine site beside an affine one | `ring`'s `8·xs.len()`, `bfs`'s `8·dist.len()` | a site with no readable index contributes nothing |
| a tiled nest | `tri`'s `tiles` → `8·n` | **the loop's offset** |
| a two-deep nest | `stencil` → `16·n² − 64·n + 64` | `image_size` over two loops |
| symbolic strides | `matmul` → `24·n²` | **the coefficient's value** |

Two of those are worth stating before writing any code.

**`tiles` needs the loop's offset, and the settle pass already computes it.** `for i in ii*4..ii*4+4`
makes `a[i]` injective on *both* `i` and `ii`, because `i`'s own start is `ii*4` — its affine form is
`i + 4·ii`, so the units are 1 and 4, they nest exactly (`4 ≥ 3 + 1`), and the image is
`4 · n/4 = n`. This compiler already chains an inner loop's lower bound into an outer coefficient,
for the stride at each level; the bound wants the same numbers for a different purpose.

**`matmul` needs what §17 concluded was never needed.** That section measured that a symbolic stride
is never compared with `B` and is only ever a three-way tag, so a coefficient's *value* is wanted
only when it is a number. That is true of `moves` and it is **not** true here: `injective_dims` sorts
by unit stride and `image_size` multiplies trip counts, so `a[i*n + k]` needs `n` as a polynomial,
not as a flag. §17 is not wrong — it is scoped to the walk it was about, and this is the first thing
to ask a different question of the same index.

So: a second reader of an index, `idx_coef_pol`, returning a **polynomial** coefficient, used only
by the bound. Not a widening of `idx_coef`, whose three-way answer is exactly right for the walk
that uses it.

### How it will be measured

A **fourth column** in `compiler/costdump.nt` — `name ⇥ work ⇥ moves ⇥ bound` — compared against the
`lower bound … (footprint, …)` line of `neant cost`, the way the first three are. A function the
Rust gives no footprint bound must get none here either; that is the half of the test that keeps a
bound from being invented.

Order: the single-loop case first, since it is most of the corpus; then the offset, which `tiles`
alone pays for; then `image_size` over a nest for `stencil`; then the polynomial coefficient for
`matmul`, last because it is the only one that needs a new reader of the index.

### Which references count, measured — a block's tail is not recognised

§22's table assumed every affine reference in a function contributes to the bound. It does not, and
the rule took four measurements and one instrumented run to state, because three readings of the
source each predicted something the corpus contradicted.

```
fn for_cond(xs: &[i64]) -> i64 {                 fn while_cond(xs: &[i64]) -> i64 {
    let mut n = 0;                                   let mut i = 0; let mut n = 0;
    for i in 0..xs.len() {                           while i < xs.len() {
        if xs[i] > 0 { n += 1; }                         if xs[i] > 0 { n += 1; }
    }                                                    i += 1;
    n                                                }
}                                                    n
                                                 }
        NO footprint bound                               moves 8·xs.len()
```

The same body, the same reference, two answers. It is not `for` versus `while` and it is not the
`if`: **`recognise` is called on a block's *statements* and not on its tail expression.** In
`for_cond` the `if` is the only thing in the loop body, so it is the block's tail; in `while_cond`
the `i += 1` after it makes it a statement. `NEANT_DEBUG_IMG` printed the difference in one run —
`recognise Expr in while_cond` with no counterpart in `for_cond` — after three rounds of reading
`collect_refs` had produced three confident and wrong explanations.

The same rule explains the rest of the corpus without special cases:

- `arrayview`'s `tagged` bounds at `8·cs.len()` and not `9·cs.len()`: the `if` is the loop body's
  tail, so the 1-byte `cs[i].tag` in its condition is never collected, while `n += cs[i].v` in its
  arm is a statement of its own and is.
- `words`' `upcase` bounds at `s.len()` for the same reason, from the arm's assignment alone.
- `while.nt`'s `count_lt` and `first_zero` **do** count their `if` conditions, because `i += 1`
  follows and makes the `if` a statement.

A footprint bound is a *lower* bound, so a missed reference is sound — which is exactly why this
rule has never had to be principled. It is one line to state and one line to reproduce: **a site
inside a block's tail expression does not contribute.** Reproducing it is the cheaper and more
honest choice than diverging, because a divergence here would have to be argued as an improvement,
and "we count a reference `neant cost` forgets" is a claim about an accident, not about a model.

### Built: 90 bound columns, and nothing invented

**Done, and the fourth column agrees everywhere.** 90 functions, counting the ones that must have
*no* bound and get none — a bound invented where `neant cost` states none is as wrong as a missing
one, and only one of those two would otherwise show up as a difference.

The order §22 set held, and each step cost what it predicted:

- the single-loop case, which is most of the corpus;
- **the loop's offset**, which `tiles` alone pays for — `for i in ii*4..ii*4+4` makes `a[i]`
  injective on both `i` and `ii` because `i`'s own start moves 4 a lap of `ii`, the units 1 and 4
  nest exactly (`4 ≥ 3 + 1`), and the image is `4 · n/4 = n`. The settle pass already chained that
  number for the stride at each level; the bound wanted the same number for a different purpose;
- `image_size` over a nest, for `stencil`;
- **the polynomial coefficient**, last, for `matmul`. `idx_coef_pol` is a second reader of an index
  rather than a widening of `idx_coef` — §17's three-way tag is exactly right for the walk that
  uses it, and exactly not enough here.

Two things the build corrected.

**`injective_dims` orders by dominance, not by size.** The first version compared integer units,
which cannot order `1` against `n`. Sorting by `pol_dominates` puts `1` before `n` and `1` before
`4` alike, which is what `analyze.rs`'s comparator does and what the mixed-radix argument needs:
each unit must clear everything the smaller ones already span.

**−1 is not a token, and comparing it as one made `stencil` 2.5× too large.** The images table is
keyed by `(array, field)`, and a whole-element site has no field. Comparing the two `−1`s with
`name_eq` read outside the token array, so `stencil`'s four reads of `src` became four entries
instead of one and the bound came out `40·n² − 160·n + 160` against the wanted
`16·n² − 64·n + 64` — five arrays' worth where there are two. The ratio was what gave it away.

## 23. The footprint report lines, and retiring §20's approximation

51 `footprint` lines, the largest block of the report still missing, and the one that pays twice:
it prints what a caller may credit, and it replaces the approximation §20 recorded rather than
adding a new one beside it.

### What this compiler computes now, and why it is not enough

`foot_flags` gives a parameter's footprint as a **whole field array**: `[base, base + size)` bytes
per element, scaled by the argument's length at the call. §20 said plainly what that assumes —
every walk covers the whole array — and that a partial walk would overstate the footprint and
over-credit its caller. The corpus never showed it because every walk is `for i in 0..xs.len()`.

The report shows it immediately. Half the shapes in the corpus are not whole arrays:

| shape | example | what it needs |
|---|---|---|
| a constant index outside every loop | `parse`: `pos: [0, 8)` | a range at depth 0 |
| an index that is a **parameter**, not a loop variable | `recur`: `xs: [8·i, 8·i + 8)` | the index's constant part as a polynomial |
| a rational index | `recur`: `xs: [4·hi + 4·lo, …)` | `(lo + hi) / 2`, coefficients ½ |
| a two-deep nest | `matmul`: `a: [0, 8·n²)` | symbolic coefficients, already built for the bound |
| a nest that does not start at 0 | `stencil`: `dst: [8·n + 8, 8·n² − 8·n − 8)` | `lo` and `last` per level |
| a tiled nest | `tri`: `a: [0, 8·n)` | the loop's offset, already chained |
| one field of an SoA struct | `particles`: `ps: [16·ps.len(), 32·ps.len())` | the field's base, already recorded |

So the work is **`site_range`**: the byte range one site covers over its nest, as a polynomial.
Everything it needs is already recorded — the per-level polynomial coefficients from §22, each
loop's first and last value, the site's stride, what one touch covers, and the field's base. What
is missing is the index's **constant part**, which is the index with every loop variable set to its
own bound rather than to an atom.

### The rule, corrected

`analyze.rs`'s own `site_range`, with the double count this session removed
(docs/experiments.md): the affine form already carries each loop's start, so a variable's own
contribution runs from `0` to `(trip − 1)·step`, and the sign of its coefficient decides which end
is `lo`.

```
lo = konst + Σ over levels  (coefficient ≥ 0 ?  0  :  c·(trip−1)·step)
hi = konst + Σ over levels  (coefficient ≥ 0 ?  c·(trip−1)·step  :  0)
range = [lo·stride + base,  hi·stride + touch + base)
```

and `konst` is the index evaluated with every loop variable at zero — which is `val_of` of the
index followed by `pol_subst` of each loop atom by zero, not a new reader.

### The merge, the residency line, and the order

Several sites on one parameter merge the way `signature_footprint` merges them, which this compiler
already reproduces on integers and will now do on polynomials: the same range twice is one, two
disjoint ranges become the span, anything else is the whole array and **not exact**. One inexact
parameter forfeits the residue for the whole call, and the line then reads `no residue claimed`
instead of `resident after if … < M`, whose condition is the sum of every parameter's `hi − lo`.

Printed in **parameter order**, which is the order `signature_footprint` sorts by and not the order
the sites were seen.

### How it will be measured, and the risk

A **fifth column**, whitespace-normalised on both sides so the report's column padding is not part
of the comparison. The risk is not the printing: it is that footprints feed residency credit, so
getting a range wrong moves a `moves` column that is exact today. **`moves` staying at 76 is the
test that matters**, and it is a stronger check on this than the new column is — a wrong range that
happens to print plausibly will still mis-credit a caller.

### Built: 89 footprint columns, and the approximation is gone

**Done.** A fifth column, 89 functions agreeing, and `moves` unmoved at 76 — which was the test that
mattered, since footprints feed residency credit and a plausible-looking wrong range would have
shown up there rather than here.

`site_range` replaces §20's whole-array assumption outright. The corpus's shapes that it could not
have stated before, and now does:

```
recur    xs: [8·i, 8·i + 8)                     an index that is a parameter, not a loop variable
recur    xs: [4·hi + 4·lo, 4·hi + 4·lo + 8)     (lo + hi) / 2, coefficients of a half
parse    pos: [0, 8)                            a constant index, at depth 0
stencil  dst: [8·n + 8, 8·n² − 8·n − 8)         a two-deep nest that starts at 1
tri      a: [0, 8·n)                            a tiled nest, through the inner loop's offset
matmul   a: [0, 8·n²)                           symbolic coefficients
particles ps: [16·ps.len(), 32·ps.len())        one field of an SoA struct
```

Two things the build corrected, both about the constant part.

**The index's constant part is the index at the nest's *first iteration*, not at zero.** §23 said
"every loop variable set to zero", copying `analyze.rs`'s `aff.konst` without noticing that its
affine form has already absorbed each loop's start into that constant. Here the loop variable's atom
means the variable's actual value, so the constant is the index with each variable at its own `lo`.
Setting them to zero gave `stencil` `dst: [0, 8·n² − 16·n − 16)` where the range starts at `8·n + 8`.

**And the substitution has to run innermost first**, because an inner loop's start may name the
variable outside it: `for i in ii*4..ii*4+4` substitutes `i := 4·ii`, which puts `ii` *back* into
the expression, and only a later substitution of `ii` takes it out again. Outermost-first leaves a
loop variable in a footprint a caller is supposed to read — which is exactly the shape of the bug
this session found in `analyze.rs`, arrived at from the other direction.

**One narrowing, listed by name.** `parse`'s `number` states no footprint where `neant cost` states
`pos: [0, 8)`. Both compilers call its cost unknown — its `while` carries a `decreasing` measure
neither can follow — but the Rust has recorded the site on `pos` by the time it gives up and this
walk has not, because it stops at the loop it cannot bound rather than walking the body for sites it
will not charge for. Widening it is a change to the walk, not to the footprint, so it is recorded in
`FOOTPRINT_NARROWER` rather than absorbed into the count.

## 24. `#[cost]`, measured before designing

With work, moves, the footprint bound and the footprint all reproduced, what is left in the report
is coupled, and the front end's own slice is now the binding constraint. **37 of the 45 positive
goldens parse**; the eight that do not fail for three reasons only:

| blocker | files |
|---|---|
| a method call or chain | `while`, `chains`, `par`, `par_span` |
| a `#[cost(...)]` attribute | `assert`, `decl`, `decl_extern` |
| a comprehension | `owned` |

`while.nt` is out for a single line of its `main` — `o.iter().filter(|c| c == b'L').count()` — and
nothing else in the file. That is worth knowing before anyone reads "four files need chains" as
four files' worth of work.

### The `gap` annotation is not the cheap win it looks like

It reads as pure arithmetic: the cost over the bound at the machine's `B` and `M`, and both are
already reproduced. It is not, because the *clauses* depend on which bounds exist. `assert`'s
`pairs` prints

```
lower bound  moves 8·a.len()              (footprint, …)   gap 2× if ≈ 8·a.len() < M
lower bound  moves 16·a.len()²/M − M      (HBL, σ = 2)     gap 1048576× if ≈ 8·a.len() ≥ M
```

— one regime each, because `strongest_first` gives each regime to whichever bound leads there. A
footprint bound alone would print both regimes on one line and match neither. **The gap needs the
HBL bound first**, which is the rational LP over the loop nest, for the single corpus line that has
one. Recorded so it is not picked up as a quick job later.

### What `#[cost]` is, measured

Three behaviours, and the rule that selects them is the one thing that had to be measured rather
than read:

**A `#[cost]` is a declaration only when it gives *both* `work_at_most` and `moves_at_most`.**

```
#[cost(work_at_most = "10 xs.len()", moves_at_most = "8 xs.len() + 2 B")]
  both        work ≤ 10·xs.len()   moves ≤ 8·xs.len() + 2·B   declared
              inferred   work 4·xs.len()   moves 8·xs.len() + B   within the declaration

#[cost(work_at_most = "10 xs.len()")]                     ← one only
  only_work   work 4·xs.len()      moves 8·xs.len() + B       exact
#[cost(moves_at_most = "8 xs.len() + 2 B")]               ← one only
  only_moves  work 4·xs.len()      moves 8·xs.len() + B       exact
```

With both, the declaration **replaces** the reported line, the inference moves to an `inferred`
line, and the lower-bound and footprint lines disappear — a caller sees the declaration and nothing
else, which is Stage B's "delete the body" test. With one, it is an **assertion**: the line stays
the inference and a breach prints `✗ … is asserted moves at most X but its moves is Y when <cond>`,
per regime. A bare number with `sizes` is a **budget**, checked numerically at the declared size:
`✗ … has a moves budget of 1024 at the declared sizes but needs 2112`.

`sizes = "xs.len() <= 256"` also appears on the declared line as `sizes xs.len() ≤ 256`.

### What building it needs

1. **The attribute, in the parser.** A prefix on `fn` and on `extern fn` — `decl_extern.nt` declares
   libc's `labs`, which has no body at all, and is the case that shows a declaration is not a
   summary of an inference but a claim in its own right.
2. **A reader for the cost expression**, which is its own small language: `"8 xs.len() + 2 B"`,
   juxtaposition for multiplication, `^` for powers, `.len()`, and `B` and `M` as themselves. It
   produces a polynomial, so everything downstream already exists.
3. **The comparison**, which is `pol_dominates` — already built, and already used for exactly this
   kind of question.
4. **The call path**: a caller must take the declaration where there is one, not the inference.
   This is the only part that is not additive, and it is what `rests on` then records.

Of these only (2) and (4) are new; (1) is small and (3) is done. Order: the attribute and the
reader first, since `assert.nt` and `decl_extern.nt` need no chains and would come into the slice
on those alone.

### Built: the declaration half of `#[cost]`

`#[cost(...)]` parses, and a declaration that gives **both** columns replaces the function's cost
outright. Three files come into the slice — `assert.nt`, `decl_extern.nt` and `err_assert.nt` —
and all four columns stay exact: **work 81, moves 81, bound 95, footprint 94**.

The part that turned out to need no code is the one §24 called "the only part that is not
additive": a caller taking the declaration rather than the inference. The driver already reads
`fw[si]` and `fm[si]` and asks nothing else, so **writing the declaration into them is the whole
change** — `decl_extern.nt`'s `total` charges `labs` its declared 4 and 0 without ever having a
body to look at, which is what a declaration being a claim in its own right means.

The cost expression is its own small language and its reader is small because it produces a
polynomial: juxtaposition multiplies, `^` raises, `B`, `M` and `P` are the machine's own atoms, and
a parameter must be spelled with its `.len()`. Everything after that — dominance, printing, a
caller's view — already existed.

**The assertion half is what remains, and it is a coupling, not a rule.** A `#[cost]` with one
column is an assertion about an inference that still stands, and `neant check` *rejects* a program
whose assertion is breached — so a checker verdict depends on the cost pass. `self_host_check.rs`
runs the checker alone and cannot see one, so `err_assert.nt` is listed in `NEEDS_THE_COST_PASS`
there. Closing it means giving that driver the cost pass, not giving the checker a new rule.

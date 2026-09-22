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

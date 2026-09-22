# neant

A language where what a program costs is a property the compiler infers, reports, and can be asked
to hold — the way a type is.

Correctness has types: `f: A → B` and `g: B → C` compose to `A → C`, and you know that without
running anything. Performance has nothing. Two fast functions composed can be slow, and the only way
to find out is to run it, profile it, guess, and try again. Performance today is where correctness
was before types existed.

```rust
fn score(events: &[Event]) -> f64 {          // work n·c(weight)   moves n/B   exact ✓
    events.iter().map(|e| e.weight()).sum()
}
```

The grey text is the compiler's. It is not written by hand, it appears in the editor next to the
signature, it is recorded in a lockfile that shows up in code review, and it changes when the code
does. Nobody computed it.

## Two facts the theory knows and no language reflects

**Cost is data movement, not operations.** An add is one cycle; a cache miss is a hundred to three
hundred. Every language's implicit cost model — and LLVM's explicit one — counts instructions. That
was right in 1980. The theory that counts bytes moved across cache boundaries has existed for forty
years: the red-blue pebble game (Hong–Kung 1981), the I/O model (Aggarwal–Vitter 1988),
cache-oblivious algorithms (Frigo–Leiserson–Prokop–Ramachandran 1999), communication-avoiding
algorithms (Demmel). It has never been a language's cost model.

**Performance is empirical; correctness is compositional.** A cost that lives in a signature
composes. A cost that lives in a profile does not. Making the first one true is the whole of this
project.

## What the compiler does with a cost

Three things, in order of how often they happen:

**Infer.** Every function gets a cost — work, bytes moved, span — derived from its body. Where a
known lower bound exists for what the function computes (matrix multiply, sort, stencil, scan,
join, a fused pipeline), the compiler compares the two and reports the gap and the schedule
transformation that closes it:

```
matmul           work 10·n³ + 5·n² + 2·n           moves                              exact  (3 regimes)
                                               moves 24·n² + 2·B·n                if ≈ 8·n² < M
                                               moves B·n³ + 8·n³ + 2·B·n²         if ≈ B·n + 8·n ≥ M
                                               moves 8·n³ + 16·n² + 2·B·n         if ≈ B·n + 8·n < M and ≈ 8·n² ≥ M
                 lower bound      moves 8·n³/√M                      (matrix product, Hong–Kung 1981)   gap 13033× if ≈ B·n + 8·n ≥ M; 1448× if ≈ B·n 
                 `b` moves by 8·n bytes per iteration of the innermost loop: a new line every time (line 7)
                 tile by 256      work ≈ 10.0509·n³                 moves ≈ 4.5776e-5·B·n³ + n³/8 if ≈ 8·n ≥ M  …
                 transpose the column operand work ≈ 10·n³                      moves ≈ 16·n³ if ≈ 16·n ≥ M  …
```

That is `neant cost` on the naive triple loop, as it prints today. The function's own line is
**piecewise**: the calculus cannot decide, for a symbolic `n`, whether the column of `b` (`B·n`
bytes) or the whole matrix (`8·n²`) stays in a cache of `M` bytes, so it says what happens in
each case and where the thresholds are, and gives the gap to the Hong–Kung bound per regime —
1448× where the column fits, 13033× where nothing does. A `main` that calls it with `n = 1984`
decides every test with numbers and gets one piece: 6.27e10 bytes, a gap of 1453×, and 1.59e9
after `--apply matmul:tile`. The two suggestions were not looked up: each is the rewrite applied
to the IR and the calculus run again on the result, which is why the transpose is offered with
its real cost and not with a slogan.

## What it looks like

Nothing about cost is in the grammar. The grammar is deliberately unremarkable — expression-oriented,
Rust-shaped — because everything that made earlier drafts of this design ugly turned out to be
analysis information that belongs in the type checker and the editor, not in the source.

```rust
fn matmul(a: Matrix<f64>, b: Matrix<f64>) -> Matrix<f64> {   // n³ · n³/√M   exact, at bound ✓
    let mut c = Matrix::zeros(a.rows, b.cols);
    for (i, j, k) in tiles(a.rows, b.cols, a.cols) {
        c[i, j] += a[i, k] * b[k, j];
    }
    c
}

fn positives(xs: &[f64]) -> f64 {                             // n · n/B   exact, fused ✓
    [x * x for x in xs if x > 0.0].sum()
}

fn build_index(path: Path) -> Index uses io, unbounded {      // ~n^1.02  measured, 1k–10M
    ...
}
```

`for i in 0..n` is ordinary syntax; that its bound must be a size expression is a type check, not
a grammar rule, and the checker tells you when a loop is not one. `while` is ordinary syntax; the
termination measure is inferred when it can be (`i < n` with `i += 1` is most of them) and asked
for when it cannot, with the same frequency and the same mild annoyance as a lifetime in Rust.
Effects go where `throws` goes in Swift and `suspend` in Kotlin.

### The decisions underneath, and what each one buys

Each of these is what it is because a cost has to flow through it.

- **A reference is a view, not an address.** `&xs[i]` exists and reads as you expect; it denotes
  (collection, index), not a location. Nothing in the language can pin a byte to a place. This is
  what lets the compiler own layout — pack hot fields, split cold ones, choose SoA for one loop and
  AoS for another — and owning layout is what makes bytes moved a thing it can bound. It is also
  what makes every value position-independent, so writing a structure to disk or a socket and
  reading it back is a copy of bytes and not a serialization step.
- **Regions are inferred.** Pointer structures — lists, trees, graphs — live inside a region the
  compiler finds by escape analysis (Tofte–Talpin). Inside a region the pointers are free; the
  region's size is known; and if it fits a cache level, a traversal costs one load of the region
  regardless of access order. You write `Arena::with_capacity(..)` only when you want to pin that
  size yourself.
- **Mutation is written one way and the compiler says what it did.** `ys = xs; ys[3] = 9` is the
  same text whether it copies or updates in place. If `xs` is uniquely referenced it is in place and
  the cost says `1`; if it is not, the cost says `n` and names the line that keeps `xs` alive. This
  is Perceus/FBIP exposed as a report instead of a syntax.
- **Operators can be overloaded, and the overload's cost is part of its type.** `Matrix + Matrix`
  is `n²`, and the compiler knows it at every use. The rule is not "no overloading"; it is "no
  operation without a cost."
- **Dynamic dispatch is `dyn`, costed at the maximum over implementations.** Exceptions are
  `Result`; unwinding has no bound. Null is `Option`. Sizes are named by the compiler and appear in
  reports as `xs.len()`, not as a bare `n` you had to declare.

The array-language lineage of the previous project survives in one place: the semantics of the
collection tier. Every operation there has a known cost, programs are compositions, and fusion is
guaranteed — a pipeline that would allocate an intermediate is a type error, not a missed
optimization. `.sum()` means what `+/` meant. The notation is words now.

## Why this is a language and not a library on Rust or Zig

Not for the reasons a first draft of this section gave. Aliasing does not need a language: safe
Rust already has a sound aliasing discipline and hands LLVM `noalias`; forbidding `unsafe` is a
lint. `repr(Rust)` being unspecified is no obstacle to an analyser that sits on rustc, which knows
the layout exactly. And position-independent data is a separate feature that has nothing to do
with cost. Those arguments are withdrawn.

One argument about mechanism survives, narrower than before. The compiler can choose a type's
representation — split a struct into columns, store a declared `f64` as `f32`, pack bits — only
if nothing in the program can hold an address into it. In Rust, `Index` returns `&T` and `Vec<T>`
is *defined* as contiguous `T`: every projection returns an address, so the representation is
fixed by the type's definition and no pass can change it. Making projections not be addresses
means redefining what a reference is, and that is a language decision.

But the real alternative is not a library on Rust. It is an **embedded IR with its own value
world** — MLIR's `tensor` and `memref`, Halide, TVM, Triton, Exo — where values have no
addresses, layout is an attribute the compiler chooses, and aliasing is absent by construction.
They got layout ownership without becoming a general-purpose language, and switching to one of
them costs nothing: the Python and C++ around the kernel stay.

What only a language gets is **scope**. The analysis covers every function, so `costs.lock` is
total and "nobody is silent" is a statement about the program, not about the 5% of it inside a
DSL region; `io` and `unbounded` propagate up the whole call graph, so the tier of `main` means
something. That is the argument, and it is also the argument's strongest objection: scope ends
where the program calls C. A cost signature on a function that calls `read` or `malloc` rests on
whatever is assumed about `read` and `malloc`. The answer is not to pretend otherwise but to make
the boundary a declared, measured and audited thing — an `extern` carries a declared cost, `neant
measure` confirms it on the machine, and every line in the lockfile says what it rests on: the
machine model, an external declaration, or a person's assumption. How much of a program's cost
rests on the boundary is then a number, and that number is the size of this language's reason to
exist.

A correction to the analogy this document opens with. Correctness composes like a type: `f: A→B`
and `g: B→C` give `A→C` with no further information. Cost does not, because the cache is a shared
resource: what `g` costs after `f` depends on what `f` left in the cache. So a cost signature is
not a type but an **effect**: it must say what a function touches (its footprint) and what it
leaves resident (its residue), and composition subtracts the overlap. The calculus is being
rebuilt in that shape (plan, stage A); until it is, the compiler re-analyses a callee at each call
site, which is not composition but whole-program analysis, and is said so.

## Where the pieces already exist

| | has | lacks |
|---|---|---|
| NESL (Blelloch, 1990s) | provable work–span cost semantics | an I/O model; it is gone |
| RAML (Hoffmann) | automatic polynomial resource bounds for an OCaml subset | movement, practicality |
| calf (Harper et al., 2022–) | cost in types, as a logical framework | a language outside a proof assistant |
| Futhark | guaranteed fusion, parallel cost | costs in signatures; CPU as a target |
| cache-oblivious algorithms | the theory, complete | any language; they are library code |
| Tofte–Talpin / MLKit | region inference | a cost model to serve |
| Koka / Lean 4 | Perceus in-place reuse | reporting whether it happened |
| IOLB (Olivry et al., 2020) | automatic, parametric, non-asymptotic **lower bounds** on data movement for any affine program; proofs that a kernel cannot be tiled | the upper side: what *this* program moves |
| Elango et al. (POPL 2015) | lower bounds of a program composed from the bounds of its sub-computations | costs composed from function signatures |
| Bao et al. (POPL 2018) | exact, closed-form cache-miss counts for affine programs in set-associative caches, parametric in the cache | anything outside affine control: calls, `while`, recursion, data-dependent access; a language around it |

The parts are twenty to forty years old. Nobody has assembled them, and the thing that would hold
them together — a cost that is inferred, reported and lockable, always, for every function — has
not been built.

## What this is not

**It predicts movement, not time.** `moves` is bytes across one cache boundary in an ideal cache
of one size `M`. The machine has three levels, a TLB the model does not mention that dominates at
large strides, set associativity that made one tiled product 75× slower at a stride with a big
power of two in it, and a transition from "fits" to "does not" that the model puts at one `n` and
the hardware spreads over an octave. `O(·)` does not capture constants either: a function can meet
its movement bound and lose 2× to a prefetcher or out-of-order effects. The model turns "why is
this slow" from a profiling question into a compositional one; it does not answer "will this be
fast".

**What was verified is shape, not number.** The M1 sweep confirmed slopes and the one transition
that matters — reuse or not — and calibrated a stable constant per access pattern; the counter it
was measured against pairs read streams and does not see write streams, so it was never in the
model's unit. The linear kernels' slopes are near-trivial; the information is in the naive/tiled
separation and in the two rules the data forced.

**The lower bounds are not this project's.** The hand-written catalogue has one entry, the
matrix product, and it is not going to grow by hand: IOLB derives parametric lower bounds for any
affine program automatically, matches Hong–Kung on the product, improves on every other published
hand bound in PolyBench, and proves two kernels untileable. What this compiler owns is the other
side of the gap — what the program as written moves — and `neant cost --iolb` feeds the affine
functions of a program to IOLB for the bound rather than writing bounds down (its first answer:
the hand entry's constant for the product was `5.7×` too small). For code that is not affine no
bound exists, and the compiler says what yours costs and cannot say what it should.

**The exact tier covers less than the demo suggests.** On the four ordinary programs of the M3
corpus — string processing, a stack machine, breadth-first search, a recursive-descent parser —
6 of 11 functions are exact, and 2 of those 6 only because a `decreasing` measure was declared;
5 are unknown and go to the measured tier, which is a profiler with the boundary written down.
Dense affine loop nests, where the calculus is at its best, are also where Halide, TVM and the
polyhedral compilers already are. Whether the exact tier grows past half of ordinary code is the
open question of the next stages, and the number will be kept in this document.

**M0–M3 built the analyser, not yet the language.** Everything so far could have been an analysis
over a Rust subset or an MLIR dialect. The bet that this is a language pays, if it pays, when the
compiler owns representation — stage M4 — and not before. That is the project's first real gate.

The rest is the usual: inference refuses things you know are fine and asks for measures and sizes
you find obvious, the bargain Rust struck with the borrow checker made for time instead of memory.

## Status

Built and measured, 2026-09-22, in `bootstrap/`: a Rust compiler emitting C for the subset
described above; `neant cost`, `lock`, `measure`, `--apply`; the calculus with piecewise costs,
exact loop summation and solved recurrences; the M1 experiment (passed on the second rule set)
and the M2 rewrite measurement (tile: 53× less traffic, predicted 39×). The corpus tier count
above is the honest coverage number. Record of the sweeps: [docs/experiments.md](docs/experiments.md);
the calculus as implemented: [docs/cost-model.md](docs/cost-model.md); decisions with their
reasons: [docs/decisions.md](docs/decisions.md); what is next and in what order:
[docs/plan.md](docs/plan.md).

Next is not M4 but three stages that make the cost object compose: signatures that carry a
footprint and a residue so a callee is never re-analysed (A); declarations first, with budgets in
real units and callers seeing only the callee's declaration (B); and the boundary as a declared,
measured, audited thing (C). Each has an exit test and a kill condition, and all three run on the
subset that exists. M4 stands on A.

This repository previously held a different language of the same name — a k-family array language
with a self-hosted arm64 JIT and a checker that read the emitted machine code to decide whether a
function was constant-time. It reached a verified TLS 1.3 handshake against the public web. It was
retired at commit `4cc1410`, where its README, its five documents and its 28,000 lines remain, and
where the idea that became this project — that how a program compiled should be a value the
compiler answers for, not a log a person reads afterwards — was first tried on something narrower.

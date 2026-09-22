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
matmul           work 10·n³ + 5·n² + 2·n           moves B·n³ + 8·n³ + B·n²           exact
                 lower bound      moves 8·n³/√M    (matrix product, Hong–Kung 1981)   gap 13033× at M = 2 MiB, B = 64
                 `b` moves by 8·n bytes per iteration of the innermost loop: a new line every time (line 7)
                 tile by 256      work ≈ 10.0509·n³  moves ≈ n³/8     [--apply matmul:tile]        gap 23×
                 transpose the column operand      moves 16·n³ + …  [--apply matmul:transpose]   gap 2896×
```

That is `neant cost` on the naive triple loop, as it prints today. The function's own line is the
conservative one — nothing is assumed to fit the cache when the sizes are symbols — and a `main`
that calls it with `n = 1984` gets the same report with numbers: 6.27e10 bytes against a bound
of 4.31e7, a gap of 1453×, and 1.59e9 after `--apply matmul:tile`. The two suggestions were not
looked up: each is the rewrite applied to the IR and the calculus run again on the result, which
is why the transpose is offered with its real cost and not with a slogan.

**Report.** The cost is not in the source. It lives in four places: an inlay hint after the
signature; `costs.lock`, one line per function, committed, diffed in every pull request the way
`Cargo.lock` is; an error when a function falls out of the exact tier, naming the line and why;
and an attribute when you want to lock one:

```rust
#[cost(moves_at_most = "n log n")]
fn sort(xs: &mut [T]) { ... }         // build fails if an edit makes this n²
```

**Never stay silent.** Inference is undecidable in general, so the compiler will not always have an
exact answer. It always has *an* answer, and says which kind:

| the code looks like | work | moves |
|---|---|---|
| bounded loops over arrays, comprehensions, iterator chains | exact | exact |
| higher-order: `map f`, callbacks, combinators | parametric in `cost(f)` | parametric |
| pointer structures inside an inferred region | exact | region-granular bound |
| structural recursion, or `while` with an inferable measure | recurrence | measured |
| input-dependent loops, external calls | effect `unbounded` | measured |

"Measured" means the compiler ran it on the sizes it could, fit a curve, and reports that curve
marked as measured, not proven. A function that falls from exact to measured is a diff in
`costs.lock`, and the error says what to change to bring it back.

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

Neither has a cost semantics. Performance in both is emergent — the reason a program is fast or
slow lives in LLVM, not in the language, and changes with the compiler version. A cost in a
signature needs the compiler to know layout, aliasing and effects for the whole program, and one
`&mut p.x` destroys the first, one raw pointer the second. `repr(Rust)` is unspecified; `Vec`,
`String` and `Box` hold absolute addresses; making a struct position-independent means leaving the
native type system for a parallel one (`rkyv`). None of this is a pass that can be added. It is the
part of the language that decides what a value is.

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

The parts are twenty to forty years old. Nobody has assembled them, and the thing that would hold
them together — a cost that is inferred, reported and lockable, always, for every function — has
not been built.

## What this is not

`O(·)` does not capture constants. A function can meet its movement bound and still lose 2× to
a prefetcher, NUMA, or out-of-order effects the model does not see; the model turns "why is this
slow" from a profiling question into a compositional one and leaves the last factor of two to
measurement, where it has always been. The ideal-cache assumption (full associativity, optimal
replacement) is a proven constant-factor approximation of real hardware, and that constant is
sometimes uncomfortable. Inference will refuse things you know are fine, and ask for measures and
sizes you find obvious — the same bargain Rust struck with the borrow checker, made for time
instead of memory. And costing works best where the lower bounds are known; for an algorithm the
compiler has never seen, it can tell you what yours costs but not what it should.

## Status

Nothing is built. This document is the design, and it exists to be argued with before anything is.

The first thing to build is the smallest possible demonstration of the thesis: a subset with arrays,
bounded loops and iterator chains; inference of work and moves for that subset; a `costs.lock`;
and one lower bound (matrix multiply) with its gap report. If that is not convincing on its own,
nothing downstream of it will be.

This repository previously held a different language of the same name — a k-family array language
with a self-hosted arm64 JIT and a checker that read the emitted machine code to decide whether a
function was constant-time. It reached a verified TLS 1.3 handshake against the public web. It was
retired at commit `4cc1410`, where its README, its five documents and its 28,000 lines remain, and
where the idea that became this project — that how a program compiled should be a value the
compiler answers for, not a log a person reads afterwards — was first tried on something narrower.

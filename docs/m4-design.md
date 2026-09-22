# M4 — structs, views, layout, arenas: the design

The first gate ([plan.md](plan.md)): the point at which being a language, not an analyser, has to
buy something. What it buys is one thing — **the compiler chooses how a struct array is laid out,
because nothing in the program can hold an address into it** — and everything here is either
that or what is needed to state and test it. Where the design narrows what M4 was going to be,
it says so and why.

## 1. What is in, what is out

In: struct types with scalar fields; arrays and slices of structs; field access on values and on
elements; struct values by copy; arrays as values that move and are returned; a layout per struct
type chosen by the compiler and reported, or fixed by an attribute; arenas — struct arrays linked
by indices — and the region rule that bounds a data-dependent walk over one by the arena's
footprint.

Out, deliberately: references to single elements (`&ps[i]`) and to fields (`&p.x`) — a view of one
element would need a fat representation and buys nothing the exit tests need; arrays inside
structs; nested structs; `dyn`; region *inference* in the Tofte–Talpin sense — see §6, there is
nothing to infer when the arena is a named value; in-place reuse of moved arrays (M5).

## 2. Syntax and typing

```rust
struct Point { x: f64, y: f64, z: f64 }
#[layout(soa)] struct Particle { pos: f64, vel: f64, alive: bool }   // the override

fn norm2(p: Point) -> f64 { p.x * p.x + p.y * p.y + p.z * p.z }       // struct by value: a copy
fn sum_x(ps: &[Point]) -> f64 { ps.iter().map(|p| p.x).sum() }        // element field access
fn shift(ps: &mut [Point], d: f64) { for i in 0..ps.len() { ps[i].x += d; } }

let ps = [Point { x: 0.0, y: 0.0, z: 0.0 }; n];      // an array of structs, born here
let q = ps[3];                                         // a copy of one element
let ys = xs;                                           // xs is moved; using it again is an error
fn doubled(xs: &[f64]) -> [f64] { [2.0 * x for x in xs] }   // an owned array is returned
```

- `struct S { f: T, … }` at top level, fields scalar (`i64 f64 bool u8`). `S { f: e, … }` literal, all
  fields given. `v.f` read; `v.f = e` when `v` is `let mut`; `xs[i].f` read and write.
- A struct is a value: passed and returned by copy, like a scalar, whatever the layout of any array
  it came from. A closure parameter over `&[S]` is an `S` value (`|p| p.x`).
- `[T]` as a return type is an **owned array**: `return xs` or a comprehension moves it out. The
  callee's signature carries the returned array's size in its atoms (`doubled` returns
  `xs.len()`), and the caller's local gets that size substituted; a size the callee cannot express
  makes the caller's array size unknown, and loops over it go to the measured tier — the same
  honesty as everywhere else.
- `let ys = xs` **moves** `xs`; passing an array by value (`f(xs)`, parameter type `[T]`) moves it;
  a moved local used again is a type error, decided by a walk over the structured control flow
  (moved on either side of an `if` is moved after it; moved inside a loop body is moved at the
  next iteration, so a move inside a loop of something born outside it is an error).
- Views are as today — `&xs`, `&mut xs`, whole arrays — extended to struct arrays. **No view of an
  element or a field exists.** `&ps[i]` is not syntax. This is the one mechanism claim of the
  README made concrete: projections return values, never addresses, so the representation of
  `ps` is the compiler's.

## 3. Layout: the choice the compiler makes

For each struct type `S` the program has one layout, **AoS** (an array of `S`, C's `struct S[]`)
or **SoA** (one array per field, a slice of `S` being one pointer per field plus a length). One
layout per type program-wide, not per array: a function taking `&[S]` must know what it is
passed, and monomorphising per layout or passing strides at runtime were judged not worth their
cost for M4. It is chosen like this:

1. Analyse the whole module under AoS and under SoA for `S` (with `k` struct types, greedy per
   type in declaration order — each type's choice made with the others at their current
   choice; `2^k` combinations are not tried).
2. For each function that touches `S`, take its moves under each layout. Sum over functions —
   each once; how often they are called is not known at this level, and the sum is a proxy the
   report shows.
3. Choose the layout whose total is **dominated** by the other's (term by term, sizes ≥ 1,
   regimes compared piece by piece under the same conditions). If neither dominates, AoS, and
   the report says the choice was not decided by the model.
4. The attribute `#[layout(aos)]` / `#[layout(soa)]` overrides. Always available; the fallback
   that always works.

The report gets a section per struct:

```
struct Point     layout SoA     program moves 8·n + … (AoS: 24·n + …)     decided by: sum_x, shift
```

What the model sees that makes the choice: in AoS a loop reading only `ps[i].x` moves by 24 bytes
per element and touches every line of the array — `24·n` bytes of lines for `8·n` bytes wanted;
in SoA the same loop is a stream of the `x` column, `8·n`. A loop that reads all fields of each
element sees the reverse: AoS is one stream, SoA three. The cost model already has everything
needed to say this once sites know their **byte stride and their touched bytes** separately
(§5).

The **kill condition** for the layout claim: build the same loop over `[Point; n]` under both
layouts (the attribute forces each), measure refills, and check the ratio matches the predicted
`24/8` within the counter's factors. If the measured ratio does not move with the layout, owning
the layout bought nothing measurable and the M4 gate is failed.

## 4. Emission

AoS: a C `struct Point` and a `struct Point *restrict ps_p, int64_t ps_n` pair, as arrays are now.
SoA: `double *restrict ps_x_p, double *restrict ps_y_p, …, int64_t ps_n`; a slice of `Point`
passes one pointer per field. `ps[i].x` emits `ps_p[i].x` or `ps_x_p[i]` by the type's layout. A
struct value is a C struct in both cases; reading `ps[i]` under SoA gathers the fields into one,
writing `ps[i] = q` scatters. Arrays are still `malloc`ed and never freed in M4; an owned return is
the pair (or the pointer tuple) returned by value in a C struct.

## 5. What changes in the cost model

- A **site** carries, besides its root and affine index, the **field** it touches: `es` becomes
  the field's bytes, and the stride per index step becomes `sizeof(S)` under AoS or the field's
  bytes under SoA. The slide rule (`in + Σ s/B` for a contiguous inner set) then charges AoS's
  over-fetch exactly: a stride-24 walk touching 8 bytes brings `24·n/B` lines.
- Under SoA a struct array has one **root per field** for the footprint and the fit test; under
  AoS one root whose range is the whole element span. The residue rule is unchanged.
- Reading a whole element `ps[i]` is one site per field under SoA (a gather), one site under AoS.
- `[S { … }; n]` streams `n · sizeof(S)` bytes either way.
- The **region rule** (§6) for a site whose index is not affine.

## 6. Arenas, not regions

The plan spoke of region inference for pointer-linked structures. This language has no pointers:
a linked structure is a struct array — an **arena** — whose links are indices into it:

```rust
struct Node { val: i64, next: i64 }                      // next = -1 ends the list
fn sum_list(nodes: &[Node], head: i64) -> i64 {
    let mut i = head; let mut s = 0;
    while i >= 0 decreasing nodes.len() - steps { s += nodes[i].val; i = nodes[i].next; steps += 1; }
    s
}
```

There is nothing to infer about *where* a node lives: it lives in `nodes`. What Tofte–Talpin
would have inferred — that the list's nodes share one allocation — the programmer has written.
Region inference is therefore **not in M4 and not on the plan**; typed indices (`Idx<Node>`, an
`i64` that can only index one arena) are a later convenience.

What is in M4 is the **region rule**, the bound the plan promised for a traversal. `nodes[i]` with
`i` loaded from memory is a non-affine site; today it costs a fresh line every iteration, `trip`
lines. But the site's root has a footprint — `nodes.len() · sizeof(Node)` bytes — and once every
line of an arena that fits `M` has been touched, nothing more is fetched, whatever the order:

```
lines(non-affine site over a loop) = F/B    if F·1 < M and trip dominates F/B      -- the arena, once
                                   = trip   otherwise
```

as a piece under the arena's fit condition, which is the residue rule applied inside one loop. A
list walk of `n` steps over an arena that fits costs the arena; the same walk over one that does
not costs `n` lines. That is the exit line the plan asked for, and it falls out of the machinery
that exists rather than a new one.

## 7. Exit tests

1. **Layout, predicted and measured.** `sum_x` over `[Point{x,y,z}; n]` at sizes past L2, built
   under `#[layout(aos)]` and `#[layout(soa)]`: predicted moves `24·n` vs `8·n`, measured refills
   in a ratio within the counter's factors of 3. And the compiler, unforced, chooses SoA for a
   program that only reads `x`, AoS for one that reads all three, and says why.
2. **The region rule.** `sum_list` and a tree walk (`left`/`right` indices) over an arena: the
   cost line has the two pieces, the arena's footprint where it fits and `n` lines where it does
   not; measured at sizes on both sides of `M`, the refills follow the piece that applies.
3. **Moves.** `let ys = xs; xs[0]` is rejected with the line of the move; `doubled(&xs)` returns
   an array the caller's loops cost with size `xs.len()`; the goldens for everything before M4 are
   unchanged.
4. **Coverage re-count** on a domain corpus written for the purpose — particles, a grid stencil,
   an arena-based tree, a ring buffer — the number the README carries next.

## 8. Order

Structs and field access (typing, AoS emission, sites with fields) → moves and owned returns →
SoA emission behind the attribute → the layout analysis and report → the region rule → the
kernels and measurements → the corpus and the count. Each step keeps the goldens green; the
layout kill test runs as soon as SoA emits.

## 9. Open questions, answered provisionally

- *Per-type layout is coarse.* Two arrays of `Point` with different access patterns get one
  layout. Accepted for M4; per-array layout needs monomorphisation and is a later refinement the
  report will motivate or not.
- *What does a function with `&[Point]` cost when the layout is not yet chosen?* Its line is
  computed under the chosen layout after the choice; the report shows the alternative's moves
  beside it so the reader sees what the other layout would have cost.
- *Structs with a single field, or with padding.* `sizeof` follows C's rules for AoS; SoA has no
  padding. The model uses the real sizes.
- *Does the greedy per-type choice miss a better joint choice?* It can. The report shows the
  per-type totals; if a case appears where it matters, `2^k` over the few types touched by hot
  loops is affordable and can replace it.

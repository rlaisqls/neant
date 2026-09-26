# Arrays by value, first slice

**Written 2026-09-26, before the code; § 6 is what was done.** Plan § Stage D names "argv, modules and arrays by value"
as what a domain corpus cannot be written without. This says what the phrase has to mean here,
what the first slice is, and what the calculus charges for it.

## 1 — What is rejected today, and why

Probed on seed one at `defd2af`:

| program | today |
|---|---|
| `struct S { x: [f64; 3] }` | `a field is a scalar (i64 f64 bool u8); x is not` |
| `fn f(x: [f64; 3])` | `arrays are passed as views: write &[T] or &mut [T]` |
| `fn f(..) -> [f64; 3] { [a, b, c] }` | `an array literal can only initialise a let for now` |
| `let mut b = a; …; a[0]` | `a was moved at line 1; using it again is an error` |
| `a == b` on arrays | `== on [i64; 2]` |
| `[[1, 2], [3, 4]]` | `an array literal can only initialise a let for now` |
| `a = b`, whole | accepted: a move, in place or a copy (m5-design §2) |

An array in this language is a *buffer*: a local owns it, it is moved rather than copied, it is
seen through views, it may be sized by data, and every rule of the calculus (sites, footprints,
residue, the slide rule, layout) is written over it. None of that is wrong for `xs: [f64; n]`, and
none of it should change. What a control loop or a small kernel lacks is the other thing: a
**small, fixed-size aggregate that is a value** — a state vector `[f64; 4]`, a 3×3 matrix — that
is copied when passed, returned whole, and held in a struct. The M4 design put "arrays inside
structs" out deliberately (m4-design §1); this is where they come in.

## 2 — The slice: a fixed-size array field

```neant
struct State { x: [f64; 4], t: i64 }

fn step(s: State, u: f64) -> State {              // a copy in, a new value out
    let mut n = s;                                  // a copy
    for i in 0..n.x.len() { n.x[i] = 0.5 * s.x[i] + u; }
    n.t = s.t + 1;
    n
}

let s = State { x: [0.0; 4], t: 0 };
let s2 = State { x: [1.0, 2.0, 3.0, 4.0], t: 1 };
```

- **A struct field may be `[T; k]`**, `T` a scalar, `k` an integer literal ≥ 1. The struct stays a
  value — passed, returned and assigned by copy — and the array is part of it: copying the struct
  copies the array. This is the whole of "by value": there is no second copy rule, only the
  struct's, which already exists.
- **Access is by element**: `s.x[i]` reads (bounds-checked, like any index), `s.x[i] = e` and
  `s.x[i] op= e` write when `s` is `let mut`, and `s.x.len()` is the literal `k` — so a loop over
  it has a constant trip count and stays exact.
- **Construction**: in a struct literal, an array field is given as `[a, b, …]` with exactly `k`
  elements, or `[e; k]` with the same literal `k` and `e` a literal or a variable (evaluated once
  per element, so it must be free of effects).
- **The array alone is not a value**: `let v = s.x`, `f(s.x)`, `s.x = …` whole, and `&s.x` are
  rejected with a message that says what to write instead. A view of a field would be an address
  into a value (m4-design §2: projections are values, never addresses), and a whole-field copy
  is a second copy rule; neither is needed for the slice.

**Still rejected, with a reason:** an array or slice *of* a struct that holds an array field
(`[State; n]`, `&[State]`). Its layout would be a third case — AoS with an inline array, or SoA
with `k` columns per field — and the layout choice, sites and emission are all written for
scalar fields. The error names the local, the struct and this reason. Local arrays passed or
returned by value, `==` on arrays, and nested array literals stay as they are: a buffer is a
buffer.

## 3 — What the calculus charges

Scalars in a struct live in registers and cost nothing to move (cost-model § Structs). An array
field does not fit in registers in general, so it is charged as what it is, a small block of
memory written:

- **Writing a value array** — `[a, b, …]` or `[e; k]` in a struct literal — costs what `let xs =
  [a, b, …]` costs today: work `k` (one store per element) and moves `k · elem_bytes`, a
  sequential write. A constant, since `k` is a literal.
- **A copy** costs the same as writing it: work `k` and moves `k · elem_bytes` for every array
  field, summed over the struct's fields. A copy is charged where the program names a second place
  for a value that already has one: `let t = s` and `t = s` with `s` a local, and every argument
  passed by value. A value built by a literal or returned by a call lands where it is bound and is
  charged only its construction (C returns such a struct through a hidden slot, written once).
- **An element** `s.x[i]` is a load, work `1 +` the index, moves `0`: the bytes were charged when
  the value was written, and a fixed-size value is resident while it is used. A store is work `1`
  (`3` for `op=`), moves `0`.

This over-charges a copy the C compiler keeps in registers or elides, and the report says
nothing about that; it is the same bargain as `let xs = [1, 2, 3]` inside a loop, which is
charged a stream every lap. The number to watch is that a control loop's cost stays **exact**:
the copy is a constant per iteration, so `for t in 0..n { s = step(s, u) }` is `c · n`.

## 4 — How it is built

The IR gets two expression kinds and one place: `FieldIndex(base, index, field)` for `e.f[i]`,
`ArrayVal(elems)` for an array field's value inside a struct literal, and `LValue::FieldIndex`.
A field's type is `Ty::Array(T, Size::Const(k))`; `StructDef::size` counts it as `k` elements
aligned to one. The emitter writes the field as a C array member (`double x[4];`), so C's struct
copy is the copy; an element is `(s).x[nt_idx(i, 4, line)]`. The cost pass learns the three
charges above in `analyze.rs` and nothing else. `parsedump` reports an array field as outside
the self-hosted parser's slice, so the self-hosting corpus tests skip these goldens.

## 5 — Left

Arrays of such structs (the layout question above); a fixed-size local array passed or returned
by value without a struct around it (`fn f(x: [f64; 3])`), which is the same copy rule on a bare
array and needs a decision on whether `let b = a` then copies or moves; `==` on arrays and
structs; nested fixed-size arrays (`[[f64; 3]; 3]`, today written as `[f64; 9]` with `3·i + j`);
the self-hosted compiler (`compiler/check.nt` would take the same field rule, `emit.nt` the same
C member).

## 6 — Done, 2026-09-26

As designed; nothing in §2–§4 moved in the building. `value_array.nt` steps a `[f64; 4]` state by
value and checks that the caller's copy is untouched (`step` exact at work 29, moves 32: the
`let mut n = s` copy); `value_array_loop.nt` is a 2×2 control loop, `run` exact at `56·n + 2` /
`64·n + 16`; `err_value_array.nt` is the array of holders, rejected. A bounds failure in
`s.x[i]` exits 101 like any index. No existing golden changed. The report still prints a layout
line for a struct that is never in an array ("the model does not decide"), as it did before for
any such struct.

## 7 — A bare `[T; k]`: moved, not copied

**Written 2026-09-26, with part two.** § 5 left the decision open: when a fixed-size local array
is passed or returned without a struct around it, does `let b = a` copy or move? **It moves**, as
it does today for every array. `let ys = xs` on `xs: [f64; 3]` has been a move since M5 and is
charged nothing; making a literal length turn it into a copy would change what existing programs
mean and cost (a use after it, rejected today, would be accepted and charged `k` stores) for the
sake of a distinction the type already draws elsewhere. A struct is a value and is copied; an
array is a buffer and is moved — the literal length does not make it a struct. What the literal
length buys is the signature:

- **A parameter `x: [T; k]`**, `T` a scalar and `k` a literal ≥ 1, takes the array itself. The
  argument must be a variable holding a `[T; k]` of the same length, and the call **moves** it:
  using it afterwards is rejected at the call's line, as after `let b = a`, and so is moving one
  born outside a loop from inside it. A view (`&a`) is rejected with what to write instead. The
  callee owns the buffer: it may move it into a `let mut` and write it, or return it. In C it is
  the pointer and length a view is, so passing it costs what passing a view costs — nothing — and
  an argument that is moved is treated as written for the no-overlap rule, so `f(&a, a)` is
  rejected.
- **A return type `-> [T; k]`** is an owned array whose length is the literal: the body's array
  must have exactly that length, and a caller's `let b = f(…)` has length `k`, not a size of its
  own, so a loop over it stays exact with a constant trip count.
- **`s = f(…, s, …)`**, `s` a `let mut [T; k]` and `f -> [T; k]`: the local takes the call's
  array, as a `let` would. `s` is moved into the call and given a value again by the same
  statement, so it may be done in a loop — this is the control loop over a bare state vector.
  The old buffer's residue and read atoms are dropped: the analysis cannot tell whether the
  callee handed back the same buffer or a new one.

**Cost.** A move is free, as `let ys = xs` is: no copy, no bytes. The callee's reads and writes of
its parameter are charged as reads and writes of any array it was handed — lines, not registers,
unlike an array field (§3), because the buffer is not known to be resident. In `value_array_param`
`scaled` is exact at work 15, moves `2·B`; in `value_array_rebind` a two-element state stepped `n`
times is `14·n + 5` / `2·B·n + 2·B + 16`, exact, the `2·B` per lap being `step`'s own.

**A bug this found.** An owned-array return of a literal (`let o = [a, b, c]; o` in a function
`-> [f64]`) returned a pointer to its stack frame: the literal was a C array on the stack. The
emitter now builds a literal on the heap when its buffer may leave the function — returned, or
moved into a call whose result is returned, through any chain of `let`s, moves and rebindings —
and on the stack otherwise, so no existing program's C changes but that one's. The charge is the
literal's, as before.

The self-hosted parser's slice (the `parsedump` table in `main.rs`) excludes a function with a
by-value array parameter or return, as it excludes an array field, so the parity tests skip these
goldens.

## 8 — `==` and `!=` on a whole value

A struct — every struct, since its fields are scalars and fixed-size arrays of scalars — and a
fixed-size array of scalars held in a variable compare with `==` and `!=`, element by element and
field by field, with each element's own `==`: `-0.0 == 0.0` holds and a `NaN` makes the whole
comparison false, as it does for one `f64`. An array whose length is data (`[0; n]`), a view, and
an array of structs are rejected with that reason; the two sides must have the same type, so two
arrays of different literal lengths are rejected as any `==` between two types is.

**Cost.** The comparison always looks at every element — the emitted C folds each element's
result into one flag with no early exit — so it is a constant, and exact:

- a fixed-size array: two loads and a compare per element, the conjunction folded into the
  compare, so work `3·k`, and both arrays' element bytes read, moves `2·k·elem_bytes`;
- a struct: one compare per scalar field, which is in a register, and two loads and a compare per
  element of each array field, with no bytes — an element of a value array is resident while the
  value is used (§3).

In `value_eq`, `same` on two `[i64; 4]` is work 12, moves 64; `moved` on a `Pose { x: [f64; 3],
id: i64 }` is work 10, moves 0, and the two struct arguments it is passed are charged as the
copies §3 says they are, in `main`.

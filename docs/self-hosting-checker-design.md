# Self-hosting: the checker

Third stage, after `compiler/lex.nt` and `compiler/parse.nt`. `bootstrap/src/types.rs` is 964
lines and is not one thing: it resolves names, checks types, desugars chains into loops, tracks
size variables for the cost model, checks moves and uniqueness (M5), and builds the typed IR the
cost calculus and the emitter both read. **This slice is the first of those, and only the first:
name resolution and type checking**, over exactly the grammar `parse.nt` produces.

## 1. What this slice answers, and what it does not

In: does a program type-check, and what type does each expression have. Out, each because it
serves a stage that does not exist self-hosted yet, not because it is hard:

- **Size variables.** `Ty::Array(elem, Size)` in the Rust checker carries a size because the *cost
  model* needs it. Nothing here consumes one, so an array's type is its element type alone, and
  `check_declared`'s "declared `[i64; 5]` but the value has 3 elements" check is out with it.
- **Chain desugaring.** `parse.nt` does not parse a chain (parser design §6), so there is nothing
  to desugar.
- **Moves, uniqueness, roots, layout.** M4/M5 machinery, all of it downstream of a typed IR this
  slice does not build.
- **The typed IR itself.** This slice answers a question; it does not yet hand a data structure to
  a next stage, because there is no next stage self-hosted. When there is, `Node` grows a `ty`
  field and this becomes the pass that fills it.

## 2. Types: a small arena, scalars interned

```rust
struct Ty { kind: i64, elem: i64, mutable: i64 }   // kind: 0 i64  1 f64  2 bool  3 u8  4 unit
                                                    //       5 array  6 slice (elem: index into this arena)
```

The five scalar/unit types are allocated first, once, so their arena indices are always `0..4` and
a scalar comparison is an integer comparison. Arrays and slices are allocated as met and compared
structurally (`same_ty` recurses on `elem`, and a slice's `mutable` participates only where the
Rust checker lets it: `&mut [T]` may be passed where `&[T]` is expected, not the reverse).

## 3. Names: one flat table, a scope is a saved length

No hash map, and none needed. One array of

```rust
struct Sym { name: i64, ty: i64, mutable: i64 }     // name: the identifier's token index
```

appended to as `let`s and parameters are declared, searched **backwards** so an inner declaration
shadows an outer one, and compared by the *source bytes* the two name tokens span (`lex.nt` kept
`(start, len)` into `src` rather than copying identifiers, so this is a byte loop, and the checker
needs `src` as a parameter for it).

A scope is not a data structure: **entering one saves the table's current length, leaving one
restores it.** A block, a function body, and a `for`'s loop variable each do exactly that. This is
the same trick the arena pattern uses everywhere else — the array is the scope stack.

## 4. Signatures first, bodies second

Two passes over the functions, as the Rust checker does, so a call may name a function declared
later:

```rust
struct Sig { name: i64, params: i64, n_params: i64, ret: i64 }   // params: index into a flat param-type array
```

The parameter-type array is flat and contiguous per function — unlike the parser's child lists,
signature resolution is one non-recursive loop over functions, so nothing interleaves.

## 5. The rules, and where each negative golden lands

The rules this slice implements are the ones `tests/golden`'s in-slice negative cases test, plus
what the positive ones need:

| rule | the golden that fires it |
|---|---|
| a call's argument count matches its signature | `err_arity.nt` |
| `break` only inside a loop | `err_break.nt` |
| an `if`'s two branches have one type | `err_ifty.nt` |
| arithmetic needs both sides the same type; no implicit conversion | `err_mix.nt` |
| assignment needs a `let mut` target | `err_mut.nt` |
| a function returning `T` has a body whose value is `T` | `err_ret.nt` |
| an index is `i64`, the base is an array or slice, the result is the element | `arith`, `forsum` |
| a `for` range is `i64`; the loop variable is an immutable `i64` | `forsum`, `fib` |
| `while`'s condition is `bool`; `!` needs `bool`; comparison gives `bool` | `ifexpr` |
| `as` converts between `i64`/`f64`/`u8` only | `arith` |
| `println` takes one scalar and gives `()`; `min`/`max` take two of one numeric type | every file |

## 6. Exit test

**Passed** (`compiler/check.nt`, `bootstrap/tests/self_host_check.rs`). Two halves, because there
is no way to compare *inside* the Rust checker without writing a second one there:

- **Verdict parity, on every in-slice file.** `neant check <file>` already answers accept/reject
  for the real checker; the self-hosted one must agree on all of them — which includes the six
  negative goldens above, each of which fires a different rule. This is the half that catches a
  rule being missing or too permissive.
- **Types, pinned.** The self-hosted checker prints each expression's type in `parsedump`'s own
  depth-first order, as a small canonical code (`0..4` scalar, `10+k` array of `k`, `20+k`/`30+k`
  shared/mutable slice of `k`), and that output is pinned as a golden for a few files and
  hand-checked once. This is the half that catches a rule being *wrong* rather than absent —
  accept/reject alone would not notice `1 + 2` typed as `f64`.

## 7. What building it changed

- **A node-type table, sooner than §1 said.** §1 put "the typed IR" out of scope on the grounds
  that no stage consumes one yet — but the exit test's second half does, so `check_expr` records
  every expression's type into a `ntys` array indexed by node. That array *is* the beginning of
  the typed IR, arrived at from the test rather than the design. Blocks are recorded inside
  `check_block` rather than by `check_expr`'s wrapper, since a function body, a loop body and an
  `if` arm reach one directly without being an expression themselves.
- **The `if`/unary-minus parse quirk again.** `scalar_named`'s last `if … { return 3; }` followed
  by `-1` on the next line parsed as a subtraction, exactly as self-hosting-design.md §5 recorded
  from the lexer. Same fix, an explicit `;`. It is worth treating as a language question rather
  than a recurring papercut the next time the grammar is open.
- **Twenty probes, not just the corpus.** The corpus's six in-slice negative goldens each fire one
  rule; the probes add the pairs the corpus has no file for — `&mut [T]` passed where `&[T]` is
  wanted (accepted) and the reverse (rejected), writing through a `&[T]` (rejected) and a
  `&mut [T]` (accepted), an index that is not `i64`, an element type that does not match the
  return type, an undeclared name. Each is a bad form and its fixed form, and the verdict must
  flip; all twenty agree with `neant check`.

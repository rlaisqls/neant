# Self-hosting: arrays and slices, and what it takes to read `emit.nt`

With structs in, the self-hosted front end parses and type-checks `lex.nt + parse.nt + check.nt`
([self-hosting-structs.md](self-hosting-structs.md) §4). `emit.nt` is the one stage still outside,
and the measurement is unusually precise: it needs exactly **two** things the slice does not have.

```
$ grep -c '\.len()'          compiler/emit.nt   →  1
$ grep -c 'let [a-z_]* = b"' compiler/emit.nt   →  79
```

`.len()`, once, and a byte-string literal bound by `let`, seventy-nine times — every C keyword the
emitter writes costs one (emitter design §1). Nothing else. So this slice is not "arrays" in
general; it is the smallest set of array machinery that makes the compiler readable by itself, and
the pieces that follow from it because C has no other way to say them.

## 1. The representation is not a choice

`bootstrap/src/emit_c.rs` line 1: *every array or view is two C variables, `name_p` and `name_n`.*
A parameter of array or slice type becomes **two** C parameters. The self-hosted emitter has to use
the same convention, not for tidiness but because `bootstrap/rt.c` already does — `read_file(path_p,
path_n, buf_p, buf_n)` is four C parameters for two neant views, and getting that wrong is the bug
that made `write_file` drop its length (emitter design §6).

So:

| neant | C |
|---|---|
| `xs: &[T]` (parameter) | `const T *xs_p, int64_t xs_n` |
| `xs: &mut [T]` (parameter) | `T *xs_p, int64_t xs_n` |
| `let xs = b"…"` (local) | `static const uint8_t xs_buf[] = "…"; const uint8_t *xs_p = xs_buf; int64_t xs_n = N;` |
| `let xs = [e; n]` (local) | `int64_t xs_n = n; T *xs_p = nt_alloc(n, sizeof(T));` and a fill loop |
| `xs[i]` | `xs_p[nt_idx(i, xs_n, <line>)]` |
| `xs.len()` | `xs_n` |
| `&xs` / `&mut xs` (argument) | `xs_p, xs_n` — two arguments |

`nt_idx` is the bounds check the Rust emitter emits by default (`opts.checked`), and the
self-hosted emitter emits it unconditionally: there is no `-O` flag to thread through, and
`tests/golden/bounds.exit` says an out-of-range index exits 101 with a message. Behavioural
equality is the exit test, so the check is not optional.

## 2. `.len()` is a method call, and stays the only one

The parser rejects `.name(` as "a method call or chain" (parser design §6), and that rejection is
load-bearing — it is what keeps chains, closures and comprehensions honestly out. `.len()` is
carved out by name and by shape: **`.len` followed by `()`, and nothing else**. Any other
`.name(` is rejected exactly as before. It becomes its own node kind (72 `Len`) rather than a
general call, so nothing downstream has to ask whether a call is a method.

This is a narrowing to record, not a feature: the self-hosted parser will accept `xs.len()` and
reject `xs.count()` with "a method call or chain", which reads oddly until you know that `len` is
the only one the compiler's own source uses.

## 3. Array literals only where the language already allows them

The language restricts an array literal to a `let` initialiser ("an array literal can only
initialise a `let`") — the restriction that shaped the lexer's byte-at-a-time keyword matcher and
then the emitter's `let s = b"static ";` idiom. The slice takes exactly that position and no other:

- `let s = b"…";` — the 79 uses, type `[u8; N]`. Emitted as a C string literal into a `static
  const uint8_t[]`, which is both shorter than a brace list and the thing C is good at.
- `let xs = [e; n];` — the repeat form. Not used by `compiler/*.nt`, but every **driver** is built
  from it (`[b'\0'; 262144]`, `[Token { … }; 262144]`, `[0; 4096]`, `[-1; 262144]`), and a driver
  is what makes the stages a program. A fixed-size C array, filled by a loop.

A brace list (`[1, 2, 3]`) is **out**: nothing in the compiler or its drivers writes one, so it
would be untested code, which is the same reason the emitter design §4 kept arrays out entirely
until now.

`n` in `[e; n]` is any `i64`, and the array is **heap-allocated** — `nt_alloc(n, sizeof T)`, the
same helper and the same shape the Rust emitter uses — not a C automatic array. Fixing `n` to a
literal and declaring `T xs_buf[n]` would have been simpler and would have diverged from
`neant check`, which accepts `[0; n]` for a variable `n`; it would also have put a driver's
`[Token { … }; 262144]` on the stack, which is 6 MiB of it.

## 4. Arrays of structs are AoS, always

`[Token { … }; 262144]` is an array of structs, and the Rust compiler decides AoS or SoA from the
cost model (M4). **The self-hosted emitter always emits AoS.** The layout decision belongs to the
cost calculus, which is not self-hosted, and choosing it without one would be a guess dressed as a
decision.

This is safe for the exit test precisely because the test compares *behaviour*: a program whose
arrays the Rust compiler lays out as SoA prints the same numbers when the self-hosted compiler lays
them out as AoS. It is not safe for anything that compares emitted C, which is why nothing does.

## 5. What the checker already has, and the one thing it does not

Most of this needs no checker work: `ByteStr` is already typed `[u8]`, `&x` of an array already
gives `&[T]` with the right mutability, indexing already requires an `i64` and yields the element
type, and the `&mut [T]`/`&[T]` argument direction is already tested in both directions
(checker design §7). The additions are small:

- `.len()` on an array or a slice → `i64`, and its receiver must be a **variable** — the same
  restriction indexing already carries, and the Rust checker's own message says so ("the receiver
  of `.len()` must be a variable for now"), so `b"abc".len()` is rejected by both.
- `[e; n]` → an array of `e`'s type, for any `i64` length.

The **name question stays open.** Locals are still emitted by source spelling (emitter design §3),
and now they are emitted as *two* names built from that spelling, so `let s = b"a"; let s = b"b";`
in one block produces a duplicate `s_p` rather than a duplicate `s`. Same failure, same place —
loudly, in `cc` — and the same fix, which is for the checker to record which declaration each
`Var` resolved to. `emit.nt` shadows nothing in a block, so this slice does not need it; the next
one should stop deferring it.

## 6. Exit test — and what it turned out to reach

Planned as two, both existing tests moved rather than new ones:

1. `the_self_hosted_front_end_checks_its_own_source` gains `compiler/emit.nt`: all four stages
   concatenated, parsed and type-checked by the self-hosted lexer, parser and checker. **Passed.**
   The whole self-hosted compiler, about 2000 lines, read by itself.
2. `self_hosted_emit_runs_the_same` gains `tests/golden/arrayview.nt` — a repeat array, an array of
   structs, `.len()`, indexing, the same array as `&[T]` and `&mut [T]`, and a whole-array
   reassignment — and its output must equal `neant run`'s byte for byte. **Passed**, and the
   comparison went from 7 golden programs to 18, because arrays are what most of the corpus is
   made of. Three goldens are now named in the test rather than left to a count.

The paragraph that stood here said emitting the compiler's own source was "the next measurement,
not this one." It was the same one. `the_self_hosted_emitter_emits_its_own_source` runs the whole
chain over all four stages, writes 160 KB of C, and `cc` compiles it — 92 functions, no
diagnostic. **The self-hosted compiler emits itself.**

It stops at `cc -c`. Linking needs `main`, and `main` lives in a driver whose first line is
`extern fn read_file(…)`; `extern` is still outside the parser's slice, so the compiler cannot read
the handful of lines that turn its four stages into a program. That is the entire remaining gap
between here and a real fixpoint, and it is now a `-c` in a test rather than a paragraph.

## 7. What building it changed

- **Size atoms are not only the cost model's.** Design §1 of the checker put size variables out of
  the slice because "only the cost model wants them". Wrong: `ys = xs` on whole arrays type-checks
  exactly when the two lengths are known to agree, and the corpus has one golden that must be
  accepted (`reassign_named_size.nt`, two arrays from the same immutable `n`) and one that must be
  refused (`err_reassign_mutable_size.nt`, the same source with `n` mutable and changed between).
  A checker with no atom at all agrees with `neant check` on neither. `Ty` grew a `size` field
  holding three disjoint ranges — a declaration's token, a literal's value, or a fresh counter.
- **The one place a number had to be read out of a token.** The lexer keeps a numeric literal's
  span and never parses it, and the emitter copies the span straight through to C, so nothing had
  needed the value until an integer literal became a size atom. `int_value` is that one place.
- **`let mut s = b"…"` cannot be `static const`.** The first version put every byte-string literal
  in `.rodata`, which is right until `upcase(&mut text)` writes to it. `words.nt` printed nothing
  at all — a segfault, not a wrong answer. A mutable binding now gets an automatic array that C
  initialises from the literal on each entry.
- **An array variable is two names everywhere, including when it is just passed on.** `keyword(src,
  start, len)` inside the lexer passes its own `&[u8]` parameter through, and the first version
  emitted the bare name. Caught by `cc`, not by a test, because no golden re-passes a view.

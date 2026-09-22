# Self-hosting: the emitter, and the first end-to-end run

Fourth stage, after `lex.nt`, `parse.nt` and `check.nt`. This is the one that makes the other three
mean something: with it, a neant program compiles a neant program **and the result runs**, so the
exit test stops being "the same node kinds" and becomes "the same output from the same program."

## 1. Writing text, from a language with no strings

The emitter's output is C source — bytes, built up, then handed to `cc`. Three things it needs
that no earlier stage did, and none of them is a language feature:

- **A place to put bytes**: `out: &mut [u8]` plus a length in the state, the pre-sized-array
  discipline again, bounded by nothing in particular — C output is a few times the source, and the
  buffer is simply made generous.
- **Fixed strings**: `let s = b"static int64_t "; emit_bytes(out, ost, &s);`. A byte-string literal
  cannot be an argument *inline* ("an array literal can only initialise a `let`"), but it can be
  bound and passed by reference, so every C keyword costs one `let` and one call — not the
  byte-at-a-time spelling the lexer's keyword matcher needed.
- **A way out of the process**: `write_file(path, buf, n)` in `bootstrap/rt.c`, the mirror of
  `read_file` and matching the same pointer+length convention.

Numbers: an `i64` is emitted by the usual divide-by-ten loop. A **float literal is emitted by
copying its source span** — `lex.nt` kept `(start, len)` and never parsed the value, and C accepts
the same spelling, so the `f64` bit-cast deferred twice is not needed here either. An integer
literal copies its span too, minus any `_` separators, which C does not take.

## 2. What the emitted C looks like, and what it borrows

The same shape `bootstrap/src/emit_c.rs` produces, because it has to interoperate with the same
`cc` and the same expectations:

- A fixed prelude: the includes, `nt_println_i64` and friends. Emitted as one byte-string literal.
- `static <ret> ntu_<name>(<params>)` per function, prototypes first so order does not matter —
  and `ntu_`, not `nt_`, for the reason the Rust emitter now also uses it (a user function named
  `alloc` collided with the runtime's own).
- `int main(void) { ntu_main(); return 0; }` at the end.
- **`if` and a block used as expressions** become GNU statement expressions with a temporary,
  `({ int64_t t3; if (c) { t3 = …; } else { t3 = …; } t3; })` — the same trick the Rust emitter
  uses, and the reason its output only ever targets gcc and clang.

## 3. What the emitter needs from the checker: names, not just types

A local's C name cannot simply be its source spelling: neant allows `let x = 1; let x = 2;` in one
block and C does not. The honest fix is for the checker to record, per `Var` node, *which
declaration* it resolved to, and for the emitter to name locals after that declaration's token —
which is what a real pipeline does, and one more column of the typed IR the checker started
growing when it began recording types.

**Not done in this slice.** Locals are emitted by their source spelling, and same-block shadowing
is therefore **not supported — it fails loudly**, as a duplicate declaration `cc` rejects, not as
wrong code. Nested-block shadowing works, because a neant block becomes a C block. Recorded here
so the next pass on the checker knows what to add and why.

## 4. The slice

In: functions taking and returning scalars (`i64`, `f64`, `bool`, `u8`) or `()`; `let`, assignment
and compound assignment, `for`, `while`, `break`, `return`, expression statements; every operator
the checker's slice types, `as`, calls, `println`, `min`/`max`; `if` and blocks as both statements
and expressions.

Out: slices and arrays as parameters — the checker types them, but nothing in the corpus can
*build* one to pass (array literals are outside the parser's slice), so emitting them would be
untested code. Also out, as before: structs, chains, closures, comprehensions.

## 5. Exit test: the strongest one available

For every `tests/golden/*.nt` file inside the slice:

1. `neant run <file>` — the Rust compiler's output.
2. The self-hosted chain reads the same file, emits C to a temporary, `cc` compiles it, and it runs.

**The two outputs must be byte-identical.** Not a tree, not a token sequence — the program's own
behaviour, which is the only comparison that cannot be fooled by two implementations agreeing on a
representation and disagreeing on what it means.

## 6. What building it changed

**Passed** — seven programs (`arith`, `baddec`, `block`, `fib`, `forsum`, `hello`, `ifexpr`) go
through the self-hosted chain and print exactly what `neant run` prints
(`bootstrap/tests/self_host_emit.rs`). Three things the build itself produced:

- **`write_file`'s signature was wrong in the way §1 of the *first* design warned about.** Declared
  `write_file(path: &[u8], buf: &[u8], n: i64)`, a neant view passes as *two* C arguments, so the
  C side receives `(path_p, path_n, buf_p, buf_cap, n)` — five, not four. Written with four, `n`
  was silently dropped and `buf_cap` used instead, so the first run wrote the whole 256 KiB buffer,
  padding and all. The same convention mismatch that made calling libc's `open` directly
  impossible, this time inside the shim written to avoid it.
- **Fixed strings cost one `let`, not one call per byte.** `let s = b"static "; emit_bytes(…, &s);`
  — the restriction is on an array literal as an *inline argument*, not on a bound one, which is
  why the emitter reads nothing like the lexer's byte-at-a-time keyword matcher.
- **Nothing needed a float parsed.** A float literal is emitted by copying its source span, so the
  `f64` bit-cast deferred in the lexer and again in the checker is still not needed. The one place
  a value is read back out of a token is a byte literal's `ival`, which the lexer did decode.

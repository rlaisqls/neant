# Self-hosting: the design for a first step

plan.md's "Self-hosting" section is one paragraph: the compiler in neant, the Rust compiler frozen
as seed one, `bootstrap/neant.c` as seed two, a two-seed fixpoint in CI — "a goal the author holds,
not a proof of anything: the compiler is the program this language is worst at" (tree-shaped,
string-heavy, `while` over tokens), and "Not scheduled: … strings and I/O beyond the harness." That
tension — the goal needs exactly what was explicitly not scheduled — is the first thing this design
has to resolve, and it resolves narrower than it sounds: no new syntax, one small build-system
addition, and a staged order that starts with the smallest stage, not the whole compiler.

## 1. Can the current language write a compiler at all?

Checked against `bootstrap/src/{lex,parse,types,ir,emit_c}.rs` (~7\,400 lines of Rust), stage by
stage, for what each would need and whether M0–M5 already provide it.

- **"Strings."** Never needed as a first-class type. Source text, identifiers and token spans are
  all *fixed data read once* — a byte array (`[u8]`, already real: byte-string literals, `.len()`,
  indexing) is exactly what a lexer wants, and an identifier is a `(start, length)` pair into the
  source buffer, a view by offset instead of by address — the same idea `&[T]` already is, one
  step more literal. No language feature is missing here; the "not scheduled" line was about a
  general `String` type with growth and concatenation, which this does not need.
- **Growable collections.** Never truly needed either, for the same reason M4's region rule
  exists: every "how many tokens / AST nodes / locals" question has a **known upper bound before
  the pass starts** — a lexer emits at most one token per input byte, a parser at most one AST node
  per token, a symbol table at most one entry per identifier occurrence. Pre-size the array to the
  bound (`[EMPTY; src.len()]`), track a separate `count: i64`, index the used prefix — the exact
  discipline M1's kernels and M4's arena examples already use, not a new pattern.
- **A tree.** No nested structs, no arrays inside structs (m4-design.md §1, still true). An AST
  node is therefore a struct of **scalar fields only** — a `kind: i64` discriminant plus a handful
  of `i64` payload fields that mean different things per kind — stored in one flat array, children
  referenced by **index**, exactly M4's arena (`struct Node { val: i64, next: i64 }`) scaled up to
  more fields and more discriminant values. Dispatch on `kind` is a chain of `if`/`else` — there is
  no `match`, and the cost model already costs a chain of `if`s correctly (each `if` is `max` of
  its branches, cost-model.md), so this is not a gap either, only a verbosity.
- **Recursive descent.** Ordinary recursion, done since M3.
- **The scan loop.** `while` with an inferred or declared `decreasing` measure, done since M3.
- **File I/O.** The one real gap, and it is a build-system gap, not a language one — §2.

**Conclusion: nothing here needs new syntax or a new type.** Every stage is expressible with
structs, arrays, arenas, recursion and `while`, as already built. What is missing is getting bytes
in and out of the process at all.

## 2. The one real gap: `extern fn` cannot do file I/O yet

**Done (commit 604f0ab).** `bootstrap/rt.c`'s `read_file`, `cc()`'s fixed linking convention, and
`extern fn read_file(path: &[u8], buf: &mut [u8]) -> i64 uses io, unbounded;` all work as designed
below — verified by hand (correct content and byte count; `-1` on a missing path; `-1`, not a
silent truncation, on a buffer too small). Not yet in the golden suite, which assumes
CWD-independent absolute paths and has no fixture file to point at yet.

Two independent problems, checked in `types.rs` and `emit_c.rs`:

- **An `extern` cannot return an owned array.** `check_func`'s owned-array-size inference (`if let
  Ty::Array(_, _) = &ret { … }`, reading the size off the body's tail expression) runs only for a
  function *with* a body; an `extern`'s early return skips it entirely, so `extern fn read_file(...)
  -> [u8]` would type but its returned array's size stays the placeholder `Size::Const(-1)` — not
  usable. **Not fixed by adding a declared-size mechanism for this one case**, because the second
  problem makes it moot:
- **`neant build`/`run` link only the one generated `.c` file** (`cc()` in `main.rs`: writes the
  emitted C, invokes `cc` on it and nothing else). Any `extern fn` used so far names a libc symbol
  (`labs`) that the platform's C library already provides. A file-reading shim does not exist in
  libc under a name this language could declare and have the *calling convention* match — an
  `extern fn` parameter of type `&[u8]` is emitted as a `(pointer, length)` pair (`emit_c.rs`,
  every view parameter), which is this language's own convention, not `open`'s or `read`'s
  (`const char *`, NUL-terminated, no length argument) — declaring `open` or `read` directly as an
  `extern fn` would pass the wrong argument list in the generated C.

**The fix is the small one, not the general one.** Write the few bytes of actual I/O once, in a
tiny hand-written C file (`bootstrap/rt.c`: `nt_read_file(uint8_t *path_p, int64_t path_n, uint8_t
*buf_p, int64_t buf_n) -> int64_t`, matching this language's own pointer+length convention exactly,
internally NUL-terminating the path onto a small stack buffer and calling `fopen`/`fread`), declare
it as `extern fn read_file(path: &[u8], buf: &mut [u8]) -> i64;` (an `i64` return — the byte count
actually read — sidesteps the owned-array-return gap completely: the caller pre-allocates `buf` at
a generous fixed size, exactly the pre-sized-array discipline of §1, and uses `read_file`'s return
value as the real length, the same `(array, separate length)` pattern every kernel in this repo
already uses), and teach `cc()` a way to link `rt.c` in — the smallest version being a fixed
convention (a sibling `rt.c` next to `bootstrap/`'s own sources, or beside the `.nt` file being
built, is always linked if present) rather than a new flag, so the CLI surface grows by nothing.
`neant measure --fn read_file` costs it like any other `extern`, Stage C's whole audit chain
(declared, measured, provenance) applies unchanged.

**This is the entire scope of what "not scheduled" needs revisiting for.** No new type, no new
syntax, no general string/IO feature — one ~30-line C file and one small, fixed linking convention.

## 3. Order: the lexer first, and why

**Done (`compiler/lex.nt`).** Built close to the shape below, two differences found while writing
it, both recorded in `lex.nt`'s own header comment rather than silently: it takes `n: i64`
separately from `src`, since `src` is the caller's fixed-capacity buffer (`read_file`'s `buf`) and
there is no sub-array view to hand it a shorter slice of the same array — the first real instance
of the "pre-sized array with a tracked count" pattern §1 predicted, one level deeper than
predicted (the count has to travel as its own parameter, not just live inside the array). And
`Float`'s value is not attempted at all yet — kept as a `(start, len)` span like `Ident`/`Str`,
not `ival` — so the `f64` bit-cast §3 flagged is deferred again, past this stage, not added here
after all.

The bootstrap compiler's own stage order (lex → parse → types → ir → emit_c) is also the right
self-hosting order, but **not attempted as one leap**: the lexer is the only stage with no
dependency on any other neant-in-neant code, the smallest (188 lines of Rust), and it exercises
exactly the two things §1 identified as new *usage*, if not new *features* — the arena-of-scalar-
structs pattern at real size, and pre-sized arrays with a tracked count — without also needing the
parser's recursive descent or the checker's size calculus. It is the first milestone, not a
detour: `bootstrap/rt.c` + `read_file` (§2) has to exist before anything self-hosted can read a
`.nt` file at all, so it is shared, unavoidable, first work regardless of which stage comes next.

**Shape of the lexer in neant**, concretely:

```rust
struct Token { kind: i64, start: i64, len: i64, ival: i64 }   // ival: an Int/Byte literal's value

fn lex(src: &[u8], toks: &mut [Token]) -> i64 { … }            // returns the count filled
```

`toks` is pre-sized by the caller to `src.len()` (an upper bound: at least one byte consumes at
least one token or is skipped whitespace/comment, so token count never exceeds input length).
`kind` is a small integer per `Tok` variant (`Ident = 0`, `Int = 1`, … in `lex.rs`'s own order, so
a later parser stage and this one agree without a shared enum the language cannot express). A
`Tok::Ident`/`Str` payload is `(start, len)` into `src`, not a copy; `Tok::Int`/`Byte`/`Float`'s
value goes in `ival` (a float's bits, reinterpreted — `f64` has no `as i64` bit-cast in this
language yet, checked and confirmed missing; needed here, a small, contained addition: `as` already
converts *numerically* between `i64`/`f64`/`u8`, and a *bit* reinterpretation is a different,
new operation, `f64.bits() -> i64` and back, worth adding when this stage is actually built, not
before).

## 4. Exit test

**Passed, on every file, not a handful.** `bootstrap/tests/self_host_lex.rs`: for all 69 files in
`tests/golden`, `compiler/lex.nt` run through the neant compiler itself produces the identical
sequence of token kinds `bootstrap/src/lex.rs` does (compared via `neant lexdump`, a small debug
command added for exactly this — main.rs's `lex_kind_number` mirrors `compiler/lex.nt`'s numbering
by hand). Spans and literal values are not compared, only the kind at each position — the set of
decisions a lexer makes, which is what this stage exists to get right. `neant cost` on `lex` itself
— work and moves as a function of `n`, the arena's own bound, the first data point for "what does
the compiler cost, by its own tool" — is not yet taken.

## 5. What is deliberately not decided here

- **The parser, checker, IR and emitter** are not designed in this document — each is materially
  bigger than the lexer and should get its own pass at this design once the lexer's arena-of-
  structs pattern has been built once and is known to work in practice, not just on paper. The
  parser has since had that pass: [self-hosting-parser-design.md](self-hosting-parser-design.md).
- **The two-seed fixpoint in CI** (plan.md: the neant-in-neant compiler, compiled by
  `bootstrap/neant.c`, reproduces `bootstrap/neant.c` byte-for-byte or checked-equivalently) is the
  eventual exit condition for the whole effort, not this step's.
- **`f64` bit-cast** is named in §3 as needed and deferred to when the lexer's float-literal path is
  actually written, not designed here in the abstract.
- **Whether `rt.c`'s fixed-linking convention scales past one file** (a parser stage might want its
  own small C helpers too) is left for when a second one is needed.

**One parsing surprise, found writing `lex.nt`, not designed around, just worked past.** An `if`
with no `else`, used as a statement, followed on the next line by an expression starting with
unary `-` (`if len == 10 { … }` then `-1`) parses as one expression, `(if …) - 1` — a subtraction,
not two statements. `keyword()`'s length-gated `if` blocks needed an explicit `;` after each to
force the split. Whether this is the intended reading of the grammar or a rough edge worth its own
note in the language docs is not decided here — recorded so the next stage does not rediscover it
by the same error main.rs's own diagnostics gave (`"-" between "()" and "i64"`).

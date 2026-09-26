# Modules, first form

**Written 2026-09-26, before the code; § 2 amended the same day where the code taught otherwise,
and § 7 is what was done.** `compiler/build.sh` says "there is no module system, so
`compiler/*.nt` are fragments of one program", and plan § Stage D lists modules among the three
things a domain corpus cannot be written without. This is the smallest form that is honest about
what it is: a program can span files, and nothing else changes. Seed one only; the self-hosted side
is § 6.

## 1 — What it is

```neant
use "geometry.nt";
use "lib/io.nt";

fn main() { … }
```

- **`use "path";` is a top-level item.** The path is a string, resolved relative to the directory
  of the file that names it (an absolute path is taken as it is). It may appear anywhere a
  function or struct may, and names a file, not a thing in the program.
- **Each file is loaded once.** Identity is the canonical path, so a diamond — `main` uses `a` and
  `b`, both use `c` — loads `c` once, and `c.nt` and `./c.nt` are the same file.
- **A cycle is rejected**, at the `use` that closes it, with the chain printed:
  ``b.nt:3:1: `use "a.nt"` closes a cycle: a.nt → b.nt → a.nt``.
- **One flat namespace**, as the concatenation has today. Every function and struct of every loaded
  file is visible everywhere; there is no `pub`, no qualification, no renaming. A name defined twice
  in a multi-file program is rejected naming both places:
  ``main.nt:3:1: function `area` is defined twice: at shapes.nt:2:1 and main.nt:3:1``.
- **Order** is dependencies first: a file's items come after those of every file it uses, in the
  order of its `use`s, and the root file's last — the order `build.sh` writes by hand. Nothing in
  the checker depends on it (signatures are gathered before bodies), but emission and the report
  list functions in it, so it is fixed.

What is deliberately **not** in it: visibility, namespaces, separate compilation, a search path.
Each one is a decision this corpus has not yet asked for; the flat namespace is what the
compiler's own seven files already live in.

## 2 — How it is built: one line space

The type checker, the cost pass and the emitter all carry a position as a bare `u32` line, and a
good many of their messages have `line N` written into the text (`(line 57)` in the report, "read
again at line 12", every `#[cost]` violation). Giving them a file each would touch all three,
which this change must not. So the loader does what `cat` does and no more:

- each file is lexed on its own, so a lexical error is already in the right file;
- its tokens' lines are shifted by the number of lines of the files loaded before it, so the
  program has one line space, numbered through the concatenation in load order;
- the `use` items are taken out of the token stream at brace depth 0 (they name no node of the
  program, so the AST is unchanged), and each file's tokens are parsed on their own and the items
  appended — a file cannot end in the middle of an item and continue in the next, which `cat`
  would allow. The parser tells `S { … }` from a block by the struct names it has seen, so each
  file is parsed knowing every file's struct names (`parse::parse_knowing`, the one change to the
  parser);
- at every boundary where a line leaves the compiler, the driver maps a global line back to
  `path:line` — a structured error's position exactly, and in text every `line N` becomes
  `path:N` (`(line 57)` reads `(geometry.nt:12)`). The emitted C's bounds check, which prints
  `(line %d)`, gets a table from line ranges to paths and prints `(geometry.nt:12)`.

**A single-file program is not touched**: with no `use` in the root, the old path runs, line
numbers are the file's own, and no text is rewritten — every existing golden is byte-for-byte what
it was. Paths print as they were reached: the root as given on the command line, an imported file
as the naming file's directory joined with the string.

The rewriting of text is the one place this is a patch rather than a design: a function literally
named `line` followed by a number in a report would be relabelled too. The cure is a file index in
the IR's positions, which is § 6's work on the other side and would retire the rewrite here.

## 3 — What it means for the report and `costs.lock`

- **`neant cost`**: one report for the whole program, functions in load order; every line number in
  it names its file, `(lib.nt:12)`. A function's line belongs to the file that defines it.
- **`costs.lock`**: one lockfile per program, next to the root file, `generated from main.nt`. It
  already carries no `(line N)` (stage D (4): a number that moves when something above grows), so
  a function's entry is keyed by its name alone — unique, since the namespace is flat — and does not
  say which file it is in. Moving a function between files does not change the lock; changing what
  it costs does. Any other line a note names is `path:N`.
- **`#[cost]` violations** print `path:N: …` for the file of the function they are about.

## 4 — Every command

`run`, `build`, `check`, `cost`, `lock`, `emit` (and `emit --scop`, `measure`) take the root file and
load what it names. `lexdump` and `parsedump` stay single-file: they are cross-checks of the
self-hosted lexer and parser on one file's tokens.

## 5 — Tests

`tests/golden/modules/<case>/main.nt` and whatever it uses, with `main.out` / `.err` / `.exit` /
`.cost` as in `tests/golden`. They live in a subdirectory so that the per-file goldens and the
self-hosting tests, which read `tests/golden/*.nt` one file at a time, never see a fragment that is
not a program on its own. The runner runs each case from its own directory, so paths in messages
are the short ones a reader writes. Cases: a two-file program that runs, a diamond, a cycle, a
name defined in two files, a type error inside an imported file, a bounds failure at run time in an
imported file, and the cost report of a two-file program.

## 6 — What the self-hosted side would take

`compiler/*.nt` would not need a new pass. The self-hosted driver reads one source from stdin;
modules there need: `read_file` on a path (the bridge has it), path joining on bytes, a table of
loaded paths (identity by the string as joined, since there is no `realpath` in the bridge — a
weaker rule than seed one's, to be stated), the same depth-0 strip of `use "…";` over the token
array, and the same line shift. Its diagnostics print `line:col` today with no file; they would
take the same global-to-local map. Then `build.sh`'s `cat` becomes a `main.nt` that `use`s the six
others, and the fixpoint must be reached again with seed two built from it. The better form on
both sides is a file index carried in every position, so that no text is rewritten — a change to
the IR, the checker and the report together, and to both compilers at once.

## 7 — Done, 2026-09-26

`bootstrap/src/modules.rs` is the loader and `Sources` the map back; the driver calls it in place
of lexing and parsing one file, and prints every diagnostic, report, lockfile and violation
through it. Eight cases in `tests/golden/modules/`, run by `golden.rs`'s `modules` test: a
two-file program with a struct defined in the other file, a diamond reached as `lib/base.nt` and
`./lib/base.nt`, a cycle (`b.nt:3:1: … closes a cycle: a.nt → b.nt → a.nt`), `area` in two files,
an arity error at `util.nt:5:25` (line 10 of the concatenation), a bounds failure printed as
`(pick.nt:2)`, a `#[cost]` budget broken at `kernel.nt:3`, and a report and lockfile over two
files. No existing golden changed. What the struct-name rule showed: parsing a file alone is not
free in this grammar — its parse depends on names defined elsewhere — so a later `use` that is
more than concatenation (visibility, qualification) has to give the parser a program-wide view
first, as this one does.

## 8 — The standard library, 2026-09-26

`use "std/…";` names the standard library, wherever the program is: a path whose first component
is `std` is resolved in `$NEANT_STD` when that is set, and otherwise in the `std/` directory at the
root of the repository the compiler was built from (`bootstrap/`'s parent, fixed when `cargo build`
ran). Every other `use` is resolved as § 1 says, relative to the naming file, so a program with a
directory of its own called `std` has to reach it as `./std/…`. A standard-library file prints as
it is named — `std/text.nt:23` — not as the absolute path it was read from, so a report or a
diagnostic that mentions one is the same on every machine; its own `use`s resolve against the
library's directory, and one library file uses another as `std/…` too. Identity is still the
canonical path, so `std/math.nt` reached from two files is loaded once.

The first two modules: **`std/text.nt`** — `is_digit`, `is_space`, `skip_space`, `parse_int` and
`parse_float` at an offset (returning the value, the offset after it and whether there was one),
`format_int` and `format_float` into a `&mut [u8]` at an offset — and **`std/math.nt`** —
`abs_f64`, `abs_i64`, `clamp_f64`, `clamp_i64`, `sign_f64` in neant, and `sqrt`, `floor`, `ceil`,
`exp`, `log`, `sin`, `cos`, `pow` as libm externs with declared costs. Every function is exact or
declared: the parsers and formatters walk a fixed number of places (18 digits, which an `i64`
holds; 19 when formatting) and stop storing at the first that does not belong, so each is a
constant — `parse_int` work 333, `format_int` 406 — and `skip_space`, which scans by what it reads,
is charged the rest of the text, as its comment says. The libm declarations are bounds read off
the implementations' common paths, not measured. `tests/golden/modules/std/` uses both modules
from its own directory; its `main` was unknown, because printing a formatted buffer looped to
the offset the formatter returned, which is data; since `print_bytes` (decisions §13) it prints the
buffer in one call charged by the view, and `main` is exact. Left: `tests/corpus/lib/` still carries its own copies, to be replaced by `use
"std/…"` once the corpus is re-pinned; a search path or a version of the library; the
self-hosted loader.

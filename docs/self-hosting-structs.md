# Self-hosting: structs, and the front end reading its own source

The four stages were measured against their own source and **all four stopped at their first
`struct`** — which every one of them opens with, because the arena pattern (M4) is a `struct` of
scalars over a pre-sized array. That made structs the single unlock worth taking next, ahead of
arrays, `.len()` or anything else on the list.

## 1. What each stage needed

**Parser.** A `struct` item (kinds 112 `StructDef` / 113 `FieldDef`), a struct literal (70) with
its field initialisers (114), and the one hard part: `S { … }` is a struct literal only when `S`
names a struct, and `if c { … }` must not read `c { … }` as one. The Rust parser knows the struct
names because it collects items first; `parse.nt` parses in one pass, so it **pre-scans the token
stream for `struct <Ident>` and lays a marker node (kind 115) per name** before parsing anything,
and a `no_struct_lit` depth (`st[4]`) suppresses the literal inside an `if`/`while`/`for` head.
This is also why every parser function now takes `src`: a marker is compared by the bytes its name
token spans, and the parser never had a reason to look at the source text before.

**Checker.** Type kind 7, whose `elem` indexes a flat `strs` table of `Str { name, fields,
n_fields }` over a flat `flds` table of `Fld { name, ty }` — the arena pattern again, two levels.
Structs are collected before signatures, so a parameter or a return type may name one. **Fields are
scalars**, as they are in the Rust checker, which is what makes one collection pass enough: no
field's type can name a struct that has not been read yet.

**Emitter.** `struct nt_<name> { … };` per definition, `(e).f` for a field, and a compound literal
with designated initialisers, `((struct nt_P){.y = 2, .x = 1})` — which is why the emitter does not
have to reorder anything to match the declaration: C gives the designated form the same meaning
whichever order it is written in.

Both the checker's and the emitter's item loops had to learn that the item list **interleaves**
functions and structs in source order. Before this they indexed signatures by position in that
list, which was the same thing only while every item was a function.

## 2. What the build found

- **`P { x: 1, x: 2 }` was accepted.** The first check was "every initialiser names a field of this
  struct, and the count matches", and the comment in the code claimed those two together forbade
  both a repeat and an omission. They do not: that literal satisfies both and never gives `y`. A
  repeat is now caught directly, by scanning the initialisers already read. Found by a probe, not
  by the corpus.
- **A golden was nearly overwritten.** `tests/golden/structs.nt` already existed — the
  array-of-structs/SoA case, out of the slice — and a new file with the same name replaced it. The
  golden test caught it on the next run (`cost report differs`); the by-value program is
  `structval.nt`, and its name says why it is separate.
- **A skip is silent.** `self_host_emit.rs` compares only what the chain can read, so a program
  falling out of the slice shows up as one fewer comparison and nothing else. The test now names
  `structval.nt` and fails if it is not among the compared, rather than trusting a count.

## 3. `b'\''`, and why the lexer's corpus is now the compiler's own source

With structs in, the self-hosted parser was pointed at `compiler/lex.nt` and stopped at line 82:

```
else if e == b'\'' { 39 }
```

The byte- and string-literal scanner read forward to the next quote **without skipping an escaped
one**, so `b'\''` closed at the second quote and left a stray `'` that lexed as "unknown byte".
`bootstrap/src/lex.rs` has always handled it. The self-hosted lexer had not, since the day it was
written, and the exit test — 69 golden programs, compared token kind by token kind — never saw it,
because no golden program contains an escaped quote.

The compiler's own source does. `self_host_lex.rs` now lexes `tests/golden/*.nt` **and
`compiler/*.nt`**, 74 files; reverting the fix fails 2 of them. The general point is the one
self-hosting is for: the corpus that matters most for a compiler stage is the compiler.

## 4. Where the frontier is now

`the_self_hosted_front_end_checks_its_own_source` (in `self_host_check.rs`) concatenates
`lex.nt + parse.nt + check.nt` — about 1500 lines — and runs the self-hosted lexer, parser and
checker over it. **It parses and type-checks.** The front end accepts its own source.

`emit.nt` is not in that concatenation and the test says so explicitly. It stops at its first
`s.len()`: a method call, which the parser rejects by design. Arrays, slices, `.len()`, array
literals and `[e; n]` are what stand between here and the whole compiler reading itself, and the
frontier is now a test that has to be moved rather than a paragraph that has to be rewritten.

## 5. The checker now says *where*

`cst[2]` was a flag: the program is wrong. On a 1500-line file that is unusable. The checker
records two more slots — `cst[9]`, the innermost expression or statement that failed (the deepest
frame to see the flag go from clear to set, recorded by the wrappers that already existed for the
type table), and `cst[10]`, the item being read, which is where a failure in a signature or a
field's type lands. Every measurement in §3 and §4 came from those two.

## 6. The `if`/unary-minus quirk, a third time

An else-less `if` followed by a line starting with `-` parses as one subtraction. It has now been
hit in `lex.nt`, in `check.nt`, and in a throwaway driver written to debug this work. Recorded in
self-hosting-design.md §5 and the checker design §7; the third occurrence is the argument for
fixing the grammar rather than the `;`.

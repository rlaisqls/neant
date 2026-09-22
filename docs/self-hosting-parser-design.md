# Self-hosting: the parser

self-hosting-design.md deferred this on purpose ("materially bigger than the lexer... its own
pass at this design once the lexer's arena pattern is known to work in practice"). The lexer
(`compiler/lex.nt`) is done and passed its exit test on all 69 `tests/golden` files; this is that
pass, for `bootstrap/src/parse.rs` (530 lines) and `bootstrap/src/ast.rs` (135 lines).

## 1. Why this is bigger than "the lexer, but for a tree"

The lexer had one arena of one struct shape (`Token`). The parser's tree is not one shape:
`ExprKind` has 15 variants and `Stmt` 7, and their payloads range from "one child" (`Unary`) to
"a name, a variable-length list of (name, expr) pairs" (`StructLit`). M4's arena pattern (a flat
array of scalar-field structs, children by index) still applies — nothing here needs a feature
the language lacks — but it needs two things the lexer didn't:

- **A generous-enough uniform node shape** that every kind's payload fits into, since there is
  still no sum type to make each kind's struct different.
- **A second arena for variable-arity lists** (call arguments, array elements, a struct literal's
  fields, a block's statements, a function's parameters) — one `Node` cannot hold "zero or more
  children" in scalar fields alone.

Both are described precisely below, because getting them wrong is expensive to unwind once
`.nt` nodes start pointing at each other.

## 2. The node schema

One arena, one struct, used for expressions, statements, and type expressions alike (they are
structurally the same problem — a kind tag plus a handful of child references):

```rust
struct Node { kind: i64, a: i64, b: i64, c: i64, d: i64, ival: i64, list: i64, list_len: i64 }
```

- `kind` — which `ExprKind`/`Stmt`/`TypeExpr` variant, numbered below.
- `a`, `b`, `c`, `d` — up to four child references, each either another node's index in this same
  arena, or a token's index in `compiler/lex.nt`'s `toks` (which field means which is fixed per
  `kind`, documented in the table, the same "verbosity, not a gap" if/else dispatch the lexer
  already established). `-1` means absent (an optional child).
- `ival` — a small discriminant this node needs besides its children: `BinOp`/`UnOp`, whether a
  `&`/`let` is mutable, an `Assign`'s compound operator. Literal *values* are not duplicated here:
  an `Int`/`Float`/`Byte`/`Bool` node's `a` is the **token index**, and the value lives in that
  token (`ival` for `Int`/`Byte`, a `(start, len)` span for `Float` — lex.nt's own deferral, still
  deferred), read from there when a later stage needs it.
- `list`, `list_len` — a slice into a second, shared arena (§3) for a variable-length child list;
  `list_len = 0` when a kind has none.

Kind numbers, continuing past the lexer's `0`–`57` (the two arenas are never confused — a `Node`
is never mistaken for a `Token` — but disjoint ranges make a stray mix-up loud instead of silent):

```
expressions   60 Int  61 Float  62 Bool  63 Byte  64 Bytes  65 Var  66 Binary  67 Unary
              68 Index  69 Field  70 StructLit  71 Call  72 MethodCall  73 Ref  74 Cast
              75 If  76 Block  77 ArrayLit  78 ArrayRepeat  79 Lambda  80 Comprehension
statements    90 Let  91 Assign  92 For  93 While  94 Break  95 ExprStmt  96 Return
types        100 TyNamed  101 TyUnit  102 TyArray  103 TySlice  104 TyOwned
```

Per-`kind` field meaning (children are node indices unless marked *tok*, a token index):

| kind | a | b | c | d | ival | list |
|---|---|---|---|---|---|---|
| Int/Byte/Bool | *tok* (value: token's `ival`) | | | | | |
| Float | *tok* (span: token's `start,len`) | | | | | |
| Bytes | *tok* (span) | | | | | |
| Var | *tok* (name span) | | | | | |
| Binary | left | right | | | `BinOp` (§4) | |
| Unary | operand | | | | `UnOp` (§4) | |
| Index | base | index | | | | |
| Field | base | name *tok* | | | | |
| StructLit | name *tok* | | | | | (name-tok, value-node) pairs |
| Call | name *tok* | | | | | arg nodes |
| MethodCall | receiver | name *tok* | | | | arg nodes |
| Ref | operand | | | | mutable (0/1) | |
| Cast | operand | type node | | | | |
| If | cond | then-Block | else-Block (−1) | | | |
| Block | tail (−1 if none) | | | | | stmt nodes |
| ArrayLit | | | | | | elem nodes |
| ArrayRepeat | elem | count | | | | |
| Lambda | body | | | | | param name toks |
| Comprehension | elem | var *tok* | source | cond (−1) | | |
| Let | name *tok* | type node (−1) | init | | mutable (0/1) | |
| Assign | target | value | | | `BinOp` or −1 (plain `=`) | |
| For | var *tok* | start | end | body-Block | | |
| While | cond | decreasing (−1) | body-Block | | | |
| Break | | | | | | |
| ExprStmt | expr | | | | | |
| Return | expr (−1) | | | | | |
| TyNamed | name *tok* | | | | | |
| TyArray | elem type | count node | | | | |
| TySlice | elem type | | | | mutable (0/1) | |
| TyOwned | elem type | | | | | |

## 3. The child-list arena, and the two upper bounds

**This section is wrong, and building it is what showed that (`compiler/parse.nt`).** A flat
`children` arena filled left to right cannot hold a block's statement list: a nested block parses
*its* statements while the outer block is still collecting its own, so the outer list's entries are
not contiguous. Children are chained through the nodes instead — `Node` carries a `next` (−1 ends
the list) and a parent points at its first child — which is M4's arena-of-index-links again, one
arena rather than two, with nothing to interleave. `list`/`list_len` in §2's schema are therefore
`next`, and the `children` parameter below does not exist. Everything else in §2 and §3 stands,
including the bound: at most one node per token, so `nodes` sized to the token count is enough.

```rust
fn parse(toks: &[Token], n_toks: i64, nodes: &mut [Node], children: &mut [i64]) -> i64 { … }
```

`children: &mut [i64]` is the second pre-sized array §1 named — a flat list every variable-arity
node's `list`/`list_len` slices into, filled left to right as each such node finishes parsing its
children, never revisited (so a later node's slice never aliases an earlier one's).

Both arrays need an upper bound fixed *before* parsing starts, the same discipline the lexer's
`toks` used, one token further removed:

- **`nodes`**: at most one node is born per token consumed by `primary`/`postfix`/a statement
  keyword — `n_toks` is a safe, if generous, bound (a real program's ratio is closer to one node
  per 2–3 tokens, since operators and punctuation consume a token without their own node).
- **`children`**: every entry is one node's *reference* to something already in `nodes`, so the
  same bound applies again — `n_toks` is enough headroom for both arrays.

## 4. `BinOp`/`UnOp` numbers, and what needs porting besides parsing

```
BinOp:  0 Add 1 Sub 2 Mul 3 Div 4 Rem 5 Eq 6 Ne 7 Lt 8 Le 9 Gt 10 Ge 11 And 12 Or
UnOp:   0 Neg 1 Not
```

`keyword()` already exists in `lex.nt` and is reused as-is; the parser does not re-lex.

## 5. No function pointers: precedence climbing, one level at a time, inline

`parse.rs`'s `binary_level` takes `next: fn(&mut Self) -> Result<Expr>` — a function passed as a
value. Whether this language has function-valued parameters at all has not come up before now:
`.map`/`.filter` closures are a chain's own syntax, checked and lowered specially by `apply_closure`,
never a value a `let` can hold or a parameter can accept — there is no general function type.
**Not adding one for this.** Each precedence level (`or`, `and`, `cmp`, `add`, `mul`) gets its own
function with its own inlined loop instead of calling a shared `binary_level` through a pointer —
six short, near-identical functions instead of one parameterised one, the same "verbosity, not a
gap" trade the lexer's keyword dispatch already made.

## 6. What is in the first slice, and what is named and deferred

Full parity with `parse.rs` in one pass was not how the lexer was built either. In:

- Every expression precedence level (`or` → `and` → `cmp` → `add` → `mul` → `cast` → `unary` →
  `postfix` → `primary`) for: `Int`, `Float`, `Byte`, `Bool`, `Var`, `Binary`, `Unary`, `Index`,
  `Field`, `Call`, `Ref`, `Cast`, `If`, `Block`, parenthesised grouping.
- `Let`, `Assign` (plain and compound), `For`, `While`, `Break`, `Return`, expression statements,
  and a block's tail-expression rule.
- A function's signature (name, typed parameters, return type) and body, and the type expressions
  `§2`'s `Ty*` kinds cover (`Named`, `Array`, `Slice`, `Owned`, `Unit`).

Out, named rather than silently missing, each because it adds a real new shape this pass chose not
to design around yet, not because it is hard to type out:

- **`StructLit`, `MethodCall`, chains (`.iter().map(...)…`), `Lambda`, comprehensions,
  `ArrayLit`/`ArrayRepeat`.** Every one of these needs the child-list arena for a *nested*
  variable-arity shape (a chain is a list of stages, each with its own args) that `§3`'s flat,
  once-through-left-to-right allocation does not obviously extend to without becoming its own
  design question — deferred with the question named, not answered by assumption.
- **`extern fn`, `#[cost(...)]`/`#[layout(...)]` attributes, `struct` definitions themselves.**
  Needed for a real program eventually, not needed to prove the node arena works on one.
- **Error recovery.** `parse.rs` stops at the first error with a line/column; the self-hosted
  parser does the same (return on the first failure) rather than attempting multiple diagnostics.

## 7. Exit test

**Passed.** Not "all of `tests/golden`" the way the lexer's was — most golden files use at least
one out-of-scope construct (§6) — but not a hand-picked set either: `neant parsedump` (mirroring
`lexdump`; prints each node's `kind` in this document's numbering, depth-first, and exits 2 naming
the construct when a file is outside the slice) partitions the corpus itself, and
`bootstrap/tests/self_host_parse.rs` compares whatever is inside it. Of 69 files: **13 parse
inside the slice and all 13 produce an identical node-kind sequence** (`arith`, `ifexpr`, `fib`,
`forsum`, `block`, `hello`, `baddec` and the small rejection cases — 94 and 108 nodes at the top
end), 56 are out of slice, and the negative goldens the bootstrap parser rejects are required to
be rejected by the self-hosted parser too rather than skipped.

**One real difference found by it, in `item()`.** `parse.rs` checks "no `;` and the next token is
`}`" — the block's tail — *before* the "an `if` or block statement needs no `;`" case. Written the
other way round, a trailing `if` at the end of a function body becomes a statement instead of the
block's value, which is a different tree and (once a checker exists) a different type. The order
is not arbitrary and is now commented at both sites.

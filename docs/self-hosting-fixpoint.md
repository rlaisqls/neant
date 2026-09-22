# The fixpoint

`compiler/` compiles `compiler/`, and the result is itself.

```
stage1   the four stages + a driver, as neant source, run by the Rust compiler's interpreter
stage1 ─reads─▶ stage1's source ─writes─▶ stage2.c   (182,930 bytes)
cc stage2.c rt.c ─▶ stage2                            (a native binary)
stage2 ─reads─▶ stage1's source ─writes─▶ C
                                          └── byte-identical to stage2.c
```

Pinned as `the_self_hosted_compiler_reaches_its_fixpoint` in `bootstrap/tests/self_host_emit.rs`.

## 1. Why this is the test that matters

Every other self-hosting test compares the neant compiler against the Rust one: the same token
kinds, the same node kinds, the same accept/reject verdicts, the same program output. Those say
the neant compiler *agrees* with a compiler. They cannot say it **is** one, because a program that
agrees with an oracle on a corpus is still a program that agrees with an oracle on a corpus.

The fixpoint removes the oracle. The Rust compiler builds stage2 and then steps out of the
comparison entirely; what is compared is one native binary's output against the C it was itself
built from. It fails if the compiler is compiled differently from the way it compiles — the
classic bootstrap discrepancy — and it fails if any construct survives one pass and not the next.

It is also, unexpectedly, a performance result: stage2 compiles the whole compiler in **42 ms**,
where stage1 — the same source, walked by the Rust interpreter — takes about ninety seconds. Three
orders of magnitude, and nothing was optimised; that is only the difference between interpreting a
tree and running compiled code.

## 2. What is in the fixpoint, and what is not

`compiler/*.nt` is about 2000 lines of neant and covers: the lexer, a one-pass recursive-descent
parser over an arena of uniform scalar-field nodes, name resolution and type checking with struct
types and size atoms, and a C emitter. It reads a `.nt` file and writes a `.c` file that `cc`
compiles and runs.

It is **not** the whole language. The self-hosted slice leaves out, each because it feeds a stage
that is not self-hosted yet or because nothing in the compiler's own source uses it:

- **The cost calculus.** `work`, `moves`, `span`, regimes, `costs.lock`, the IOLB bound — none of
  it. This is the largest omission by far, and it is the language's entire point; the self-hosted
  compiler compiles neant programs but says nothing about what they cost.
- **Moves, uniqueness and layout (M5, M4).** Five negative goldens are listed by name in the
  checker's parity test as "not in this slice", and the emitter takes the safe branch everywhere
  the Rust one takes a proved one: `ys = xs` copies rather than aliasing.
- **Chains, closures, comprehensions, `#[cost]`, `decreasing`,** and everything else the parser
  design §6 deferred.

So this is the fixpoint of a **front end plus a C backend**, not of the compiler the plan
describes. Saying otherwise would be the interesting-sounding half of a true statement.

## 3. What the two seeds are now

plan.md has said from the start: *two seeds forever.* Seed one is `bootstrap/`, the Rust compiler,
frozen after self-hosting and never deleted. Seed two was to be `neant.c` — `compiler/` compiled by
itself, checked in, so that a C compiler and nothing else can rebuild the chain.

Seed two now exists: it is what `stage2.c` is. It is not yet checked in, because the file that
produces it is assembled by a test from four stages and a driver, and a checked-in seed should
come from a committed source file rather than a concatenation a test performs. That is the next
piece of work, and it is bookkeeping rather than compiler work.

## 4. The order things were found in

Worth recording, because none of it was the plan:

1. Measuring where each stage stopped on its own source said **`struct`**, in all four — the arena
   pattern means every stage opens with one. (self-hosting-structs.md)
2. With structs in, pointing the parser at `compiler/lex.nt` found a lexer bug that had been there
   since the day it was written: an **escaped quote**, `b'\''`, which appears in the lexer's own
   source and in no golden program. The lexer's corpus is now `compiler/*.nt` as well.
3. Measuring again said `.len()` **once** and `b"…"` bound by `let` **79 times** — the whole array
   representation, pulled in by two constructs. (self-hosting-arrays-design.md)
4. Arrays forced the checker to grow **size atoms**, which its own design had said were only the
   cost model's business. Whole-array assignment cannot be type-checked without them.
5. The last gap was **`extern fn`**: the stages are not a program without a driver, and every
   driver opens with one.

Each step was chosen by a measurement rather than by a plan, and each measurement was a `grep -c`
or a one-line probe. That is the part worth keeping.

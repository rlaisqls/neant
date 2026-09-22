# M5, part 2 — uniqueness and in-place reuse: the design

Part 1 of M5 ([plan.md](plan.md)) is done: `let ys = xs;` moves an array, a further use of `xs`
is rejected with the line, and a move inside a loop of something born outside it is rejected
outright. That is Rust's rule — moving is always free and reuse is always an error. This part is
the other one README already promises and part 1 does not build: **`ys = xs; ys[3] = 9` is written
one way, and the compiler decides whether it can reuse `xs`'s buffer or must copy it, and says
which.** The two constructs are syntactically distinct (`let ys = xs` binds a name for the first
time; `ys = xs` reassigns one that already exists) so they can carry different rules without
colliding.

## 1. What is in, what is out

In: `ys = xs;` as a statement, `ys` an existing `let mut` array local, `xs` a plain variable of
the same array type and the same size; a static uniqueness test — no live use of `xs`, and no
live view rooted at `xs`, remains after this statement — decided at compile time, never piecewise,
never at runtime; two emissions, in place (pointer takeover, as a move already does) and copy (a
sequential write of `xs`'s bytes into `ys`'s existing buffer before `ys`'s name is retargeted);
the report naming which, and the line of the conflicting later use when it copies. Also in, as a
fix this shares a mechanism with: closing a gap in `let ys = xs` where a live view of `xs` survives
the move today (§4).

Out: cross-iteration reuse — `ys = xs;` where the pair is meant to ping-pong every lap of a loop
(a Jacobi-style double buffer) is the case this feature is really for, and it is **not** handled
well here; §9 says why and leaves it for a named swap construct, not this one. Out too: `xs` as
anything other than a plain variable (`ys = f(xs)`, `ys = xs_or_zs`) — the source must be a name,
same as a move. Out: structs and scalars, which are already `Copy` and need no uniqueness test at
all — this is only for arrays, the one type in the language that is expensive to duplicate.

## 2. The uniqueness test

At `ys = xs;`, `xs` is **unique** — the buffer can be handed to `ys` rather than copied — when
nothing that runs after this statement can still read `xs`'s old contents through any name. Two
things can do that: `xs` itself, used again; or a view — `let v = &xs;` or `let v = &mut xs;`,
directly or through a chain of local-to-local rebinding — whose root is `xs`, used again. Both are
**used again** in the same sense the move checker already gives that phrase: a name that appears,
as a read, anywhere lexically after this statement in the rest of the function, including inside
a loop this statement is itself inside (conservatively — see below), counts.

This is decided by a single backward scan of the function's checked statements, run once before
the assignments that need it are reached, not a dataflow fixpoint: for every local, its **last
line** is the highest line number at which it, or a local whose root is it, is read. `ys = xs;` is
in place iff `xs`'s last line is not after this statement's line, for every local rooted at `xs`
(itself included). No piece, no regime: this is a syntactic fact about the program text, not a
value the program computes, so it does not belong in the piecewise machinery the rest of the model
uses for data-dependent conditions ([cost-model.md](cost-model.md) — the fit test forks because a
size is not known at compile time; this forks nothing because whether a line exists is known).

**Loops, conservatively.** If `ys = xs;` is inside a loop and `xs` (or a view rooted at it) was
declared outside that loop, treat it as used again unconditionally — the same restriction the move
checker already applies to a plain move in a loop, for the same reason: one lexical occurrence
covers every lap, and a later lap's use of `xs` cannot be ruled out by looking at this lap's text
alone without a real per-iteration analysis, which is out of scope (§9). Concretely: the copy
branch is forced whenever the reassignment is inside a loop and either name predates it, whether or
not a later use is actually present. This makes the loop case always report `n`, honestly stating
that the feature does not help there yet, rather than silently getting it wrong.

## 3. Syntax and typing

`ys = xs;` where `ys` is a `let mut` local of type `Array(T, sz)`, `xs` an unmoved local of the
same `Array(T, sz)` — same element type, same size expression, checked the way `check_declared`
already checks a `let`'s declared type against its value. `xs` is **always** moved by this
statement, in place or not: after it, using `xs` is a type error with the line, exactly like a
`let`-move (§4 of the M5 plan entry). What differs is only what happens to the **bytes**, decided
by §2, and that decision is invisible to the type checker — both branches typecheck identically,
which is the point (README: "the same text whether it copies or updates in place").

The existing ban on assigning a whole array (`types.rs`, "cannot assign a whole array or view;
assign elements") narrows: it still holds for a `Ty::Slice` target (a view is never reassigned
wholesale — `&mut xs` from `&mut ys` is not this feature) and for anything on the right that is not
a plain variable of matching array type. `ys[3] = 9`, `ys[i].f = e` continue exactly as today;
nothing about element assignment changes.

## 4. A gap this closes in the move checker

`let ys = xs;`, shipped in part 1, does not check for a live view of `xs`. `let v = &xs; let ys =
xs; println(v[0]);` typechecks today and should not: `v` and `ys` now alias one buffer without the
checker's `root` map knowing it (`ys`'s root is itself, `v`'s root is `xs`), which is exactly the
condition the `restrict` emission and the call-argument disjointness check both assume cannot
happen. This was not caught in part 1 because nothing in m4-design.md §2 mentioned views, and no
golden exercised it. The same "last line of a rooted local" scan from §2 closes it: extend the
`let`-move check to require `xs`'s last line — over `xs` itself and every view rooted at it — to
not be after the move. Where §2 lets `ys = xs` fall back to a copy, a `let`-move has no fallback
(there is no existing buffer to copy into and no name to retarget without a body rewrite), so it
stays a hard error, now with the right condition: `err_move_view.nt`, a golden that must start
failing once this lands.

## 5. Emission

In place: identical to a move — `ys`'s pointer (or, under SoA, pointer tuple) and length become
`xs`'s; nothing is copied. Copy: a loop of `ys_n` (`= xs`'s length, equal to `ys`'s own by
typing) iterations, `ys_p[k] = xs_p[k]` (or per field under SoA), emitted before `ys`'s name is
retargeted — `ys`'s **existing** buffer is written into, not a fresh allocation, since `ys` already
owns one of the right size; `xs`'s buffer becomes garbage exactly as a move's source always does
(M4: arrays are never freed).

## 6. What changes in the cost model

A new site kind, not a variant of an access site: **an assignment site**. In place costs `work 1`
(the pointer/length copy) and `moves 0` — no byte crosses the boundary, the same as a move. Copy
costs `work ys.len()` and `moves ys.len()·sizeof(elem)` (or, under SoA, summed per field) — a
sequential write, the same line the model already gives `[e; n]` ([cost-model.md](cost-model.md)
§Moves, "other moves" table). Because §2 decides the branch from the program text alone, not from
a symbolic condition, the function's cost has **one** term here, not a piece — the report is
`work 1, moves 0` or `work n, moves 8n` (say), never `work 1 if … else n`. The report line for the
copy case names the conflicting use: "`ys = xs` at line L copies (`xs` used again at line L2)" —
the same information §2 already computed to decide the branch, carried into `neant cost`'s output
rather than discarded once the yes/no is made.

## 7. Exit tests

1. **In place, measured.** `ys = xs; ys[0] = 9;` with no further use of `xs`: predicted `moves 0`
   for the assignment (plus the one-element write); a loop repeating this pattern at sizes past L2
   moves nothing extra, confirmed by `perf stat`.
2. **Copy, measured.** The same, but `xs` used once more after: predicted `moves 8n`; measured
   refills include the full copy.
3. **The report names the line.** `neant cost` on the copy case's golden prints the conflicting
   use's line number, checked as a golden `.cost` file, not just that a copy happened.
4. **The view gap.** `err_move_view.nt` (§4) is rejected; every part-1 golden (`moves`,
   `err_move_use`, `err_move_loop`, `err_move_if`) is unchanged.
5. **The loop case is honest.** A golden with `ys = xs;` inside a `while`, both declared outside
   it, reports the copy unconditionally, and its `.cost` golden says so — this is the exit test
   for §2's stated limitation, not a claim the loop case is solved.

## 8. Order

The backward last-line scan (a new small pass, run per function before the body is walked a second
time for `ys = xs;` sites — or folded into `check_func` as a pre-pass over the already-parsed AST,
since it only needs names and line numbers, not types) → the view-gap fix to `let`-moves (§4, its
own golden first, red before the scan exists, green after) → `ys = xs` typing and the in-place
emission → the copy emission → the cost-model site and the report line → the loop conservatism and
its golden → measurement.

## 9. Open questions, answered provisionally

- *Why is the loop case out, when it is the motivating one?* A real double buffer swaps two names
  every lap (`a = b_new; b = a_old`, or a `swap(&mut a, &mut b)`), which needs either a dedicated
  swap primitive (typed as an atomic rename, no bytes moved, never a copy) or a per-iteration
  liveness analysis this project has explicitly called a team-scale problem to avoid extrapolating
  a pace onto (decisions §4, "region inference, cost-driven layout, uniqueness analysis and
  schedule search are each team-scale problems"). A `swap` primitive is cheap to add later and
  does not need this design's machinery at all, since it never copies by construction; it is not
  in this pass because nothing in the M4/M5 corpus asks for it yet.
- *Why a syntactic last-line scan and not real liveness (a backward dataflow pass over the CFG)?*
  Straight-line code and the two structured joins already in the checker (`if`/`else`, one level of
  loop) do not need a fixpoint: "used again" is a property of the text once loops are handled
  conservatively (§2), and a fixpoint earns its cost only once a program has control flow a single
  backward pass cannot see through — which, given loops are already the conservative case, is
  never, for this feature, in this language.
- *Does this ever need to be piecewise?* No (§2, §6) — this is the one place in the calculus where
  a yes/no question about the program is decided once, at compile time, from the text, rather than
  forked into regimes over a runtime value. Worth stating because everywhere else in the model,
  "cannot decide → fork" is the rule (decisions §2); here the answer is knowable, so forking would
  be manufacturing an uncertainty that is not there.
- *What about a struct array under SoA — is uniqueness per field or per value?* Per value: `ys`
  and `xs` are one array of `S`, and the move/copy is of the whole thing (every field's buffer),
  never one field at a time. Per-field reuse (copy field `a`, move field `b`) is a real
  refinement a later pass could make, since SoA already gives each field its own root internally
  (M4, cost-model.md §Structs) — not attempted here.

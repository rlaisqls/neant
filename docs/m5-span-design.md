# M5, part 3 — span: the design

The last piece of M5 ([plan.md](plan.md)): "`span` joins the cost; `T ≤ W/P + O(S)` becomes a
statement about a parallel loop." Parts 1 (real moves) and 2 (uniqueness, [m5-design.md](m5-design.md))
are done. This is the one genuinely new surface in M5 — nothing before this parses, types, costs
or emits parallelism at all — so it gets its own design, argued the same way M4's did: what the
claim is, what would falsify it, and what is deliberately cut to keep the first version testable.

## 0. What already does the hard part

A general `for`/`while` loop can mutate anything reachable from it; proving two iterations don't
race is a real static-analysis problem, and not one this pass takes on. A **chain**
(`xs.iter().map(...).filter(...).sum()`) already cannot: `types.rs`'s `apply_closure` sets
`in_closure`, and `Stmt::Assign` inside a closure is rejected there today — "a closure in a chain
cannot assign; it must be a pure function of its arguments." Every `map`/`filter` stage is
*already checked to be a pure function of its own element*, for a reason that had nothing to do
with parallelism (fusion). That check is exactly the soundness argument parallel execution needs:
if a stage cannot see or touch anything outside its own element, its iterations have no dependency
on each other to prove absent. Span is therefore scoped to chains only, not loops in general — the
one place the language has already done the proof, as a side effect of a different decision.

## 1. What is in, what is out

In: `.par()` as a chain stage, directly after the source (`xs.iter().par()...` or `xs.par()...`
with no explicit `.iter()`); a `.par()` chain's terminal restricted to `sum`, `count`, `max`,
`min`, `any`, `all` — every terminal whose combine is associative, and whose code already exists;
`span` as a third quantity alongside `work` and `moves`, inferred for every function, `span = work`
by sequential composition wherever nothing is parallel (a fully sequential program's critical path
is its own total work, the standard convention); a machine parameter `P` beside `M` and `B`; the
report line `T ≤ W/P + O(S)`; OpenMP emission (`#pragma omp parallel for reduction(...)`) for a
`.par()` chain, `-fopenmp` added only to a build that uses one.

Out: `.par()` on `fold` — an arbitrary closure is not known associative, and guessing wrong is
silent unsoundness the rest of this project does not accept elsewhere, so it is a type error
naming the reason; general loop parallelism; nested `.par()`; a scheduler cost (thread spawn,
work-stealing) — folded into the `O(S)` the plan's own formula already leaves unaccounted; and,
provisionally, any notion of memory bandwidth as a second resource `P` divides — §6 says why this
is the thing most likely to be wrong, and why it is a question for the exit test rather than a
decision made here.

## 2. Syntax and typing

```rust
fn total(xs: &[f64]) -> f64 { xs.iter().par().map(|x| x * x).sum() }   // ok: sum is associative
fn worst(xs: &[f64]) -> f64 { xs.iter().par().fold(0.0, |a, x| a.max(x)) }  // error
```

- `.par()` takes no arguments, like `.iter()`; it may appear once, immediately after the source
  (`xs.par()` and `xs.iter().par()` are the same chain, `.iter()` staying optional as it is today).
  `.zip()` still has to come first when present (existing rule) — `xs.zip(ys).par()` — since a
  parallel loop over two arrays is the same shape as a sequential one, just distributed.
- The terminal is checked against a fixed associative set. `.par()` before `.fold(...)` is
  rejected: `"a parallel chain's terminal must combine elements associatively (sum, count, max,
  min, any, all); fold's closure is not known to, since it can do anything"`. This is the same
  shape of refusal `dyn`'s absence and the M3 corpus's "unknown" tier already use — a real
  limitation stated, not silently narrowed.
- `map`/`filter` closures inside a `.par()` chain need no new check: `in_closure` already forbids
  assignment inside any chain closure, parallel or not.

## 3. What span means, and how it composes

Work and moves already have a full composition calculus (cost-model.md); span reuses it almost
unchanged; the one new rule is what a `.par()` loop does at the point work multiplies by trip count.

- **Sequential composition**: two statements in a row have span = the sum of their spans, same as
  work. A loop's *body* span is the same as its work per iteration (nothing inside is parallel
  unless it is itself a `.par()` chain). An `if`'s span is `max` of its branches' spans — already
  the rule work uses (cost-model.md: "the cost of the statement is the larger of the two, not
  their sum"), for the same reason: only one branch runs.
- **A `.par()` chain's own span** has two parts. The `map`/`filter` stages run "at once" under the
  work-span convention of unlimited processors: their contribution to span is the cost of *one*
  element's closures, a constant in `n`, not `n` times that constant the way work is. The terminal
  reduces `n` values with an associative combine, whose span is a balanced binary tree of depth
  `⌈log₂ n⌉`, each level one combine — `O(log n)`, not `O(n)`. A `.par()` chain's span is therefore
  `O(1) + O(log n)`, its work unchanged from the sequential chain's (same total operations, same
  moves, same footprint and residue — parallelism changes how fast, not what or how much).
- **A call's span** is the callee's span, substituted the same way work already is; a function
  with no `.par()` anywhere in it (directly or transitively) has `span == work` identically, which
  is the exit test for "nothing broke" on every existing golden.
- Implementation-wise this is one loop-leaving rule away from existing code: `Fa` gains a `span:
  Cost` accumulator next to `work`, every site that adds to `work` adds the identical amount to
  `span` by default (sequential composition needs no new rule), and `leave_loop` — where work
  becomes `Σ_v(body)`, trip-count many copies — instead makes span `O(log(trip))` copies when the
  loop came from a `.par()` chain, a flag on `Loop` set where that chain is lowered. `moves`,
  `footprint` and `residue` are untouched: `.par()` does not change what bytes a function touches,
  only how the touching is scheduled, so the whole exact-moves machinery this project spent M0–M4
  building needs no parallel-aware version at all.

## 4. The report and `P`

`P` joins `M` and `B` as a third machine parameter (`Atom::P`, `Machine.p_cores`), symbolic in the
general report, numeric under `--eval`/`-P`, defaulted to this machine's big-cluster core count
the way `M`/`B` default to this machine's cache. A function containing a `.par()` chain gets a
fourth report line:

```
total            work 2·n                          moves 8·n + B            exact
                 span  2 + 2·⌈log₂ n⌉               T ≤ work/P + O(span)
```

`#[cost(span_at_most = "...")]` joins `work_at_most`/`moves_at_most` in Stage B's declaration
syntax (cost-model.md §`#[cost(...)]`) for the same reason those exist: an interface can budget a
parallel function's critical path without seeing its body, and `neant measure` can be pointed at
it the same way it already confirms a declared `work`/`moves` pair, once P is a value that varies
in the measurement (§6).

## 5. Emission

```c
double acc = 0.0;
#pragma omp parallel for reduction(+:acc)
for (int64_t i = 0; i < xs_n; i++) {
    double x = xs_p[i];
    acc += x * x;
}
```

`sum`/`count` → `reduction(+:acc)`; `max`/`min` → `reduction(max:acc)`/`reduction(min:acc)`
(OpenMP 4.5, which every compiler this project targets has); `any`/`all` → `reduction(||:acc)`/
`reduction(&&:acc)`. A filter inside the loop body is an ordinary `if` guarding the update, exactly
as the sequential chain already emits it — OpenMP's reduction clause does not care that an
iteration sometimes contributes nothing. `-fopenmp` is added to the `cc()` invocation (`main.rs`)
only for a build whose module actually lowers a `.par()` chain, so a program that never uses one
gets the same command line as today, and no new runtime dependency (`libgomp`) it did not ask for.

## 6. The thing most likely to be wrong, named before it is measured

`T ≤ W/P + O(S)` is the classical work-span bound, and it is a **pure compute** model: it has no
term for the fact that `P` cores share one path to memory. M1 already established that several of
this project's own kernels — `sum`, `dot` — are bandwidth-bound, not compute-bound: doubling the
data barely changes work per byte, and the counter shows a stable, explained gap
(experiments.md §M1 "a pure read stream registers half its lines as refills"). A `.par()` sum over
such a kernel predicts linear speedup in `P` up to core count; on real hardware, aggregate memory
bandwidth is a shared, finite resource, and speedup should flatten once enough cores are saturating
it — well before `P` runs out, on most machines.

This is not fixed here because it should not be decided here: it is exactly the kind of claim this
project's whole method exists to test rather than assume (decisions §1–§3). The exit test (§7)
runs the same `.par()` chain shape on a compute-bound kernel (a closure with real arithmetic per
element, work dominating) and a memory-bound one (`sum`, moves dominating), across a sweep of `P`.
**If the memory-bound kernel's wall-clock time flattens while `W/P` keeps predicting improvement,
`T ≤ W/P + O(S)` is shown wrong for the kernels this project cares most about**, and the honest fix
is the same shape as the fit test's own: a second term, `moves/BW` for some aggregate bandwidth
`BW`, and the reported bound becomes `T ≤ max(W/P, moves/BW) + O(S)` — a roofline, not a straight
line — decided by what the machine says, not proposed here as a fait accompli. If the compute-bound
kernel does scale as `W/P` predicts and the memory-bound one does not, that difference *is* the
result: the model's compute term is right, and it needs a second, memory term to be complete. If
neither scales as predicted, the compute term itself is wrong and `P` is reconsidered from there.

## 7. Exit tests

1. **Nothing already exact changes.** Every existing golden's `.cost` file is unchanged
   (`span == work` everywhere `.par()` does not appear, computed but not yet printed unless a
   `.par()` chain is present, so the report line does not even show up to differ).
2. **The chain span, predicted and checked against its own formula.** `xs.iter().par().map(|x| x
   * x).sum()`: predicted span `O(1) + O(log n)` at several `n`, checked structurally (the
   reported polynomial has a `Log` atom at the right coefficient) — this is an assertion about the
   compiler's arithmetic, not yet about the machine.
3. **Wall-clock scaling, a compute-bound kernel.** A `.par()` chain whose closure does real
   arithmetic per element (enough that work dominates moves by the model's own numbers), timed at
   `P = 1, 2, 4, 8` on the pinned big cluster (`taskset -c 5-9` and `15-19` give ten cores;
   `OMP_NUM_THREADS` set per run). Exit: wall-clock tracks `work/P + O(log n)` within a stated
   factor, the same tolerance discipline as every other measured claim in this file.
4. **Wall-clock scaling, a memory-bound kernel.** The same sweep on `.par()` `sum`. Exit: whatever
   the machine says (§6) — this test's job is to produce the number that decides §6, not to pass
   or fail a predetermined bar.
5. **The associativity refusal.** `.par()` before `.fold(...)` is rejected with the line and the
   reason; every associative terminal (`sum`, `count`, `max`, `min`, `any`, `all`) accepted.

## 8. Order

`P` as a machine parameter, wired through `Atom`/`Machine`/`-P`/`--eval` the way `M`/`B` already
are (mechanical, no design choice) → `.par()` parsed and typed, restricted to the associative
terminals → `span: Cost` added to `Fa`, mirroring `work` by default, sequential composition needs
nothing new → the `.par()` loop's span rule (`O(log trip)` instead of `Σ_v`) → the report line and
`#[cost(span_at_most = ...)]` → OpenMP emission, conditional `-fopenmp` → exit test 1–2 (structural,
no hardware) → the wall-clock harness (new: core-affinity lists, `OMP_NUM_THREADS`, timing instead
of `perf` counters) → exit tests 3–4, measured, §6 decided by what they show.

## 9. Open questions, answered provisionally

- *Why not let `.par()` sit on a general `for`?* Because nothing today proves a `for` body doesn't
  race, and inventing that proof is exactly the "team-scale problem" decisions §4 warned against
  extrapolating a pace onto. Chains already have the proof as a side effect of fusion; spending it
  is free, inventing a new one for loops is not.
- *Does `.par()` compose across a call boundary — can a callee's `.par()` chain contribute a
  parallel span to its caller?* Yes, the same way a callee's work already does (§3, "a call's
  span"): the caller's cost composes the callee's signature, span included, atoms substituted. A
  caller does not need to know the callee parallelised anything to get the right bound.
- *What if two `.par()` chains run one after another in the same function?* Sequential composition
  (§3): their spans add. Whether the *machine* actually runs them one after another or a real
  scheduler could overlap independent ones is exactly the kind of scheduling question `O(S)` is
  named to absorb rather than model; this project's argument is about what a function costs as
  written, not the best schedule an optimiser could find for a whole program (decisions §5, on
  IOUB's upper bound being a different question from this one).
- *Nested `.par()` — a `.par()` chain whose closure calls a function that itself has a `.par()`
  chain?* Refused for now: the closure-purity argument (§0) says the outer iterations don't race,
  but says nothing about two levels of OpenMP `parallel for` nesting safely and efficiently, which
  needs `omp_set_nested` or `collapse` and a cost rule this design does not attempt. A function
  called from inside a `.par()` closure that itself contains `.par()` is a type error until this is
  designed on purpose.

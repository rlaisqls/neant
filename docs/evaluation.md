# Evaluation

What neant set out to show, what was measured, and where the measurements stand on 2026-09-28.
Every number here is from a document that shows how it was taken; this file only puts them side by
side. The machine throughout is one Cortex-X925 core (CPU 5) of the development machine, L2 2 MiB
private, L3 16 MiB, pinned with `taskset`.

## The claim

The README's: *what a program costs is a property the compiler infers, reports, and can be asked to
hold — the way a type is.* A cost is work, bytes moved across a cache boundary `M` (the I/O model's
`moves`), and span; it composes through calls because it lives in the signature; it is recorded in
`costs.lock` and reviewed like code.

That breaks into four questions, each answered below with the measurement that answers it:

1. **Bytes.** Does `moves` predict the bytes the machine actually moves?
2. **Time.** With two constants per machine, does the cost predict wall-clock?
3. **Reach.** On code not written to suit the rules, how much gets a cost at all? (plan § Stage D:
   *the governing number is that the exact tier moves on ordinary code*)
4. **Trust.** When the compiler says `exact`, is it?

## 1. Bytes: the moves model against the machine's counters

experiments.md § M1, § M2, § M4, § M5. Predicted bytes against `l2d_cache_refill × 64` from `perf
stat`, over size sweeps past the cache.

- Log-log slopes agree within 0.1 on the five kernels whose access pattern does not change with
  size (`sum`, `dot`, `saxpy`, `transpose`, tiled `matmul`); per-kernel ratios are stable across the
  upper half of every sweep, within a factor of three.
- The model separates naive from tiled matrix multiply: **measured 30× apart at n = 1984,
  predicted 28×**.
- The compiler's own costed rewrites (`--apply matmul:tile`, `transpose`) landed within 1.4× of their
  predicted effect, in the predicted direction and ranking; the tile side the model chose beat the
  hand-written 64 by 1.75×.
- Layout (AoS against SoA, M4) and in-place reuse against a forced copy (M5) move the machine in the
  ratio predicted.

Where it fails, it fails where the ideal-cache model is known to: at sizes where all operands
together exceed `M` by little (ratio 0.05 at n = 448 for three matrices of 4.8 MiB), and in set
conflicts at power-of-two strides, which the sweeps avoid on purpose.

## 2. Time: the roofline

cost-model § Time, experiments.md § The roofline, § The corpus against the clock (both tables).
`time = max(span·τ, work·τ/P, moves/BW)`, with `τ = 0.0176 ns` per unit of work and
`BW = 20.8 GB/s`, fitted once — `τ` on a polynomial in L1, `BW` on a 100 MB stream — and then held
fixed for everything else; and a latency term, `chase/B · L` in place of `chase/BW` for the lines a
pointer chase fetches, with `L = 112 ns` fitted on a chase over 64 MB; and serial work, `τ_s = 0.155
ns` for work on a chain each lap waits on, fitted on a logistic map, and charged too for an `f64` a
short lap carries through an add; and `τ_div = 0.148 ns` more for an
`f64` division, fitted on a sum of reciprocals. Five constants in all, each
fitted on one kernel written for it and then held fixed. The geometric mean of measured over predicted
over the kernels' 31 runs is 1.1, and the root-mean-square of the log ratio 0.53 (0.61 before
streams and write-backs were charged) (1.56 before the latency term, when `arena` was 5–31× too low).

**Kernels** (31 runs, `tests/kernels/roofline.py`):

| where | measured / predicted |
|---|---|
| streams past the cache (`sum`, large `saxpy`) | 0.87 – 1.25 |
| tiled `matmul`, every size | 0.89 – 0.93 |
| naive `matmul` past the cache | 0.94 – 1.16 |
| two streams, a multiply-add chain (`dot`) | 1.31 – 1.41 |
| `transpose` (a new page a column) | 1.25 – 2.19 |
| data that fits in `M`, and fresh allocations | up to 5 |
| `arena`, a pointer chase past L3, with the latency term | 1.00 |
| `arena` inside L3 / inside L2 | 0.58 / 0.28 |

**Cores** (the M5 sweep, `par_sweep.py`): with the memory's aggregate bandwidth as a ceiling,
`BW(P) = min(P·BW, 65.6 GB/s)`, a parallel sum is 1.44–1.49 at P = 1, 2, 4 (0.99 at ten, where it was
fitted) and a compute-bound map 2.0–3.1, each flat in `P`: the model now tells a chain that saturates
the memory from one that scales, which M5 found it could not, and is off by one constant each.

**The corpus's programs** (`tests/corpus/timing.py`, three generated input sizes each), after the
changes the timings forced (cost-model § A scan's accesses, § An amortised scan; experiments.md):

| program | measured / predicted |
|---|---|
| `fir` (read 10⁴–10⁶ integers, filter) | 1.93 – 2.08 |
| `pid` (read 10⁴–10⁶ integers, control loop) | 1.27 – 1.56 |
| `csv` (10⁴–10⁵ rows) | 0.81 – 0.95 |
| `matmul` (n = 100–600) | 1.35 – 2.66 |
| `heat` (n = 300–900, 50 steps) | 0.63 – 0.75 |

Every program is within **0.6 to 2.6** of its measured time, geometric mean 1.33 (0.56 before a
parse's per-call line ends were charged once and its serial work was carried through the
amortised calls; the ends had hidden the dropped serial work, and the error changed side).

**The Benchmarks Game's programs** (`tests/bench/timing.py`, experiments.md), none used in a fit:

| program | measured / predicted |
|---|---|
| `nbody` (10⁵–5·10⁶ steps) | 1.75 – 1.81 |
| `spectral_norm` (n = 200–2000) | 0.96 – 1.26 |
| `mandelbrot` (n = 200–2000) | 0.47 – 0.50 |

Geometric mean 1.00, range 0.47 to 1.81 — n-body errs the other way from before, its per-step moves
gone (the L2 refills agree) and its compute a little under-counted: laps of four or fewer. Before the scan's
accesses were charged as a stream, the text readers were 10³ to 10⁶ too high — a bound, and a
useless time; the first timing is kept in experiments.md because it is what found that.

## 3. Reach: how much gets a cost

**The compiler on itself** (`compiler/costs.lock`, `compiler/lock.sh`), the largest program in the
language, written to compile itself and not to suit the calculus:

| when | exact | modulo | bound | unknown | of |
|---|---|---|---|---|---|
| stage D starts (2026-09-23) | 81 | — | — | 195 | 276 |
| unknown callees as terms, sizes read from memory, list walks, the lockfile | 92 | 47 | 32 | 105 | 276 |
| the sign audit, a size bound once, the scan rule | 92 | 49 | 34 | 103 | 278 |
| a loop that surely runs (the lexer) | 92 | 49 | 35 | 102 | 278 |
| a `while` condition read right (`j > start`, `i + 1 < n`) | 94 | 51 | 35 | 98 | 278 |
| reads stale by slot and by field, not by array | 94 | 51 | 36 | 97 | 278 |
| recursion over a tree in an arena | 94 | 50 | 38 | 96 | 278 |
| and over a forest: mutual recursion on one arena, a path at a time | 94 | 49 | 42 | 93 | 278 |
| a scan to a sentinel, bounded by the array's end | 94 | 50 | 42 | 92 | 278 |
| a tree's invocations costed apart from their node: reads widened, writes refused | 94 | 52 | 43 | 89 | 278 |
| a callee's size in a loop that writes it, refused (a fix); a cursor in a slot | 95 | 43 | 44 | 96 | 278 |
| such a size only an unknown callee's argument is `_` there, now | 95 | 45 | 44 | 94 | 278 |

Exact did not move after the first extensions; what moved is how much is stated at all, 81 of 276
to 184 of 278, a fifth of it `bound`. A recursion over a tree in an arena, one function's or a
component's, is a bound now (cost-model § Recursion, a tree in an arena, and a forest); the cost
walkers have that shape but are unknown because their costs read state the walk writes, which
their lines now name, and the checker's and emitter's call back in once a lap of a counted loop. `lex` became a bound once a nested loop its guard
surely enters was counted, but its callers are unknown for their own reasons, so one line moved.

**The domain corpus** (`tests/corpus`, docs/corpus.md): six programs of the kinds stage C names —
a PID loop over a trajectory file, a Jacobi stencil, a dense `matmul`, a CSV aggregator, BFS over an
edge list, an FIR filter — plus two written when string output and grids arrived.

| when | exact | bound | unknown | mains with a cost |
|---|---|---|---|---|
| first count | 14 | 0 | 8 (+2 modulo) | 0 of 6 |
| now | 18 | 10 | 0 | 8 of 8 |

Every kernel and controller step is exact; every `main` is a bound, because each begins by reading
text, and a number read out of text is bounded, not counted.

## 4. Trust: is `exact` exact?

The calculus has been caught wrong, and saying so is part of the evaluation.

- **Two compilers.** The self-hosted cost pass is checked column by column against the Rust one
  (`bootstrap/tests/self_host_cost.rs`): 106 work, 92 moves, 149 footprint, 192 bound columns agree
  exactly, and every place one states less than the other is listed with its reason. The check has
  found bugs in both — most recently the self-hosted pass costing a worklist as one pass, because it
  did not see an assignment in an `if` at a block's tail (plan, 2026-09-27).
- **A size through a call.** A callee's size read from an array its caller's loop writes stood for
  every lap's value, the first's: nine `modulo` lines of the compiler were wrong, seven of them
  unknown now (cost-model § A size read from memory, through a call). The same session found the mutual
  recursion rule composing invocations whose reads the recursion itself writes, before it was
  committed.
- **The sign audit** (`NEANT_SIGNS=1`) samples every cost piece the prover cannot show non-negative.
  It found a rewrite that dropped a factor from an upper bound; four other places the calculus had
  said `exact` while wrong were found by reading the lockfile (plan § Stage D, "What the lockfile
  showed").
- **One decision the calculus made without saying so**, now written down (cost-model § Loops): a
  cost holds where every loop it counts runs a non-negative number of times.

So the tiers are checked, repeatedly, and each fix came with a golden that reproduces it. They are
not proved: there is no mechanised soundness argument, and the rate at which wrong `exact` lines
were found through stage D says the next one is likely.

## What fails, and why

Each of these is a term the model lacks or a shape the calculus does not reach, measured, not a
constant to tune:

- **One cache level.** Data inside `M` moves nothing in the model; L2 bandwidth and page faults are
  real (small sizes, up to 6×), and a chase inside L3 waits for L3 rather than memory (`arena`,
  0.28–0.58 there). The latency term closed the chase's gap past the last cache (5–31× → 1.00).
- **Strides and the TLB** (`transpose`, 1.3–2.2× with its write-backs counted), and **a
  multiply-add's latency** (`dot`, 1.3–1.4×), which the short-lap rule charges as an add's.
- **Reuse between neighbouring sites, in part.** A stencil's five reads are three streams now, not
  five (cost-model § Neighbouring sites); the centre row is still counted apart from the rows it
  shares with its neighbours (`heat`, 2× high, from 8×).
- **A branch the data decides** (`fir`, 2× low): its parse's digit loops end at a length that
  varies number to number, a mispredict each, which `csv`'s regular numbers do not pay.
- **A `τ` for code that vectorises worse** than the polynomial it was fitted on (`matmul` at small
  `n`, 2.4–2.7× low).
- **Reach.** `while` loops with no measure the compiler finds, inside the walkers and in the
  parser's cursor loops, leave a third of the compiler unknown; a recursion over a tree in an arena
  is a bound, a promise the program does not state.

## Threats to validity

- **One machine, one core.** `τ` and `BW` are this core's; nothing here is checked on a second
  machine or with more than one core, where the M5 measurement already showed the roofline is needed
  and did not fit `BW` for it.
- **The corpus is the project's own.** It was written the way someone would write those programs,
  not shaped to the rules, and what it had to be written around is kept as rejected cases — but it
  is not independent code, and eight programs are few. Five Benchmarks Game programs, ported with
  their published structure (`tests/bench`, experiments.md), are 11 of 15 functions exact and one
  more a bound (a tree recursion) — the unknowns an exponential recurrence and data-driven
  permutation loops — and
  their times are within 0.50–1.77 of the predicted (0.15–4.3 before a triangle's moves and serial
  work were charged, each found by these programs and fitted elsewhere).
- **Generated inputs.** Uniform random integers and rows; real data with long lines or skew would
  move the parse's constants.
- **Bounds err high by design, and on the corpus no longer do.** Its geometric mean was 0.56 while
  the parse overcharged its moves; with that removed it is 1.26, and three programs are predicted
  faster than they run: the time is an estimate there, not a bound.
- **The fitted constants are gcc's laps, not the core's.** The kernels they were fitted on repeat
  one call, and gcc runs two repeats in one vector iteration (experiments.md, M7's first probe):
  `τ`, `τ_s`, `τ_div` are half a chain's latency where a program's chains cannot be paired. Fitted
  on unpaired repeats they double, and the programs' log error falls from 0.48 to 0.27 (n-body
  0.94, `fir` 1.12) while the kernels' rises from 0.53 to 0.72 (a tiled `matmul` 0.28): one `τ` is
  either a chain's latency or a vectorised loop's throughput, not both.
- **Wall-clock, minimum of three to five runs,** on a machine shared at times with other sessions;
  the kernels' table was taken idle.

## Where this leaves the claim

On its own domain the claim holds in the form plan § Who switches set for it: every program in the
corpus has a cost the compiler inferred, the kernels' costs predict bytes to within the ideal-cache
model's known limits and time to within about 30% where one term dominates, and the programs' costs
predict their time within a small factor either way (0.45–2.7) — and so do five programs the project did not
write, with constants fitted on none of them (0.50–1.77). What it does not yet do is reach the larger
part of ordinary code *exactly* — the compiler is a third exact, a third stated as a bound or modulo
a callee, a third unknown — and every place it is loose is a named term or shape, each with the
measurement that found it.

A second cache level was tried (`--M3`, experiments.md): it helps mid-size streams and hurts strided
access in equal measure, so it is not the default. A TLB charge per paged line (`--tlb`) fits
`transpose` and over-charges naive matmul fourfold, so it is off too. Next, in the order they would change these
numbers: a bandwidth that depends on the access pattern (a stride, the number of streams) at each
level (the small sizes, and a chase inside L3), `fir`'s filter and `matmul`'s strided dot product,
a second machine for `τ`, `BW` and `L`, and, for reach, the `while` loops the walkers are waiting on.

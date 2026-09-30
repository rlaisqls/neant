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

## 5. Held-out: PolyBench, row one (2026-09-29)

The first numbers on code the project did not write and fitted nothing on: the 30 PolyBench/C
4.2.1 kernels (heldout.md), ported by its rules, each checked against the original C before it was
costed (`check.nt`: 30 of 30 at MINI, 29 of 29 at SMALL, gramschmidt not comparable there), costed
and timed with the compiler of the frozen commit `42f76be`. The data is
`tests/heldout/polybench/row1.json` (90 runs, 30 kernels × MEDIUM, LARGE, EXTRALARGE), each port's
report its `main.cost`, the tiers `tiers.txt`; `summary.nt` reads the JSON and applies
heldout.md's thresholds, and prints what is below.

| question | row one | heldout.md | verdict |
|---|---|---|---|
| **Bytes** (EXTRALARGE, footprint past L3) | 18 kernels, geometric mean 0.64, 15 within [1/3, 3] (83%) | holds: mean in [0.5, 2] and 80% in [1/3, 3] | **holds** |
| **Time** (every run with predicted and measured ≥ 1 ms) | 51 runs, geometric mean 1.69, 11 outside [0.25, 4] (21.6%) | fails: more than 20% outside [0.25, 4] | **fails** |
| **Reach** (port-defined functions) | 114 of 122 stated, all exact; 8 unknown, the two refusals | holds: stated ≥ 2/3 and exact ≥ the C analyser's | first half met; judged with the C analyser's row |
| **Ranking**, **Trust** | — | steps 6 and 7 | later rows |

**Bytes hold.** Where a kernel's arrays pass the last cache, the moves model is as good on code it
never saw as on its own: 2mm, 3mm, gemm, fdtd-2d, correlation and covariance within 0.97–1.24, the
geometric mean 0.64 pulled low by the matrix–vector kernels. The three outside are atax (0.25),
bicg (0.32) and trisolv (0.33), each a single pass over a matrix that the model charges three to four
times over. deriche at a gigabyte is 2.55.

**Time fails, by one run.** The threshold is 20% of 51, 10.2 runs; 11 are outside, and one of them
is durbin at EXTRALARGE, 0.2495. Without it the row would be *between*; this file reports what the
threshold says. The misses are three shapes, each a term the time model lacks rather than a constant:

- **Gauss–Seidel's carried dependence** (seidel-2d, 5.3–5.8 at every size): each point waits for the
  one just written; the calculus charges the sweep at the rate of independent points.
- **A column walk that the bytes model gets right and the time model does not** (correlation and
  covariance 9.0, gramschmidt 8.7, at EXTRALARGE only; correlation's and covariance's bytes are
  1.17–1.24, and at LARGE their time is 3.7): the bytes arrive at a column's stride, and a strided line costs more time than `BW` gives it
  — evaluation's "strides and the TLB", at a hundred megabytes.
- **Matrix–vector kernels at 3–4.1** past the noise (atax, bicg, gemver, gesummv, mvt, trisolv): the
  one-pass matrix read whose bytes are overcharged is undercharged in time — most likely a dot
  product's serial add chain, which the short-lap rule prices as independent adds (the `dot` of
  § What fails); not yet measured apart.

And the opposite side, predicted slower than they run: the stencils jacobi-2d (0.41–0.42) and
fdtd-2d (0.39–0.65), whose bytes are overcharged 3× (0.33–0.34 at jacobi-2d) — the neighbouring-site
reuse of § What fails, still counted apart.

**No prediction for a third of the kernels, one cause for most.** Ten kernels have no time or bytes
line: adi and heat-3d are refusals (the frozen compiler's cost pass did not finish in 900 s; its
`settle_moves` tries every combination of sites), and in the other eight — cholesky, lu, ludcmp,
symm, syrk, syr2k, trmm, nussinov — `main` is *exact* but its regimes are conditions on the outer
variable of a triangular loop (`16·kernel_cholesky.i < M`), which `--eval` cannot be given and no
caller can decide. The fit test of a loop whose inner extent depends on the outer index is left in
that index instead of being split over it. It is the largest single gap row one found: 8 of 30
kernels, exact in form and unusable as a prediction.

**How it was measured, and what differs from heldout.md.** Wall-clock is the minimum of three runs
pinned with `taskset`, an empty program's time subtracted; bytes are `l2d_cache_refill × 64`, the
minimum of three runs under `perf stat`, the empty program's subtracted. Load was 1.0–2.1. Seven
kernels' runs (21 of 90) were taken on CPU 15, the other L3 cluster's X925, alongside CPU 5, which heldout.md
does not name: gemm rerun on CPU 15 while CPU 5 was busy agreed with its CPU 5 run within 3% in time
and 0.2% in bytes (`row1-cpu15-check.json`), and of the seven kernels counted from CPU 15 only mvt
has a prediction. So the verdict rests on two runs at the edge: durbin's EXTRALARGE at 0.2495 on CPU 5, and mvt's
EXTRALARGE at 4.08 on CPU 15 — were either inside [0.25, 4], 10 of 51 would be outside, 19.6%, and
the row *between*. The harness is neant
(`measure.nt` over `std/os.nt`, decisions §14), not the Python heldout.md's step 2 names.

### Later rows (2026-09-29)

Each a later compiler costing the same runs: the emitted C of the 27 ports that build under every
compiler here is byte-identical to `42f76be`'s, so the binaries are row one's and only the
predictions are new (`measure.nt --predict`). Row one is not replaced.

| row | compiler | time | bytes | no prediction |
|---|---|---|---|---|
| 1 | `42f76be`, frozen | 51 runs, mean 1.69, 11 outside — **fails** | 18 kernels, mean 0.64, 15 in — **holds** | 10 kernels |
| 2a | `cc66c66`: the time model's two changes since the freeze | 51 runs, mean 1.65, 8 outside — **between** | as row 1 | 10 kernels |
| 2b | `5e997b9`: 2a and a triangle's fit test | 75 runs, mean 1.53, 10 outside — **between** | 26 kernels, mean 0.66, 22 in — **holds** | 2, the refusals |
| 2c | `97cb921`: 2b and a store that reads the last lap's | 75 runs, mean 1.42, 7 outside — **between** | as 2b — **holds** | 2, the refusals |

What moved each, so that neither is credited with the other's: **2a** is `17d7d75` (a carried
`f64` add in a short lap is serial work) and `1be9f2a` (two streams share a bandwidth; a stored
line is written back), both committed on 2026-09-28, before the first held-out number (09-29
14:19); they raise the matrix–vector kernels' predicted time by a fifth to a third, which brings
bicg at LARGE, trisolv and mvt at EXTRALARGE inside [0.25, 4], and that is the whole of the move
from *fails* to *between*. **2b** is `f91bd83` and `5e997b9` (cost-model § A triangle's fit test):
the eight kernels whose `main` was exact but named a loop variable get predictions, 24 runs more,
of which syrk, syr2k, symm, cholesky, lu and ludcmp land inside and trmm at EXTRALARGE (5.58) and
nussinov at MEDIUM and LARGE (0.17, 0.23) outside; nussinov's bytes are the one new kernel outside
[1/3, 3] (0.17). The verdicts are the same as 2a's, on half again as many runs.

`f91bd83` alone was a row too, and a bad one — time *fails* with 18 of 74 outside, bytes *between*
at a mean of 0.38 (`row-f91bd83.json`): once its regimes could be evaluated, two older faults in a
triangle's hull were chosen by them (lu and ludcmp predicted at `64·n⁴/(3·B)` bytes, nussinov at
`n⁴`), which `5e997b9` fixed. It is kept, since a row is not dropped for being worse.

**2c** is `97cb921` (2026-09-30): a store that reads the element the last lap stored is a
recurrence through memory (cost-model § Time, a store that reads the last lap's), and seidel-2d's
passes a division, so its whole lap is serial. Only seidel-2d's three runs move, from 5.3–5.8 to
0.78–0.81, with `τ_s` as it was; time is 75 runs, mean 1.42, 7 outside — **between** — and bytes
as 2b (`row2c-97cb921.json`).

What is left, in 2c: correlation's and covariance's column walk at EXTRALARGE (9.0) and
gramschmidt's (8.7), trmm at EXTRALARGE (5.58), nussinov overcharged about fivefold (0.17, 0.23; its
inner column walk charged lap by lap), durbin at EXTRALARGE (0.2495), the matrix–vector bytes
(0.25–0.33), and the two refusals. *Holds* needs every run inside [0.25, 4]: seven to go.

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

**A per-loop cost line (M7's first slice, experiments.md).** Each innermost loop of the emitted
assembly, mapped back to the calculus's loop (`neant emit --lines`), costed by a model of this core
measured on dependency chains, independent chains, the divider and a missed exit, and multiplied by
the laps and entries the calculus gives (`--eval --laps`): spectral-norm 1.07, n-body 0.95, the
corpus's `matmul` 0.96 and 1.02, `heat` 0.82 — a log error of 0.10 against the calculus's 0.56 on
the same runs, with no constant fitted on a program. It needs laps the calculus can state exactly;
where they are a bound (mandelbrot) or amortised away (`fir`), it cannot be applied.

## Threats to validity

- **One machine, two cores.** The same programs on the machine's other core, a Cortex-A725
  (experiments.md, a second core): the calculus with the X925's constants is 2–4.5× off, refitted on
  the A725 a log error of 0.34; M7 with the A725's measured table 0.19, the X925's 0.10 — the method
  carries to a second microarchitecture with its microbenchmarks rerun. Not a second machine: the
  memory, the compiler and the operating system are the same.
- **One machine, one core** (as first written). `τ` and `BW` are this core's; nothing here is checked on a second
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

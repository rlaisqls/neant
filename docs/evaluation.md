# Evaluation

What neant set out to show, what was measured, and where the measurements stand on 2026-09-27.
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
pointer chase fetches, with `L = 112 ns` fitted on a chase over 64 MB. The geometric mean of
measured over predicted over the kernels' 31 runs is 1.14 (it was 1.56 before the latency term, when
`arena` was 5–31× too low).

**Kernels** (31 runs, `tests/kernels/roofline.py`):

| where | measured / predicted |
|---|---|
| streams past the cache (`sum`, large `saxpy`) | 0.87 – 1.02 |
| tiled `matmul`, every size | 0.89 – 0.90 |
| naive `matmul` past the cache | 0.93 – 1.15 |
| two or three streams (`dot`, small `saxpy`) | 0.60 – 0.71 |
| `transpose` (a new page a column) | 1.9 – 2.9 |
| data that fits in `M`, and fresh allocations | up to 6 |
| `arena`, a pointer chase past L3, with the latency term | 1.00 |
| `arena` inside L3 / inside L2 | 0.58 / 0.28 |

**The corpus's programs** (`tests/corpus/timing.py`, three generated input sizes each), after the
changes the first timing forced (cost-model § A scan's accesses):

| program | measured / predicted |
|---|---|
| `fir`, `pid` (read 10⁴–10⁶ integers, filter / control loop) | 0.41 – 0.53 |
| `csv` (10³–10⁵ rows) | 0.10 – 0.22 |
| `matmul` (n = 100–600) | 1.36 – 2.66 |
| `heat` (n = 300–900, 50 steps) | 0.48 – 0.51 |

Every program is within **0.10 to 2.7** of its measured time; the geometric mean was 0.41 before the
stencil's neighbouring sites were grouped and `heat` moved from 0.13 to 0.50. Before the scan's
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
| a loop that surely runs (the lexer), now | 92 | 49 | 35 | 102 | 278 |

Exact did not move after the first extensions; what moved is how much is stated at all, 81 of 276
to 175 of 278, a third of it `bound`. The largest remaining row is mutual recursion over the
compiler's own tree walks (plan § Stage D (3)). `lex` became a bound once a nested loop its guard
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
  (`bootstrap/tests/self_host_cost.rs`): 105 work, 103 moves, 140 footprint, 152 bound columns agree
  exactly, and every place one states less than the other is listed with its reason. The check has
  found bugs in both — most recently the self-hosted pass costing a worklist as one pass, because it
  did not see an assignment in an `if` at a block's tail (plan, 2026-09-27).
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
- **Strides and the TLB** (`transpose`, 2–3×), and **bandwidth that grows with the number of
  streams** (`dot`, 0.6×).
- **Reuse between neighbouring sites, in part.** A stencil's five reads are three streams now, not
  five (cost-model § Neighbouring sites); the centre row is still counted apart from the rows it
  shares with its neighbours (`heat`, 2× high, from 8×).
- **Per-call constants in a parse.** `3·B` of partly used lines at each end of a call, which
  consecutive calls share (`csv`, 5–10× high).
- **A `τ` for code that vectorises worse** than the polynomial it was fitted on (`matmul` at small
  `n`, 2.4–2.7× low).
- **Reach.** Mutual recursion, recursion over a tree in an arena, and the lexer's scan shapes leave
  a third of the compiler unknown.

## Threats to validity

- **One machine, one core.** `τ` and `BW` are this core's; nothing here is checked on a second
  machine or with more than one core, where the M5 measurement already showed the roofline is needed
  and did not fit `BW` for it.
- **The corpus is the project's own.** It was written the way someone would write those programs,
  not shaped to the rules, and what it had to be written around is kept as rejected cases — but it
  is not independent code, and eight programs are few.
- **Generated inputs.** Uniform random integers and rows; real data with long lines or skew would
  move the parse's constants.
- **Bounds err high by design.** A geometric mean of 0.41 is a bound behaving as one; it is also a
  factor of two and a half of slack at the median.
- **Wall-clock, minimum of three to five runs,** on a machine shared at times with other sessions;
  the kernels' table was taken idle.

## Where this leaves the claim

On its own domain the claim holds in the form plan § Who switches set for it: every program in the
corpus has a cost the compiler inferred, the kernels' costs predict bytes to within the ideal-cache
model's known limits and time to within about 30% where one term dominates, and the programs' costs
predict their time within a small factor, erring high. What it does not yet do is reach the larger
part of ordinary code *exactly* — the compiler is a third exact, a third stated as a bound or modulo
a callee, a third unknown — and every place it is loose is a named term or shape, each with the
measurement that found it.

A second cache level was tried (`--M3`, experiments.md): it helps mid-size streams and hurts strided
access in equal measure, so it is not the default. A TLB charge per paged line (`--tlb`) fits
`transpose` and over-charges naive matmul fourfold, so it is off too. Next, in the order they would change these
numbers: a bandwidth that depends on the access pattern (a stride, the number of streams) at each
level (the small sizes, and a chase inside L3), the parse's per-call constants (`csv`),
a second machine for `τ`, `BW` and `L`, and, for reach, recursion over an arena tree.

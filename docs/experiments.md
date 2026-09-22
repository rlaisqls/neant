# Experiments

What was measured, against what prediction, and what it changed. The numbers are the argument.

## M1 — does the moves model predict the machine?

**Claim under test.** `neant cost` derives, for a whole program with concrete sizes, the number
of bytes that cross the L2 boundary. If that number does not track what the hardware counts, the
project's central claim is false and nothing downstream of M1 should be built.

**Method.** Six kernels (`tests/kernels/*.nt.in`), each instantiated at four or five sizes,
compiled with `neant build --unchecked`, run pinned to one Cortex-X925 core (`taskset -c 5`,
L1d 64 KiB, L2 2 MiB private, L3 16 MiB, 64-byte lines), measured with
`perf stat -e l2d_cache_refill`, minimum of three runs. Predicted = `neant cost --eval B=64`
on `main`, which includes the setup loops. Measured = refills × 64. Sizes avoid powers of two,
and the tiled product's sizes are 64·(odd), for the reason given under findings. Kill criterion
from the plan: the model cannot separate naive from tiled matrix multiply, or the ratio wanders
by an order of magnitude across sizes.

`tests/kernels/sweep.py` produces the table.

### Result

Predicted / measured, bytes across L2. Ratio = predicted ÷ measured. Slopes are log-log over the
upper half of each sweep, where the I/O model is meant to hold.

| kernel | n | predicted | measured | ratio |
|---|---:|---:|---:|---:|
| sum ×20 | 800 000 | 1.41e8 | 5.00e7 | 2.81 |
| | 3 200 000 | 5.63e8 | 2.45e8 | 2.30 |
| | 12 800 000 | 2.25e9 | 1.02e9 | 2.22 |
| | *slope* | **1.00** | **1.09** | |
| dot ×20 | 800 000 | 2.82e8 | 1.14e8 | 2.47 |
| | 3 200 000 | 1.13e9 | 5.03e8 | 2.24 |
| | 12 800 000 | 4.51e9 | 2.05e9 | 2.20 |
| | *slope* | **1.00** | **1.04** | |
| saxpy ×20 | 800 000 | 2.75e8 | 2.31e8 | 1.19 |
| | 3 200 000 | 1.10e9 | 1.01e9 | 1.09 |
| | 12 800 000 | 4.40e9 | 4.12e9 | 1.07 |
| | *slope* | **1.00** | **1.04** | |
| transpose ×5 | 2000 | 4.16e8 | 3.26e8 | 1.28 |
| | 3000 | 9.36e8 | 7.46e8 | 1.26 |
| | 4000 | 1.66e9 | 1.38e9 | 1.21 |
| | *slope* | **2.00** | **2.08** | |
| matmul, naive | 832 | 4.65e9 | 4.87e9 | 0.95 |
| | 1216 | 1.45e10 | 1.54e10 | 0.94 |
| | 1600 | 3.29e10 | 3.58e10 | 0.92 |
| | 1984 | 6.27e10 | 7.01e10 | 0.89 |
| | *slope* | **3.00** | **3.10** | |
| matmul, 64×64 tiles | 832 | 1.22e8 | 1.15e8 | 1.06 |
| | 1216 | 3.31e8 | 4.44e8 | 0.75 |
| | 1600 | 6.96e8 | 1.20e9 | 0.58 |
| | 1984 | 2.20e9 | 2.33e9 | 0.95 |
| | *slope* | 3.87 | 3.39 | see below |

**Naive against tiled at n = 1984: measured 30×, predicted 28×.** The kill criterion is not
met. Slopes agree to within 0.1 on the five kernels whose access pattern does not change with
size. Ratios are stable per kernel across the upper half of every sweep, within the plan's
factor of three; the constant differs by access pattern, which is the next finding.

The small sizes are not in the table: at 50 000 doubles (400 KiB) the whole program is a few
hundred thousand refills and process start-up is a visible fraction; at n = 448 for the products
all three matrices total 4.8 MiB and the ideal-cache assumption fails outright (ratio 0.05).
Those rows are in the sweep output and they are where the model is weakest, not where it is
tested.

### Findings, and what each one changed

**Two rules were added to the model by this experiment.** Both are in `docs/cost-model.md`.

1. **Access sites compete for the cache.** The first run predicted the tiled product at
   `n = 1984` as 1.26e9 against 2.33e9 measured, and the gap grew with `n` (ratio 1.02 at 832,
   0.53 at 1984). The `a`-panel the tiles reuse across the `jj` loop is 64 rows × n × 8 bytes —
   1 MiB at 1984 — and fits `M` on its own, which is what the model tested. But the `b` tiles
   streaming through the same loop level are another 1 MiB, and together with `c` the working
   set is exactly `M`. The hardware evicted `a`; the model kept it. The fit test now sums the
   working sets of every site sharing a loop. Prediction became 2.20e9.
2. **Fits means strictly less than `M`.** With the sum, the tiled working set at 1984 is
   32 768 lines × 64 = 2 097 152 = `M` to the byte. A cache is never empty of everything else,
   so equality does not fit.

**Three things the model does not see were measured, and are recorded rather than fixed.**

3. **A pure read stream registers half its lines as refills.** `sum` and `dot` sit at a stable
   2.2× over-prediction; `saxpy`, `transpose` and the products, which write, sit at 0.9–1.3×.
   On the read stream, `l2d_cache` (every L2 lookup) counted 34.6 M against 32 M lines touched
   plus 3.2 M of setup — the model's line count is right — while `l2d_cache_refill` counted
   15.9 M. The core's L2 prefetcher brings sequential read streams in as 128-byte pairs and one
   refill event covers two lines; a write stream in the same loop defeats the pairing. This is
   an accounting property of the counter on this core, stable per access pattern, and it is
   why "measured bytes" understates a pure stream by two. The plan's tolerance absorbs it.
4. **Associativity.** The first tiled sweep used `n = 1792 = 7·2⁸`. L1 refills went from 26 M at
   1344 to 1 995 M at 1792 — 75×, for 2.4× the work. A row stride of 1792·8 bytes is 224 lines,
   and `gcd(224, 256 sets) = 32`: a 64-deep column of `b` maps onto 8 of L1's 256 sets and
   cannot be resident. The ideal cache is fully associative and does not know this; the sweep
   was changed to `64·(odd)` sizes, where the stride reaches 32 sets and the column fits.
   Power-of-two-heavy sizes are a known hole and the model should say so when it sees one
   (M2, along with the lower-bound catalogue, since it needs the machine's geometry).
5. **The fit test is a step; the cache is a slope.** At 1216 and 1600 the tiled working set is
   65% and 85% of `M`; the model says it fits, and the machine re-reads part of it (0.75, 0.58).
   The transition the model puts at one `n` the hardware spreads over an octave. This is what
   the measured slope of 3.39 against the predicted 3.87 is: the same step, smoothed. A
   utilisation factor would fit these numbers and would be a number fitted to these numbers;
   it is not added.

### Verdict

The model's shape is right where the access pattern is fixed and right about the one
transition that matters — reuse or not — once it counts everyone in the loop. Its constants
are off by a stable factor that depends on the pattern and on how the counter works, in the
direction the plan reserved for measurement. M1's exit criterion is met, on the second version
of the rule set, and the two rules it forced are the experiment's real product.

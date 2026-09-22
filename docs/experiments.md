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

What this verified, said plainly: the *shape* — slopes, and the one transition between reuse and
no reuse — and a stable constant per access pattern. It did not verify numbers: the counter pairs
read streams and does not see write streams, so it was never in the model's unit, and the linear
kernels' slopes are near-trivial. The information is in the naive/tiled separation and in the two
rules the data forced.

## M2 — do the compiler's rewrites move the machine the way the model says?

**Claim under test.** `neant cost` offers two rewrites on a recognised matrix product, each
costed by running the calculus on the rewritten IR. If applying a rewrite does not change the
measured refills the way its costed suggestion says, the suggestions are decoration.

**Method.** As M1: the naive kernel `tests/kernels/matmul_naive.nt.in`, sizes `64·(odd)`, one
X925 core, `l2d_cache_refill × 64`, minimum of three runs — built three ways: as written, with
`--apply matmul:tile` (the compiler chose `T = 256` from `M = 2 MiB`), and with
`--apply matmul:transpose`. Every rewritten binary printed the same checksum as the original.
The naive row is from the M1 sweep on the same day.

### Result

| n = 1984 | predicted bytes | measured bytes | measured ÷ naive |
|---|---:|---:|---:|
| naive | 6.27e10 | 7.01e10 | 1 |
| `--apply matmul:tile` | 1.59e9 | 1.33e9 | **1/53** |
| `--apply matmul:transpose` | 6.28e10 | 6.00e10 | 1/1.17 |
| hand-written 64×64 tiles (M1) | 2.20e9 | 2.33e9 | 1/30 |

Predicted ÷ measured over the upper half of the sweep: tile 1.54, 1.18, 1.19; transpose 1.14,
1.07, 1.05. The model said tiling would remove 39× of the traffic; it removed 53×. The model
said transposing would remove nothing at this size — the column walk already shares each line
across eight consecutive `j` — and it removed 15%.

Distance to the Hong–Kung bound at this size: naive 1453×, tiled 37× predicted and 31× measured.

### Findings

1. **The costed suggestion is a prediction, and it held.** Both rewrites landed within 1.4× of
   their predicted effect, in the predicted direction, with the predicted ranking. The report's
   `[--apply]` lines are computed, not looked up, and this is the evidence that computing them
   was worth it.
2. **The compiler's tile beat the hand-written one by 1.75×.** `T` was chosen as the largest
   power of two with three tiles strictly inside `M`; the hand kernel used 64 because that is
   what one writes. A number derived from the machine did better than a number from habit, which
   is the argument for deriving it.
3. **Transposing bought 15% the model did not see.** In the ideal-cache model a column whose
   lines fit `M` costs the same as a row; on the core, a row is a stream the prefetcher runs
   ahead of and a column is not. The prefetcher is on the list of things the model does not
   see, and here is its size.
4. **Under tiling, the bound is counted on the hull.** The tiled nest's trip counts are
   `(n+T−1)/T` tiles of `T`, so `N` is counted as `(n+T−1)³` and the bound printed for the
   rewritten function is over by `(1+T/n)³` — 1.44× at `n = 1984`. The suggestion lines in the
   report therefore give the gap against the *original* function's bound, which is exact; the
   `--apply` path reports the inflated one. A precise count needs the calculus to know that a
   `min`-bounded tile loop and its `ceil` count multiply back to `n`, which it does not yet.
5. **The predicted slope of the tiled cost is wrong in a way that does not matter.** 2.50
   against 3.04 measured over 1216–1984: the `ceil` in the tile count makes the predicted
   polynomial step rather than grow, and over one octave that reads as a shallower slope. The
   ratio converges (1.54 → 1.19) as `n` grows past a few tiles.

### Verdict

M2's exit criterion is met: the report in the README is the compiler's output on the code in
the README, and the two rewrites it offers move the measured refills as their costed lines say —
one of them better than a person did by hand.


## M3 — the measured tier, checked against a known function

`neant measure` was pointed at a function whose cost the calculus knows, so its instructions
and refills could be read against the prediction.

```
count_lt         work 6·xs.len() + 1               moves 8·xs.len()                   exact
           n     instructions         L2 bytes   predicted work / moves
       20000           482625           211200   6.400e5 / 6.400e5
      320000          5882661          2512320   1.024e7 / 1.024e7
     5120000         92282669         81693248   1.638e8 / 1.638e8
count_lt         work ~n^0.99                     moves ~n^1.26                     measured over n = 20000..5120000
```

Work: the slope is 1, and 4.5 instructions per element against the model's 6 — work was
redefined after this run to approximate instructions (it said 8 "operations" at the time), and
the remaining gap is gcc fusing the compare into the branch and the increment into the address. Moves: half the predicted bytes at the top (the read-stream
pairing from M1, finding 3), and a slope above 1 because the two smallest sizes fit in L2 and
were repeated four times.

**Finding 6: a pure write stream does not refill.** BFS on the chain graph the driver builds
initialises `dist` — 20 MB at the top size — and the whole run registered 5×10⁵ bytes of
refills. Full-line store streams on this core go into write-streaming mode and bypass L2
allocation; a write-only pass is invisible to `l2d_cache_refill`. Reads pair up (M1), stores
vanish: what the counter calls a refill is a narrower thing than what the model calls a move,
and the difference is stable per access kind.

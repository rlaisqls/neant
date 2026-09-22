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


## Stage A — is moves a composable quantity?

**Claim under test.** A caller can compute its cost from a callee's signature — work, moves,
footprint, residue — without re-analysing the callee's body. The kill condition: if the numbers
the old call-site re-analysis produced cannot be reproduced from signatures, moves is not
composable and "cost lives in the signature" is withdrawn.

**Result.** Call-site specialisation and inherited loops were deleted. The naive product called
from `main` with `n = 1984` costs **62,696,939,712 bytes from the signature — the same figure to
the byte** the re-analysis gave, because the callee's piecewise cost, substituted and decided at
the machine's `B` and `M`, is the re-analysis. Twenty scans of a 1.6 MiB array cost one scan and
nineteen line-touches through the residue rule; twenty scans of a 32 MiB array cost twenty. The M1
and M2 kernel predictions (`sweep.py --dry`) are unchanged at every size. The exit criterion is
met; the kill condition is not.

**What changed in the numbers.** Sequential calls now credit each other: `dot(&a, &b)` followed by
`dot(&a, &a)` pays for `a` once. The goldens' `main` lines moved down by those credits and by
nothing else.


## Stage C — a declaration on the boundary, confirmed

`extern fn labs(x: i64) -> i64` declared `work_at_most = "4", moves_at_most = "0"`. `neant measure`
built the driver with and without the call, 10 000 repeats, three sizes, and subtracted:
instructions 0.0 per call (gcc inlines its own `labs`), refills 0.0–2.6 bytes per call — process
noise over the repeats — against a tolerance of one line. Confirmed; the lockfile line reads
`declared  measured over n = 1000..16000: confirmed`, and `total`, which calls it, `rests on labs
(declared, extern)`. A small thing measured; the shape of the chain is the point.

## Lower bounds — IOLB, and the compiler's own

**Question.** With IOLB (Olivry et al. 2020) hooked up through `neant emit --scop` and
`neant cost --iolb`, and the compiler deriving its own bounds (`bounds.rs`: the HBL exponent by
an exact LP, and the footprint from cold), what do the three say about the kernels, and was the
hand entry right?

**Setup.** IOLB built from `gitlab.inria.fr/CORSE/iolb` inside its docker image, run under
amd64 emulation on the arm64 host (`tonistiigi/binfmt --install amd64`; the image is x86 only).
`tests/kernels/iolb.sh` copies the export into the checkout and runs `iolb-affine` with a
wall-clock limit. IOLB's bounds are in words with `S` the cache in words, converted with
`S = M/8` and ×8. The native bounds are in bytes over `M` directly.

| function | own moves (leading) | footprint | HBL (native) | IOLB | best gap |
|---|---|---|---|---|---|
| `matmul` naive | `8·n³` (column fits), `B·n³` (not) | `24·n²` | `8·n³/√M − M`, σ = 3/2 | `45.25·n³/√M` | 256×, 2304× |
| `matmul` tiled by hand, `t = 64` | `≈ n³/8` | `24·n²` | `8·n³/√M − M`, σ = 3/2 | `45.25·n³/√M` after untiling, if `64 \| n` | 9× |
| `dot` | `16·n` | `16·n` | σ = 1: none | `16·n` | 1× |
| `saxpy` | `16·n` | `16·n` | σ = 1: none | `16·n` | 1× |
| `sum` | `8·n` | `8·n` | σ = 1: none | `8·n` | 1× |
| `transpose` | `16·n²` (fits), `72·n²` (not) | `16·n²` | σ = 1: none | `8·n²` | 1×, 4× |
| `pairs` (triangular reduction) | `3·n²` … | `8·n` | `16·n²/M − M`, σ = 2 | `32·n²/M` | — |
| `tiles` (`for ii in 0..n/4`) | `8·n` | `8·n` | σ = 1: none | none in 240 s | 1× |

**Findings.**

- **The hand entry's constant was `4·√2 ≈ 5.66×` too small.** `bounds.rs` carried
  Irony–Toledo–Tiskin's `N/(2√2·√S)` words; IOLB derives Smith–van de Geijn's `2·N/√S`. Both are
  valid lower bounds and the second is the one to quote. The native HBL bound reproduces the
  Irony–Toledo–Tiskin constant from first principles — `σ = 3/2` out of the LP, `|I| = n³` out
  of the summation — so the hand entry is now derived rather than written, and every matmul gap
  quoted before this is `5.66×` smaller against IOLB's line.
- **On the streaming kernels the bounds meet the model.** `dot`, `saxpy`, `sum` and `tiles` move
  exactly their inputs once; the footprint bound says so, and IOLB agrees. That is the first
  external confirmation that the moves rules are tight on the easy cases, not only that they
  agree with the counters.
- **The native bound sees through the tiled nest; IOLB does not, until the nest is untiled.** On
  the hand-tiled product IOLB first returned only the size of the data, `3·n²` words. The export
  now drops a tile-outer loop whose variable appears only in one inner loop's bounds and runs the
  inner loop over the full range — the same computation when `T | n`, which the bound states —
  and IOLB returns `2·n³/√S` for it. The native bound needed nothing: the six loop variables
  project onto the three arrays with `σ = 3/2` regardless of the nesting.
- **The accumulator had to be seen as the element it is stored into.** `acc += a·b` inside the
  `k` loop references `a` and `b` only, and the LP over those two gives `σ = 2` — `n³/M`, a
  bound weaker than the truth by `√M`. The compiler now reads `c[i·n + j] = acc` after the loop
  as saying that `acc` *is* `c[i][j]` inside the block, and the statement references all three
  arrays. Register promotion does not change the computation, so the bound is the same one.
- **`pairs` was misreported here as a weakness of IOLB.** An earlier version of this section said
  IOLB's bound for the triangular reduction fell below the input size. It was the parser's
  choice: only the second-to-last line of IOLB's output, the asymptotic bound, is read, and its
  last line adds the input size. The footprint bound now states the input size natively (`8·n`),
  and both are printed.
- **Floors in loop bounds blow IOLB's search up.** `for ii in 0..n/4` did not finish in four
  minutes; the same nest without the division returns at once. That is why untiling emits the
  full range with a divisibility assumption rather than `T·(n/T)`, which is exact and does not
  return.
- **Two things the export had to get right for PET to accept the file**, found by feeding it:
  the flat index `a[i*n+k]` crashes GiNaC with a pole error (hence delinearisation), and a
  statement with no effect — the function's tail expression printed as `(void)(s);` — made IOLB
  run for more than ten minutes on a two-line reduction; dropping it, the same file answers in a
  second.

## The tile side — the model's choice against the machine

**Question.** The tile side is now read off the model (cost-model § Rewrites): the tiled product
analysed with its side `T` symbolic, the side the boundary of the fit condition of the cheapest
regime. The model's first answer at `M = 2 MiB` was `T < √(M/8)`, 510 — the regime in which one
tile fits and the other two operands stream, `32·n³/T` — which in the ideal cache exactly ties
the regime in which every tile fits (`16·n³/T` at `T < √(M/32)`, 256), both `90.5·n³/√M`. Is
the tie real?

**Setup.** `tests/kernels/sweep.py matmul_tile_*`: the naive kernel with `--apply matmul:tile=T`
for seven sides, `n` a multiple of 64 with an odd cofactor, `l2d_cache_refill × 64` on core 5.
Predicted bytes are the model's; the ratio is predicted over measured.

| `T` | working set of the tile level | `n = 1600` pred / meas | ratio | `n = 2496` pred / meas | ratio |
|---|---|---|---|---|---|
| 128 | `32·T²` = `M/4` | 8.38e8 / 7.60e8 | 1.10 | 2.74e9 / 2.87e9 | 0.96 |
| **181** | **`M/2`** | 6.86e8 / **5.74e8** | 1.19 | 2.15e9 / **2.24e9** | 0.96 |
| 256 | `M` (the boundary) | 9.40e8 / 7.44e8 | 1.26 | 2.96e9 / 2.79e9 | 1.06 |
| 300 | one tile: `8·T²` = `M/3` | 8.68e8 / 1.14e9 | 0.77 | 2.68e9 / 4.33e9 | 0.62 |
| 361 | `M/2` | 8.02e8 / 2.95e9 | 0.27 | 2.40e9 / 1.34e10 | 0.18 |
| 420 | `2M/3` | 7.59e8 / 6.78e9 | 0.11 | 2.21e9 / 3.07e10 | 0.07 |
| 510 | `M` (the model's first choice) | 7.19e8 / 1.69e10 | 0.04 | 2.02e9 / 6.76e10 | 0.03 |

**Findings.**

- **The tie is not real.** Every side at which all three tiles fit in `M` moved what the model
  said (ratios 0.96–1.26, the M1 constants). Every side in the one-tile regime moved more than
  the model said, by a factor that grows from `1.6×` at 300 to `30×` at 510 — the regime the
  ideal cache computes correctly is one the machine does not have. One tile resident while two
  operands stream is optimal replacement, not LRU; a working set larger than the cache with a
  cyclic access pattern is the case LRU gets nothing from.
- **The best measured side sits at half the cache.** 181 (`32·T² = M/2`) moved the least at both
  sizes: 20% less than 256, which sits at the boundary and whose measured bytes fall between the
  two regimes' predictions. This is the octave M1 saw the fit transition spread over, seen from
  the other side, and it is Sleator–Tarjan's factor: an LRU cache of `M` is as good as the ideal
  cache of `M/2`.
- **The rule changed three times from this table.** A regime that depends on a *partial* fit is
  no longer a candidate at all; the boundary is taken at `M/2`; ties go to the smaller side. The
  choice for the product became `T < 0.1443·√M`, 206 once the edge lines are counted, which a
  further run measured at 6.06e8 and 2.46e9 — `1.02` and `0.78` of its prediction, `19%` fewer
  bytes than the square of 256 the old rule picked, and `6%` more than 181, the best of the seven.
  The `2×` gain the model had claimed for 510 was worth `-30×` on the machine.
- **The model's partial-fit regimes are optimistic for the machine**, and this is now written in
  cost-model § What the model does not see. Nothing in the calculus corrects for it yet except
  the tile choice.

## M4 — the layout kill condition

**Question.** The one thing M4 claims is that the compiler owns the layout of a struct array,
because nothing in the program can hold an address into one. The claim is worth something only if
the choice moves the machine. `sum_x` reads one field of every element of `[Point{x,y,z}; n]`:
the model says `24·n` bytes under AoS (every line of the array is fetched for a third of its
bytes) and `8·n` under SoA (one stream). Does the measured traffic move by that factor?

**Setup.** `tests/kernels/struct_aos.nt.in` and `struct_soa.nt.in`, the same loop with
`#[layout(aos)]` and `#[layout(soa)]`; five repeats of the read over a freshly written array;
`l2d_cache_refill × 64` on core 5. AoS spans `24·n` bytes, so every size past `n = 100_000` is
past the 2 MiB L2.

| `n` | AoS pred / meas | SoA pred / meas | measured AoS ÷ SoA | predicted |
|---|---|---|---|---|
| 200 000 | 3.36e7 / 2.02e7 | 8.00e6 / 2.71e6 | 7.4 | 3 |
| 800 000 | 1.34e8 / 1.08e8 | 5.76e7 / 2.68e7 | 4.0 | 3 |
| 3 200 000 | 5.38e8 / 4.58e8 | 2.30e8 / 1.23e8 | 3.7 | 3 |

**Findings.**

- **The kill condition is not met: the layout moves the machine.** At the two sizes where
  neither layout fits L2 the measured ratio is 3.7–4.0 against a predicted 3.0. The M4 gate
  stands, and with it the one mechanism claim of the README — projections return values, so the
  representation is the compiler's to choose.
- **The measured advantage is larger than the model's, and the reason is the counter.** The AoS
  walk touches every line and its prediction is close (ratios 1.17–1.24); the SoA walk is a pure
  read stream, which M1 already measured as registering at about half (ratios 1.88–2.95, the same
  band as `sum` and `dot`). The counter under-reports exactly the case the layout improves, so
  the real advantage is if anything nearer the modelled 3× than the table's 3.7×.
- **At 200 000 the SoA array fits L2 and the repeats are free**, which is why the ratio is 7.4
  there; that point measures residence, not layout.

## M4 — the region rule against a pointer chase

**Question.** A walk whose index is loaded from memory — `i = nodes[i].next` — costs a fresh line
per step in general, but no more than the arena once the arena is in cache, whatever the order.
The model says so as two pieces. Does the machine follow the piece that applies?

**Setup.** `tests/kernels/arena.nt.in`: a 16-byte `Node`, its `next` links a full-period LCG
permutation of `0..n`, so the chase visits every node in an order the prefetcher cannot guess;
three walks of the whole arena per run. The arena fits the 2 MiB L2 below `n = 131072`.

| `n` | arena | predicted | measured | ratio |
|---|---|---|---|---|
| 16 384 | 256 KiB, fits | 1.31e6 | 1.92e5 | 6.8 |
| 65 536 | 1 MiB, fits | 5.24e6 | 3.31e5 | 15.8 |
| 262 144 | 4 MiB | 5.87e7 | 7.09e7 | 0.83 |
| 1 048 576 | 16 MiB | 2.35e8 | 3.25e8 | 0.72 |
| 4 194 304 | 64 MiB | 9.40e8 | 1.26e9 | 0.75 |

**Findings.**

- **Where the arena does not fit, the model is right within the counter's factors.** The chase
  pays a line per step and the measured traffic is 1.2–1.4× the prediction, with a measured slope
  of 1.04 against a predicted 1.00. Without the region rule this is the only thing the model could
  ever have said, and here it is the true one.
- **Where the arena fits, the model is conservative by the number of walks, and the reason is
  stated in the model.** It charges each call the arena, because a site whose index is loaded from
  memory has no exact range and therefore **claims no residue** — the compiler cannot know that
  the walk touched every node rather than the first one `n` times, and crediting a caller for what
  was only possibly touched would be unsound. The machine pays for the arena once: the build
  writes it, write streams do not register in this counter (M1), and the three walks then read
  what is already resident. The shape the piece predicts — a cost that is the arena and not the
  number of steps — is what the machine shows; the constant is the number of calls.
- **The rule needs the trip count to be comparable with the arena.** `for k in 0..nodes.len()` is
  bounded by the arena and the rule fires; a separate `steps` parameter is not, and the model
  says so rather than guessing, because a condition in this calculus can compare a working set
  with the cache but not two size expressions with each other.

## M5 — does in-place reuse move nothing, and a forced copy move `n`?

**Question.** `ys = xs;` is decided once, from the text, never from a runtime value
(m5-design.md §2): in place when no view of `xs` survives it (predicted `moves 0` for the
statement itself), a copy when one does (predicted `moves 8·n`). Does the machine show the
difference, and does it scale with `n` the way the model says?

**Setup.** `tests/kernels/reassign_inplace.nt.in` and `reassign_copy.nt.in`: each repeat builds a
fresh `xs` and `ys` of `n` `i64`s, reassigns, mutates one element, and sums the whole of `ys`
(`reassign_copy` also reads a view of `xs` taken before the reassignment, which is what forces its
copy) — the sum is there so nothing is dead-code-eliminated; an earlier version that only read
`ys[0]` measured flat, near-zero traffic at every `n`, because gcc could prove the rest of both
arrays was never read and dropped the writes that built them.

| kernel | `n` | predicted | measured | ratio |
|---|---:|---:|---:|---:|
| reassign_inplace ×3 | 800 000 | 5.76e7 | 1.55e7 | 3.72 |
| | 3 200 000 | 2.30e8 | 7.52e7 | 3.06 |
| | 12 800 000 | 9.22e8 | 3.08e8 | 2.99 |
| | *slope* | **1.00** | **1.02** | |
| reassign_copy ×3 | 800 000 | 7.68e7 | 2.82e7 | 2.73 |
| | 3 200 000 | 3.07e8 | 1.01e8 | 3.05 |
| | 12 800 000 | 1.23e9 | 4.11e8 | 2.99 |
| | *slope* | **1.00** | **1.01** | |

`n = 200 000` is dropped from the table (both arrays under 1.6 MiB, near L2's 2 MiB — the same
residency effect noted for `struct_soa` at that size, not the thing under test).

**The copy's own marginal cost**, `reassign_copy` minus `reassign_inplace`, isolates the
reassignment from the build-and-sum cost the two kernels share:

| `n` | predicted delta | measured delta | ratio |
|---:|---:|---:|---:|
| 3 200 000 | 7.68e7 | 2.55e7 | 3.02 |
| 12 800 000 | 3.07e8 | 1.03e8 | 2.99 |

**Findings.**

- **The exit tests pass.** In place, past L2, adds nothing to the measured traffic beyond the
  build and the sum; forced to copy, the machine pays for it, and the extra bytes scale with `n`
  at slope 1.00 — exactly the `8·n` the model predicts, not a fraction of it and not a different
  power. The two kernels' own slopes (1.02, 1.01) track the model's (1.00) as closely as any
  kernel in this file has.
- **The ratio is the same ~3× on both kernels, so it is not about the copy.** 2.99–3.06 at the two
  largest sizes, in place and copied alike, and the copy's own isolated delta lands at the same
  2.99–3.02. This is the M1 read-stream finding (§3 above) plus one more effect these kernels have
  that `sum`'s own sweep does not: `reassign_inplace`/`reassign_copy` `malloc` a fresh pair every
  repeat rather than filling one array once outside the loop, so first touch of every page is a
  kernel zero-fill fault the model has no line for. `sum` alone measures 2.2–2.8×; a pure read
  stream on freshly faulted pages landing at 3.0× is consistent with both effects adding, not a
  new one.
- **The naive kernel is the honest one.** No attempt was made here to net out the shared
  build-and-sum cost or the page-fault effect and report a "clean" ratio nearer 1×; the number
  that matters is that the copy's *marginal* bytes, isolated by subtraction, scale exactly as
  predicted, at the same constant factor the rest of the traffic does.
- **A rough edge the kernels had to route around, found here and fixed the same day.** `let n =
  @N@; let xs = [1; n]; let mut ys = [0; n];` failed to typecheck when this was written — `[e; n]`
  allocated a fresh `Size::Var` for `n` on every use, even the same local `n`, so `xs` and `ys`
  ended up with two different size atoms and `ys = xs` saw them as different types. The kernels
  below use `let n = @N@;` and build `[0; n]` / `[1; n]` directly, which now typechecks: an
  immutable local used as a count is given its size atom once and reused, a mutable one still gets
  a fresh atom at every use (it may have changed between them), and `[e; xs.len()]` takes `xs`'s
  own size rather than minting a new one that merely agrees with it. Fixed in `types.rs`
  (`size_of_local`), goldens `reassign_named_size`, `reassign_len_size`,
  `err_reassign_mutable_size`.

## M5 — does `T ≤ W/P + O(S)` hold, and does it fail the way §6 predicted?

**Question.** `.par()`'s bound has no memory-bandwidth term (m5-span-design.md §6): `P` cores are
assumed to divide `work` freely, with only the reduction's `O(log n)` span left over. A
compute-bound `.par()` chain should scale close to `P`; a memory-bound one — cores sharing one
path to memory — should scale worse, and by how much worse is exactly what the bound cannot see.

**Setup.** `tests/kernels/par_compute.nt.in` (a degree-16 Horner polynomial per element, work
dominating) and `par_memory.nt.in` (`.par().sum()`, moves dominating): `n = 20\,000\,000` `f64`
(160 MiB, past L3), each `.par()` call repeated (40× compute, 150× memory, chosen so `P=1` runs a
second or two) over one array built once. `tests/kernels/par_sweep.py` times the whole binary,
best of 5, `taskset` to `P` of the cores `5,6,7,8,9,15,16,17,18,19` — this machine's whole big
cluster: `/sys/devices/system/cpu/cpu*/regs/identification/midr_el1` reads the same part (Cortex-
X925) on all ten, so unlike the tiled-matmul sweep earlier in this file there is no core-speed
mismatch inside the range for OpenMP's static, equal-iteration-count schedule to trip over.

| `P` | compute speedup | compute efficiency | memory speedup | memory efficiency | predicted speedup (either kernel) |
|---:|---:|---:|---:|---:|---:|
| 1 | 1.00 | 100% | 1.00 | 100% | 1.00 |
| 2 | 1.88 | 94% | 1.94 | 97% | 1.99 |
| 3 | 2.75 | 92% | 2.73 | 91% | 2.98 |
| 4 | 3.44 | 86% | 2.95 | 74% | 3.95 |
| 5 | 4.34 | 87% | 2.98 | 60% | 4.92 |
| 8 | 5.54 | 69% | 4.26 | 53% | 7.88 |
| 10 | 4.57 | 46% | 3.81 | 38% | 9.83 |

Every `P` favours compute over memory, but neither reaches the smooth curve `P = 1..5` on a quiet
machine showed in an earlier pass of this experiment: both efficiencies dip and partly recover
between `P = 6` and `9`, and both drop hard at `P = 10`. This run shared the machine with another
session doing its own CPU-bound work at the same time — real contention, not a property of this
kernel or this core range — so the exact numbers above are noisier than the shape they support;
the "predicted" column is `neant cost --eval` at each `P`, normalised to `P=1`, indistinguishable
between the two kernels because `work/P + span` has nothing in it that could tell them apart.

**Findings.**

- **The predicted failure is the measured one, at every `P`.** Compute's efficiency stays above
  memory's at every point past `P=2` — 86% vs 74% at `P=4`, 69% vs 53% at `P=8`, 46% vs 38% at
  `P=10` — and the gap is there whether the machine is quiet or, as for the `P=6..10` end of this
  run, shared with other load. The model predicts the identical curve for both; the machine never
  does.
- **This is `sum`'s M1 finding at a second layer.** M1 found `sum`/`dot` bandwidth-bound on *one*
  core (experiments.md §M1 §3). This asks the next question — bandwidth-bound against how many
  cores — and the answer is: fewer than the ten big cores this machine has, well before contention
  or heat make everything noisier.
- **The fix `T ≤ W/P + O(S)` needs is the one §6 named, not a smaller version of it.** A second
  term, `moves/BW` for some aggregate bandwidth `BW`, would predict compute's curve (`moves` small,
  the `work/P` term wins the `max`) and predict memory's flattening (`moves/BW` becomes the binding
  term once `P·(bytes/element/iteration)` exceeds what `BW` delivers) — a roofline, not a straight
  line. `BW` itself was not fit here; this experiment answers *whether* the model needs it, not yet
  *what number* goes in it.
- **What this experiment does not settle.** The `P=6..10` numbers were taken under real contention
  from another process on the same cores, which a rerun on a quiet machine should tighten
  considerably (an earlier, quiet `P=1..5` pass on this same pair of kernels showed a cleaner,
  more monotonic version of the same gap). Fitting
  `BW` and re-including the rest of the machine are open, not attempted here.


## The compiler's cost model, on the compiler — and the machine's answer

The self-hosted cost reporter (`compiler/costdump.nt`, compiled by the self-hosted compiler) was
pointed at `compiler/*.nt` itself. It reports `work` for 31 of the 91 functions and `moves` for 28,
in **20 ms**, and one line stood out:

```
emit_bytes    work 8·s.len() + 1    moves B·s.len() + s.len() + 2·B
```

A whole cache line *per byte copied*. The cause is visible in the source:

```neant
fn emit_bytes(out: &mut [u8], est: &mut [i64], s: &[u8]) {
    let mut i = 0;
    while i < s.len() {
        out[est[0]] = s[i];        // the index is a *load*, not the loop variable
        est[0] = est[0] + 1;
        i += 1;
    }
}
```

`est` is the one-element array the language forces mutable state into — there are no globals — so
the write's index is an array read, the model cannot follow it, and the rule for an index it cannot
follow is "a whole line or more" (`analyze.rs`'s `access`, and the same rule in `compiler/cost.nt`).

Rewritten to index by the loop variable, `out[start + i] = s[i]`, the report becomes
`work 5·s.len() + 4`, `moves 2·s.len() + 3·B` — **32× less traffic predicted**.

### What the machine said

100 self-compiles of the compiler's own source (110 KB in, 183 KB of C out), `taskset -c 5`,
`perf stat -r 3`:

| | `l2d_cache_refill` | wall clock |
|---|---|---|
| before | 12,195,570 ±0.29% | 3.003 ±0.039 s |
| after | 12,116,281 ±0.11% | 3.029 ±0.013 s |

**0.65% fewer refills. No wall-clock change** — the second run's "after" was marginally slower, and
an earlier, shorter measurement that appeared to show a 10% win did not survive repetition. It was
noise, and is recorded here because it was believed for about five minutes.

### Why, and what it says about the model

The emitted C is `out_p[nt_idx(est_p[nt_idx(0, …)], …)] = s_p[…]`, and `out_p` is `uint8_t *` —
character types are exempt from strict aliasing, so gcc **must** reload `est_p[0]` after every
store. The cursor really is re-read each iteration. But `est_p[0]` is a single word that never
leaves L1, and `out_p[…]` walked the buffer contiguously whatever the model believed.

So the model's estimate was an over-estimate by 32×, in the direction it deliberately rounds:
every assumption rounds up, and "an index I cannot follow scatters" is sound as a bound and wrong
as a description whenever the index happens to be a cursor. **What the rewrite improved was the
report, not the program.**

The change is kept — a cost report that is true beats one that is merely sound, and the idiom will
hide a cursor from this analysis every time it is used — but it is kept with this measurement
attached, not as a performance fix.

### The loop that produced it

This is the first time in the project that the language's own cost model was applied to its own
compiler, produced a specific claim, and had that claim checked against hardware. The claim was
wrong, the reason is understood, and the reason is a property of the model rather than of the port.
That is the loop working.

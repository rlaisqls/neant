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

## The standard library's declarations, and the builtins', measured

**2026-09-26, Cortex-X925, `taskset -c 5`** (`--cpu 5`; glibc 2.39, gcc 13.3 `-O2`, Linux
6.17). The eight libm externs in `std/math.nt` were declared from the implementations' common
paths, and the five builtins (`arg_count`, `arg`, `read_file`, `file_size`, and `print_bytes`,
decisions §13) from the earlier sweep. `neant measure --fn <name>` builds the driver with and
without the call, runs each under `perf stat -e instructions,l2d_cache_refill` three times, keeps
the fewest instructions, and subtracts; per call below.

**The first finding is about the driver.** Run as it was, every libm function measured **0.0
instructions a call** and was "confirmed": the driver passed `1.0` to an `f64` parameter, and gcc,
which knows `sqrt`, `exp` and the rest, folded `sqrt(1.0)` to a constant — the loop measured
nothing. Stage C's `labs` confirmation (above, "gcc inlines its own `labs`") was the same thing.
An `f64` argument now varies with the repeat loop, `r·0.001 + 0.5` (0.5..100.5 over 100 000
repeats), and the baseline computes the same value in place of the call, so the difference is the
call alone.

| function | declared before | measured / call | declared now | verdict at the new one |
|---|---|---|---|---|
| `sqrt` | 20 | 3.7 | 5 | confirmed |
| `floor` | 5 | 0.9 | 2 | confirmed |
| `ceil` | 5 | 1.0 | 2 | confirmed |
| `exp` | 80 | 42.0 | 50 | confirmed |
| `log` | 80 | 55.7 | 65 | confirmed |
| `sin` | 100 | 119.0 | 140 | confirmed |
| `cos` | 100 | 122.8 | 140 | confirmed |
| `pow` | 200 | 111.0 | 130 | confirmed |

Sizes 1000, 4000, 16000 (they do not enter a scalar function; the per-call numbers agree to the
instruction across them), moves 0.0–0.8 bytes a call, process noise, against a declared 0. `sin`
and `cos` were **under-declared**: 119 and 123 against 100, confirmed only by the tolerance
(×1.5 + 2); they are 140 now, and for a large argument their reduction takes a longer path than
this range reaches. The rest were over-declared by up to 5×; `sqrt`, `floor` and `ceil` are one or
two instructions once inlined.

The builtins, 10 000 calls a size (1000 for `print_bytes`, which writes them to a pipe):

| builtin | declared | measured / call, n = 1000 → 32000 | verdict |
|---|---|---|---|
| `arg_count` | 10 / 0 | 12.3 / 1.8–5.2 bytes | confirmed (tolerance) |
| `arg` | `result.len()` / same | 603 → 10316, ≈ `n/3 + 300` / 4–110 bytes | confirmed |
| `read_file` | `result.len() + path.len() + 2500` / `result.len() + path.len()` | 2264 → 4320 / 12–87 bytes | confirmed |
| `file_size` | `path.len() + 2500` / `path.len()` | 1974–2002 / 3–5 bytes | confirmed |
| `print_bytes` | `s.len() + 60` / `s.len()` | 142 at n = 1, 138 at 10, 147 at 100, 317 at 1000, 948 at 32000 | **exceeded** at n ≤ 10 |

The input builtins are as the earlier sweep left them (cost-model § Program input): `arg_count`
is still 12.3 against 10, inside the tolerance and not raised here. `print_bytes`'s first
constant, 60, was a guess, and a call costs about 140 instructions of `fwrite` and the check before
it; per byte it is well under one (the stdio buffer takes the bytes, the kernel's copy is not
counted), so the declaration is now `s.len() + 200` and confirmed from 1 byte to 32 000. Moves are
confirmed only as not exceeded, as for the input builtins: a write goes through stdio and the
kernel, which the counter and the model do not see.

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


## The roofline: predicted time against wall-clock (2026-09-27)

**Prediction.** `time = max(work·τ, moves/BW)` on one core (cost-model § Time), with `τ` and `BW`
fitted once and then held fixed for every other kernel.

**Setup.** `tests/kernels/roofline.py`, the release compiler, `taskset -c 5` (a Cortex-X925), the
minimum of 3–5 runs, an empty program's time (0.87 ms) subtracted, the machine otherwise idle. Fit:
`horner` at `n = 3000` (24 KB, in L1), 2·10⁴ repeats → **τ = 0.0176 ns/work**; `sum` at
`n = 12.8·10⁶` (100 MB), 20 repeats → **BW = 20.8 GB/s**, where `work·τ` would have been 19 ms of
the 108 measured.

| kernel | n | predicted ms | measured ms | measured/predicted |
|---|---|---|---|---|
| sum | 200 000 / 800 000 / 3.2·10⁶ / 12.8·10⁶ | 0.30 / 6.77 / 27.1 / 108 | 1.73 / 5.99 / 26.6 / 110 | 5.80 / 0.89 / 0.98 / 1.02 |
| dot | 200 000 … 12.8·10⁶ | 3.38 … 217 | 2.17 … 150 | 0.64 – 0.71 |
| saxpy | 200 000 … 12.8·10⁶ | 3.31 … 212 | 1.99 … 204 | 0.60 – 0.97 |
| horner | 1000 / 30 000 / 3·10⁶ | 21.1 / 21.1 / 25.4 | 21.0 / 21.2 / 32.5 | 1.00 / 1.00 / 1.28 |
| matmul_tiled | 448 / 832 / 1216 | 16.2 / 103 / 322 | 14.3 / 92.9 / 288 | 0.89 / 0.90 / 0.89 |
| matmul_naive | 448 / 832 / 1216 | 15.9 / 223 / 696 | 25.2 / 208 / 802 | 1.59 / 0.93 / 1.15 |
| transpose | 1000 / 2000 / 3000 | 5.02 / 20.0 / 45.1 | 9.70 / 44.0 / 131 | 1.93 / 2.19 / 2.91 |
| struct_aos | 200 000 / 3.2·10⁶ | 1.61 / 25.9 | 2.21 / 44.4 | 1.37 / 1.72 |
| struct_soa | 200 000 / 3.2·10⁶ | 0.38 / 11.1 | 2.11 / 35.0 | 5.49 / 3.16 |
| arena | 65 536 / 1 048 576 / 4 194 304 | 0.25 / 11.3 / 45.2 | 1.26 / 207 / 1410 | 4.98 / 18.3 / 31.2 |

Over the 31 runs of every kernel but `sum`, the geometric mean of measured over predicted is 1.56,
range 0.60 to 31.

**Where it holds.** Where one term clearly dominates and the access is a stream or a tiled block,
two constants predict wall-clock within about 30% across two orders of magnitude of size:
`sum` past `M` (0.89–1.02), `matmul_tiled` at every size (0.89–0.90), `horner` in cache (1.00),
`matmul_naive` past `M` (0.93–1.15), `saxpy` at the large end (0.87–0.97). That is the claim the
roofline term was built to test, and on these kernels it holds.

**Where it does not, and why — each a term the model lacks, not a constant off.**
- **Latency.** `arena` is a pointer chase: one dependent miss at a time. The model charges its
  lines at the streaming rate, and the ratio grows with the arena, 5 to 31. A latency term — lines
  that cannot overlap, times the miss latency — is what `max(work, moves/BW)` has no room for.
- **One cache level.** Data that fits in `M` (2 MiB) moves nothing in the model, so `sum` at
  200 000 and both `struct` kernels at 200 000 are predicted as pure work, while L2 bandwidth and
  the page faults of a fresh allocation are real: 5–6× at the small end, gone by 800 000.
- **Strides and the TLB.** `transpose` reads a column: its lines are counted right (M1), but each
  touches a new page past a few thousand columns, and the ratio climbs 1.9 → 2.9 with `n`.
  `struct_soa` at 3.2·10⁶ (3.2) and `matmul_naive` at 448 (1.6) are the same class.
- **Two streams outrun one.** `dot` and `saxpy` read two or three arrays and are measured faster than
  predicted (0.60–0.71 at the small end): the fitted `BW` is one stream's, and the prefetchers run
  several at once. A bandwidth that depends on the number of streams is the refinement.

**The latency term, added the same day** (cost-model § Time, latency). A chase's lines are counted
apart and charged `L` each instead of `B/BW`, `L` fitted on `arena` at 4 194 304 nodes (64 MB):
**L = 112 ns**, `τ` and `BW` refitting to 0.0177 ns and 20.8 GB/s. With the three constants held
fixed, `arena` goes from 4.98 / 18.3 / 31.2 to **0.28 / 0.58 / 1.00** at 65 536 / 1 048 576 /
4 194 304 nodes, every other kernel is where it was (no other kernel chases), and the geometric mean
of measured over predicted over the 31 runs goes from 1.56 to **1.14**. What is left of the `arena`
row is the missing cache levels again: at 16 MB the arena fits in L3 and waits for L3, and at 1 MB it
fits in L2.

**A second level, tried the same day, and not made the default.** `--M3 16777216` re-runs the
analysis with `M` = L3 and charges what crosses L2 but not L3 at L3's rates, fitted by differences
(100 more repeats of a 6.4 MB stream: **BW₂ = 30.1 GB/s**; a second walk of an 8 MB arena: **L₃ =
23 ns** a line). A first fit that read the model's own first touch gave a negative `L₃`: the arena
had just been written by `main` and was in L3 already, which the model, charging each call's first
touch cold, does not know. With it on, mid-size streams move toward 1 (`dot` at 800 000: 0.64 →
0.97) and strided `matmul_naive` moves away (at 832 / 1216: 0.94 / 1.16 → 1.36 / 1.69, L3 serving a
column walk slower than a stream), and the geometric mean goes from 1.14 to 1.16. A level whose
bandwidth depends on the access pattern is what that asks for, which is the TLB row's question
too.

**Pages, tried the same day, and not made the default.** An access that moves a page or more a lap
of its innermost loop has each of its lines counted as `paged`, charged `--tlb` nanoseconds for the
TLB walk; fitted on `transpose` at 2000 it is **9.0 ns** a line. `transpose` then comes to 0.85 /
1.06 / 1.28 (from 1.9–3.0), but `matmul_naive` past the cache goes to 0.24 / 0.30 (from 0.94 /
1.16): its column walk reuses its 832 pages column after column, within TLB reach, and each of its
lines is a miss to memory the walk overlaps with. The transpose's lines are hits, reused while the
column slides, and it is there the walk shows. A per-line charge cannot tell the two apart; it is
off (`--tlb 0`), and the question it leaves is which lines are hits — the same reuse the model
already computes for moves, asked of pages.

**The M5 question answered, 2026-09-28** (cost-model § Time, cores). With `BW(P) = min(P·BW, BW_max)`
and `BW_max = 65.6 GB/s` fitted on `par_memory` at P = 10, the M5 sweep (`par_sweep.py`, quiet
machine) against the prediction, measured / predicted:

| kernel | P = 1 | 2 | 4 | 10 |
|---|---|---|---|---|
| `par_memory` (a `.par().sum()`) | 1.45 | 1.49 | 1.44 | 0.99 (the fit) |
| `par_compute` (a degree-16 map, summed) | 2.73 | 2.87 | 3.12 | 2.01 |

The shapes are the machine's: the memory-bound chain saturates past four cores (speedup 4.6 at ten,
predicted 3.2 over P = 1) and the compute-bound one keeps scaling (6.8 at ten). The constants are
off by a steady factor each — one OpenMP thread streams at 14 GB/s, not the 20.8 a plain loop does,
and the map's 16-deep chain per element inside a reduction runs 2.7–3× slower than the polynomial
`τ` was fitted on. What M5 said the model could not do, tell the two apart, it now does.

**What changed.** The `--eval` line prints the time and its bound (`work-bound` / `moves-bound`);
`--tau` and `--bw` set the constants, defaulting to the fit above. Nothing in any report without
`--eval` changed, and no golden moved. Next: the same comparison on the corpus's programs, which
read their input and mix both regimes.

## The corpus against the clock, before any fix (2026-09-27)

**Prediction.** Each corpus program's `main`, `time = max(work·τ, moves/BW)` with the constants the
kernels fitted (τ = 0.0176 ns, BW = 20.8 GB/s), at three generated input sizes; measured wall-clock
on CPU 5, minimum of three runs, an empty program subtracted (`tests/corpus/timing.py`). `bfs` is
out: its cost is in `max(start[_])`, a value read from the graph, which `--eval` cannot be given.

| program | size | bound | predicted ms | measured ms | measured/predicted |
|---|---|---|---|---|---|
| matmul | n = 100 / 300 / 600 | work / work / moves | 0.18 / 4.81 / 84.2 | 0.25 / 12.5 / 113 | 1.37 / 2.60 / 1.34 |
| heat | n = 100 / 300 / 900, 50 steps | moves | 2.41 / 21.2 / 189 | 0.02 / 2.40 / 24.2 | 0.01 / 0.11 / 0.13 |
| fir | 10⁴ / 10⁵ / 10⁶ integers | moves | 145 / 1.45·10⁴ / 4.7·10⁷ | 0.02 / 2.05 / 23.0 | ≈ 10⁻⁴ … 10⁻⁶ |
| pid | 10⁴ / 10⁵ / 10⁶ integers | moves | 183 / 1.8·10⁴ / 5.9·10⁷ | 0.30 / 2.31 / 21.4 | ≈ 10⁻³ … 10⁻⁶ |
| csv | 10³ / 10⁴ / 10⁵ rows | work | 16.8 / 2024 / 2.4·10⁵ | ≈ 0 / 0.02 / 2.23 | ≈ 10⁻⁵ |

**What it says.** Where the program is a kernel, the roofline carries over: `matmul` is within
1.3–2.6×. Everywhere the program reads text, the prediction is a true upper bound and useless as a
time: 10³ to 10⁶ too high, and growing with the input, because its *moves* are quadratic. Two
causes, both in how a scan is charged, neither in the roofline:
- **A scan's accesses are charged a line each.** `xs[i]` with `i` a scan's index is not affine, so
  § Moves charges it `B` bytes per access — 64 bytes for one byte of text — and never as the stream
  it is: `i` only grows, so the lines are visited in order, each once.
- **A scan's footprint is the whole array.** `next_int(xs, start)` claims `xs: [0, xs.len())`, not
  `[start, xs.len())`, so while the text fits in memory each call is charged a cold read of all of
  it. Its moves then have no distance form, the amortised scan leaves them per lap, and a loop of
  calls reads the text once per integer.

`csv` is quadratic in work as well: its row loop steps by what `after_header`-style parsing returns
through a local the amortised scan does not follow. `heat` is 8× too high in moves: the five reads
of the stencil are counted as more streams than the rows they share. These are the next changes;
each is a place the bound is loose, measured, not a constant to tune.

## The corpus against the clock, after a scan's accesses (2026-09-27)

Same harness, constants and machine as the table before; the calculus now charges a scan's accesses
as a stream and amortises a chain of parser calls (cost-model § A scan's accesses, § An amortised
scan).

| program | size | bound | predicted ms | measured ms | measured/predicted |
|---|---|---|---|---|---|
| fir | 10⁴ / 10⁵ / 10⁶ integers | moves | 0.52 / 5.20 / 51.9 | 0.26 / 2.19 / 21.5 | 0.49 / 0.42 / 0.41 |
| pid | 10⁴ / 10⁵ / 10⁶ integers | moves | 0.53 / 5.34 / 53.4 | 0.29 / 2.48 / 22.7 | 0.53 / 0.46 / 0.43 |
| csv | 10³ / 10⁴ / 10⁵ rows | moves | 0.13 / 1.41 / 15.4 | 0.03 / 0.15 / 2.58 | 0.22 / 0.10 / 0.17 |
| matmul | n = 100 / 300 / 600 | work / work / moves | 0.18 / 4.81 / 84.2 | 0.44 / 12.8 / 114 | 2.43 / 2.66 / 1.36 |
| heat | n = 100 / 300 / 900, 50 steps | moves | 2.41 / 21.2 / 189 | 0.33 / 2.65 / 23.9 | 0.14 / 0.13 / 0.13 |

**Every program is now within 0.10 to 2.7 of its measured time**, where the text readers were off
by up to 10⁶; the geometric mean of measured over predicted is 0.41 — the prediction is an upper
bound and errs high. What remains is the same short list, each a place the bound is loose and why:
- `fir`, `pid`: 2.4× high. A call to `next_int` is charged `3·B` of partly used lines at its ends,
  which consecutive calls share in fact.
- `csv`: 5–10× high, the same `3·B` a call, three calls a row, on rows of a dozen bytes.
- `heat`: 8× high in moves, of which 2× was a plain overcount, fixed the same day: an `if` whose two
  branches each call `sweep` added both calls' moves, where work took the larger branch. With the
  larger taken (when it is cheap to know — the same cost, or one dominating regime by regime; the
  sum otherwise, which a plain `max` of piecewise costs had turned into a regime blow-up on the
  compiler), `heat` is **0.21 / 0.26** at n = 300 / 900, five of the compiler's lock lines drop and its
  count does not move. The other 3× was the stencil's five reads counted as five streams; with
  neighbouring sites grouped (cost-model § Neighbouring sites) it is three, `24·n²` a sweep against
  a lower bound of `16·n²`, and `heat` is **0.48 / 0.51** at n = 300 / 900. Measured refills × 64
  stay below the prediction (0.05 and 0.34 of it), as a bound's should.
- `matmul`: 1.4–2.7× low where it is work-bound at small `n`: its inner loop's `work·τ` uses a `τ`
  fitted on a vectorised polynomial, and a strided dot product vectorises worse.

## The corpus against the clock, a parse's ends and its serial work (2026-09-28)

Same harness, constants and machine. Two changes to the calculus, each found by the one before it.

**The ends of a call's stretch, once.** An amortised call's moves are its distance, charged once
for the loop, and a constant — `next_int`'s `3·B`, the partly used lines at the two ends of the
stretch it reads. The next call starts where this one stopped, so the lines are the chain's, and the
loop now pays them once where the callee touches the scanned array alone and has none of its own
(cost-model § An amortised scan). And the region rule, which lets a data-indexed site on an array
that fits cost at most that array (`csv`'s `count[s.v]`, eight elements), had not applied: whether
the loop is longer than the array was decided by a test that compared coefficients as written, and
`text.len()/3 − 1/3 ≥ 1` failed it. It asks now whether every term subtracted is outgrown by one
added. `csv`'s `main` moves `4·text.len()` bytes, from `B·text.len() + 4·text.len()`; `count_ints`
`3·B` in all, from `3·B` a number.

With only that, the parsers went from 2–10× high to 2–2.8× **low** — `fir` 2.0–2.4, `pid` 2.7–4.5,
`csv` 2.0–2.2: the overcharged moves had hidden a compute term. The number loop, `v = v·10 + d`, is
serial work (cost-model § Time, serial work), and `next_int` alone has it — 7 units a digit — but
an amortised call's serial work is in the distance it moves, which the call cannot name, and it was
dropped. Serial work and divisions are now amortised as work is: a lap pays what is not distance,
the loop the distance at the chain's largest rate, once.

| program | size | bound | predicted ms | measured ms | measured/predicted |
|---|---|---|---|---|---|
| fir | 10⁴ / 10⁵ / 10⁶ integers | work | 0.11 / 1.14 / 11.4 | 0.22 / 2.37 / 23.3 | 1.93 / 2.08 / 2.04 |
| pid | 10⁴ / 10⁵ / 10⁶ integers | work | 0.14 / 1.38 / 13.8 | 0.17 / 2.11 / 21.4 | 1.27 / 1.53 / 1.56 |
| csv | 10⁴ / 10⁵ rows | work | 0.21 / 2.24 | 0.17 / 2.13 | 0.81 / 0.95 |
| matmul | n = 100 / 300 / 600 | work / work / moves | 0.18 / 4.81 / 84.2 | 0.47 / 12.8 / 113 | 2.58 / 2.66 / 1.35 |
| heat | n = 100 / 300 / 900, 50 steps | moves | 0.65 / 5.47 / 48.2 | 0.20 / 2.46 / 24.4 | 0.31 / 0.45 / 0.51 |

**The programs are within 0.31 to 2.7** — the parsers from 0.10–0.53 to 0.81–2.1 — geometric mean
1.26 over the runs past the harness's noise (`csv` at 10³ rows is shorter than a process's start).
The error changed side: the prediction is no longer an upper bound on these programs. `fir`'s
remaining 2× is its filter loop, compute the model charges at `τ`.

## A carried add in a short lap (2026-09-28)

`sum` in L1 (3000 elements, 2·10⁵ repeats) measured 0.150 s where the model said 0.042: 0.25 ns a
lap, the latency of the `f64` add the lap carries, against four units of work at `τ`. Counting a
carried `f64` add as a unit of serial work in every lap brought it to 1.2× and spectral-norm from
1.26 to 0.96, but took `horner` (eight multiplies a lap beside the add) to 0.70 and the tiled
`matmul` (a 64-long chain, then the next `j`'s) to 0.51 — the core overlaps an add's wait with the
lap's other work, and with the next chain. Counted only where the lap's work is less than a unit
of serial work's, `work·τ < τ_s`: the kernels' geometric mean is 1.09 (31 runs, 0.27–6.4; `horner`
0.99–1.26, tiled `matmul` 0.89–0.95, `sum` 0.88–2.1), the Benchmarks Game's 1.07 (0.50–1.79;
spectral-norm back at 1.26), the corpus's 1.25. The check's sizes are all past L1, so the tables
barely move; the kernel it fixes is the one the fit's sizes skip. `roofline.py` now reads a
prediction line whatever time terms it carries.

## Bandwidth by the number of streams (2026-09-28, measured, not yet in the model)

`BW` is one constant, fitted on `sum`'s one stream (20.8 GB/s with the setup's first touch of the
pages charged). `dot` (two read streams) runs at 0.60–0.71 of the prediction and `saxpy` at 0.57–0.98,
so a loop of `k` i64 read streams over 6.4·10⁶ elements was timed on CPU 5, 40 repeats, its setup's
time subtracted:

| streams | 1 | 2 | 3 | 4 | 6 |
|---|---|---|---|---|---|
| GB/s | 30.1 | 55.8 | 60.1 | 62.8 | 59.1 |

One core's bandwidth is `min(k·30, ≈60)` GB/s: a second stream doubles it, and two streams nearly
saturate the memory — the ten cores' aggregate is 65.6 (§ Cores). An `f64` sum and an `i64` sum
stream at the same 27 GB/s, so the one-stream limit is the memory's and not the add's. A copy (one
read, one write stream) moves 70 GB/s counting its write-back, 47 without. So the model's `moves/BW`
is right for one stream and a factor of about two too slow for two or more — `dot`'s 0.7 — and it
does not count a written line's write-back, which `saxpy`'s 0.98 at size hides by the two errors
cancelling. The fix is per loop, `moves/min(k·BW₁, BW_core)` with write-backs counted, which moves
`BW` itself and every prediction; it is left for a pass of its own.

Tried the same day, alone: the bytes of loops with two streams or more charged at `min(2·BW,
BW_max)`. `dot` went from 0.60–0.71 to 1.16–1.46 and `saxpy` from 0.57–0.98 to 0.87–1.79, the
kernels' mean from 1.09 to 1.24 — `dot`'s lap carries `s += a·b`, a multiply-add whose latency, not
the memory, sets its pace (0.54 ns a lap, where two independent `i64` streams run 0.29), and
`saxpy`'s write-backs are still not counted. Each of the three terms — streams, a carried
multiply-add's latency, write-backs — was covering for the others, and one of them alone makes the
model worse; they go in together or not at all. Reverted.

Then streams and write-backs together (cost-model § Time, streams and write-backs), the carried
add already charged in a short lap. The first run took n-body from 1.77 to 0.93 for the wrong
reason — 72 MB of write-backs of five bodies that never leave the cache, charged a call at a time
though the calls' moves were credited as resident — and a call whose lines the caller holds now
takes none back. With that, over the kernels' 31 runs, the root-mean-square of `ln(measured /
predicted)` goes from 0.605 to 0.526:

| kernel | before | after |
|---|---|---|
| `sum` | 0.88 – 2.09 | 0.87 – 1.75 |
| `dot` | 0.53 – 0.73 | 1.31 – 1.41 |
| `saxpy` | 0.61 – 0.96 | 0.71 – 1.25 |
| `horner` | 0.99 – 1.29 | 1.00 – 1.21 |
| `transpose` | 1.85 – 3.05 | 1.25 – 2.19 |
| naive / tiled `matmul` | 0.94 – 1.52 / 0.89 – 0.93 | 0.94 – 1.61 / 0.89 – 0.93 |
| `struct_aos` / `struct_soa` | 1.40 – 1.68 / 2.97 – 6.36 | 1.46 – 1.53 / 2.79 – 4.88 |
| `arena` | 0.27 – 1.00 | 0.22 – 0.99 |

`dot` changes side, 1.3–1.4 now: it is paced by its multiply-add's latency, 0.54 ns a lap past the
cache against two independent streams' 0.29. The Benchmarks Game's programs are at a geometric mean
of 1.00 (0.47–1.81; n-body 1.75–1.81, spectral-norm 0.96–1.26, mandelbrot 0.47–0.50), the corpus's
1.33 (`heat` 0.63–0.75 from 0.45–0.51, the rest as before).

## Where `fir`'s time goes (2026-09-28)

`fir` at 10⁶ integers, 22.5 ms: reading and parsing 17.4 of it, the filter 5.0 (5 ns an element).
The parse alone is predicted 11.9 ms, 1.46× fast; the filter about 1.4. The parse's gap is about
what a mispredicted branch at the end of each number's digit loop costs, twice a number (counting,
then reading) — the integers are uniform in −100…100, one to three digits and a sign, so the exit
is not predictable. `csv`'s numbers are mostly of one length (a sensor id, a time growing a digit
at a time) and its parse is predicted at 0.9. A branch that data decides is not in the calculus,
and a per-exit charge would make `csv` wrong to make `fir` right; left as the reason for `fir`'s 2×.

## M7's first probe: llvm-mca against the kernels' laps (2026-09-28)

The calculus's compute term is `work·τ`, and three more constants (`τ_s`, `τ_div`, the short
lap's carried add) each patch one way a lap is not its work. M7 proposes a per-block cost line from
a machine model instead. `tests/kernels/mca.py` finds the innermost loops of `gcc -O2 -S` output and
runs each through llvm-mca 18 (`cortex-x4`, the nearest model to the X925 it has); each kernel was
timed in L1, a lap's nanoseconds its time over its laps, the empty repeat subtracted:

| kernel | measured ns a lap | llvm-mca cycles an iteration | ns an mca cycle |
|---|---|---|---|
| `sum` | 0.254 | 2.01 | 0.126 |
| `dot` | 0.254 | 2.01 | 0.126 |
| `horner` | 0.350 | 3.02 | 0.116 |
| `logistic` | 0.746 | 6.00 | 0.124 |
| `divide` | 0.254 | 2.03 | 0.125 |

One constant, 0.125 ns an mca cycle, fits the five — but it is not the machine's. Dependency chains
written in C (`tests/kernels/chains.c`, 10⁸ laps, timed inside the program) put llvm-mca's cycles
against this core's directly:

| chain | measured ns a lap | llvm-mca cycles | ns a cycle |
|---|---|---|---|
| `f64` add | 0.514 | 2 | 0.257 |
| `f64` multiply | 0.771 | 3 | 0.257 |
| multiply-add | 1.028 | 4 | 0.257 |
| `f64` divide | 3.341 | 15 | 0.223 |
| `i64` multiply-add | 0.770 | 3 | 0.257 |

`cortex-x4`'s latencies are this core's, one cycle 0.257 ns (3.9 GHz), the divide two cycles
shorter here. The kernels' 0.125 is half of it because gcc runs two of a kernel's repeats in one
vector iteration — `sum`'s loop loads `xs[i]` into both lanes of a `dup` and adds both, two repeats'
sums at once — so a lap measured is half an iteration. That reaches past this probe: `τ`, `τ_s` and
`τ_div` were fitted on the same kernels, their repeats paired the same way, so they are a lap of
work as gcc compiles those kernels, not one chain's latency; a program whose chains gcc cannot pair
runs at up to twice what they say. A threat to the constants' meaning, which the programs' ratios
(0.5–2.6) are consistent with and do not separate from the model's other errors.

**Refitted on repeats gcc cannot pair** (each repeat starting from the last one's result): τ 0.0176 →
0.0291 ns, `τ_s` 0.155 → 0.310, `τ_div` 0.148 → 0.345 — a chain's own latency, twice what the
paired kernels gave, as the probe said. With those constants the programs improve and the kernels
do not. The Benchmarks Game and corpus runs (16, mandelbrot left out, whose 50 laps are the most a
point takes and whose ratio is a bound's): root-mean-square of `ln(measured / predicted)` 0.48 → 0.27 —
n-body 1.76 → 0.94, `fir` 2.0 → 1.12, small `matmul` 2.5 → 0.84, `pid` 1.5 → 0.87, spectral-norm 1.26
→ 0.68, `csv` 0.9 → 0.6. The kernels, their repeats unpaired the same way: 0.53 → 0.72, the tiled
`matmul` 0.9 → 0.28 (gcc vectorises its inner loop and the core overlaps its short chains, so a
chain's `τ` overcharges it) and `sum` 1.5 at every size, its unpaired add chain slower than the
memory even at 100 MB. Pooled, 0.51 → 0.60. One `τ` cannot be a chain's latency and a vectorised
loop's throughput at once; which calibration is right depends on which the code is, and that is
the per-nest cost line M7 is for. The defaults stay as fitted; the unpaired set is
`--tau 0.0291 --taus 0.3102 --tdiv 0.3447`.

**Step 1 of the slice, and the programs.** `neant emit --lines` puts `#line L "file"` before each
loop, `neant cost --eval … --laps` gives each loop's laps at those sizes, and `tests/kernels/m7.py`
puts them together: each innermost assembly loop is the loop of the calculus at the line its
branch back names, and its compute is laps × llvm-mca's cycles an iteration × 0.257 ns.

| program | measured / llvm-mca's | measured / the calculus's |
|---|---|---|
| spectral-norm, n = 2000 | 0.53 | 1.28 |
| n-body, 10⁶ steps | 0.65 | 1.81 |
| mandelbrot, n = 800 | 0.17 | 0.53 |

llvm-mca errs as far the other way. Spectral-norm's inner loop — `fmadd d1, d2, d0, d1` a lap,
the sum `d1` its addend — is 4.03 cycles to llvm-mca and 2.1 on the core: Arm cores forward an
accumulator late, so a chain through the addend of a multiply-add waits two cycles, where a chain
through a multiplicand waits four (`chains.c`'s 1.03 ns). llvm-mca 18's `cortex-x4` model charges
four either way. (A first reading here took the loop for a two-lane one and the agreement for 4%;
the loop is scalar, and the agreement was the wrong factor of two.) Mandelbrot's laps are the
calculus's upper bound, fifty a point, which is not what the loop runs. So the probe's latencies are
right for the chains `chains.c` has and wrong for the one programs have most, and a cost line on
llvm-mca needs accumulator forwarding before it is better than the calculus on programs.

Writing each multiply-add into its own addend as a multiply and an add before llvm-mca sees it
(`m7.py --forward`) puts the chain on the add: spectral-norm 0.53 → 0.71 of llvm-mca's time, and
n-body 0.65 → 0.56 — its multiply-adds into their addends are mostly not carried from lap to lap,
and split they only cost issue slots. Rewriting only those whose addend nothing earlier in the lap
writes — carried from the last lap — keeps spectral-norm at 0.70 and n-body at 0.65. The rest of
the gap is the core against the model it is not (`cortex-x4`), on work that is not a chain; llvm-mca
on a model that is not this core's is not yet better than the calculus's four constants on programs,
and M7 waits on a core model, not on the map, which works.

**A model of this core.** `tp.c`'s eight independent chains of each operation measure what the
`cortex-x4` model does not have: `f64` adds four a cycle (the model allows two), a multiply-add
through its addend two cycles, a division thirteen. `m7.py --own` replaces llvm-mca with that
table and a lap that is the longer of its carried chain (dependences only, sixty laps simulated)
and its pipes' share (four floating-point, three loads, two stores, six integer, eight in all):

| program | llvm-mca | this core's model | + an entry's critical path | the calculus |
|---|---|---|---|---|
| spectral-norm | 0.53 | **1.06** | 1.06 | 1.28 |
| n-body | 0.65 | 2.34 | 0.45 | 1.81 |

Spectral-norm's loop is long, its laps overlap, and the model has it. N-body's inner loop is
entered 5·10⁶ times for 10⁷ laps, two a time: a lap's own critical path, a square root and a
division, is fifty cycles from a cold start and 4.5 in the steady state, and the core runs it in
12.8 — part of each entry overlapped with the last, by as much as its window and the memory chain
through the bodies' velocities allow. Neither bound is the time; the overlap between entries is the
next term, and it is the one the out-of-order window sets.

**The overlap, and the exits.** Two more measurements on the core: the divider takes one division or
square root a cycle (sixteen chains, `dv.c`; the eight of `tp.c` had been latency-bound), a square
root fourteen cycles; and a loop exit the predictor misses costs 13.5 cycles an entry (`br.c`, trips
random in 0…4 against a constant 2). `m7.py --own --window` lays a short loop's laps (at most
sixteen an entry) out inside one iteration of the loop around it and simulates forty such
iterations — eight dispatched a cycle, none before the one six hundred earlier has finished, each
on its pipe — and the calculus now says which loops' trips vary from one entry to the next (the
trip moves with an outer loop, is read from memory, or is a scan's), each such entry charged a
missed exit. And the assembly read is the unchecked one, as the binary timed is. Every constant
here is a measurement of the core; none is fitted on a program:

| program | this core's model | the calculus |
|---|---|---|
| spectral-norm, n = 2000 | **1.07** | 1.28 |
| n-body, 10⁶ steps | **0.95** | 1.80 |
| tiled `matmul`, n = 300 / 600 | **0.96 / 1.02** | 2.80 / 1.35 |
| `heat`, n = 300 | 0.82 | 0.92 |

The root-mean-square of `ln(measured / predicted)` over these five is **0.10**, where the
calculus's four fitted constants give 0.56. Two programs are outside it: mandelbrot (0.41), whose
laps are the calculus's bound of fifty a point, and `fir` (2.03), whose parse loops are amortised
and have no laps the calculus can give — a scan's bound is the text's length, not the numbers'.
The map works, the core model is fifteen numbers, and the calculus supplies what llvm-mca cannot:
how many times each loop runs, how often it is entered, and whether its trip varies.

**In the harnesses.** `tests/bench/timing.py` and `tests/corpus/timing.py` print `neant cost --m7`'s
time beside the calculus's. The Benchmarks Game: n-body 0.93–0.94, spectral-norm 1.01–1.06 (the
calculus 1.75–1.78 and 1.21–1.26); mandelbrot 0.33–0.39 against 0.42–0.50, its laps the bound. The
corpus: `matmul` 0.90–1.30 (the calculus 1.34–3.74), `heat` 0.64–0.82 (0.73–0.84); and the parsers
fall apart — `fir` 2.0–4.1, `pid` 2.2–3.5, `csv` 7.7–22 — because the laps of an amortised parse are
not the calculus's to give, so M7 counts the loops it can and misses the one that does the work.
Where every loop's laps are exact, M7 is the better time; where one's are a bound or amortised
away, the calculus is — and M7 now says so: `m7 ≤ T` for mandelbrot, whose escape loop has a
condition of two parts, and "does not apply" for `fir`, `pid` and `csv`, whose parse's laps were
left out; the harnesses keep both out of M7's mean.

On the kernels (`m7.py` on each at one check size, pinned to CPU 9): where compute binds it agrees
with the calculus or does better — `divide` 1.04 (0.68), `logistic` 1.04 (1.04), `horner` 1.14 (1.04);
where memory binds it is the calculus's memory term and says the same (`sum` 0.94, `dot` 1.30,
`saxpy` 1.09, `transpose` 1.46, the struct kernels 1.49 and 2.87); and the naive `matmul` at
n = 448 was worse, 2.25 against 1.65. An L2 hit's latency on a strided load was the first guess —
measured (`l2.c`, a dependent load
a step through a random cycle of lines): 4 cycles to 64 KB, the L1; 5–7 to 512 KB; 15–20 at 1–2 MB.
It was not that: a column of the 448 matrix is 28 KB and stays in L1. gcc runs the loop two `j`s at a
time, `fmla v0.2d, v2.2d, v1.2d` a lap, and the model read `v0` as written only, not as the addend it
accumulates — no chain, 0.5 cycles a lap. Read as both, the chain is the addend's two cycles for two
laps, and the naive `matmul` is **1.09** (the calculus 1.60); the programs' ratios do not move
(spectral-norm 1.06, n-body 0.96, `matmul` 0.99 / 1.02, `heat` 0.85).

It does not survive a loop nest. The tiled `matmul`'s inner loop, a 64-long `acc += a·b` chain,
is 6 cycles an iteration to llvm-mca and measures 0.165 ns (0.027 an mca cycle); the naive one 4.0
and 0.276 (0.069). llvm-mca runs one loop in its steady state, and the core overlaps a short chain
with the next outer iteration's — the effect the short-lap rule had to allow for. A cost line for
M7 would be per nest, not per innermost block: the inner loop's latency against the outer
iterations' independence, which is what the calculus's affine forms already know — and it
would need the compiler's own vector width and repeat pairing, which only the emitted assembly shows.

## A second core: the Cortex-A725 (2026-09-29)

The machine has ten Cortex-X925 cores and ten Cortex-A725 (CPUs 0–4 and 10–14, 2.8 GHz), a smaller
design on the same memory. `chains.c`, `tp.c`, `dv.c`, `br.c` and `l2.c` on CPU 0 give the A725 the
X925's latencies — an add two cycles, a multiply three, a multiply-add four through a multiplicand
and two through its addend, a division thirteen, a square root fourteen, an L1 hit four — at a
0.357 ns cycle, with half the floating-point pipes (two a cycle), the divider one a cycle, and a
missed exit 12.7 cycles. The dispatch width, the integer, load and store pipes and the window are
Arm's published figures (`m7::A725`; `neant cost --m7 --core a725`). The calculus refitted there
(`roofline.py fit --cpu 0`): `τ` 0.0604 ns, `BW` 17.0 GB/s, `L` 136 ns, `τ_s` 0.213, `τ_div` 0.062.
Five programs on CPU 0 (`tests/kernels/a725.py`):

| program | calculus, the X925's constants | calculus, refitted | M7, the X925's model | M7, the A725's |
|---|---|---|---|---|
| spectral-norm, n = 2000 | 2.15 | 0.90 | 1.80 | **1.12** |
| n-body, 10⁶ steps | 4.49 | 1.86 | 2.37 | **1.05** |
| `matmul`, n = 300 / 600 | 4.42 / 2.17 | 1.29 / 1.39 | 1.52 / 1.64 | **1.09 / 1.18** |
| `heat`, n = 300 | 2.15 | 0.95 | 1.93 | 0.70 |

`ld.c` then checked the figures taken from Arm: a lap of 27 instructions with eight independent L1
loads runs at eight instructions a cycle on the X925 and five on the A725 — the dispatch widths the
model has — and twelve independent integer pairs at four a cycle on the A725, its integer pipes. So
`heat`'s 0.70 there is not those numbers.

Root-mean-square log error: 1.12, 0.34, 0.62, **0.19**. On a second core the calculus has to be
refitted (its five constants on five kernels) and then errs about as it did on the first; M7 needs
the core's table, which the same microbenchmarks give, and errs as little as it did there. `heat`,
memory-bound, is M7's worst on both: its memory term is still the X925's bandwidth.

## Programs the project did not write: the Benchmarks Game (2026-09-28)

**Why.** The corpus is the project's own (evaluation.md, threats). Five programs of the Computer
Language Benchmarks Game were ported keeping the published programs' structure — `tests/bench/`,
pinned by `bootstrap/tests/bench.rs` — and each prints the reference output at its test size (n-body
at 1000 steps −0.169075164 / −0.169087605, spectral-norm at 100 1.274219991, fannkuch-redux at 7
checksum 228 and 16 flips, binary-trees at 10 the published counts). Only one thing had to be
worked around: there is no shift operator, so `1 << k` is a loop.

**Tiers.** 11 of 15 functions exact, 4 unknown. Exact: all of n-body (`advance`, `energy`, `main`
`634·steps + …`), all of spectral-norm (`main` ≈ `640·n²`), both of mandelbrot. Unknown, each for a
reason already named: fannkuch's `main` (permutation state in `while` loops whose trips are the
data's); binary-trees' `build` (two recursive calls each shrinking the depth by one — an exponential
recurrence, which the calculus refuses), `check` (recursion over a tree in an arena, the compiler's
largest unknown row) and `main`. Since 2026-09-28 `check` is a bound, `22·ns.len() + 11` (cost-model
§ Recursion, a tree), and three remain unknown.

**Against the clock** (`tests/bench/timing.py`, the fitted roofline, CPU 5):

| program | sizes | bound | measured / predicted |
|---|---|---|---|
| nbody | 10⁵ / 10⁶ / 5·10⁶ steps | moves | 0.15 / 0.15 / 0.15 |
| spectral_norm | n = 200 / 800 / 2000 | work | 1.47 / 1.89 / 1.92 |
| mandelbrot | n = 200 / 800 / 2000 | work | 3.58 / 4.27 / 4.24 |

Geometric mean 1.02, range 0.15 to 4.3 — the stable ratios say the shapes are right and the
constants are not, and each constant is off for a reason:
- **Work is not one rate.** `τ` was fitted on a polynomial the C compiler vectorises and pipelines.
  Mandelbrot's inner loop carries `z` from one lap to the next and exits early, so it runs at the
  latency of its dependent multiply-adds, 4× slower; spectral-norm divides in its inner loop, 2×.
  A latency term for work — a dependency chain's length, as `chase` is for memory — is what this asks.
- **n-body's moves are charged where it has none.** Five bodies are 280 bytes; `advance` is charged
  ≈ `28·n²` moves a call while its whole array fits, because its inner loop runs `j in i + 1..n`, a
  range that moves with `i`, and each lap's lines were summed rather than taken as the hull they
  share. The prediction is moves-bound at 6.7× the time. Charged its hull once (cost-model § Moves,
  a triangle), `advance` is `272·n + 20·B − 112` in cache and n-body is 0.27 of the predicted; the rest
  is each call charged cold, since seven SoA fields make one inexact footprint and no residue is
  credited across calls.

**Serial work, the same day** (cost-model § Time, serial work). Work in a loop that carries a scalar
through a multiply is charged `τ_s`, fitted on a logistic map that is not one of these programs:
**τ_s = 0.155 ns**. Mandelbrot, never used in any fit, goes from 3.6 / 4.3 / 4.2 to **0.53 / 0.51 /
0.50** — the remaining half is its escape loop's trip, 50 laps as an upper bound where most points
leave sooner — and the geometric mean over the three programs is 0.64, range 0.26 to 1.93. The corpus
and the kernels do not move (their loops carry only adds). Spectral-norm's 1.9× is its divide, which is
not on a chain: a throughput for division is what is left there. **Divisions, counted apart** and
charged `τ_div = 0.148 ns` beyond a unit of work, fitted on a sum of reciprocals (cost-model § Time,
divisions): spectral-norm 1.89–1.94 → **1.26–1.28**, the three programs within **0.26–1.28**,
geometric mean 0.57; the kernels (1.15) and the corpus (0.60) are where they were.

**A resident call, the same day** (cost-model § Moves, footprints of several ranges). n-body's
`advance` on five bodies now leaves a footprint of exact ranges, and the steps loop's calls move
nothing once it is resident: the prediction is work-bound, **2.3×** too fast (from 0.27, too slow),
and the L2 refills agree that nothing moves per step (220–280 KB at 10³, 10⁵ and 10⁶ steps, an empty
program's 180 KB of start-up included). What is left there is compute the constants do not see: inner
loops of four laps or fewer, and `bs[i].vx` accumulated through memory on every lap — a chain the
serial rule, which looks at scalars, does not follow. The three programs are within 0.47–2.28, geometric
mean 1.11; the corpus 0.60, the kernels 1.13. **The velocity chain through memory**, a store of
`bs[i].vx` from `bs[i].vx` with `i` fixed in the loop, is one serial unit a lap (cost-model § Time,
serial work, through memory): n-body 2.28 → **1.76**, the three programs within 0.50–1.77, geometric
mean 1.09. Counting the whole loop serial instead overshot, to 0.37.

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

## A bug in the reference compiler, found by building the second one

`site_range` in `analyze.rs` reports a site's byte range **shifted right by its loop's starting
offset**, because the offset is counted twice. Found while designing the self-hosted footprint
report (design §22), when a derivation straight from the source did not reproduce the printed range.

```neant
fn from2(a: &mut [f64]) { for i in 2..a.len() { a[i] = 1.0; } }
fn from3(a: &mut [f64]) { for i in 3..a.len() { a[i] = 1.0; } }
fn from0(a: &mut [f64]) { for i in 0..a.len() { a[i] = 1.0; } }
```

| function | true range | `neant cost` says |
|---|---|---|
| `from0` | `[0, 8·a.len())` | `[0, 8·a.len())` |
| `from2` | `[16, 8·a.len())` | `[32, 8·a.len() + 16)` |
| `from3` | `[24, 8·a.len())` | `[48, 8·a.len() + 24)` |

Both ends are out by exactly the start. `from0` is right because zero doubled is zero, which is why
the corpus never showed it: every loop in it starts at 0 except `stencil`'s, and `stencil`'s
footprint was never checked against a hand computation.

**The cause.** `affine()` gives a loop variable the form `Affine::var(l) + offset`, where `offset`
is the loop's start expressed in the enclosing loops — it has to, so that `for i in ii*4..ii*4+4`
is seen to move with `ii`. `site_range` then substitutes `rec.lo` and `rec.last()` for that same
variable, and `rec.lo` *is* the start again. The constant is added once in the affine form and once
in the substitution.

**The fix** is one line: the affine form already carries the start, so the variable's own
contribution runs from `0`, not from `rec.lo`:

```rust
let last_rel = rec.trip.sub(&Poly::constant(1)).scale(Rat::int(rec.step));
let (a, b) = (Poly::zero(), c.mul(&last_rel));
```

Checked by hand against all three probes and against `stencil`'s two-deep nest, where it turns
`dst: [16·n + 16, 8·n²)` into `[8·n + 8, 8·n² − 8·n − 8)` — the range the indices actually reach.

**Why it matters, and it is not cosmetic.** A footprint is what a caller credits against: a call
leaves its footprint resident, and a later call pays nothing for the part that is already there.
A range shifted *right* claims bytes past the end were brought in, so a caller can be credited for
memory the callee never touched — an under-report of `moves`, which is the unsound direction. The
corpus does not contain the pair of calls that would show it, which is luck rather than safety.

**Fixed, and the blast radius was two lines.** Only `stencil`'s and `tri`'s golden reports change,
and only their `footprint` line; no `work`, `moves` or `lower bound` column in the corpus moves at
all, and the self-hosted pass still agrees on all three of its columns. `tri`'s correction is the
second thing the fix buys: it read `a: [32·ii, 32·ii + 8·n)`, a footprint stated in terms of a loop
variable that does not exist when the function returns — `rec.lo` for `for i in ii*4..ii*4+4` is
`ii*4`, so the double count put `ii` in a range a caller was supposed to read. It is now `[0, 8·n)`.

**How it was found is the point.** Not by reading `site_range` — three readings of it produced
formulas that each matched the source and none of the output. It was found by *predicting* what a
double count would print for `2..len` and `3..len` before running them, and then running them. The
second implementation is only a check on the first when the two are derived independently and
compared on numbers neither was tuned to.

## What each language decision buys: ablations (2026-09-29, static, then the clock)

README § The decisions underneath says what each decision buys and measured none of it apart from
the M4 gate. So each one that the compiler can turn off is turned off, `NEANT_ABLATE=layout`,
`region` or `reuse` (`bootstrap/src/cost/mod.rs`), and every program the repository has is costed
both ways, `tests/ablate/count.nt`:

- **layout**: every struct stays as declared, AoS, as Rust's `Vec<T>` must — what the compiler
  may do because a projection is not an address;
- **region**: a walk over an arena costs a line per step even where the arena fits, as a walk
  through pointers must when nothing says where they point — what the index-into-a-named-array
  link buys;
- **reuse**: `ys = xs` always copies — what uniqueness and in-place update buy.

The programs: 82 goldens that check, the corpus's 8, the Benchmarks Game's 5, PolyBench's 28 (adi
and heat-3d do not finish being costed, either way) and the compiler: 1080 report lines, 278 of
them the compiler's. A line counts as changed if any column of it does. In all, 31 lines change
without the layout choice, 20 without the arena rule and 4 without reuse.

| turned off | lines changed outside the goldens | by how much | tiers changed |
|---|---|---|---|
| layout | the compiler 20 (one struct of 19, `Fac`), n-body 1 | the changed lines' moves ×2; n-body's five bodies fit either way | **none** |
| region | the compiler 2, corpus `bfs` and `csv` 1 each | `csv`'s `main` `4·len` → `(B + 4)·len`, ×17 at `B` = 64; in the goldens `arena`, `tree` up to ×8 | **none** |
| reuse | **none** | only the three goldens written for it | **none** |
| any | PolyBench: **none**, in 390 lines | — | — |

**What that says.**

1. **No decision changes a tier.** Exact, modulo, bound and unknown are the same counts under
   every ablation, on every program. What the decisions buy is precision in a line the compiler
   would state anyway, and the code it emits — not reach. Stage D's number does not rest on any
   of them.
2. **Reuse is never used outside its own tests.** No program in the corpus, the Benchmarks Game,
   PolyBench or the compiler reassigns a whole array. M5's result stands (a forced copy moves what
   the model says), but nothing written since needed it.
3. **Layout is chosen once in the compiler's nineteen structs.** `Fac { atom, exp }` goes SoA
   because the monomial loops (`mono_cmp`, `mono_degree`, `pol_eq`, `log_atom` …) read one field;
   the other eighteen are read a whole element at a time and stay AoS. The two builds of the
   compiler (`neant emit` with and without `NEANT_ABLATE=layout`) compile the compiler to the same
   bytes, so the difference is time only; it is measured below.
4. **The region rule changes most, and it is the least a language's.** That an index lands in the
   array it indexes is the language's guarantee, but a C analyser told a pool's extent could apply
   the same rule, and what it changes is a bound's tightness, not what the program does.
5. **PolyBench is untouched.** Affine code with no structs is where the language neither costs
   anything (the ports' changes are all syntax: a downward loop as a `while`, an out-parameter set
   in `main`, `<=` as a half-open range) nor buys anything — the same result an analyser on the C
   would give, which is what stage E's reach baseline will show.

So on this repository's programs the argument that this is a language, and not an analyser over a
Rust subset, rests on representation — and representation, as built, is the choice between AoS
and SoA, taken once in nineteen structs of the one ordinary program. What the rest of the argument
needs is a representation change the compiler does not make yet: the compiler's nineteen structs
are all `i64` fields, most of them an index into an arena or a kind, whose width only a compiler
that owns the layout can choose.

**Against the clock** (`tests/ablate/measure.nt`, CPU 5, an X925 with 2 MiB of L2; each figure
the minimum of 5 runs, bytes `l2d_cache_refill × 64` in separate runs; two sittings, the second's ratios after the slash):

| program | built (SoA) | ablated (AoS) | time AoS/SoA | bytes AoS/SoA | predicted |
|---|---|---|---|---|---|
| `particles.nt`, n = 4 000 000 × 20 (128 MB, past L3) | 0.212 s, 4.54e9 B | 0.303 s, 7.83e9 B | ×1.43 / 1.41 | ×1.72 / 1.73 | 4.48e9 against 7.68e9 B, ×1.71 |
| `particles.nt`, n = 20 000 × 5 000 (640 KB, inside L2) | 0.157 s, 5.2e5 B | 0.141 s, 1.7e8 B | ×0.90 / 0.90 | ×325 / 1.03, both counts noise | nothing moves |
| the compiler compiling itself, `Fac` SoA against AoS | 0.344 s, 2.41e8 B | 0.346 s, 2.48e8 B | ×1.00 / 1.01 | ×1.03 / 0.97 | — |

Past the cache the model's bytes are the counter's within 2% in both layouts, and their ratio is
the counter's; the time follows at ×1.43, less than the bytes (the time model, not the byte count, is what
would have to say by how much). Inside L2 both layouts move under 2% of a streaming pass and AoS is if
anything faster — where the model says nothing moves, the choice buys nothing. The compiler's one
SoA struct buys nothing on the clock: turning it off leaves the compiler's bytes unchanged
within ±3% across two runs, and its time too. So layout, the one representation choice the compiler makes, is right where it
applies and measured where it matters, and on the one ordinary program it is not visible.

## Held-out, row one: PolyBench with the frozen compiler (2026-09-29)

Stage E step 3 (heldout.md). The 30 PolyBench/C 4.2.1 ports, committed and checked against the
original C at a962c26, costed with the compiler of `42f76be` (`measure.nt --cost`: 122 port-defined
functions, 114 exact, 8 unknown — the refusals adi and heat-3d, whose cost pass did not finish in
900 s; 21 std functions, 12 exact and 9 declared) and timed at MEDIUM, LARGE and EXTRALARGE:
wall-clock minimum of three pinned runs and `l2d_cache_refill × 64` minimum of three, an empty
program's subtracted, load 1.0–2.1. Runs: 23 kernels on CPU 5, 7 on CPU 15 (the other cluster's X925,
separate L2 and L3), the two at once; gemm rerun on CPU 15 during a CPU 5 run matched CPU 5 within
3% in time (376.6 against 389.1 ms, 3660 against 3740 ms) and 0.2% in bytes. The data is
`tests/heldout/polybench/row1.json`; `summary.nt` judges it.

- **Bytes** (EXTRALARGE, past L3): 18 kernels, geometric mean 0.64, 15 in [1/3, 3] — holds.
  Outside: atax 0.25, bicg 0.32, trisolv 0.33.
- **Time** (51 runs ≥ 1 ms): geometric mean 1.69, 11 outside [0.25, 4] — fails, against a
  threshold of 10.2: seidel-2d 5.3–5.8 at all three sizes, correlation and covariance 9.0 and
  gramschmidt 8.7 at EXTRALARGE, bicg 4.15, trisolv 4.11, mvt 4.08 (CPU 15), correlation at MEDIUM
  0.04, durbin at EXTRALARGE 0.2495.
- **No prediction**: 30 of 90 runs. Two refusals, and eight kernels whose `main` is exact but whose
  regimes are conditions on a triangular loop's outer variable (`kernel_symm.i`), which `--eval`
  cannot be given.

What it points at, in the order it would move the numbers: the fit test of a triangular loop split
over its outer index (eight kernels' predictions), a carried dependence in a sweep (seidel-2d), a
strided walk's time at the bytes the model already gets right (correlation, covariance), a dot
product's serial chain (the matrix–vector kernels), and `settle_moves`' exponential search (the two
refusals). Each is a later row, labelled with its commit; this one is not replaced.

## Held-out, later rows: the time model's changes and a triangle's fit test (2026-09-29)

Row one's 90 runs costed again by later compilers (`measure.nt --predict`): the 27 ports that
build under all of them emit the same C as `42f76be`, byte for byte, so the binaries and their
measurements are row one's. Judged by `summary.nt`; the data in `tests/heldout/polybench/`.

| row | compiler | time | bytes | no prediction |
|---|---|---|---|---|
| 1 | `42f76be` | 51 runs, gm 1.69, 11 outside — fails | 18 kernels, gm 0.64, 15 in — holds | 10 kernels |
| 2a | `cc66c66` | 51 runs, gm 1.65, 8 outside — between | as row 1 | 10 kernels |
| — | `f91bd83` | 74 runs, gm 0.94, 18 outside — fails | 26 kernels, gm 0.38, 20 in — between | 2 kernels |
| 2b | `5e997b9` | 75 runs, gm 1.53, 10 outside — between | 26 kernels, gm 0.66, 22 in — holds | 2 kernels |
| 2c | `97cb921` | 75 runs, gm 1.42, 7 outside — between | as 2b — holds | 2 kernels |
| 2d | `796deb5` | 75 runs, gm 1.06, 4 outside — between | as 2b — holds | 2 kernels |

- `cc66c66` carries the freeze's calculus plus `17d7d75` and `1be9f2a` (the time model's serial add
  and stream rules, committed 2026-09-28, before any held-out number): atax, bicg, gemver and
  trisolv are predicted 20–33% slower, which brings bicg at LARGE, trisolv and mvt at EXTRALARGE
  inside [0.25, 4].
- `f91bd83` decides a triangle's fit test over its laps: eight kernels (cholesky, lu, ludcmp, symm,
  syrk, syr2k, trmm, nussinov) whose regimes named `kernel_X.i` get a prediction. Two faults that
  those regimes had hidden were then chosen: a triangle's hull slid a second time (lu and ludcmp
  at `64·n⁴/(3·B)` bytes, 0.01 of the counter) and the sum it falls back to slid the same way
  (nussinov `n⁴`, 0.00).
- `5e997b9` stops both slides and takes a nested trip's variable at its extreme before the hull:
  at EXTRALARGE, measured over predicted bytes are lu 0.60, ludcmp 0.59, cholesky 0.64, syrk 1.12,
  syr2k 1.26, symm 1.16, trmm 1.05, and time 1.53, 1.53, 1.53, 2.08, 3.67, 3.20, 5.58; nussinov
  stays overcharged, bytes 0.17 and time 0.44. `tri`'s `pairs` goes from 32 to 24·a.len() in cache and n-body's `advance`
  from 272 to 216·bs.len(); neither changes a tier or n-body's layout.

`~/work/.neant-loop/at-cc66c66` is the worktree `cc66c66` was built from, apart from the main tree.

`97cb921` (2026-09-30) adds a recurrence through memory at a moving index — a lap that reads the
element the last one stored — charged as a carried scalar is: seidel-2d's value passes the `/ 9`, so
its whole lap is serial, 53 units at `τ_s`. Predicted over measured time goes from 5.31, 5.75, 5.81
to 0.78, 0.80, 0.81 at MEDIUM, LARGE and EXTRALARGE; no other kernel's prediction moves by more than
1%. Golden `lap_chain` pins the rule's `serial` column through a new `.eval` file.

`796deb5` (2026-09-30) charges translations. `tests/kernels/pagewalk.nt` walks a column of lines in
cache at a stride of 4160, 11 200 and 20 800 bytes, four sums so no add chain paces it: 0.13 ns an
access while the pages fit the first-level TLB, 0.3–0.7 ns while they fit the second (to 2048 pages),
1.3–2.1 ns past it; `τ_tlb` = 1.6 ns is the mean at 4096–8192 pages less the one at 1024. A first
run at 4 KiB and 8 KiB strides read 1.0 ns already at 128 pages, which was the lines meeting in one
set of the cache, not the TLB: at a stride of 832 elements (6.5 KiB) it is 0.48. `matmul_naive` at
832, a new page every lap and 832 pages a column, measures 0.37 ns a lap and is charged nothing.
Held-out, measured over predicted time at EXTRALARGE: correlation 9.16 → 0.98, covariance 9.19 →
0.98, gramschmidt 9.01 → 0.96, 2mm 2.81 → 0.95, 3mm 3.00 → 1.06, symm 3.20 → 0.69, lu 1.53 → 0.86,
cholesky 1.53 → 0.98; at LARGE correlation and covariance 3.62 → 0.41 and 3.78 → 0.42.

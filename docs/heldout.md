# Held-out: the thresholds, before the numbers

Plan § Stage E, step 1. This file is committed before any program below is costed or timed, and
is not edited after that except to fill in the lines marked *recorded*, each in a commit of its
own. What the numbers turn out to be goes in evaluation.md, beside the thresholds here; a threshold
that looks wrong once the numbers are in is argued there, and this file keeps the one it had.

## What is frozen

- **The calculus and the roofline's constants: `42f76be`** (the parse's ends and its serial work;
  `τ`, `BW`, `L`, `τ_s`, `τ_div`, `BW_max` as cost-model § Time has them there). Row one of every
  table is this commit's compiler, built from a worktree checked out at it. A change to
  `bootstrap/src/cost/` after it is measured as a later row, labelled with its commit, and never
  replaces row one.
- **The machine**: one Cortex-X925 core (CPU 5), pinned with `taskset`, the harness of
  plan § Validation harness notes; bytes are `l2d_cache_refill × 64`. The second machine's
  counter is chosen and written here (*recorded*) before its sweep.
- **The ports**: each committed before it is costed. A port's text is not edited after its first
  cost is seen, except to fix an output that differs from the reference — a fix that is listed,
  with the diff, in the port's directory as `FIXES`, and re-counted as a later row.

## The held-out set

Written by the rules of plan § Stage E (2): the published structure kept, one function in the port
for each function of the original, loops in the original's order with its bounds, the original's
initialisation formulas. What the language forces is the same everywhere and is not counted as a
change: a 2-D or 3-D array is flat and row-major (`A[i][j]` is `a[i * nj + j]`, decisions §11:
a grid cannot be passed as a grid), sizes come from the command line instead of macros, `int` is
`i64`, `float` is `f64`, and the dump of the live-out arrays is one checksum line per array, the
sum of its elements in row-major order. Anything else a port had to change is written at the top
of its `main.nt`, and a program the language cannot express is a **refusal**, kept with its
reason and counted.

1. **PolyBench/C 4.2.1-beta**, all 30 kernels (`tests/heldout/polybench/`). The source is the
   release tarball, `sha256 426519ee8443a5f2175de6a3e9328cda8917a5e33053d0e8f59855ab56d689a4`;
   the reference output is that source compiled with `-O2`, `-DDATA_TYPE_IS_DOUBLE`, the port's
   sizes as `-D` defines, `-DPOLYBENCH_DUMP_ARRAYS`, and each dump summed the same way.
2. **The rest of the Benchmarks Game**: fasta, k-nucleotide, reverse-complement, pidigits,
   regex-redux (`tests/bench/`, beside the five there).
3. **Outside numerics**: a sort, a hash join, a B-tree search, an LZ77 compressor, each from a
   named published implementation (*recorded* with its source before it is ported).

## What is counted

- **A function** is one the port defines — not the `std/` functions it `use`s, which are
  counted once, apart. A refusal counts every function of the original as unknown.
- **A run** past the harness's noise: a predicted or measured time under 1 ms is reported and not
  in a mean. The sizes are the original's `MEDIUM`, `LARGE` and `EXTRALARGE` datasets for
  PolyBench, and three sizes a factor of ten or more apart elsewhere, fixed in each port's
  directory before the first timing.

## The thresholds

Each question has three outcomes, and evaluation.md will say which one in these words.

| question | holds | fails | between |
|---|---|---|---|
| **Bytes** (PolyBench, the largest size, every kernel whose footprint exceeds L3) | geometric mean of measured / predicted moves in [0.5, 2], and 80% of kernels within [1/3, 3] | geometric mean outside [1/3, 3] | reported, with the kernels outside |
| **Time** (every held-out run past the noise) | geometric mean in [0.5, 2], and every run in [0.25, 4] | geometric mean outside [1/3, 3], or more than 20% of runs outside [0.25, 4] | reported, with the runs outside |
| **Ranking** (the variants, step 6) | mean Kendall τ ≥ 0.8, and above the work-only model's by 0.1 or more | mean τ no higher than the work-only model's | reported |
| **Reach** (functions of the held-out set) | stated (exact, modulo or bound) ≥ 2/3, and exact ≥ the C analyser's exact on the same functions | the C analyser, on the emitted C, states as many functions as the compiler | reported, row by cause |
| **Trust** (the generator, step 7) | no wrong `exact` and no `bound` below the count, at the large budget | a wrong line in each of three rounds, the rate not falling | each wrong line fixed with a golden and listed |

Where these came from, so that they can be argued with: the time band is the one the corpus and
the Benchmarks Game already sit in (evaluation § 2), so holding means the model generalises, not
that it improves; the reach floor is the compiler's own stated share (184 of 278 at `3eacf60`);
the bytes band is the one the M1 kernels passed. None of them is chosen from a held-out number,
since none has been taken.

## The order of the work

1. This file, committed.
2. The PolyBench ports and their reference check (`tests/heldout/polybench/check.py`), which runs
   each port and the original C and compares outputs — and does not cost anything — committed.
3. Costing, bytes and time on PolyBench with the frozen compiler: the first held-out row.
4. The baselines (IOLB, the simulator, the C analyser, the smaller time models), the other two
   parts of the set, the second machine, the variants and the generator, each its own row.

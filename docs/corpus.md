# The domain corpus

**Measured 2026-09-26, on `d4b1c85`** (main with modules, program input, arrays by value). Plan
§ Stage C asked for the tier count to be taken again "for a corpus chosen from the domain where
these constraints are assets (control loops, kernels, real-time code)", and § Stage D named argv,
modules and arrays by value as what such a corpus could not be written without. All three exist,
so here it is: six programs in `tests/corpus/`, written the way one would write them, with the
language and the calculus as they are. `bootstrap/tests/corpus.rs` pins every program's output,
its cost report, and `tests/corpus/tiers.txt`, the tier of every function as the reports state it.

## The programs

| program | what it does | input |
|---|---|---|
| `pid` | a PID controller holding a mass-spring-damper to a setpoint trajectory; plant state `[f64; 2]` and matrix `[f64; 4]` stepped by value; RMS error through `sqrt` from libm | a file of 600 setpoints |
| `heat` | a 5-point Jacobi stencil on an n×n plate, two buffers alternating | `n` and the step count as arguments |
| `matmul` | dense `C = A·B`, trace and checksum | `n` as an argument |
| `csv` | per-sensor count, mean and maximum over a `sensor,time,value` log with a header | a CSV file of 300 rows |
| `bfs` | an edge list read into compressed rows, breadth-first hop counts from vertex 0 | a graph file, 60 vertices, 91 edges |
| `fir` | an 8-tap moving average, its ring buffer a struct with an array field stepped by value; energy and lag of the filtered signal | a file of 160 samples |

Shared code is `tests/corpus/lib/parse.nt` (integers out of a `[u8]`: `is_digit`, `next_int`,
`parse_int`, `count_ints`, `read_ints`) and `lib/math.nt` (`sqrt`, an `extern` with a declared
cost). Every program `use`s the first; the report of each includes the library's functions, so
the per-program rows below count them, and the totals count each function once.

## Tiers

Functions as the reports state them; the three input externs and `sqrt` are the boundary
(declared) and are not in the share.

| program | exact | modulo | bound | unknown | functions |
|---|---|---|---|---|---|
| pid | 5 | 2 | 0 | 3 | 10 |
| heat | 4 | 2 | 0 | 3 | 9 |
| matmul | 4 | 2 | 0 | 3 | 9 |
| csv | 2 | 2 | 0 | 3 | 7 |
| bfs | 1 | 2 | 0 | 3 | 6 |
| fir | 3 | 2 | 0 | 3 | 8 |
| **distinct functions** | **14** | **2** | **0** | **8** | **24** |

**14 of 24 exact, 58%** — against 6 of 11 on the M3 corpus and 92 of 276 (33%) on the compiler
(plan § Stage D). And **0 of 6 `main`s exact.** Every function that computes — `sweep`, `matmul`,
`plant_step`, `pid_out`, `push`, `mean`, `edges`, `trace`, `fill` — is exact, with its regimes;
no program is.

## Causes

Every function that is not exact, with the cause as its report states it (the report names the
first cause it meets, so a line may have others behind it; see below):

| function | tier | cause |
|---|---|---|
| `next_int` (lib) | unknown | `` `i` is assigned inside a loop before this one, so its entry value is not known `` (parse.nt:23) |
| `count_ints` (lib) | unknown | `` `i` is compared in the `while` condition but is not stepped by a constant exactly once in the body; `while` has no measure the compiler can find `` (parse.nt:41) |
| `parse_int` (lib) | modulo | rests on `next_int (unknown)` |
| `read_ints` (lib) | modulo | rests on `next_int (unknown)` |
| `pid` `main` | unknown | `` the length of `sp` is not a size expression `` (main.nt:43) — `sp` is `[0; count_ints(..)]` |
| `heat` `main` | unknown | `` the length of `a` is not a size expression `` (main.nt:39) — `n = parse_int(&arg(0))` |
| `matmul` `main` | unknown | `` the length of `a` is not a size expression `` (main.nt:37) — the same |
| `csv` `main` | unknown | `` `i` is compared in the `while` condition but is not stepped by a constant exactly once in the body `` (main.nt:25) — the row loop, `i = v.end + 1` |
| `bfs` `main` | unknown | `` the length of `nums` is not a size expression `` (main.nt:11) — `[0; count_ints(..)]` |
| `fir` `main` | unknown | `` the length of `xs` is not a size expression `` (main.nt:25) — the same |

So the eight unknowns are two shapes: **a size that comes out of parsed input** (five `main`s)
and **a scan whose step is data** (`next_int`, `count_ints`, and `csv`'s row loop), which is how
text is read when a number ends where its digits end. The two modulo lines are the second shape by
contagion.

**Behind the first cause.** Probed on scratch copies, not in the corpus: with the parsed size
replaced by an argument's length (`n = a0.len()`, an atom the calculus has, since stage D's input
work), `heat`'s and `matmul`'s `main` become **exact** — `≈ 31·n²·steps` and `≈ 10·n³` with their
regimes — so for the two kernels the input boundary is the only thing in the way. `pid`'s and
`fir`'s become **modulo** `next_int`: the parser loop is behind the size. `bfs`'s stops next at
the queue: `` the bound of `head` is assigned inside the body `` — the worklist M3 named, a trip
count that is the number of vertices reached, and not a size expression.

## What the numbers say

The governing claim (plan § Who switches) is that the exact tier moves on ordinary code. On code
from its own domain it does, for the part that is the domain: every kernel, every controller step,
every per-element function is exact, and the share is 58% against 33% on the compiler. What does
not move is the program: none of the six whole runs has a cost, because each begins by reading a
number out of text, and a number read out of text is not a size. That is one gap, not six — an
`i64` returned by a call has no atom (the calculus names `arg(k).len()` and `data.len()`, but not
`parse_int(&arg(k))`), and a scan that advances by what it read has no measure. The first is the
smaller: an integer read from input could be an atom of its own for the run, as `arg_count()` is,
and the probe says that alone makes the two kernels' programs exact. The second is the parser
shape the compiler corpus already showed (`tok_text_eq`, the lexer), met again here from the
other side; `decreasing` would state it by hand. The worklist is the third and is where it was.

## Re-counted after a size bound once, 2026-09-26

The first gap named above is closed in the calculus (cost-model § A size bound once): an `i64`
bound once to an immutable local outside every loop is an atom of its own. No program changed;
five reports moved (`tiers.txt` and the `.cost` pins are the new ones).

| program | exact | modulo | bound | unknown | functions |
|---|---|---|---|---|---|
| pid | 5 | 3 | 0 | 2 | 10 |
| heat | 4 | 3 | 0 | 2 | 9 |
| matmul | 4 | 3 | 0 | 2 | 9 |
| csv | 2 | 2 | 0 | 3 | 7 |
| bfs | 1 | 2 | 0 | 3 | 6 |
| fir | 3 | 3 | 0 | 2 | 8 |
| **distinct functions** | **14** | **6** | **0** | **4** | **24** |

**Still 14 of 24 exact, but 4 of the 6 `main`s now have a cost**, where none had one. The parsed
size is an atom: `heat` is `≈ 31·n²·steps` in work and `≈ 96·n²·steps` in moves while a row fits,
and `matmul` is `≈ 10·n³` with its six regimes. `pid` and `fir` are linear in `n`, the number of
samples `count_ints` found. They are modulo and not exact because each still calls the parser to
get the number: `work[next_int](a0.len(), 0)` is a term in `heat`'s line, and `count_ints`'s cost
is one in `pid`'s. The probe above replaced the parse with an argument's length, which costs
nothing, so it said exact; the real program pays for the parse, whose loop has no measure. The
causes left:

| function | tier | cause |
|---|---|---|
| `next_int` (lib) | unknown | unchanged: `i`'s entry value is set in an earlier loop |
| `count_ints` (lib) | unknown | unchanged: `i` is not stepped by a constant |
| `parse_int`, `read_ints` (lib) | modulo | rest on `next_int` |
| `pid`, `heat`, `matmul`, `fir` `main` | modulo | rest on `next_int`, and `pid`/`fir` on `count_ints` |
| `csv` `main` | unknown | unchanged: the row loop steps by what it read |
| `bfs` `main` | unknown | now its second cause: `` the bound of `head` is assigned inside the body `` (main.nt:42), the worklist |

So what stands between these programs and an exact cost is now one shape and one old hole. The
shape is a scan that steps by what it read, which is all of the parser. The hole is the worklist.
Both were named in plan § Stage D before the corpus existed.

## What the corpus could not say, and could not be written

Rejected, each kept as a minimal case in `tests/corpus/rejected_*` with its error pinned:

| case | what one would write | error |
|---|---|---|
| `rejected_string` | `println("mean error")` — any labelled report | `expected an expression, found a string` |
| `rejected_grid2d` | `let g = [[0.0; n]; n]` — a plate as rows | `an array literal can only initialise a let for now` |
| `rejected_plants` | `[Plant { x: [0.0; 2] }; 3]` — several plants under one supervisor | `` `ps` in `main` is an array of `Plant`, which holds an array field `` |
| `rejected_vecparam` | `fn dot(a: [f64; 3], b: [f64; 3])` — a 3-vector without a struct around it | `arrays are passed as views: write &[T] or &mut [T]` |

The programs were written around them: output is bare numbers, the plate is flat (`i * n + j`),
there is one plant, and every fixed-size vector sits in a struct. None of these changes a tier;
they are the cost of writing the corpus, not of measuring it. `sqrt` is not in the list: an
`extern` with `#[cost(work_at_most, moves_at_most)]` is enough, and `pid` crosses the boundary
through it.

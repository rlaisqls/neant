#!/usr/bin/env python3
"""The Benchmarks Game programs against the clock: each program whose `main` has a cost, its
predicted time (`neant cost --eval`, the fitted roofline) against wall-clock at three sizes, on one
pinned big core. The same harness as tests/corpus/timing.py, over programs not written here.

    timing.py [--cpu 5] [--runs 3]
"""
import argparse, math, pathlib, sys
HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / "../corpus"))
import timing as corpus   # predict, wall, run, NEANT

def arg_int(key):
    return lambda n, d: ([str(n)], {key: n, "a0.len()": len(str(n))})

PROGRAMS = {
    "nbody":         ([100_000, 1_000_000, 5_000_000], arg_int("steps")),
    "spectral_norm": ([200, 800, 2000], arg_int("n")),
    "mandelbrot":    ([200, 800, 2000], arg_int("n")),
}

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cpu", type=int, default=5)
    ap.add_argument("--runs", type=int, default=3)
    a = ap.parse_args()
    import tempfile
    work = pathlib.Path(tempfile.mkdtemp(prefix="neant-bench-timing-"))
    empty = work / "empty.nt"; empty.write_text("fn main() {\n    println(0);\n}\n")
    corpus.run([str(corpus.NEANT), "build", str(empty), "--unchecked", "-o", str(work / "empty")])
    base = corpus.wall([str(work / "empty")], a.cpu, a.runs)
    print(f"baseline {base*1e3:.2f} ms, cpu {a.cpu}")
    print(f"  {'program':<14} {'size':>9} {'bound':>6} {'pred ms':>10} {'meas ms':>10} {'meas/pred':>9}")
    ratios = []
    for name, (sizes, make) in PROGRAMS.items():
        nt = HERE / name / "main.nt"
        binary = work / name
        corpus.run([str(corpus.NEANT), "build", str(nt), "--unchecked", "-o", str(binary)])
        for s in sizes:
            argv, env = make(s, work)
            p = corpus.predict(nt, env)
            t = corpus.wall([str(binary), *argv], a.cpu, a.runs) - base
            if p is None:
                print(f"  {name:<14} {s:>9}   no prediction"); continue
            w, mv, pt, bound = p
            ratios.append(t / pt)
            print(f"  {name:<14} {s:>9} {bound:>6} {pt*1e3:>10.3f} {t*1e3:>10.2f} {t/pt:>9.2f}")
    g = math.exp(sum(math.log(x) for x in ratios) / len(ratios))
    print(f"\ngeometric mean measured/predicted over {len(ratios)} runs: {g:.2f}  (range {min(ratios):.3g} … {max(ratios):.3g})")

if __name__ == "__main__":
    main()

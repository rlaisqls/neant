#!/usr/bin/env python3
"""The second core: five programs timed on a Cortex-A725 (CPU 0), against the calculus with the
X925's constants and with constants refitted on CPU 0 (roofline.py fit --cpu 0), and against M7 with
each core's model (neant cost --m7 [--core a725]; docs/experiments.md, a second core)."""
import subprocess, time, re, pathlib, math, tempfile
N = pathlib.Path.home() / "work/neant/bootstrap/target/release/neant"
R = pathlib.Path.home() / "work/neant/tests"
progs = [("bench/spectral_norm", "n=2000,a0.len()=4", ["2000"]), ("bench/nbody", "steps=1000000,a0.len()=7", ["1000000"]),
         ("corpus/matmul", "n=300,a0.len()=3", ["300"]), ("corpus/matmul", "n=600,a0.len()=3", ["600"]),
         ("corpus/heat", "n=300,steps=50,a0.len()=3,a1.len()=2", ["300", "50"])]
work = pathlib.Path(tempfile.mkdtemp())
def pred(d, ev, extra):
    out = subprocess.run([str(N), "cost", "main.nt", "--eval", ev + ",B=64", *extra], cwd=R / d, capture_output=True, text=True).stdout
    main = re.split(r"\n(?=\S)", out.split("\nmain", 1)[1], maxsplit=1)[0]
    t = float(re.search(r"time (\S+) s", main).group(1))
    m = re.search(r" m7 (≤ )?([0-9.e+-]+) s", main)
    return t, (float(m.group(2)) if m else None)
rows = []
for d, ev, args in progs:
    b = work / d.replace("/", "_")
    subprocess.run([str(N), "build", "main.nt", "--unchecked", "-o", str(b)], cwd=R / d, check=True, capture_output=True)
    best = 1e9
    for _ in range(3):
        t0 = time.perf_counter(); subprocess.run(["taskset", "-c", "0", str(b), *args], capture_output=True); best = min(best, time.perf_counter() - t0)
    calc, m7x = pred(d, ev, ["--m7"])
    calcA, _ = pred(d, ev, ["--tau", "0.0604", "--bw", "17.04", "--lat", "136.0", "--taus", "0.2134", "--tdiv", "0.0620"])
    _, m7a = pred(d, ev, ["--m7", "--core", "a725"])
    rows.append((best / calc, best / calcA, best / m7x, best / m7a))
    print(f"{d:22} {ev:40} measured {best*1e3:8.2f} ms   calculus {best/calc:5.2f}   calculus refit {best/calcA:5.2f}   m7 x925 {best/m7x:5.2f}   m7 a725 {best/m7a:5.2f}")
rms = lambda v: math.sqrt(sum(math.log(x) ** 2 for x in v) / len(v))
print("rms log: calculus %.2f  refit %.2f  m7 x925 %.2f  m7 a725 %.2f" % tuple(rms([r[i] for r in rows]) for i in range(4)))

#!/usr/bin/env python3
"""The corpus against the clock (docs/evaluation.md): each program's predicted time, from
`neant cost --eval` with the roofline's fitted constants (cost-model § Time), against its measured
wall-clock at three input sizes, pinned to one big core.

    timing.py [--cpu 5] [--runs 3] [program ...]

Inputs are generated here, with a fixed seed, at sizes that span the cache: an argument, or a file
of integers or of CSV rows. The sizes the report is in — `n`, `steps`, `text.len()`,
`path.len()` — are handed to `--eval` from the input itself. `bfs` is left out: its cost is in
`max(start[_])`, a value read from the graph, which `--eval` cannot be given.
"""
import argparse, math, random, re, subprocess, sys, tempfile, time, pathlib

HERE = pathlib.Path(__file__).resolve().parent
NEANT = HERE / "../../bootstrap/target/release/neant"

def run(cmd, **kw):
    return subprocess.run(cmd, check=True, capture_output=True, text=True, **kw)

def ints_file(path, k, lo, hi):
    rnd = random.Random(k)
    path.write_text("".join(f"{rnd.randint(lo, hi)}\n" for _ in range(k)))
    return path.stat().st_size

def csv_file(path, k):
    rnd = random.Random(k)
    rows = ["sensor,time,value"] + [f"{rnd.randint(0, 7)},{10 * t},{rnd.randint(-100, 100)}" for t in range(k)]
    path.write_text("\n".join(rows) + "\n")
    return path.stat().st_size

# program: sizes, and for one size (dir) -> (argv, {name: value} for --eval)
def heat(n, d):  return [str(n), "50"], {"n": n, "steps": 50, "a0.len()": len(str(n)), "a1.len()": 2}
def matmul(n, d): return [str(n)], {"n": n, "a0.len()": len(str(n))}
def fir(k, d):
    p = d / "signal.txt"; size = ints_file(p, k, -100, 100)
    return [str(p)], {"n": k, "text.len()": size, "path.len()": len(str(p))}
def pid(k, d):
    p = d / "trajectory.txt"; size = ints_file(p, k, 0, 1000)
    return [str(p)], {"n": k, "text.len()": size, "path.len()": len(str(p))}
def csv(k, d):
    p = d / "readings.csv"; size = csv_file(p, k)
    return [str(p)], {"text.len()": size, "path.len()": len(str(p))}

PROGRAMS = {
    "heat":   ([100, 300, 900], heat),
    "matmul": ([100, 300, 600], matmul),
    "fir":    ([10_000, 100_000, 1_000_000], fir),
    "pid":    ([10_000, 100_000, 1_000_000], pid),
    "csv":    ([1_000, 10_000, 100_000], csv),
}

def predict_m7(nt, env):
    """M7's time for `main` (`neant cost --eval … --m7`, cost-model § M7's line), or None."""
    ev = ",".join(f"{k}={v}" for k, v in env.items()) + ",B=64"
    out = run([str(NEANT), "cost", nt.name, "--eval", ev, "--m7"], cwd=nt.parent).stdout
    sec = out.split("\nmain", 1)
    if len(sec) < 2: return None
    main = re.split(r"\n(?=\S)", sec[1], maxsplit=1)[0]
    m = re.search(r" m7 (\S+) s", main)
    return float(m.group(1)) if m else None

def predict(nt, env):
    ev = ",".join(f"{k}={v}" for k, v in env.items()) + ",B=64"
    out = run([str(NEANT), "cost", str(nt), "--eval", ev]).stdout
    lines = out.splitlines()
    start = next((i for i, l in enumerate(lines) if l.startswith("main")), None)
    for l in lines[(start or 0) + 1:]:
        m = re.search(r"at .*?: work (\S+)\s+moves (\S+) bytes.*?\s+time (\S+) s \((\w+)-bound\)", l)
        if m: return float(m.group(1)), float(m.group(2)), float(m.group(3)), m.group(4)
        if l and not l.startswith(" "): break
    return None

def wall(argv, cpu, runs):
    best = math.inf
    for _ in range(runs):
        t0 = time.perf_counter()
        subprocess.run(["taskset", "-c", str(cpu), *argv], check=True, capture_output=True)
        best = min(best, time.perf_counter() - t0)
    return best

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cpu", type=int, default=5)
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("programs", nargs="*")
    a = ap.parse_args()
    work = pathlib.Path(tempfile.mkdtemp(prefix="neant-corpus-timing-"))
    empty = work / "empty.nt"; empty.write_text("fn main() {\n    println(0);\n}\n")
    run([str(NEANT), "build", str(empty), "--unchecked", "-o", str(work / "empty")])
    base = wall([str(work / "empty")], a.cpu, a.runs)
    print(f"baseline {base*1e3:.2f} ms, cpu {a.cpu}")
    print(f"  {'program':<8} {'size':>9} {'pred work':>11} {'pred bytes':>11} {'bound':>6} {'pred ms':>10} {'meas ms':>10} {'meas/pred':>9} {'m7 ms':>9} {'meas/m7':>8}")
    ratios, ratios_m7 = [], []
    for name in a.programs or list(PROGRAMS):
        sizes, make = PROGRAMS[name]
        nt = HERE / name / "main.nt"
        binary = work / name
        run([str(NEANT), "build", str(nt), "--unchecked", "-o", str(binary)])
        for s in sizes:
            d = work / f"{name}_{s}"; d.mkdir()
            argv, env = make(s, d)
            p = predict(nt, env)
            t = wall([str(binary), *argv], a.cpu, a.runs) - base
            if p is None:
                print(f"  {name:<8} {s:>9} {'—':>11} {'—':>11} {'—':>6} {'—':>10} {t*1e3:>10.2f} {'—':>9}   (no prediction: a size the input does not name)")
                continue
            w, mv, pt, bound = p
            # a run shorter than the baseline's noise has no ratio worth averaging
            m7 = predict_m7(nt, env)
            if t > 0.5e-3:
                ratios.append(t / pt)
                if m7: ratios_m7.append(t / m7)
            m7s = f"{m7*1e3:>9.3f} {t/m7:>8.2f}" if m7 else f"{'—':>9} {'—':>8}"
            print(f"  {name:<8} {s:>9} {w:>11.3e} {mv:>11.3e} {bound:>6} {pt*1e3:>10.3f} {t*1e3:>10.2f} {t/pt:>9.2f} {m7s}")
    if ratios:
        g = math.exp(sum(math.log(x) for x in ratios) / len(ratios))
        print(f"\ngeometric mean measured/predicted over {len(ratios)} runs: {g:.2f}  (range {min(ratios):.3g} … {max(ratios):.3g})")
    if ratios_m7:
        g7 = math.exp(sum(math.log(x) for x in ratios_m7) / len(ratios_m7))
        print(f"geometric mean measured/m7 over {len(ratios_m7)} runs: {g7:.2f}  (range {min(ratios_m7):.3g} … {max(ratios_m7):.3g})")
    print(f"inputs and binaries in {work}")

if __name__ == "__main__":
    main()

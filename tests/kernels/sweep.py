#!/usr/bin/env python3
"""The M1 experiment: predicted bytes moved (neant cost) against measured L2 refills × line size
(perf stat), over a size sweep, on one pinned big core.

    sweep.py [--dry] [--cpu 5] [--runs 3] [kernel ...]

Prints one row per (kernel, n) and, per kernel, the log-log slope of predicted and measured
against n over the sizes past the cache. --dry prints predictions only and needs no perf.
"""
import argparse, math, os, re, subprocess, sys, tempfile, pathlib

HERE = pathlib.Path(__file__).resolve().parent
NEANT = HERE / "../../stage0/target/release/neant"
LINE = 64
EVENT = "l2d_cache_refill"

# sizes avoid powers of two: set-associativity conflicts are not in the model
SWEEP = {
    "sum":          ([50_000, 200_000, 800_000, 3_200_000, 12_800_000], 20),
    "dot":          ([50_000, 200_000, 800_000, 3_200_000, 12_800_000], 20),
    "saxpy":        ([50_000, 200_000, 800_000, 3_200_000, 12_800_000], 20),
    "transpose":    ([500, 1000, 2000, 3000, 4000], 5),
    # multiples of 64 for the tiles, with an odd cofactor: a row stride with a large power of
    # two in it maps a column onto a handful of cache sets, which the ideal-cache model cannot see
    "matmul_naive": ([448, 832, 1216, 1600, 1984], 1),
    "matmul_tiled": ([448, 832, 1216, 1600, 1984], 1),
}

def run(cmd, **kw):
    return subprocess.run(cmd, check=True, capture_output=True, text=True, **kw)

def predict(nt):
    out = run([str(NEANT), "cost", str(nt), "--eval", "B=%d" % LINE]).stdout
    m = re.search(r"^main\s+.*\n\s+at .*?: work (\S+)\s+moves (\S+) bytes", out, re.M)
    if not m:
        sys.exit("could not read prediction from:\n" + out)
    return float(m.group(1)), float(m.group(2))

def measure(binary, cpu, runs):
    best = None
    for _ in range(runs):
        p = subprocess.run(["taskset", "-c", str(cpu), "perf", "stat", "-x,", "-e",
                            f"{EVENT},l1d_cache_refill,ll_cache_miss_rd,instructions", str(binary)],
                           capture_output=True, text=True)
        if p.returncode != 0:
            sys.exit(p.stderr)
        # two PMUs on a big.LITTLE machine: the event shows up once per cluster as
        # armv8_pmuv3_N/event/u, and only the pinned cluster's copy is counted
        counts = {}
        for line in p.stderr.splitlines():
            parts = line.split(",")
            if len(parts) > 3 and parts[2]:
                try: v = int(parts[0])
                except ValueError: continue
                for ev in (EVENT, "l1d_cache_refill", "ll_cache_miss_rd", "instructions"):
                    if ev in parts[2]:
                        counts[ev] = counts.get(ev, 0) + v
        if EVENT not in counts:
            sys.exit("perf gave no %s:\n%s" % (EVENT, p.stderr))
        if best is None or counts[EVENT] < best[EVENT]:
            best = counts
    return best

def slope(xs, ys):
    if len(xs) < 2: return float("nan")
    lx, ly = [math.log(x) for x in xs], [math.log(y) for y in ys]
    mx, my = sum(lx)/len(lx), sum(ly)/len(ly)
    num = sum((a-mx)*(b-my) for a, b in zip(lx, ly)); den = sum((a-mx)**2 for a in lx)
    return num/den if den else float("nan")

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dry", action="store_true")
    ap.add_argument("--cpu", type=int, default=5)
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("kernels", nargs="*")
    a = ap.parse_args()
    if not NEANT.exists():
        sys.exit("build the compiler first: cargo build --release in stage0/")
    kernels = a.kernels or list(SWEEP)
    work = tempfile.mkdtemp(prefix="neant-sweep-")
    for k in kernels:
        sizes, reps = SWEEP[k]
        src = (HERE / f"{k}.nt.in").read_text()
        print(f"\n{k}   (repeats {reps}, event {EVENT} × {LINE}B, cpu {a.cpu})")
        print(f"  {'n':>10} {'pred work':>14} {'pred bytes':>14} {'meas bytes':>14} {'ratio':>7}  {'l1 refills':>12} {'ll misses':>12}")
        ns, preds, meas = [], [], []
        for n in sizes:
            nt = pathlib.Path(work) / f"{k}_{n}.nt"
            nt.write_text(src.replace("@N@", str(n)).replace("@R@", str(reps)))
            pw, pb = predict(nt)
            row = f"  {n:>10} {pw:>14.3e} {pb:>14.3e}"
            if a.dry:
                print(row); continue
            binary = nt.with_suffix("")
            run([str(NEANT), "build", str(nt), "--unchecked", "-o", str(binary)])
            c = measure(binary, a.cpu, a.runs)
            mb = c[EVENT] * LINE
            ns.append(n); preds.append(pb); meas.append(mb)
            print(row + f" {mb:>14.3e} {pb/mb if mb else float('inf'):>7.2f}  {c.get('l1d_cache_refill',0):>12} {c.get('ll_cache_miss_rd',0):>12}")
        if not a.dry and len(ns) >= 3:
            # slope over the larger half of the sweep, where the I/O model is meant to hold
            h = len(ns) // 2
            print(f"  slope (log-log, upper half): predicted {slope(ns[h:], preds[h:]):.2f}   measured {slope(ns[h:], meas[h:]):.2f}")
    print(f"\nsources and binaries in {work}")

if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""The span exit tests 3 and 4 (m5-span-design.md §7): wall-clock scaling of a `.par()` chain
against P, on a list of pinned cores, for a compute-bound kernel and a memory-bound one.

    par_sweep.py [--cores 5,6,7,8,9,15,16,17,18,19] [--runs 5] [kernel ...]

Prints, per kernel and P, the measured wall-clock time, the speedup over P=1, and the efficiency
against ideal (speedup / P) — the question is whether speedup tracks P (work/P holds) or flattens
(bandwidth-bound, per m5-span-design.md §6). --cores should list same-type cores in order: mixing
core types would let OpenMP's static, equal-iteration-count schedule turn a slower core into a
straggler, a real confound but not this experiment's question. On the development machine, the
default ten (5-9, 15-19) read the same part (Cortex-X925) at
`/sys/devices/system/cpu/cpu*/regs/identification/midr_el1` — one cluster, not two; check on a
different machine before assuming the same.
"""
import argparse, pathlib, subprocess, sys, tempfile, time

HERE = pathlib.Path(__file__).resolve().parent
NEANT = HERE / "../../bootstrap/target/release/neant"

# name: (N, R) — chosen so P=1 runs a second or two: long enough that process and thread-pool
# startup do not dominate, short enough for a sweep to finish in reasonable time
SWEEP = {
    "par_compute": (20_000_000, 40),
    "par_memory": (20_000_000, 150),
}

def build(kernel, n, r, work):
    src = (HERE / f"{kernel}.nt.in").read_text().replace("@N@", str(n)).replace("@R@", str(r))
    nt = work / f"{kernel}.nt"
    nt.write_text(src)
    binary = nt.with_suffix("")
    subprocess.run([str(NEANT), "build", str(nt), "--unchecked", "-o", str(binary)], check=True, capture_output=True, text=True)
    return binary

def timed(binary, cores, p, runs):
    best = None
    for _ in range(runs):
        t0 = time.perf_counter()
        proc = subprocess.run(["taskset", "-c", cores, str(binary)],
                               capture_output=True, text=True,
                               env={"OMP_NUM_THREADS": str(p), "PATH": "/usr/bin:/bin"})
        dt = time.perf_counter() - t0
        if proc.returncode != 0:
            sys.exit(f"{binary} failed under P={p}: {proc.stderr}")
        if best is None or dt < best:
            best = dt
    return best

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cores", default="5,6,7,8,9,15,16,17,18,19", help="comma-separated CPU ids, same type, most-pinned first")
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("kernels", nargs="*")
    a = ap.parse_args()
    core_list = a.cores.split(",")
    ps = list(range(1, len(core_list) + 1))
    if not NEANT.exists():
        sys.exit("build the compiler first: cargo build --release in bootstrap/")
    kernels = a.kernels or list(SWEEP)
    work = pathlib.Path(tempfile.mkdtemp(prefix="neant-par-sweep-"))
    for k in kernels:
        n, r = SWEEP[k]
        binary = build(k, n, r, work)
        print(f"\n{k}   (n={n}, r={r}, {a.runs} runs each, cores {a.cores})")
        print(f"  {'P':>3} {'time (s)':>10} {'speedup':>9} {'ideal':>7} {'efficiency':>11}")
        t1 = None
        for p in ps:
            cores = ",".join(core_list[:p])
            t = timed(binary, cores, p, a.runs)
            if t1 is None:
                t1 = t
            speedup = t1 / t
            print(f"  {p:>3} {t:>10.4f} {speedup:>9.2f} {p:>7} {speedup / p:>10.1%}")
    print(f"\nsources and binaries in {work}")

if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""The roofline term (plan § Stage D, decisions §7): predicted time `max(span·τ, work·τ/P, moves/BW)`
against measured wall-clock, on one pinned big core.

    roofline.py fit       [--cpu 5] [--runs 5]        fit τ (horner) and BW (sum), print them
    roofline.py check     [--cpu 5] [--runs 5] [--tau NS] [--bw GBPS] [kernel ...]

`fit` times a compute-bound kernel that stays in L1 (τ = time / work) and a streaming one far past
the last cache (BW = moves / time), and subtracts the time of a program that does nothing, so
process start and the page faults of the allocation are not charged to either. `check` predicts
every other kernel's time from `neant cost --eval` with the fitted constants and prints measured
over predicted. Sizes avoid powers of two, as in sweep.py.
"""
import argparse, math, re, subprocess, sys, tempfile, time, pathlib

HERE = pathlib.Path(__file__).resolve().parent
NEANT = HERE / "../../bootstrap/target/release/neant"

FIT = {
    "horner": (3_000, 20_000),        # 24 KB, in L1; 6·10⁷ evaluations
    "sum":    (12_800_000, 20),       # 100 MB, far past L3
    "arena":  (4_194_304, 3),         # 64 MB of nodes in a random cycle: one dependent miss a step
}
CHECK = {
    "sum":          ([200_000, 800_000, 3_200_000, 12_800_000], 20),
    "dot":          ([200_000, 800_000, 3_200_000, 12_800_000], 20),
    "saxpy":        ([200_000, 800_000, 3_200_000, 12_800_000], 20),
    "horner":       ([1_000, 30_000, 3_000_000], None),
    "transpose":    ([1000, 2000, 3000], 5),
    "matmul_naive": ([448, 832, 1216], 1),
    "matmul_tiled": ([448, 832, 1216], 1),
    "struct_aos":   ([200_000, 3_200_000], 5),
    "struct_soa":   ([200_000, 3_200_000], 5),
    "arena":        ([65_536, 1_048_576, 4_194_304], 3),
}

def run(cmd, **kw):
    return subprocess.run(cmd, check=True, capture_output=True, text=True, **kw)

def predict(nt, extra=()):
    """work, moves and predicted seconds of `main` at the kernel's sizes (all constants in the source)."""
    out = run([str(NEANT), "cost", str(nt), "--eval", "B=64", *extra]).stdout
    lines = out.splitlines()
    start = next((i for i, l in enumerate(lines) if l.startswith("main")), None)
    for l in lines[(start or 0) + 1:]:
        m = re.search(r"at .*?: work (\S+)\s+moves (\S+) bytes(?:\s+chase (\S+) bytes)?(?:\s+time (\S+) s)?", l)
        if m: return float(m.group(1)), float(m.group(2)), (float(m.group(4)) if m.group(4) else None), float(m.group(3) or 0)
        if l and not l.startswith(" "): break
    sys.exit("could not read prediction from:\n" + out)

def wall(binary, cpu, runs):
    best = math.inf
    for _ in range(runs):
        t0 = time.perf_counter()
        subprocess.run(["taskset", "-c", str(cpu), str(binary)], check=True, capture_output=True)
        best = min(best, time.perf_counter() - t0)
    return best

def build(src, name, n, reps, work):
    nt = pathlib.Path(work) / f"{name}_{n}.nt"
    text = src.replace("@N@", str(n))
    if reps is not None: text = text.replace("@R@", str(reps))
    nt.write_text(text)
    binary = nt.with_suffix("")
    run([str(NEANT), "build", str(nt), "--unchecked", "-o", str(binary)])
    return nt, binary

def baseline(cpu, runs, work):
    nt = pathlib.Path(work) / "empty.nt"
    nt.write_text("fn main() {\n    println(0);\n}\n")
    binary = nt.with_suffix("")
    run([str(NEANT), "build", str(nt), "--unchecked", "-o", str(binary)])
    return wall(binary, cpu, runs)

def reps_for(name, n, given):
    if given is not None: return given
    # horner: keep the total near 6·10⁷ evaluations whatever n is
    return max(1, 60_000_000 // n)

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("mode", choices=["fit", "check"])
    ap.add_argument("--cpu", type=int, default=5)
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--tau", type=float)
    ap.add_argument("--bw", type=float)
    ap.add_argument("--lat", type=float)
    ap.add_argument("kernels", nargs="*")
    a = ap.parse_args()
    if not NEANT.exists():
        sys.exit("build the compiler first: cargo build --release in bootstrap/")
    work = tempfile.mkdtemp(prefix="neant-roofline-")
    base = baseline(a.cpu, a.runs, work)
    print(f"baseline (an empty program): {base*1e3:.2f} ms, cpu {a.cpu}")
    if a.mode == "fit":
        n, r = FIT["horner"]
        nt, b = build((HERE / "horner.nt.in").read_text(), "horner", n, r, work)
        w, mv, _, _ = predict(nt)
        t = wall(b, a.cpu, a.runs) - base
        tau = t / w * 1e9
        print(f"horner n={n} R={r}: work {w:.3e}  moves {mv:.3e}  time {t*1e3:.1f} ms  →  τ = {tau:.4f} ns per work unit")
        n, r = FIT["sum"]
        nt, b = build((HERE / "sum.nt.in").read_text(), "sum", n, r, work)
        w, mv, _, _ = predict(nt)
        t = wall(b, a.cpu, a.runs) - base
        bw = mv / t / 1e9
        print(f"sum n={n} R={r}: work {w:.3e}  moves {mv:.3e}  time {t*1e3:.1f} ms  →  BW = {bw:.2f} GB/s"
              f"  (work·τ would be {w*tau/1e6:.1f} ms)")
        n, r = FIT["arena"]
        nt, b = build((HERE / "arena.nt.in").read_text(), "arena", n, r, work)
        w, mv, _, ch = predict(nt)
        t = wall(b, a.cpu, a.runs) - base
        lat = (t - (mv - ch) / (bw * 1e9)) / (ch / 64) * 1e9
        print(f"arena n={n} R={r}: moves {mv:.3e}  of them a chase {ch:.3e}  time {t*1e3:.1f} ms  →  L = {lat:.1f} ns a chased line")
        print(f"\nneant cost --tau {tau:.4f} --bw {bw:.2f} --lat {lat:.1f}")
        return
    extra = []
    if a.tau is not None: extra += ["--tau", str(a.tau)]
    if a.bw is not None: extra += ["--bw", str(a.bw)]
    if a.lat is not None: extra += ["--lat", str(a.lat)]
    print(f"  {'kernel':<14} {'n':>10} {'pred work':>11} {'pred bytes':>11} {'pred ms':>9} {'meas ms':>9} {'meas/pred':>9}")
    ratios = []
    for k in a.kernels or list(CHECK):
        sizes, reps = CHECK[k]
        src = (HERE / f"{k}.nt.in").read_text()
        for n in sizes:
            r = reps_for(k, n, reps)
            nt, b = build(src, k, n, r, work)
            w, mv, pt, _ = predict(nt, extra)
            if pt is None: sys.exit("neant cost printed no time: rebuild the compiler with the roofline term")
            t = wall(b, a.cpu, a.runs) - base
            ratios.append(t / pt)
            print(f"  {k:<14} {n:>10} {w:>11.3e} {mv:>11.3e} {pt*1e3:>9.2f} {t*1e3:>9.2f} {t/pt:>9.2f}")
    g = math.exp(sum(math.log(x) for x in ratios) / len(ratios))
    print(f"\ngeometric mean of measured/predicted over {len(ratios)} runs: {g:.2f}"
          f"   (range {min(ratios):.2f} … {max(ratios):.2f})")
    print(f"sources and binaries in {work}")

if __name__ == "__main__":
    main()

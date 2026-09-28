#!/usr/bin/env python3
"""M7's first slice (plan § M7): a program's compute time from llvm-mca and the calculus's laps.

    m7.py main.nt 'n=2000,a0.len()=4' [--cpu 5] [--own [--entries]] [--forward] [-- args...]

Each innermost loop of `gcc -O2 -g -S` output of `neant emit --lines` is the loop of the calculus at
the file and line its branch back names; llvm-mca (`cortex-x4`) gives its cycles an iteration, an
iteration is two laps where the loop works on two-lane vectors (`.2d`), four on `.4s`; `neant cost
--eval … --laps` gives the laps of that loop. Compute is Σ laps · cycles a lap · 0.257 ns (this
core's cycle); the prediction is the longer of it and the calculus's memory term. With arguments
after `--`, the binary is timed too (pinned, best of three). `--forward` writes a multiply-add
into its own addend as a multiply and an add first (accumulator forwarding, which llvm-mca 18
lacks), where the addend is carried from the last lap: spectral-norm 0.53 → 0.70 of it, n-body
0.65 (experiments.md, M7). `--own` uses this core's model below instead of llvm-mca's: a lap is
the longer of the chain it waits on (dependences only, latencies measured here) and the pipes' share;
`--entries` also charges each entry into a loop its first lap's critical path, before laps overlap.
"""
import re, subprocess, sys, tempfile, pathlib, time
HERE = pathlib.Path(__file__).resolve().parent
NEANT = HERE / "../../bootstrap/target/release/neant"
CYCLE = 0.257  # ns, this core (docs/experiments.md, M7's first probe)
ENTRIES = "--entries" in sys.argv
if ENTRIES: sys.argv.remove("--entries")
OWN = "--own" in sys.argv
if OWN: sys.argv.remove("--own")
FORWARD = "--forward" in sys.argv
if FORWARD: sys.argv.remove("--forward")

def forward_accumulators(body):
    """A multiply-add accumulating into its own addend, `fmadd dX, a, b, dX`, that nothing earlier in
    the lap writes — so `dX` is carried from the last lap — waits on the chain
    through `dX` only for the add: Arm cores forward the accumulator late, two cycles where a
    multiplicand waits four, which llvm-mca 18 does not model. Written as a multiply into a scratch
    register and an add, the chain is the add's (experiments.md, M7)."""
    out = []
    written = set()
    for l in body:
        m = re.match(r"^(\s+)f(n?)m(add|sub)\s+(d\d+),\s*(d\d+),\s*(d\d+),\s*(d\d+)\s*$", l)
        # carried from the last lap only if nothing earlier in this one wrote the addend
        carried = m is not None and m.group(4) not in written
        dest = re.match(r"^\s+\w[\w.]*\s+([dvxwsq]\d+)", l)
        if dest: written.add(dest.group(1))
        if m and carried and m.group(4) == m.group(7) and m.group(2) == "":
            ind, _, op, d, a, b, _ = m.groups()
            out.append(f"{ind}fmul\td31, {a}, {b}")
            out.append(f"{ind}f{'add' if op == 'add' else 'sub'}\t{d}, {d}, d31")
        else:
            out.append(l)
    return out

# ---- a model of this core (Cortex-X925, measured by tests/kernels/chains.c and tp.c) ----
# latency in cycles; a multiply-add's addend is forwarded late, two cycles where a multiplicand waits four
LAT = {"fadd": 2, "fsub": 2, "fmul": 3, "fnmul": 3, "fmadd": 4, "fmsub": 4, "fnmadd": 4, "fnmsub": 4,
       "fdiv": 13, "fsqrt": 13, "fabs": 1, "fneg": 1, "fmov": 1, "fcvt": 3, "scvtf": 3, "ucvtf": 3,
       "fcvtzs": 3, "fcmp": 2, "fcsel": 2, "fmax": 2, "fmin": 2, "fmla": 4, "fmls": 4, "faddp": 3,
       "mul": 3, "madd": 3, "msub": 3, "smulh": 3, "umulh": 3, "sdiv": 12, "udiv": 12,
       "ldr": 4, "ldp": 4, "ldur": 4, "ld1": 5, "dup": 3, "ins": 3, "movi": 1}
ADDEND = {"fmadd": 3, "fmsub": 3, "fnmadd": 3, "fnmsub": 3, "fmla": 0, "fmls": 0}   # which operand is the addend
PIPES = {"fp": 4, "div": 0.6, "load": 3, "store": 2, "alu": 6, "branch": 2, "all": 8}   # per cycle

def reg(tok):
    m = re.match(r"^\[?([xwdsqvhb])(\d+)", tok)
    if m: return ("v" if m.group(1) in "dsqvhb" else "x") + m.group(2)
    return "sp" if tok.startswith("sp") or tok.startswith("[sp") else None

def parse(line):
    t = line.strip().split(None, 1)
    op = t[0]
    ops = [o.strip() for o in re.split(r",(?![^\[]*\])", t[1])] if len(t) > 1 else []
    base = op.split(".")[0]
    store = base.startswith("st")
    flags_out = base in ("cmp", "cmn", "tst", "fcmp", "subs", "adds", "ands")
    dests, srcs = [], []
    for k, o in enumerate(ops):
        for part in re.findall(r"[xwdsqvhb]\d+|sp", o):
            r = reg(part)
            if r is None: continue
            if k == 0 and not store and not base.startswith(("cmp", "cmn", "tst", "fcmp", "b", "cb", "tb")) and "[" not in o:
                dests.append(r)
            else:
                srcs.append((k, r))
    if base.startswith(("ldp",)) and len(ops) > 1 and "[" not in ops[1]:
        dests.append(reg(ops[1]))
    if base.startswith(("b.", "bne", "beq", "bgt", "blt", "bge", "ble", "bhi", "bls", "bcc", "bcs", "bmi", "bpl")) or base in ("csel", "csinc", "cset", "fcsel"):
        srcs.append((-1, "nzcv"))
    if flags_out: dests.append("nzcv")
    kind = ("div" if base in ("fdiv", "fsqrt", "sdiv", "udiv") else "fp" if base.startswith("f") or base in ("scvtf", "ucvtf", "dup", "ins", "movi") or "." in op and base not in ("b",)
            else "load" if base.startswith("ld") else "store" if store else "branch" if base.startswith(("b", "cb", "tb")) else "alu")
    return base, dests, srcs, kind

def own_cycles(body):
    ins = [parse(l) for l in body if l.strip() and not l.strip().startswith("//")]
    # resources: each pipe's share of a lap
    counts = {}
    for _, _, _, kind in ins: counts[kind] = counts.get(kind, 0) + 1
    res = max([counts.get(k, 0) / w for k, w in PIPES.items() if k != "all" and w > 0] + [len(ins) / PIPES["all"]])
    # the chain a lap waits on: dependences only, sixty laps, the slope of the last thirty
    ready, finish = {}, []
    K = 60
    for it in range(K):
        last = 0
        for base, dests, srcs, kind in ins:
            lat = LAT.get(base, 1)
            start = 0.0
            done = 0.0
            for k, r in srcs:
                t = ready.get(r, 0.0)
                if base in ADDEND and k == ADDEND[base]:
                    done = max(done, t + 2)          # the addend joins at the end
                else:
                    start = max(start, t)
            end = max(start + lat, done)
            for r in dests: ready[r] = end
            last = max(last, end)
        finish.append(last)
    rec = (finish[-1] - finish[K // 2]) / (K - 1 - K // 2)
    # a lap's own critical path from a cold start: what an entry pays before laps overlap
    return max(rec, res), finish[0]

def loops(asm):
    lines = asm.splitlines()
    files = {m.group(1): m.group(2) for l in lines for m in [re.match(r'^\s+\.file\s+(\d+)\s+"([^"]+)"', l)] if m}
    labels = {m.group(1): i for i, l in enumerate(lines) for m in [re.match(r"^(\.L\w+):", l)] if m}
    found = []
    for j, l in enumerate(lines):
        m = re.match(r"^\s+(b\w*|b\.\w+|cbn?z|tbn?z)\s+(?:.*,\s*)?(\.L\w+)\s*$", l)
        if m and m.group(2) in labels and labels[m.group(2)] < j: found.append((labels[m.group(2)], j))
    inner = [(a, b) for (a, b) in found if not any(a < c and d <= b and (c, d) != (a, b) for (c, d) in found)]
    out = []
    for a, b in inner:
        body = [l for l in lines[a + 1:b + 1] if l.strip() and not l.strip().startswith(".") and not re.match(r"^\.L\w+:", l)]
        locs = [(files.get(m.group(1), "?"), int(m.group(2))) for l in lines[a:b + 1] for m in [re.match(r"^\s+\.loc\s+(\d+)\s+(\d+)", l)] if m and int(m.group(2)) > 0]
        if not body or not locs or len(body) > 200: continue
        if FORWARD: body = forward_accumulators(body)
        r = subprocess.run(["llvm-mca-18", "-mcpu=cortex-x4", "-iterations=1000"], input="\n".join(body) + "\n", capture_output=True, text=True)
        m = re.search(r"Total Cycles:\s+(\d+)", r.stdout)
        if not m: continue
        if OWN: cyc_own, crit = own_cycles(body)
        else: crit = 0.0
        text = "\n".join(body)
        lanes = 4 if ".4s" in text else 2 if ".2d" in text else 1
        out.append((f"{pathlib.Path(locs[-1][0]).name}:{locs[-1][1]}", cyc_own if OWN else int(m.group(1)) / 1000, lanes, crit))
    return out

def main():
    argv = sys.argv[1:]
    run_args = argv[argv.index("--") + 1:] if "--" in argv else None
    if run_args is not None: argv = argv[:argv.index("--")]
    cpu = "5"
    if "--cpu" in argv: cpu = argv[argv.index("--cpu") + 1]; argv = [a for a in argv if a not in ("--cpu", cpu)]
    nt, ev = pathlib.Path(argv[0]).resolve(), argv[1]
    work = pathlib.Path(tempfile.mkdtemp(prefix="neant-m7-"))
    c = subprocess.run([str(NEANT), "emit", "--lines", nt.name], cwd=nt.parent, capture_output=True, text=True, check=True).stdout
    (work / "p.c").write_text(c)
    subprocess.run(["gcc", "-O2", "-g", "-std=gnu11", "-w", "-S", "-o", str(work / "p.s"), str(work / "p.c")], check=True)
    ls = loops((work / "p.s").read_text())
    rep = subprocess.run([str(NEANT), "cost", nt.name, "--eval", ev + ",B=64", "--laps"], cwd=nt.parent, capture_output=True, text=True).stdout
    sec = rep.split("\nmain", 1)[1]
    # main's section: its lines up to the next function's
    sec = re.split(r"\n(?=\S)", sec, maxsplit=1)[0]
    laps = {m.group(1): (float(m.group(2)), float(m.group(3))) for m in re.finditer(r"laps (\S+) (\S+) entries (\S+)", sec)}
    tm = re.search(r"time (\S+) s \((\w+)-bound\)", sec)
    mv = re.search(r"moves (\S+) bytes", sec)
    total = 0.0
    print(f"  {'loop':<22} {'laps':>12} {'cycles/it':>9} {'lanes':>5} {'ms':>9}")
    seen = set()
    for at, cyc, lanes, crit in ls:
        if at not in laps or at in seen: continue
        n, entries = laps[at]
        seen.add(at)
        # each entry pays its first lap's critical path before the laps overlap
        ms = (n * cyc / lanes + (entries * max(0.0, crit - cyc) if ENTRIES else 0.0)) * CYCLE * 1e-6
        total += ms
        print(f"  {at:<22} {n:>12.3e} {cyc:>9.2f} {lanes:>5} {ms:>9.2f}")
    print(f"compute from llvm-mca {total:.2f} ms; the calculus's time {float(tm.group(1))*1e3:.2f} ms ({tm.group(2)}-bound)")
    if run_args is not None:
        b = work / "p"
        subprocess.run([str(NEANT), "build", nt.name, "--unchecked", "-o", str(b)], cwd=nt.parent, check=True, capture_output=True)
        best = 1e9
        for _ in range(3):
            t0 = time.perf_counter(); subprocess.run(["taskset", "-c", cpu, str(b), *run_args], capture_output=True); best = min(best, time.perf_counter() - t0)
        print(f"measured {best*1e3:.2f} ms: {best*1e3/total:.2f} of llvm-mca's, {best/float(tm.group(1)):.2f} of the calculus's")

if __name__ == "__main__":
    main()

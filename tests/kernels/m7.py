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
WINDOWED = "--window" in sys.argv
if WINDOWED: sys.argv.remove("--window")
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
       "fdiv": 13, "fsqrt": 14, "fabs": 1, "fneg": 1, "fmov": 1, "fcvt": 3, "scvtf": 3, "ucvtf": 3,
       "fcvtzs": 3, "fcmp": 2, "fcsel": 2, "fmax": 2, "fmin": 2, "fmla": 4, "fmls": 4, "faddp": 3,
       "mul": 3, "madd": 3, "msub": 3, "smulh": 3, "umulh": 3, "sdiv": 12, "udiv": 12,
       "ldr": 4, "ldp": 4, "ldur": 4, "ld1": 5, "dup": 3, "ins": 3, "movi": 1}
ADDEND = {"fmadd": 3, "fmsub": 3, "fnmadd": 3, "fnmsub": 3, "fmla": 0, "fmls": 0}   # which operand is the addend
PIPES = {"fp": 4, "div": 1, "load": 3, "store": 2, "alu": 6, "branch": 2, "all": 8}   # per cycle

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
    # a vector multiply-add accumulates into its destination, which is its addend too
    if base in ("fmla", "fmls", "mla", "mls") and dests: srcs.append((0, dests[0]))
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

WINDOW = 600   # instructions in flight (the reorder buffer, about)
MISS = 13.5    # cycles a mispredicted loop exit costs, where the trip varies entry to entry (br.c)

def window_cycles(trace, iters=40):
    """Cycles an iteration of `trace` takes in the steady state, with a window: an instruction is
    dispatched eight a cycle, not before the one `WINDOW` earlier has finished, and starts when its
    sources are ready and its pipe has a slot that cycle."""
    ins = [parse(l) for l in trace if l.strip()]
    ready, used = {}, {}
    fin, marks = [], []
    disp = 0.0
    for it in range(iters):
        for base, dests, srcs, kind in ins:
            n = len(fin)
            gate = fin[n - WINDOW] if n >= WINDOW else 0.0
            disp = max(disp + 1.0 / PIPES["all"], gate)
            start, done = disp, 0.0
            for k, r in srcs:
                t = ready.get(r, 0.0)
                if base in ADDEND and k == ADDEND[base]: done = max(done, t + 2)
                else: start = max(start, t)
            cap = PIPES.get(kind, 6)
            c = int(start)
            while used.get((kind, c), 0) >= max(cap, 1) and cap >= 1: c += 1
            if cap < 1:   # a pipe slower than one a cycle: one every 1/cap cycles
                c = max(c, int(used.get((kind, "next"), 0)))
                used[(kind, "next")] = c + 1 / cap
            else:
                used[(kind, c)] = used.get((kind, c), 0) + 1
            end = max(max(start, c) + LAT.get(base, 1), done)
            for r in dests: ready[r] = end
            fin.append(end)
        marks.append(max(fin[-len(ins):]))
    return (marks[-1] - marks[iters // 2]) / (iters - 1 - iters // 2)

def loops(asm):
    lines = asm.splitlines()
    files = {m.group(1): m.group(2) for l in lines for m in [re.match(r'^\s+\.file\s+(\d+)\s+"([^"]+)"', l)] if m}
    labels = {m.group(1): i for i, l in enumerate(lines) for m in [re.match(r"^(\.L\w+):", l)] if m}
    found = []
    for j, l in enumerate(lines):
        m = re.match(r"^\s+(b\w*|b\.\w+|cbn?z|tbn?z)\s+(?:.*,\s*)?(\.L\w+)\s*$", l)
        if m and m.group(2) in labels and labels[m.group(2)] < j: found.append((labels[m.group(2)], j))
    inner = [(a, b) for (a, b) in found if not any(a < c and d <= b and (c, d) != (a, b) for (c, d) in found)]
    def instrs(x, y): return [l for l in lines[x:y] if l.strip() and not l.strip().startswith(".") and not re.match(r"^\.L\w+:", l)]
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
        # the smallest loop around it, as code before the inner loop, the inner body, and after
        par = min([(c, d) for (c, d) in found if c < a and b < d], key=lambda cd: cd[1] - cd[0], default=None)
        nest = (instrs(par[0] + 1, a + 1), body, instrs(b + 1, par[1] + 1)) if par else None
        out.append((f"{pathlib.Path(locs[-1][0]).name}:{locs[-1][1]}", cyc_own if OWN else int(m.group(1)) / 1000, lanes, crit, nest))
    return out

def main():
    argv = sys.argv[1:]
    run_args = argv[argv.index("--") + 1:] if "--" in argv else None
    if run_args is not None: argv = argv[:argv.index("--")]
    cpu = "5"
    if "--cpu" in argv: cpu = argv[argv.index("--cpu") + 1]; argv = [a for a in argv if a not in ("--cpu", cpu)]
    nt, ev = pathlib.Path(argv[0]).resolve(), argv[1]
    work = pathlib.Path(tempfile.mkdtemp(prefix="neant-m7-"))
    c = subprocess.run([str(NEANT), "emit", "--lines", "--unchecked", nt.name], cwd=nt.parent, capture_output=True, text=True, check=True).stdout
    (work / "p.c").write_text(c)
    subprocess.run(["gcc", "-O2", "-g", "-std=gnu11", "-w", "-S", "-o", str(work / "p.s"), str(work / "p.c")], check=True)
    ls = loops((work / "p.s").read_text())
    rep = subprocess.run([str(NEANT), "cost", nt.name, "--eval", ev + ",B=64", "--laps"], cwd=nt.parent, capture_output=True, text=True).stdout
    sec = rep.split("\nmain", 1)[1]
    # main's section: its lines up to the next function's
    sec = re.split(r"\n(?=\S)", sec, maxsplit=1)[0]
    # by the loop's file name and line, as the assembly's `.file` has it
    laps = {}
    for m in re.finditer(r"laps (\S+):(\d+) (\S+) entries (\S+)( varies)?", sec):
        k = f"{pathlib.Path(m.group(1)).name}:{m.group(2)}"
        n, e, v = laps.get(k, (0.0, 0.0, False))
        laps[k] = (n + float(m.group(3)), e + float(m.group(4)), v or bool(m.group(5)))
    mem = re.search(r"memory (\S+) s", sec)
    mem_ms = float(mem.group(1)) * 1e3 if mem else 0.0
    tm = re.search(r"time (\S+) s \((\w+)-bound\)", sec)
    mv = re.search(r"moves (\S+) bytes", sec)
    total = 0.0
    print(f"  {'loop':<22} {'laps':>12} {'cycles/it':>9} {'lanes':>5} {'ms':>9}")
    seen = set()
    # of the assembly loops one source loop became — a vector loop and its scalar remainder — the
    # widest does the laps
    best = {}
    for x in ls:
        if x[0] not in best or x[2] > best[x[0]][2]: best[x[0]] = x
    for at, cyc, lanes, crit, nest in best.values():
        if at not in laps or at in seen: continue
        n, entries, varies = laps[at]
        seen.add(at)
        trip = n / entries if entries else 0
        if OWN and WINDOWED and nest and 0 < trip <= 16:
            # a short loop: its entries overlap as far as the window lets them — one iteration of
            # the loop around it, with this one's laps laid out in it, simulated
            pre, bodyl, post = nest
            per_entry = window_cycles(pre + bodyl * max(1, round(trip)) + post)
            ms = entries * (per_entry + (MISS if varies else 0.0)) * CYCLE * 1e-6
        else:
            # each entry pays its first lap's critical path before the laps overlap
            ms = (n * cyc / lanes + (entries * max(0.0, crit - cyc) if ENTRIES else 0.0) + (entries * MISS if OWN and varies else 0.0)) * CYCLE * 1e-6
        total += ms
        print(f"  {at:<22} {n:>12.3e} {cyc:>9.2f} {lanes:>5} {ms:>9.2f}")
    model = "this core's model" if OWN else "llvm-mca"
    print(f"compute from {model} {total:.2f} ms, memory from the calculus {mem_ms:.2f} ms; the calculus's time {float(tm.group(1))*1e3:.2f} ms ({tm.group(2)}-bound)")
    total = max(total, mem_ms)
    if run_args is not None:
        b = work / "p"
        subprocess.run([str(NEANT), "build", nt.name, "--unchecked", "-o", str(b)], cwd=nt.parent, check=True, capture_output=True)
        best = 1e9
        for _ in range(3):
            t0 = time.perf_counter(); subprocess.run(["taskset", "-c", cpu, str(b), *run_args], capture_output=True); best = min(best, time.perf_counter() - t0)
        print(f"measured {best*1e3:.2f} ms: {best*1e3/total:.2f} of the longer of the two, {best/float(tm.group(1)):.2f} of the calculus's")

if __name__ == "__main__":
    main()

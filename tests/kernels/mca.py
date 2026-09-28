#!/usr/bin/env python3
"""M7's first probe (plan § M7, experiments.md): the innermost loops of a `gcc -O2 -S` file — a
label and a later branch back to it, no other loop inside — each run through llvm-mca for its
cycles an iteration, to set against a kernel's measured nanoseconds a lap.

    mca.py file.s [cpu] [-v]      cpu: an llvm-mca model, cortex-x4 by default; -v prints bodies

With `neant emit --lines f.nt > f.c; gcc -O2 -g -S f.c`, each loop also names the `.nt` lines its
instructions came from — the loop of the calculus it is (plan § M7).
"""
import re, subprocess, sys
lines = open([a for a in sys.argv[1:] if a != "-v"][0]).read().splitlines()
args = [a for a in sys.argv[1:] if a != "-v"]
cpu = args[1] if len(args) > 1 else "cortex-x4"
# `.file N "name"`: the file a `.loc N line` names
files = {m.group(1): m.group(2) for l in lines for m in [re.match(r'^\s+\.file\s+(\d+)\s+"([^"]+)"', l)] if m}
labels = {}
for i, l in enumerate(lines):
    m = re.match(r"^(\.L\w+):", l)
    if m: labels[m.group(1)] = i
loops = []
for j, l in enumerate(lines):
    m = re.match(r"^\s+(b\w*|b\.\w+|cbn?z|tbn?z)\s+(?:.*,\s*)?(\.L\w+)\s*$", l)
    if m and m.group(2) in labels and labels[m.group(2)] < j:
        loops.append((labels[m.group(2)], j))
inner = [(a, b) for (a, b) in loops if not any(a < c and d <= b and (c, d) != (a, b) for (c, d) in loops)]
for a, b in inner:
    body = [l for l in lines[a + 1:b + 1] if l.strip() and not l.strip().startswith(".") and not re.match(r"^\.L\w+:", l)]
    # with `neant emit --lines` and `gcc -g`, the `.nt` lines the loop's instructions came from
    # the loop's own line: the `.loc` of the branch back, its control, which is the loop header's
    locs = [f"{files.get(m.group(1), '?')}:{m.group(2)}" for l in lines[a:b + 1] for m in [re.match(r"^\s+\.loc\s+(\d+)\s+(\d+)", l)] if m and int(m.group(2)) > 0][-1:]
    if not body or len(body) > 200: continue
    r = subprocess.run(["llvm-mca-18", f"-mcpu={cpu}", "-iterations=1000"], input="\n".join(body) + "\n", capture_output=True, text=True)
    m = re.search(r"Total Cycles:\s+(\d+)", r.stdout)
    rt = re.search(r"Block RThroughput:\s+([\d.]+)", r.stdout)
    cyc = int(m.group(1)) / 1000 if m else None
    src = f", at {locs[0]}" if locs else ""
    print(f"loop at line {a+1}-{b+1}: {len(body)} instructions, {cyc} cycles/iteration, RThroughput {rt.group(1) if rt else '?'}{src}")
    if "-v" in sys.argv: print("\n".join(body))

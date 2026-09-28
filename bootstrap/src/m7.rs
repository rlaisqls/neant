//! **M7's cost line** (plan § M7; docs/experiments.md, a model of this core): a loop's cycles read
//! off the assembly the C compiler made of it, on a model of the core measured by
//! `tests/kernels/{chains,tp,dv,br,l2}.c`, times the laps and entries the calculus gives it.
//! Nothing here is proved: it is the compiler's estimate of a constant factor, measured once per
//! build, beside the calculus's cost and never in it. `tests/kernels/m7.py` is the same model in
//! Python, and the two are kept alike.

use std::collections::HashMap;

/// A core: its cycle, its window, what a missed loop exit costs, and its pipes a cycle. The
/// latencies are the same on both of this machine's kinds (tests/kernels/*.c on CPU 5 and CPU 0).
#[derive(Clone, Copy)]
pub struct Core {
    /// one cycle, in nanoseconds
    pub cycle_ns: f64,
    /// instructions in flight: none is dispatched before the one this many earlier has finished
    window: usize,
    /// cycles a mispredicted loop exit costs, where the trip varies from one entry to the next
    miss: f64,
    dispatch: f64,
    fp: f64, div: f64, load: f64, store: f64, alu: f64, branch: f64,
}

/// The Cortex-X925 at 3.9 GHz (CPU 5): measured, the window about.
pub const X925: Core = Core { cycle_ns: 0.257, window: 600, miss: 13.5, dispatch: 8.0, fp: 4.0, div: 1.0, load: 3.0, store: 2.0, alu: 6.0, branch: 2.0 };
/// The Cortex-A725 at 2.8 GHz (CPU 0): the floating-point pipes, the divider and the missed exit
/// measured; the dispatch width, the integer, load and store pipes and the window as Arm publishes them.
pub const A725: Core = Core { cycle_ns: 0.357, window: 160, miss: 12.7, dispatch: 5.0, fp: 2.0, div: 1.0, load: 2.0, store: 1.0, alu: 4.0, branch: 1.0 };

/// Latency in cycles. A multiply-add's addend joins late: two cycles, where a multiplicand waits four.
fn latency(op: &str) -> f64 {
    match op {
        "fadd" | "fsub" | "fcmp" | "fcsel" | "fmax" | "fmin" => 2.0,
        "fmul" | "fnmul" | "fcvt" | "scvtf" | "ucvtf" | "fcvtzs" | "faddp" | "dup" | "ins" | "mul" | "madd" | "msub" | "smulh" | "umulh" => 3.0,
        "fmadd" | "fmsub" | "fnmadd" | "fnmsub" | "fmla" | "fmls" | "ldr" | "ldp" | "ldur" => 4.0,
        "ld1" | "ld1r" => 5.0,
        "sdiv" | "udiv" => 12.0,
        "fdiv" => 13.0,
        "fsqrt" => 14.0,
        _ => 1.0,
    }
}

/// The operand that is a multiply-add's addend, which joins at the end.
fn addend(op: &str) -> Option<usize> {
    match op {
        "fmadd" | "fmsub" | "fnmadd" | "fnmsub" => Some(3),
        "fmla" | "fmls" => Some(0),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Pipe { Fp, Div, Load, Store, Alu, Branch }

impl Pipe {
    /// How many a cycle, on `core`.
    fn width(self, core: &Core) -> f64 {
        match self { Pipe::Fp => core.fp, Pipe::Div => core.div, Pipe::Load => core.load, Pipe::Store => core.store, Pipe::Alu => core.alu, Pipe::Branch => core.branch }
    }
}

struct Ins { op: String, dests: Vec<String>, srcs: Vec<(usize, String)>, pipe: Pipe }

fn reg(tok: &str) -> Option<String> {
    let t = tok.trim_start_matches('[').trim_start_matches('{');
    let mut ch = t.chars();
    let c = ch.next()?;
    let digits: String = ch.take_while(|d| d.is_ascii_digit()).collect();
    if t == "sp" || t.starts_with("sp,") || t.starts_with("sp]") { return Some("sp".into()); }
    if digits.is_empty() { return None; }
    match c {
        'x' | 'w' => Some(format!("x{digits}")),
        'd' | 's' | 'q' | 'v' | 'h' | 'b' => Some(format!("v{digits}")),
        _ => None,
    }
}

/// Split operands on commas outside brackets.
fn operands(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth) = (Vec::new(), String::new(), 0i32);
    for c in s.chars() {
        match c {
            '[' | '{' => { depth += 1; cur.push(c); }
            ']' | '}' => { depth -= 1; cur.push(c); }
            ',' if depth == 0 => { out.push(cur.trim().to_string()); cur.clear(); }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() { out.push(cur.trim().to_string()); }
    out
}

fn parse(line: &str) -> Ins {
    let t = line.trim();
    let (op_full, rest) = t.split_once(char::is_whitespace).unwrap_or((t, ""));
    let op = op_full.split('.').next().unwrap_or(op_full).to_string();
    let ops = operands(rest);
    let store = op.starts_with("st");
    let compares = ["cmp", "cmn", "tst", "fcmp"].contains(&op.as_str());
    let branch = op.starts_with('b') && !op.starts_with("bic") || op.starts_with("cb") || op.starts_with("tb");
    let (mut dests, mut srcs) = (Vec::new(), Vec::new());
    for (k, o) in ops.iter().enumerate() {
        let parts: Vec<String> = o.split(|c: char| !(c.is_ascii_alphanumeric())).filter_map(reg).collect();
        for r in parts {
            if k == 0 && !store && !compares && !branch && !o.contains('[') { dests.push(r); } else { srcs.push((k, r)); }
        }
    }
    // a vector multiply-add accumulates into its destination, which is its addend too
    if ["fmla", "fmls", "mla", "mls"].contains(&op.as_str()) { if let Some(d) = dests.first().cloned() { srcs.push((0, d)); } }
    if op == "ldp" && ops.len() > 1 && !ops[1].contains('[') { if let Some(r) = reg(&ops[1]) { dests.push(r); } }
    if op_full.starts_with("b.") || ["bne", "beq", "bgt", "blt", "bge", "ble", "bhi", "bls", "bcc", "bcs", "bmi", "bpl", "csel", "csinc", "cset", "fcsel"].contains(&op.as_str()) {
        srcs.push((usize::MAX, "nzcv".into()));
    }
    if compares || ["subs", "adds", "ands"].contains(&op.as_str()) { dests.push("nzcv".into()); }
    let pipe = if ["fdiv", "fsqrt", "sdiv", "udiv"].contains(&op.as_str()) { Pipe::Div }
        else if op.starts_with('f') || ["scvtf", "ucvtf", "dup", "ins", "movi"].contains(&op.as_str()) || op_full.contains('.') && !branch { Pipe::Fp }
        else if op.starts_with("ld") { Pipe::Load }
        else if store { Pipe::Store }
        else if branch { Pipe::Branch }
        else { Pipe::Alu };
    Ins { op, dests, srcs, pipe }
}

/// The steady state of a lap run over and over (the chain it waits on, dependences only, and its
/// pipes' share, the longer), and one lap's critical path from a cold start.
pub fn lap_cycles(body: &[String], core: &Core) -> (f64, f64) {
    let ins: Vec<Ins> = body.iter().map(|l| parse(l)).collect();
    let mut counts: HashMap<Pipe, f64> = HashMap::new();
    for i in &ins { *counts.entry(i.pipe).or_default() += 1.0; }
    let res = counts.iter().map(|(p, n)| n / p.width(core)).fold(ins.len() as f64 / core.dispatch, f64::max);
    let mut ready: HashMap<String, f64> = HashMap::new();
    let k = 60;
    let mut finish = Vec::new();
    for _ in 0..k {
        let mut last = 0.0f64;
        for i in &ins {
            let (mut start, mut done) = (0.0f64, 0.0f64);
            for (at, r) in &i.srcs {
                let t = *ready.get(r).unwrap_or(&0.0);
                if addend(&i.op) == Some(*at) { done = done.max(t + 2.0); } else { start = start.max(t); }
            }
            let end = (start + latency(&i.op)).max(done);
            for r in &i.dests { ready.insert(r.clone(), end); }
            last = last.max(end);
        }
        finish.push(last);
    }
    let rec = (finish[k - 1] - finish[k / 2]) / ((k - 1 - k / 2) as f64);
    (rec.max(res), finish[0])
}

/// Cycles one pass of `trace` takes in the steady state, with the window: eight dispatched a cycle,
/// none before the one `WINDOW` earlier has finished, each on its pipe when it has a slot.
pub fn window_cycles(trace: &[String], core: &Core) -> f64 {
    let ins: Vec<Ins> = trace.iter().map(|l| parse(l)).collect();
    if ins.is_empty() { return 0.0; }
    let iters = 40;
    let mut ready: HashMap<String, f64> = HashMap::new();
    let mut used: HashMap<(Pipe, i64), f64> = HashMap::new();
    let mut fin: Vec<f64> = Vec::new();
    let mut marks = Vec::new();
    let mut disp = 0.0f64;
    for _ in 0..iters {
        let from = fin.len();
        for i in &ins {
            let n = fin.len();
            let gate = if n >= core.window { fin[n - core.window] } else { 0.0 };
            disp = (disp + 1.0 / core.dispatch).max(gate);
            let (mut start, mut done) = (disp, 0.0f64);
            for (at, r) in &i.srcs {
                let t = *ready.get(r).unwrap_or(&0.0);
                if addend(&i.op) == Some(*at) { done = done.max(t + 2.0); } else { start = start.max(t); }
            }
            let mut c = start.floor() as i64;
            while *used.get(&(i.pipe, c)).unwrap_or(&0.0) >= i.pipe.width(core) { c += 1; }
            *used.entry((i.pipe, c)).or_default() += 1.0;
            let end = (start.max(c as f64) + latency(&i.op)).max(done);
            for r in &i.dests { ready.insert(r.clone(), end); }
            fin.push(end);
        }
        marks.push(fin[from..].iter().cloned().fold(0.0, f64::max));
    }
    (marks[iters - 1] - marks[iters / 2]) / ((iters - 1 - iters / 2) as f64)
}

/// An innermost loop of the assembly: its source file and line (the `.loc` of its branch back),
/// its lap, how many laps an iteration does (vector lanes), and the loop around it as the code
/// before the inner loop, the inner body, and the code after.
pub struct Loop { pub at: (String, u32), pub body: Vec<String>, pub lanes: f64, pub nest: Option<(Vec<String>, Vec<String>)> }

fn is_instr(l: &str) -> bool {
    let t = l.trim();
    !t.is_empty() && !t.starts_with('.') && !t.starts_with("//") && !(t.starts_with(".L") && t.ends_with(':')) && !t.ends_with(':')
}

pub fn loops(asm: &str) -> Vec<Loop> {
    let lines: Vec<&str> = asm.lines().collect();
    let mut files: HashMap<String, String> = HashMap::new();
    let mut labels: HashMap<String, usize> = HashMap::new();
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if let Some(rest) = t.strip_prefix(".file") {
            let mut it = rest.split_whitespace();
            if let (Some(n), Some(name)) = (it.next(), it.next()) {
                if n.chars().all(|c| c.is_ascii_digit()) { files.insert(n.to_string(), name.trim_matches('"').to_string()); }
            }
        }
        if !l.starts_with(char::is_whitespace) && t.starts_with(".L") && t.ends_with(':') { labels.insert(t.trim_end_matches(':').to_string(), i); }
    }
    let mut found: Vec<(usize, usize)> = Vec::new();
    for (j, l) in lines.iter().enumerate() {
        let t = l.trim();
        let Some((op, rest)) = t.split_once(char::is_whitespace) else { continue };
        if !(op.starts_with('b') || op.starts_with("cb") || op.starts_with("tb")) || op.starts_with("bl") || op.starts_with("bic") { continue; }
        let target = rest.rsplit(',').next().unwrap_or("").trim();
        if let Some(&a) = labels.get(target) { if a < j { found.push((a, j)); } }
    }
    let instrs = |x: usize, y: usize| -> Vec<String> { lines[x..y].iter().filter(|l| is_instr(l)).map(|l| l.to_string()).collect() };
    let mut out = Vec::new();
    for &(a, b) in &found {
        if found.iter().any(|&(c, d)| a < c && d <= b && (c, d) != (a, b)) { continue; }
        let body = instrs(a + 1, b + 1);
        let loc = lines[a..=b].iter().filter_map(|l| {
            let t = l.trim().strip_prefix(".loc")?;
            let mut it = t.split_whitespace();
            let f = it.next()?;
            let line: u32 = it.next()?.parse().ok()?;
            (line > 0).then(|| (files.get(f).cloned().unwrap_or_default(), line))
        }).last();
        let Some((file, line)) = loc else { continue };
        if body.is_empty() || body.len() > 200 { continue; }
        let text = body.join("\n");
        let lanes = if text.contains(".4s") { 4.0 } else if text.contains(".2d") { 2.0 } else { 1.0 };
        let par = found.iter().filter(|&&(c, d)| c < a && b < d).min_by_key(|&&(c, d)| d - c);
        let nest = par.map(|&(c, d)| (instrs(c + 1, a + 1), instrs(b + 1, d + 1)));
        let name = std::path::Path::new(&file).file_name().map_or(file.clone(), |n| n.to_string_lossy().to_string());
        out.push(Loop { at: (name, line), body, lanes, nest });
    }
    out
}

/// The compute time, in nanoseconds, of loops with these laps: `laps` maps a loop's file name and
/// line to its laps, entries and whether its trip varies. A short loop (sixteen laps an entry or
/// fewer) is costed inside the loop around it, with the window; any other as laps × its lap; an
/// entry whose trip varies pays a missed exit. Of the assembly loops one source loop became — a
/// vector loop and its scalar remainder — the widest does the laps.
/// Also returned: the part of it in loops whose laps are only a bound.
pub fn compute_ns(asm: &str, laps: &HashMap<(String, u32), (f64, f64, bool, bool)>, core: &Core) -> (f64, f64) {
    let mut best: HashMap<(String, u32), Loop> = HashMap::new();
    for l in loops(asm) {
        let wider = best.get(&l.at).is_none_or(|b| l.lanes > b.lanes);
        if wider { best.insert(l.at.clone(), l); }
    }
    let (mut total, mut bounded) = (0.0, 0.0);
    for (at, l) in &best {
        let Some(&(n, entries, varies, bound)) = laps.get(at) else { continue };
        let trip = if entries > 0.0 { n / entries } else { 0.0 };
        let miss = if varies { entries * core.miss } else { 0.0 };
        let cycles = match &l.nest {
            Some((pre, post)) if trip > 0.0 && trip <= 16.0 => {
                let mut trace = pre.clone();
                for _ in 0..(trip.round().max(1.0) as usize) { trace.extend(l.body.iter().cloned()); }
                trace.extend(post.iter().cloned());
                entries * window_cycles(&trace, core)
            }
            _ => n * lap_cycles(&l.body, core).0 / l.lanes,
        };
        let ns = (cycles + miss) * core.cycle_ns;
        total += ns;
        if bound { bounded += ns; }
    }
    (total, bounded)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn body(s: &str) -> Vec<String> { s.lines().map(|l| l.to_string()).collect() }
    fn lap_cycles_x(b: &[String]) -> (f64, f64) { lap_cycles(b, &X925) }

    #[test]
    fn an_add_chain_waits_two_cycles() {
        let (c, _) = lap_cycles_x(&body("\tfadd\td0, d0, d1\n\tadd\tx0, x0, 1\n\tcmp\tx0, x2\n\tbne\t.L3"));
        assert!((c - 2.0).abs() < 0.05, "{c}");
    }

    #[test]
    fn a_multiply_add_through_its_addend_waits_two() {
        let (c, _) = lap_cycles_x(&body("\tldr\td2, [x6, x1, lsl 3]\n\tadd\tx1, x1, 1\n\tfmadd\td1, d2, d0, d1\n\tcmp\tx5, x1\n\tbne\t.L41"));
        assert!((c - 2.0).abs() < 0.05, "{c}");
        let (m, _) = lap_cycles_x(&body("\tfmadd\td1, d1, d0, d2\n\tadd\tx1, x1, 1\n\tcmp\tx5, x1\n\tbne\t.L41"));
        assert!((m - 4.0).abs() < 0.05, "{m}");
    }

    #[test]
    fn a_vector_accumulator_is_its_addend() {
        let (c, _) = lap_cycles_x(&body("\tldr\tq1, [x1]\n\tadd\tx1, x1, 3584\n\tld1r\t{v2.2d}, [x2], 8\n\tfmla\tv0.2d, v2.2d, v1.2d\n\tcmp\tx1, x4\n\tbne\t.L32"));
        assert!((c - 2.0).abs() < 0.05, "{c}");
    }

    #[test]
    fn independent_adds_are_the_pipes() {
        let lines: String = (0..8).map(|k| format!("\tfadd\td{k}, d{k}, d9\n")).collect::<String>() + "\tsubs\tx0, x0, 1\n\tbne\t.L2";
        let (c, _) = lap_cycles_x(&body(&lines));
        assert!((c - 2.0).abs() < 0.05, "{c}");
    }
}

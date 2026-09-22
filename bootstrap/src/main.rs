//! neant, the bootstrap compiler.
//!
//!   neant build f.nt [-o out] [--unchecked]   compile to a binary via the C compiler
//!   neant run   f.nt [--unchecked] [-- args]  build to a temp file and run it
//!   neant emit  f.nt [--unchecked]            print the generated C
//!   neant check f.nt                          parse and type-check only
//!   neant cost  f.nt [-M bytes] [-B bytes] [-P cores] [--eval n=..,..]
//!                                             infer work, moves and span for every function
//!   neant lock  f.nt [--check]                write costs.lock next to the source, or diff it
//!   neant measure f.nt --fn name [--sizes 1000,4000,...] [--shape p=n*n,...] [--repeat k] [--cpu 5] [--lock]
//!                                             run the function over a size sweep under perf and fit ~n^k
//!   any command: --apply fn:tile[,fn:transpose]  rewrite a function first

mod ast;
mod cost;
mod diag;
mod emit_c;
mod ir;
mod lex;
mod parse;
mod types;

use std::path::{Path, PathBuf};
use std::process::{self, Command};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: neant (build|run|emit|check) file.nt [-o out] [--unchecked]");
        process::exit(2);
    }
    let cmd = args[0].as_str();
    let mut file: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut checked = true;
    let mut passthrough: Vec<String> = Vec::new();
    let default_p = std::thread::available_parallelism().map(|n| n.get() as i128).unwrap_or(4);
    let mut machine = cost::Machine { m_bytes: 2 << 20, b_bytes: 64, p_cores: default_p };
    let mut eval: Option<String> = None;
    let mut lock_check = false;
    let mut applies: Vec<String> = Vec::new();
    let mut m_fn: Option<String> = None;
    let mut m_sizes: Vec<i64> = vec![1000, 2000, 4000, 8000, 16000, 32000];
    let mut m_shape: Vec<(String, String)> = Vec::new();
    let mut m_repeat: i64 = 1;
    let mut m_cpu: Option<u32> = None;
    let mut m_lock = false;
    let mut scop_fn: Option<String> = None;
    let mut use_iolb = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => { i += 1; out = args.get(i).map(PathBuf::from); }
            "--unchecked" => checked = false,
            "--check" => lock_check = true,
            "-M" => { i += 1; machine.m_bytes = parse_bytes(args.get(i)); }
            "-B" => { i += 1; machine.b_bytes = parse_bytes(args.get(i)); }
            "-P" => { i += 1; machine.p_cores = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(machine.p_cores); }
            "--eval" => { i += 1; eval = args.get(i).cloned(); }
            "--apply" => { i += 1; applies.extend(args.get(i).map(|s| s.split(',').map(String::from).collect::<Vec<_>>()).unwrap_or_default()); }
            "--fn" => { i += 1; m_fn = args.get(i).cloned(); }
            "--sizes" => { i += 1; m_sizes = args.get(i).map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect()).unwrap_or_default(); }
            "--shape" => { i += 1; m_shape = args.get(i).map(|s| s.split(',').filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect()).unwrap_or_default(); }
            "--repeat" => { i += 1; m_repeat = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(1); }
            "--cpu" => { i += 1; m_cpu = args.get(i).and_then(|s| s.parse().ok()); }
            "--lock" => m_lock = true,
            "--scop" => { i += 1; scop_fn = args.get(i).cloned(); }
            "--iolb" => use_iolb = true,
            "--" => { passthrough = args[i + 1..].to_vec(); break; }
            a if a.starts_with('-') => { eprintln!("unknown flag {a}"); process::exit(2); }
            a => file = Some(PathBuf::from(a)),
        }
        i += 1;
    }
    let Some(file) = file else {
        eprintln!("no input file");
        process::exit(2);
    };
    let src = match std::fs::read_to_string(&file) {
        Ok(s) => s,
        Err(e) => { eprintln!("{}: {e}", file.display()); process::exit(2); }
    };
    let mut module = match compile(&src) {
        Ok(m) => m,
        Err(e) => { eprintln!("{}:{e}", file.display()); process::exit(1); }
    };
    if let Err(e) = cost::analyze::apply_rewrites(&mut module, &applies, &machine) {
        eprintln!("{e}");
        process::exit(1);
    }
    // the layout of every struct array, chosen before anything is costed or emitted
    let layouts = cost::analyze::choose_layouts(&mut module, &machine);
    let opts = emit_c::Options { checked };
    // `#[cost]` is checked on every command: a broken bound is a build error
    if cmd != "cost" {
        let costs = cost::analyze(&module, &machine);
        let bad: Vec<&String> = costs.iter().flat_map(|c| c.violations.iter()).collect();
        if !bad.is_empty() {
            for v in bad { eprintln!("{}:{v}", file.display()); }
            process::exit(1);
        }
    }
    match cmd {
        "check" => {}
        "cost" => {
            print!("{}", cost::lock::layout_report(&layouts));
            let mut costs = cost::analyze(&module, &machine);
            if use_iolb { iolb_bounds(&module, &mut costs, &machine); }
            for c in &costs {
                print!("{}", cost::lock::report(c, &machine));
                if let (Some(ev), cost::CostResult::Exact { work, moves, .. }) = (&eval, &c.result) {
                    if let Some((w, m)) = evaluate(c, work, moves, ev, &machine) {
                        println!("{:<16}   at {ev}: work {w:.0}  moves {m:.0} bytes", "");
                    }
                }
            }
        }
        "lock" => {
            let costs = cost::analyze(&module, &machine);
            let path = file.parent().unwrap_or(Path::new(".")).join("costs.lock");
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            let rendered = cost::lock::render_with(&file.file_name().unwrap().to_string_lossy(), &costs, &existing, &layouts);
            if lock_check {
                let old = std::fs::read_to_string(&path).unwrap_or_default();
                let d = cost::lock::diff(&old, &rendered);
                if d.is_empty() { println!("costs.lock is up to date"); }
                else {
                    for (o, n) in &d {
                        if !o.is_empty() { println!("- {o}"); }
                        if !n.is_empty() { println!("+ {n}"); }
                    }
                    process::exit(1);
                }
            } else if let Err(e) = std::fs::write(&path, rendered) {
                eprintln!("{}: {e}", path.display());
                process::exit(1);
            }
        }
        "emit" => match &scop_fn {
            Some(name) => {
                let Some(f) = module.funcs.iter().find(|f| f.name == *name) else { eprintln!("no function `{name}`"); process::exit(2); };
                match cost::scop::export(&module, f) {
                    Ok((c, assumptions)) => { print!("{c}"); for a in assumptions { eprintln!("assumes {a}"); } }
                    Err(e) => { eprintln!("{}: `{name}` is not a SCoP: {e}", file.display()); process::exit(1); }
                }
            }
            None => print!("{}", emit_c::emit(&module, &opts)),
        },
        "build" => {
            let out = out.unwrap_or_else(|| file.with_extension(""));
            let c = emit_c::emit(&module, &opts);
            if let Err(e) = cc(&c, &out, &file) { eprintln!("{e}"); process::exit(1); }
        }
        "run" => {
            let c = emit_c::emit(&module, &opts);
            let dir = std::env::temp_dir().join(format!("neant-{}", process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let bin = dir.join("a.out");
            if let Err(e) = cc(&c, &bin, &file) { eprintln!("{e}"); process::exit(1); }
            let status = Command::new(&bin).args(&passthrough).status();
            let _ = std::fs::remove_dir_all(&dir);
            match status {
                Ok(s) => process::exit(s.code().unwrap_or(1)),
                Err(e) => { eprintln!("could not run {}: {e}", bin.display()); process::exit(1); }
            }
        }
        "measure" => {
            let Some(name) = m_fn else { eprintln!("measure needs --fn <function>"); process::exit(2) };
            let Some(fid) = module.funcs.iter().position(|f| f.name == name) else { eprintln!("no function `{name}`"); process::exit(2) };
            let f = &module.funcs[fid];
            let mut shapes = Vec::new();
            for &p in &f.params {
                let pname = &f.locals[p].name;
                let sh = m_shape.iter().find(|(k, _)| k == pname).map(|(_, v)| v.as_str()).unwrap_or("n");
                match cost::measure::Shape::parse(sh) { Some(s) => shapes.push(s), None => { eprintln!("--shape: cannot read `{sh}` for `{pname}`"); process::exit(2) } }
            }
            let costs = cost::analyze(&module, &machine);
            let fc = &costs[fid];
            println!("{}", cost::lock::line(fc));
            let declared = fc.declared.work.is_some() && fc.declared.moves.is_some();
            // a declaration is confirmed per call: repeat enough for process noise to divide away
            let m_repeat = if declared && m_repeat == 1 { 10000 } else { m_repeat };
            let dir = std::env::temp_dir().join(format!("neant-measure-{}", process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let mut ns = Vec::new(); let mut ws = Vec::new(); let mut mvs = Vec::new();
            let mut confirmed = true;
            // the driver's own setup and loop are measured without the call and subtracted
            let head = if declared { "declared work / moves, per call" } else { "predicted work / moves" };
            println!("  {:>10} {:>16} {:>16}   {head}", "n", "instructions", "L2 bytes");
            let run_perf = |bin: &Path| -> (u64, u64) {
                let mut best: Option<(u64, u64)> = None;
                for _ in 0..3 {
                    let mut cmd = if let Some(cpu) = m_cpu { let mut c = Command::new("taskset"); c.arg("-c").arg(cpu.to_string()).arg("perf"); c } else { Command::new("perf") };
                    let out = cmd.args(["stat", "-x,", "-e", "instructions,l2d_cache_refill"]).arg(bin).output();
                    let Ok(out) = out else { eprintln!("could not run perf"); process::exit(1) };
                    let err = String::from_utf8_lossy(&out.stderr);
                    let (Some(ins), Some(ref_)) = (cost::measure::perf_count(&err, "instructions"), cost::measure::perf_count(&err, "l2d_cache_refill")) else {
                        eprintln!("perf gave no counts (is kernel.perf_event_paranoid ≤ 2?):\n{err}"); process::exit(1)
                    };
                    if best.is_none_or(|(b, _)| ins < b) { best = Some((ins, ref_)); }
                }
                best.unwrap()
            };
            for &n in &m_sizes {
                let drv = cost::measure::driver(&module, fid, n, m_repeat, &shapes);
                let base = cost::measure::baseline(&module, fid, n, m_repeat, &shapes);
                let bin = dir.join(format!("m{n}")); let bbin = dir.join(format!("b{n}"));
                if let Err(e) = cc(&emit_c::emit(&drv, &emit_c::Options { checked: false }), &bin, &file) { eprintln!("{e}"); process::exit(1); }
                if let Err(e) = cc(&emit_c::emit(&base, &emit_c::Options { checked: false }), &bbin, &file) { eprintln!("{e}"); process::exit(1); }
                let (ins, ref_) = run_perf(&bin);
                let (bins, bref) = run_perf(&bbin);
                let ins = ins.saturating_sub(bins); let ref_ = ref_.saturating_sub(bref);
                let bytes = ref_ * machine.b_bytes as u64;
                let ev: Vec<String> = f.params.iter().zip(&shapes).map(|(&p, s)| {
                    let l = &f.locals[p];
                    let nm = if l.ty.is_arrayish() { format!("{}.len()", l.name) } else { l.name.clone() };
                    format!("{nm}={}", s.at(n))
                }).collect();
                let pred = if declared {
                    let (w, mv) = (cost::Cost::poly(fc.declared.work.clone().unwrap()), cost::Cost::poly(fc.declared.moves.clone().unwrap()));
                    match evaluate(fc, &w, &mv, &ev.join(","), &machine) {
                        Some((w, m)) => {
                            let (pw, pm) = (ins as f64 / m_repeat as f64, bytes as f64 / m_repeat as f64);
                            // within the counter's known factors: reads pair, prefetch overfetches,
                            // and process noise divided by the repeats is under a line per call
                            let ok = pw <= w * 1.5 + 2.0 && pm <= m * 2.0 + machine.b_bytes as f64;
                            if !ok { confirmed = false; }
                            format!("{w:.0} / {m:.0}   measured {pw:.1} / {pm:.1} per call   {}", if ok { "✓" } else { "✗ exceeds" })
                        }
                        None => String::new(),
                    }
                } else {
                    match &fc.result {
                        cost::CostResult::Exact { work, moves, .. } => evaluate(fc, work, moves, &ev.join(","), &machine).map_or(String::new(), |(w, m)| format!("{:.3e} / {:.3e}", w * m_repeat as f64, m * m_repeat as f64)),
                        _ => String::new(),
                    }
                };
                println!("  {n:>10} {ins:>16} {bytes:>16}   {pred}");
                ns.push(n as f64); ws.push(ins as f64); mvs.push(bytes as f64);
            }
            let _ = std::fs::remove_dir_all(&dir);
            let range = format!("{}..{}", m_sizes.first().copied().unwrap_or(0), m_sizes.last().copied().unwrap_or(0));
            let measured = if declared {
                let verdict = if confirmed { "confirmed" } else { "EXCEEDED" };
                println!("{:<16} declaration {verdict} by measurement over n = {range} (tolerance: work ×1.5 + 2, moves ×2 + one line, per call)", name);
                format!("{}  measured over n = {range}: {verdict}", cost::lock::line(fc))
            } else {
                let h = ns.len() / 2;
                let (kw, km) = (cost::measure::slope(&ns[h..], &ws[h..]), cost::measure::slope(&ns[h..], &mvs[h..]));
                let l = format!("{:<16} work ~n^{kw:.2}{:<20} moves ~n^{km:.2}{:<20} measured over n = {range}", name, "", "");
                println!("{l}");
                l
            };
            if m_lock {
                let path = file.parent().unwrap_or(Path::new(".")).join("costs.lock");
                let old = std::fs::read_to_string(&path).unwrap_or_else(|_| format!("# neant costs.lock — generated from {}; do not edit\n", file.file_name().unwrap().to_string_lossy()));
                if let Err(e) = std::fs::write(&path, cost::measure::update_lock(&old, &name, &measured)) { eprintln!("{}: {e}", path.display()); process::exit(1); }
                println!("written to {}", path.display());
            }
        }
        other => { eprintln!("unknown command `{other}`"); process::exit(2); }
    }
}

fn compile(src: &str) -> diag::Result<ir::Module> {
    let toks = lex::lex(src)?;
    let prog = parse::parse(toks)?;
    types::check(&prog)
}

fn cc(c: &str, out: &Path, src: &Path) -> Result<(), String> {
    let cfile = out.with_extension("c");
    std::fs::write(&cfile, c).map_err(|e| format!("{}: {e}", cfile.display()))?;
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let mut cmd = Command::new(&cc);
    cmd.args(["-O2", "-std=gnu11", "-Wall", "-Wno-unused-variable", "-Wno-unused-but-set-variable", "-Wno-unused-value", "-Wno-unused-function"]);
    // only a module with a `.par()` chain needs it — no new dependency for one that has none
    if c.contains("#pragma omp") { cmd.arg("-fopenmp"); }
    let status = cmd
        .arg("-o").arg(out)
        .arg(&cfile)
        .arg("-lm")
        .status()
        .map_err(|e| format!("could not run `{cc}`: {e} (set CC)"))?;
    if !status.success() {
        return Err(format!("{}: C compilation failed; the C is at {}", src.display(), cfile.display()));
    }
    Ok(())
}

fn parse_bytes(s: Option<&String>) -> i128 {
    let Some(s) = s else { eprintln!("-M/-B need a value"); process::exit(2) };
    let (num, mult) = if let Some(k) = s.strip_suffix('K') { (k, 1 << 10) }
        else if let Some(m) = s.strip_suffix('M') { (m, 1 << 20) }
        else { (s.as_str(), 1) };
    match num.parse::<i128>() { Ok(v) => v * mult, Err(_) => { eprintln!("bad size `{s}`"); process::exit(2) } }
}

/// `--eval n=1000,a.len()=1000`: every size variable of the function must be given. Conditions
/// are decided at the machine's `B` and `M`, and the applicable pieces' maximum is taken.
/// `--iolb`: hand every function IOLB can take to it and let its bound replace the catalogue's.
/// A function the export refuses keeps whatever the catalogue said and gets a note saying why.
fn iolb_bounds(module: &ir::Module, costs: &mut [cost::FuncCost], machine: &cost::Machine) {
    if cost::iolb::command().is_none() {
        eprintln!("--iolb: set NEANT_IOLB to a command with `{{file}}` in it, e.g. `tests/kernels/iolb.sh {{file}}`");
        process::exit(2);
    }
    let dir = std::env::temp_dir().join(format!("neant-iolb-{}", process::id()));
    let _ = std::fs::create_dir_all(&dir);
    for (f, c) in module.funcs.iter().zip(costs.iter_mut()) {
        let (src, assumptions) = match cost::scop::export(module, f) {
            Ok(s) => s,
            Err(e) => { if !c.bounds.is_empty() { c.notes.push(format!("IOLB not asked: {e}")); } continue; }
        };
        match cost::iolb::bound(&src, &c.names, &dir) {
            Ok(p) => {
                // both are valid lower bounds; the report leads with the asymptotically stronger
                // one (the gap of a rewrite is measured against the first). IOLB does not always
                // see through a tiled nest, where the catalogue's contraction bound stands.
                let line = c.bounds.first().map_or(f.line, |b| b.line);
                let kind = if assumptions.is_empty() { "whole function".to_string() } else { format!("whole function, untiled, if {}", assumptions.join(" and ")) };
                c.bounds.push(cost::bounds::Bound { kind, citation: "IOLB, Olivry et al. 2020".into(), moves: p, line, cold: false });
                cost::bounds::strongest_first(&mut c.bounds, &machine);
            }
            Err(e) => c.notes.push(format!("IOLB gave no bound: {e}")),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn evaluate(c: &cost::FuncCost, work: &cost::Cost, moves: &cost::Cost, ev: &str, m: &cost::Machine) -> Option<(f64, f64)> {
    use cost::size::Atom;
    let mut vals: Vec<(String, f64)> = Vec::new();
    for part in ev.split(',') {
        let (k, v) = part.split_once('=')?;
        vals.push((k.trim().to_string(), v.trim().parse().ok()?));
    }
    let f = |a: Atom| -> Option<f64> {
        match a {
            Atom::B => Some(m.b_bytes as f64),
            Atom::M => Some(m.m_bytes as f64),
            Atom::P => Some(m.p_cores as f64),
            Atom::Var(i) => {
                let name = c.names.get(i)?;
                vals.iter().find(|(k, _)| k == name).map(|(_, v)| *v)
            }
            Atom::Log(_) => None, // handled inside eval
        }
    };
    Some((work.eval(&f, m)?, moves.eval(&f, m)?))
}

//! neant, the bootstrap compiler.
//!
//!   neant build f.nt [-o out] [--unchecked]   compile to a binary via the C compiler
//!   neant run   f.nt [--unchecked] [-- args]  build to a temp file and run it
//!   neant emit  f.nt [--unchecked]            print the generated C
//!   neant check f.nt                          parse and type-check only
//!   neant cost  f.nt [-M bytes] [-B bytes] [--eval n=..,..]
//!                                             infer work and moves for every function
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
    let mut machine = cost::Machine { m_bytes: 2 << 20, b_bytes: 64 };
    let mut eval: Option<String> = None;
    let mut lock_check = false;
    let mut applies: Vec<String> = Vec::new();
    let mut m_fn: Option<String> = None;
    let mut m_sizes: Vec<i64> = vec![1000, 2000, 4000, 8000, 16000, 32000];
    let mut m_shape: Vec<(String, String)> = Vec::new();
    let mut m_repeat: i64 = 1;
    let mut m_cpu: Option<u32> = None;
    let mut m_lock = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => { i += 1; out = args.get(i).map(PathBuf::from); }
            "--unchecked" => checked = false,
            "--check" => lock_check = true,
            "-M" => { i += 1; machine.m_bytes = parse_bytes(args.get(i)); }
            "-B" => { i += 1; machine.b_bytes = parse_bytes(args.get(i)); }
            "--eval" => { i += 1; eval = args.get(i).cloned(); }
            "--apply" => { i += 1; applies.extend(args.get(i).map(|s| s.split(',').map(String::from).collect::<Vec<_>>()).unwrap_or_default()); }
            "--fn" => { i += 1; m_fn = args.get(i).cloned(); }
            "--sizes" => { i += 1; m_sizes = args.get(i).map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect()).unwrap_or_default(); }
            "--shape" => { i += 1; m_shape = args.get(i).map(|s| s.split(',').filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect()).unwrap_or_default(); }
            "--repeat" => { i += 1; m_repeat = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(1); }
            "--cpu" => { i += 1; m_cpu = args.get(i).and_then(|s| s.parse().ok()); }
            "--lock" => m_lock = true,
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
            let costs = cost::analyze(&module, &machine);
            for c in &costs {
                print!("{}", cost::lock::report(c, &machine));
                if let (Some(ev), cost::CostResult::Exact { work, moves }) = (&eval, &c.result) {
                    if let Some((w, m)) = evaluate(c, work, moves, ev, &machine) {
                        println!("{:<16}   at {ev}: work {w:.0}  moves {m:.0} bytes", "");
                    }
                }
            }
        }
        "lock" => {
            let costs = cost::analyze(&module, &machine);
            let rendered = cost::lock::render(&file.file_name().unwrap().to_string_lossy(), &costs);
            let path = file.parent().unwrap_or(Path::new(".")).join("costs.lock");
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
        "emit" => print!("{}", emit_c::emit(&module, &opts)),
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
            let dir = std::env::temp_dir().join(format!("neant-measure-{}", process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let mut ns = Vec::new(); let mut ws = Vec::new(); let mut mvs = Vec::new();
            println!("  {:>10} {:>16} {:>16}   {}", "n", "instructions", "L2 bytes", "predicted work / moves");
            for &n in &m_sizes {
                let drv = cost::measure::driver(&module, fid, n, m_repeat, &shapes);
                let c = emit_c::emit(&drv, &emit_c::Options { checked: false });
                let bin = dir.join(format!("m{n}"));
                if let Err(e) = cc(&c, &bin, &file) { eprintln!("{e}"); process::exit(1); }
                let mut best: Option<(u64, u64)> = None;
                for _ in 0..3 {
                    let mut cmd = if let Some(cpu) = m_cpu { let mut c = Command::new("taskset"); c.arg("-c").arg(cpu.to_string()).arg("perf"); c } else { Command::new("perf") };
                    let out = cmd.args(["stat", "-x,", "-e", "instructions,l2d_cache_refill"]).arg(&bin).output();
                    let Ok(out) = out else { eprintln!("could not run perf"); process::exit(1) };
                    let err = String::from_utf8_lossy(&out.stderr);
                    let (Some(ins), Some(ref_)) = (cost::measure::perf_count(&err, "instructions"), cost::measure::perf_count(&err, "l2d_cache_refill")) else {
                        eprintln!("perf gave no counts (is kernel.perf_event_paranoid ≤ 2?):\n{err}"); process::exit(1)
                    };
                    if best.is_none_or(|(b, _)| ins < b) { best = Some((ins, ref_)); }
                }
                let (ins, ref_) = best.unwrap();
                let pred = match &fc.result {
                    cost::CostResult::Exact { work, moves } => {
                        let ev: Vec<String> = f.params.iter().zip(&shapes).map(|(&p, s)| {
                            let l = &f.locals[p];
                            let nm = if l.ty.is_arrayish() { format!("{}.len()", l.name) } else { l.name.clone() };
                            format!("{nm}={}", s.at(n))
                        }).collect();
                        evaluate(fc, work, moves, &ev.join(","), &machine).map_or(String::new(), |(w, m)| format!("{:.3e} / {:.3e}", w * m_repeat as f64, m * m_repeat as f64))
                    }
                    _ => String::new(),
                };
                println!("  {n:>10} {ins:>16} {:>16}   {pred}", ref_ * machine.b_bytes as u64);
                ns.push(n as f64); ws.push(ins as f64); mvs.push((ref_ * machine.b_bytes as u64) as f64);
            }
            let _ = std::fs::remove_dir_all(&dir);
            let h = ns.len() / 2;
            let (kw, km) = (cost::measure::slope(&ns[h..], &ws[h..]), cost::measure::slope(&ns[h..], &mvs[h..]));
            let range = format!("{}..{}", m_sizes.first().copied().unwrap_or(0), m_sizes.last().copied().unwrap_or(0));
            let measured = format!("{:<16} work ~n^{kw:.2}{:<20} moves ~n^{km:.2}{:<20} measured over n = {range}", name, "", "");
            println!("{measured}");
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
    let status = Command::new(&cc)
        .args(["-O2", "-std=gnu11", "-Wall", "-Wno-unused-variable", "-Wno-unused-but-set-variable", "-Wno-unused-value", "-Wno-unused-function"])
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

/// `--eval n=1000,a.len()=1000`: every size variable of the function must be given.
fn evaluate(c: &cost::FuncCost, work: &cost::size::Poly, moves: &cost::size::Poly, ev: &str, m: &cost::Machine) -> Option<(f64, f64)> {
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
            Atom::Var(i) => {
                let name = c.names.get(i)?;
                vals.iter().find(|(k, _)| k == name).map(|(_, v)| *v)
            }
            Atom::Log(_) => None, // handled inside eval
        }
    };
    Some((work.eval(&f)?, moves.eval(&f)?))
}

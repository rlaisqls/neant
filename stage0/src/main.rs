//! neant stage 0.
//!
//!   neant build f.nt [-o out] [--unchecked]   compile to a binary via the C compiler
//!   neant run   f.nt [--unchecked] [-- args]  build to a temp file and run it
//!   neant emit  f.nt [--unchecked]            print the generated C
//!   neant check f.nt                          parse and type-check only
//!   neant cost  f.nt [-M bytes] [-B bytes] [--eval n=..,..]
//!                                             infer work and moves for every function
//!   neant lock  f.nt [--check]                write costs.lock next to the source, or diff it

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
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => { i += 1; out = args.get(i).map(PathBuf::from); }
            "--unchecked" => checked = false,
            "--check" => lock_check = true,
            "-M" => { i += 1; machine.m_bytes = parse_bytes(args.get(i)); }
            "-B" => { i += 1; machine.b_bytes = parse_bytes(args.get(i)); }
            "--eval" => { i += 1; eval = args.get(i).cloned(); }
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
    let module = match compile(&src) {
        Ok(m) => m,
        Err(e) => { eprintln!("{}:{e}", file.display()); process::exit(1); }
    };
    let opts = emit_c::Options { checked };
    match cmd {
        "check" => {}
        "cost" => {
            let costs = cost::analyze(&module, &machine);
            for c in &costs {
                println!("{}", cost::lock::line(c));
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
        }
    };
    Some((work.eval(&f)?, moves.eval(&f)?))
}

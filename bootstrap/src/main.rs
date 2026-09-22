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
//!   neant lexdump f.nt   this lexer's token kinds, one per line, numbered per compiler/lex.nt's
//!                        own scheme (`lex_kind_number`) — the self-hosted lexer's cross-check
//!                        (bootstrap/tests/self_host_lex.rs, docs/self-hosting-design.md)

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
    if cmd == "parsedump" {
        let prog = match lex::lex(&src).and_then(parse::parse) {
            Ok(p) => p,
            Err(e) => { eprintln!("{}:{e}", file.display()); process::exit(1); }
        };
        match parsedump::program(&prog) {
            Ok(kinds) => { for k in kinds { println!("{k}"); } }
            Err(why) => { eprintln!("{}: out of the self-hosted parser's slice: {why}", file.display()); process::exit(2); }
        }
        return;
    }
    if cmd == "lexdump" {
        match lex::lex(&src) {
            Ok(toks) => { for t in &toks { println!("{}", lex_kind_number(&t.tok)); } }
            Err(e) => { eprintln!("{}:{e}", file.display()); process::exit(1); }
        }
        return;
    }
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
                if let (Some(ev), cost::CostResult::Exact { work, moves, span }) = (&eval, &c.result) {
                    if let Some((w, m)) = evaluate(c, work, moves, ev, &machine) {
                        print!("{:<16}   at {ev}: work {w:.0}  moves {m:.0} bytes", "");
                        if span != work {
                            if let Some(s) = eval_one(c, span, ev, &machine) {
                                // `P` defaults to the machine's own core count unless `ev` names one
                                let p = ev.split(',').find_map(|p| p.split_once('=').filter(|(k, _)| k.trim() == "P").and_then(|(_, v)| v.trim().parse().ok()))
                                    .unwrap_or(machine.p_cores as f64);
                                print!("  span {s:.0}  T ≲ work/P + span = {:.0} (P={p:.0})", w / p + s);
                            }
                        }
                        println!();
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

/// `compiler/parse.nt`'s node kinds (docs/self-hosting-parser-design.md §2), printed depth-first,
/// children in the order that document's table lists them — the self-hosted parser's cross-check.
/// Kinds out of its first slice (struct literals, chains, closures, comprehensions, array
/// literals) make this print nothing and report that, rather than a number the other side cannot
/// produce.
mod parsedump {
    use crate::ast::*;

    /// Top-level items in source order, which the self-hosted parser chains the same way: the
    /// Rust `Program` splits them into two lists, so they are interleaved back by line.
    pub fn program(p: &Program) -> Result<Vec<i32>, String> {
        let mut items: Vec<(u32, &dyn Fn(&mut Vec<i32>) -> Result<(), String>)> = Vec::new();
        let fs: Vec<_> = p.funcs.iter().map(|f| (f.line, move |o: &mut Vec<i32>| func(f, o))).collect();
        let ss: Vec<_> = p.structs.iter().map(|d| (d.line, move |o: &mut Vec<i32>| strukt(d, o))).collect();
        for (l, f) in &fs { items.push((*l, f)); }
        for (l, f) in &ss { items.push((*l, f)); }
        items.sort_by_key(|(l, _)| *l);
        let mut out = Vec::new();
        for (_, f) in items { f(&mut out)?; }
        Ok(out)
    }
    fn strukt(d: &StructDef, o: &mut Vec<i32>) -> Result<(), String> {
        if d.layout.is_some() { return Err("a `#[layout(...)]` attribute".into()); }
        o.push(112);
        for (_, t, _, _) in &d.fields { o.push(113); ty(t, o)?; }
        Ok(())
    }
    fn func(f: &Func, o: &mut Vec<i32>) -> Result<(), String> {
        if !f.asserts.is_empty() { return Err("a `#[cost(...)]` attribute".into()); }
        if f.body.is_none() {
            o.push(116);
            for p in &f.params { o.push(111); ty(&p.ty, o)?; }
            if !matches!(f.ret, TypeExpr::Unit) { ty(&f.ret, o)?; }
            return Ok(());
        }
        o.push(110);
        for p in &f.params { o.push(111); ty(&p.ty, o)?; }
        if !matches!(f.ret, TypeExpr::Unit) { ty(&f.ret, o)?; }
        block(f.body.as_ref().unwrap(), o)
    }
    fn ty(t: &TypeExpr, o: &mut Vec<i32>) -> Result<(), String> {
        match t {
            TypeExpr::Named(_) => { o.push(100); Ok(()) }
            TypeExpr::Unit => { o.push(101); Ok(()) }
            TypeExpr::Array(e, n) => { o.push(102); ty(e, o)?; expr(n, o) }
            TypeExpr::Slice(e, _) => { o.push(103); ty(e, o) }
            TypeExpr::Owned(e) => { o.push(104); ty(e, o) }
        }
    }
    fn block(b: &Block, o: &mut Vec<i32>) -> Result<(), String> {
        o.push(76);
        for s in &b.stmts { stmt(s, o)?; }
        if let Some(t) = &b.tail { expr(t, o)?; }
        Ok(())
    }
    fn stmt(s: &Stmt, o: &mut Vec<i32>) -> Result<(), String> {
        match s {
            Stmt::Let { ty: t, init, .. } => {
                o.push(90);
                if let Some(t) = t { ty(t, o)?; }
                expr(init, o)
            }
            Stmt::Assign { target, value, .. } => { o.push(91); expr(target, o)?; expr(value, o) }
            Stmt::For { start, end, body, .. } => { o.push(92); expr(start, o)?; expr(end, o)?; block(body, o) }
            Stmt::While { cond, decreasing, body, .. } => {
                o.push(93);
                expr(cond, o)?;
                if let Some(d) = decreasing { expr(d, o)?; }
                block(body, o)
            }
            Stmt::Break(..) => { o.push(94); Ok(()) }
            Stmt::Expr(e) => { o.push(95); expr(e, o) }
            Stmt::Return(e, ..) => { o.push(96); if let Some(e) = e { expr(e, o)?; } Ok(()) }
        }
    }
    fn expr(e: &Expr, o: &mut Vec<i32>) -> Result<(), String> {
        match &e.kind {
            ExprKind::Int(_) => { o.push(60); Ok(()) }
            ExprKind::Float(_) => { o.push(61); Ok(()) }
            ExprKind::Bool(_) => { o.push(62); Ok(()) }
            ExprKind::Byte(_) => { o.push(63); Ok(()) }
            ExprKind::Bytes(_) => { o.push(64); Ok(()) }
            ExprKind::Var(_) => { o.push(65); Ok(()) }
            ExprKind::Binary(_, l, r) => { o.push(66); expr(l, o)?; expr(r, o) }
            ExprKind::Unary(_, a) => { o.push(67); expr(a, o) }
            ExprKind::Index(b, i) => { o.push(68); expr(b, o)?; expr(i, o) }
            ExprKind::Field(b, _) => { o.push(69); expr(b, o) }
            ExprKind::Call(_, args) => { o.push(71); for a in args { expr(a, o)?; } Ok(()) }
            ExprKind::Ref(a, _) => { o.push(73); expr(a, o) }
            ExprKind::Cast(a, t) => { o.push(74); expr(a, o)?; ty(t, o) }
            ExprKind::If(c, t, els) => {
                o.push(75);
                expr(c, o)?;
                block(t, o)?;
                if let Some(b) = els { block(b, o)?; }
                Ok(())
            }
            ExprKind::Block(b) => block(b, o),
            ExprKind::StructLit(_, fields) => {
                o.push(70);
                for (_, v) in fields { o.push(114); expr(v, o)?; }
                Ok(())
            }
            // `.len()` is the one method the self-hosted parser takes, by name and by shape
            // (docs/self-hosting-arrays-design.md §2); everything else is still a method call
            ExprKind::MethodCall(recv, m, args) if m == "len" && args.is_empty() => {
                o.push(72);
                expr(recv, o)
            }
            ExprKind::MethodCall(..) => Err("a method call or chain".into()),
            ExprKind::ArrayLit(es) => {
                o.push(77);
                for e in es { expr(e, o)?; }
                Ok(())
            }
            ExprKind::ArrayRepeat(e, n) => {
                o.push(78);
                expr(e, o)?;
                expr(n, o)
            }
            ExprKind::Lambda(..) => Err("a closure".into()),
            ExprKind::Comprehension { .. } => Err("a comprehension".into()),
        }
    }
}

/// `compiler/lex.nt`'s numbering, kept in sync by hand (no shared enum across files yet) — used
/// only by `lexdump`, a temporary cross-check for the self-hosted lexer (self-hosting-design.md).
fn lex_kind_number(t: &lex::Tok) -> i32 {
    use lex::Tok::*;
    match t {
        Ident(_) => 0, Int(_) => 1, Float(_) => 2, Byte(_) => 3, Bytes(_) => 4, Str(_) => 5,
        Fn => 6, Let => 7, Mut => 8, If => 9, Else => 10, For => 11, In => 12, Return => 13,
        True => 14, False => 15, As => 16, While => 17, Break => 18, Decreasing => 19,
        Extern => 20, Uses => 21, Struct => 22,
        LParen => 23, RParen => 24, LBracket => 25, RBracket => 26, LBrace => 27, RBrace => 28,
        Comma => 29, Semi => 30, Colon => 31, Arrow => 32, Dot => 33, DotDot => 34,
        Eq => 35, EqEq => 36, Ne => 37, Lt => 38, Le => 39, Gt => 40, Ge => 41,
        Plus => 42, Minus => 43, Star => 44, Slash => 45, Percent => 46,
        PlusEq => 47, MinusEq => 48, StarEq => 49, SlashEq => 50,
        Amp => 51, AmpAmp => 52, Pipe => 53, PipePipe => 54, Bang => 55, Hash => 56,
        Eof => 57,
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
    // the self-hosting I/O bridge (docs/self-hosting-design.md §2): a fixed convention, not a
    // flag — bootstrap/rt.c is linked in whenever the generated C actually calls into it
    let rt_c = Path::new(env!("CARGO_MANIFEST_DIR")).join("rt.c");
    if ["read_file(", "write_file(", "read_stdin(", "write_stdout(", "quit("].iter().any(|f| c.contains(f))
        && rt_c.exists() { cmd.arg(&rt_c); }
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

fn eval_one(c: &cost::FuncCost, cost: &cost::Cost, ev: &str, m: &cost::Machine) -> Option<f64> {
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
    cost.eval(&f, m)
}

fn evaluate(c: &cost::FuncCost, work: &cost::Cost, moves: &cost::Cost, ev: &str, m: &cost::Machine) -> Option<(f64, f64)> {
    Some((eval_one(c, work, ev, m)?, eval_one(c, moves, ev, m)?))
}

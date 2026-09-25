//! `neant hints f.nt`: the grey text, for an editor. One JSON document on stdout: every function
//! with the line of its `fn`, its tier, its costs as the report states them and what they rest on,
//! and every error the front end or a `#[cost]` bound raised as a diagnostic with a position. It
//! derives nothing of its own — each field is a line of `neant cost`'s report or of the lockfile,
//! taken apart — and it always prints a document, so an editor never has to read stderr.

use crate::cost::{self, CostResult, FuncCost, Machine};
use crate::cost::lock::{brief, report};
use std::path::Path;

/// A JSON string literal.
fn q(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn list(xs: &[String]) -> String {
    format!("[{}]", xs.iter().map(|x| q(x)).collect::<Vec<_>>().join(", "))
}

fn opt(x: &Option<String>) -> String {
    x.as_ref().map_or("null".to_string(), |s| q(s))
}

fn diagnostic(line: u32, col: u32, severity: &str, msg: &str) -> String {
    format!("{{\"line\": {line}, \"col\": {col}, \"severity\": {}, \"message\": {}}}", q(severity), q(msg))
}

/// `line N: rest`, the shape of every `#[cost]` violation, taken apart.
fn at_line(v: &str) -> (u32, &str) {
    v.strip_prefix("line ")
        .and_then(|r| r.split_once(": "))
        .and_then(|(n, rest)| n.parse().ok().map(|n| (n, rest)))
        .unwrap_or((0, v))
}

/// The grey text: what the README shows after a signature, `work …   moves …   tier`.
fn grey(c: &FuncCost) -> String {
    let fx = if c.effects.is_empty() { String::new() } else { format!(", {}", c.effects.join(", ")) };
    if let (Some(w), Some(mv)) = (&c.declared.work, &c.declared.moves) {
        let ok = if c.violations.is_empty() { "declared" } else { "declared ✗" };
        return format!("work ≤ {}   moves ≤ {}   {ok}{fx}", w.display(&c.names), mv.display(&c.names));
    }
    match &c.result {
        CostResult::Exact { work, moves, .. } => format!("work {}   moves {}   {}{fx}", brief(work, &c.names), brief(moves, &c.names), c.tier),
        CostResult::Unknown { reason, .. } => format!("unknown: {reason}{fx}"),
    }
}

fn function(c: &FuncCost, f: &crate::ir::Func, file: &str, m: &Machine) -> String {
    // the report's lines under the function's own, each as the report prints it, left-trimmed;
    // the first line (and a piecewise cost's regimes) is the function's own and is in the fields
    let lines: Vec<String> = report(c, m).lines().skip(1).map(|l| l.trim().to_string()).filter(|l| !l.starts_with("moves ")).collect();
    let starting = |p: &str| -> Vec<String> { lines.iter().filter(|l| l.starts_with(p)).map(|l| l[p.len()..].trim().to_string()).collect() };
    let bounds = starting("lower bound");
    let footprint = starting("footprint").into_iter().next();
    let (work, moves, span, regimes, cause, cause_line) = match &c.result {
        CostResult::Exact { work, moves, span } => {
            let mut ps: Vec<&cost::piece::Piece> = moves.pieces.iter().collect();
            ps.sort_by_key(|pc| pc.conds.len());
            let regimes: Vec<String> = if moves.single().is_some() { vec![] } else {
                ps.iter().map(|pc| {
                    let one = cost::Cost { pieces: vec![(*pc).clone()] };
                    one.display(&c.names).to_string()
                }).collect()
            };
            let span = if span != work { Some(span.display(&c.names).to_string()) } else { None };
            (Some(work.display(&c.names).to_string()), Some(moves.display(&c.names).to_string()), span, regimes, None, None)
        }
        CostResult::Unknown { reason, line } => (None, None, None, vec![], Some(reason.clone()), Some(*line)),
    };
    let declared = |p: &Option<cost::size::Poly>| p.as_ref().map(|p| p.display(&c.names).to_string());
    let fields = [
        ("name", q(&c.name)),
        ("file", q(file)),
        ("line", f.line.to_string()),
        // the lockfile's word: a declaration is the line, whatever was inferred under it
        ("tier", q(if c.declared.work.is_some() && c.declared.moves.is_some() { "declared" } else if cause.is_some() { "unknown" } else { c.tier })),
        ("hint", q(&grey(c))),
        ("work", opt(&work)),
        ("moves", opt(&moves)),
        ("span", opt(&span)),
        ("regimes", list(&regimes)),
        ("declared_work", opt(&declared(&c.declared.work))),
        ("declared_moves", opt(&declared(&c.declared.moves))),
        ("effects", list(&c.effects.iter().map(|e| e.to_string()).collect::<Vec<_>>())),
        ("bounds", list(&bounds)),
        ("footprint", opt(&footprint)),
        ("rests_on", list(&c.rests_on)),
        ("cause", opt(&cause)),
        ("cause_line", cause_line.map_or("null".to_string(), |l| l.to_string())),
        ("violations", list(&c.violations)),
        ("report", list(&lines)),
    ];
    format!("    {{{}}}", fields.iter().map(|(k, v)| format!("{}: {v}", q(k))).collect::<Vec<_>>().join(", "))
}

pub fn run(file: &Path, src: &str, machine: &Machine) -> String {
    let name = file.display().to_string();
    let mut diags = Vec::new();
    let mut funcs = Vec::new();
    match crate::compile(src) {
        Err(e) => diags.push(diagnostic(e.line, e.col, "error", &e.msg)),
        Ok(mut module) => {
            // the layouts are chosen before anything is costed, as on every other command
            cost::analyze::choose_layouts(&mut module, machine);
            let costs = cost::analyze(&module, machine);
            for (c, f) in costs.iter().zip(&module.funcs) {
                for v in &c.violations {
                    let (line, msg) = at_line(v);
                    diags.push(diagnostic(if line == 0 { f.line } else { line }, 1, "error", msg));
                }
                funcs.push(function(c, f, &name, machine));
            }
        }
    }
    let block = |xs: &[String]| if xs.is_empty() { "[]".to_string() } else { format!("[\n{}\n  ]", xs.iter().map(|x| if x.starts_with("    ") { x.clone() } else { format!("    {x}") }).collect::<Vec<_>>().join(",\n")) };
    format!("{{\n  \"file\": {},\n  \"diagnostics\": {},\n  \"functions\": {}\n}}\n", q(&name), block(&diags), block(&funcs))
}

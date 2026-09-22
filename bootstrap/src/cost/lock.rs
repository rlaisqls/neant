//! `costs.lock`: one line per function, sorted by name, written next to the source. It is
//! generated and committed, so a change in a function's cost is a diff in review.

use super::analyze::{CostResult, FuncCost, Machine};
use super::size::{Atom, Poly};

/// A polynomial for a report line: in full when short, its leading terms with `≈` otherwise.
fn brief(p: &Poly, names: &[String]) -> String {
    if p.terms.len() <= 3 { p.display(names).to_string() } else { format!("≈ {}", p.leading().display(names)) }
}

/// The ratio of two costs' leading terms, as a number at the machine's `B` and `M` when the
/// size variables cancel.
/// With `B` and `M` at their machine values, the leading terms of a cost collapse to one
/// coefficient per monomial in the size variables; this is the highest-degree one.
fn leading_numeric(p: &Poly, m: &Machine) -> Option<(Vec<(usize, super::size::Rat)>, f64)> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<Vec<(usize, super::size::Rat)>, f64> = BTreeMap::new();
    for (mono, c) in &p.terms {
        let mut coef = c.to_f64();
        let mut key = Vec::new();
        for (a, e) in &mono.factors {
            match a {
                Atom::B => coef *= (m.b_bytes as f64).powf(e.to_f64()),
                Atom::M => coef *= (m.m_bytes as f64).powf(e.to_f64()),
                Atom::Var(i) => key.push((*i, *e)),
            }
        }
        *groups.entry(key).or_insert(0.0) += coef;
    }
    let degree = |k: &Vec<(usize, super::size::Rat)>| k.iter().map(|(_, e)| e.to_f64()).sum::<f64>();
    groups.into_iter().max_by(|(a, _), (b, _)| degree(a).partial_cmp(&degree(b)).unwrap())
}

/// The ratio of two costs' leading terms at the machine's `B` and `M`, when the size variables
/// cancel — or the plain ratio when both are numbers.
fn gap(moves: &Poly, bound: &Poly, m: &Machine) -> Option<f64> {
    let (km, cm) = leading_numeric(moves, m)?;
    let (kb, cb) = leading_numeric(bound, m)?;
    if km != kb || cb <= 0.0 { return None; }
    Some(cm / cb)
}

/// The full report for one function: its line, then any bound, note and suggestion under it.
pub fn report(c: &FuncCost, m: &Machine) -> String {
    let mut out = line(c);
    out.push('\n');
    for b in &c.bounds {
        let g = match &c.result {
            CostResult::Exact { moves, .. } => gap(moves, &b.moves, m).map_or(String::new(), |g| format!("   gap {g:.0}× at M = {}, B = {}", human(m.m_bytes), m.b_bytes)),
            _ => String::new(),
        };
        out.push_str(&format!("{:<16} lower bound      moves {:<28} ({}, {}){g}\n", "", b.moves.display(&c.names).to_string(), b.kind, b.citation));
    }
    for n in &c.notes {
        out.push_str(&format!("{:<16} {n}\n", ""));
    }
    for s in &c.suggestions {
        match &s.result {
            CostResult::Exact { work, moves } => {
                // the gap is against the function's own bound: the rewrite does not change what is computed
                let g = c.bounds.first().and_then(|b| gap(moves, &b.moves, m)).map_or(String::new(), |g| format!("   gap {g:.0}×"));
                out.push_str(&format!(
                    "{:<16} {:<16} work {:<28} moves {:<28} [--apply {}]{g}\n", "", s.label, brief(work, &c.names), brief(moves, &c.names), s.flag));
            }
            CostResult::Unknown { reason, .. } => out.push_str(&format!("{:<16} {:<16} unknown: {reason}\n", "", s.label)),
        }
    }
    out
}

pub fn render(source_name: &str, costs: &[FuncCost]) -> String {
    let mut lines: Vec<String> = costs.iter().map(line).collect();
    lines.sort();
    let mut out = format!("# neant costs.lock — generated from {source_name}; do not edit\n");
    for l in lines { out.push_str(&l); out.push('\n'); }
    out
}

pub fn line(c: &FuncCost) -> String {
    match &c.result {
        CostResult::Exact { work, moves } => format!(
            "{:<16} work {:<28} moves {:<28} exact",
            c.name, work.display(&c.names).to_string(), moves.display(&c.names).to_string()
        ),
        CostResult::Unknown { reason, line } => format!("{:<16} unknown: {reason} (line {line})", c.name),
    }
}

fn human(b: i128) -> String {
    if b % (1 << 20) == 0 { format!("{} MiB", b >> 20) } else if b % 1024 == 0 { format!("{} KiB", b >> 10) } else { b.to_string() }
}

/// Lines that differ between an existing lockfile and a fresh rendering, as `(old, new)` pairs
/// keyed by function name. Header lines are ignored.
pub fn diff(old: &str, new: &str) -> Vec<(String, String)> {
    let entries = |s: &str| -> std::collections::BTreeMap<String, String> {
        s.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .map(|l| (l.split_whitespace().next().unwrap_or("").to_string(), l.to_string()))
            .collect()
    };
    let (a, b) = (entries(old), entries(new));
    let mut out = Vec::new();
    for name in a.keys().chain(b.keys()).collect::<std::collections::BTreeSet<_>>() {
        let (x, y) = (a.get(name).cloned().unwrap_or_default(), b.get(name).cloned().unwrap_or_default());
        if x != y { out.push((x, y)); }
    }
    out
}

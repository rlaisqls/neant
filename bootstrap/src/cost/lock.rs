//! `costs.lock`: one line per function, sorted by name, written next to the source. It is
//! generated and committed, so a change in a function's cost is a diff in review.

use super::analyze::{CostResult, FuncCost, Machine};
use super::piece::Cost;
use super::size::{Atom, Poly};

/// A polynomial for a report line: in full when short, its leading terms with `≈` otherwise.
fn brief_poly(p: &Poly, names: &[String]) -> String {
    if p.terms.len() <= 3 { p.display(names).to_string() } else { format!("≈ {}", p.leading().display(names)) }
}
/// A condition for the report: its working set's leading term, `≈` when that dropped something.
fn brief_cond(k: &super::piece::Cond, names: &[String]) -> String {
    let bytes = k.ws.mul_atom_pow(Atom::B, super::size::Rat::int(1));
    let (lead, approx) = if bytes.terms.len() > 1 { (bytes.leading(), "≈ ") } else { (bytes.clone(), "") };
    format!("{approx}{} {} M", lead.display(names), if k.fits { "<" } else { "≥" })
}
/// A cost for the report: one piece in full or leading terms; a piecewise cost as its two
/// least-conditional pieces with abbreviated conditions and a count of the rest. The lockfile
/// line keeps every piece exactly.
fn brief(c: &Cost, names: &[String]) -> String {
    match c.single() {
        Some(p) => brief_poly(p, names),
        None => {
            let mut ps: Vec<&super::piece::Piece> = c.pieces.iter().collect();
            ps.sort_by_key(|pc| pc.conds.len());
            let shown: Vec<String> = ps.iter().take(2).map(|pc| {
                let cond = if pc.conds.is_empty() { String::new() } else { format!(" if {}", pc.conds.iter().map(|k| brief_cond(k, names)).collect::<Vec<_>>().join(" and ")) };
                format!("{}{cond}", brief_poly(&pc.poly, names))
            }).collect();
            let more = if ps.len() > 2 { format!(" (+{} regimes)", ps.len() - 2) } else { String::new() };
            format!("{}{more}", shown.join(" | "))
        }
    }
}
/// The gap of every piece of a cost against a bound: `(condition text, ratio)`.
fn gaps(moves: &Cost, bound: &Poly, m: &Machine, names: &[String]) -> Vec<(String, f64)> {
    let mut ps: Vec<&super::piece::Piece> = moves.pieces.iter().collect();
    ps.sort_by_key(|pc| pc.conds.len());
    ps.iter().filter_map(|pc| {
        let g = gap(&pc.poly, bound, m)?;
        let cond = if pc.conds.is_empty() { String::new() } else { pc.conds.iter().map(|k| brief_cond(k, names)).collect::<Vec<_>>().join(" and ") };
        Some((cond, g))
    }).collect()
}
fn gap_text(gs: &[(String, f64)], suffix: &str) -> String {
    if gs.is_empty() { return String::new(); }
    if gs.len() == 1 && gs[0].0.is_empty() { return format!("   gap {:.0}×{suffix}", gs[0].1); }
    format!("   gap {}{suffix}", gs.iter().map(|(c, g)| if c.is_empty() { format!("{g:.0}×") } else { format!("{g:.0}× if {c}") }).collect::<Vec<_>>().join("; "))
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
                Atom::Log(_) => key.push((usize::MAX, *e)),
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
/// A piecewise cost is abbreviated here; `costs.lock` holds it in full.
pub fn report(c: &FuncCost, m: &Machine) -> String {
    let mut out = pretty_line(c);
    out.push('\n');
    for b in &c.bounds {
        let g = match &c.result {
            CostResult::Exact { moves, .. } => gap_text(&gaps(moves, &b.moves, m, &c.names), &format!(" at M = {}, B = {}", human(m.m_bytes), m.b_bytes)),
            _ => String::new(),
        };
        out.push_str(&format!("{:<16} lower bound      moves {:<28} ({}, {}){g}\n", "", b.moves.display(&c.names).to_string(), b.kind, b.citation));
    }
    // the signature's footprint: what a caller may be credited for, and when it stays resident
    if !c.footprint.is_empty() {
        let feet: Vec<String> = c.footprint.iter().map(|f| {
            let name = c.names.get(f.param).cloned().unwrap_or_default().trim_end_matches(".len()").to_string();
            format!("{name}: [{}, {}){}", f.lo.display(&c.names), f.hi.display(&c.names), if f.exact { "" } else { " (whole array)" })
        }).collect();
        let res = match &c.resident { Some(k) => format!("   resident after if {}", brief_cond(k, &c.names)), None => "   no residue claimed".to_string() };
        out.push_str(&format!("{:<16} footprint        {}{res}\n", "", feet.join("  ")));
    }
    for n in &c.notes {
        out.push_str(&format!("{:<16} {n}\n", ""));
    }
    for v in &c.violations {
        out.push_str(&format!("{:<16} ✗ {v}\n", ""));
    }
    for s in &c.suggestions {
        match &s.result {
            CostResult::Exact { work, moves } => {
                // the gap is against the function's own bound: the rewrite does not change what is computed
                let g = c.bounds.first().map_or(String::new(), |b| gap_text(&gaps(moves, &b.moves, m, &c.names), ""));
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
    let fx = if c.effects.is_empty() { String::new() } else { format!(", {}", c.effects.join(", ")) };
    match &c.result {
        CostResult::Exact { work, moves } => format!(
            "{:<16} work {:<28} moves {:<28} {}{fx}",
            c.name, work.display(&c.names).to_string(), moves.display(&c.names).to_string(), c.tier
        ),
        CostResult::Unknown { reason, line } => format!("{:<16} unknown: {reason} (line {line}){fx}", c.name),
    }
}

fn human(b: i128) -> String {
    if b % (1 << 20) == 0 { format!("{} MiB", b >> 20) } else if b % 1024 == 0 { format!("{} KiB", b >> 10) } else { b.to_string() }
}

/// The report's version of a function's line: piecewise costs abbreviated, then each piece on its
/// own line underneath so the regimes can be read.
pub fn pretty_line(c: &FuncCost) -> String {
    let fx = if c.effects.is_empty() { String::new() } else { format!(", {}", c.effects.join(", ")) };
    match &c.result {
        CostResult::Exact { work, moves } => {
            let mut s = format!("{:<16} work {:<28} moves {:<28} {}{fx}", c.name, brief(work, &c.names), brief_poly(moves.pieces.iter().min_by_key(|p| p.conds.len()).map(|p| &p.poly).unwrap_or(&Poly::zero()), &c.names), c.tier);
            if moves.single().is_none() {
                let mut ps: Vec<&super::piece::Piece> = moves.pieces.iter().collect();
                ps.sort_by_key(|pc| pc.conds.len());
                s = format!("{:<16} work {:<28} moves {:<28} {}{fx}  ({} regimes)", c.name, brief(work, &c.names), "", c.tier, ps.len());
                for pc in ps {
                    let cond = if pc.conds.is_empty() { "otherwise".to_string() } else { format!("if {}", pc.conds.iter().map(|k| brief_cond(k, &c.names)).collect::<Vec<_>>().join(" and ")) };
                    s.push_str(&format!("\n{:<16}                               moves {:<28} {cond}", "", brief_poly(&pc.poly, &c.names)));
                }
            }
            s
        }
        CostResult::Unknown { .. } => line(c),
    }
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

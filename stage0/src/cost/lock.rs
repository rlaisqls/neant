//! `costs.lock`: one line per function, sorted by name, written next to the source. It is
//! generated and committed, so a change in a function's cost is a diff in review.

use super::analyze::{CostResult, FuncCost};

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

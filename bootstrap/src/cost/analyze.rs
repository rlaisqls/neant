//! The cost calculus: work and moves for every function, in one walk over the typed IR.
//! The rules are specified in docs/cost-model.md; this file is their implementation and the
//! comments here say which rule each piece is.
//!
//! Work approximates the instructions the C compiler will emit: what lives in a register is
//! free, every arithmetic, load, store and branch is one. Moves counts bytes crossing the cache boundary in the
//! I/O model: for each array access inside a loop nest, the number of distinct cache lines it
//! touches is computed level by level from the innermost loop outward, and a level reuses
//! lines only when the working set of the levels inside it is known to fit in `M`.

use std::collections::{BTreeMap, HashMap};

use crate::ast::BinOp;
use crate::ir::*;

use super::bounds::{self, Bound};
use super::rewrite;
use super::piece::{Cond, Cost, Piece};
use super::size::{Atom, Opaque, Poly, Rat, Read, Root};

/// Every read atom's number, unique across functions, since a callee's reads travel into its callers.
static NEXT_READ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Debug, Clone, Copy)]
pub struct Machine {
    /// Bytes of the cache whose boundary moves are counted across.
    pub m_bytes: i128,
    /// Bytes per cache line.
    pub b_bytes: i128,
    /// Processors a `.par()` chain's `T ≤ work/P + span` bound is evaluated at.
    pub p_cores: i128,
    /// The roofline's two constants (plan § Stage D, decisions §7): nanoseconds per unit of work
    /// on one core, and bytes per nanosecond across the `M` boundary. Fitted by
    /// `tests/kernels/roofline.py fit`; a predicted time is `max(span·τ, work·τ/P, moves/BW)`.
    pub ns_per_work: f64,
    pub bytes_per_ns: f64,
    /// Nanoseconds a line fetched by a pointer chase costs, the miss it waits for: the roofline's
    /// latency term (cost-model § Time), fitted on a chase over an arena past the last cache.
    pub ns_per_miss: f64,
    /// Nanoseconds a line on a page of its own adds, the TLB walk before it (cost-model § Time,
    /// pages), fitted on a transpose.
    pub ns_per_page: f64,
    /// Nanoseconds a unit of serial work takes — work on a chain each lap waits on — fitted on a
    /// logistic map (cost-model § Time, serial work).
    pub ns_per_serial: f64,
    /// Nanoseconds an f64 division takes beyond an ordinary unit of work, its lower throughput,
    /// fitted on a sum of reciprocals (cost-model § Time, divisions).
    pub ns_per_div: f64,
    /// The bytes the second-level TLB maps, 2048 entries of a 4 KiB page, and the nanoseconds an
    /// access past it waits for its page walk (cost-model § Time, translations), both from
    /// `tests/kernels/pagewalk.nt`.
    pub tlb_bytes: i128,
    pub page_bytes: i128,
    pub ns_per_tlb: f64,
}

#[derive(Debug, Clone)]
pub enum CostResult {
    Exact { work: Cost, moves: Cost, span: Cost },
    Unknown { reason: String, line: u32 },
}

/// How a line that rests on a callee bounded by a scan names it (docs/cost-model.md § A scan).
const SCAN_TAG: &str = "(bound, a scan)";
/// the same for a loop charged a bound where a fit test in its variable changes inside it
const REGIME_TAG: &str = "(bound, a regime)";

/// Whether `c` only falls as the atom `k` grows: every term in it is `−c·k·(sizes)` with `k` to
/// the first power, `k` in no condition and inside no read or unknown callee's argument.
fn falls_in(c: &Cost, k: usize) -> bool {
    c.pieces.iter().all(|pc| {
        !pc.conds.iter().any(|cd| cd.ws.mentions(k)) && pc.poly.terms.iter().all(|(m, coef)| {
            let direct = m.factors.get(&Atom::Var(k));
            let inner = m.factors.keys().any(|a| !matches!(a, Atom::Var(_)) && Poly::atom(a.clone()).mentions(k));
            !inner && match direct { None => true, Some(e) => *e == Rat::one() && *coef < Rat::zero() }
        })
    })
}

#[derive(Debug, Clone)]
pub struct FuncCost {
    pub name: String,
    /// Names of `Atom::Var(i)` for this function. The first `params.len()` are the parameters,
    /// in order, so a caller can substitute by position.
    pub names: Vec<String>,
    pub result: CostResult,
    /// Bytes of lines fetched by accesses whose address is the value a load in the previous
    /// iteration produced — a pointer chase — part of `moves` that cannot overlap. Only a time
    /// reads it (cost-model § Time, latency); bounds and tiers never do.
    pub chase: Cost,
    /// Bytes of lines fetched by an access that moves a page or more per lap of its innermost
    /// loop — a column walk — each line on a page of its own, which a TLB walk precedes. Like
    /// `chase`, read only by a time (cost-model § Time, pages).
    pub paged: Cost,
    /// Bytes of `moves` read or written in a loop with two streams or more going at once: one
    /// core runs two streams at twice one stream's rate, near what the memory gives (cost-model
    /// § Time, streams). Read only by a time.
    pub conc: Cost,
    /// How many times each loop is entered and its body runs in one call, whether its trip varies
    /// from one entry to the next (its exit is then a branch the predictor misses), and whether the
    /// laps are only a bound (a `break`, a condition of several parts, a scan), by the loop's line
    /// (the line
    /// `neant emit --lines` puts before it): the laps a per-loop cycle count is multiplied by
    /// (plan § M7). Composed through calls; read only by `--eval`.
    pub laps: Vec<(u32, Poly, Poly, bool, bool)>,
    /// A callee's laps were left out, a size it needs being one the call cannot name (an amortised
    /// scan's): the laps above are not all the loops that run, and M7's line does not apply.
    pub laps_dropped: bool,
    /// Bytes a store's lines take back to memory, a write-back `moves` does not count, in one
    /// stream's bytes: in a loop of two streams or more, half (cost-model § Time, streams).
    pub wback: Cost,
    /// The part of `work` done in a loop whose body carries a scalar from one lap to the next
    /// through a multiply or a divide (`z = z·z + c`): each lap waits for the last, so it runs at
    /// an operation's latency, not the pipelines' throughput. Read only by a time (cost-model
    /// § Time, serial work).
    pub serial: Cost,
    /// How many `f64` divisions `work` holds: a division's throughput is a fraction of an add's,
    /// so a time charges each more (cost-model § Time, divisions). Read only by a time.
    pub divs: Cost,
    /// How many accesses translate a page the TLB no longer holds: each lap of an innermost loop
    /// whose accesses move a page or more a lap, over more pages than the TLB maps (cost-model
    /// § Time, translations). Read only by a time.
    pub tlb: Cost,
    /// Whether the function touches an array of its own, which a caller's residue cannot cover.
    pub internal: bool,
    /// Lower bounds the catalogue recognised in this function's body.
    pub bounds: Vec<Bound>,
    /// What the bound's operands do in the innermost loop, when it is worth saying.
    pub notes: Vec<String>,
    /// Rewrites that were tried on this function, with what they cost.
    pub suggestions: Vec<Suggestion>,
    /// Inferred effects: `io` when it prints or calls something that does.
    pub effects: Vec<&'static str>,
    /// How the cost was obtained: `exact`, or `recurrence` when a self-recursion was solved.
    pub tier: &'static str,
    /// `#[cost]` bounds this function breaks, as sentences.
    pub violations: Vec<String>,
    /// What the function touches, by parameter, in bytes — the part of its cost a caller can
    /// credit when the data is already resident.
    pub footprint: Vec<Foot>,
    /// The condition under which the whole footprint is resident when the function returns
    /// (its total working set, in lines, under `M`); `None` when some range is not exact, so
    /// no residue is claimed.
    pub resident: Option<Cond>,
    /// `#[cost(...)]` as parsed: the declared work and moves bounds. When present they are the
    /// function's line in the lockfile and all a caller sees of it.
    pub declared: Declared,
    /// What this line rests on besides the machine model: the declarations it composes,
    /// transitively — `labs (declared)`, `read (declared, measured over n = …)`.
    pub rests_on: Vec<String>,
    /// For a function returning an owned array: how many elements, over this function's own size
    /// atoms. A caller substitutes its argument sizes and knows what it was handed.
    pub result_size: Option<Poly>,
}

#[derive(Debug, Clone, Default)]
pub struct Declared {
    pub work: Option<Poly>,
    pub moves: Option<Poly>,
    /// `sizes = "n <= 512, a.len() <= 4096"`: the size bounds a budget is checked under
    pub sizes: Vec<(usize, f64)>,
}

/// A byte range of one parameter's array: `[lo, hi)`.
#[derive(Debug, Clone)]
pub struct Foot {
    pub param: usize,
    pub lo: Poly,
    pub hi: Poly,
    /// false when the range is the whole array because the exact range could not be found
    pub exact: bool,
}

/// A resident range in the caller's arrays, and under what conditions it is resident.
#[derive(Debug, Clone)]
struct Res {
    root: LocalId,
    lo: Poly,
    hi: Poly,
    conds: Vec<Cond>,
}

#[derive(Debug, Clone)]
pub struct Suggestion {
    pub label: String,
    /// The `--apply` spec that performs it.
    pub flag: String,
    pub result: CostResult,
}

/// Every function's cost, with no rewrites tried: what the layout pass compares.
pub fn analyze_costs(m: &Module, machine: &Machine) -> Vec<FuncCost> {
    let mut an = Analyzer::new(m, machine);
    for i in 0..m.funcs.len() { an.func(i); }
    an.done.into_iter().map(|c| c.unwrap()).collect()
}

/// What the layout pass decided for one struct type, for the report.
pub struct LayoutChoice {
    pub name: String,
    pub layout: Layout,
    /// the attribute fixed it; the model was not asked
    pub fixed: bool,
    /// functions whose moves differ between the layouts: (name, moves under the choice, under the other)
    pub decided_by: Vec<(String, String, String)>,
    /// a layout the chooser did not weigh, and why
    pub why: Option<&'static str>,
}

/// **The choice the compiler makes.** For each struct type the program has one layout, and it is
/// the one under which the program moves fewer bytes. The module is analysed twice per type —
/// every array of that type as an array of structs, then as one array per field — and the moves
/// of every function that touches the type are summed at a reference point (this machine, every
/// size a million). Types are decided in declaration order, each with the others at their current
/// choice; the `2^k` joint choices are not tried (docs/m4-design.md §9). `#[layout(...)]` fixes a
/// type and the model is not asked. A tie is AoS, and the report says the model did not decide.
pub fn choose_layouts(m: &mut Module, machine: &Machine) -> Vec<LayoutChoice> {
    let mut out = Vec::new();
    let mut refused: std::collections::HashSet<FuncId> = Default::default();
    for sid in 0..m.structs.len() {
        if m.structs[sid].fixed {
            out.push(LayoutChoice { name: m.structs[sid].name.clone(), layout: m.structs[sid].layout, fixed: true, decided_by: vec![], why: None });
            continue;
        }
        if super::ablate("layout") {
            m.structs[sid].layout = Layout::Aos;
            out.push(LayoutChoice { name: m.structs[sid].name.clone(), layout: Layout::Aos, fixed: false, decided_by: vec![], why: Some("ablated: the declared layout") });
            continue;
        }
        // an array of holders is AoS only (docs/arrays-by-value-design.md §9): SoA is not weighed
        if m.structs[sid].array_part().0 > 0 {
            let in_array = m.funcs.iter().any(|f| f.locals.iter().any(|l| matches!(l.ty.elem(), Some(Ty::Struct(s)) if *s == sid)));
            m.structs[sid].layout = Layout::Aos;
            let why = in_array.then_some("AoS only: it holds an array field");
            out.push(LayoutChoice { name: m.structs[sid].name.clone(), layout: Layout::Aos, fixed: false, decided_by: vec![], why });
            continue;
        }
        let mut totals = [0f64; 2];
        // only the functions that touch the type are compared, and their costs need only their
        // callees': the rest of the program is not analysed per layout
        let touches: Vec<bool> = m.funcs.iter().map(|f| f.locals.iter().any(|l| matches!(l.ty.elem(), Some(Ty::Struct(s)) if *s == sid))).collect();
        let mut costs: [Vec<Option<FuncCost>>; 2] = [vec![], vec![]];
        for (k, l) in [Layout::Aos, Layout::Soa].into_iter().enumerate() {
            m.structs[sid].layout = l;
            let mut an = Analyzer::new(m, machine);
            an.forest_skip = refused.clone();
            for (fi, t) in touches.iter().enumerate() { if *t { an.func(fi); } }
            refused.extend(an.forest_refused.iter().copied());
            costs[k] = an.done;
        }
        // functions that touch this type, and what each moves under the two layouts
        let mut decided_by: Vec<(String, String, String)> = Vec::new();
        for (fi, f) in m.funcs.iter().enumerate() {
            if !touches[fi] { continue; }
            let at = |c: &FuncCost| -> (f64, String) {
                match &c.result {
                    CostResult::Exact { moves, .. } => {
                        let point = |a: Atom| match a { Atom::B => Some(machine.b_bytes as f64), Atom::M => Some(machine.m_bytes as f64), Atom::P => Some(machine.p_cores as f64), Atom::Var(_) => Some(1e6), Atom::Log(_) | Atom::Opaque(_) | Atom::Read(_) => None };
                        (moves.eval(&point, machine).unwrap_or(0.0), super::lock::brief(moves, &c.names))
                    }
                    CostResult::Unknown { .. } => (0.0, "unknown".into()),
                }
            };
            let (va, sa) = at(costs[0][fi].as_ref().unwrap());
            let (vs, ss) = at(costs[1][fi].as_ref().unwrap());
            totals[0] += va;
            totals[1] += vs;
            if sa != ss { decided_by.push((f.name.clone(), sa, ss)); }
        }
        // strictly fewer bytes wins; a tie, or no difference at all, is AoS
        let soa_wins = totals[1] < totals[0] * 0.999;
        let layout = if soa_wins { Layout::Soa } else { Layout::Aos };
        m.structs[sid].layout = layout;
        let decided_by = decided_by.into_iter()
            .map(|(n, a, s)| if soa_wins { (n, s, a) } else { (n, a, s) })
            .collect();
        out.push(LayoutChoice { name: m.structs[sid].name.clone(), layout, fixed: false, decided_by, why: None });
    }
    out
}

pub fn analyze(m: &Module, machine: &Machine) -> Vec<FuncCost> {
    let mut an = Analyzer::new(m, machine);
    for i in 0..m.funcs.len() {
        an.func(i);
    }
    // a function with reuse to be had — an HBL exponent above one — gets each rewrite tried on it
    for i in 0..m.funcs.len() {
        if an.done[i].as_ref().is_some_and(|c| !c.bounds.iter().any(|b| b.kind.starts_with("HBL"))) { continue; }
        let f = &m.funcs[i];
        let mut sugg = Vec::new();
        // the tile side: read off the cost of the tiled program with the side symbolic, else the
        // square that fits three tiles
        let choice = tile_choice(&mut an, f);
        let t = choice.as_ref().map_or_else(|| rewrite::tile_side(machine.m_bytes, 8), |c| c.t);
        if let Some(c) = &choice {
            sugg.push(Suggestion { label: format!("tile T < {}", c.side), flag: format!("{}:tile={}", f.name, c.t), result: CostResult::Exact { work: c.work.clone(), moves: c.moves.clone(), span: c.work.clone() } });
        }
        if let Some(g) = rewrite::tile(f, t) {
            let r = Fa::new(&mut an, &g, None).run().result;
            sugg.push(Suggestion { label: format!("tile by {t}"), flag: format!("{}:tile={t}", f.name), result: r });
        }
        if let Some(g) = rewrite::transpose(f) {
            let r = Fa::new(&mut an, &g, None).run().result;
            sugg.push(Suggestion { label: "transpose the column operand".into(), flag: format!("{}:transpose", f.name), result: r });
        }
        an.done[i].as_mut().unwrap().suggestions = sugg;
    }
    an.done.into_iter().map(|c| c.unwrap()).collect()
}

/// The tile side the model prefers, and the tiled cost at it.
struct TileChoice {
    /// the side as an expression in `M` (and the sizes), e.g. `√(M/24)`
    side: String,
    /// the largest integer below it at this machine
    t: i64,
    work: Cost,
    moves: Cost,
}

/// **Choosing the tile side from the model.** The tiled program is analysed with its side `T` as a
/// size variable; its moves come out piecewise in `T`, each piece under fit conditions some of
/// which bound `T` from above (`32·T² < M`: every tile fits). In a piece whose moves fall as `T`
/// grows — every power of `T` non-positive — the best side is the largest the conditions allow,
/// so each such condition is solved for `T` at its boundary and substituted; the other conditions
/// of the piece, substituted too, must stay feasible and hold at the reference point. Two things
/// the machine taught (docs/experiments.md, the tile-side sweep):
///
/// - the boundary is taken at **half the cache**. A fit the ideal cache decides at `M` is not
///   one the machine honours: the tile at the edge of `M` moved twenty times what the model said,
///   the tile at the edge of `M/2` moved what it said and the least of all sides tried. A fit
///   decided at `M/2` is what an LRU cache of `M` can be relied on for (Sleator–Tarjan), and
///   what M1 measured as the width of the transition.
/// - among candidates the model cannot tell apart (within five percent at the reference point)
///   the **smaller side** wins: the regime where one tile fits and the rest stream ties the regime
///   where everything fits in the ideal cache and loses by an order of magnitude on the machine.
///
/// The integer recommended is the largest for which the working set, edge lines included, is
/// strictly below `M/2`. This is the upper side of the I/O question — what the best tiling of
/// this loop order moves — from the model's own exact cost rather than a separate cost formula,
/// in closed form where IOUB solves numerically.
fn tile_choice(an: &mut Analyzer, f: &Func) -> Option<TileChoice> {
    let machine = an.machine;
    let (g, tv) = rewrite::tile_sym(f)?;
    let tvar = g.params.iter().position(|&p| p == tv)?;
    let c = Fa::new(an, &g, None).run();
    let CostResult::Exact { work, moves, .. } = &c.result else { return None };
    if std::env::var("NEANT_DEBUG_TILE").is_ok() { eprint!("{}", super::lock::report(&c, &machine)); }
    // the reference point: this machine, every size a million — tiling is for large sizes
    let point = |t: Option<f64>| move |a: Atom| match a { Atom::B => Some(machine.b_bytes as f64), Atom::M => Some(machine.m_bytes as f64), Atom::P => Some(machine.p_cores as f64), Atom::Var(v) if v == tvar => t, Atom::Var(_) => Some(1e6), Atom::Log(_) | Atom::Opaque(_) | Atom::Read(_) => None };
    let holds = |conds: &[Cond]| conds.iter().all(|c| c.ws.eval(&point(None)).is_some_and(|ws| ((ws * (machine.b_bytes as f64)) < (machine.m_bytes as f64)) == c.fits));
    let mut best: Option<(f64, Poly, bool, Poly, Piece)> = None;
    let half = Poly::atom(Atom::M).scale(Rat::new(1, 2));
    for piece in &moves.pieces {
        let t_exps: Vec<Rat> = piece.poly.terms.keys().filter_map(|m| m.factors.get(&Atom::Var(tvar)).copied()).collect();
        if t_exps.is_empty() || t_exps.iter().any(|e| e.n > 0) { continue; }

        for (ci, cond) in piece.conds.iter().enumerate() {
            if !cond.fits { continue; }
            let bytes = cond.ws.mul_atom_pow(Atom::B, Rat::one());
            // the term of the working set that grows fastest in T sets the side; the rest are
            // edge lines, dropped from the expression and kept for the integer
            let Some((mono, coef)) = bytes.terms.iter().filter_map(|(m, c)| m.factors.get(&Atom::Var(tvar)).map(|e| (*e, m, *c))).max_by(|a, b| a.0.cmp(&b.0)).map(|(_, m, c)| (m, c)) else { continue };
            let k = mono.factors[&Atom::Var(tvar)];
            if !(k.is_int() && k.n > 0) { continue; }
            // c·rest·T^k = M/2  ⇒  T = (M / (2·c·rest))^(1/k)
            let mut rest = mono.clone();
            rest.factors.remove(&Atom::Var(tvar));
            let mut rest_p = Poly::zero();
            rest_p.terms.insert(rest, coef);
            // A regime that keeps *part* of a level resident — one tile in cache while the
            // others stream — is one the ideal cache computes and an LRU machine does not
            // deliver: the sweep measured such sides at up to thirty times their prediction.
            // Two working sets of the same degree in `T` belong to the same level, so if one of
            // them does not fit, this side is a partial fit and is not recommended.
            if piece.conds.iter().any(|c| !c.fits && degree_in(&c.ws, tvar) == k) { continue; }
            let Some(inv) = rest_p.inv_mono() else { continue };
            let Some(side) = half.mul(&inv).root_mono(k.n) else { continue };
            let poly = piece.poly.subst_pow(tvar, &side);
            let conds: Vec<Cond> = piece.conds.iter().enumerate().filter(|(j, _)| *j != ci).map(|(_, c)| Cond { ws: c.ws.subst_pow(tvar, &side), fits: c.fits }).collect();
            if !super::piece::feasible(&conds) || !holds(&conds) { continue; }
            let score = poly.eval(&point(None)).unwrap_or(f64::INFINITY);
            let side_at = side.eval(&point(None)).unwrap_or(f64::INFINITY);
            let better = match &best {
                None => true,
                Some((b, bside, ..)) => {
                    let bside_at = bside.eval(&point(None)).unwrap_or(f64::INFINITY);
                    score < *b * 0.95 || (score <= *b * 1.05 && side_at < bside_at)
                }
            };
            if better { best = Some((score, side, bytes.terms.len() > 1, bytes, Piece { conds, poly })); }
        }
    }
    let (_, side, approx, bytes, piece) = best?;
    // the integer side at this machine: the largest for which the working set, edge lines
    // included, is strictly below M/2
    let at: f64 = side.eval(&point(None))?;
    let mut t = at.floor() as i64;
    while t >= 2 && bytes.eval(&point(Some(t as f64))).is_some_and(|b| b >= machine.m_bytes as f64 / 2.0) { t -= 1; }
    if (t as f64) >= at { t -= 1; }
    if t < 2 { return None; }
    let side_text = format!("{} (at M/2{})", side.display(&c.names), if approx { ", leading term" } else { "" });
    let mut mv = Cost { pieces: vec![piece] };
    mv.prune_at(&machine);
    let mut wk = Cost { pieces: work.pieces.iter().map(|p| Piece { conds: p.conds.iter().map(|c| Cond { ws: c.ws.subst_pow(tvar, &side), fits: c.fits }).collect(), poly: p.poly.subst_pow(tvar, &side) }).collect() };
    wk.prune_at(&machine);
    Some(TileChoice { side: side_text, t, work: wk, moves: mv })
}

/// The largest power of `Var(v)` in any term.
fn degree_in(p: &Poly, v: usize) -> Rat {
    p.terms.keys().filter_map(|m| m.factors.get(&Atom::Var(v)).copied()).max().unwrap_or_else(Rat::zero)
}

/// Apply `--apply` specs (`name:tile`, `name:tile=T`, `name:transpose`) to a module before anything else sees it.
pub fn apply_rewrites(m: &mut Module, specs: &[String], machine: &Machine) -> Result<(), String> {
    for spec in specs {
        let Some((name, what)) = spec.split_once(':') else { return Err(format!("--apply takes `function:tile` or `function:transpose`, not `{spec}`")) };
        let Some(i) = m.funcs.iter().position(|f| f.name == name) else { return Err(format!("--apply: no function `{name}`")) };
        let g = match what {
            "tile" => rewrite::tile(&m.funcs[i], rewrite::tile_side(machine.m_bytes, 8)),
            t if t.starts_with("tile=") => rewrite::tile(&m.funcs[i], t[5..].parse().map_err(|_| format!("--apply: `{t}` needs an integer tile side"))?),
            "transpose" => rewrite::transpose(&m.funcs[i]),
            other => return Err(format!("--apply: unknown rewrite `{other}`")),
        };
        match g {
            Some(g) => m.funcs[i] = g,
            None => return Err(format!("--apply: `{name}` does not have the shape `{what}` applies to (a product with `let mut acc = 0` over the inner loop)")),
        }
    }
    Ok(())
}

struct Analyzer<'a> {
    m: &'a Module,
    machine: Machine,
    done: Vec<Option<FuncCost>>,
    active: Vec<bool>,
    /// `reach[f][g]`: `f` calls `g`, directly or through others
    reach: Vec<Vec<bool>>,
    /// `field_writes[f][i]`: the fields of the array `f`'s `i`th parameter that `f` may write,
    /// itself or through its callees — what a list walk in a caller needs left alone
    field_writes: Vec<Vec<FieldWrites>>,
    /// what each function guarantees about what it returns (`end ≥ start`), for a scan
    /// (docs/cost-model.md § A scan)
    scan_summ: Vec<Vec<super::scan::Ge>>,
    /// which functions advance an index through an array and return where it stopped, for an
    /// amortised scan in a caller (docs/cost-model.md § An amortised scan)
    advances: Vec<Option<super::scan::Advance>>,
    /// a member of a component of mutual recursion costed on its own, the calls into the
    /// component charged their call only (§ Recursion, a forest)
    scc_raw: HashMap<FuncId, FuncCost>,
    /// components refused as a recursion over a tree, by the function they were tried from: the
    /// verdict does not depend on a layout, so the layout pass tries each once
    forest_refused: std::collections::HashSet<FuncId>,
    forest_skip: std::collections::HashSet<FuncId>,
}

impl<'a> Analyzer<'a> {
    fn new(m: &'a Module, machine: &Machine) -> Analyzer<'a> {
        let n = m.funcs.len();
        let direct: Vec<Vec<FuncId>> = m.funcs.iter().map(|f| {
            let mut out = Vec::new();
            if let Some(b) = &f.body { callees_block(b, &mut out); }
            out
        }).collect();
        let reach = (0..n).map(|s| {
            let mut seen = vec![false; n];
            let mut stack: Vec<FuncId> = direct[s].clone();
            while let Some(g) = stack.pop() {
                if seen[g] { continue; }
                seen[g] = true;
                stack.extend(direct[g].iter().copied());
            }
            seen
        }).collect();
        let summ = super::scan::summaries(m);
        let advances: Vec<Option<super::scan::Advance>> = m.funcs.iter().map(|f| super::scan::advance(f, &summ)).collect();
        Analyzer { m, machine: *machine, done: vec![None; n], active: vec![false; n], reach, field_writes: field_writes(m), scan_summ: summ, advances, scc_raw: HashMap::new(), forest_refused: Default::default(), forest_skip: Default::default() }
    }
    /// Two distinct functions that call each other, however indirectly: a cost of either is a
    /// system of recurrences, and neither can stand as a named term in the other.
    fn mutual(&self, f: FuncId, g: FuncId) -> bool { f != g && self.reach[f][g] && self.reach[g][f] }

    /// **A component of mutual recursion over a tree in an arena** (docs/cost-model.md
    /// § Recursion, a forest): where `forest::shape` finds the members calling one another on the
    /// nodes of one array, each member costed on its own with those calls charged their call only,
    /// and the whole at most `L·xs.len() + 1` groups of calls, each of at most `m_G` invocations of
    /// each member `G`. `None` where the component is not that shape, and it stays unknown.
    fn forest(&mut self, fid: FuncId) -> Option<FuncCost> {
        if self.forest_skip.contains(&fid) { return None; }
        let n = self.m.funcs.len();
        let members = (0..n).filter(|&g| g == fid || self.mutual(fid, g)).count();
        // one function recursing on its own is a component of one, tried once its recurrence is
        // not solved (`func`)
        if members < 2 && !(self.reach[fid][fid] && self.done[fid].as_ref().is_some_and(|c| matches!(c.result, CostResult::Unknown { .. }))) { return None; }
        let r = self.forest_try(fid);
        if !r.as_ref().is_some_and(|c| matches!(c.result, CostResult::Exact { .. })) { self.forest_refused.insert(fid); }
        r
    }

    fn forest_try(&mut self, fid: FuncId) -> Option<FuncCost> {
        let n = self.m.funcs.len();
        let members: Vec<FuncId> = (0..n).filter(|&g| g == fid || self.mutual(fid, g)).collect();
        let f = &self.m.funcs[fid];
        if !f.asserts.is_empty() || matches!(f.ret, Ty::Array(..)) { return None; }
        if std::env::var("NEANT_DEBUG_FOREST").is_ok() { eprintln!("forest {}: {} members", f.name, members.len()); }
        let shape = super::forest::shape(self.m, fid, &members)?;
        let (_, ai) = shape.node[&fid];
        let names = param_names(f);
        let unknown = |reason: String| FuncCost {
            name: f.name.clone(), names: names.clone(), result: CostResult::Unknown { reason, line: f.line },
            chase: Cost::zero(), paged: Cost::zero(), laps: vec![], laps_dropped: false, conc: Cost::zero(), wback: Cost::zero(), serial: Cost::zero(), divs: Cost::zero(), tlb: Cost::zero(), internal: true,
            bounds: vec![], notes: vec![], suggestions: vec![], effects: vec![], violations: vec![], tier: "unknown", result_size: None,
            footprint: whole_arrays(self.m, f), resident: None, declared: Declared::default(), rests_on: vec![],
        };
        let unknown = |reason: String, rests: &[String]| FuncCost { rests_on: rests.to_vec(), ..unknown(reason) };
        let mut cols = [Cost::zero(), Cost::zero(), Cost::zero(), Cost::zero(), Cost::zero(), Cost::zero(), Cost::zero(), Cost::zero(), Cost::zero()];
        let mut rests_on: Vec<String> = Vec::new();
        let mut io = false;
        for &g in &members {
            // each member costed on its own and checked before the next, so a component refused
            // at its first member is not costed through
            if !self.scc_raw.contains_key(&g) {
                let gf = &self.m.funcs[g];
                self.active[g] = true;
                let mut fa = Fa::new(self, gf, Some(g));
                fa.scc = Some(members.clone());
                let fc = fa.run();
                self.active[g] = false;
                self.scc_raw.insert(g, fc);
            }
            let raw = &self.scc_raw[&g];
            let gf = &self.m.funcs[g];
            let CostResult::Exact { work, moves, .. } = &raw.result else {
                let CostResult::Unknown { reason, .. } = &raw.result else { unreachable!() };
                if g == fid { return Some(unknown(reason.clone(), &raw.rests_on)); }
                return Some(unknown(format!("calls `{}`, whose cost is unknown ({reason})", gf.name), &raw.rests_on));
            };
            let (pg, _) = shape.node[&g];
            let map = &shape.map[&g];
            let np = gf.params.len();
            // what one invocation's cost says of the data it is handed holds of that invocation
            // only: a regime is dropped for the sum of its pieces, an element read at an index
            // that moves from one invocation to the next is the most its array holds, and an
            // unknown callee's argument that moves is `_`
            let moving = |p: &Poly| p.mentions(pg) || (0..np).any(|j| p.mentions(j) && !matches!(map[j], super::forest::From::Param(_))) || p.max_var().is_some_and(|v| v >= np);
            let settle = |c: &Cost| -> Cost {
                // the regimes' sum: each is no less than zero, so no less than the one that holds,
                // and a sum is cheap where a `max` of large pieces is not
                let c = Cost::poly(c.pieces.iter().fold(Poly::zero(), |a, p| a.add(&p.poly)));
                c.hide_args(&moving).map(|q| q.widen_reads_where(&moving))
            };
            let own = [settle(work), settle(moves), settle(&raw.chase), settle(&raw.paged), settle(&raw.serial), settle(&raw.divs), settle(&raw.conc), settle(&raw.wback), settle(&raw.tlb)];
            // an invocation's own cost may not depend on which node it is on, nor on a parameter
            // that is not handed on unchanged from the start
            for c in &own {
                // refused with the reason: the component is the shape, and one member's cost is not
                let why = if c.mentions(pg) { Some(format!("on its node `{}`", raw.names[pg])) }
                    else if let Some(j) = (0..np).find(|&j| c.mentions(j) && !matches!(map[j], super::forest::From::Param(_))) { Some(format!("on `{}`, which not every call hands on unchanged", raw.names[j])) }
                    else if c.max_var().is_some_and(|v| v >= np) { Some(format!("on `{}`, a size it binds once", raw.names.get(c.max_var().unwrap()).cloned().unwrap_or_default())) }
                    else { None };
                // a read is a value at the start only where nothing in the component writes it
                let why = why.or_else(|| {
                    let mut rs = Vec::new();
                    for pc in &c.pieces { pc.poly.reads(&mut rs); }
                    rs.into_iter().find_map(|r| match &r.root {
                        Root::Param(j, nm) => {
                            let w = self.field_writes[g].get(*j)?;
                            let fi = r.field.as_ref().and_then(|fname| match gf.locals[gf.params[*j]].ty.elem() {
                                Some(Ty::Struct(sid)) => self.m.structs[*sid].fields.iter().position(|(n, _)| n == fname),
                                _ => None,
                            });
                            let slot = match (&r.field, &r.index) { (None, Some(i)) => i.as_const().filter(|c| c.is_int() && c.n >= 0).map(|c| IDX + c.n as usize), _ => None };
                            let hit = w.all || match (fi, slot) {
                                (Some(f), _) | (None, Some(f)) => w.fields.contains(&f),
                                (None, None) => !w.fields.is_empty(),
                            };
                            hit.then(|| format!("on `{nm}`, which the recursion writes"))
                        }
                        Root::Local(nm) => Some(format!("on `{nm}`, an array of an invocation's own")),
                    })
                });
                if why.is_some() && std::env::var("NEANT_DEBUG_FOREST").is_ok() {
                    for pc in &c.pieces { if pc.poly.mentions(pg) || (0..np).any(|j| pc.poly.mentions(j) && !matches!(map[j], super::forest::From::Param(_))) { eprintln!("  term of {}: {}", gf.name, pc.poly.display(&raw.names).to_string().chars().take(600).collect::<String>()); break; } }
                }
                if let Some(why) = why {
                    return Some(unknown(format!("a recursion over the tree in `{}`, but the cost of an invocation of `{}` depends {why}", f.locals[f.params[ai]].name, gf.name), &raw.rests_on));
                }
            }
            let subst: Vec<(usize, Poly)> = (0..np).filter_map(|j| match map[j] { super::forest::From::Param(k) => Some((j, Poly::var(k))), _ => None }).collect();
            let bad = std::cell::Cell::new(false);
            let rename = |r: &Root| -> Root {
                match r {
                    Root::Param(j, nm) => match map.get(*j) {
                        Some(super::forest::From::Param(k)) => Root::Param(*k, f.locals[f.params[*k]].name.clone()),
                        _ => { bad.set(true); Root::Param(*j, nm.clone()) }
                    },
                    Root::Local(nm) if g != fid && !nm.contains('.') => Root::Local(format!("{}.{nm}", gf.name)),
                    other => other.clone(),
                }
            };
            let m_g = Rat::int(*shape.mult.get(&g).unwrap_or(&1) as i128);
            for (k, c) in own.iter().enumerate() {
                let c = if g == fid { c.clone() } else { c.rename_roots(&rename).subst_many(&subst) };
                cols[k] = cols[k].add(&c.scale(m_g));
            }
            if bad.get() { return None; }
            for r in &raw.rests_on { if !rests_on.contains(r) { rests_on.push(r.clone()); } }
            if raw.effects.contains(&"io") { io = true; }
        }
        let groups = Poly::var(ai).scale(Rat::int(shape.links as i128)).add(&Poly::constant(1));
        let [work, moves, chase, paged, serial, divs, conc, wback, tlb] = cols.map(|c| c.mul_poly(&groups));
        let an = &f.locals[f.params[ai]].name;
        let others: Vec<String> = members.iter().filter(|&&g| g != fid).map(|&g| format!("`{}`", self.m.funcs[g].name)).collect();
        let with = if others.is_empty() { String::new() } else { format!("with {} ", others.join(", ")) };
        let note = format!("tree: {with}every call is on a node of `{an}`, its own or one below it, down a different link each time; over an arena that is a tree at most {} groups of calls (a promise, as a walk's)",
            groups.display(&names));
        let tier = if work.has_opaque() || moves.has_opaque() { "modulo" } else { "bound" };
        let footprint = whole_arrays(self.m, f);
        Some(FuncCost {
            name: f.name.clone(), names, result: CostResult::Exact { span: work.clone(), work, moves },
            chase, paged, laps: vec![], laps_dropped: true, conc, wback, serial, divs, tlb, internal: true, bounds: vec![], notes: vec![note], suggestions: vec![],
            effects: if io { vec!["io"] } else { vec![] }, violations: vec![], tier, result_size: None,
            footprint, resident: None, declared: Declared::default(), rests_on,
        })
    }
    fn func(&mut self, fid: FuncId) -> &FuncCost {
        if self.done[fid].is_none() && !self.active[fid] {
            if let Some(fc) = self.forest(fid) { self.done[fid] = Some(fc); }
        }
        if self.done[fid].is_none() {
            let f = &self.m.funcs[fid];
            if self.active[fid] {
                // a cycle: recursion is a recurrence, which is not solved yet (M3)
                self.done[fid] = Some(FuncCost {
                    name: f.name.clone(),
                    names: param_names(f),
                    result: CostResult::Unknown { reason: "mutually recursive with another function; only self-recursion is solved".into(), line: f.line }, chase: Cost::zero(), paged: Cost::zero(), laps: vec![], laps_dropped: false, conc: Cost::zero(), wback: Cost::zero(), serial: Cost::zero(), divs: Cost::zero(), tlb: Cost::zero(), internal: true,
                    bounds: vec![], notes: vec![], suggestions: vec![], effects: vec![], violations: vec![], tier: "unknown", result_size: None,
                    footprint: vec![], resident: None, declared: Declared::default(), rests_on: vec![],
                });
            } else {
                self.active[fid] = true;
                let mut fc = Fa::new(self, f, Some(fid)).run();
                // a recursion no measure solves may be one over a tree in an arena
                if self.reach[fid][fid] && matches!(fc.result, CostResult::Unknown { .. }) && (0..self.m.funcs.len()).all(|g| g == fid || !self.mutual(fid, g)) {
                    self.done[fid] = Some(fc.clone());
                    // the shape found: its verdict, a bound or the reason one member's cost is not one
                    if let Some(t) = self.forest(fid) { fc = t; }
                    self.done[fid] = None;
                }
                self.active[fid] = false;
                self.done[fid] = Some(fc);
            }
        }
        self.done[fid].as_ref().unwrap()
    }
}

/// Every array parameter of `f`, whole and inexact: a footprint that credits nothing and claims no
/// residue.
fn whole_arrays(m: &Module, f: &Func) -> Vec<Foot> {
    f.params.iter().enumerate().filter(|(_, p)| f.locals[**p].ty.is_arrayish()).map(|(i, &p)| {
        let es = f.locals[p].ty.elem().map_or(8, |t| m.size_of(t));
        Foot { param: i, lo: Poly::zero(), hi: Poly::var(i).scale(Rat::int(es)), exact: false }
    }).collect()
}

fn param_names(f: &Func) -> Vec<String> {
    f.params.iter().map(|&p| {
        let l = &f.locals[p];
        if l.ty.is_arrayish() { format!("{}.len()", l.name) } else { l.name.clone() }
    }).collect()
}

/// An index expression as an affine function of the enclosing loop variables, with
/// coefficients that are size polynomials.
#[derive(Debug, Clone, Default, PartialEq)]
struct Affine {
    coeffs: BTreeMap<LocalId, Poly>,
    konst: Poly,
}

impl Affine {
    fn constant(p: Poly) -> Affine { Affine { coeffs: BTreeMap::new(), konst: p } }
    fn var(l: LocalId) -> Affine {
        let mut a = Affine::default();
        a.coeffs.insert(l, Poly::constant(1));
        a
    }
    fn add(&self, o: &Affine) -> Affine {
        let mut c = self.coeffs.clone();
        for (l, p) in &o.coeffs {
            let np = c.get(l).map_or(p.clone(), |x| x.add(p));
            if np.is_zero() { c.remove(l); } else { c.insert(*l, np); }
        }
        Affine { coeffs: c, konst: self.konst.add(&o.konst) }
    }
    fn scale(&self, p: &Poly) -> Affine {
        Affine { coeffs: self.coeffs.iter().map(|(l, c)| (*l, c.mul(p))).collect(), konst: self.konst.mul(p) }
    }
    fn is_const(&self) -> bool { self.coeffs.is_empty() }
}

#[derive(Clone, Copy, PartialEq)]
enum Dir { Upper, Lower }

struct Loop {
    id: usize,
    /// `None` for a loop inherited from the caller at a specialised call: no index in this
    /// function can depend on it, so it only ever contributes reuse (stride 0) or repetition.
    var: Option<LocalId>,
    /// The loop variable as a size atom, so a cost accumulated in the body may mention it and
    /// be summed over it exactly when the loop is left. `None` for inherited and `decreasing` loops.
    atom: Option<usize>,
    trip: Poly,
    /// first value of the variable, and how much it moves per iteration
    lo: Poly,
    step: i128,
    /// The loop's start as an affine function of the loops outside it, so that an index written
    /// in terms of this variable is seen to move with the outer loops too. `None` when the start
    /// is not affine, in which case any index using this variable is treated as non-affine.
    offset: Option<Affine>,
    /// A scan (docs/cost-model.md § A scan's accesses): `var` only grows, by at least `step` a
    /// lap, from at least `lo`, and stays below the bound — not an induction variable. Its affine
    /// form is for access sites only, where `lo + step·k` for the `k`th lap is the densest the lines
    /// can be; sizes, lower bounds and residue never read it.
    monotone: bool,
}

enum Fail {
    Unknown(String, u32),
}

/// One `x[i]` in the source, with the loops it sits in (outermost first). Moves are settled
/// for all sites together once the body has been walked, because whether a level reuses its
/// lines depends on every site sharing that loop.
struct Site {
    /// the index is a local a loop loads: each access waits for the one before (§ Time)
    dep: bool,
    /// the address moves a page or more per lap of the innermost loop (§ Time, pages)
    paged: bool,
    /// the site is stored to: its lines go back to memory (§ Time, streams)
    store: bool,
    arr: LocalId,
    aff: Option<Affine>,
    /// bytes the site actually touches: the element's size, or one field's under AoS
    es: i128,
    /// bytes the address moves per unit of the index: the element's size in memory. They differ
    /// for a field of a struct array, and that difference is what a layout costs.
    stride: i128,
    /// which field, when the site is one field of a struct element
    field: Option<usize>,
    /// a canonical form of the index expression, for sites whose index is not affine: two
    /// non-affine reads of the same element are still one element
    key: Option<String>,
    /// where this site's addresses start inside the array. Zero under AoS; under SoA the field
    /// arrays are modelled as laid end to end, so a field's base is the size of the fields before
    /// it. The ranges of two fields are then disjoint, and every rule about ranges — footprint,
    /// residue, disjointness — holds without a special case.
    base: Poly,
    path: Vec<usize>,
    /// the `if` branches this site sits in, outermost first: sites on different sides of one
    /// `if` are alternatives, and their moves combine by max, not sum
    branch: Vec<(usize, bool)>,
    /// the index in the loops' own values (`(k + 1)·n + j`), where `aff` has each loop's from its
    /// start: what a triangle's hull is taken over, one loop's extreme inside the next's
    raw: Option<Poly>,
}

/// What is remembered of a loop after it is popped.
#[derive(Clone)]
struct LoopRec {
    var: Option<LocalId>,
    atom: Option<usize>,
    trip: Poly,
    lo: Poly,
    step: i128,
    monotone: bool,
    /// set when a sum over this loop decided a fit test in its variable at both ends and found
    /// them on two sides (`Cost::sum_split`): the loop is then charged a bound
    straddled: std::cell::Cell<bool>,
}

impl LoopRec {
    /// `Σ` of `p` over this loop's iterations: exact over the atom, `× trip` when `p` does not
    /// mention it.
    fn sum(&self, p: &Poly) -> Poly {
        match self.atom { Some(a) => p.sum_over(a, &self.lo, self.step, &self.trip), None => p.mul(&self.trip) }
    }
    fn sum_cost(&self, c: &Cost) -> Cost {
        match self.atom {
            Some(a) => {
                let mut s = false;
                let r = c.sum_split(a, &self.lo, self.step, &self.trip, &mut s);
                if s { self.straddled.set(true); }
                r
            }
            None => c.mul_poly(&self.trip),
        }
    }
    /// The loop variable's last value.
    fn last(&self) -> Poly { self.lo.add(&self.trip.sub(&Poly::constant(1)).scale(Rat::int(self.step))) }
}

/// One call amortised over a loop: `let r = g(.., a, .., v, ..)` once a lap and `v = r.f + c`, so
/// the laps' distances telescope. `alpha` is what the callee charges per unit of distance, found
/// at the call from its cost, and `(work, moves)`.
#[derive(Debug, Clone)]
struct Amort { line: u32, fid: FuncId, adv: super::scan::Advance, arr: LocalId, var: LocalId, alpha: Option<(Cost, Cost)>, group: usize, edge: Poly, rates: (Cost, Cost) }

struct Fa<'a, 'b, 'c> {
    an: &'b mut Analyzer<'a>,
    f: &'c Func,
    names: Vec<String>,
    sites: Vec<Site>,
    loop_recs: Vec<LoopRec>,
    bounds: Vec<Bound>,
    /// per array root **and field**, the largest injective image (bytes) any reference to it
    /// has. Two fields of one struct array are disjoint words, so they add; two references to
    /// the same field cover each other, so the larger stands.
    images: HashMap<(LocalId, Option<usize>), Poly>,
    /// the size of the array this function returns, once the walk has seen it
    result_size: Option<Poly>,
    /// the size of the array the last call returned, for the `let` that binds it
    last_result: Option<Poly>,
    /// when that size is an atom minted at the call for an extern's `result.len()` (program
    /// input), so the `let` can name it after the local: `data.len()`
    last_result_atom: Option<usize>,
    /// `arg_count()`, one atom for the whole function when the program has the builtin: the
    /// number of arguments is fixed for the run, so every call returns it (docs/cost-model.md
    /// § Program input)
    argc_atom: Option<usize>,
    /// A scalar that a statement of the enclosing block stores into, or loads from, an array
    /// element (`c[i·n + j] = acc`): inside that block the scalar *is* that element's running
    /// value, and a statement using it references the element. Register promotion does not
    /// change the computation, so the bound over the element's projection holds.
    scalar_alias: HashMap<LocalId, (LocalId, Expr, Option<usize>)>,
    alias_scopes: Vec<Vec<LocalId>>,
    notes: Vec<String>,
    io: bool,
    /// the function being analysed, when it may call itself
    self_fid: Option<FuncId>,
    /// costing one member of a component on its own: calls into it charge their call only
    scc: Option<Vec<FuncId>>,
    /// each self-call: the argument sizes by parameter (None when not a size), how many times
    /// the site runs per invocation, and its line
    rec_calls: Vec<(Vec<Option<Poly>>, Poly, u32)>,
    /// each self-call's arguments, for a recursion over a tree (docs/cost-model.md § Recursion, a tree)
    /// size in elements of every array/slice local
    local_size: HashMap<LocalId, Poly>,
    /// value of every immutable i64 local that is an affine expression
    local_affine: HashMap<LocalId, Affine>,
    /// every value a mutable i64 local may hold here, as sizes, by the definitions that reach
    /// this point: one after a straight-line assignment, several after an `if` that assigns in
    /// both branches, none (`None`) after a loop that assigns it. Structured control flow makes
    /// this a walk rather than a fixpoint.
    initial: HashMap<LocalId, Option<Vec<Poly>>>,
    /// while set, `size_of` reads a mutable local as that entry value: for a `decreasing` measure
    at_entry: bool,
    loops: Vec<Loop>,
    /// cost accumulated in the innermost open loop body (or the function body); when a loop is
    /// left, its frame is summed over the loop atom into the frame below
    work: Cost,
    moves: Cost,
    /// the critical path: equal to `work` everywhere by sequential composition (mirrored at every
    /// site that adds to `work`), except a `.par()` loop's own reduction depth
    /// (m5-span-design.md §3); never threaded through self-recursion (`solve_recurrence` has no
    /// span of its own, so a recursive function's span falls back to its work, conservatively).
    span: Cost,
    saved: Vec<(Cost, Cost, Cost)>,
    /// the `if` branches currently open, for tagging access sites
    branch: Vec<(usize, bool)>,
    next_if: usize,
    /// the array a view local looks into (parameters and arrays are their own root)
    local_root: HashMap<LocalId, LocalId>,
    /// what is resident in this function's arrays at the current point, from calls and loop
    /// nests already walked — what a call here may be credited for
    resident: Vec<Res>,
    /// moves of calls, kept apart from the function's own sites: a loop body is walked a second
    /// time with the residue of its first iteration to cost the iterations after the first
    call_moves: Cost,
    saved_calls: Vec<Cost>,
    /// the second walk: only call moves are recounted; work, sites, recursion and effects are not
    replay: bool,
    /// whether the innermost open frame has called anything (a loop then needs the second walk)
    has_call: Vec<bool>,
    /// declarations this function's cost composes, transitively
    rests_on: Vec<String>,
    /// sizes read from memory so far, by array, element and field: a later read of the same
    /// element is the same value until something writes the array (stage D)
    reads: std::cell::RefCell<HashMap<(LocalId, Option<Poly>, Option<usize>, bool), Poly>>,
    /// for each open loop, the arrays its body writes: a read of one of them is a different value
    /// every iteration, and not a size
    loop_writes: Vec<HashMap<LocalId, FieldWrites>>,
    /// reading at entry to the innermost loop: its own writes have not happened yet
    entry_read: std::cell::Cell<bool>,
    /// the scans this function's loops are bounded by, as notes (docs/cost-model.md § A scan)
    scans: std::cell::RefCell<Vec<String>>,
    /// for each open `while` (by loop depth), the calls in its body whose laps are amortised
    /// against one index (docs/cost-model.md § An amortised scan)
    amort: Vec<(usize, Vec<Amort>)>,
    /// the scan a `while`'s trip was just found by, as (index, least entry, least growth)
    scan_ind: std::cell::RefCell<Option<(LocalId, Poly, i128)>>,
    /// the pointer chase accumulated in the open frame, framed like `work` (cost-model § Time)
    chase: Cost,
    chase_saved: Vec<Cost>,
    /// the paged lines, framed the same way
    paged: Cost,
    paged_saved: Vec<Cost>,
    /// the bytes moved by concurrent streams, and the write-backs, framed the same way
    conc: Cost,
    conc_saved: Vec<Cost>,
    wback: Cost,
    wback_saved: Vec<Cost>,
    /// an access being walked is a store
    storing: bool,
    /// each loop's laps, by line, and the line of the loop about to be entered
    laps: Vec<(u32, Poly, Poly, bool, bool)>,
    laps_dropped: bool,
    pending_line: u32,
    /// the loop about to be entered stops early or on a condition of several parts
    pending_bounded: bool,
    /// the divisions, framed the same way
    divs: Cost,
    divs_saved: Vec<Cost>,
    /// accesses past the TLB's reach (cost-model § Time, translations), saved and summed as `divs` are
    tlb: Cost,
    tlb_saved: Vec<Cost>,
    /// the serial work, framed the same way
    serial: Cost,
    serial_saved: Vec<Cost>,
    /// locals a loop assigns from a load: an index made of one is the next hop of a chase
    chase_vars: Vec<LocalId>,
}

impl<'a, 'b, 'c> Fa<'a, 'b, 'c> {
    fn new(an: &'b mut Analyzer<'a>, f: &'c Func, self_fid: Option<FuncId>) -> Self {
        let mut fa = Fa {
            an, f, names: param_names(f), sites: vec![], loop_recs: vec![], bounds: vec![], images: HashMap::new(), result_size: None, last_result: None, last_result_atom: None, argc_atom: None, scalar_alias: HashMap::new(), alias_scopes: vec![], notes: vec![], io: false, self_fid, scc: None, rec_calls: vec![],
            local_size: HashMap::new(), local_affine: HashMap::new(), initial: HashMap::new(), at_entry: false,
            loops: vec![], work: Cost::zero(), moves: Cost::zero(), span: Cost::zero(), saved: vec![], branch: vec![], next_if: 0,
            local_root: HashMap::new(), resident: vec![], call_moves: Cost::zero(), saved_calls: vec![], replay: false, has_call: vec![], rests_on: vec![],
            reads: Default::default(), loop_writes: vec![], entry_read: Default::default(), scans: Default::default(), amort: vec![], scan_ind: Default::default(), chase: Cost::zero(), chase_saved: vec![], chase_vars: vec![], paged: Cost::zero(), paged_saved: vec![], conc: Cost::zero(), conc_saved: vec![], wback: Cost::zero(), wback_saved: vec![], storing: false, laps: vec![], laps_dropped: false, pending_line: 0, pending_bounded: false, divs: Cost::zero(), divs_saved: vec![], tlb: Cost::zero(), tlb_saved: vec![], serial: Cost::zero(), serial_saved: vec![],
        };
        for (i, &p) in f.params.iter().enumerate() {
            let l = &f.locals[p];
            if l.ty.is_arrayish() {
                // `[T; k]` by value: its length is the literal (docs/arrays-by-value-design.md §7)
                let n = match &l.ty { Ty::Array(_, Size::Const(k)) if *k >= 0 => Poly::constant(*k as i128), _ => Poly::var(i) };
                fa.local_size.insert(p, n);
                fa.local_root.insert(p, p);
            } else if l.ty == Ty::I64 {
                fa.local_affine.insert(p, Affine::constant(Poly::var(i)));
            }
        }
        if fa.an.m.funcs.iter().any(|g| crate::input::is_builtin(g) && g.name == "arg_count") {
            fa.argc_atom = Some(fa.new_atom("arg_count()"));
        }
        fa
    }

    /// Whether `fid` is the `arg_count()` builtin, whose value is the atom `argc_atom`.
    fn is_argc(&self, fid: FuncId) -> bool {
        let g = &self.an.m.funcs[fid];
        crate::input::is_builtin(g) && g.name == "arg_count"
    }

    fn run(mut self) -> FuncCost {
        // an extern that returns an array names that array's length `result.len()`, one atom past
        // its parameters: its declaration may be written in it, and a caller gets an atom of its
        // own for it (docs/decisions.md, "Program input")
        if self.f.body.is_none() && matches!(self.f.ret, Ty::Array(..)) {
            let a = self.new_atom("result.len()");
            self.result_size = Some(Poly::var(a));
        }
        // the declaration, parsed once: bounds and the size limits budgets are checked under
        let mut declared = Declared::default();
        let mut violations = Vec::new();
        for (key, text, line, _) in &self.f.asserts {
            match key.as_str() {
                "work_at_most" | "moves_at_most" => match super::assert::parse(text, &self.names) {
                    Ok(p) => { if key == "work_at_most" { declared.work = Some(p); } else { declared.moves = Some(p); } }
                    Err(e) => violations.push(format!("line {line}: in `{key} = \"{text}\"`: {e}")),
                },
                "sizes" => {
                    for part in text.split(',') {
                        let Some((name, val)) = part.split_once("<=") else { violations.push(format!("line {line}: `sizes` entries are `name <= value`")); continue };
                        match (self.names.iter().position(|n| n == name.trim()), val.trim().parse::<f64>()) {
                            (Some(i), Ok(v)) => declared.sizes.push((i, v)),
                            _ => violations.push(format!("line {line}: `sizes`: `{}` is not a size of this function or `{}` is not a number", name.trim(), val.trim())),
                        }
                    }
                }
                other => violations.push(format!("line {line}: unknown bound `{other}`; use `work_at_most`, `moves_at_most`, `sizes`")),
            }
        }
        // an extern has no body: its cost is its declaration, or unknown
        let Some(body) = &self.f.body else {
            let effects: Vec<&'static str> = self.f.uses.iter().filter_map(|u| match u.as_str() { "io" => Some("io"), "unbounded" => Some("unbounded"), _ => None }).collect();
            let result = match (&declared.work, &declared.moves) {
                (Some(w), Some(m)) => CostResult::Exact { work: Cost::poly(w.clone()), moves: Cost::poly(m.clone()), span: Cost::poly(w.clone()) },
                _ if effects.contains(&"unbounded") => CostResult::Unknown { reason: "declared unbounded".into(), line: self.f.line },
                _ => CostResult::Unknown { reason: "an extern needs `#[cost(work_at_most = …, moves_at_most = …)]` or `uses unbounded`".into(), line: self.f.line },
            };
            return FuncCost { name: self.f.name.clone(), names: self.names, result, chase: Cost::zero(), paged: Cost::zero(), laps: vec![], laps_dropped: false, conc: Cost::zero(), wback: Cost::zero(), serial: Cost::zero(), divs: Cost::zero(), tlb: Cost::zero(), internal: true, bounds: vec![], notes: vec![], suggestions: vec![], effects, violations, tier: "declared", footprint: vec![], resident: None, declared, rests_on: vec![], result_size: self.result_size.clone() };
        };
        let mut tier = "exact";
        self.chase_vars = loaded_in_loops(body);
        let walked = self.block(body);
        // what this function hands back, if it hands back an array
        if matches!(self.f.ret, Ty::Array(..)) {
            let returned = body.tail.as_ref().map(|t| &t.kind)
                .or_else(|| body.stmts.iter().rev().find_map(|s| match s { Stmt::Return(Some(e)) => Some(&e.kind), _ => None }));
            if let Some(ExprKind::Local(l)) = returned { self.result_size = self.local_size.get(l).cloned(); }
        }
        let (footprint, resident) = self.signature_footprint();
        let result = match walked {
            Ok(()) => {
                self.settle_moves();
                let calls = std::mem::replace(&mut self.call_moves, Cost::zero());
                self.moves = self.moves.add(&calls);
                let m = self.machine();
                self.work.prune_at(&m);
                self.moves.prune_at(&m);
                self.span.prune_at(&m);
                if self.rec_calls.is_empty() {
                    CostResult::Exact { work: self.work.clone(), moves: self.moves.clone(), span: self.span.clone() }
                } else {
                    tier = "recurrence";
                    match self.solve_recurrence() {
                        // span is not threaded through self-recursion (docs/m5-span-design.md
                        // does not attempt it); work stands in, always a safe over-approximation
                        Ok((work, moves)) => CostResult::Exact { span: work.clone(), work, moves },
                        Err(reason) => CostResult::Unknown { reason, line: self.rec_calls[0].2 },
                    }
                }
            }
            Err(Fail::Unknown(reason, line)) => CostResult::Unknown { reason, line },
        };
        // exact modulo the unknown callees it names — unless one of them is this function, reached
        // back through a cycle: a cost stated in terms of itself is a recurrence, not a cost
        let result = match result {
            CostResult::Exact { work, moves, span } if work.has_opaque() || moves.has_opaque() => {
                let mut named = Vec::new();
                work.opaque_callees(&mut named);
                moves.opaque_callees(&mut named);
                if named.contains(&self.f.name) {
                    CostResult::Unknown { reason: "mutually recursive with another function; only self-recursion is solved".into(), line: self.f.line }
                } else {
                    tier = "modulo";
                    CostResult::Exact { work, moves, span }
                }
            }
            other => other,
        };
        // a size that is the most (or least) an array holds makes the cost an upper bound
        if tier != "modulo" {
            if let CostResult::Exact { work, moves, .. } = &result {
                if work.has_loose_read() || moves.has_loose_read() { tier = "bound"; }
            }
        }
        // so does a loop bounded by a scan, here or in a callee (§ A scan)
        let scans = self.scans.take();
        if matches!(result, CostResult::Exact { .. }) {
            if tier == "exact" && (!scans.is_empty() || self.rests_on.iter().any(|r| r.ends_with(SCAN_TAG) || r.ends_with(REGIME_TAG))) { tier = "bound"; }
            for n in scans { if !self.notes.contains(&n) { self.notes.push(n); } }
        }
        // `#[cost(...)]`: the inferred cost must stay within the declaration. Without `sizes`,
        // by asymptotic dominance in every regime; with `sizes`, as numbers at those bounds and
        // the machine's B and M — a budget in real units.
        let m = self.machine();
        let line_of = |key: &str| self.f.asserts.iter().find(|a| a.0 == key).map_or(self.f.line, |a| a.2);
        for (which, asserted) in [("work", declared.work.clone()), ("moves", declared.moves.clone())] {
            let Some(asserted) = asserted else { continue };
            let line = line_of(&format!("{which}_at_most"));
            match &result {
                CostResult::Exact { work, moves, .. } => {
                    let inferred = if which == "work" { work } else { moves };
                    if declared.sizes.is_empty() {
                        for piece in &inferred.pieces {
                            if !super::assert::dominated(&piece.poly, &asserted) {
                                let when = if piece.conds.is_empty() { String::new() } else { format!(" when {}", piece.conds.iter().map(|c| c.display(&self.names)).collect::<Vec<_>>().join(" and ")) };
                                violations.push(format!(
                                    "line {line}: `{}` is asserted {which} at most {} but its {which} is {}{when}",
                                    self.f.name, asserted.display(&self.names), piece.poly.display(&self.names)));
                            }
                        }
                    } else {
                        let at = |a: Atom| -> Option<f64> {
                            match a {
                                Atom::B => Some(m.b_bytes as f64), Atom::M => Some(m.m_bytes as f64), Atom::P => Some(m.p_cores as f64),
                                Atom::Var(i) => declared.sizes.iter().find(|(v, _)| *v == i).map(|(_, x)| *x),
                                Atom::Log(_) | Atom::Opaque(_) | Atom::Read(_) => None,
                            }
                        };
                        match (inferred.eval(&at, &m), asserted.eval(&at)) {
                            (Some(got), Some(limit)) if got > limit => violations.push(format!(
                                "line {line}: `{}` has a {which} budget of {limit:.0} at the declared sizes but needs {got:.0}", self.f.name)),
                            (None, _) | (_, None) => violations.push(format!("line {line}: `{}`'s {which} budget cannot be evaluated: every size the cost mentions needs a bound in `sizes`", self.f.name)),
                            _ => {}
                        }
                    }
                }
                CostResult::Unknown { reason, .. } => {
                    violations.push(format!("line {line}: `{}` asserts {which} at most {} but its cost is unknown: {reason}", self.f.name, asserted.display(&self.names)));
                }
            }
        }
        let effects = if self.io { vec!["io"] } else { vec![] };
        // the footprint bound: distinct elements of parameter arrays, each crossing once from cold
        let mut foot = Poly::zero();
        let mut keys: Vec<&(LocalId, Option<usize>)> = self.images.keys().collect();
        keys.sort();
        for k in keys { if self.f.params.contains(&k.0) { foot = foot.add(&self.images[k]); } }
        if !foot.is_zero() {
            self.bounds.push(Bound { kind: "footprint".into(), citation: "every distinct element crosses once".into(), moves: foot, line: self.f.line, cold: true });
        }
        // a bound still in a loop's atom — a read at the loop variable, left behind when the loop
        // closed — is in no size a caller can name; dropping it only weakens what is claimed
        let np = self.f.params.len();
        self.bounds.retain(|b| b.moves.vars().iter().all(|&v| v < np));
        bounds::strongest_first(&mut self.bounds, &m);
        let chase = if matches!(result, CostResult::Exact { .. }) { self.chase.clone() } else { Cost::zero() };
        let paged = if matches!(result, CostResult::Exact { .. }) { self.paged.clone() } else { Cost::zero() };
        let conc = if matches!(result, CostResult::Exact { .. }) { self.conc.clone() } else { Cost::zero() };
        let laps = if matches!(result, CostResult::Exact { .. }) { std::mem::take(&mut self.laps) } else { vec![] };
        let wback = if matches!(result, CostResult::Exact { .. }) { self.wback.clone() } else { Cost::zero() };
        let serial = if matches!(result, CostResult::Exact { .. }) { self.serial.clone() } else { Cost::zero() };
        let divs = if matches!(result, CostResult::Exact { .. }) { self.divs.clone() } else { Cost::zero() };
        let tlb = if matches!(result, CostResult::Exact { .. }) { self.tlb.clone() } else { Cost::zero() };
        let internal = self.sites.iter().any(|st| { let r = self.local_root.get(&st.arr).copied().unwrap_or(st.arr); !self.f.params.contains(&r) });
        FuncCost { name: self.f.name.clone(), names: self.names, result, chase, paged, laps, laps_dropped: self.laps_dropped, conc, wback, serial, divs, tlb, internal, bounds: self.bounds, notes: self.notes, suggestions: vec![], effects, violations, tier, footprint, resident, declared, rests_on: self.rests_on, result_size: self.result_size.clone() }
    }

    /// The byte range one access site covers over its loop nest, from its affine index and the
    /// ranges of the loop variables it mentions; `None` when a coefficient's sign is not known or
    /// the index is not affine.
    fn site_range(&self, site: &Site) -> Option<(Poly, Poly)> {
        let aff = site.aff.as_ref()?;
        let mut lo = aff.konst.clone();
        let mut hi = aff.konst.clone();
        for (v, c) in &aff.coeffs {
            let rec = site.path.iter().map(|&l| &self.loop_recs[l]).find(|r| r.var == Some(*v))?;
            let negative = match self.numeric(c) {
                Some(x) => x < 0.0,
                None => { if c.terms.values().all(|k| k.n >= 0) { false } else { return None; } }
            };
            // **the affine form already carries the loop's start**, since `affine()` gives a loop
            // variable `var + offset` so that an index is seen to move with the loops outside it.
            // The variable's own contribution therefore runs from 0, not from `rec.lo` — adding
            // `rec.lo` here counted the start twice and shifted the whole range right by it
            // (docs/experiments.md, "A bug in the reference compiler").
            let span = rec.trip.sub(&Poly::constant(1)).scale(Rat::int(rec.step));
            // which end the span extends is the sign of `c·step`: a loop counting down with a
            // positive coefficient walks toward lower addresses, and taking `c`'s sign alone
            // gave `sym_find`'s `i = n − 1` down to 0 the range `[32·n − 32, 40)`
            let b = c.mul(&span);
            if negative != (rec.step < 0) { lo = lo.add(&b); } else { hi = hi.add(&b); }
        }
        let st = Rat::int(site.stride);
        Some((lo.scale(st).add(&site.base), hi.scale(st).add(&Poly::constant(site.es)).add(&site.base)))
    }

    /// A site's range with every loop atom of its nest gone: a range whose ends move with an outer
    /// loop (`j in i + 1..n`) is widened to its hull over that loop's laps, each end taken at the lap
    /// where it is furthest out; `None` when neither lap's end dominates the other's.
    fn hull_over_laps(&self, site: &Site, (mut lo, mut hi): (Poly, Poly)) -> Option<(Poly, Poly)> {
        use super::piece::dominates;
        for &l in &site.path {
            let rec = &self.loop_recs[l];
            let Some(a) = rec.atom else { continue };
            if !lo.mentions(a) && !hi.mentions(a) { continue; }
            let (l0, l1) = (lo.subst(a, &rec.lo), lo.subst(a, &rec.last()));
            let (h0, h1) = (hi.subst(a, &rec.lo), hi.subst(a, &rec.last()));
            lo = if dominates(&l1, &l0) { l0 } else if dominates(&l0, &l1) { l1 } else { return None };
            hi = if dominates(&h1, &h0) { h1 } else if dominates(&h0, &h1) { h0 } else { return None };
        }
        // the ends are each taken at their own lap, so the hull can overrun what the site can touch:
        // clamp it to the site's own region, its field's under SoA, the array's under AoS
        let root = self.local_root.get(&site.arr).copied().unwrap_or(site.arr);
        if let Some(len) = self.local_size.get(&root) {
            let (rlo, rhi) = (site.base.clone(), site.base.add(&len.scale(Rat::int(site.stride))));
            if dominates(&rlo, &lo) { lo = rlo; }
            if dominates(&hi, &rhi) { hi = rhi; }
        }
        Some((lo, hi))
    }

    /// The function's footprint over its parameters, and the condition under which all of it is
    /// resident on return. A site whose range is not exact makes its parameter's range the whole
    /// array and forfeits the residue.
    fn signature_footprint(&self) -> (Vec<Foot>, Option<Cond>) {
        // per parameter, its ranges: disjoint exact ones kept apart — two fields of one element
        // under SoA are two ranges `8·n` apart, not the whole array (cost-model § Moves, footprint)
        let mut feet: HashMap<usize, Vec<(Poly, Poly, bool)>> = HashMap::new();
        let mut total = Poly::zero();
        let mut all_exact = true;
        for site in &self.sites {
            let root = self.local_root.get(&site.arr).copied().unwrap_or(site.arr);
            let whole = |r: LocalId| (Poly::zero(), self.local_size.get(&r).cloned().unwrap_or_else(Poly::zero).scale(Rat::int(self.elem_bytes(r))));
            let Some(pi) = self.f.params.iter().position(|&p| p == root) else {
                // an internal array: competes for the cache, invisible to the caller
                let (_, h) = whole(root);
                feet.entry(usize::MAX - root).or_insert_with(|| vec![(Poly::zero(), h, true)]);
                continue;
            };
            // a range whose end is the most or least an array holds is a hull over elements, not
            // a range every byte of which was read; a scan's site is a hull too
            let scanned = site.path.iter().any(|&l| self.loop_recs[l].monotone);
            let (lo, hi, exact) = match self.site_range(site).and_then(|r| self.hull_over_laps(site, r)) { Some((l, h)) => { let ex = !scanned && !l.has_loose_read() && !h.has_loose_read(); (l, h, ex) } None => { let (l, h) = whole(root); (l, h, false) } };
            let list = feet.entry(pi).or_default();
            if list.iter().any(|r| !r.2) || !exact {
                // anything inexact on a parameter makes it the whole array, inexact
                let (l, h) = whole(root);
                *list = vec![(l, h, false)];
                continue;
            }
            // the same range twice is one; one that overlaps or touches a range is their union;
            // one apart from every range is a range of its own
            let mut merged = false;
            for r in list.iter_mut() {
                if r.0 == lo && r.1 == hi { merged = true; break; }
                if super::piece::dominates(&r.1, &lo) && super::piece::dominates(&hi, &r.0) {
                    let nlo = if super::piece::dominates(&r.0, &lo) { lo.clone() } else { r.0.clone() };
                    let nhi = if super::piece::dominates(&hi, &r.1) { hi.clone() } else { r.1.clone() };
                    *r = (nlo, nhi, true);
                    merged = true;
                    break;
                }
            }
            if !merged {
                // apart only where it can be shown: the new range ends before a range starts or
                // starts after it ends; otherwise the whole array, inexact
                let apart = list.iter().all(|r| super::piece::dominates(&r.0, &hi) || super::piece::dominates(&lo, &r.1));
                if apart { list.push((lo, hi, true)); } else { let (l, h) = whole(root); *list = vec![(l, h, false)]; }
            }
        }
        for (_, rs) in &feet { for (lo, hi, exact) in rs { total = total.add(&hi.sub(lo)); if !exact { all_exact = false; } } }
        let mut out: Vec<Foot> = feet.into_iter().filter(|(k, _)| *k < usize::MAX / 2)
            .flat_map(|(param, rs)| rs.into_iter().map(move |(lo, hi, exact)| Foot { param, lo, hi, exact }))
            .collect();
        out.sort_by(|a, b| a.param.cmp(&b.param).then_with(|| format!("{:?}", a.lo).cmp(&format!("{:?}", b.lo))));
        let resident = if all_exact && !out.is_empty() { Some(Cond { ws: total.mul_atom_pow(Atom::B, Rat::int(-1)), fits: true }) } else { None };
        (out, resident)
    }

    /// The body's own cost `f` (self-calls charged nothing) and the self-calls make a
    /// recurrence in some measure `m` that every call shrinks: an `i64` parameter, `len − p`,
    /// or `hi − lo`. Shrinking by a constant with one call is a sum, `T = f·m/c`; with more
    /// calls it is exponential and refused. Shrinking by a factor `b` with `a` calls is the
    /// master theorem on the degree `d` of `f` in `m`.
    fn solve_recurrence(&self) -> Result<(Cost, Cost), String> {
        let mut a_poly = Poly::zero();
        for (_, o, _) in &self.rec_calls { a_poly = a_poly.add(o); }
        let a = match a_poly.as_const() {
            Some(c) if c.is_int() && c.n >= 1 => c.n,
            _ => return Err("the number of recursive calls per invocation depends on the input".into()),
        };
        // candidate measures over the parameter atoms
        let mut cands: Vec<Poly> = Vec::new();
        let ps: Vec<(usize, &Local)> = self.f.params.iter().enumerate().map(|(i, &p)| (i, &self.f.locals[p])).collect();
        for (i, l) in &ps { if l.ty == Ty::I64 { cands.push(Poly::var(*i)); } }
        for (i, l) in &ps { if l.ty == Ty::I64 { for (j, s) in &ps { if s.ty.is_arrayish() { cands.push(Poly::var(*j).sub(&Poly::var(*i))); } } } }
        for (i, l) in &ps { if l.ty == Ty::I64 { for (k, q) in &ps { if k != i && q.ty == Ty::I64 { cands.push(Poly::var(*k).sub(&Poly::var(*i))); } } } }
        #[derive(PartialEq, Clone, Copy)]
        enum Shrink { Linear(i128), Div(i128) }
        for m in &cands {
            let mvars = m.vars();
            let mut shrink: Option<Shrink> = None;
            let mut ok = true;
            for (args, _, _) in &self.rec_calls {
                let mut map = Vec::new();
                for &v in &mvars {
                    match args.get(v) { Some(Some(p)) => map.push((v, p.clone())), _ => { ok = false; break; } }
                }
                if !ok { break; }
                let mp = m.subst_many(&map);
                let d = m.sub(&mp);
                let this = if let Some(c) = d.as_const() {
                    if c.n > 0 && c.is_int() { Shrink::Linear(c.n) } else { ok = false; break; }
                } else {
                    let mut found = None;
                    for b in 2..=8i128 {
                        if let Some(k) = mp.scale(Rat::int(b)).sub(m).as_const() { if k.n <= 0 { found = Some(Shrink::Div(b)); break; } }
                    }
                    match found { Some(s) => s, None => { ok = false; break; } }
                };
                match (shrink, this) {
                    (None, s) => shrink = Some(s),
                    (Some(Shrink::Linear(c1)), Shrink::Linear(c2)) => shrink = Some(Shrink::Linear(c1.min(c2))),
                    (Some(Shrink::Div(b1)), Shrink::Div(b2)) if b1 == b2 => {}
                    _ => { ok = false; break; }
                }
            }
            if !ok { continue; }
            // how each parameter in m moves per call: v ↦ v + δ_v (the linear case) — needed to
            // unroll the recurrence exactly
            let shifts: Option<Vec<(usize, Rat)>> = {
                let (args, _, _) = &self.rec_calls[0];
                mvars.iter().map(|&v| {
                    let ap = args.get(v).cloned().flatten()?;
                    let d = ap.sub(&Poly::var(v));
                    d.as_const().map(|c| (v, c))
                }).collect()
            };
            let solve = |f: &Poly| -> Result<Poly, String> {
                match shrink.unwrap() {
                    Shrink::Linear(c) => {
                        if a > 1 { return Err(format!("{a} recursive calls each shrinking `{}` by a constant: exponential", m.display(&self.names))); }
                        // T(m) = f(m) + T(m − c): unrolled, T = Σ_{j=0}^{m/c} f with every
                        // parameter of m advanced j steps along its shift — exact by summation
                        const J: usize = usize::MAX / 2 - 1;
                        let trip = m.scale(Rat::new(1, c)).add(&Poly::constant(1));
                        match &shifts {
                            Some(sh) => {
                                let map: Vec<(usize, Poly)> = sh.iter().map(|(v, d)| (*v, Poly::var(*v).add(&Poly::var(J).scale(*d)))).collect();
                                let fj = f.subst_many(&map);
                                Ok(fj.sum_over(J, &Poly::zero(), 1, &trip))
                            }
                            None => Ok(f.mul(&trip)),
                        }
                    }
                    Shrink::Div(b) => {
                        // T(m) = a·T(m/b) + f(m): each monomial g of f, of degree d in m, is paid
                        // once per level with weight (a/b^d)^i, i = 0..log_b m — a geometric series
                        let mut total = Poly::zero();
                        let log2b = (b as f64).log2();
                        let inv_log2b = if (log2b - log2b.round()).abs() < 1e-9 { Rat::new(1, log2b.round() as i128) } else { Rat::new((1000.0 / log2b).round() as i128, 1000) };
                        for (mono, coef) in &f.terms {
                            let mut g = Poly::zero();
                            g.terms.insert(mono.clone(), *coef);
                            let d = mono.factors.iter().filter(|(at, _)| matches!(at, Atom::Var(i) if mvars.contains(i))).fold(Rat::zero(), |acc, (_, e)| acc.add(*e));
                            if !d.is_int() || d.n < 0 { return Err("a fractional power of the measure in the body cost".into()); }
                            let bd: i128 = b.pow(d.n as u32);
                            if a < bd {
                                // Σ (a/b^d)^i ≤ b^d / (b^d − a)
                                total = total.add(&g.scale(Rat::new(bd, bd - a)));
                            } else if a == bd {
                                // log_b m + 1 levels, each paying g
                                let levels = Poly::atom(Atom::Log(Box::new(m.clone()))).scale(inv_log2b).add(&Poly::constant(1));
                                total = total.add(&g.mul(&levels));
                            } else {
                                // ((a/b^d)^{L+1} − 1)/((a/b^d) − 1) with (a/b^d)^L = m^{log_b a − d}
                                let k = (a as f64).ln() / (b as f64).ln();
                                let lift = k - d.n as f64;
                                let mk = if (lift - lift.round()).abs() < 1e-9 {
                                    m.pow(lift.round() as i128)
                                } else if mvars.len() == 1 {
                                    Poly::var(mvars[0]).mul_atom_pow(Atom::Var(mvars[0]), Rat::new((lift * 1000.0).round() as i128 - 1000, 1000))
                                } else {
                                    return Err(format!("recurrence T = {a}·T(m/{b}) + Θ(m^{}) has a non-integer exponent on a compound measure", d.n));
                                };
                                let r = Rat::new(a, bd); // a/b^d > 1
                                let num = mk.scale(r).sub(&Poly::constant(1));
                                total = total.add(&g.mul(&num).scale(Rat::new(r.d, r.n - r.d)));
                            }
                        }
                        Ok(total)
                    }
                }
            };
            // each unconditional alternative of the body cost is solved on its own; a condition
            // inside a recursive body would shift with the unrolling and is not solved yet
            let solve_cost = |c: &Cost| -> Result<Cost, String> {
                let mut out: Vec<super::piece::Piece> = Vec::new();
                for piece in &c.pieces {
                    if !piece.conds.is_empty() { return Err("a cache-dependent cost inside a recursive body is not solved yet".into()); }
                    out.push(super::piece::Piece { conds: vec![], poly: solve(&piece.poly)? });
                }
                let mut r = Cost { pieces: out };
                r.prune();
                Ok(r)
            };
            return Ok((solve_cost(&self.work)?, solve_cost(&self.moves)?));
        }
        Err("no argument shrinks toward a base case across every recursive call; the measure must be an `i64` parameter, `xs.len() − p`, or `hi − lo`".into())
    }

    fn machine(&self) -> Machine { self.an.machine }

    /// How many times the current point runs per invocation: `Σ` of 1 over the enclosing loops,
    /// innermost first — the product of the trips when they are independent, the exact count
    /// when an inner bound mentions an outer variable.
    fn times_here(&self) -> Poly {
        let mut o = Poly::constant(1);
        for l in self.loops.iter().rev() {
            o = match l.atom { Some(a) => o.sum_over(a, &l.lo, l.step, &l.trip), None => o.mul(&l.trip) };
        }
        o
    }
    /// Cost of the current point, charged once; the enclosing loops sum it when they are left.
    fn add_work(&mut self, p: Poly) {
        if !self.replay { self.work = self.work.add_poly(&p); self.span = self.span.add_poly(&p); }
    }
    /// A fresh size atom for a loop variable.
    fn new_atom(&mut self, name: &str) -> usize {
        self.names.push(name.to_string());
        self.names.len() - 1
    }
    /// A loop whose sum decided a fit test in its own variable at two ends on two sides of `M`
    /// (`Cost::sum_split`) is charged a bound there, and says so as a scan does.
    fn note_straddle(&self, rec: &LoopRec) {
        if !rec.straddled.get() { return; }
        let v = rec.var.map_or("its variable".to_string(), |v| format!("`{}`", self.f.locals[v].name));
        let n = format!("regime: a fit test inside the loop over {v} depends on {v}; where it changes inside the loop, each side is charged every lap, a bound");
        let mut s = self.scans.borrow_mut();
        if !s.contains(&n) { s.push(n); }
    }
    /// Open a loop: push it and start a fresh accumulator frame for its body.
    fn enter_loop(&mut self, lp: Loop) {
        let entries = self.times_here();
        let bounded = self.pending_bounded || lp.monotone || lp.trip.has_loose_read();
        // a trip that moves with a loop outside, is a value read from memory, or a scan's: the
        // exit branch is not the same each entry
        let varies = lp.monotone || self.loops.iter().any(|l| l.atom.is_some_and(|a| lp.trip.mentions(a)))
            || { let mut rs = Vec::new(); lp.trip.reads(&mut rs); !rs.is_empty() };
        self.loop_recs.push(LoopRec { var: lp.var, atom: lp.atom, trip: lp.trip.clone(), lo: lp.lo.clone(), step: lp.step, monotone: lp.monotone, straddled: Default::default() });
        let lp = Loop { id: self.loop_recs.len() - 1, ..lp };
        self.loops.push(lp);
        if !self.replay && self.pending_line > 0 { let t = self.times_here(); self.laps.push((self.pending_line, entries, t, varies, bounded)); }
        self.push_frame();
    }
    fn push_frame(&mut self) {
        self.saved.push((std::mem::replace(&mut self.work, Cost::zero()), std::mem::replace(&mut self.moves, Cost::zero()), std::mem::replace(&mut self.span, Cost::zero())));
        self.saved_calls.push(std::mem::replace(&mut self.call_moves, Cost::zero()));
        self.chase_saved.push(std::mem::replace(&mut self.chase, Cost::zero()));
        self.paged_saved.push(std::mem::replace(&mut self.paged, Cost::zero()));
        self.conc_saved.push(std::mem::replace(&mut self.conc, Cost::zero()));
        self.wback_saved.push(std::mem::replace(&mut self.wback, Cost::zero()));
        self.divs_saved.push(std::mem::replace(&mut self.divs, Cost::zero()));
        self.tlb_saved.push(std::mem::replace(&mut self.tlb, Cost::zero()));
        self.serial_saved.push(std::mem::replace(&mut self.serial, Cost::zero()));
        self.has_call.push(false);
    }
    /// Pop a frame (an `if` branch). Its calls' moves are handed back with the rest, not added to
    /// the frame below: the two branches are alternatives, and the caller takes the larger.
    fn pop_frame(&mut self) -> (Cost, Cost, Cost, Cost, Cost, Cost, Cost, Cost, Cost, Cost) {
        let (pw, pm, psp) = self.saved.pop().unwrap();
        let pc = self.saved_calls.pop().unwrap();
        let pch = self.chase_saved.pop().unwrap();
        let ch = std::mem::replace(&mut self.chase, pch);
        let ppg = self.paged_saved.pop().unwrap();
        let pg = std::mem::replace(&mut self.paged, ppg);
        let pcc = self.conc_saved.pop().unwrap();
        let cc = std::mem::replace(&mut self.conc, pcc);
        let pwb = self.wback_saved.pop().unwrap();
        let wb = std::mem::replace(&mut self.wback, pwb);
        let pdv = self.divs_saved.pop().unwrap();
        let dv = std::mem::replace(&mut self.divs, pdv);
        // a branch's translations go back into the frame below, both branches' together: more
        // than the one that runs, and only a time reads them
        let ptl = self.tlb_saved.pop().unwrap();
        let tl = std::mem::replace(&mut self.tlb, ptl);
        self.tlb = self.tlb.add(&tl);
        let pse = self.serial_saved.pop().unwrap();
        let se = std::mem::replace(&mut self.serial, pse);
        let had = self.has_call.pop().unwrap_or(false);
        if let Some(h) = self.has_call.last_mut() { *h |= had; }
        let calls = std::mem::replace(&mut self.call_moves, pc);
        (std::mem::replace(&mut self.work, pw), std::mem::replace(&mut self.moves, pm), std::mem::replace(&mut self.span, psp), ch, calls, pg, se, dv, cc, wb)
    }
    /// Leave a loop. The body frame is summed over the loop variable. Calls in the body are costed
    /// twice: as walked (cold, the first iteration) and again with the residue the first iteration
    /// left (the iterations after it), so a call that re-reads what the last one left in cache
    /// pays once.
    fn leave_loop(&mut self, body: &Block) -> Result<(), Fail> {
        let lp = self.loops.pop().unwrap();
        let (pw, pm, psp) = self.saved.pop().unwrap();
        let pc = self.saved_calls.pop().unwrap();
        let pch = self.chase_saved.pop().unwrap();
        let body_chase = std::mem::replace(&mut self.chase, pch);
        let ppg = self.paged_saved.pop().unwrap();
        let body_paged = std::mem::replace(&mut self.paged, ppg);
        let pcc = self.conc_saved.pop().unwrap();
        let body_conc = std::mem::replace(&mut self.conc, pcc);
        let pwb = self.wback_saved.pop().unwrap();
        let body_wback = std::mem::replace(&mut self.wback, pwb);
        let pdv = self.divs_saved.pop().unwrap();
        let body_divs = std::mem::replace(&mut self.divs, pdv);
        let ptl = self.tlb_saved.pop().unwrap();
        let body_tlb = std::mem::replace(&mut self.tlb, ptl);
        let pse = self.serial_saved.pop().unwrap();
        let body_serial = std::mem::replace(&mut self.serial, pse);
        let had_call = self.has_call.pop().unwrap_or(false);
        let cold_calls = std::mem::replace(&mut self.call_moves, pc);
        let (w, m) = (std::mem::replace(&mut self.work, pw), std::mem::replace(&mut self.moves, pm));
        let sp = std::mem::replace(&mut self.span, psp);
        let rec = self.loop_recs[lp.id].clone();
        self.work = self.work.add(&rec.sum_cost(&w));
        self.moves = self.moves.add(&rec.sum_cost(&m));
        self.chase = self.chase.add(&rec.sum_cost(&body_chase));
        self.paged = self.paged.add(&rec.sum_cost(&body_paged));
        self.conc = self.conc.add(&rec.sum_cost(&body_conc));
        self.wback = self.wback.add(&rec.sum_cost(&body_wback));
        self.divs = self.divs.add(&rec.sum_cost(&body_divs));
        self.tlb = self.tlb.add(&rec.sum_cost(&body_tlb));
        // a loop whose lap waits on the last one: all its work is serial, nested loops' included
        // through memory only the chained stores wait, one unit each a lap, and an `f64` carried
        // through an add waits for the add, one unit a lap, in a short lap
        // an add's latency binds only a lap with less work than it: more, and the core runs the
        // lap's other work while the add waits (`horner`'s eight multiplies; `matmul`'s next `j`)
        let mach = self.machine();
        let short = w.pieces.len() == 1 && w.pieces[0].poly.as_const().is_some_and(|c| c.to_f64() * mach.ns_per_work < mach.ns_per_serial);
        // a store that reads the element the last lap stored is a recurrence through memory: the
        // whole lap when the value passes a multiply or divide on its way, one unit when only adds
        let lap = lap_chain(body, rec.var, rec.step);
        let chained = memory_chain(body, rec.var) + if short { float_chain(body, self.f) } else { 0 } + usize::from(lap == Some(false));
        self.serial = self.serial.add(&rec.sum_cost(&if carried_chain(body) || lap == Some(true) { w.clone() } else { body_serial.add_poly(&Poly::constant(chained as i128)) }));
        // an ordinary loop is sequential: its span sums the same way work does
        self.span = self.span.add(&rec.sum_cost(&sp));
        self.note_straddle(&rec);
        if had_call {
            let first = match rec.atom { Some(a) => cold_calls.subst(a, &rec.lo), None => cold_calls.clone() };
            let warm = if self.numeric(&rec.trip).is_some_and(|t| t <= 1.0) { Cost::zero() } else {
                self.loops.push(Loop { id: lp.id, ..lp });
                let outer_replay = self.replay;
                self.replay = true;
                self.push_frame();
                // what the walk left resident is the *previous* iteration's: in the loop's own
                // variable it is at `i − step`, or a call at `xs[i]` is credited for re-reading
                // the element it reads for the first time
                // a scan's last lap is not `step` back: a jump leaves no residue to be sure of
                if let (Some(a), true) = (rec.atom, rec.monotone) {
                    self.resident.retain(|r| !(r.lo.mentions(a) || r.hi.mentions(a) || r.conds.iter().any(|c| c.ws.mentions(a))));
                } else if let Some(a) = rec.atom {
                    let back = Poly::var(a).sub(&Poly::constant(rec.step));
                    for r in &mut self.resident {
                        r.lo = r.lo.subst(a, &back);
                        r.hi = r.hi.subst(a, &back);
                        for c in &mut r.conds { c.ws = c.ws.subst(a, &back); }
                    }
                }
                let r = self.block(body);
                // keep only the recounted call moves; everything else returns to what it was
                let (pw, pm, psp) = self.saved.pop().unwrap();
                let pc = self.saved_calls.pop().unwrap();
                self.chase = self.chase_saved.pop().unwrap();
                self.paged = self.paged_saved.pop().unwrap();
                self.conc = self.conc_saved.pop().unwrap();
                self.wback = self.wback_saved.pop().unwrap();
                self.divs = self.divs_saved.pop().unwrap();
                self.tlb = self.tlb_saved.pop().unwrap();
                self.serial = self.serial_saved.pop().unwrap();
                self.has_call.pop();
                let warm_body = std::mem::replace(&mut self.call_moves, pc);
                self.work = pw;
                self.moves = pm;
                self.span = psp;
                self.replay = outer_replay;
                self.loops.pop();
                r?;
                let rest_lo = rec.lo.add(&Poly::constant(rec.step));
                let rest_trip = rec.trip.sub(&Poly::constant(1));
                match rec.atom { Some(a) => warm_body.sum_over(a, &rest_lo, rec.step, &rest_trip), None => warm_body.mul_poly(&rest_trip) }
            };
            self.call_moves = self.call_moves.add(&first).add(&warm);
            if let Some(h) = self.has_call.last_mut() { *h = true; }
        }
        Ok(())
    }
    /// Leave a `.par()` loop (m5-span-design.md §3). Work and moves sum over the trip count
    /// exactly as `leave_loop` would — parallelism changes nothing about what is computed or what
    /// bytes move. Span does not: it is one iteration's own cost (never trip-many copies) plus the
    /// reduction tree's own depth, `⌈log₂ trip⌉`. Calls inside the body are costed cold at every
    /// iteration, with no warm-residue discount — simpler than `leave_loop`'s, and conservative in
    /// the safe direction; this design does not attempt the residue argument for a parallel loop.
    fn leave_par_loop(&mut self, _body: &Block, trip: &Poly) -> Result<(), Fail> {
        let lp = self.loops.pop().unwrap();
        let (pw, pm, psp) = self.saved.pop().unwrap();
        let pc = self.saved_calls.pop().unwrap();
        let pch = self.chase_saved.pop().unwrap();
        let body_chase = std::mem::replace(&mut self.chase, pch);
        let ppg = self.paged_saved.pop().unwrap();
        let body_paged = std::mem::replace(&mut self.paged, ppg);
        let pcc = self.conc_saved.pop().unwrap();
        let body_conc = std::mem::replace(&mut self.conc, pcc);
        let pwb = self.wback_saved.pop().unwrap();
        let body_wback = std::mem::replace(&mut self.wback, pwb);
        let pdv = self.divs_saved.pop().unwrap();
        let body_divs = std::mem::replace(&mut self.divs, pdv);
        let ptl = self.tlb_saved.pop().unwrap();
        let body_tlb = std::mem::replace(&mut self.tlb, ptl);
        let pse = self.serial_saved.pop().unwrap();
        let body_serial = std::mem::replace(&mut self.serial, pse);
        let had_call = self.has_call.pop().unwrap_or(false);
        let cold_calls = std::mem::replace(&mut self.call_moves, pc);
        let (w, m) = (std::mem::replace(&mut self.work, pw), std::mem::replace(&mut self.moves, pm));
        self.span = psp;
        let rec = self.loop_recs[lp.id].clone();
        self.work = self.work.add(&rec.sum_cost(&w));
        self.moves = self.moves.add(&rec.sum_cost(&m));
        self.chase = self.chase.add(&rec.sum_cost(&body_chase));
        self.paged = self.paged.add(&rec.sum_cost(&body_paged));
        self.conc = self.conc.add(&rec.sum_cost(&body_conc));
        self.wback = self.wback.add(&rec.sum_cost(&body_wback));
        self.divs = self.divs.add(&rec.sum_cost(&body_divs));
        self.tlb = self.tlb.add(&rec.sum_cost(&body_tlb));
        self.serial = self.serial.add(&rec.sum_cost(&body_serial));
        self.note_straddle(&rec);
        let depth = Cost::poly(Poly::atom(Atom::Log(Box::new(trip.clone()))));
        self.span = self.span.add(&w.add(&depth));
        if had_call {
            self.call_moves = self.call_moves.add(&cold_calls.mul_poly(trip));
            if let Some(h) = self.has_call.last_mut() { *h = true; }
        }
        Ok(())
    }
    fn add_work_n(&mut self, n: i128) { self.add_work(Poly::constant(n)); }

    /// Numeric value of a polynomial with `B` and `M` at their machine values, if it has no
    /// size variables. The fit test and the stride test are decided with this.
    fn numeric(&self, p: &Poly) -> Option<f64> {
        let m = self.machine();
        if p.has_vars() { return None; }
        p.eval(&|a| match a {
            Atom::B => Some(m.b_bytes as f64),
            Atom::M => Some(m.m_bytes as f64),
            Atom::P => Some(m.p_cores as f64),
            Atom::Var(_) | Atom::Log(_) | Atom::Opaque(_) | Atom::Read(_) => None,
        })
    }

    // ---- size expressions ----

    /// An `i64` expression as a size polynomial: literals, size parameters, `.len()`, immutable
    /// lets bound to such, `+ - *` of them, and loop variables as their atoms — so a triangular
    /// bound `i..n` is the exact `n − i`, summed when the outer loop is left. `Dir` only decides
    /// which side of a `min`/`max` to take.
    fn size_of(&self, e: &Expr, bound: Dir) -> Option<Poly> {
        let flip = |b: Dir| if b == Dir::Upper { Dir::Lower } else { Dir::Upper };
        match &e.kind {
            ExprKind::Int(v) => Some(Poly::constant(*v as i128)),
            ExprKind::Local(l) => {
                if let Some(lp) = self.loops.iter().find(|lp| lp.var == Some(*l) && !lp.monotone) {
                    return lp.atom.map(Poly::var);
                }
                if self.at_entry && self.f.locals[*l].mutable { return self.entry_value(*l); }
                let a = self.local_affine.get(l)?;
                if !a.is_const() { return None; }
                Some(a.konst.clone())
            }
            ExprKind::Len(l) => self.local_size.get(l).cloned(),
            ExprKind::Call(fid, _) if self.is_argc(*fid) => self.argc_atom.map(Poly::var),
            ExprKind::Cast(inner, Ty::I64) => self.size_of(inner, bound),
            // min is bounded above by either argument, max below by either: the first that is a size
            ExprKind::InRow(j, _) => self.size_of(j, bound),
            ExprKind::MinMax(true, a, b) if bound == Dir::Upper => self.size_of(a, bound).or_else(|| self.size_of(b, bound)),
            ExprKind::MinMax(false, a, b) if bound == Dir::Lower => self.size_of(a, bound).or_else(|| self.size_of(b, bound)),
            ExprKind::Binary(BinOp::Add, a, b) => Some(self.size_of(a, bound)?.add(&self.size_of(b, bound)?)),
            ExprKind::Binary(BinOp::Sub, a, b) => Some(self.size_of(a, bound)?.sub(&self.size_of(b, flip(bound))?)),
            ExprKind::Binary(BinOp::Mul, a, b) => {
                let pa = self.size_of(a, bound)?;
                let pb = self.size_of(b, bound)?;
                Some(pa.mul(&pb))
            }
            ExprKind::Binary(BinOp::Div, a, b) => {
                let pb = self.size_of(b, bound)?;
                match pb.as_const() {
                    Some(d) => { if d.is_zero() || !d.is_int() { return None; } Some(self.size_of(a, bound)?.scale(Rat::new(1, d.n))) }
                    // a symbolic divisor that is one size, `n / T`: exact when it divides, and the
                    // tile rewrite that produces it says so
                    None => Some(self.size_of(a, bound)?.mul(&pb.inv_mono()?)),
                }
            }
            ExprKind::Block(b) if b.stmts.is_empty() => self.size_of(b.tail.as_ref()?, bound),
            ExprKind::Index(arr, idx) if self.f.locals[*arr].ty.elem() == Some(&Ty::I64) => self.read(*arr, idx, None, bound),
            ExprKind::Field(inner, fi) => match &inner.kind {
                ExprKind::Index(arr, idx) if self.field_ty(*arr, *fi) == Some(&Ty::I64) => self.read(*arr, idx, Some(*fi), bound),
                _ => None,
            },
            _ => None,
        }
    }

    /// A size that is exactly this expression's value, not one side of a bound on it.
    fn exact_size(&self, e: &Expr) -> Option<Poly> {
        match (self.size_of(e, Dir::Upper), self.size_of(e, Dir::Lower)) {
            (Some(a), Some(b)) if a == b => Some(a),
            _ => None,
        }
    }

    fn field_ty(&self, arr: LocalId, fi: usize) -> Option<&'a Ty> {
        match self.f.locals[arr].ty.elem() {
            Some(Ty::Struct(sid)) => self.an.m.structs[*sid].fields.get(fi).map(|(_, t)| t),
            _ => None,
        }
    }

    /// An `i64` read from an array as a size: the value the read returns, as an atom of its own
    /// (stage D). Not a size inside a loop that writes the array, where it changes from one
    /// iteration to the next. The element is a size when its index is one exactly, and `_`
    /// otherwise, which makes the atom the most any element holds.
    fn read(&self, arr: LocalId, idx: &Expr, field: Option<usize>, bound: Dir) -> Option<Poly> {
        let root = self.local_root.get(&arr).copied().unwrap_or(arr);
        // stale where a loop around the read may write what it reads: its field, its constant slot,
        // or, for a slot not constant, any slot of the array
        let n = self.loop_writes.len() - (self.entry_read.get() && !self.loop_writes.is_empty()) as usize;
        let stale = self.loop_writes[..n].iter().any(|w| w.get(&root).is_some_and(|fw| fw.all || match (field, &idx.kind) {
            (Some(fi), _) => fw.fields.contains(&fi),
            (None, ExprKind::Int(k)) if *k >= 0 => fw.fields.contains(&(IDX + *k as usize)),
            (None, _) => !fw.fields.is_empty(),
        }));
        if stale { return None; }
        let index = self.exact_size(idx);
        let least = index.is_none() && bound == Dir::Lower;
        let key = (root, index.clone(), field, least);
        if let Some(p) = self.reads.borrow().get(&key) { return Some(p.clone()); }
        let name = self.f.locals[root].name.clone();
        let root_ref = match self.f.params.iter().position(|&p| p == root) { Some(i) => Root::Param(i, name), None => Root::Local(name) };
        let fname = field.and_then(|fi| match self.f.locals[root].ty.elem() {
            Some(Ty::Struct(sid)) => self.an.m.structs[*sid].fields.get(fi).map(|(n, _)| n.clone()),
            _ => None,
        });
        let id = NEXT_READ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = Poly::atom(Atom::Read(Box::new(Read { id, root: root_ref, index, field: fname, least, walk: false }.canon())));
        self.reads.borrow_mut().insert(key, p.clone());
        Some(p)
    }

    /// A write to `arr`: every size read from it is stale.
    fn wrote(&self, arr: LocalId) {
        let root = self.local_root.get(&arr).copied().unwrap_or(arr);
        self.reads.borrow_mut().retain(|k, _| k.0 != root);
    }

    /// The arrays a loop body writes, by root, for `loop_writes`.
    /// A call counts only where the callee may write what it is handed, by `field_writes`.
    fn written_roots(&self, body: &Block) -> HashMap<LocalId, FieldWrites> {
        let mut roots = self.local_root.clone();
        let mut out: HashMap<LocalId, FieldWrites> = HashMap::new();
        stores_block(body, self.f, &mut roots, &self.an.field_writes, &mut |r, fld| out.entry(r).or_default().add(fld));
        out
    }

    /// An index expression as an affine function of the loop variables in scope.
    fn affine(&self, e: &Expr) -> Option<Affine> {
        match &e.kind {
            ExprKind::Int(v) => Some(Affine::constant(Poly::constant(*v as i128))),
            ExprKind::Local(l) => {
                // scanners nest `while`s on one index: the innermost is the one moving it
                let mono = self.loops.iter().any(|lp| lp.var == Some(*l) && lp.monotone);
                let lp = if mono { self.loops.iter().rev().find(|lp| lp.var == Some(*l)) } else { self.loops.iter().find(|lp| lp.var == Some(*l)) };
                if let Some(lp) = lp {
                    return lp.offset.as_ref().map(|off| Affine::var(*l).add(off));
                }
                self.local_affine.get(l).cloned()
            }
            ExprKind::Len(l) => self.local_size.get(l).map(|p| Affine::constant(p.clone())),
            ExprKind::Call(fid, _) if self.is_argc(*fid) => self.argc_atom.map(|a| Affine::constant(Poly::var(a))),
            ExprKind::Cast(inner, Ty::I64) => self.affine(inner),
            // `min(ii*T + T, n)` as a loop end: the tile bound, the rectangular hull of the rest
            ExprKind::MinMax(true, a, _) => self.affine(a),
            ExprKind::InRow(j, _) => self.affine(j),
            ExprKind::Binary(BinOp::Div, a, b) => {
                let pb = self.affine(b)?;
                if !pb.is_const() { return None; }
                match pb.konst.as_const() {
                    Some(d) => { if d.is_zero() || !d.is_int() { return None; } Some(self.affine(a)?.scale(&Poly::from_rat(Rat::new(1, d.n)))) }
                    None => Some(self.affine(a)?.scale(&pb.konst.inv_mono()?)),
                }
            }
            ExprKind::Binary(BinOp::Add, a, b) => Some(self.affine(a)?.add(&self.affine(b)?)),
            ExprKind::Binary(BinOp::Sub, a, b) => {
                let nb = self.affine(b)?.scale(&Poly::constant(-1));
                Some(self.affine(a)?.add(&nb))
            }
            ExprKind::Binary(BinOp::Mul, a, b) => {
                let (pa, pb) = (self.affine(a)?, self.affine(b)?);
                if pa.is_const() { Some(pb.scale(&pa.konst)) }
                else if pb.is_const() { Some(pa.scale(&pb.konst)) }
                else { None }
            }
            ExprKind::Block(b) if b.stmts.is_empty() => self.affine(b.tail.as_ref()?),
            _ => None,
        }
    }

    /// Bytes of one element of the array `l` in memory — a struct's size under AoS.
    fn elem_bytes(&self, l: LocalId) -> i128 {
        self.f.locals[l].ty.elem().map_or(8, |t| self.an.m.size_of(t))
    }
    /// The fields of `l`'s element type when that type is a struct laid out one array per field.
    fn soa_fields(&self, l: LocalId) -> Option<&'a [(String, Ty)]> {
        match self.f.locals[l].ty.elem() {
            Some(Ty::Struct(i)) if self.an.m.structs[*i].layout == Layout::Soa => Some(&self.an.m.structs[*i].fields),
            _ => None,
        }
    }
    /// How far the address moves per unit of the index: the field's own bytes under SoA, the
    /// whole element under AoS. This one number is what a layout decides.
    fn stride_bytes(&self, l: LocalId, field: Option<usize>) -> i128 {
        match (self.soa_fields(l), field) {
            (Some(fs), Some(fi)) => fs[fi].1.elem_bytes(),
            _ => self.elem_bytes(l),
        }
    }
    /// Where a field's array starts in the modelled address space of `l`.
    fn soa_base(&self, l: LocalId, field: Option<usize>) -> Poly {
        let (Some(fs), Some(fi)) = (self.soa_fields(l), field) else { return Poly::zero() };
        let n = self.local_size.get(&l).cloned().unwrap_or_else(Poly::zero);
        let before: i128 = fs[..fi].iter().map(|(_, t)| t.elem_bytes()).sum();
        n.scale(Rat::int(before))
    }
    /// What a site on `l` touches: one field's bytes, or the whole element.
    fn touch_bytes(&self, l: LocalId, field: Option<usize>) -> i128 {
        match (self.f.locals[l].ty.elem(), field) {
            // an array field is touched whole: which element is read is not the site's (§9)
            (Some(Ty::Struct(s)), Some(fi)) => match &self.an.m.structs[*s].fields[fi].1 {
                Ty::Array(e, Size::Const(k)) => *k as i128 * e.elem_bytes(),
                t => t.elem_bytes(),
            },
            _ => self.elem_bytes(l),
        }
    }

    // ---- the moves rule ----

    /// Record an access site; its moves are settled with everyone else's at the end.
    /// Record an access site. Two accesses to the same array at the same index inside the same
    /// loops touch the same lines — `ps[i].x` and `ps[i].y` are one element, `a[i]` twice in one
    /// expression is one load — so they are **one site**, widened to cover both fields. Counting
    /// them apart would charge an array of structs once per field read and no loop reading a
    /// whole element could ever prefer that layout.
    fn access(&mut self, arr: LocalId, idx: &Expr, field: Option<usize>) {
        if self.replay { return; }
        // a whole element under SoA is a gather: one site per field array
        if field.is_none() {
            if let Some(n) = self.soa_fields(arr).map(|f| f.len()) {
                for fi in 0..n { self.access(arr, idx, Some(fi)); }
                return;
            }
        }
        let es = self.touch_bytes(arr, field);
        let stride = self.stride_bytes(arr, field);
        let base = self.soa_base(arr, field);
        let soa = self.soa_fields(arr).is_some();
        let key = idx_key(idx);
        let aff = self.affine(idx);
        let path: Vec<usize> = self.loops.iter().map(|l| l.id).collect();
        let root = self.local_root.get(&arr).copied().unwrap_or(arr);
        if let Some(prev) = self.sites.iter_mut().find(|s| {
            s.arr == arr && s.stride == stride && s.path == path && s.branch == self.branch
                // the same address: an affine index equal term by term, or, where the index is
                // not affine — a walk over an arena — the same expression
                && ((s.aff.is_some() && s.aff == aff) || (s.aff.is_none() && aff.is_none() && key.is_some() && s.key == key))
                // under SoA two fields are two arrays and never one site
                && (!soa || s.field == field)
        }) {
            let _ = root;
            // the same element: the span the two fields cover together, which is the element
            if prev.field != field { prev.es = stride; }
            prev.store |= self.storing;
            return;
        }
        let dep = !self.loops.is_empty() && matches!(&idx.kind, ExprKind::Local(l) if self.chase_vars.contains(l));
        // a page or more a lap of the innermost loop: a number at least a page, or a stride in a
        // size (a row of a grid), which for any size worth timing is a page or more
        let paged = match (self.loops.last(), &aff) {
            (Some(lp), Some(a)) if !lp.monotone => lp.var.and_then(|v| a.coeffs.get(&v)).is_some_and(|k| {
                let bytes = k.scale(Rat::int(stride * lp.step.abs()));
                match self.numeric(&bytes) { Some(x) => x.abs() >= 4096.0, None => !bytes.is_zero() && bytes.terms.keys().any(|m| !m.factors.is_empty()) }
            }),
            _ => false,
        };
        let raw = self.exact_size(idx);
        self.sites.push(Site { dep, paged, store: self.storing, arr, aff, es, stride, field, base, key, path, branch: self.branch.clone(), raw });
    }

    /// The access sites of loop `lid` that read one array at offsets the loop carries into each
    /// other: the same array, field and nest, the same coefficient on every loop variable, and
    /// constant parts that differ by whole laps of this loop — `src[(i − 1)·n + j]`, `src[i·n + j]`,
    /// `src[(i + 1)·n + j]` at `i` — and by less than one lap of the loop outside it, so a group is
    /// found at the loop that moves it and not again further in. Each group lists `(member, laps
    /// ahead of the one behind)`, sorted, with at least two members a lap or more apart.
    fn reuse_groups(&self, lid: usize, members: &[(usize, usize)], grouped: &[bool]) -> Vec<Vec<(usize, i128)>> {
        let rec = &self.loop_recs[lid];
        let Some(var) = rec.var else { return vec![] };
        if rec.monotone || rec.atom.is_none() { return vec![]; }
        let mut groups: Vec<Vec<(usize, i128)>> = Vec::new();
        let mut used = vec![false; members.len()];
        for a in 0..members.len() {
            if used[a] || grouped[members[a].0] { continue; }
            let sa = &self.sites[members[a].0];
            let Some(aa) = &sa.aff else { continue };
            let Some(k) = aa.coeffs.get(&var) else { continue };
            let unit = k.scale(Rat::int(rec.step));
            if unit.is_zero() { continue; }
            // one lap of the loop outside, in index units, when there is one
            let pos = members[a].1;
            let outer = if pos > 0 {
                let o = &self.loop_recs[sa.path[pos - 1]];
                match o.var.and_then(|ov| aa.coeffs.get(&ov)) { Some(ko) => Some(ko.scale(Rat::int(o.step.abs()))), None => None }
            } else { None };
            let mut g: Vec<(usize, Rat)> = vec![(a, Rat::zero())];
            for b in a + 1..members.len() {
                if used[b] || grouped[members[b].0] { continue; }
                let sb = &self.sites[members[b].0];
                let Some(ab) = &sb.aff else { continue };
                if sb.arr != sa.arr || sb.field != sa.field || sb.stride != sa.stride || sb.path != sa.path || sb.branch != sa.branch || ab.coeffs != aa.coeffs { continue; }
                let delta = ab.konst.sub(&aa.konst);
                // laps: `delta` as a rational multiple of one lap's move
                let Some((m0, k0)) = unit.terms.iter().next() else { continue };
                let r = match delta.terms.get(m0) { Some(dk) => dk.mul(Rat::new(k0.d, k0.n)), None if delta.is_zero() => Rat::zero(), None => continue };
                if unit.scale(r) != delta || !r.is_int() { continue; }
                // less than one lap of the loop outside, both ways, where both are numbers: an
                // offset in a size (`2·n`) is never a whole number of laps of a loop moving by one
                if let (true, Some(ko)) = (pos > 0, &outer) {
                    if let (Some(d), Some(o)) = (self.numeric(&delta), self.numeric(ko)) {
                        if d.abs() >= o.abs() { continue; }
                    }
                }
                g.push((b, r));
            }
            if g.len() < 2 { continue; }
            let lo = g.iter().map(|(_, r)| r.n).min().unwrap();
            let mut g: Vec<(usize, i128)> = g.into_iter().map(|(m, r)| (m, r.n - lo)).collect();
            g.sort_by_key(|&(_, c)| c);
            if g[g.len() - 1].1 < 1 { continue; }
            for &(m, _) in &g { used[m] = true; }
            groups.push(g);
        }
        groups
    }

    /// Lines touched by every access site over its loop nest, times B, added to moves. Level by
    /// level from the innermost loop out, for all sites at once:
    ///
    ///   lines(inner of innermost) = 1
    ///   lines(level) = lines(inner) × t                      if the working set at this level does not fit M
    ///                = lines(inner) × 1                      if the access does not move with this loop
    ///                = lines(inner) × t                      if it moves by a whole line or more
    ///                = lines(inner) × t·s/B                  if it moves by s < B bytes per iteration
    ///
    /// The working set at a level is the sum, over every site inside that loop, of the lines
    /// that site touches per iteration of it — the sites compete for the same cache. It fits
    /// when that is a known number ≤ M. A symbolic working set is assumed not to fit, and a
    /// non-affine index is assumed to move by a whole line or more. Every assumption rounds up.
    fn settle_moves(&mut self) {
        let m = self.machine();
        let nsites = self.sites.len();
        if std::env::var("NEANT_DEBUG_SITES").is_ok() {
            eprintln!("=== {} : {} sites", self.f.name, nsites);
            for (i, s) in self.sites.iter().enumerate() {
                eprintln!("  [{i}] arr={} stride={} es={} field={:?} path={:?} branch={:?} aff={} key={:?}",
                    self.f.locals[s.arr].name, s.stride, s.es, s.field, s.path, s.branch,
                    s.aff.is_some(), s.key);
            }
        }
        // for every (site, loop): the alternatives for the lines the site touches over one full
        // run of that loop — each under the conditions that produced it, with whether they form
        // one contiguous region
        #[derive(Clone)]
        struct LP { conds: Vec<Cond>, lines: Poly, contig: bool }
        let mut table: HashMap<(usize, usize), Vec<LP>> = HashMap::new();
        let inner = |table: &HashMap<(usize, usize), Vec<LP>>, s: usize, path: &[usize], pos: usize| -> Vec<LP> {
            if pos + 1 < path.len() { table[&(s, path[pos + 1])].clone() } else { vec![LP { conds: vec![], lines: Poly::constant(1), contig: true }] }
        };
        let mut ids: Vec<usize> = (0..self.loop_recs.len()).collect();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        // a site already charged less by a group further in is not a member of another
        let mut grouped = vec![false; nsites];
        // the loops whose sum decided an inner test at two ends on two sides of `M`
        let mut straddle_at: Vec<usize> = Vec::new();
        for lid in ids {
            let members: Vec<(usize, usize)> = (0..nsites)
                .filter_map(|s| self.sites[s].path.iter().position(|&l| l == lid).map(|pos| (s, pos)))
                .collect();
            if members.is_empty() { continue; }
            let rec = &self.loop_recs[lid];
            let inners: Vec<Vec<LP>> = members.iter().map(|&(s, pos)| inner(&table, s, &self.sites[s].path, pos)).collect();
            let mut out: Vec<Vec<LP>> = vec![Vec::new(); members.len()];
            // neighbouring sites (cost-model § Moves, neighbouring sites): per group, each member
            // and its offset in laps of this loop, the one furthest ahead last
            let groups = self.reuse_groups(lid, &members, &grouped);
            for g in &groups { for &(m, _) in g { grouped[members[m].0] = true; } }
            let group_of = |i: usize| groups.iter().find(|g| g.iter().any(|&(m, _)| m == i));
            // every feasible choice of one alternative per site is a working set to test
            let mut choice = vec![0usize; members.len()];
            loop {
                let picks: Vec<&LP> = (0..members.len()).map(|i| &inners[i][choice[i]]).collect();
                let mut conds: Vec<Cond> = Vec::new();
                for lp in &picks { for c in &lp.conds { if !conds.contains(c) { conds.push(c.clone()); } } }
                // a test an inner loop made in this loop's variable (a triangle's inner range, `B·i
                // < M`) is decided at this loop's two ends now that its laps are summed away
                // (`split_conds`); where the ends are on two sides it changes inside the loop, and
                // each side is charged every lap, a bound
                let variants = match rec.atom {
                    Some(a) if conds.iter().any(|c| c.ws.mentions(a)) => super::piece::split_conds(&conds, a, &rec.lo, &rec.last(), rec.step),
                    _ if super::piece::feasible(&conds) => vec![(conds.clone(), false)],
                    _ => vec![],
                };
                for (conds, s) in variants {
                    if s && !straddle_at.contains(&lid) { straddle_at.push(lid); }
                    let ws = picks.iter().fold(Poly::zero(), |acc, lp| acc.add(&lp.lines));
                    // a working set that varies with this loop's variable is tested at its largest
                    let test: Option<Poly> = match rec.atom {
                        Some(a) if ws.mentions(a) => {
                            let signs: Vec<i128> = ws.terms.iter().filter(|(mo, _)| mo.has_atom(&Atom::Var(a))).map(|(_, c)| c.n.signum()).collect();
                            // at the variable's largest value where the set grows with it, its least where
                            // it shrinks: the last lap's and the first's when it counts up, the other
                            // way round when it counts down (`while i >= 0 { … i -= 1 }`)
                            let (least, most) = if rec.step > 0 { (rec.lo.clone(), rec.last()) } else { (rec.last(), rec.lo.clone()) };
                            if signs.iter().all(|&x| x >= 0) { Some(ws.subst(a, &most)) }
                            else if signs.iter().all(|&x| x <= 0) { Some(ws.subst(a, &least)) }
                            else { None }
                        }
                        _ => Some(ws.clone()),
                    };
                    // decided, or forked into the two outcomes
                    let outcomes: Vec<(Vec<Cond>, bool)> = match &test {
                        None => vec![(conds.clone(), false)],
                        Some(t) => match self.numeric(t) {
                            Some(l) => vec![(conds.clone(), (l * m.b_bytes as f64) < m.m_bytes as f64)],
                            None => {
                                let mut cf = conds.clone(); cf.push(Cond { ws: t.clone(), fits: true });
                                let mut cn = conds.clone(); cn.push(Cond { ws: t.clone(), fits: false });
                                let mut v = Vec::new();
                                if super::piece::feasible(&cf) { v.push((cf, true)); }
                                if super::piece::feasible(&cn) { v.push((cn, false)); }
                                v
                            }
                        },
                    };
                    for (conds, fits) in outcomes {
                        // an arena's lines are the arena's, however many sites walk it
                        let mut arenas_taken: Vec<LocalId> = Vec::new();
                        for (i, &(s, _)) in members.iter().enumerate() {
                            let site = &self.sites[s];
                            let lp = picks[i];
                            let summed = rec.sum(&lp.lines);
                            // **The region rule.** A site whose index is not an affine function of
                            // the loops — a walk over an arena, `nodes[i]` with `i` loaded from
                            // memory — costs a fresh line per iteration, unless the array it walks
                            // is small enough to stay in cache: once every line of it has been
                            // touched, nothing more is fetched, whatever the order. So the site
                            // costs at most the array, under the condition that the array fits.
                            if site.aff.is_none() && fits && !super::ablate("region") {
                                let root = self.local_root.get(&site.arr).copied().unwrap_or(site.arr);
                                let bytes = self.local_size.get(&root).cloned().unwrap_or_else(Poly::zero).scale(Rat::int(self.elem_bytes(root)));
                                if !bytes.is_zero() {
                                    let arena = bytes.mul_atom_pow(Atom::B, Rat::int(-1));
                                    // the walk is worth bounding only when it is longer than the
                                    // arena, which is a comparison between a trip count and a
                                    // number of lines: it needs this machine's `B`
                                    let mm = self.machine();
                                    let longer = super::piece::dominates_eventually(&summed.at_machine(mm.b_bytes, mm.m_bytes), &arena.at_machine(mm.b_bytes, mm.m_bytes));
                                    if longer {
                                        let first = !arenas_taken.contains(&root);
                                        arenas_taken.push(root);
                                        let arena = if first { arena } else { Poly::zero() };
                                        let mut cf = conds.clone();
                                        let cond = Cond { ws: arena.clone(), fits: true };
                                        if !cf.contains(&cond) { cf.push(cond); }
                                        if super::piece::feasible(&cf) {
                                            let np = LP { conds: cf, lines: arena, contig: false };
                                            if !out[i].iter().any(|x| x.conds == np.conds && x.lines == np.lines) { out[i].push(np); }
                                        }
                                        // and the piece where it does not fit keeps the walk's cost
                                        let mut cn = conds.clone();
                                        let cond = Cond { ws: bytes.mul_atom_pow(Atom::B, Rat::int(-1)), fits: false };
                                        if !cn.contains(&cond) { cn.push(cond); }
                                        if super::piece::feasible(&cn) {
                                            let np = LP { conds: cn, lines: summed.clone(), contig: false };
                                            if !out[i].iter().any(|x| x.conds == np.conds && x.lines == np.lines) { out[i].push(np); }
                                        }
                                        continue;
                                    }
                                }
                            }
                            // a site that does not move with this loop re-reads the same lines every lap — or,
                            // when its inner range varies with this loop (`j in i + 1..n`, a triangle),
                            // subsets of the lines of its hull over every lap, each fetched once while they fit
                            let same_set = || if rec.atom.is_some_and(|a| lp.lines.mentions(a)) {
                                let a = rec.atom.unwrap();
                                // the index in the loops' own values, where there is one: `site_range`
                                // adds each loop's span apart, which counts twice a variable that is
                                // also an inner loop's trip (`k in i + 1..j` inside `j`)
                                let range = site.raw.as_ref().map(|r| {
                                    let st = Rat::int(site.stride);
                                    (r.scale(st).add(&site.base), r.scale(st).add(&Poly::constant(site.es)).add(&site.base))
                                }).or_else(|| self.site_range(site));
                                let hull = if fits { range } else { None }.and_then(|(lo, hi)| {
                                    use super::piece::dominates;
                                    // a loop inside this one whose trip is a loop variable (`k in 0..j`)
                                    // leaves that variable in the range: each is taken at its own
                                    // extreme first, innermost out, so what is left is in this loop's
                                    // variable and the loops outside it
                                    // an end at its least or its most over a loop's laps: where it is
                                    // linear in the loop's variable the coefficient's sign says which
                                    // lap (`n·j` is largest at the last `j`, whatever the loops outside
                                    // it hold), and otherwise the two laps' values must be ordered
                                    let extreme = |p: &Poly, b: usize, r: &LoopRec, most: bool| -> Option<Poly> {
                                        if !p.mentions(b) { return Some(p.clone()); }
                                        let (first, last) = (p.subst(b, &r.lo), p.subst(b, &r.last()));
                                        match super::piece::direction(p, b) {
                                            // the atom's larger value is the last lap's when it counts up
                                            Some(up) => Some(if up == most && r.step > 0 || up != most && r.step < 0 { last } else { first }),
                                            None if most => if dominates(&last, &first) { Some(last) } else if dominates(&first, &last) { Some(first) } else { None },
                                            None => if dominates(&last, &first) { Some(first) } else if dominates(&first, &last) { Some(last) } else { None },
                                        }
                                    };
                                    let pos = site.path.iter().position(|&l| l == lid)?;
                                    let (mut lo, mut hi) = (lo, hi);
                                    for &l in site.path[pos + 1..].iter().rev() {
                                        let r = &self.loop_recs[l];
                                        let Some(b) = r.atom else { continue };
                                        lo = extreme(&lo, b, r, false)?;
                                        hi = extreme(&hi, b, r, true)?;
                                    }
                                    let lo = extreme(&lo, a, rec, false)?;
                                    let hi = extreme(&hi, a, rec, true)?;
                                    // a hull whose ends are out of order, its leading term negative, is
                                    // no hull (a trip that is empty at an end)
                                    Some(hi.sub(&lo))
                                });
                                match hull {
                                    Some(bytes) => (bytes.mul_atom_pow(Atom::B, Rat::int(-1)).add(&Poly::constant(1)), true),
                                    // ends that cannot be ordered (a trip that falls as an offset rises,
                                    // `j in i + 1..n` inside `i`): the laps' lines are at most the
                                    // array's, all of them, while it fits — the region rule's bound
                                    None if fits => {
                                        let root = self.local_root.get(&site.arr).copied().unwrap_or(site.arr);
                                        let bytes = self.local_size.get(&root).cloned().unwrap_or_else(Poly::zero).scale(Rat::int(self.elem_bytes(root)));
                                        let whole = bytes.mul_atom_pow(Atom::B, Rat::int(-1));
                                        let mm = self.machine();
                                        if !bytes.is_zero() && super::piece::dominates_eventually(&summed.at_machine(mm.b_bytes, mm.m_bytes), &whole.at_machine(mm.b_bytes, mm.m_bytes)) { (whole, true) } else { (summed.clone(), true) }
                                    }
                                    None => (summed.clone(), true),
                                }
                            } else { (lp.lines.clone(), false) };
                            let (total, contig) = if !fits {
                                (summed.clone(), false)
                            } else if rec.atom.is_some_and(|a| lp.lines.mentions(a)) && !matches!(site.aff, None) {
                                // a triangle's laps: the hull of every lap is the lines they share,
                                // whatever the step between laps — a step of a size (`(n + 1)·8` a
                                // lap of `i`, nussinov's column) as much as one under a line
                                (same_set().0, false)
                            } else {
                                let stride = site.aff.as_ref().map(|a| rec.var.and_then(|v| a.coeffs.get(&v).cloned()).unwrap_or_else(Poly::zero).scale(Rat::int(site.stride)));
                                match stride {
                                    None => (summed.clone(), false),
                                    Some(st) if st.is_zero() => (same_set().0, lp.contig),
                                    Some(st) => match self.numeric(&st) {
                                        Some(sb) if sb.abs() >= m.b_bytes as f64 => (summed.clone(), false),
                                        Some(sb) => {
                                            let slide = rec.sum(&Poly::constant(1)).scale(Rat::new(sb.abs() as i128, 1)).mul_atom_pow(Atom::B, Rat::int(-1));
                                            let slide = match self.numeric(&slide) { Some(v) if v < 1.0 => Poly::constant(1), _ => slide };
                                            // a hull, or the sum it falls back to, is the lines of every lap
                                            // already, the slide's included: it is not slid again
                                            match same_set() {
                                                (h, true) => (h, false),
                                                (s, false) => if lp.contig { (s.add(&slide), true) } else { (s.mul(&slide), false) },
                                            }
                                        }
                                        None => (summed.clone(), false),
                                    },
                                }
                            };
                            // reuse only takes lines away: a charge for the fitting case that is not
                            // below the one with no reuse at all (the hull of a triangle's laps times
                            // a slide that already walks them, `n²·n²`) is wrong, and the plain sum
                            // stands for it
                            // — and so is a hull that is only more for small sizes, compared at this
                            // machine's `B` the way the region rule compares (a triangle's bounding
                            // rows, `8·n²`, against the rows it reads, `4·n²`)
                            let mm = self.machine();
                            let more = |p: &Poly, q: &Poly| super::piece::dominates(p, q) || super::piece::dominates_eventually(&p.at_machine(mm.b_bytes, mm.m_bytes), &q.at_machine(mm.b_bytes, mm.m_bytes));
                            let (total, contig) = if fits && total != summed && more(&total, &summed) { (summed.clone(), false) } else { (total, contig) };
                            let np = LP { conds: conds.clone(), lines: total, contig };
                            // a member of a group whose window fits: the first is charged in full,
                            // the one furthest ahead the lines of the laps between them, the rest
                            // nothing — they read what those two already brought in
                            let grouped = match (fits, group_of(i), &test) {
                                (true, Some(g), Some(t)) => {
                                    let span = g.iter().map(|&(_, c)| c).max().unwrap_or(0);
                                    let win = Cond { ws: t.scale(Rat::int(span + 1)), fits: true };
                                    let lines = if g[0].0 == i { np.lines.clone() }
                                        else if g[g.len() - 1].0 == i { picks[i].lines.scale(Rat::int(span)) }
                                        else { Poly::zero() };
                                    Some((win, lines))
                                }
                                _ => None,
                            };
                            let alts: Vec<LP> = match grouped {
                                None => vec![np],
                                Some((win, lines)) => {
                                    let mut cf = conds.clone(); if !cf.contains(&win) { cf.push(win.clone()); }
                                    let mut cn = conds.clone(); let nw = Cond { ws: win.ws.clone(), fits: false }; if !cn.contains(&nw) { cn.push(nw); }
                                    let mut v = Vec::new();
                                    if super::piece::feasible(&cf) { v.push(LP { conds: cf, lines, contig: false }); }
                                    if super::piece::feasible(&cn) { v.push(LP { conds: cn, lines: np.lines.clone(), contig: np.contig }); }
                                    v
                                }
                            };
                            for np in alts {
                                if !out[i].iter().any(|x| x.conds == np.conds && x.lines == np.lines && x.contig == np.contig) { out[i].push(np); }
                            }
                        }
                    }
                }
                // next combination
                let mut k = 0;
                loop {
                    if k == members.len() { break; }
                    choice[k] += 1;
                    if choice[k] < inners[k].len() { break; }
                    choice[k] = 0;
                    k += 1;
                }
                if k == members.len() { break; }
            }
            for (i, &(s, _)) in members.iter().enumerate() {
                table.insert((s, lid), std::mem::take(&mut out[i]));
            }
        }
        for lid in straddle_at {
            let rec = self.loop_recs[lid].clone();
            rec.straddled.set(true);
            self.note_straddle(&rec);
        }
        // **Translations** (cost-model § Time, translations): an access whose address moves a page
        // or more a lap of its innermost loop needs a page of its own every lap, and when that loop's
        // run touches more pages than the TLB maps — the accesses that move so, times its laps, its
        // trip taken at its largest — every such lap waits for a page walk. Counted per access,
        // not per line fetched: a column that stays in cache still translates each lap.
        // each such access, with the condition that its step is a page or more when the step is a
        // size (`8·m ≥ 4096`, written as a fit test the way the pages are below)
        let mut paged_at: Vec<(usize, Vec<Option<Cond>>)> = Vec::new();
        for site in &self.sites {
            let (Some(&lid), Some(aff)) = (site.path.last(), site.aff.as_ref()) else { continue };
            let rec = &self.loop_recs[lid];
            let Some(v) = rec.var else { continue };
            let Some(c) = aff.coeffs.get(&v) else { continue };
            let step = c.scale(Rat::int(site.stride * rec.step.abs()));
            let cond = match self.numeric(&step) {
                Some(bytes) if bytes.abs() < m.page_bytes as f64 => continue,
                Some(_) => None,
                None if step.terms.values().all(|k| k.n >= 0) => Some(Cond { ws: step.scale(Rat::new(m.m_bytes, m.page_bytes)).mul_atom_pow(Atom::B, Rat::int(-1)), fits: false }),
                None => continue,
            };
            match paged_at.iter_mut().find(|(l, _)| *l == lid) { Some((_, cs)) => cs.push(cond), None => paged_at.push((lid, vec![cond])) }
        }
        for (lid, conds) in paged_at {
            let n = conds.len() as i128;
            let path: Vec<usize> = match self.sites.iter().find(|s| s.path.last() == Some(&lid)) { Some(s) => s.path.clone(), None => continue };
            // the pages one run of the loop touches, at its largest over the loops outside it
            let mut pages = self.loop_recs[lid].trip.scale(Rat::int(n));
            for &l in path.iter().rev() {
                let r = &self.loop_recs[l];
                let Some(a) = r.atom else { continue };
                if !pages.mentions(a) { continue; }
                // the variable's largest value where the pages grow with it, its least where they
                // shrink, whichever way the loop counts
                let (least, most) = if r.step > 0 { (r.lo.clone(), r.last()) } else { (r.last(), r.lo.clone()) };
                pages = match super::piece::direction(&pages, a) {
                    Some(false) => pages.subst(a, &least),
                    _ => pages.subst(a, &most),
                };
            }
            // every lap of it, over every lap of the loops around it, one access at a time
            let mut laps = Poly::constant(1);
            for &l in path.iter().rev() { laps = self.loop_recs[l].sum(&laps); }
            // `pages · page ≥ T`, written as a fit test on `M` at this machine's ratio of the two
            let ws = pages.scale(Rat::new(m.page_bytes * m.m_bytes, m.tlb_bytes)).mul_atom_pow(Atom::B, Rat::int(-1));
            for c in conds {
                let mut under = vec![Cond { ws: ws.clone(), fits: false }];
                under.extend(c);
                self.tlb = self.tlb.add_under(&under, &laps);
            }
        }
        // the total: sites add up, except that sites on the two sides of an `if` are alternatives
        let top = |s: usize| -> Cost {
            let site = &self.sites[s];
            let lps = match site.path.first() { Some(&l0) => table[&(s, l0)].clone(), None => vec![LP { conds: vec![], lines: Poly::constant(1), contig: true }] };
            let mut c = Cost { pieces: lps.into_iter().map(|lp| super::piece::Piece { conds: lp.conds, poly: lp.lines }).collect() };
            c.prune();
            c
        };
        fn group(sites: &[usize], depth: usize, all: &[Site], top: &dyn Fn(usize) -> Cost) -> Cost {
            let mut total = Cost::zero();
            for &s in sites { if all[s].branch.len() == depth { total = total.add(&top(s)); } }
            let mut ifs: Vec<usize> = Vec::new();
            for &s in sites { if all[s].branch.len() > depth { let id = all[s].branch[depth].0; if !ifs.contains(&id) { ifs.push(id); } } }
            for id in ifs {
                let t: Vec<usize> = sites.iter().copied().filter(|&s| all[s].branch.len() > depth && all[s].branch[depth] == (id, true)).collect();
                let e: Vec<usize> = sites.iter().copied().filter(|&s| all[s].branch.len() > depth && all[s].branch[depth] == (id, false)).collect();
                total = total.add(&group(&t, depth + 1, all, top).max(&group(&e, depth + 1, all, top)));
            }
            total
        }
        let all: Vec<usize> = (0..nsites).collect();
        let total = group(&all, 0, &self.sites, &top);
        self.moves = self.moves.add(&total.mul_poly(&Poly::atom(Atom::B)));
        // the part of those lines a chase fetches, one waiting on the last (cost-model § Time)
        let dep: Vec<usize> = all.iter().copied().filter(|&s| self.sites[s].dep).collect();
        if !dep.is_empty() { self.chase = self.chase.add(&group(&dep, 0, &self.sites, &top).mul_poly(&Poly::atom(Atom::B))); }
        let pg: Vec<usize> = all.iter().copied().filter(|&s| self.sites[s].paged && !self.sites[s].dep).collect();
        if !pg.is_empty() { self.paged = self.paged.add(&group(&pg, 0, &self.sites, &top).mul_poly(&Poly::atom(Atom::B))); }
        // the sites that stream in their innermost loop — an address that moves with it, by less
        // than a line a lap — and the loops with two streams or more: one per array, a field
        // under SoA its own (cost-model § Time, streams)
        let (conc_add, wb_add) = {
        let streams = |s: usize| -> Option<(usize, (LocalId, String))> {
            let site = &self.sites[s];
            let &lid = site.path.last()?;
            let rec = &self.loop_recs[lid];
            let c = site.aff.as_ref()?.coeffs.get(&rec.var?)?.clone();
            let b = self.numeric(&c.scale(Rat::int(site.stride)))?;
            if b == 0.0 || b.abs() >= m.b_bytes as f64 { return None; }
            let root = self.local_root.get(&site.arr).copied().unwrap_or(site.arr);
            Some((lid, (root, format!("{:?}", site.base))))
        };
        let mut per_loop: HashMap<usize, Vec<(LocalId, String)>> = HashMap::new();
        for s in 0..nsites { if let Some((lid, k)) = streams(s) { let v = per_loop.entry(lid).or_default(); if !v.contains(&k) { v.push(k); } } }
        let concurrent = |s: usize| !self.sites[s].dep && streams(s).is_some_and(|(lid, _)| per_loop[&lid].len() >= 2);
        let cc: Vec<usize> = all.iter().copied().filter(|&s| concurrent(s)).collect();
        let conc_add = if cc.is_empty() { Cost::zero() } else { group(&cc, 0, &self.sites, &top).mul_poly(&Poly::atom(Atom::B)) };
        // a stored site's lines go back: at one stream's rate alone, at half its bytes with others
        let st1: Vec<usize> = all.iter().copied().filter(|&s| self.sites[s].store && !concurrent(s)).collect();
        let st2: Vec<usize> = all.iter().copied().filter(|&s| self.sites[s].store && concurrent(s)).collect();
        let mut wb_add = Cost::zero();
        if !st1.is_empty() { wb_add = wb_add.add(&group(&st1, 0, &self.sites, &top).mul_poly(&Poly::atom(Atom::B))); }
        if !st2.is_empty() { wb_add = wb_add.add(&group(&st2, 0, &self.sites, &top).mul_poly(&Poly::atom(Atom::B)).scale(Rat::new(1, 2))); }
        (conc_add, wb_add)
        };
        self.conc = self.conc.add(&conc_add);
        self.wback = self.wback.add(&wb_add);
    }

    /// The native lower bound for the statement's nest (`bounds.rs`): every array reference in
    /// the statement as an injective affine map on the loop variables, the HBL exponent over
    /// them, and the exact iteration count. References that are not injective (`a[i + j]`) or
    /// not affine leave the statement without an HBL bound; each injective reference still
    /// records the size of its image for the footprint bound.
    fn recognise(&mut self, s: &Stmt) {
        if self.replay { return; }
        // the references, reads and writes alike
        let mut refs: Vec<(LocalId, &Expr, Option<usize>)> = Vec::new();
        match s {
            Stmt::Assign(lv, _, e) => {
                match lv {
                    LValue::Index(a, i, _) => refs.push((*a, i, None)),
                    LValue::IndexField(a, i, fi, _) | LValue::IndexFieldIndex(a, i, fi, _, _) => refs.push((*a, i, Some(*fi))),
                    _ => {}
                }
                collect_refs(e, &mut refs);
            }
            Stmt::Let(_, e) | Stmt::Expr(e) => collect_refs(e, &mut refs),
            _ => {}
        }
        // scalars that stand for an array element
        let mut scalars: Vec<LocalId> = Vec::new();
        match s {
            Stmt::Assign(lv, _, e) => { if let LValue::Var(v) = lv { scalars.push(*v); } collect_scalars(e, &mut scalars); }
            Stmt::Let(_, e) | Stmt::Expr(e) => collect_scalars(e, &mut scalars),
            _ => {}
        }
        let aliased: Vec<(LocalId, Expr, Option<usize>)> = scalars.iter().filter_map(|v| self.scalar_alias.get(v).cloned()).collect();
        for (arr, idx, fi) in &aliased { refs.push((*arr, idx, *fi)); }
        if refs.is_empty() { return; }
        let line = match s { Stmt::Assign(_, _, e) | Stmt::Let(_, e) | Stmt::Expr(e) => e.line, _ => 0 };
        // loop variables in scope with atoms, outermost first
        let nest: Vec<(LocalId, usize, usize)> = self.loops.iter().enumerate().filter(|(_, l)| !l.monotone).filter_map(|(k, l)| Some((l.var?, l.atom?, k))).collect();
        let mut all_injective = true;
        let mut dim_sets: Vec<u32> = Vec::new();
        for (arr, idx, fi) in &refs {
            let Some(aff) = self.affine(idx) else { all_injective = false; continue };
            let Some(dims) = self.injective_dims(&aff, &nest) else { all_injective = false; continue };
            let mut mask = 0u32;
            for d in &dims { if let Some(pos) = nest.iter().position(|(v, _, _)| v == d) { mask |= 1 << pos; } }
            dim_sets.push(mask);
            // the image of this reference, for the footprint bound: the count over its own loops
            // when their bounds mention no other loop
            if let Some(img) = self.image_size(&dims, &nest) {
                let root = self.local_root.get(arr).copied().unwrap_or(*arr);
                let bytes = img.scale(Rat::int(self.touch_bytes(*arr, *fi)));
                let e = self.images.entry((root, *fi)).or_insert_with(Poly::zero);
                if bounds_dominates(&bytes, e) { *e = bytes; }
            }
        }
        if all_injective && !dim_sets.is_empty() {
            // loops no reference mentions are repetition, left out of |I| when nothing inside
            // depends on them; a loop an inner bound depends on stays in and the LP says so
            let used: u32 = dim_sets.iter().fold(0, |a, b| a | b);
            let mut count = Poly::constant(1);
            let mut in_lp = used;
            for (pos, (_, atom, k)) in nest.iter().enumerate().rev() {
                let l = &self.loops[*k];
                if used & (1 << pos) != 0 || count.mentions(*atom) {
                    in_lp |= 1 << pos;
                    count = count.sum_over(*atom, &l.lo, l.step, &l.trip);
                }
            }
            // renumber the dims that are in the LP
            let positions: Vec<usize> = (0..nest.len()).filter(|p| in_lp & (1 << p) != 0).collect();
            let compact = |mask: u32| positions.iter().enumerate().fold(0u32, |acc, (new, &old)| if mask & (1 << old) != 0 { acc | (1 << new) } else { acc });
            let sets: Vec<u32> = dim_sets.iter().map(|&m| compact(m)).collect();
            if let Some(sigma) = bounds::hbl_sigma(positions.len(), &sets) {
                if sigma > Rat::one() {
                    self.bounds.push(Bound {
                        kind: format!("HBL, σ = {}", if sigma.d == 1 { sigma.n.to_string() } else { format!("{}/{}", sigma.n, sigma.d) }), citation: "CDKSY 2013".into(),
                        moves: bounds::hbl_bound(&count, sigma), line, cold: false,
                    });
                }
            }
        }
        // what each operand of a multiply-accumulate does in the innermost loop
        let Some(mac) = bounds::as_mac(s) else { return };
        let (Some(aa), Some(ab)) = (self.affine(mac.ia), self.affine(mac.ib)) else { return };
        let es = self.elem_bytes(mac.a);
        if let Some(inner) = self.loops.last().filter(|l| !l.monotone).and_then(|l| l.var) {
            let m = self.machine();
            for (arr, aff) in [(mac.a, &aa), (mac.b, &ab)] {
                let stride = aff.coeffs.get(&inner).cloned().unwrap_or_else(Poly::zero).scale(Rat::int(es));
                let big = match self.numeric(&stride) { Some(v) => v.abs() >= m.b_bytes as f64, None => !stride.is_zero() };
                if big {
                    let nm = self.f.locals[arr].name.clone();
                    let sd = stride.display(&self.names).to_string();
                    self.notes.push(format!("`{nm}` moves by {sd} bytes per iteration of the innermost loop: a new line every time (line {})", mac.line));
                }
            }
        }
    }

    /// The loop variables an affine index depends on, when the index is injective on their
    /// ranges: sorted by the span each variable contributes, every coefficient must reach past
    /// the whole span of the smaller ones (mixed radix), decided for all sizes ≥ 1.
    fn injective_dims(&self, aff: &Affine, nest: &[(LocalId, usize, usize)]) -> Option<Vec<LocalId>> {
        // (var, |coefficient·step|, span = |coefficient·step|·(trip − 1))
        let mut parts: Vec<(LocalId, Poly, Poly)> = Vec::new();
        for (v, c) in &aff.coeffs {
            let &(_, _, k) = nest.iter().find(|(var, _, _)| var == v)?;
            let l = &self.loops[k];
            let unit = c.scale(Rat::int(l.step.abs()));
            let unit = match self.numeric(&unit) {
                Some(x) if x < 0.0 => unit.scale(Rat::int(-1)),
                Some(_) => unit,
                None => { if unit.terms.values().all(|k| k.n >= 0) { unit } else { return None; } }
            };
            let span = unit.mul(&l.trip.sub(&Poly::constant(1)));
            parts.push((*v, unit, span));
        }
        if parts.is_empty() { return None; }
        // ascending by unit: a total order is needed, dominance gives a partial one
        parts.sort_by(|a, b| if bounds_dominates(&b.1, &a.1) { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater });
        let mut covered = Poly::zero();
        for (_, unit, span) in &parts {
            // unit ≥ covered + 1
            if !bounds_dominates(unit, &covered.add(&Poly::constant(1))) { return None; }
            covered = covered.add(span);
        }
        Some(parts.into_iter().map(|p| p.0).collect())
    }

    /// `|π_D(I)|`: the number of distinct values the loop variables in `D` take together, exact
    /// when their bounds mention only loops in `D`.
    fn image_size(&self, dims: &[LocalId], nest: &[(LocalId, usize, usize)]) -> Option<Poly> {
        let mut count = Poly::constant(1);
        for (v, atom, k) in nest.iter().rev() {
            let l = &self.loops[*k];
            if dims.contains(v) {
                count = count.sum_over(*atom, &l.lo, l.step, &l.trip);
            } else if count.mentions(*atom) || l.trip.mentions(*atom) {
                return None;
            }
        }
        for (v, atom, _) in nest { if !dims.contains(v) && count.mentions(*atom) { return None; } }
        Some(count)
    }

    /// A sequential pass over `n` elements of `es` bytes, once per enclosing iteration.
    /// A copy of a struct value costs its array fields as a write (docs/arrays-by-value-design.md
    /// §3); scalars are registers and a struct without an array field costs nothing, as before.
    fn copy_value(&mut self, t: &Ty) {
        let Ty::Struct(sid) = t else { return };
        let (k, bytes) = self.an.m.structs[*sid].array_part();
        if k == 0 { return; }
        self.add_work_n(k);
        self.stream(&Poly::constant(bytes), 1);
    }

    /// `a == b` on a whole value. A fixed-size array: two loads and a compare per element, the
    /// conjunction folded into the compare, and both arrays' element bytes read. A struct: a
    /// compare per scalar field, which is in a register, and per array-field element two loads
    /// and a compare with no bytes, an element being resident as in §3.
    fn compare_value(&mut self, t: &Ty) {
        match t {
            Ty::Array(e, Size::Const(k)) => {
                let k = *k as i128;
                self.add_work_n(3 * k);
                self.stream(&Poly::constant(2 * k), e.elem_bytes());
            }
            Ty::Struct(sid) => {
                let sd = &self.an.m.structs[*sid];
                let (k, _) = sd.array_part();
                let scalars = sd.fields.iter().filter(|(_, t)| t.is_scalar()).count() as i128;
                self.add_work_n(scalars + 3 * k);
            }
            _ => self.add_work_n(1),
        }
    }

    fn stream(&mut self, n: &Poly, es: i128) {
        if !self.replay { self.moves = self.moves.add_poly(&n.scale(Rat::int(es))); }
    }

    // ---- the walk ----

    fn block(&mut self, b: &Block) -> Result<(), Fail> {
        self.open_aliases(b);
        let r = (|| { for s in &b.stmts { self.stmt(s)?; } if let Some(t) = &b.tail { self.expr(t)?; } Ok(()) })();
        self.close_aliases();
        r
    }

    /// Scan a block for scalars stored to or loaded from an array element, before walking it.
    fn open_aliases(&mut self, b: &Block) {
        let mut opened: Vec<LocalId> = Vec::new();
        let mut conflicted: Vec<LocalId> = Vec::new();
        let scalar_of = |e: &Expr| -> Option<LocalId> { match &e.kind { ExprKind::Local(v) => Some(*v), ExprKind::Cast(x, _) => if let ExprKind::Local(v) = &x.kind { Some(*v) } else { None }, _ => None } };
        let mut found: Vec<(LocalId, LocalId, Expr, Option<usize>)> = Vec::new();
        for st in &b.stmts {
            match st {
                Stmt::Assign(LValue::Index(arr, idx, _), _, e) => { if let Some(v) = scalar_of(e) { found.push((v, *arr, idx.clone(), None)); } }
                Stmt::Assign(LValue::IndexField(arr, idx, fi, _), _, e) => { if let Some(v) = scalar_of(e) { found.push((v, *arr, idx.clone(), Some(*fi))); } }
                Stmt::Let(v, e) | Stmt::Assign(LValue::Var(v), None, e) => match &e.kind {
                    ExprKind::Index(arr, idx) => found.push((*v, *arr, (**idx).clone(), None)),
                    ExprKind::Field(base, fi) => { if let ExprKind::Index(arr, idx) = &base.kind { found.push((*v, *arr, (**idx).clone(), Some(*fi))); } }
                    _ => {}
                },
                _ => {}
            }
        }
        for (v, arr, idx, fi) in found {
            if !self.f.locals[v].ty.is_scalar() || conflicted.contains(&v) { continue; }
            match self.scalar_alias.get(&v) {
                Some((a2, i2, f2)) if *a2 == arr && *f2 == fi && self.affine(i2).is_some() && self.affine(i2) == self.affine(&idx) => {}
                Some(_) => { self.scalar_alias.remove(&v); conflicted.push(v); opened.retain(|x| *x != v); }
                None => { self.scalar_alias.insert(v, (arr, idx, fi)); opened.push(v); }
            }
        }
        self.alias_scopes.push(opened);
    }
    fn close_aliases(&mut self) {
        if let Some(opened) = self.alias_scopes.pop() { for v in opened { self.scalar_alias.remove(&v); } }
    }

    fn stmt(&mut self, s: &Stmt) -> Result<(), Fail> {
        // the line `neant emit --lines` gives the loop, for its laps
        match s {
            Stmt::For { start, end, body, .. } => { self.pending_line = start.line.max(end.line); self.pending_bounded = exits_early(body); }
            Stmt::ParFor { end, body, .. } => { self.pending_line = end.line; self.pending_bounded = exits_early(body); }
            Stmt::While { line, cond, body, .. } => {
                self.pending_line = *line;
                self.pending_bounded = exits_early(body) || matches!(cond.kind, ExprKind::Binary(BinOp::And | BinOp::Or, ..));
            }
            Stmt::LetBuild { len, .. } => { self.pending_line = len.line; self.pending_bounded = false; }
            _ => {}
        }
        match s {
            Stmt::Let(id, e) => {
                self.recognise(s);
                self.expr(e)?;
                // `let t = s`: a second place for a value that has one
                if matches!(e.kind, ExprKind::Local(_)) { self.copy_value(&e.ty); }
                let l = &self.f.locals[*id];
                if l.ty.is_arrayish() {
                    if matches!(e.kind, ExprKind::Call(..)) {
                        // an owned array handed back by a call: the caller owns it, and its size
                        // is the callee's, in the caller's atoms
                        if let Some(sz) = self.last_result.take() { self.local_size.insert(*id, sz); }
                        if let Some(a) = self.last_result_atom.take() { self.names[a] = format!("{}.len()", l.name); }
                        self.local_root.insert(*id, *id);
                        return Ok(());
                    }
                    let src = match &e.kind { ExprKind::Ref(s, _) | ExprKind::Local(s) => Some(*s), _ => None };
                    if let Some(sz) = src.and_then(|s| self.local_size.get(&s).cloned()) {
                        self.local_size.insert(*id, sz);
                    }
                    if let Some(s0) = src { let r = self.local_root.get(&s0).copied().unwrap_or(s0); self.local_root.insert(*id, r); }
                } else if l.ty == Ty::I64 && !l.mutable {
                    match self.affine(e) {
                        Some(a) => { self.local_affine.insert(*id, a); }
                        // a value read from memory: a size, though not an affine one
                        None => if let Some(p) = self.exact_size(e) { self.local_affine.insert(*id, Affine::constant(p)); }
                        // a value the calculus cannot name — a call's result, a parse of input —
                        // bound once outside every loop: immutable, so fixed for the rest of the
                        // run, and an atom of its own named after the local, as a parameter is
                        // (docs/cost-model.md § A size bound once). Inside a loop it would be a
                        // new value every lap, and stays no size
                        else if self.loops.is_empty() {
                            let name = self.f.locals[*id].name.clone();
                            let a = self.new_atom(&name);
                            self.local_affine.insert(*id, Affine::constant(Poly::var(a)));
                        },
                    }
                } else if l.ty == Ty::I64 {
                    let v = self.exact_size(e).map(|p| vec![p]);
                    self.initial.insert(*id, v);
                }
                Ok(())
            }
            Stmt::LetBuild { id, len, var, body } => {
                self.expr(len)?;
                let Some(size) = self.size_of(len, Dir::Upper) else {
                    return Err(Fail::Unknown(format!("the length of `{}` is not a size expression", self.f.locals[*id].name), len.line));
                };
                let es = self.elem_bytes(*id);
                self.stream(&size, es);
                self.local_size.insert(*id, size.clone());
                self.local_root.insert(*id, *id);
                let atom = self.new_atom(&self.f.locals[*var].name.clone());
                self.enter_loop(Loop { id: 0, var: Some(*var), atom: Some(atom), trip: size, lo: Poly::zero(), step: 1, offset: Some(Affine::constant(Poly::zero())), monotone: false });
                self.add_work_n(3); // store, increment, compare-and-branch
                let w = self.written_roots(body);
                self.loop_writes.push(w);
                let r = self.block(body);
                r?;
                self.leave_loop(body)?;
                self.loop_writes.pop();
                Ok(())
            }
            Stmt::LetRepeat(id, e, n) => {
                self.expr(e)?;
                self.expr(n)?;
                let Some(size) = self.size_of(n, Dir::Upper) else {
                    return Err(Fail::Unknown(format!("the length of `{}` is not a size expression", self.f.locals[*id].name), n.line));
                };
                self.add_work(size.clone());
                let es = self.elem_bytes(*id);
                self.stream(&size, es);
                self.local_size.insert(*id, size);
                self.local_root.insert(*id, *id);
                Ok(())
            }
            Stmt::LetArray(id, elems) => {
                for e in elems { self.expr(e)?; }
                let k = elems.len() as i128;
                self.add_work_n(k);
                let es = self.elem_bytes(*id);
                self.stream(&Poly::constant(k), es);
                self.local_size.insert(*id, Poly::constant(k));
                self.local_root.insert(*id, *id);
                Ok(())
            }
            Stmt::Assign(lv, op, e) => {
                self.recognise(s);
                self.expr(e)?;
                if let LValue::Var(v) = lv {
                    if self.f.locals[*v].ty == Ty::I64 {
                        // inside a loop the value at the loop's next iteration is not this one
                        let val = if op.is_none() && self.loops.is_empty() { self.exact_size(e).map(|p| vec![p]) } else { None };
                        self.initial.insert(*v, val);
                    }
                }
                if let (LValue::Var(_), ExprKind::Local(_)) = (lv, &e.kind) { self.copy_value(&e.ty); }
                // `s = f(…)`, `f -> [T; k]`: a new buffer, as a `let` of the call gives one — what
                // was read from the old one, or resident of it, is not of this one (§7)
                if let LValue::Var(v) = lv {
                    if self.f.locals[*v].ty.is_arrayish() {
                        self.last_result = None;
                        self.last_result_atom = None;
                        self.wrote(*v);
                        let root = self.local_root.get(v).copied().unwrap_or(*v);
                        self.resident.retain(|r| r.root != root);
                    }
                }
                match lv {
                    // a register: only the operation of `op=` costs
                    LValue::Var(_) => self.add_work_n(if op.is_some() { 1 } else { 0 }),
                    // a store, and for `op=` a load and the operation as well
                    LValue::Index(arr, idx, _) => {
                        self.expr(idx)?;
                        self.add_work_n(if op.is_some() { 3 } else { 1 });
                        // a whole holder stored: a store per element of its array fields too (§9)
                        if let Ty::Struct(sid) = e.ty { self.add_work_n(self.an.m.structs[sid].array_part().0); }
                        self.storing = true;
                        self.access(*arr, idx, None);
                        self.storing = false;
                        self.wrote(*arr);
                    }
                    LValue::Field(..) => self.add_work_n(if op.is_some() { 1 } else { 0 }),
                    // a store into a value array: resident, no bytes (§3)
                    LValue::FieldIndex(_, idx, _, _) => {
                        self.expr(idx)?;
                        self.add_work_n(if op.is_some() { 3 } else { 1 });
                    }
                    LValue::IndexField(arr, idx, fi, _) => {
                        self.expr(idx)?;
                        self.add_work_n(if op.is_some() { 3 } else { 1 });
                        self.storing = true;
                        self.access(*arr, idx, Some(*fi));
                        self.storing = false;
                        self.wrote(*arr);
                    }
                    // `xs[i].f[j] = e`: a store into element `i`'s array field, a site on the
                    // field as `xs[i].f` is (docs/arrays-by-value-design.md §9)
                    LValue::IndexFieldIndex(arr, idx, fi, j, _) => {
                        self.expr(idx)?;
                        self.expr(j)?;
                        self.add_work_n(if op.is_some() { 3 } else { 1 });
                        self.storing = true;
                        self.access(*arr, idx, Some(*fi));
                        self.storing = false;
                        self.wrote(*arr);
                    }
                }
                Ok(())
            }
            Stmt::Reassign(idx) => {
                let r = self.f.reassigns[*idx].clone();
                self.wrote(r.target);
                let size = self.local_size.get(&r.src).or_else(|| self.local_size.get(&r.target)).cloned();
                if r.in_place {
                    self.add_work_n(1); // a pointer and a length, not a byte moved
                    let (ynm, xnm) = (self.f.locals[r.target].name.clone(), self.f.locals[r.src].name.clone());
                    let note = format!("`{ynm} = {xnm}` (line {}) reuses `{xnm}`'s buffer in place: moves 0", r.line);
                    if !self.notes.contains(&note) { self.notes.push(note); }
                } else {
                    let Some(sz) = &size else {
                        return Err(Fail::Unknown(format!("the size of `{}` is not tracked", self.f.locals[r.src].name), r.line));
                    };
                    self.add_work(sz.clone());
                    let es = self.elem_bytes(r.target);
                    self.stream(sz, es);
                    let (ynm, xnm) = (self.f.locals[r.target].name.clone(), self.f.locals[r.src].name.clone());
                    let note = match r.conflict_line {
                        Some(vl) => format!("`{ynm} = {xnm}` (line {}) copies: a view of `{xnm}` is still read at line {vl}", r.line),
                        None => format!("`{ynm} = {xnm}` (line {}) copies: inside a loop, `{xnm}` may be read again next lap", r.line),
                    };
                    if !self.notes.contains(&note) { self.notes.push(note); }
                }
                if let Some(sz) = size { self.local_size.insert(r.target, sz); }
                self.local_root.insert(r.target, r.target);
                Ok(())
            }
            Stmt::For { var, start, end, body } => {
                self.expr(start)?;
                self.expr(end)?;
                let (Some(lo), Some(hi)) = (self.size_of(start, Dir::Lower), self.size_of(end, Dir::Upper)) else {
                    return Err(Fail::Unknown("loop bound is not a size expression".into(), start.line.max(end.line)));
                };
                // the trip count is exact when the outer loop variables cancel between the two
                // bounds (`ii*T .. ii*T+T` is `T`); otherwise the rectangular hull
                let a_lo = self.affine(start);
                let a_hi = self.affine(end);
                let mut trip = match (&a_lo, &a_hi) {
                    (Some(l), Some(h)) => {
                        let d = h.add(&l.scale(&Poly::constant(-1)));
                        if d.is_const() { d.konst } else { hi.sub(&lo) }
                    }
                    _ => hi.sub(&lo),
                };
                if let Some(c) = trip.as_const() { if c.n < 0 { trip = Poly::zero(); } }
                let _ = hi;
                let atom = self.new_atom(&self.f.locals[*var].name.clone());
                self.enter_loop(Loop { id: 0, var: Some(*var), atom: Some(atom), trip, lo, step: 1, offset: a_lo, monotone: false });
                self.add_work_n(2); // increment, compare-and-branch, per iteration
                let w = self.written_roots(body);
                self.loop_writes.push(w);
                let r = self.block(body);
                r?;
                self.leave_loop(body)?;
                self.loop_writes.pop();
                Ok(())
            }
            Stmt::ParFor { var, end, body, acc, op } => {
                let _ = (acc, op); // the combine's own work is inside `body`, walked normally
                self.expr(end)?;
                let Some(hi) = self.size_of(end, Dir::Upper) else {
                    return Err(Fail::Unknown("a `.par()` chain's length is not a size expression".into(), end.line));
                };
                let mut trip = hi;
                if let Some(c) = trip.as_const() { if c.n < 0 { trip = Poly::zero(); } }
                let atom = self.new_atom(&self.f.locals[*var].name.clone());
                self.enter_loop(Loop { id: 0, var: Some(*var), atom: Some(atom), trip: trip.clone(), lo: Poly::zero(), step: 1, offset: Some(Affine::constant(Poly::zero())), monotone: false });
                self.add_work_n(2);
                let w = self.written_roots(body);
                self.loop_writes.push(w);
                let r = self.block(body);
                r?;
                self.leave_par_loop(body, &trip)?;
                self.loop_writes.pop();
                Ok(())
            }
            Stmt::Break => Ok(()),
            Stmt::While { cond, decreasing, body, line } => {
                self.scan_ind.borrow_mut().take();
                self.expr(cond)?;
                // the condition and the measure are read again every iteration: a size read from
                // an array the body writes is not one
                let w = self.written_roots(body);
                self.loop_writes.push(w);
                // the trip count: the programmer's measure, else an induction variable found in
                // the condition and stepped by a constant in the body
                // (trip, induction variable and its entry value, when there is one)
                let found = match decreasing {
                    Some(m) => {
                        self.expr(m)?;
                        // the measure's value at entry: mutable locals read as what they were last assigned
                        self.at_entry = true;
                        let v = self.size_of(m, Dir::Upper);
                        self.at_entry = false;
                        let Some(v) = v else {
                            return Err(Fail::Unknown("the `decreasing` measure is not a size expression — it reads memory or a value the compiler cannot follow — so this loop has no static bound; `neant measure` can fit one".into(), m.line));
                        };
                        // the promise is checked: along every path through the body that comes
                        // back to the condition, the measure must go down by at least one
                        match self.min_decrease(m, body) {
                            Some(d) if d >= Rat::one() => {}
                            Some(d) => return Err(Fail::Unknown(format!(
                                "the `decreasing` measure is not shown to decrease: there is a path through the body along which it changes by {}",
                                if d.is_zero() { "0".to_string() } else { format!("{}{}", if d.n < 0 { "+" } else { "−" }, Rat::new(d.n.abs(), d.d).to_f64()) }), m.line)),
                            None => return Err(Fail::Unknown("the `decreasing` measure is not shown to decrease: the body changes one of its variables in a way the compiler cannot follow".into(), m.line)),
                        }
                        Some((v, None))
                    }
                    None => match self.induction_trip(cond, body) {
                        Ok(t) => Some(t),
                        Err(why) => return Err(Fail::Unknown(why, *line)),
                    },
                };
                let Some((trip, ind)) = found else {
                    return Err(Fail::Unknown("`while` has no measure the compiler can find; write `while cond decreasing <expr>` with an `i64` that goes down by at least one every iteration".into(), *line));
                };
                let scan = self.scan_ind.borrow_mut().take();
                let (var, atom, lo, step, offset, monotone) = match (ind, scan) {
                    (Some((v, i0, st)), _) => {
                        let a = self.new_atom(&self.f.locals[v].name.clone());
                        (Some(v), Some(a), i0.clone(), st, Some(Affine::constant(i0)), false)
                    }
                    // a scan: its index moves on, for the sites (§ A scan's accesses)
                    (None, Some((v, i0, st))) => {
                        let a = self.new_atom(&self.f.locals[v].name.clone());
                        (Some(v), Some(a), i0.clone(), st, Some(Affine::constant(i0)), true)
                    }
                    (None, None) => (None, None, Poly::zero(), 1, None, false),
                };
                self.enter_loop(Loop { id: 0, var, atom, trip, lo, step, offset, monotone });
                self.add_work_n(1);
                // the condition runs once more than the body: the walk above was the last, failing
                // test, and this one is the test before each iteration. Only its memory is
                // charged here — its work is the loop's compare-and-branch, as it always was
                let (w0, sp0) = (self.work.clone(), self.span.clone());
                self.expr(cond)?;
                self.work = w0;
                self.span = sp0;
                let cands = self.amortised_calls(body);
                let spans: Vec<Poly> = cands.iter().map(|(_, sp)| sp.clone()).collect();
                self.amort.push((self.loops.len(), cands.into_iter().map(|(c, _)| c).collect()));
                let r = self.block(body);
                if r.is_err() { self.amort.pop(); }
                r?;
                let left = self.leave_loop(body);
                let (_, done) = self.amort.pop().unwrap();
                left?;
                // the amortised calls' distance, charged once for the whole loop: a chain shares
                // one distance, so its calls' largest rate, once
                let mut charged: Vec<usize> = Vec::new();
                for (c, span) in done.iter().zip(&spans) {
                    if charged.contains(&c.group) { continue; }
                    let members: Vec<&Amort> = done.iter().filter(|d| d.group == c.group).collect();
                    // a call of the chain the walk never reached leaves its rate unknown: charge
                    // nothing amortised for the group only if none was stripped
                    if members.iter().any(|d| d.alpha.is_none()) && members.iter().any(|d| d.alpha.is_some()) {
                        // mixed: those stripped still need their distance — charge each its own
                        for d in members.iter().filter(|d| d.alpha.is_some()) {
                            let (aw, am) = d.alpha.as_ref().unwrap();
                            if !self.replay {
                                self.work = self.work.add(&aw.mul_poly(span)); self.span = self.span.add(&aw.mul_poly(span)); self.moves = self.moves.add(&am.mul_poly(span).add_poly(&d.edge));
                                self.serial = self.serial.add(&d.rates.0.mul_poly(span)); self.divs = self.divs.add(&d.rates.1.mul_poly(span));
                            }
                        }
                        charged.push(c.group);
                        continue;
                    }
                    charged.push(c.group);
                    let Some((aw, am)) = members.iter().filter_map(|d| d.alpha.clone()).reduce(|(a, b), (x, y)| (a.max(&x), b.max(&y))) else { continue };
                    // the chain's ends once: its calls' largest
                    let edge = members.iter().map(|d| d.edge.clone()).fold(Poly::zero(), |a, e| if super::piece::dominates(&e, &a) { e } else { a });
                    let (wc, mc) = (aw.mul_poly(span), am.mul_poly(span).add_poly(&edge));
                    if !self.replay {
                        self.work = self.work.add(&wc);
                        self.span = self.span.add(&wc);
                        self.moves = self.moves.add(&mc);
                        // serial work and divisions: the chain's largest rate, once
                        let pick = |f: &dyn Fn(&Amort) -> Cost| members.iter().map(|d| f(d)).reduce(|a, b| a.max(&b)).unwrap_or_default();
                        let (rs, rd) = (pick(&|d| d.rates.0.clone()), pick(&|d| d.rates.1.clone()));
                        self.serial = self.serial.add(&rs.mul_poly(span));
                        self.divs = self.divs.add(&rd.mul_poly(span));
                    }
                    let callee = self.an.m.funcs[c.fid].name.clone();
                    let arr = self.f.locals[c.arr].name.clone();
                    let var = self.f.locals[c.var].name.clone();
                    self.scans.borrow_mut().push(format!("scan: the calls to `{callee}` at line {} are amortised: `{var}` only moves on through where `{callee}` stopped in `{arr}`, so together they are charged {} once, not a scan of `{arr}` a lap",
                        c.line, wc.display(&self.names)));
                }
                self.loop_writes.pop();
                Ok(())
            }
            Stmt::Expr(e) => { self.recognise(s); self.expr(e) }
            Stmt::Return(Some(e)) => self.expr(e),
            Stmt::Return(None) => Ok(()),
        }
    }

    /// The one value a mutable local holds on entry to a loop, when exactly one definition reaches
    /// it. Several definitions need `max` in the cost algebra to give a bound; until then they
    /// are "not a size".
    fn entry_value(&self, l: LocalId) -> Option<Poly> {
        match self.initial.get(&l) {
            Some(Some(vs)) if vs.len() == 1 => Some(vs[0].clone()),
            _ => None,
        }
    }

    /// The least amount the measure `m` decreases along any path through `body` that reaches the
    /// end of the body (a path that leaves by `break` or `return` need not decrease it). `m` is
    /// read as an affine form in the locals; an assignment `x += c` to a local with coefficient
    /// `k` in `m` changes `m` by `k·c`. A nested loop that only ever decreases `m` contributes
    /// nothing (it may run zero times); one that can increase it, or any update the affine
    /// reading cannot follow, is `None`.
    fn min_decrease(&self, m: &Expr, body: &Block) -> Option<Rat> {
        // coefficients of the locals in m
        let mut coef: HashMap<LocalId, Rat> = HashMap::new();
        fn collect(e: &Expr, sign: Rat, coef: &mut HashMap<LocalId, Rat>) -> bool {
            match &e.kind {
                ExprKind::Local(l) => { let c = coef.entry(*l).or_insert(Rat::zero()); *c = c.add(sign); true }
                ExprKind::Int(_) | ExprKind::Len(_) => true,
                ExprKind::Binary(BinOp::Add, a, b) => collect(a, sign, coef) && collect(b, sign, coef),
                ExprKind::Binary(BinOp::Sub, a, b) => collect(a, sign, coef) && collect(b, sign.neg(), coef),
                ExprKind::Binary(BinOp::Mul, a, b) => {
                    match (&a.kind, &b.kind) {
                        (ExprKind::Int(k), _) => collect(b, sign.mul(Rat::int(*k as i128)), coef),
                        (_, ExprKind::Int(k)) => collect(a, sign.mul(Rat::int(*k as i128)), coef),
                        _ => false,
                    }
                }
                ExprKind::Cast(a, Ty::I64) => collect(a, sign, coef),
                _ => false,
            }
        }
        if !collect(m, Rat::one(), &mut coef) { return None; }
        let coef = &coef;
        // Some(Some(d)): the path falls through decreasing m by at least d; Some(None): every
        // path leaves the loop; None: cannot follow
        fn block(b: &Block, coef: &HashMap<LocalId, Rat>, f: &Func) -> Option<Option<Rat>> {
            let mut acc = Rat::zero();
            // an `if` in tail position is a statement to this analysis
            let tail_stmt = b.tail.as_ref().map(|t| Stmt::Expr((**t).clone()));
            for s in b.stmts.iter().chain(tail_stmt.iter()) {
                match s {
                    Stmt::Break | Stmt::Return(_) => return Some(None),
                    Stmt::Assign(LValue::Var(v), op, e) if coef.get(v).is_some_and(|k| !k.is_zero()) => {
                        let k = coef[v];
                        // the change to v: op= c, or v = v ± c
                        let delta: Option<i128> = match (op, &e.kind) {
                            (Some(BinOp::Add), ExprKind::Int(c)) => Some(*c as i128),
                            (Some(BinOp::Sub), ExprKind::Int(c)) => Some(-(*c as i128)),
                            (None, ExprKind::Binary(BinOp::Add, a, c)) if matches!(a.kind, ExprKind::Local(l) if l == *v) => if let ExprKind::Int(c) = c.kind { Some(c as i128) } else { None },
                            (None, ExprKind::Binary(BinOp::Sub, a, c)) if matches!(a.kind, ExprKind::Local(l) if l == *v) => if let ExprKind::Int(c) = c.kind { Some(-(c as i128)) } else { None },
                            _ => None,
                        };
                        let delta = delta?;
                        // m changes by k·delta, so it decreases by −k·delta
                        acc = acc.add(k.mul(Rat::int(delta)).neg());
                    }
                    // a `.par()` chain's body cannot assign to anything outside it (its closures
                    // are already checked pure), so it can never touch the measure either
                    Stmt::Assign(LValue::Var(_) | LValue::Index(..) | LValue::Field(..) | LValue::IndexField(..) | LValue::FieldIndex(..) | LValue::IndexFieldIndex(..), _, _) | Stmt::Let(..) | Stmt::LetArray(..) | Stmt::LetRepeat(..) | Stmt::LetBuild { .. } | Stmt::Reassign(..) | Stmt::ParFor { .. } => {}
                    Stmt::Expr(Expr { kind: ExprKind::If(_, t, e), .. }) => {
                        let bt = block(t, coef, f)?;
                        let be = match e { Some(e) => block(e, coef, f)?, None => Some(Rat::zero()) };
                        match (bt, be) {
                            (None, None) => return Some(None),
                            (Some(a), None) | (None, Some(a)) => acc = acc.add(a),
                            (Some(a), Some(b)) => acc = acc.add(if a < b { a } else { b }),
                        }
                    }
                    Stmt::Expr(_) => {}
                    Stmt::For { body, .. } | Stmt::While { body, .. } => {
                        // a nested loop may run zero times: it helps only if it never hurts
                        match block(body, coef, f) {
                            Some(Some(d)) if d >= Rat::zero() => {}
                            Some(None) => {}
                            _ => return None,
                        }
                    }
                }
            }
            Some(Some(acc))
        }
        match block(body, coef, self.f)? {
            Some(d) => Some(d),
            None => Some(Rat::int(1_000_000)), // every path leaves: the loop runs once
        }
    }

    /// `while i < e { … i += c … }` runs at most `(e − i₀)/c` times when `i` is a mutable local
    /// stepped by the constant `c` exactly once in the body and nowhere else, `e` is a size
    /// expression, and `i₀` — the last assignment to `i` before the loop — is one too.
    /// `i > e` with `i -= c` is the mirror. Anything else is not an induction variable.
    /// `while s >= 0 { …; s = xs[s].f }` follows a list threaded through `xs`, and runs at most
    /// as many times as the longest walk along `f`, the atom `walk(xs[_].f)` — when `s` is
    /// assigned that once, at the top level of the body, and nowhere else, and nothing in the
    /// body writes `f`, itself or through a callee (cost-model § A walk down a list). `xs[s]` of an
    /// `[i64]` is the same with no field.
    /// `Err(None)` when the loop is not that shape, `Err(Some(why))` when it is but the list may
    /// change under it.
    fn walk_trip(&self, var: LocalId, body: &Block) -> Result<Poly, Option<String>> {
        let is_step = |st: &Stmt| -> Option<(LocalId, Option<usize>)> {
            let Stmt::Assign(LValue::Var(v), None, e) = st else { return None };
            if *v != var { return None; }
            let (base, field) = match &e.kind {
                ExprKind::Field(inner, fi) => (&**inner, Some(*fi)),
                _ => (e, None),
            };
            let ExprKind::Index(arr, idx) = &base.kind else { return None };
            if !matches!(idx.kind, ExprKind::Local(l) if l == var) { return None; }
            let ok = match field { Some(fi) => self.field_ty(*arr, fi) == Some(&Ty::I64), None => self.f.locals[*arr].ty.elem() == Some(&Ty::I64) };
            ok.then_some((*arr, field))
        };
        let steps: Vec<usize> = body.stmts.iter().enumerate().filter(|(_, st)| is_step(st).is_some()).map(|(k, _)| k).collect();
        let [k] = steps[..] else { return Err(None) };
        let Some((arr, field)) = is_step(&body.stmts[k]) else { return Err(None) };
        let rest = Block { stmts: body.stmts.iter().enumerate().filter(|(j, _)| *j != k).map(|(_, st)| st.clone()).collect(), tail: body.tail.clone(), ty: body.ty.clone() };
        if assigns(&rest, |l| l == var) { return Err(None); }
        let mut roots = self.local_root.clone();
        let root = roots.get(&arr).copied().unwrap_or(arr);
        let mut moved = false;
        stores_block(body, self.f, &mut roots, &self.an.field_writes, &mut |r, fi| {
            if r == root && (fi.is_none() || field.is_none() || fi == field) { moved = true; }
        });
        let name = self.f.locals[root].name.clone();
        let fname = field.and_then(|fi| match self.f.locals[root].ty.elem() {
            Some(Ty::Struct(sid)) => self.an.m.structs[*sid].fields.get(fi).map(|(n, _)| n.clone()),
            _ => None,
        });
        if moved {
            let list = match &fname { Some(f) => format!("{name}[_].{f}"), None => format!("{name}[_]") };
            return Err(Some(format!("`{}` walks the list `{list}`, which the body may write, itself or through a call", self.f.locals[var].name)));
        }
        let root_ref = match self.f.params.iter().position(|&p| p == root) { Some(i) => Root::Param(i, name), None => Root::Local(name) };
        Ok(Poly::atom(Atom::Read(Box::new(Read { id: 0, root: root_ref, index: None, field: fname, least: false, walk: true }))))
    }

    /// The least a mutable `i64` is on entry to the loop being walked: its one entry value, else
    /// what it was first bound to when nothing ever makes it smaller (§ A scan).
    fn start_of(&self, v: LocalId) -> Option<Poly> {
        use super::scan::{only_increased, Base};
        if let Some(p) = self.entry_value(v) { return Some(p); }
        only_increased(self.f, &self.an.scan_summ, v)?.iter().find_map(|(b, c)| {
            let base = match b {
                Base::Zero => Poly::zero(),
                Base::Param(p) => self.local_affine.get(&self.f.params[*p]).filter(|a| a.is_const())?.konst.clone(),
                Base::Cur => return None,
            };
            Some(base.add(&Poly::from_rat(*c)))
        })
    }

    /// The calls in a `while` body amortised against one index (docs/cost-model.md § An amortised
    /// scan): `let r = g(.., a, .., v, ..)` at the body's top level, `g` advancing its index through
    /// `a`, and later `v = r.f + c` with `c ≥ 0`, the only assignment to `v` in the body; with what
    /// the loop's total adds, `α·(a.len() − v₀)`, needing `a`'s length and `v₀` known now.
    fn amortised_calls(&self, body: &Block) -> Vec<(Amort, Poly)> {
        // a chain: `let r₁ = g(.., a, .., v, ..)`, then each next call at `rₖ.f + c` of the one
        // before, and finally `v = r_last.f + c`, every `c ≥ 0` — the distances of all its calls,
        // over all laps, still telescope to at most `a.len() − v₀`
        let mut out = Vec::new();
        let mut group = 0;
        let mut k = 0;
        while k < body.stmts.len() {
            let Stmt::Let(r0, Expr { kind: ExprKind::Call(g, args), line, .. }) = &body.stmts[k] else { k += 1; continue };
            let Some(adv) = self.an.advances[*g] else { k += 1; continue };
            let Some(Expr { kind: ExprKind::Local(v), .. }) = args.get(adv.p) else { k += 1; continue };
            let v = *v;
            if !self.f.locals[v].mutable || self.f.locals[v].ty != Ty::I64 { k += 1; continue; }
            let arr_of = |args: &[Expr], a: usize| match args.get(a).map(|x| &x.kind) { Some(ExprKind::Ref(x, _)) | Some(ExprKind::Local(x)) => Some(*x), _ => None };
            let Some(arr) = arr_of(args, adv.a) else { k += 1; continue };
            let mut chain = vec![(*line, *g, adv)];
            let (mut last_r, mut last_f) = (*r0, adv.field);
            let mut closed = false;
            let mut t = k + 1;
            while t < body.stmts.len() {
                match &body.stmts[t] {
                    Stmt::Let(r, Expr { kind: ExprKind::Call(g2, a2), line: l2, .. }) => {
                        if let Some(ad2) = self.an.advances[*g2] {
                            if a2.get(ad2.p).is_some_and(|e| from_field(e, last_r, last_f)) && arr_of(a2, ad2.a) == Some(arr) {
                                chain.push((*l2, *g2, ad2));
                                (last_r, last_f) = (*r, ad2.field);
                            }
                        }
                    }
                    Stmt::Assign(LValue::Var(w), None, e) if *w == v => { closed = from_field(e, last_r, last_f); break; }
                    _ => {}
                }
                t += 1;
            }
            if !closed || count_assigns(body, v) != 1 { k += 1; continue; }
            let (Some(len), Some(v0)) = (self.local_size.get(&arr).cloned(), self.start_of(v)) else { k += 1; continue };
            for (l, gid, ad) in chain {
                out.push((Amort { line: l, fid: gid, adv: ad, arr, var: v, alpha: None, group, edge: Poly::zero(), rates: (Cost::zero(), Cost::zero()) }, len.sub(&v0)));
            }
            group += 1;
            k = t + 1;
        }
        out
    }

    /// `while h < t` over a worklist (docs/cost-model.md § A bounded worklist): `t` is assigned in
    /// the body only as `t += 1` straight after a write `a[t] = …` in the same block, which the
    /// bounds check stops unless `t < a.len()`; so `t` is never more than `max(t₀, a.len())`, at
    /// most `t₀ + a.len()` for `t₀ ≥ 0`, and `h`, stepped by `step` a lap from `h₀`, meets it within
    /// `(t₀ + a.len() − h₀)/step` laps.
    fn worklist_trip(&self, cond: &Expr, h: LocalId, bound: &Expr, body: &Block, step: i128) -> Option<Poly> {
        let ExprKind::Local(t) = bound.kind else { return None };
        if t == h || !self.f.locals[t].mutable || self.f.locals[t].ty != Ty::I64 { return None; }
        let mut arr = None;
        if !pushes_only(body, t, &mut arr) { return None; }
        let a = arr?;
        let len = self.local_size.get(&a)?.clone();
        let t0 = self.entry_value(t)?;
        if !super::piece::dominates(&t0, &Poly::zero()) { return None; }
        let h0 = self.entry_value(h)?;
        let trip = t0.add(&len).sub(&h0).scale(Rat::new(1, step));
        let (hn, tn, an) = (&self.f.locals[h].name, &self.f.locals[t].name, &self.f.locals[a].name);
        self.scans.borrow_mut().push(format!("worklist: `{tn}` only grows by one straight after a checked write `{an}[{tn}]`, so it stays at most {} and the `while` at line {} runs at most {} times as `{hn}` meets it",
            t0.add(&len).display(&self.names), cond.line, trip.display(&self.names)));
        Some(trip)
    }

    /// `while i < e` bounded as a scan (docs/cost-model.md § A scan): `i` grows by at least `d`
    /// along every path through the body, from at least `i₀`, so the loop runs at most
    /// `(e − i₀)/d` times. `Err(None)`: the rule does not apply and the caller's own reason
    /// stands; `Err(Some(why))`: it applies as far as `why`. `step` is the constant step when there
    /// is one and only the entry value was missing.
    fn scan_trip(&self, cond: &Expr, var: LocalId, bound: &Expr, op: &BinOp, body: &Block, step: Option<i128>) -> Result<(Poly, Option<(LocalId, Poly, i128)>), Option<String>> {
        use super::scan::{least_growth_in, only_increased, Base};
        if !matches!(op, BinOp::Lt | BinOp::Le) || !matches!(&cond.kind, ExprKind::Binary(_, l, _) if matches!(l.kind, ExprKind::Local(v) if v == var)) { return Err(None); }
        let name = self.f.locals[var].name.clone();
        let d = match step {
            Some(c) if c >= 1 => Rat::int(c),
            Some(_) => return Err(None),
            None => match least_growth_in(self.an.m, self.f, &self.an.scan_summ, cond, body, var) {
                Some(Some(d)) if d >= Rat::one() => d,
                Some(Some(_)) => return Err(Some(format!("`{name}` does not grow on every path through the body, so this is not a scan"))),
                Some(None) => Rat::one(),
                None => return Err(None),
            },
        };
        let mut w = Writes::default();
        w.block(body);
        if w.locals.iter().any(|&l| l != var && bound_mentions(bound, l)) { return Err(None); }
        let Some(e) = self.size_of(bound, Dir::Upper) else { return Err(None) };
        // at least `i₀` on entry: its one entry value, else what it was first bound to when
        // nothing ever makes it smaller
        let i0 = match self.initial.get(&var) {
            Some(Some(vs)) if vs.len() == 1 => vs[0].clone(),
            _ => {
                let Some(lbs) = only_increased(self.f, &self.an.scan_summ, var) else {
                    return Err(Some(format!("`{name}`'s entry value is not known and `{name}` is not only increased")));
                };
                let as_poly = |(b, c): &(Base, Rat)| -> Option<Poly> {
                    let base = match b {
                        Base::Zero => Poly::zero(),
                        Base::Param(p) => self.local_affine.get(&self.f.params[*p]).filter(|a| a.is_const())?.konst.clone(),
                        Base::Cur => return None,
                    };
                    Some(base.add(&Poly::from_rat(*c)))
                };
                match lbs.iter().find_map(as_poly) {
                    Some(p) => p,
                    None => return Err(Some(format!("`{name}`'s entry value is not known and nothing bounds it below"))),
                }
            }
        };
        let span = e.sub(&i0);
        let span = if matches!(op, BinOp::Le) { span.add(&Poly::from_rat(d)) } else { span };
        let trip = span.scale(Rat::new(d.d, d.n));
        self.scans.borrow_mut().push(format!("scan: `{name}` grows by at least {} a lap, so the `while` at line {} runs at most {} times",
            d.to_f64(), cond.line, trip.display(&self.names)));
        if d.is_int() { *self.scan_ind.borrow_mut() = Some((var, i0, d.n)); }
        Ok((trip, None))
    }

    /// **A scan to a sentinel** (docs/cost-model.md § A scan to a sentinel): `while xs[i].f >= 0 { …;
    /// i += c }` reads `xs[i]` every time its condition is evaluated, and an index past the end
    /// stops the program, so the loop runs at most `(xs.len() − i₀)/c` times, `i₀` the least `i`
    /// is on entry — when `i` is a mutable `i64` stepped by a constant `c > 0` once, at the top
    /// level of the body, and nowhere else. A bound: the sentinel may come first.
    fn sentinel_trip(&self, cond: &Expr, body: &Block) -> Option<Poly> {
        fn indexed(e: &Expr, out: &mut Vec<(LocalId, LocalId)>) {
            match &e.kind {
                ExprKind::Index(arr, i) => {
                    if let ExprKind::Local(v) = i.kind { out.push((*arr, v)); }
                    indexed(i, out);
                }
                ExprKind::Binary(op, a, b) if !matches!(op, BinOp::And | BinOp::Or) => { indexed(a, out); indexed(b, out); }
                ExprKind::Unary(_, a) | ExprKind::Field(a, _) | ExprKind::Cast(a, _) => indexed(a, out),
                _ => {}
            }
        }
        let mut sites = Vec::new();
        indexed(cond, &mut sites);
        for (arr, v) in sites {
            let l = &self.f.locals[v];
            if !l.mutable || l.ty != Ty::I64 { continue; }
            let Some(c) = single_step(body, v).filter(|&c| c > 0) else { continue };
            let top = body.stmts.iter().any(|st| matches!(st, Stmt::Assign(LValue::Var(x), _, _) if *x == v));
            if !top { continue; }
            let root = self.local_root.get(&arr).copied().unwrap_or(arr);
            let Some(len) = self.local_size.get(&root) else { continue };
            let Some(i0) = self.start_of(v) else { continue };
            let span = len.sub(&i0).add(&Poly::constant(c as i128 - 1));
            let trip = span.scale(Rat::new(1, c as i128));
            self.scans.borrow_mut().push(format!("scan: `{}` indexes `{}` in the `while` condition at line {} and grows by {c} a lap, so the loop runs at most {} times",
                l.name, self.f.locals[root].name, cond.line, trip.display(&self.names)));
            return Some(trip);
        }
        None
    }

    /// **A cursor in a slot** (docs/cost-model.md § A cursor in a slot): `while ds[0] < ds[1] { …;
    /// ds[0] = ds[0] + c }` counts as an induction variable does, the slot `ds[k]` of an array of
    /// scalars standing for the variable — a parser keeps its cursor so, to hand it to callees. It
    /// applies when the slot is stepped by a constant `c > 0` once, at the top level of the body, and
    /// written nowhere else in it, itself or through a callee; and when the bound is a size nothing
    /// in the body writes. `(bound − ds[k])/c` laps, each read at entry to the loop.
    fn slot_trip(&self, cond: &Expr, body: &Block) -> Option<Poly> {
        let ExprKind::Binary(op, l, r) = &cond.kind else { return None };
        let slot = |e: &Expr| match &e.kind {
            ExprKind::Index(a, i) => match i.kind { ExprKind::Int(k) if k >= 0 && self.f.locals[*a].ty.elem().is_some_and(|t| t.is_scalar()) => Some((*a, k)), _ => None },
            _ => None,
        };
        let (cur, bound, le) = match (op, slot(l), slot(r)) {
            (BinOp::Lt, Some(s), _) => (s, &**r, false),
            (BinOp::Le, Some(s), _) => (s, &**r, true),
            (BinOp::Gt, _, Some(s)) => (s, &**l, false),
            (BinOp::Ge, _, Some(s)) => (s, &**l, true),
            _ => return None,
        };
        let (arr, k) = cur;
        let is_cur = |e: &Expr| slot(e) == Some((arr, k));
        // the step, once at the top level
        let steps: Vec<i64> = body.stmts.iter().filter_map(|st| match st {
            Stmt::Assign(LValue::Index(a, i, _), op, e) if *a == arr && matches!(i.kind, ExprKind::Int(x) if x == k) => match (op, &e.kind) {
                (Some(BinOp::Add), ExprKind::Int(c)) => Some(*c),
                (None, ExprKind::Binary(BinOp::Add, x, c)) if is_cur(x) => if let ExprKind::Int(c) = c.kind { Some(c) } else { Some(0) },
                _ => Some(0),
            },
            _ => None,
        }).collect();
        let [c] = steps[..] else { return None };
        if c <= 0 { return None; }
        // nothing else writes the slot, and nothing writes what the bound reads
        let mut roots = self.local_root.clone();
        let root = roots.get(&arr).copied().unwrap_or(arr);
        let mut bound_slots: Vec<(LocalId, Option<usize>)> = Vec::new();
        fn reads(e: &Expr, out: &mut Vec<(LocalId, Option<usize>)>, bad: &mut bool) {
            match &e.kind {
                ExprKind::Index(a, i) => { out.push((*a, match i.kind { ExprKind::Int(k) if k >= 0 => Some(IDX + k as usize), _ => None })); reads(i, out, bad); }
                ExprKind::Binary(_, a, b) | ExprKind::MinMax(_, a, b) => { reads(a, out, bad); reads(b, out, bad); }
                ExprKind::Unary(_, a) | ExprKind::Cast(a, _) => reads(a, out, bad),
                ExprKind::Field(..) | ExprKind::Call(..) | ExprKind::If(..) | ExprKind::Block(_) => *bad = true,
                _ => {}
            }
        }
        let mut bad = false;
        reads(bound, &mut bound_slots, &mut bad);
        if bad { return None; }
        let bound_roots: Vec<(LocalId, Option<usize>)> = bound_slots.iter().map(|(a, s)| (self.local_root.get(a).copied().unwrap_or(*a), *s)).collect();
        let mut own = 0;
        let mut clash = false;
        stores_block(body, self.f, &mut roots, &self.an.field_writes, &mut |rt, fi| {
            if rt == root && (fi.is_none() || fi == Some(IDX + k as usize)) { own += 1; }
            if bound_roots.iter().any(|(br, bs)| *br == rt && (fi.is_none() || bs.is_none() || *bs == fi)) { clash = true; }
        });
        if own != 1 || clash { return None; }
        let mut w = Writes::default();
        w.block(body);
        if w.locals.iter().any(|&lc| bound_mentions(bound, lc)) { return None; }
        let e = self.size_of(bound, Dir::Upper)?;
        // the slot at entry to the loop, before its own writes
        self.entry_read.set(true);
        let i0 = self.size_of(match l.kind { ExprKind::Index(..) if slot(l) == Some(cur) => l, _ => r }, Dir::Lower);
        self.entry_read.set(false);
        let i0 = i0?;
        let span = e.sub(&i0);
        let span = if le { span.add(&Poly::constant(c as i128)) } else { span.add(&Poly::constant(c as i128 - 1)) };
        Some(span.scale(Rat::new(1, c as i128)))
    }

    /// The least an `i64` argument can be, as a size (§ A scan).
    fn least_of(&self, a: &Expr) -> Option<Poly> {
        use super::scan::{least, Base};
        least(self.f, &self.an.scan_summ, a).iter().find_map(|(b, d)| {
            let base = match b {
                Base::Zero => Poly::zero(),
                Base::Param(p) => self.local_affine.get(&self.f.params[*p]).filter(|a| a.is_const())?.konst.clone(),
                Base::Cur => return None,
            };
            Some(base.add(&Poly::from_rat(*d)))
        })
    }

    fn induction_trip(&self, cond: &Expr, body: &Block) -> Result<(Poly, Option<(LocalId, Poly, i128)>), String> {
        let ask = "`while` has no measure the compiler can find; write `while cond decreasing <expr>` with an `i64` that goes down by at least one every iteration".to_string();
        // `a && b` stops no later than either: the first conjunct with a trip bounds the loop, as
        // a `break` does — an upper bound (cost-model § Loops without a range)
        if let ExprKind::Binary(BinOp::And, a, b) = &cond.kind {
            return match self.induction_trip(a, body) {
                Ok(t) => Ok(t),
                Err(ea) => self.induction_trip(b, body).map_err(|eb| if eb == ask { ea } else { eb }),
            };
        }
        if let Some(t) = self.sentinel_trip(cond, body) { return Ok((t, None)); }
        if let Some(t) = self.slot_trip(cond, body) { return Ok((t, None)); }
        let ExprKind::Binary(op, l, r) = &cond.kind else { return Err(ask) };
        // `i + c < e` is `i < e − c`, on either side: the variable alone on its side
        let unshift = |x: &Expr, y: &Expr| -> Option<(Expr, Expr)> {
            match &x.kind {
                ExprKind::Binary(BinOp::Add, a, c) if matches!(a.kind, ExprKind::Local(_)) && matches!(c.kind, ExprKind::Int(_)) => {
                    Some(((**a).clone(), Expr { kind: ExprKind::Binary(BinOp::Sub, Box::new(y.clone()), c.clone()), ty: y.ty.clone(), line: y.line }))
                }
                _ => None,
            }
        };
        let (l, r): (Expr, Expr) = match (unshift(l, r), unshift(r, l)) {
            (Some((v, b)), _) => (v, b),
            (None, Some((v, b))) => (b, v),
            _ => ((**l).clone(), (**r).clone()),
        };
        let (l, r) = (&l, &r);
        // normalise to (var, bound, ascending); when both sides are locals, the variable is the
        // one the body assigns — `j > start` walks `j`, not `start`
        let assigned = |e: &Expr| matches!(e.kind, ExprKind::Local(v) if self.f.locals[v].mutable && { let mut w = Writes::default(); w.block(body); w.locals.contains(&v) });
        let (var, bound, asc) = match (&l.kind, &r.kind, op) {
            (ExprKind::Local(_), ExprKind::Local(_), _) if assigned(r) && !assigned(l) => match (&r.kind, op) {
                (ExprKind::Local(v), BinOp::Gt | BinOp::Ge) => (*v, l, true),
                (ExprKind::Local(v), BinOp::Lt | BinOp::Le) => (*v, l, false),
                _ => return Err(ask),
            },
            (ExprKind::Local(v), _, BinOp::Lt | BinOp::Le | BinOp::Ne) => (*v, r, true),
            (ExprKind::Local(v), _, BinOp::Gt | BinOp::Ge) if assigned(l) || !matches!(r.kind, ExprKind::Local(_)) => (*v, r, false),
            (_, ExprKind::Local(v), BinOp::Gt | BinOp::Ge) => (*v, l, true),
            (ExprKind::Local(v), _, BinOp::Gt | BinOp::Ge) => (*v, r, false),
            (_, ExprKind::Local(v), BinOp::Lt | BinOp::Le) => (*v, l, false),
            _ => return Err(ask),
        };
        let name = &self.f.locals[var].name;
        if !self.f.locals[var].mutable || self.f.locals[var].ty != Ty::I64 { return Err(ask); }
        // `s >= 0` with `s = xs[s].f` in the body is a walk down a list, not a count
        let to_negative = matches!((&l.kind, &r.kind, op), (ExprKind::Local(_), ExprKind::Int(0), BinOp::Ge) | (ExprKind::Int(0), ExprKind::Local(_), BinOp::Le));
        if to_negative {
            match self.walk_trip(var, body) {
                // a walk that calls into the component being costed is its recursion, its laps
                // counted as invocations (§ Recursion, a forest): one lap here
                Ok(_) if self.scc.as_ref().is_some_and(|ms| { let mut o = Vec::new(); callees_block(body, &mut o); o.iter().any(|g| ms.contains(g)) }) => return Ok((Poly::constant(1), None)),
                Ok(t) => return Ok((t, None)),
                Err(Some(why)) => return Err(format!("{why}; {ask}")),
                Err(None) => {}
            }
        }
        let Some(step) = single_step(body, var) else {
            let not_stepped = format!("`{name}` is compared in the `while` condition but is not stepped by a constant exactly once in the body; {ask}");
            return self.scan_trip(cond, var, bound, op, body, None).map_err(|why| why.unwrap_or(not_stepped));
        };
        if (asc && step <= 0) || (!asc && step >= 0) { return Err(format!("`{name}` steps away from its bound; {ask}")); }
        let mut w = Writes::default();
        w.block(body);
        if w.locals.iter().any(|&l| l != var && bound_mentions(bound, l)) {
            // a worklist whose tail only grows by a push (docs/cost-model.md § A bounded worklist)
            if asc && matches!(op, BinOp::Lt) && step >= 1 {
                if let Some(t) = self.worklist_trip(cond, var, bound, body, step as i128) { return Ok((t, None)); }
            }
            return Err(format!("the bound of `{name}` is assigned inside the body; {ask}"));
        }
        let Some(e) = self.size_of(bound, if asc { Dir::Upper } else { Dir::Lower }) else { return Err(format!("the bound of `{name}` is not a size expression; {ask}")) };
        let i0 = match self.initial.get(&var) {
            Some(Some(vs)) if vs.len() == 1 => vs[0].clone(),
            Some(Some(vs)) => return Err(format!("`{name}` may hold any of {} values on entry, one per path that defines it; a single value is needed until `max` is in the cost algebra", vs.len())),
            _ => {
                let unknown_entry = format!("`{name}` is assigned inside a loop before this one, so its entry value is not known");
                if !asc { return Err(unknown_entry); }
                return self.scan_trip(cond, var, bound, op, body, Some(step as i128)).map_err(|why| why.unwrap_or(unknown_entry));
            }
        };
        let span = if asc { e.sub(&i0) } else { i0.sub(&e) };
        // `<=` and `>=` run the bound itself too: `i >= 0` from `n − 1` is `n` iterations, not `n − 1`
        let span = if matches!(op, BinOp::Le | BinOp::Ge) { span.add(&Poly::constant(step.abs() as i128)) } else { span };
        // the variable steps by |step| per iteration: indices in it move by step·elem bytes,
        // which the stride rule sees through the loop variable's coefficient
        Ok((span.scale(Rat::new(1, step.abs() as i128)), Some((var, i0, step as i128))))
    }

    fn expr(&mut self, e: &Expr) -> Result<(), Fail> {
        match &e.kind {
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Byte(_) | ExprKind::Local(_) | ExprKind::Ref(..) => Ok(()),
            ExprKind::Len(_) => Ok(()), // the length is already in a register
            // a whole value compared: element by element (docs/arrays-by-value-design.md §8)
            ExprKind::Binary(_, a, b) if !a.ty.is_scalar() => { self.expr(a)?; self.expr(b)?; self.compare_value(&a.ty); Ok(()) }
            ExprKind::Binary(op, a, b) => {
                self.expr(a)?; self.expr(b)?; self.add_work_n(1);
                // an f64 division, counted apart for a time (cost-model § Time, divisions)
                if matches!(op, BinOp::Div) && e.ty == Ty::F64 && !self.replay { self.divs = self.divs.add_poly(&Poly::constant(1)); }
                Ok(())
            }
            ExprKind::MinMax(_, a, b) => { self.expr(a)?; self.expr(b)?; self.add_work_n(2); Ok(()) } // compare, select
            // a row's bound check is a bounds check, which no index is charged for
            ExprKind::InRow(j, n) => { self.expr(j)?; self.expr(n) }
            ExprKind::Unary(_, a) | ExprKind::Cast(a, _) => { self.expr(a)?; self.add_work_n(1); Ok(()) }
            ExprKind::Println(a) => { self.expr(a)?; self.add_work_n(1); if !self.replay { self.io = true; } Ok(()) }
            // a literal of n bytes written out: one call, and its n bytes (the newline one more)
            // read from where the literal lives, a constant (docs/decisions.md §10)
            ExprKind::Text(t, nl) => {
                self.add_work_n(1);
                self.stream(&Poly::constant(t.len() as i128 + *nl as i128), 1);
                if !self.replay { self.io = true; }
                Ok(())
            }
            ExprKind::Index(arr, idx) => {
                self.expr(idx)?;
                self.add_work_n(1);
                // a whole holder: a load per element of its array fields too (§9)
                if let Ty::Struct(sid) = e.ty { self.add_work_n(self.an.m.structs[sid].array_part().0); }
                self.access(*arr, idx, None);
                Ok(())
            }
            // `xs[i].f` is one load of one field, not of the element: the site touches the field
            // and steps by the element, which is what makes AoS and SoA differ
            ExprKind::Field(base, fi) => match &base.kind {
                ExprKind::Index(arr, idx) => {
                    self.expr(idx)?;
                    self.add_work_n(1);
                    self.access(*arr, idx, Some(*fi));
                    Ok(())
                }
                // a field of a value in registers is free
                _ => self.expr(base),
            },
            ExprKind::StructLit(_, vals) => { for v in vals { self.expr(v)?; } Ok(()) }
            // an element of a value array: a load, the bytes were charged when the value was
            // written (docs/arrays-by-value-design.md §3)
            // `xs[i].f[j]` in an array of holders: one load, a site on the field of element `i` (§9)
            ExprKind::FieldIndex(base, j, fi) if matches!(base.kind, ExprKind::Index(..)) => {
                let ExprKind::Index(arr, idx) = &base.kind else { unreachable!() };
                self.expr(idx)?;
                self.expr(j)?;
                self.add_work_n(1);
                self.access(*arr, idx, Some(*fi));
                Ok(())
            }
            ExprKind::FieldIndex(base, idx, _) => { self.expr(base)?; self.expr(idx)?; self.add_work_n(1); Ok(()) }
            // a value array written: one store per element and a sequential write, as `[a, b, c]`
            ExprKind::ArrayVal(vals) => {
                for v in vals { self.expr(v)?; }
                let k = vals.len() as i128;
                self.add_work_n(k);
                let es = e.ty.elem().map_or(8, |t| t.elem_bytes());
                self.stream(&Poly::constant(k), es);
                Ok(())
            }
            ExprKind::If(c, t, els) => {
                self.expr(c)?;
                self.add_work_n(1);
                // both branches are charged: an upper bound, tight when one is empty — except
                // for recursive call sites, which are counted along the heavier path only, or a
                // binary search would read as two calls per level and come out linear
                let before = self.rec_calls.len();
                let entry_init = self.initial.clone();
                let if_id = self.next_if;
                self.next_if += 1;
                self.branch.push((if_id, true));
                self.push_frame();
                self.block(t)?;
                let (wt, mt, spt, cht, callt, pgt, set, dvt, cct, wbt) = self.pop_frame();
                self.branch.pop();
                let then_calls: Vec<_> = self.rec_calls.drain(before..).collect();
                let then_init = std::mem::replace(&mut self.initial, entry_init);
                self.branch.push((if_id, false));
                self.push_frame();
                if let Some(b) = els { self.block(b)?; }
                let (we, me, spe, che, calle, pge, see, dve, cce, wbe) = self.pop_frame();
                self.branch.pop();
                // the two branches are alternatives: the cost is the larger, not the sum
                self.work = self.work.add(&wt.max(&we));
                self.moves = self.moves.add(&mt.max(&me));
                self.span = self.span.add(&spt.max(&spe));
                self.chase = self.chase.add(&cht.max(&che));
                self.paged = self.paged.add(&branch_max(&pgt, &pge));
                self.conc = self.conc.add(&branch_max(&cct, &cce));
                self.wback = self.wback.add(&branch_max(&wbt, &wbe));
                self.divs = self.divs.add(&branch_max(&dvt, &dve));
                self.serial = self.serial.add(&branch_max(&set, &see));
                // and the calls' moves: the larger where that is cheap to know, else both, which
                // bounds it too — a `max` of piecewise costs multiplies regimes under nested `if`s
                self.call_moves = self.call_moves.add(&branch_max(&callt, &calle));
                // a local defined differently on the two sides may hold either value after
                for (l, tv) in then_init {
                    let merged = match (tv, self.initial.get(&l).cloned().flatten()) {
                        (Some(mut a), Some(b)) => { for p in b { if !a.contains(&p) { a.push(p); } } Some(a) }
                        _ => None,
                    };
                    self.initial.insert(l, merged);
                }
                let else_n = self.rec_calls.len() - before;
                if then_calls.len() > else_n {
                    self.rec_calls.truncate(before);
                    self.rec_calls.extend(then_calls);
                }
                Ok(())
            }
            ExprKind::Block(b) => self.block(b),
            ExprKind::Call(fid, args) => {
                for a in args { self.expr(a)?; }
                // every argument is passed by value: a struct with array fields is a copy
                for a in args { self.copy_value(&a.ty); }
                // an array handed over writable may come back changed, where the callee may
                // store into it (`field_writes`)
                for (i, a) in args.iter().enumerate() {
                    let may = self.an.field_writes[*fid].get(i).is_none_or(|w| w.all || !w.fields.is_empty());
                    match &a.kind {
                        ExprKind::Ref(s, true) if may => self.wrote(*s),
                        ExprKind::Local(s) if may && self.f.locals[*s].ty.is_arrayish() && !matches!(self.f.locals[*s].ty, Ty::Slice(_, false, _)) => self.wrote(*s),
                        _ => {}
                    }
                }
                self.add_work_n(2); // call and return; arguments are register moves
                if self.scc.as_ref().is_some_and(|ms| ms.contains(fid)) { return Ok(()); }
                if Some(*fid) == self.self_fid {
                    if self.replay { return Ok(()); }
                    let cf = &self.an.m.funcs[*fid];
                    let sizes: Vec<Option<Poly>> = args.iter().zip(&cf.params).map(|(a, &p)| {
                        if cf.locals[p].ty.is_arrayish() {
                            match &a.kind { ExprKind::Ref(s, _) | ExprKind::Local(s) => self.local_size.get(s).cloned(), _ => None }
                        } else { self.size_of(a, Dir::Upper) }
                    }).collect();
                    let o = self.times_here();
                    self.rec_calls.push((sizes, o, e.line));
                    return Ok(());
                }
                // the callee's signature, and this call's argument sizes for its atoms
                let callee = self.an.func(*fid).clone();
                if !self.replay && callee.effects.contains(&"io") { self.io = true; }
                let cf = &self.an.m.funcs[*fid];
                // a declared callee is seen through its declaration only: its bounds, no footprint
                // and no residue — what a caller could know without the body
                let declared_only = callee.declared.work.is_some() && callee.declared.moves.is_some();
                // a declared callee has no span of its own (no `span_at_most` yet): its work
                // stands in, which is always a safe over-approximation (span ≤ work)
                if !self.replay && callee.effects.contains(&"unbounded") {
                    return Err(Fail::Unknown(format!("calls `{}`, which is declared unbounded", callee.name), e.line));
                }
                // an unknown callee does not make this function unknown (stage D): its cost is a
                // named term, `work[f](…)`, over the parameters that carry a size — an array's
                // length, an `i64`'s value — and the rest of this function stays exact modulo it
                let opaque = !declared_only && matches!(callee.result, CostResult::Unknown { .. })
                    && !self.self_fid.is_some_and(|me| self.an.mutual(me, *fid));
                let (mut w, mut mv, mut sp) = if declared_only {
                    let w = Cost::poly(callee.declared.work.clone().unwrap());
                    (w.clone(), Cost::poly(callee.declared.moves.clone().unwrap()), w)
                } else {
                    match &callee.result {
                        CostResult::Exact { work, moves, span } => (work.clone(), moves.clone(), span.clone()),
                        CostResult::Unknown { reason, .. } if !opaque => {
                            let reason = if reason.starts_with("calls `") { reason.clone() }
                                else { format!("calls `{}`, whose cost is unknown ({reason})", callee.name) };
                            return Err(Fail::Unknown(reason, e.line));
                        }
                        CostResult::Unknown { .. } => {
                            let args: Vec<Option<Poly>> = cf.params.iter().enumerate()
                                .map(|(i, &p)| if cf.locals[p].ty.is_arrayish() || cf.locals[p].ty == Ty::I64 { Some(Poly::var(i)) } else { None })
                                .collect();
                            let term = |moves: bool| Cost::poly(Poly::atom(Atom::Opaque(Box::new(Opaque { callee: callee.name.clone(), moves, args: args.clone() }))));
                            (term(false), term(true), term(false))
                        }
                    }
                };
                // provenance: a declared callee is an assumption this line now rests on
                if !self.replay {
                    if declared_only || opaque {
                        let how = if opaque { "unknown" } else if cf.body.is_none() { "declared, extern" } else { "declared, checked" };
                        let tag = format!("{} ({how})", callee.name);
                        if !self.rests_on.contains(&tag) { self.rests_on.push(tag); }
                    }
                    for r in &callee.rests_on { if !self.rests_on.contains(r) { self.rests_on.push(r.clone()); } }
                    // a callee bounded by a scan of its own (§ A scan)
                    if callee.notes.iter().any(|n| n.starts_with("scan: ") || n.starts_with("worklist: ") || n.starts_with("tree: ")) {
                        let tag = format!("{} {SCAN_TAG}", callee.name);
                        if !self.rests_on.contains(&tag) { self.rests_on.push(tag); }
                    }
                    // a callee charged a bound where a fit test in a loop's variable changes
                    if callee.notes.iter().any(|n| n.starts_with("regime: ")) {
                        let tag = format!("{} {REGIME_TAG}", callee.name);
                        if !self.rests_on.contains(&tag) { self.rests_on.push(tag); }
                    }
                }
                // an amortised call (§ An amortised scan): a lap pays what the callee charges
                // besides the distance it moves, and the loop pays the distance once
                let depth = self.loops.len();
                let mut amort_alpha: Option<(usize, Cost, Cost, Poly)> = None;
                // the serial work and divisions of an amortised call, the same way: a lap pays
                // what is not distance, the loop the distance once
                let (mut c_serial, mut c_divs) = (callee.serial.clone(), callee.divs.clone());
                let mut amort_rates: Option<(Cost, Cost)> = None;
                if let Some((d, cands)) = self.amort.last() {
                    if *d == depth {
                        if let Some((ci, c)) = cands.iter().enumerate().find(|(_, c)| c.line == e.line && c.fid == *fid) {
                            // each column on its own: one whose distance term is not of the form
                            // stays charged a lap at a time, which is sound
                            let (aw, asp, am) = (distance_rate(&w, c.adv), distance_rate(&sp, c.adv), distance_rate(&mv, c.adv));
                            let aw = match (aw, asp) { (Some(a), Some(b)) => { w = strip_distance(&w, c.adv); sp = strip_distance(&sp, c.adv); Some(a.max(&b)) } _ => None };
                            let am = am.map(|a| { mv = strip_distance(&mv, c.adv); a });
                            // the partly used lines at the two ends of a call's stretch: the next
                            // call starts where this one stopped, so the chain shares them and the
                            // loop pays them once — where the callee touches the scanned array
                            // alone and has none of its own
                            let mut edge = Poly::zero();
                            if am.is_some() && !callee.internal && callee.footprint.iter().all(|f| f.param == c.adv.a) && mv.pieces.len() == 1 {
                                let b1 = Poly::atom(Atom::B);
                                let q = &mv.pieces[0].poly;
                                let k = q.terms.iter().find(|(m, k)| Poly { terms: [((*m).clone(), Rat::one())].into_iter().collect() } == b1 && k.n > 0).map(|(_, k)| *k);
                                if let Some(k) = k {
                                    edge = b1.scale(k);
                                    mv = mv.map(|q| q.sub(&edge));
                                }
                            }
                            let has_w = aw.is_some();
                            if aw.is_some() || am.is_some() { amort_alpha = Some((ci, aw.unwrap_or_default(), am.unwrap_or_default(), edge)); }
                            if has_w {
                                let rs = distance_rate(&c_serial, c.adv);
                                let rd = distance_rate(&c_divs, c.adv);
                                if rs.is_some() { c_serial = strip_distance(&c_serial, c.adv); }
                                if rd.is_some() { c_divs = strip_distance(&c_divs, c.adv); }
                                amort_rates = Some((rs.unwrap_or_default(), rd.unwrap_or_default()));
                            }
                        }
                    }
                }
                let mut map: Vec<(usize, Poly)> = Vec::new();
                let mut roots: Vec<Option<LocalId>> = Vec::new();
                let mut unnamed: Vec<(usize, u32)> = Vec::new();
                for (i, (a, &p)) in args.iter().zip(&cf.params).enumerate() {
                    let (by, root) = if cf.locals[p].ty.is_arrayish() {
                        match &a.kind {
                            ExprKind::Ref(s, _) | ExprKind::Local(s) => (self.local_size.get(s).cloned(), Some(self.local_root.get(s).copied().unwrap_or(*s))),
                            _ => (None, None),
                        }
                    } else { (self.size_of(a, Dir::Upper), None) };
                    roots.push(root);
                    // an `i64` the call cannot name, where the callee's cost only falls as it grows:
                        // at its least it is an upper bound (docs/cost-model.md § A scan)
                        let by = by.or_else(|| {
                            if cf.locals[p].ty != Ty::I64 || ![&w, &mv, &sp].iter().any(|c| c.mentions(i)) || ![&w, &mv, &sp].iter().all(|c| falls_in(c, i)) { return None; }
                            let lb = self.least_of(a)?;
                            self.scans.borrow_mut().push(format!("scan: argument {} to `{}` is taken at its least, {}, and `{}`'s cost falls as it grows",
                                i + 1, callee.name, lb.display(&self.names), callee.name));
                            Some(lb)
                        });
                    match by {
                        Some(b) => map.push((i, b)),
                        None => unnamed.push((i, a.line)),
                    }
                }
                // an argument this call cannot name is `_` wherever it is only an unknown
                // callee's argument; anywhere else the cost depends on it and is unknown here
                if !unnamed.is_empty() {
                    let hide = |p: &Poly| unnamed.iter().any(|(i, _)| p.mentions(*i));
                    w = w.hide_args(&hide);
                    mv = mv.hide_args(&hide);
                    sp = sp.hide_args(&hide);
                }
                for &(i, line) in &unnamed {
                    // a footprint whose ends depend on it is not dropped but made inexact below:
                    // then nothing is credited from it and nothing claimed resident after
                    let used = w.mentions(i) || mv.mentions(i) || sp.mentions(i) || (!opaque && callee.footprint.iter().any(|f| f.param == i));
                    if used {
                        return Err(Fail::Unknown(
                            format!("argument {} to `{}` is not a size expression, and `{}`'s cost depends on it", i + 1, callee.name, callee.name),
                            line,
                        ));
                    }
                }
                // an atom of the callee's that is none of its parameters — the length of what an
                // extern returned, program input, reached here directly or through a callee that
                // read it — is a new quantity at every call: this function gets an atom of its
                // own for it, free the way a parameter's length is. Once per call, so not in a
                // loop, where each lap would read an input of its own length: there `arg(k)` is
                // at most the longest argument, `max(arg[_].len())`, a bound, and anything else
                // is unknown. `arg_count()` is the same number in every function and every lap,
                // so the callee's is this function's own
                self.last_result_atom = None;
                for k in cf.params.len()..callee.names.len() {
                    let in_result = callee.result_size.as_ref().is_some_and(|p| p.mentions(k));
                    if !(w.mentions(k) || mv.mentions(k) || sp.mentions(k) || in_result) { continue; }
                    if let (true, Some(a)) = (callee.names[k] == "arg_count()", self.argc_atom) {
                        map.push((k, Poly::var(a)));
                        continue;
                    }
                    if !self.loops.is_empty() {
                        if crate::input::is_builtin(cf) && cf.name == "arg" && callee.result_size == Some(Poly::var(k)) {
                            map.push((k, longest_arg()));
                            continue;
                        }
                        // an atom the callee bound once (§ A size bound once) that only indexes an
                        // element it reads, or is an unknown callee's argument, is widened as a
                        // variable summed away is: `max(xs[_])`, `_`. Used as a size it is refused
                        let hide = |p: &Poly| p.mentions(k);
                        let (w2, mv2, sp2) = (w.hide_args(&hide), mv.hide_args(&hide), sp.hide_args(&hide));
                        let in_feet = callee.footprint.iter().any(|f| f.lo.mentions(k) || f.hi.mentions(k));
                        if !in_result && !in_feet && !(w2.mentions(k) || mv2.mentions(k) || sp2.mentions(k)) {
                            (w, mv, sp) = (w2, mv2, sp2);
                            continue;
                        }
                        if !callee.names[k].ends_with(".len()") {
                            return Err(Fail::Unknown(format!("calls `{}` in a loop: `{}`, a size it binds once, is a new value at every iteration", callee.name, callee.names[k]), e.line));
                        }
                        return Err(Fail::Unknown(format!("calls `{}` in a loop: what it reads is a size of its own at every iteration, `{}`", callee.name, callee.names[k]), e.line));
                    }
                    let a = self.new_atom(&format!("{}.{}", callee.name, callee.names[k]));
                    if callee.result_size == Some(Poly::var(k)) { self.last_result_atom = Some(a); }
                    map.push((k, Poly::var(a)));
                }
                // the callee's reads are of the arrays this call passed it
                let rename = |r: &Root| -> Root {
                    match r {
                        Root::Param(i, n) => match roots.get(*i).copied().flatten() {
                            Some(rt) => {
                                let name = self.f.locals[rt].name.clone();
                                match self.f.params.iter().position(|&p| p == rt) { Some(j) => Root::Param(j, name), None => Root::Local(name) }
                            }
                            None => Root::Local(format!("{}.{n}", callee.name)),
                        },
                        Root::Local(n) if n == LONGEST_ARG => r.clone(),
                        Root::Local(n) if !n.contains('.') => Root::Local(format!("{}.{n}", callee.name)),
                        other => other.clone(),
                    }
                };
                // a size the callee reads from an array it is handed is the value at the call; in a
                // loop that writes that field or slot, each lap's call reads a value of its own, and
                // one atom does not stand for them all
                if !self.loop_writes.is_empty() {
                    // the array of the caller's a callee's read looks into, when a loop around the
                    // call writes what it reads
                    let stale = |r: &Read| -> Option<LocalId> {
                        let Root::Param(i, _) = &r.root else { return None };
                        let rt = roots.get(*i).copied().flatten()?;
                        let fi = r.field.as_ref().and_then(|fname| match self.f.locals[rt].ty.elem() {
                            Some(Ty::Struct(sid)) => self.an.m.structs[*sid].fields.iter().position(|(n, _)| n == fname),
                            _ => None,
                        });
                        let slot = match (&r.field, &r.index) { (None, Some(ix)) => ix.as_const().filter(|c| c.is_int() && c.n >= 0).map(|c| IDX + c.n as usize), _ => None };
                        self.loop_writes.iter().any(|lw| lw.get(&rt).is_some_and(|fw| fw.all || match (fi, slot) {
                            (Some(f), _) | (None, Some(f)) => fw.fields.contains(&f),
                            (None, None) => !fw.fields.is_empty(),
                        })).then_some(rt)
                    };
                    // only an unknown callee's argument, it is `_` there, as an argument the call
                    // cannot name is; anywhere else the call is unknown
                    let hide = |p: &Poly| { let mut rs = Vec::new(); p.reads(&mut rs); rs.iter().any(|r| stale(r).is_some()) };
                    w = w.hide_args(&hide);
                    mv = mv.hide_args(&hide);
                    sp = sp.hide_args(&hide);
                    let mut rs = Vec::new();
                    for c in [&w, &mv, &sp] { for pc in &c.pieces { pc.poly.reads(&mut rs); } }
                    if let Some(rt) = rs.iter().find_map(|r| stale(r)) {
                        return Err(Fail::Unknown(format!("calls `{}` in a loop that writes `{}`, which its cost reads as a size", callee.name, self.f.locals[rt].name), e.line));
                    }
                }
                w = w.rename_roots(&rename).subst_many(&map);
                mv = mv.rename_roots(&rename).subst_many(&map);
                sp = sp.rename_roots(&rename).subst_many(&map);
                // the rate per unit of distance, in this function's sizes as the rest of the cost is
                if let Some((ci, aw, am, edge)) = amort_alpha {
                    let (aw, am) = (aw.rename_roots(&rename).subst_many(&map), am.rename_roots(&rename).subst_many(&map));
                    let rates = amort_rates.take().map(|(a, b)| (a.rename_roots(&rename).subst_many(&map), b.rename_roots(&rename).subst_many(&map))).unwrap_or_default();
                    if !self.replay { if let Some((_, cands)) = self.amort.last_mut() { cands[ci].alpha = Some((aw, am)); cands[ci].edge = edge; cands[ci].rates = rates; } }
                }
                self.last_result = callee.result_size.as_ref().map(|p| p.subst_many(&map));
                // the callee's footprint in this function's arrays (none is known of a declared callee)
                let feet: Vec<(LocalId, Poly, Poly, bool)> = callee.footprint.iter().filter(|_| !declared_only && !opaque).filter_map(|f| {
                    let root = roots.get(f.param).copied().flatten()?;
                    let named = !unnamed.iter().any(|(i, _)| f.lo.mentions(*i) || f.hi.mentions(*i));
                    let (lo, hi) = (f.lo.subst_many(&map), f.hi.subst_many(&map));
                    let loose = lo.has_loose_read() || hi.has_loose_read();
                    Some((root, lo, hi, f.exact && named && !loose))
                }).collect();
                let dbg = std::env::var("NEANT_DEBUG_CALLS").is_ok() && !self.replay;
                if dbg {
                    eprintln!("call {} -> {} @{}", self.f.name, callee.name, e.line);
                    eprintln!("   mv    {}", mv.display(&self.names));
                    for (r, lo, hi, ex) in &feet {
                        eprintln!("   foot  {} [{}, {}) exact={ex}", self.f.locals[*r].name,
                            lo.display(&self.names), hi.display(&self.names));
                    }
                    for r in &self.resident {
                        eprintln!("   resid {} [{}, {}) conds={}", self.f.locals[r.root].name,
                            r.lo.display(&self.names), r.hi.display(&self.names), r.conds.len());
                    }
                }
                // a written line goes back when it leaves the cache: a call whose lines were already
                // resident here takes none back (cost-model § Time, streams)
                let mut credited = false;
                // credit: what the callee reads that is already resident here pays nothing
                for (root, lo, hi, exact) in &feet {
                    if !exact { continue; }
                    for r in &self.resident {
                        if r.root != *root { continue; }
                        // the callee's range within the resident range, or the reverse
                        let overlap = if super::piece::dominates(&lo, &r.lo) && super::piece::dominates(&r.hi, &hi) { hi.sub(lo) }
                            else if super::piece::dominates(&r.lo, &lo) && super::piece::dominates(&hi, &r.hi) { r.hi.sub(&r.lo) }
                            else { continue };
                        mv = mv.add_under(&r.conds, &overlap.scale(Rat::int(-1)));
                        credited = true;
                    }
                }
                // everything the callee touches is resident here, and it has no array of its own:
                // the call moves nothing where those residues hold (a loop of calls on a small array)
                // — unless it calls an unknown callee, which may have arrays of its own. Its stores
                // then stay in the cache too, and go back to memory once, not a call at a time
                let mut resident_call = credited;
                if !declared_only && !opaque && !callee.internal && !feet.is_empty() && !mv.has_opaque() {
                    let mut conds: Vec<Cond> = Vec::new();
                    let all = feet.iter().all(|(root, lo, hi, exact)| *exact && self.resident.iter().any(|r| {
                        let ok = r.root == *root && super::piece::dominates(lo, &r.lo) && super::piece::dominates(&r.hi, hi);
                        if ok { for c in &r.conds { if !conds.contains(c) { conds.push(c.clone()); } } }
                        ok
                    }));
                    if all {
                        resident_call = true;
                        let mut out = Vec::new();
                        for p in &mv.pieces {
                            // a credit already at or below zero is the cross product cancelling an
                            // earlier call's charge (`dot(&xs, &xs)`): it stands; only what is left
                            // above zero is what a resident footprint does not move
                            let keep = self.numeric(&p.poly).is_some_and(|v| v <= 0.0);
                            let mut cs = p.conds.clone(); cs.extend(conds.iter().cloned());
                            if super::piece::feasible(&cs) { out.push(Piece { conds: cs, poly: if keep { p.poly.clone() } else { Poly::zero() } }); }
                            for c in &conds {
                                let mut cn = p.conds.clone(); cn.push(Cond { ws: c.ws.clone(), fits: !c.fits });
                                if super::piece::feasible(&cn) { out.push(Piece { conds: cn, poly: p.poly.clone() }); }
                            }
                            if conds.is_empty() { out.clear(); out.push(Piece { conds: p.conds.clone(), poly: Poly::zero() }); }
                        }
                        let mut c = Cost { pieces: out };
                        c.prune();
                        mv = c;
                    }
                }
                // a bound on a part is a bound on the whole, once: repeated calls are not
                // multiplied, since a bound on any schedule of the part says nothing about what
                // the repetitions may share. A cold-start bound travels only for arrays the caller
                // itself received, which were in slow memory when the caller started.
                if !self.replay {
                    let all_received = args.iter().all(|a| match &a.kind {
                        ExprKind::Ref(s, _) | ExprKind::Local(s) if self.f.locals[*s].ty.is_arrayish() => self.f.params.contains(self.local_root.get(s).unwrap_or(s)),
                        _ => true,
                    });
                    for bd in &callee.bounds {
                        if bd.cold && !all_received { continue; }
                        // a lower bound in an argument this call cannot name has no value here
                        if unnamed.iter().any(|(i, _)| bd.moves.mentions(*i)) { continue; }
                        self.bounds.push(Bound { moves: bd.moves.subst_many(&map), ..bd.clone() });
                    }
                    for n in &callee.notes {
                        let n2 = format!("in `{}`: {n}", callee.name);
                        if !self.notes.contains(&n2) { self.notes.push(n2); }
                    }
                }
                if dbg { eprintln!("   net   {}", mv.display(&self.names)); }
                // after the call: what it leaves resident replaces what was
                self.resident.clear();
                let hide = |p: &Poly| unnamed.iter().any(|(i, _)| p.mentions(*i));
                let resident = callee.resident.as_ref().map(|c| c.ws.hide_args(&hide)).filter(|ws| !hide(ws));
                if let (Some(ws), false) = (resident, declared_only || opaque) {
                    let cond = Cond { ws: ws.subst_many(&map), fits: true };
                    for (root, lo, hi, exact) in feet { if exact { self.resident.push(Res { root, lo, hi, conds: vec![cond.clone()] }); } }
                } else if dbg {
                    eprintln!("   leaves nothing resident (callee.resident={})", callee.resident.is_some());
                }
                if !self.replay {
                    self.work = self.work.add(&w);
                    self.span = self.span.add(&sp);
                    // the callee's chase, in this call's sizes; an argument this call cannot name
                    // drops it, as a time estimate may
                    if !declared_only && !opaque && !callee.chase.pieces.is_empty() {
                        let hide = |p: &Poly| unnamed.iter().any(|(i, _)| p.mentions(*i));
                        let c = callee.chase.hide_args(&hide);
                        if !c.pieces.iter().any(|pc| hide(&pc.poly)) { self.chase = self.chase.add(&c.rename_roots(&rename).subst_many(&map)); }
                    }
                    if !declared_only && !opaque && !c_serial.pieces.is_empty() {
                        let hide = |p: &Poly| unnamed.iter().any(|(i, _)| p.mentions(*i));
                        let c = c_serial.hide_args(&hide);
                        if !c.pieces.iter().any(|pc| hide(&pc.poly)) { self.serial = self.serial.add(&c.rename_roots(&rename).subst_many(&map)); }
                    }
                    // the callee's loops, once for every time this call runs
                    if !declared_only && !opaque {
                        let here = self.times_here();
                        let hide = |p: &Poly| unnamed.iter().any(|(i, _)| p.mentions(*i));
                        if callee.laps_dropped { self.laps_dropped = true; }
                        for (line, e, c, v, b) in &callee.laps {
                            if hide(c) || hide(e) { self.laps_dropped = true; continue; }
                            let sub = |p: &Poly| p.rename_roots(&rename).subst_many(&map).mul(&here);
                            self.laps.push((*line, sub(e), sub(c), *v, *b));
                        }
                    }
                    // a square root runs on the divider: counted as a division (cost-model § Time, divisions)
                    if cf.body.is_none() && cf.name == "sqrt" { self.divs = self.divs.add_poly(&Poly::constant(1)); }
                    if !declared_only && !opaque && !c_divs.pieces.is_empty() {
                        let hide = |p: &Poly| unnamed.iter().any(|(i, _)| p.mentions(*i));
                        let c = c_divs.hide_args(&hide);
                        if !c.pieces.iter().any(|pc| hide(&pc.poly)) { self.divs = self.divs.add(&c.rename_roots(&rename).subst_many(&map)); }
                    }
                    // the callee's translations, as its divisions are
                    if !declared_only && !opaque && !callee.tlb.pieces.is_empty() {
                        let hide = |p: &Poly| unnamed.iter().any(|(i, _)| p.mentions(*i));
                        let c = callee.tlb.hide_args(&hide);
                        if !c.pieces.iter().any(|pc| hide(&pc.poly) || pc.conds.iter().any(|cd| hide(&cd.ws))) { self.tlb = self.tlb.add(&c.rename_roots(&rename).subst_many(&map)); }
                    }
                    if !declared_only && !opaque && !callee.paged.pieces.is_empty() {
                        let hide = |p: &Poly| unnamed.iter().any(|(i, _)| p.mentions(*i));
                        let c = callee.paged.hide_args(&hide);
                        if !c.pieces.iter().any(|pc| hide(&pc.poly)) { self.paged = self.paged.add(&c.rename_roots(&rename).subst_many(&map)); }
                    }
                    for (col, own) in [(&callee.conc, 0), (&callee.wback, 1)] {
                        if declared_only || opaque || resident_call || col.pieces.is_empty() { continue; }
                        let hide = |p: &Poly| unnamed.iter().any(|(i, _)| p.mentions(*i));
                        let c = col.hide_args(&hide);
                        if c.pieces.iter().any(|pc| hide(&pc.poly)) { continue; }
                        let c = c.rename_roots(&rename).subst_many(&map);
                        if own == 0 { self.conc = self.conc.add(&c); } else { self.wback = self.wback.add(&c); }
                    }
                }
                self.call_moves = self.call_moves.add(&mv);
                if let Some(h) = self.has_call.last_mut() { *h = true; }
                Ok(())
            }
        }
    }
}

/// The root of `max(arg[_].len())`: the arguments, as if an array of them, which every function
/// shares, so a caller does not rename it to the callee's own.
const LONGEST_ARG: &str = "arg";

/// The longest argument's length, `max(arg[_].len())`: what `arg(k)` returns at most when a loop
/// reads one per lap. A read atom with no element, so the line that has it is `bound`.
fn longest_arg() -> Poly {
    Poly::atom(Atom::Read(Box::new(Read { id: 0, root: Root::Local(LONGEST_ARG.into()), index: None, field: Some("len()".into()), least: false, walk: false })))
}

/// The constant `var` is stepped by in `body`, when it is assigned exactly once there as
/// `var += c`, `var -= c`, `var = var + c` or `var = var - c`. Nested loops count as assignments
/// too many times.
fn single_step(body: &Block, var: LocalId) -> Option<i64> {
    let mut found: Option<i64> = None;
    let mut count = 0;
    fn walk(b: &Block, var: LocalId, found: &mut Option<i64>, count: &mut usize, nested: bool) {
        let tail_stmt = b.tail.as_ref().map(|t| Stmt::Expr((**t).clone()));
        for s in b.stmts.iter().chain(tail_stmt.iter()) {
            match s {
                Stmt::Assign(LValue::Var(v), op, e) if *v == var => {
                    *count += if nested { 2 } else { 1 };
                    let step = match (op, &e.kind) {
                        (Some(BinOp::Add), ExprKind::Int(c)) => Some(*c),
                        (Some(BinOp::Sub), ExprKind::Int(c)) => Some(-*c),
                        (None, ExprKind::Binary(BinOp::Add, a, c)) if matches!(a.kind, ExprKind::Local(l) if l == var) => if let ExprKind::Int(c) = c.kind { Some(c) } else { None },
                        (None, ExprKind::Binary(BinOp::Sub, a, c)) if matches!(a.kind, ExprKind::Local(l) if l == var) => if let ExprKind::Int(c) = c.kind { Some(-c) } else { None },
                        _ => None,
                    };
                    *found = step;
                }
                Stmt::For { body, .. } | Stmt::While { body, .. } => walk(body, var, found, count, true),
                Stmt::Expr(Expr { kind: ExprKind::If(_, t, e), .. }) => {
                    walk(t, var, found, count, nested);
                    if let Some(e) = e { walk(e, var, found, count, nested); }
                }
                _ => {}
            }
        }
    }
    walk(body, var, &mut found, &mut count, false);
    if count == 1 { found } else { None }
}

fn assigns(body: &Block, mut pred: impl FnMut(LocalId) -> bool) -> bool {
    fn walk(b: &Block, pred: &mut dyn FnMut(LocalId) -> bool) -> bool {
        let tail_stmt = b.tail.as_ref().map(|t| Stmt::Expr((**t).clone()));
        b.stmts.iter().chain(tail_stmt.iter()).any(|s| match s {
            Stmt::Assign(LValue::Var(v), _, _) => pred(*v),
            Stmt::For { body, .. } | Stmt::While { body, .. } => walk(body, pred),
            Stmt::Expr(Expr { kind: ExprKind::If(_, t, e), .. }) => walk(t, pred) || e.as_ref().is_some_and(|e| walk(e, pred)),
            _ => false,
        })
    }
    walk(body, &mut pred)
}

fn bound_mentions(e: &Expr, l: LocalId) -> bool {
    match &e.kind {
        ExprKind::Local(v) => *v == l,
        ExprKind::Len(v) => *v == l,
        ExprKind::Binary(_, a, b) | ExprKind::MinMax(_, a, b) | ExprKind::FieldIndex(a, b, _) | ExprKind::InRow(a, b) => bound_mentions(a, l) || bound_mentions(b, l),
        ExprKind::Unary(_, a) | ExprKind::Cast(a, _) | ExprKind::Field(a, _) => bound_mentions(a, l),
        ExprKind::Index(_, i) => bound_mentions(i, l),
        _ => false,
    }
}

/// Every `x[i]` in an expression tree, reads only (the write is the statement's left side).
fn collect_refs<'e>(e: &'e Expr, out: &mut Vec<(LocalId, &'e Expr, Option<usize>)>) {
    match &e.kind {
        ExprKind::Index(a, i) => { out.push((*a, i, None)); collect_refs(i, out); }
        ExprKind::Field(base, fi) => match &base.kind {
            ExprKind::Index(a, i) => { out.push((*a, i, Some(*fi))); collect_refs(i, out); }
            _ => collect_refs(base, out),
        },
        ExprKind::FieldIndex(base, j, fi) if matches!(base.kind, ExprKind::Index(..)) => {
            let ExprKind::Index(a, i) = &base.kind else { unreachable!() };
            out.push((*a, i, Some(*fi)));
            collect_refs(i, out);
            collect_refs(j, out);
        }
        ExprKind::StructLit(_, vals) | ExprKind::ArrayVal(vals) => for v in vals { collect_refs(v, out); },
        ExprKind::Binary(_, a, b) | ExprKind::MinMax(_, a, b) | ExprKind::FieldIndex(a, b, _) | ExprKind::InRow(a, b) => { collect_refs(a, out); collect_refs(b, out); }
        ExprKind::Unary(_, a) | ExprKind::Cast(a, _) | ExprKind::Println(a) => collect_refs(a, out),
        ExprKind::Call(_, args) => for a in args { collect_refs(a, out); },
        ExprKind::If(c, t, els) => {
            collect_refs(c, out);
            for b in std::iter::once(t).chain(els.iter()) { for st in &b.stmts { if let Stmt::Expr(x) | Stmt::Let(_, x) = st { collect_refs(x, out); } } if let Some(x) = &b.tail { collect_refs(x, out); } }
        }
        ExprKind::Block(b) => { for st in &b.stmts { if let Stmt::Expr(x) | Stmt::Let(_, x) = st { collect_refs(x, out); } } if let Some(x) = &b.tail { collect_refs(x, out); } }
        _ => {}
    }
}

/// `q ≥ p` for all sizes ≥ 1, by the piecewise machinery's dominance.
fn bounds_dominates(q: &Poly, p: &Poly) -> bool { super::piece::dominates(q, p) }

/// A canonical form of an index expression, free of line numbers, so that two occurrences of the
/// same index in one loop are recognised as one address. `None` for anything this does not model,
/// which is never merged with anything.
fn idx_key(e: &Expr) -> Option<String> {
    Some(match &e.kind {
        ExprKind::Int(v) => format!("{v}"),
        ExprKind::Local(l) => format!("v{l}"),
        ExprKind::Len(l) => format!("len{l}"),
        ExprKind::Binary(op, a, b) => format!("({} {} {})", idx_key(a)?, op.c_str(), idx_key(b)?),
        ExprKind::Unary(op, a) => format!("({op:?} {})", idx_key(a)?),
        ExprKind::Cast(a, t) => format!("({} as {t})", idx_key(a)?),
        ExprKind::MinMax(m, a, b) => format!("({} {m} {})", idx_key(a)?, idx_key(b)?),
        ExprKind::InRow(j, _) => idx_key(j)?,
        ExprKind::Index(arr, i) => format!("a{arr}[{}]", idx_key(i)?),
        ExprKind::Field(b, f) => format!("{}.{f}", idx_key(b)?),
        _ => return None,
    })
}

/// Every scalar local read in an expression tree, loop variables included (they have no alias).
fn collect_scalars(e: &Expr, out: &mut Vec<LocalId>) {
    match &e.kind {
        ExprKind::Local(v) => out.push(*v),
        ExprKind::Index(_, i) => collect_scalars(i, out),
        ExprKind::Binary(_, a, b) | ExprKind::MinMax(_, a, b) | ExprKind::FieldIndex(a, b, _) | ExprKind::InRow(a, b) => { collect_scalars(a, out); collect_scalars(b, out); }
        ExprKind::Unary(_, a) | ExprKind::Cast(a, _) | ExprKind::Println(a) => collect_scalars(a, out),
        ExprKind::Call(_, args) => for a in args { collect_scalars(a, out); },
        ExprKind::Field(base, _) => collect_scalars(base, out),
        ExprKind::StructLit(_, vals) | ExprKind::ArrayVal(vals) => for v in vals { collect_scalars(v, out); },
        ExprKind::If(c, t, els) => { collect_scalars(c, out); for b in std::iter::once(t).chain(els.iter()) { if let Some(x) = &b.tail { collect_scalars(x, out); } } }
        ExprKind::Block(b) => { if let Some(x) = &b.tail { collect_scalars(x, out); } }
        _ => {}
    }
}


/// Every function a block calls directly.
pub(super) fn callees_block_pub(b: &Block, out: &mut Vec<FuncId>) { callees_block(b, out) }

fn callees_block(b: &Block, out: &mut Vec<FuncId>) {
    for st in &b.stmts {
        match st {
            Stmt::Let(_, e) | Stmt::Expr(e) | Stmt::Return(Some(e)) => callees_expr(e, out),
            Stmt::LetRepeat(_, a, n) => { callees_expr(a, out); callees_expr(n, out); }
            Stmt::LetArray(_, es) => for e in es { callees_expr(e, out); },
            Stmt::LetBuild { len, body, .. } => { callees_expr(len, out); callees_block(body, out); }
            Stmt::Assign(lv, _, e) => {
                match lv { LValue::Index(_, i, _) | LValue::IndexField(_, i, _, _) | LValue::FieldIndex(_, i, _, _) => callees_expr(i, out), LValue::IndexFieldIndex(_, i, _, j, _) => { callees_expr(i, out); callees_expr(j, out); } _ => {} }
                callees_expr(e, out);
            }
            Stmt::For { start, end, body, .. } => { callees_expr(start, out); callees_expr(end, out); callees_block(body, out); }
            Stmt::ParFor { end, body, .. } => { callees_expr(end, out); callees_block(body, out); }
            Stmt::While { cond, decreasing, body, .. } => {
                callees_expr(cond, out);
                if let Some(d) = decreasing { callees_expr(d, out); }
                callees_block(body, out);
            }
            Stmt::Reassign(_) | Stmt::Break | Stmt::Return(None) => {}
        }
    }
    if let Some(t) = &b.tail { callees_expr(t, out); }
}

fn callees_expr(e: &Expr, out: &mut Vec<FuncId>) {
    match &e.kind {
        ExprKind::Call(f, args) => { if !out.contains(f) { out.push(*f); } for a in args { callees_expr(a, out); } }
        ExprKind::Binary(_, a, b) | ExprKind::MinMax(_, a, b) | ExprKind::FieldIndex(a, b, _) | ExprKind::InRow(a, b) => { callees_expr(a, out); callees_expr(b, out); }
        ExprKind::Unary(_, a) | ExprKind::Field(a, _) | ExprKind::Println(a) | ExprKind::Cast(a, _) | ExprKind::Index(_, a) => callees_expr(a, out),
        ExprKind::StructLit(_, es) | ExprKind::ArrayVal(es) => for x in es { callees_expr(x, out); },
        ExprKind::If(c, t, els) => { callees_expr(c, out); callees_block(t, out); if let Some(b) = els { callees_block(b, out); } }
        ExprKind::Block(b) => callees_block(b, out),
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Byte(_) | ExprKind::Local(_) | ExprKind::Len(_) | ExprKind::Ref(..) | ExprKind::Text(..) => {}
    }
}

/// The locals a block assigns. What it stores into arrays is `stores_block`'s.
#[derive(Default)]
struct Writes {
    locals: Vec<LocalId>,
}

impl Writes {
    fn block(&mut self, b: &Block) {
        for st in &b.stmts {
            match st {
                Stmt::Let(_, e) | Stmt::Expr(e) | Stmt::Return(Some(e)) => self.expr(e),
                Stmt::LetRepeat(_, a, n) => { self.expr(a); self.expr(n); }
                Stmt::LetArray(_, es) => for e in es { self.expr(e); },
                Stmt::LetBuild { len, body, .. } => { self.expr(len); self.block(body); }
                Stmt::Assign(lv, _, e) => {
                    match lv {
                        LValue::Var(v) | LValue::Field(v, _) => self.locals.push(*v),
                        LValue::Index(_, i, _) | LValue::IndexField(_, i, _, _) | LValue::FieldIndex(_, i, _, _) => self.expr(i),
                        LValue::IndexFieldIndex(_, i, _, j, _) => { self.expr(i); self.expr(j); }
                    }
                    self.expr(e);
                }
                Stmt::For { var, start, end, body } => { self.locals.push(*var); self.expr(start); self.expr(end); self.block(body); }
                Stmt::ParFor { var, end, body, acc, .. } => { self.locals.push(*var); self.locals.push(*acc); self.expr(end); self.block(body); }
                Stmt::While { cond, decreasing, body, .. } => {
                    self.expr(cond);
                    if let Some(d) = decreasing { self.expr(d); }
                    self.block(body);
                }
                // `ys = xs`: a new buffer or `xs`'s, either way not what a read of `ys` saw
                Stmt::Reassign(_) | Stmt::Break | Stmt::Return(None) => {}
            }
        }
        if let Some(t) = &b.tail { self.expr(t); }
    }
    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Call(_, args) => for a in args { self.expr(a); },
            ExprKind::Binary(_, a, b) | ExprKind::MinMax(_, a, b) | ExprKind::FieldIndex(a, b, _) | ExprKind::InRow(a, b) => { self.expr(a); self.expr(b); }
            ExprKind::Unary(_, a) | ExprKind::Field(a, _) | ExprKind::Println(a) | ExprKind::Cast(a, _) | ExprKind::Index(_, a) => self.expr(a),
            ExprKind::StructLit(_, es) | ExprKind::ArrayVal(es) => for x in es { self.expr(x); },
            ExprKind::If(c, t, els) => { self.expr(c); self.block(t); if let Some(b) = els { self.block(b); } }
            ExprKind::Block(b) => self.block(b),
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Byte(_) | ExprKind::Local(_) | ExprKind::Len(_) | ExprKind::Ref(..) | ExprKind::Text(..) => {}
        }
    }
}

/// The fields of an array a function may write: some by position, or every one — a store to a
/// whole element, `ys = xs`, or an `extern` handed it writable.
#[derive(Debug, Clone, Default, PartialEq)]
struct FieldWrites {
    all: bool,
    fields: std::collections::BTreeSet<usize>,
}

/// A store to `a[k]`, `k` a literal and `a` an array of scalars, is recorded as the field `IDX + k`:
/// a register file of constant slots (`wst[10]`) is written slot by slot, and a read of one slot is a
/// size in a loop that writes only others (cost-model § A size read from memory, by slot).
const IDX: usize = 1 << 40;

impl FieldWrites {
    fn add(&mut self, field: Option<usize>) {
        match field { Some(fi) => { self.fields.insert(fi); } None => self.all = true }
    }
}

/// Every function's `FieldWrites` per parameter, to a fixed point over the call graph: a
/// function writes what it stores into and what the callees it hands the array to write.
fn field_writes(m: &Module) -> Vec<Vec<FieldWrites>> {
    let mut summ: Vec<Vec<FieldWrites>> = m.funcs.iter().map(|f| {
        f.params.iter().map(|&p| {
            let writable = matches!(f.locals[p].ty, Ty::Slice(_, true, _) | Ty::Array(..));
            FieldWrites { all: f.body.is_none() && writable, fields: Default::default() }
        }).collect()
    }).collect();
    loop {
        let mut changed = false;
        for (fid, f) in m.funcs.iter().enumerate() {
            let Some(body) = &f.body else { continue };
            let mut roots: HashMap<LocalId, LocalId> = f.params.iter().map(|&p| (p, p)).collect();
            let mut next = summ[fid].clone();
            stores_block(body, f, &mut roots, &summ, &mut |root, field| {
                if let Some(i) = f.params.iter().position(|&p| p == root) { next[i].add(field); }
            });
            if next != summ[fid] { summ[fid] = next; changed = true; }
        }
        if !changed { return summ; }
    }
}

/// Walk a block for stores into arrays, calling `hit(root, field)` for each: `Some(fi)` for a
/// store to field `fi`, `None` for one that may change any. `roots` follows views to the array
/// they look into, and grows with the views the block binds.
fn stores_block(b: &Block, f: &Func, roots: &mut HashMap<LocalId, LocalId>, summ: &[Vec<FieldWrites>], hit: &mut dyn FnMut(LocalId, Option<usize>)) {
    for st in &b.stmts {
        match st {
            Stmt::Let(id, e) => {
                stores_expr(e, f, roots, summ, hit);
                if let ExprKind::Ref(s, _) | ExprKind::Local(s) = &e.kind {
                    if f.locals[*id].ty.is_arrayish() { let r = roots.get(s).copied().unwrap_or(*s); roots.insert(*id, r); }
                }
            }
            Stmt::Expr(e) | Stmt::Return(Some(e)) => stores_expr(e, f, roots, summ, hit),
            Stmt::LetRepeat(_, a, n) => { stores_expr(a, f, roots, summ, hit); stores_expr(n, f, roots, summ, hit); }
            Stmt::LetArray(_, es) => for e in es { stores_expr(e, f, roots, summ, hit); },
            Stmt::LetBuild { len, body, .. } => { stores_expr(len, f, roots, summ, hit); stores_block(body, f, roots, summ, hit); }
            Stmt::Assign(lv, _, e) => {
                match lv {
                    LValue::Index(a, i, _) => {
                        let scalar = f.locals[*a].ty.elem().is_some_and(|t| t.is_scalar());
                        let slot = match &i.kind { ExprKind::Int(k) if scalar && *k >= 0 => Some(IDX + *k as usize), _ => None };
                        hit(roots.get(a).copied().unwrap_or(*a), slot);
                        stores_expr(i, f, roots, summ, hit);
                    }
                    LValue::IndexField(a, i, fi, _) => { hit(roots.get(a).copied().unwrap_or(*a), Some(*fi)); stores_expr(i, f, roots, summ, hit); }
                    // an array local rebound by a call is written whole
                    LValue::Var(v) if f.locals[*v].ty.is_arrayish() => hit(roots.get(v).copied().unwrap_or(*v), None),
                    LValue::Var(_) | LValue::Field(..) => {}
                    LValue::FieldIndex(_, i, _, _) => stores_expr(i, f, roots, summ, hit),
                    LValue::IndexFieldIndex(a, i, fi, j, _) => { hit(roots.get(a).copied().unwrap_or(*a), Some(*fi)); stores_expr(i, f, roots, summ, hit); stores_expr(j, f, roots, summ, hit); }
                }
                stores_expr(e, f, roots, summ, hit);
            }
            // `ys = xs`: either buffer may be the other's afterwards
            Stmt::Reassign(idx) => {
                let r = &f.reassigns[*idx];
                hit(roots.get(&r.target).copied().unwrap_or(r.target), None);
                hit(roots.get(&r.src).copied().unwrap_or(r.src), None);
            }
            Stmt::For { start, end, body, .. } => { stores_expr(start, f, roots, summ, hit); stores_expr(end, f, roots, summ, hit); stores_block(body, f, roots, summ, hit); }
            Stmt::ParFor { end, body, .. } => { stores_expr(end, f, roots, summ, hit); stores_block(body, f, roots, summ, hit); }
            Stmt::While { cond, decreasing, body, .. } => {
                stores_expr(cond, f, roots, summ, hit);
                if let Some(d) = decreasing { stores_expr(d, f, roots, summ, hit); }
                stores_block(body, f, roots, summ, hit);
            }
            Stmt::Break | Stmt::Return(None) => {}
        }
    }
    if let Some(t) = &b.tail { stores_expr(t, f, roots, summ, hit); }
}

fn stores_expr(e: &Expr, f: &Func, roots: &mut HashMap<LocalId, LocalId>, summ: &[Vec<FieldWrites>], hit: &mut dyn FnMut(LocalId, Option<usize>)) {
    match &e.kind {
        ExprKind::Call(g, args) => for (i, a) in args.iter().enumerate() {
            if let ExprKind::Ref(s, true) | ExprKind::Local(s) = &a.kind {
                if f.locals[*s].ty.is_arrayish() {
                    let r = roots.get(s).copied().unwrap_or(*s);
                    match summ[*g].get(i) {
                        Some(w) if !w.all => for fi in &w.fields { hit(r, Some(*fi)); },
                        _ => hit(r, None),
                    }
                }
            }
            stores_expr(a, f, roots, summ, hit);
        },
        ExprKind::Binary(_, a, b) | ExprKind::MinMax(_, a, b) | ExprKind::FieldIndex(a, b, _) | ExprKind::InRow(a, b) => { stores_expr(a, f, roots, summ, hit); stores_expr(b, f, roots, summ, hit); }
        ExprKind::Unary(_, a) | ExprKind::Field(a, _) | ExprKind::Println(a) | ExprKind::Cast(a, _) | ExprKind::Index(_, a) => stores_expr(a, f, roots, summ, hit),
        ExprKind::StructLit(_, es) | ExprKind::ArrayVal(es) => for x in es { stores_expr(x, f, roots, summ, hit); },
        ExprKind::If(c, t, els) => { stores_expr(c, f, roots, summ, hit); stores_block(t, f, roots, summ, hit); if let Some(b) = els { stores_block(b, f, roots, summ, hit); } }
        ExprKind::Block(b) => stores_block(b, f, roots, summ, hit),
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Byte(_) | ExprKind::Local(_) | ExprKind::Len(_) | ExprKind::Ref(..) | ExprKind::Text(..) => {}
    }
}

/// `e` is `r.f + c`, `r.f` or (with no field) `r + c`, `r`, with `c ≥ 0`.
fn from_field(e: &Expr, r: LocalId, field: Option<usize>) -> bool {
    let base = |x: &Expr| match (&x.kind, field) {
        (ExprKind::Field(inner, fi), Some(f)) => *fi == f && matches!(inner.kind, ExprKind::Local(l) if l == r),
        (ExprKind::Local(l), None) => *l == r,
        _ => false,
    };
    match &e.kind {
        ExprKind::Binary(BinOp::Add, a, b) => match (&a.kind, &b.kind) {
            (_, ExprKind::Int(c)) => *c >= 0 && base(a),
            (ExprKind::Int(c), _) => *c >= 0 && base(b),
            _ => false,
        },
        _ => base(e),
    }
}

/// How many assignments to `v` a block makes, nested blocks included.
fn count_assigns(b: &Block, v: LocalId) -> usize {
    fn ex(e: &Expr, v: LocalId) -> usize {
        match &e.kind {
            ExprKind::If(c, t, els) => ex(c, v) + count_assigns(t, v) + els.as_ref().map_or(0, |b| count_assigns(b, v)),
            ExprKind::Block(b) => count_assigns(b, v),
            _ => 0,
        }
    }
    b.stmts.iter().map(|s| match s {
        Stmt::Assign(LValue::Var(w), _, e) => (*w == v) as usize + ex(e, v),
        Stmt::Assign(_, _, e) | Stmt::Let(_, e) | Stmt::Expr(e) | Stmt::LetRepeat(_, e, _) | Stmt::Return(Some(e)) => ex(e, v),
        Stmt::For { body, .. } | Stmt::While { body, .. } | Stmt::LetBuild { body, .. } | Stmt::ParFor { body, .. } => count_assigns(body, v),
        _ => 0,
    }).sum::<usize>() + b.tail.as_ref().map_or(0, |t| ex(t, v))
}

/// A callee's cost as `α·(a.len() − p) + β` piece by piece, `α` and `β` free of both (`α` may be
/// `B + 1`, bytes a line): `α` as a cost with the pieces' conditions, or `None` when some piece is
/// not of that form or `α` could be negative (§ An amortised scan).
fn distance_rate(c: &Cost, adv: super::scan::Advance) -> Option<Cost> {
    let (va, vp) = (Atom::Var(adv.a), Atom::Var(adv.p));
    let mut out = Vec::new();
    for pc in &c.pieces {
        let (mut ca, mut cp) = (Poly::zero(), Poly::zero());
        for (m, k) in &pc.poly.terms {
            let (ha, hp) = (m.has_atom(&va), m.has_atom(&vp));
            if !ha && !hp { continue; }
            if ha && hp { return None; }
            let at = if ha { &va } else { &vp };
            if m.factors.get(at) != Some(&Rat::one()) { return None; }
            let mut rest = m.clone();
            rest.factors.remove(at);
            let mut t = Poly::zero();
            t.terms.insert(rest, *k);
            if ha { ca = ca.add(&t) } else { cp = cp.add(&t) }
        }
        if !ca.add(&cp).is_zero() || !super::piece::dominates(&ca, &Poly::zero()) { return None; }
        out.push(Piece { conds: pc.conds.clone(), poly: ca });
    }
    Some(Cost { pieces: out })
}

/// The cost with each piece's distance term, `α·(a.len() − p)`, taken out.
fn strip_distance(c: &Cost, adv: super::scan::Advance) -> Cost {
    let (va, vp) = (Atom::Var(adv.a), Atom::Var(adv.p));
    c.map(|q| {
        let mut out = q.clone();
        out.terms.retain(|m, _| !(m.has_atom(&va) || m.has_atom(&vp)));
        out
    })
}

/// Whether every assignment to `t` in `b`, nested blocks included, is `t += 1` straight after a
/// write `a[t] = …` in the same block, `a` one array throughout (§ A bounded worklist).
fn pushes_only(b: &Block, t: LocalId, arr: &mut Option<LocalId>) -> bool {
    fn ex(e: &Expr, t: LocalId, arr: &mut Option<LocalId>) -> bool {
        match &e.kind {
            ExprKind::If(c, th, els) => ex(c, t, arr) && pushes_only(th, t, arr) && els.as_ref().is_none_or(|b| pushes_only(b, t, arr)),
            ExprKind::Block(b) => pushes_only(b, t, arr),
            _ => true,
        }
    }
    for (k, s) in b.stmts.iter().enumerate() {
        let ok = match s {
            Stmt::Assign(LValue::Var(v), op, e) if *v == t => {
                let one = matches!(op, Some(BinOp::Add)) && matches!(e.kind, ExprKind::Int(1));
                let after_write = k > 0 && match &b.stmts[k - 1] {
                    Stmt::Assign(LValue::Index(a, idx, _), _, _) if matches!(idx.kind, ExprKind::Local(x) if x == t) => match arr {
                        Some(y) => *y == *a,
                        None => { *arr = Some(*a); true }
                    },
                    _ => false,
                };
                one && after_write
            }
            Stmt::Assign(_, _, e) | Stmt::Let(_, e) | Stmt::Expr(e) | Stmt::LetRepeat(_, e, _) | Stmt::Return(Some(e)) => ex(e, t, arr),
            Stmt::For { body, .. } | Stmt::While { body, .. } | Stmt::ParFor { body, .. } | Stmt::LetBuild { body, .. } => pushes_only(body, t, arr),
            _ => true,
        };
        if !ok { return false; }
    }
    b.tail.as_ref().is_none_or(|e| ex(e, t, arr))
}

/// Locals assigned, inside a loop, from an expression that reads an array element: the next hop of
/// a pointer chase when one indexes an access (cost-model § Time).
fn loaded_in_loops(b: &Block) -> Vec<LocalId> {
    fn reads(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Index(..) => true,
            ExprKind::Field(x, _) | ExprKind::Unary(_, x) | ExprKind::Cast(x, _) => reads(x),
            ExprKind::Binary(_, a, c) | ExprKind::MinMax(_, a, c) => reads(a) || reads(c),
            _ => false,
        }
    }
    fn walk(b: &Block, in_loop: bool, out: &mut Vec<LocalId>) {
        for s in &b.stmts {
            match s {
                Stmt::Assign(LValue::Var(v), None, e) if in_loop && reads(e) => { if !out.contains(v) { out.push(*v); } }
                Stmt::For { body, .. } | Stmt::While { body, .. } | Stmt::ParFor { body, .. } => walk(body, true, out),
                Stmt::Expr(Expr { kind: ExprKind::If(_, t, e), .. }) => { walk(t, in_loop, out); if let Some(e) = e { walk(e, in_loop, out); } }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(b, false, &mut out);
    out
}

/// The larger of two branches' costs when it is cheap to know — one side empty, the two the same,
/// or the same regimes piece by piece with one dominating — and their sum otherwise, which is
/// never smaller than either.
fn branch_max(a: &Cost, b: &Cost) -> Cost {
    if a.pieces.is_empty() { return b.clone(); }
    if b.pieces.is_empty() || a == b { return a.clone(); }
    if a.pieces.len() == b.pieces.len() && a.pieces.iter().zip(&b.pieces).all(|(x, y)| x.conds == y.conds) {
        let pieces = a.pieces.iter().zip(&b.pieces).map(|(x, y)| {
            let poly = if super::piece::dominates(&x.poly, &y.poly) { x.poly.clone() }
                else if super::piece::dominates(&y.poly, &x.poly) { y.poly.clone() }
                else { x.poly.add(&y.poly) };
            Piece { conds: x.conds.clone(), poly }
        }).collect();
        return Cost { pieces };
    }
    a.add(b)
}

/// Whether a loop body carries a scalar from one lap to the next through a multiply or a divide:
/// some mutable local is assigned a value in which it appears under `*` or `/`, directly or through
/// a `let` of the body (`let t = zr * zr − zi * zi + cr; …; zr = t;`) — a chain the lap waits on,
/// not a reduction's one add (cost-model § Time, serial work).
fn carried_chain(b: &Block) -> bool {
    fn lets_of(b: &Block, out: &mut HashMap<LocalId, Expr>) {
        for s in &b.stmts {
            match s {
                Stmt::Let(x, e) => { out.insert(*x, e.clone()); }
                Stmt::Expr(Expr { kind: ExprKind::If(_, t, e), .. }) => { lets_of(t, out); if let Some(e) = e { lets_of(e, out); } }
                _ => {}
            }
        }
    }
    fn mentions(e: &Expr, v: LocalId, lets: &HashMap<LocalId, Expr>, depth: u32) -> bool {
        if depth == 0 { return false; }
        match &e.kind {
            ExprKind::Local(l) => *l == v || lets.get(l).is_some_and(|x| mentions(x, v, lets, depth - 1)),
            ExprKind::Binary(_, a, c) | ExprKind::MinMax(_, a, c) => mentions(a, v, lets, depth) || mentions(c, v, lets, depth),
            ExprKind::Unary(_, a) | ExprKind::Cast(a, _) => mentions(a, v, lets, depth),
            _ => false,
        }
    }
    fn under_mul(e: &Expr, v: LocalId, lets: &HashMap<LocalId, Expr>, depth: u32) -> bool {
        if depth == 0 { return false; }
        match &e.kind {
            ExprKind::Binary(BinOp::Mul | BinOp::Div, a, c) => mentions(a, v, lets, 8) || mentions(c, v, lets, 8) || under_mul(a, v, lets, depth) || under_mul(c, v, lets, depth),
            ExprKind::Binary(_, a, c) | ExprKind::MinMax(_, a, c) => under_mul(a, v, lets, depth) || under_mul(c, v, lets, depth),
            ExprKind::Unary(_, a) | ExprKind::Cast(a, _) => under_mul(a, v, lets, depth),
            ExprKind::Local(l) => lets.get(l).is_some_and(|x| under_mul(x, v, lets, depth - 1)),
            _ => false,
        }
    }
    let mut lets = HashMap::new();
    lets_of(b, &mut lets);
    fn walk(b: &Block, lets: &HashMap<LocalId, Expr>) -> bool {
        b.stmts.iter().any(|s| match s {
            Stmt::Assign(LValue::Var(v), op, e) => matches!(op, Some(BinOp::Mul | BinOp::Div)) || under_mul(e, *v, lets, 8)
                || (op.is_none() && matches!(&e.kind, ExprKind::Local(l) if lets.get(l).is_some_and(|x| under_mul(x, *v, lets, 8)))),
            Stmt::Expr(Expr { kind: ExprKind::If(_, t, e), .. }) => walk(t, lets) || e.as_ref().is_some_and(|e| walk(e, lets)),
            _ => false,
        })
    }
    walk(b, &lets)
}

/// How many stores a loop body makes to an array element of a value read from that same element, at an index
/// that does not move with the loop: `xs[k] = xs[k] − …`, `bs[i].vx = bs[i].vx − dx·m` in a loop over
/// `j`. Each lap's load waits for the last lap's store, whatever the operation (cost-model § Time,
/// serial work, through memory).
/// Whether a loop body may leave before its trip is done: a `break` of this loop (not of one
/// nested in it) or a `return`, at any depth of `if`.
fn exits_early(b: &Block) -> bool {
    fn ex(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::If(c, t, e2) => ex(c) || exits_early(t) || e2.as_ref().is_some_and(|b| exits_early(b)),
            ExprKind::Block(b) => exits_early(b),
            _ => false,
        }
    }
    b.stmts.iter().any(|s| match s {
        Stmt::Break | Stmt::Return(_) => true,
        Stmt::Expr(e) | Stmt::Let(_, e) => ex(e),
        _ => false,
    }) || b.tail.as_ref().is_some_and(|t| ex(t))
}

/// How many `f64` locals a loop body carries from one lap to the next through an add or a
/// subtract — `s += xs[i]`, `e = e − d` — a reduction the lap waits on for the add's latency, which
/// a unit of work does not have: one unit of serial work each a lap (cost-model § Time, serial work).
/// A chain through a multiply is `carried_chain`'s, and all of the lap's work serial.
fn float_chain(b: &Block, f: &Func) -> usize {
    fn top(e: &Expr, v: LocalId) -> bool {
        match &e.kind {
            ExprKind::Local(l) => *l == v,
            ExprKind::Binary(BinOp::Add, a, c) => top(a, v) || top(c, v),
            ExprKind::Binary(BinOp::Sub, a, _) => top(a, v),
            _ => false,
        }
    }
    fn walk(b: &Block, f: &Func, out: &mut Vec<LocalId>) {
        for s in &b.stmts {
            match s {
                Stmt::Assign(LValue::Var(v), op, e) if f.locals[*v].ty == Ty::F64 => {
                    let carried = match op { Some(BinOp::Add | BinOp::Sub) => true, None => !matches!(e.kind, ExprKind::Local(_)) && top(e, *v), _ => false };
                    if carried && !out.contains(v) { out.push(*v); }
                }
                Stmt::Expr(Expr { kind: ExprKind::If(_, t, e), .. }) => { walk(t, f, out); if let Some(e) = e { walk(e, f, out); } }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(b, f, &mut out);
    out.len()
}

fn memory_chain(b: &Block, var: Option<LocalId>) -> usize {
    fn same(a: &Expr, b: &Expr) -> bool {
        match (&a.kind, &b.kind) {
            (ExprKind::Local(x), ExprKind::Local(y)) => x == y,
            (ExprKind::Int(x), ExprKind::Int(y)) => x == y,
            (ExprKind::Binary(o, l, r), ExprKind::Binary(p, m, n)) => o == p && same(l, m) && same(r, n),
            _ => false,
        }
    }
    fn reads(e: &Expr, arr: LocalId, idx: &Expr, field: Option<usize>) -> bool {
        match &e.kind {
            ExprKind::Index(a, i) if field.is_none() => *a == arr && same(i, idx),
            ExprKind::Field(inner, fi) => match &inner.kind {
                ExprKind::Index(a, i) => Some(*fi) == field && *a == arr && same(i, idx),
                _ => reads(inner, arr, idx, field),
            },
            ExprKind::Binary(_, l, r) | ExprKind::MinMax(_, l, r) => reads(l, arr, idx, field) || reads(r, arr, idx, field),
            ExprKind::Unary(_, x) | ExprKind::Cast(x, _) => reads(x, arr, idx, field),
            _ => false,
        }
    }
    // an index that moves with the loop is one the body assigns, or a local it binds per lap
    let mut moving: Vec<LocalId> = var.into_iter().collect();
    for s in &b.stmts { match s { Stmt::Assign(LValue::Var(v), _, _) | Stmt::Let(v, _) => moving.push(*v), _ => {} } }
    fn fixed(e: &Expr, moving: &[LocalId]) -> bool {
        match &e.kind {
            ExprKind::Local(l) => !moving.contains(l),
            ExprKind::Int(_) => true,
            ExprKind::Binary(_, l, r) => fixed(l, moving) && fixed(r, moving),
            _ => false,
        }
    }
    b.stmts.iter().filter(|s| match s {
        Stmt::Assign(LValue::Index(a, i, _), op, e) => fixed(i, &moving) && (op.is_some() || reads(e, *a, i, None)),
        Stmt::Assign(LValue::IndexField(a, i, f, _), op, e) => fixed(i, &moving) && (op.is_some() || reads(e, *a, i, Some(*f))),
        _ => false,
    }).count()
}

/// A store that reads the element the last lap stored — `a[i·n + j] = (… + a[i·n + j − 1] + …) / 9`
/// in a loop over `j` (Gauss–Seidel) — waits for that store every lap: a recurrence through memory,
/// as a carried scalar is one through a register (cost-model § Time, serial work). `Some(true)` when
/// the value read reaches the store through a multiply or a divide, which makes the whole lap
/// serial as a scalar multiply chain does; `Some(false)` when only through adds, one unit a lap as a
/// store to a fixed element is; `None` when no store reads its last lap's element. The last lap's
/// element is the store's index less its coefficient in the loop variable times the step, the two
/// indices compared as polynomials in the locals.
fn lap_chain(b: &Block, var: Option<LocalId>, step: i128) -> Option<bool> {
    type P = BTreeMap<Vec<LocalId>, i128>;
    fn poly(e: &Expr) -> Option<P> {
        let mut out = P::new();
        match &e.kind {
            ExprKind::Int(k) => { if *k != 0 { out.insert(vec![], *k as i128); } }
            ExprKind::Local(l) => { out.insert(vec![*l], 1); }
            ExprKind::Cast(x, _) => return poly(x),
            ExprKind::Binary(op @ (BinOp::Add | BinOp::Sub), l, r) => {
                out = poly(l)?;
                let sign = if *op == BinOp::Sub { -1 } else { 1 };
                for (m, c) in poly(r)? { *out.entry(m).or_insert(0) += sign * c; }
            }
            ExprKind::Binary(BinOp::Mul, l, r) => {
                for (ma, ca) in poly(l)? {
                    for (mb, cb) in poly(r)? {
                        let mut m = ma.clone();
                        m.extend(mb.iter().copied());
                        m.sort();
                        *out.entry(m).or_insert(0) += ca * cb;
                    }
                }
            }
            _ => return None,
        }
        out.retain(|_, c| *c != 0);
        Some(out)
    }
    // does `e` read `arr` at an index whose polynomial is `want`, and is it under a multiply or divide
    fn find(e: &Expr, arr: LocalId, want: &P, under: bool) -> Option<bool> {
        match &e.kind {
            ExprKind::Index(a, i) if *a == arr && poly(i).as_ref() == Some(want) => Some(under),
            ExprKind::Binary(op, l, r) => {
                let u = under || matches!(op, BinOp::Mul | BinOp::Div);
                match (find(l, arr, want, u), find(r, arr, want, u)) {
                    (Some(x), Some(y)) => Some(x || y),
                    (x, y) => x.or(y),
                }
            }
            ExprKind::Unary(_, x) | ExprKind::Cast(x, _) => find(x, arr, want, under),
            _ => None,
        }
    }
    let v = var?;
    let mut found: Option<bool> = None;
    for s in &b.stmts {
        let Stmt::Assign(LValue::Index(a, i, _), op, e) = s else { continue };
        let Some(pi) = poly(i) else { continue };
        // the store's coefficient in the loop variable, a constant, and the index a lap earlier
        let lin: Vec<(&Vec<LocalId>, &i128)> = pi.iter().filter(|(m, _)| m.contains(&v)).collect();
        let [(m, c)] = lin.as_slice() else { continue };
        if m.len() != 1 { continue; }
        let mut prev = pi.clone();
        *prev.entry(vec![]).or_insert(0) -= **c * step;
        prev.retain(|_, c| *c != 0);
        // a compound `*=` or `/=` puts the read under the multiply too
        if let Some(u) = find(e, *a, &prev, matches!(op, Some(BinOp::Mul | BinOp::Div))) {
            found = Some(found.unwrap_or(false) || u);
        }
    }
    found
}

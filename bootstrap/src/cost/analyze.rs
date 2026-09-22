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
use super::size::{Atom, Poly, Rat};

#[derive(Debug, Clone, Copy)]
pub struct Machine {
    /// Bytes of the cache whose boundary moves are counted across.
    pub m_bytes: i128,
    /// Bytes per cache line.
    pub b_bytes: i128,
}

#[derive(Debug, Clone)]
pub enum CostResult {
    Exact { work: Cost, moves: Cost },
    Unknown { reason: String, line: u32 },
}

#[derive(Debug, Clone)]
pub struct FuncCost {
    pub name: String,
    /// Names of `Atom::Var(i)` for this function. The first `params.len()` are the parameters,
    /// in order, so a caller can substitute by position.
    pub names: Vec<String>,
    pub result: CostResult,
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

pub fn analyze(m: &Module, machine: &Machine) -> Vec<FuncCost> {
    let mut an = Analyzer { m, machine: *machine, done: vec![None; m.funcs.len()], active: vec![false; m.funcs.len()] };
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
            sugg.push(Suggestion { label: format!("tile T < {}", c.side), flag: format!("{}:tile={}", f.name, c.t), result: CostResult::Exact { work: c.work.clone(), moves: c.moves.clone() } });
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
    let CostResult::Exact { work, moves } = &c.result else { return None };
    if std::env::var("NEANT_DEBUG_TILE").is_ok() { eprint!("{}", super::lock::report(&c, &machine)); }
    // the reference point: this machine, every size a million — tiling is for large sizes
    let point = |t: Option<f64>| move |a: Atom| match a { Atom::B => Some(machine.b_bytes as f64), Atom::M => Some(machine.m_bytes as f64), Atom::Var(v) if v == tvar => t, Atom::Var(_) => Some(1e6), Atom::Log(_) => None };
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
}

impl<'a> Analyzer<'a> {
    fn func(&mut self, fid: FuncId) -> &FuncCost {
        if self.done[fid].is_none() {
            let f = &self.m.funcs[fid];
            if self.active[fid] {
                // a cycle: recursion is a recurrence, which is not solved yet (M3)
                self.done[fid] = Some(FuncCost {
                    name: f.name.clone(),
                    names: param_names(f),
                    result: CostResult::Unknown { reason: "mutually recursive with another function; only self-recursion is solved".into(), line: f.line },
                    bounds: vec![], notes: vec![], suggestions: vec![], effects: vec![], violations: vec![], tier: "unknown",
                    footprint: vec![], resident: None, declared: Declared::default(), rests_on: vec![],
                });
            } else {
                self.active[fid] = true;
                let fc = Fa::new(self, f, Some(fid)).run();
                self.active[fid] = false;
                self.done[fid] = Some(fc);
            }
        }
        self.done[fid].as_ref().unwrap()
    }
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
}

enum Fail {
    Unknown(String, u32),
}

/// One `x[i]` in the source, with the loops it sits in (outermost first). Moves are settled
/// for all sites together once the body has been walked, because whether a level reuses its
/// lines depends on every site sharing that loop.
struct Site {
    arr: LocalId,
    aff: Option<Affine>,
    /// bytes the site actually touches: the element's size, or one field's under AoS
    es: i128,
    /// bytes the address moves per unit of the index: the element's size in memory. They differ
    /// for a field of a struct array, and that difference is what a layout costs.
    stride: i128,
    /// which field, when the site is one field of a struct element
    field: Option<usize>,
    path: Vec<usize>,
    /// the `if` branches this site sits in, outermost first: sites on different sides of one
    /// `if` are alternatives, and their moves combine by max, not sum
    branch: Vec<(usize, bool)>,
}

/// What is remembered of a loop after it is popped.
#[derive(Clone)]
struct LoopRec {
    var: Option<LocalId>,
    atom: Option<usize>,
    trip: Poly,
    lo: Poly,
    step: i128,
}

impl LoopRec {
    /// `Σ` of `p` over this loop's iterations: exact over the atom, `× trip` when `p` does not
    /// mention it.
    fn sum(&self, p: &Poly) -> Poly {
        match self.atom { Some(a) => p.sum_over(a, &self.lo, self.step, &self.trip), None => p.mul(&self.trip) }
    }
    fn sum_cost(&self, c: &Cost) -> Cost {
        match self.atom { Some(a) => c.sum_over(a, &self.lo, self.step, &self.trip), None => c.mul_poly(&self.trip) }
    }
    /// The loop variable's last value.
    fn last(&self) -> Poly { self.lo.add(&self.trip.sub(&Poly::constant(1)).scale(Rat::int(self.step))) }
}

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
    /// each self-call: the argument sizes by parameter (None when not a size), how many times
    /// the site runs per invocation, and its line
    rec_calls: Vec<(Vec<Option<Poly>>, Poly, u32)>,
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
    saved: Vec<(Cost, Cost)>,
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
}

impl<'a, 'b, 'c> Fa<'a, 'b, 'c> {
    fn new(an: &'b mut Analyzer<'a>, f: &'c Func, self_fid: Option<FuncId>) -> Self {
        let mut fa = Fa {
            an, f, names: param_names(f), sites: vec![], loop_recs: vec![], bounds: vec![], images: HashMap::new(), scalar_alias: HashMap::new(), alias_scopes: vec![], notes: vec![], io: false, self_fid, rec_calls: vec![],
            local_size: HashMap::new(), local_affine: HashMap::new(), initial: HashMap::new(), at_entry: false,
            loops: vec![], work: Cost::zero(), moves: Cost::zero(), saved: vec![], branch: vec![], next_if: 0,
            local_root: HashMap::new(), resident: vec![], call_moves: Cost::zero(), saved_calls: vec![], replay: false, has_call: vec![], rests_on: vec![],
        };
        for (i, &p) in f.params.iter().enumerate() {
            let l = &f.locals[p];
            if l.ty.is_arrayish() {
                fa.local_size.insert(p, Poly::var(i));
                fa.local_root.insert(p, p);
            } else if l.ty == Ty::I64 {
                fa.local_affine.insert(p, Affine::constant(Poly::var(i)));
            }
        }
        fa
    }

    fn run(mut self) -> FuncCost {
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
                (Some(w), Some(m)) => CostResult::Exact { work: Cost::poly(w.clone()), moves: Cost::poly(m.clone()) },
                _ if effects.contains(&"unbounded") => CostResult::Unknown { reason: "declared unbounded".into(), line: self.f.line },
                _ => CostResult::Unknown { reason: "an extern needs `#[cost(work_at_most = …, moves_at_most = …)]` or `uses unbounded`".into(), line: self.f.line },
            };
            return FuncCost { name: self.f.name.clone(), names: self.names, result, bounds: vec![], notes: vec![], suggestions: vec![], effects, violations, tier: "declared", footprint: vec![], resident: None, declared, rests_on: vec![] };
        };
        let mut tier = "exact";
        let walked = self.block(body);
        let (footprint, resident) = self.signature_footprint();
        let result = match walked {
            Ok(()) => {
                self.settle_moves();
                let calls = std::mem::replace(&mut self.call_moves, Cost::zero());
                self.moves = self.moves.add(&calls);
                let m = self.machine();
                self.work.prune_at(&m);
                self.moves.prune_at(&m);
                if self.rec_calls.is_empty() {
                    CostResult::Exact { work: self.work.clone(), moves: self.moves.clone() }
                } else {
                    tier = "recurrence";
                    match self.solve_recurrence() {
                        Ok((work, moves)) => CostResult::Exact { work, moves },
                        Err(reason) => CostResult::Unknown { reason, line: self.rec_calls[0].2 },
                    }
                }
            }
            Err(Fail::Unknown(reason, line)) => CostResult::Unknown { reason, line },
        };
        // `#[cost(...)]`: the inferred cost must stay within the declaration. Without `sizes`,
        // by asymptotic dominance in every regime; with `sizes`, as numbers at those bounds and
        // the machine's B and M — a budget in real units.
        let m = self.machine();
        let line_of = |key: &str| self.f.asserts.iter().find(|a| a.0 == key).map_or(self.f.line, |a| a.2);
        for (which, asserted) in [("work", declared.work.clone()), ("moves", declared.moves.clone())] {
            let Some(asserted) = asserted else { continue };
            let line = line_of(&format!("{which}_at_most"));
            match &result {
                CostResult::Exact { work, moves } => {
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
                                Atom::B => Some(m.b_bytes as f64), Atom::M => Some(m.m_bytes as f64),
                                Atom::Var(i) => declared.sizes.iter().find(|(v, _)| *v == i).map(|(_, x)| *x),
                                Atom::Log(_) => None,
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
        bounds::strongest_first(&mut self.bounds, &m);
        FuncCost { name: self.f.name.clone(), names: self.names, result, bounds: self.bounds, notes: self.notes, suggestions: vec![], effects, violations, tier, footprint, resident, declared, rests_on: self.rests_on }
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
            let (a, b) = (c.mul(&rec.lo), c.mul(&rec.last()));
            if negative { lo = lo.add(&b); hi = hi.add(&a); } else { lo = lo.add(&a); hi = hi.add(&b); }
        }
        let st = Rat::int(site.stride);
        Some((lo.scale(st), hi.scale(st).add(&Poly::constant(site.es))))
    }

    /// The function's footprint over its parameters, and the condition under which all of it is
    /// resident on return. A site whose range is not exact makes its parameter's range the whole
    /// array and forfeits the residue.
    fn signature_footprint(&self) -> (Vec<Foot>, Option<Cond>) {
        let mut feet: HashMap<usize, (Poly, Poly, bool)> = HashMap::new();
        let mut total = Poly::zero();
        let mut all_exact = true;
        for site in &self.sites {
            let root = self.local_root.get(&site.arr).copied().unwrap_or(site.arr);
            let whole = |r: LocalId| (Poly::zero(), self.local_size.get(&r).cloned().unwrap_or_else(Poly::zero).scale(Rat::int(site.es)));
            let Some(pi) = self.f.params.iter().position(|&p| p == root) else {
                // an internal array: competes for the cache, invisible to the caller
                let (_, h) = whole(root);
                if !feet.contains_key(&(usize::MAX - root)) { feet.insert(usize::MAX - root, (Poly::zero(), h, true)); }
                continue;
            };
            let (lo, hi, exact) = match self.site_range(site) { Some((l, h)) => (l, h, true), None => { let (l, h) = whole(root); (l, h, false) } };
            match feet.get_mut(&pi) {
                None => { feet.insert(pi, (lo, hi, exact)); }
                Some(e) => {
                    // two ranges on one parameter: exact only if identical, else the whole array
                    if !(exact && e.2 && e.0 == lo && e.1 == hi) { let (l, h) = whole(root); *e = (l, h, false); }
                }
            }
        }
        for (_, (lo, hi, exact)) in &feet { total = total.add(&hi.sub(lo)); if !exact { all_exact = false; } }
        let mut out: Vec<Foot> = feet.into_iter().filter(|(k, _)| *k < usize::MAX / 2).map(|(param, (lo, hi, exact))| Foot { param, lo, hi, exact }).collect();
        out.sort_by_key(|f| f.param);
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
        if !self.replay { self.work = self.work.add_poly(&p); }
    }
    /// A fresh size atom for a loop variable.
    fn new_atom(&mut self, name: &str) -> usize {
        self.names.push(name.to_string());
        self.names.len() - 1
    }
    /// Open a loop: push it and start a fresh accumulator frame for its body.
    fn enter_loop(&mut self, lp: Loop) {
        self.loop_recs.push(LoopRec { var: lp.var, atom: lp.atom, trip: lp.trip.clone(), lo: lp.lo.clone(), step: lp.step });
        let lp = Loop { id: self.loop_recs.len() - 1, ..lp };
        self.loops.push(lp);
        self.push_frame();
    }
    fn push_frame(&mut self) {
        self.saved.push((std::mem::replace(&mut self.work, Cost::zero()), std::mem::replace(&mut self.moves, Cost::zero())));
        self.saved_calls.push(std::mem::replace(&mut self.call_moves, Cost::zero()));
        self.has_call.push(false);
    }
    /// Pop a frame (an `if` branch): its calls count once, into the frame below.
    fn pop_frame(&mut self) -> (Cost, Cost) {
        let (pw, pm) = self.saved.pop().unwrap();
        let pc = self.saved_calls.pop().unwrap();
        let had = self.has_call.pop().unwrap_or(false);
        if let Some(h) = self.has_call.last_mut() { *h |= had; }
        let calls = std::mem::replace(&mut self.call_moves, pc);
        self.call_moves = self.call_moves.add(&calls);
        (std::mem::replace(&mut self.work, pw), std::mem::replace(&mut self.moves, pm))
    }
    /// Leave a loop. The body frame is summed over the loop variable. Calls in the body are costed
    /// twice: as walked (cold, the first iteration) and again with the residue the first iteration
    /// left (the iterations after it), so a call that re-reads what the last one left in cache
    /// pays once.
    fn leave_loop(&mut self, body: &Block) -> Result<(), Fail> {
        let lp = self.loops.pop().unwrap();
        let (pw, pm) = self.saved.pop().unwrap();
        let pc = self.saved_calls.pop().unwrap();
        let had_call = self.has_call.pop().unwrap_or(false);
        let cold_calls = std::mem::replace(&mut self.call_moves, pc);
        let (w, m) = (std::mem::replace(&mut self.work, pw), std::mem::replace(&mut self.moves, pm));
        let rec = self.loop_recs[lp.id].clone();
        self.work = self.work.add(&rec.sum_cost(&w));
        self.moves = self.moves.add(&rec.sum_cost(&m));
        if had_call {
            let first = match rec.atom { Some(a) => cold_calls.subst(a, &rec.lo), None => cold_calls.clone() };
            let warm = if self.numeric(&rec.trip).is_some_and(|t| t <= 1.0) { Cost::zero() } else {
                self.loops.push(Loop { id: lp.id, ..lp });
                let outer_replay = self.replay;
                self.replay = true;
                self.push_frame();
                let r = self.block(body);
                // keep only the recounted call moves; everything else returns to what it was
                let (pw, pm) = self.saved.pop().unwrap();
                let pc = self.saved_calls.pop().unwrap();
                self.has_call.pop();
                let warm_body = std::mem::replace(&mut self.call_moves, pc);
                self.work = pw;
                self.moves = pm;
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
    fn add_work_n(&mut self, n: i128) { self.add_work(Poly::constant(n)); }

    /// Numeric value of a polynomial with `B` and `M` at their machine values, if it has no
    /// size variables. The fit test and the stride test are decided with this.
    fn numeric(&self, p: &Poly) -> Option<f64> {
        let m = self.machine();
        if p.has_vars() { return None; }
        p.eval(&|a| match a {
            Atom::B => Some(m.b_bytes as f64),
            Atom::M => Some(m.m_bytes as f64),
            Atom::Var(_) | Atom::Log(_) => None,
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
                if let Some(lp) = self.loops.iter().find(|lp| lp.var == Some(*l)) {
                    return lp.atom.map(Poly::var);
                }
                if self.at_entry && self.f.locals[*l].mutable { return self.entry_value(*l); }
                let a = self.local_affine.get(l)?;
                if !a.is_const() { return None; }
                Some(a.konst.clone())
            }
            ExprKind::Len(l) => self.local_size.get(l).cloned(),
            ExprKind::Cast(inner, Ty::I64) => self.size_of(inner, bound),
            // min is bounded above by either argument, max below by either: the first that is a size
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
            _ => None,
        }
    }

    /// An index expression as an affine function of the loop variables in scope.
    fn affine(&self, e: &Expr) -> Option<Affine> {
        match &e.kind {
            ExprKind::Int(v) => Some(Affine::constant(Poly::constant(*v as i128))),
            ExprKind::Local(l) => {
                if let Some(lp) = self.loops.iter().find(|lp| lp.var == Some(*l)) {
                    return lp.offset.as_ref().map(|off| Affine::var(*l).add(off));
                }
                self.local_affine.get(l).cloned()
            }
            ExprKind::Len(l) => self.local_size.get(l).map(|p| Affine::constant(p.clone())),
            ExprKind::Cast(inner, Ty::I64) => self.affine(inner),
            // `min(ii*T + T, n)` as a loop end: the tile bound, the rectangular hull of the rest
            ExprKind::MinMax(true, a, _) => self.affine(a),
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
    /// What a site on `l` touches: one field's bytes, or the whole element.
    fn touch_bytes(&self, l: LocalId, field: Option<usize>) -> i128 {
        match (self.f.locals[l].ty.elem(), field) {
            (Some(Ty::Struct(s)), Some(fi)) => self.an.m.structs[*s].fields[fi].1.elem_bytes(),
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
        let es = self.touch_bytes(arr, field);
        let stride = self.elem_bytes(arr);
        let aff = self.affine(idx);
        let path: Vec<usize> = self.loops.iter().map(|l| l.id).collect();
        let root = self.local_root.get(&arr).copied().unwrap_or(arr);
        if let Some(prev) = self.sites.iter_mut().find(|s| {
            let sroot = s.arr;
            sroot == arr && s.stride == stride && s.path == path && s.branch == self.branch
                && s.aff.is_some() && s.aff == aff
        }) {
            let _ = root;
            // the same element: the span the two fields cover together, which is the element
            if prev.field != field { prev.es = stride; }
            return;
        }
        self.sites.push(Site { arr, aff, es, stride, field, path, branch: self.branch.clone() });
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
        for lid in ids {
            let members: Vec<(usize, usize)> = (0..nsites)
                .filter_map(|s| self.sites[s].path.iter().position(|&l| l == lid).map(|pos| (s, pos)))
                .collect();
            if members.is_empty() { continue; }
            let rec = &self.loop_recs[lid];
            let inners: Vec<Vec<LP>> = members.iter().map(|&(s, pos)| inner(&table, s, &self.sites[s].path, pos)).collect();
            let mut out: Vec<Vec<LP>> = vec![Vec::new(); members.len()];
            // every feasible choice of one alternative per site is a working set to test
            let mut choice = vec![0usize; members.len()];
            loop {
                let picks: Vec<&LP> = (0..members.len()).map(|i| &inners[i][choice[i]]).collect();
                let mut conds: Vec<Cond> = Vec::new();
                for lp in &picks { for c in &lp.conds { if !conds.contains(c) { conds.push(c.clone()); } } }
                if super::piece::feasible(&conds) {
                    let ws = picks.iter().fold(Poly::zero(), |acc, lp| acc.add(&lp.lines));
                    // a working set that varies with this loop's variable is tested at its largest
                    let test: Option<Poly> = match rec.atom {
                        Some(a) if ws.mentions(a) => {
                            let signs: Vec<i128> = ws.terms.iter().filter(|(mo, _)| mo.has_atom(&Atom::Var(a))).map(|(_, c)| c.n.signum()).collect();
                            if signs.iter().all(|&x| x >= 0) { Some(ws.subst(a, &rec.last())) }
                            else if signs.iter().all(|&x| x <= 0) { Some(ws.subst(a, &rec.lo)) }
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
                        for (i, &(s, _)) in members.iter().enumerate() {
                            let site = &self.sites[s];
                            let lp = picks[i];
                            let summed = rec.sum(&lp.lines);
                            let same_set = || if rec.atom.is_some_and(|a| lp.lines.mentions(a)) { summed.clone() } else { lp.lines.clone() };
                            let (total, contig) = if !fits {
                                (summed.clone(), false)
                            } else {
                                let stride = site.aff.as_ref().map(|a| rec.var.and_then(|v| a.coeffs.get(&v).cloned()).unwrap_or_else(Poly::zero).scale(Rat::int(site.stride)));
                                match stride {
                                    None => (summed.clone(), false),
                                    Some(st) if st.is_zero() => (same_set(), lp.contig),
                                    Some(st) => match self.numeric(&st) {
                                        Some(sb) if sb.abs() >= m.b_bytes as f64 => (summed.clone(), false),
                                        Some(sb) => {
                                            let slide = rec.sum(&Poly::constant(1)).scale(Rat::new(sb.abs() as i128, 1)).mul_atom_pow(Atom::B, Rat::int(-1));
                                            let slide = match self.numeric(&slide) { Some(v) if v < 1.0 => Poly::constant(1), _ => slide };
                                            if lp.contig { (same_set().add(&slide), true) } else { (same_set().mul(&slide), false) }
                                        }
                                        None => (summed.clone(), false),
                                    },
                                }
                            };
                            let np = LP { conds: conds.clone(), lines: total, contig };
                            if !out[i].iter().any(|x| x.conds == np.conds && x.lines == np.lines && x.contig == np.contig) { out[i].push(np); }
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
                    LValue::IndexField(a, i, fi, _) => refs.push((*a, i, Some(*fi))),
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
        let nest: Vec<(LocalId, usize, usize)> = self.loops.iter().enumerate().filter_map(|(k, l)| Some((l.var?, l.atom?, k))).collect();
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
        if let Some(inner) = self.loops.last().and_then(|l| l.var) {
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
        match s {
            Stmt::Let(id, e) => {
                self.recognise(s);
                self.expr(e)?;
                let l = &self.f.locals[*id];
                if l.ty.is_arrayish() {
                    let src = match &e.kind { ExprKind::Ref(s, _) | ExprKind::Local(s) => Some(*s), _ => None };
                    if let Some(sz) = src.and_then(|s| self.local_size.get(&s).cloned()) {
                        self.local_size.insert(*id, sz);
                    }
                    if let Some(s0) = src { let r = self.local_root.get(&s0).copied().unwrap_or(s0); self.local_root.insert(*id, r); }
                } else if l.ty == Ty::I64 && !l.mutable {
                    if let Some(a) = self.affine(e) { self.local_affine.insert(*id, a); }
                } else if l.ty == Ty::I64 {
                    let v = self.size_of(e, Dir::Upper).map(|p| vec![p]);
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
                self.enter_loop(Loop { id: 0, var: Some(*var), atom: Some(atom), trip: size, lo: Poly::zero(), step: 1, offset: Some(Affine::constant(Poly::zero())) });
                self.add_work_n(3); // store, increment, compare-and-branch
                let r = self.block(body);
                r?;
                self.leave_loop(body)
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
                        let val = if op.is_none() && self.loops.is_empty() { self.size_of(e, Dir::Upper).map(|p| vec![p]) } else { None };
                        self.initial.insert(*v, val);
                    }
                }
                match lv {
                    // a register: only the operation of `op=` costs
                    LValue::Var(_) => self.add_work_n(if op.is_some() { 1 } else { 0 }),
                    // a store, and for `op=` a load and the operation as well
                    LValue::Index(arr, idx, _) => {
                        self.expr(idx)?;
                        self.add_work_n(if op.is_some() { 3 } else { 1 });
                        self.access(*arr, idx, None);
                    }
                    LValue::Field(..) => self.add_work_n(if op.is_some() { 1 } else { 0 }),
                    LValue::IndexField(arr, idx, fi, _) => {
                        self.expr(idx)?;
                        self.add_work_n(if op.is_some() { 3 } else { 1 });
                        self.access(*arr, idx, Some(*fi));
                    }
                }
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
                self.enter_loop(Loop { id: 0, var: Some(*var), atom: Some(atom), trip, lo, step: 1, offset: a_lo });
                self.add_work_n(2); // increment, compare-and-branch, per iteration
                let r = self.block(body);
                r?;
                self.leave_loop(body)
            }
            Stmt::Break => Ok(()),
            Stmt::While { cond, decreasing, body, line } => {
                self.expr(cond)?;
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
                let (var, atom, lo, step, offset) = match ind {
                    Some((v, i0, st)) => {
                        let a = self.new_atom(&self.f.locals[v].name.clone());
                        (Some(v), Some(a), i0.clone(), st, Some(Affine::constant(i0)))
                    }
                    None => (None, None, Poly::zero(), 1, None),
                };
                self.enter_loop(Loop { id: 0, var, atom, trip, lo, step, offset });
                self.add_work_n(1);
                let r = self.block(body);
                r?;
                self.leave_loop(body)
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
                    Stmt::Assign(LValue::Var(_) | LValue::Index(..) | LValue::Field(..) | LValue::IndexField(..), _, _) | Stmt::Let(..) | Stmt::LetArray(..) | Stmt::LetRepeat(..) | Stmt::LetBuild { .. } => {}
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
    fn induction_trip(&self, cond: &Expr, body: &Block) -> Result<(Poly, Option<(LocalId, Poly, i128)>), String> {
        let ask = "`while` has no measure the compiler can find; write `while cond decreasing <expr>` with an `i64` that goes down by at least one every iteration".to_string();
        let ExprKind::Binary(op, l, r) = &cond.kind else { return Err(ask) };
        // normalise to (var, bound, ascending)
        let (var, bound, asc) = match (&l.kind, &r.kind, op) {
            (ExprKind::Local(v), _, BinOp::Lt | BinOp::Le | BinOp::Ne) => (*v, &**r, true),
            (_, ExprKind::Local(v), BinOp::Gt | BinOp::Ge) => (*v, &**l, true),
            (ExprKind::Local(v), _, BinOp::Gt | BinOp::Ge) => (*v, &**r, false),
            (_, ExprKind::Local(v), BinOp::Lt | BinOp::Le) => (*v, &**l, false),
            _ => return Err(ask),
        };
        let name = &self.f.locals[var].name;
        if !self.f.locals[var].mutable || self.f.locals[var].ty != Ty::I64 { return Err(ask); }
        let Some(step) = single_step(body, var) else { return Err(format!("`{name}` is compared in the `while` condition but is not stepped by a constant exactly once in the body; {ask}")) };
        if (asc && step <= 0) || (!asc && step >= 0) { return Err(format!("`{name}` steps away from its bound; {ask}")); }
        if assigns(body, |l| l != var && bound_mentions(bound, l)) { return Err(format!("the bound of `{name}` is assigned inside the body; {ask}")); }
        let Some(e) = self.size_of(bound, if asc { Dir::Upper } else { Dir::Lower }) else { return Err(format!("the bound of `{name}` is not a size expression; {ask}")) };
        let i0 = match self.initial.get(&var) {
            Some(Some(vs)) if vs.len() == 1 => vs[0].clone(),
            Some(Some(vs)) => return Err(format!("`{name}` may hold any of {} values on entry, one per path that defines it; a single value is needed until `max` is in the cost algebra", vs.len())),
            _ => return Err(format!("`{name}` is assigned inside a loop before this one, so its entry value is not known")),
        };
        let span = if asc { e.sub(&i0) } else { i0.sub(&e) };
        // the variable steps by |step| per iteration: indices in it move by step·elem bytes,
        // which the stride rule sees through the loop variable's coefficient
        Ok((span.scale(Rat::new(1, step.abs() as i128)), Some((var, i0, step as i128))))
    }

    fn expr(&mut self, e: &Expr) -> Result<(), Fail> {
        match &e.kind {
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Byte(_) | ExprKind::Local(_) | ExprKind::Ref(..) => Ok(()),
            ExprKind::Len(_) => Ok(()), // the length is already in a register
            ExprKind::Binary(_, a, b) => { self.expr(a)?; self.expr(b)?; self.add_work_n(1); Ok(()) }
            ExprKind::MinMax(_, a, b) => { self.expr(a)?; self.expr(b)?; self.add_work_n(2); Ok(()) } // compare, select
            ExprKind::Unary(_, a) | ExprKind::Cast(a, _) => { self.expr(a)?; self.add_work_n(1); Ok(()) }
            ExprKind::Println(a) => { self.expr(a)?; self.add_work_n(1); if !self.replay { self.io = true; } Ok(()) }
            ExprKind::Index(arr, idx) => {
                self.expr(idx)?;
                self.add_work_n(1);
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
                let (wt, mt) = self.pop_frame();
                self.branch.pop();
                let then_calls: Vec<_> = self.rec_calls.drain(before..).collect();
                let then_init = std::mem::replace(&mut self.initial, entry_init);
                self.branch.push((if_id, false));
                self.push_frame();
                if let Some(b) = els { self.block(b)?; }
                let (we, me) = self.pop_frame();
                self.branch.pop();
                // the two branches are alternatives: the cost is the larger, not the sum
                self.work = self.work.add(&wt.max(&we));
                self.moves = self.moves.add(&mt.max(&me));
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
                self.add_work_n(2); // call and return; arguments are register moves
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
                let (mut w, mut mv) = if declared_only {
                    (Cost::poly(callee.declared.work.clone().unwrap()), Cost::poly(callee.declared.moves.clone().unwrap()))
                } else {
                    match &callee.result {
                        CostResult::Exact { work, moves } => (work.clone(), moves.clone()),
                        CostResult::Unknown { reason, .. } => {
                            let reason = if reason.starts_with("calls `") { reason.clone() }
                                else { format!("calls `{}`, whose cost is unknown ({reason})", callee.name) };
                            return Err(Fail::Unknown(reason, e.line));
                        }
                    }
                };
                if !self.replay && callee.effects.contains(&"unbounded") {
                    return Err(Fail::Unknown(format!("calls `{}`, which is declared unbounded", callee.name), e.line));
                }
                // provenance: a declared callee is an assumption this line now rests on
                if !self.replay {
                    if declared_only {
                        let how = if cf.body.is_none() { "declared, extern" } else { "declared, checked" };
                        let tag = format!("{} ({how})", callee.name);
                        if !self.rests_on.contains(&tag) { self.rests_on.push(tag); }
                    }
                    for r in &callee.rests_on { if !self.rests_on.contains(r) { self.rests_on.push(r.clone()); } }
                }
                let mut map: Vec<(usize, Poly)> = Vec::new();
                let mut roots: Vec<Option<LocalId>> = Vec::new();
                for (i, (a, &p)) in args.iter().zip(&cf.params).enumerate() {
                    let (by, root) = if cf.locals[p].ty.is_arrayish() {
                        match &a.kind {
                            ExprKind::Ref(s, _) | ExprKind::Local(s) => (self.local_size.get(s).cloned(), Some(self.local_root.get(s).copied().unwrap_or(*s))),
                            _ => (None, None),
                        }
                    } else { (self.size_of(a, Dir::Upper), None) };
                    roots.push(root);
                    match by {
                        Some(b) => map.push((i, b)),
                        None => {
                            let used = w.mentions(i) || mv.mentions(i) || callee.footprint.iter().any(|f| f.param == i || f.lo.mentions(i) || f.hi.mentions(i));
                            if used {
                                return Err(Fail::Unknown(
                                    format!("argument {} to `{}` is not a size expression, and `{}`'s cost depends on it", i + 1, callee.name, callee.name),
                                    a.line,
                                ));
                            }
                        }
                    }
                }
                w = w.subst_many(&map);
                mv = mv.subst_many(&map);
                // the callee's footprint in this function's arrays (none is known of a declared callee)
                let feet: Vec<(LocalId, Poly, Poly, bool)> = callee.footprint.iter().filter(|_| !declared_only).filter_map(|f| {
                    let root = roots.get(f.param).copied().flatten()?;
                    Some((root, f.lo.subst_many(&map), f.hi.subst_many(&map), f.exact))
                }).collect();
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
                        self.bounds.push(Bound { moves: bd.moves.subst_many(&map), ..bd.clone() });
                    }
                    for n in &callee.notes {
                        let n2 = format!("in `{}`: {n}", callee.name);
                        if !self.notes.contains(&n2) { self.notes.push(n2); }
                    }
                }
                // after the call: what it leaves resident replaces what was
                self.resident.clear();
                if let (Some(cond), false) = (&callee.resident, declared_only) {
                    let cond = Cond { ws: cond.ws.subst_many(&map), fits: true };
                    for (root, lo, hi, exact) in feet { if exact { self.resident.push(Res { root, lo, hi, conds: vec![cond.clone()] }); } }
                }
                if !self.replay { self.work = self.work.add(&w); }
                self.call_moves = self.call_moves.add(&mv);
                if let Some(h) = self.has_call.last_mut() { *h = true; }
                Ok(())
            }
        }
    }
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
        ExprKind::Binary(_, a, b) | ExprKind::MinMax(_, a, b) => bound_mentions(a, l) || bound_mentions(b, l),
        ExprKind::Unary(_, a) | ExprKind::Cast(a, _) => bound_mentions(a, l),
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
        ExprKind::StructLit(_, vals) => for v in vals { collect_refs(v, out); },
        ExprKind::Binary(_, a, b) | ExprKind::MinMax(_, a, b) => { collect_refs(a, out); collect_refs(b, out); }
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

/// Every scalar local read in an expression tree, loop variables included (they have no alias).
fn collect_scalars(e: &Expr, out: &mut Vec<LocalId>) {
    match &e.kind {
        ExprKind::Local(v) => out.push(*v),
        ExprKind::Index(_, i) => collect_scalars(i, out),
        ExprKind::Binary(_, a, b) | ExprKind::MinMax(_, a, b) => { collect_scalars(a, out); collect_scalars(b, out); }
        ExprKind::Unary(_, a) | ExprKind::Cast(a, _) | ExprKind::Println(a) => collect_scalars(a, out),
        ExprKind::Call(_, args) => for a in args { collect_scalars(a, out); },
        ExprKind::Field(base, _) => collect_scalars(base, out),
        ExprKind::StructLit(_, vals) => for v in vals { collect_scalars(v, out); },
        ExprKind::If(c, t, els) => { collect_scalars(c, out); for b in std::iter::once(t).chain(els.iter()) { if let Some(x) = &b.tail { collect_scalars(x, out); } } }
        ExprKind::Block(b) => { if let Some(x) = &b.tail { collect_scalars(x, out); } }
        _ => {}
    }
}


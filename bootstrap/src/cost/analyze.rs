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
    Exact { work: Poly, moves: Poly },
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
    // a function with a recognised bound gets each rewrite tried on it and costed
    for i in 0..m.funcs.len() {
        if an.done[i].as_ref().is_some_and(|c| c.bounds.is_empty()) { continue; }
        let f = &m.funcs[i];
        let t = rewrite::tile_side(machine.m_bytes, 8);
        let mut sugg = Vec::new();
        if let Some(g) = rewrite::tile(f, t) {
            let r = Fa::new(&mut an, &g, None, vec![], None).run().result;
            sugg.push(Suggestion { label: format!("tile by {t}"), flag: format!("{}:tile", f.name), result: r });
        }
        if let Some(g) = rewrite::transpose(f) {
            let r = Fa::new(&mut an, &g, None, vec![], None).run().result;
            sugg.push(Suggestion { label: "transpose the column operand".into(), flag: format!("{}:transpose", f.name), result: r });
        }
        an.done[i].as_mut().unwrap().suggestions = sugg;
    }
    an.done.into_iter().map(|c| c.unwrap()).collect()
}

/// Apply `--apply` specs (`name:tile`, `name:transpose`) to a module before anything else sees it.
pub fn apply_rewrites(m: &mut Module, specs: &[String], machine: &Machine) -> Result<(), String> {
    for spec in specs {
        let Some((name, what)) = spec.split_once(':') else { return Err(format!("--apply takes `function:tile` or `function:transpose`, not `{spec}`")) };
        let Some(i) = m.funcs.iter().position(|f| f.name == name) else { return Err(format!("--apply: no function `{name}`")) };
        let g = match what {
            "tile" => rewrite::tile(&m.funcs[i], rewrite::tile_side(machine.m_bytes, 8)),
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
                });
            } else {
                self.active[fid] = true;
                let fc = Fa::new(self, f, None, vec![], Some(fid)).run();
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
#[derive(Debug, Clone, Default)]
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
    aff: Option<Affine>,
    es: i128,
    path: Vec<usize>,
}

/// What is remembered of a loop after it is popped.
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
}

struct Fa<'a, 'b, 'c> {
    an: &'b mut Analyzer<'a>,
    f: &'c Func,
    names: Vec<String>,
    sites: Vec<Site>,
    loop_recs: Vec<LoopRec>,
    bounds: Vec<Bound>,
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
    work: Poly,
    moves: Poly,
    saved: Vec<(Poly, Poly)>,
}

impl<'a, 'b, 'c> Fa<'a, 'b, 'c> {
    /// With `bindings`, the function is analysed for one call site: each parameter's size is
    /// the caller's polynomial rather than the parameter's own atom, so every fit and stride
    /// decision inside is made with the caller's numbers. The result is then already in the
    /// caller's atom space.
    fn new(an: &'b mut Analyzer<'a>, f: &'c Func, bindings: Option<&[Poly]>, inherited: Vec<Loop>, self_fid: Option<FuncId>) -> Self {
        // in a specialised analysis the polynomials live in the caller's atom space: fresh loop
        // atoms must be allocated above anything the bindings or inherited trips mention
        let mut names = param_names(f);
        let mut top = names.len();
        if let Some(b) = bindings { for p in b { if let Some(m) = p.max_var() { top = top.max(m + 1); } } }
        for l in &inherited { if let Some(m) = l.trip.max_var() { top = top.max(m + 1); } }
        while names.len() < top { names.push("?".into()); }
        let mut fa = Fa {
            an, f, names, sites: vec![], loop_recs: vec![], bounds: vec![], notes: vec![], io: false, self_fid, rec_calls: vec![],
            local_size: HashMap::new(), local_affine: HashMap::new(), initial: HashMap::new(), at_entry: false,
            loops: vec![], work: Poly::zero(), moves: Poly::zero(), saved: vec![],
        };
        // the caller's loops are entered like this function's own, so the body's cost is summed
        // over them when they are left at the end of `run`
        for l in inherited { fa.enter_loop(l); }
        for (i, &p) in f.params.iter().enumerate() {
            let l = &f.locals[p];
            let size = bindings.map_or_else(|| Poly::var(i), |b| b[i].clone());
            if l.ty.is_arrayish() {
                fa.local_size.insert(p, size);
            } else if l.ty == Ty::I64 {
                fa.local_affine.insert(p, Affine::constant(size));
            }
        }
        fa
    }

    fn run(mut self) -> FuncCost {
        let mut tier = "exact";
        let walked = self.block(&self.f.body);
        // leave the inherited loops: their frames sum the body over the caller's iterations
        while !self.loops.is_empty() { self.leave_loop(); }
        let result = match walked {
            Ok(()) => {
                self.settle_moves();
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
        // `#[cost(...)]`: every asserted bound must dominate what was inferred
        let mut violations = Vec::new();
        for (key, text, line, _) in &self.f.asserts {
            let which = match key.as_str() {
                "work_at_most" => "work",
                "moves_at_most" => "moves",
                other => { violations.push(format!("line {line}: unknown bound `{other}`; use `work_at_most` or `moves_at_most`")); continue; }
            };
            let asserted = match super::assert::parse(text, &self.names) {
                Ok(p) => p,
                Err(e) => { violations.push(format!("line {line}: in `{key} = \"{text}\"`: {e}")); continue; }
            };
            match &result {
                CostResult::Exact { work, moves } => {
                    let inferred = if which == "work" { work } else { moves };
                    if !super::assert::dominated(inferred, &asserted) {
                        violations.push(format!(
                            "line {line}: `{}` is asserted {which} at most {} but its {which} is {}",
                            self.f.name, asserted.display(&self.names), inferred.display(&self.names)));
                    }
                }
                CostResult::Unknown { reason, .. } => {
                    violations.push(format!("line {line}: `{}` asserts {which} at most {} but its cost is unknown: {reason}", self.f.name, asserted.display(&self.names)));
                }
            }
        }
        let effects = if self.io { vec!["io"] } else { vec![] };
        FuncCost { name: self.f.name.clone(), names: self.names, result, bounds: self.bounds, notes: self.notes, suggestions: vec![], effects, violations, tier }
    }

    /// The body's own cost `f` (self-calls charged nothing) and the self-calls make a
    /// recurrence in some measure `m` that every call shrinks: an `i64` parameter, `len − p`,
    /// or `hi − lo`. Shrinking by a constant with one call is a sum, `T = f·m/c`; with more
    /// calls it is exponential and refused. Shrinking by a factor `b` with `a` calls is the
    /// master theorem on the degree `d` of `f` in `m`.
    fn solve_recurrence(&self) -> Result<(Poly, Poly), String> {
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
            let solve = |f: &Poly| -> Result<Poly, String> {
                match shrink.unwrap() {
                    Shrink::Linear(c) => {
                        if a > 1 { return Err(format!("{a} recursive calls each shrinking `{}` by a constant: exponential", m.display(&self.names))); }
                        // T(m) = T(m − c) + f(m)  ⇒  at most (m/c + 1)·f(m)
                        Ok(f.mul(&m.scale(Rat::new(1, c))).add(f))
                    }
                    Shrink::Div(b) => {
                        let d = f.degree_in(&mvars);
                        let bd = (b as f64).powf(d.to_f64());
                        let af = a as f64;
                        if af < bd - 1e-9 {
                            // leaves are cheaper than the root: a geometric series
                            let ratio = Rat::new(1000, ((1.0 - af / bd) * 1000.0).round() as i128);
                            Ok(f.scale(ratio))
                        } else if (af - bd).abs() < 1e-9 {
                            Ok(f.mul(&Poly::atom(Atom::Log(Box::new(m.clone())))))
                        } else {
                            // T = Θ(m^log_b a): f's top part lifted from degree d to degree k
                            let k = af.ln() / (b as f64).ln();
                            let lift = k - d.to_f64();
                            let lifted = if (lift - lift.round()).abs() < 1e-9 {
                                m.pow(lift.round() as i128)
                            } else {
                                let e = Rat::new((lift * 1000.0).round() as i128, 1000);
                                let mut mono = super::size::Mono::default();
                                mono.factors.insert(Atom::Log(Box::new(Poly::zero())), Rat::zero()); // placeholder removed below
                                mono.factors.clear();
                                // a non-integer lift of a compound measure is not representable exactly; use m's variables
                                if mvars.len() == 1 { mono.factors.insert(Atom::Var(mvars[0]), e); let mut p = Poly::zero(); p.terms.insert(mono, Rat::one()); p }
                                else { return Err(format!("recurrence T = {a}·T(m/{b}) + Θ(m^{}) has a non-integer exponent on a compound measure", d.n)); }
                            };
                            let ratio = Rat::new(1000 * a, ((af - bd) * 1000.0).round() as i128);
                            Ok(f.leading().mul(&lifted).scale(ratio))
                        }
                    }
                }
            };
            return Ok((solve(&self.work)?, solve(&self.moves)?));
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
        self.work = self.work.add(&p);
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
        self.saved.push((std::mem::take(&mut self.work), std::mem::take(&mut self.moves)));
    }
    /// Leave a loop: sum its body frame over its variable into the frame below.
    fn leave_loop(&mut self) {
        let lp = self.loops.pop().unwrap();
        let rec = &self.loop_recs[lp.id];
        let (w, m) = (rec.sum(&self.work), rec.sum(&self.moves));
        let (pw, pm) = self.saved.pop().unwrap();
        self.work = pw.add(&w);
        self.moves = pm.add(&m);
    }
    /// Add a cost that was already summed over every enclosing loop (an inherited-context call).
    fn add_at_root(&mut self, w: &Poly, m: &Poly) {
        match self.saved.first_mut() {
            Some((rw, rm)) => { *rw = rw.add(w); *rm = rm.add(m); }
            None => { self.work = self.work.add(w); self.moves = self.moves.add(m); }
        }
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
                let d = pb.as_const()?;
                if d.is_zero() || !d.is_int() { return None; }
                Some(self.size_of(a, bound)?.scale(Rat::new(1, d.n)))
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
                let d = pb.konst.as_const()?;
                if d.is_zero() || !d.is_int() { return None; }
                Some(self.affine(a)?.scale(&Poly::from_rat(Rat::new(1, d.n))))
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

    fn elem_bytes(&self, l: LocalId) -> i128 {
        self.f.locals[l].ty.elem().map_or(8, |t| t.elem_bytes())
    }

    // ---- the moves rule ----

    /// Record an access site; its moves are settled with everyone else's at the end.
    fn access(&mut self, arr: LocalId, idx: &Expr) {
        let es = self.elem_bytes(arr);
        let aff = self.affine(idx);
        let path = self.loops.iter().map(|l| l.id).collect();
        self.sites.push(Site { aff, es, path });
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
        // lines[(site, loop)] = lines the site touches over one full run of that loop
        let mut lines: HashMap<(usize, usize), Poly> = HashMap::new();
        // whether the lines a site touches over a loop form one contiguous region
        let mut contig: HashMap<(usize, usize), bool> = HashMap::new();
        let inner = |lines: &HashMap<(usize, usize), Poly>, s: usize, path: &[usize], pos: usize| -> Poly {
            if pos + 1 < path.len() { lines[&(s, path[pos + 1])].clone() } else { Poly::constant(1) }
        };
        // loops in post-order: a loop's id is smaller than every loop nested in it, so
        // descending id processes inner loops first
        let mut ids: Vec<usize> = (0..self.loop_recs.len()).collect();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        for lid in ids {
            let members: Vec<(usize, usize)> = (0..nsites)
                .filter_map(|s| self.sites[s].path.iter().position(|&l| l == lid).map(|pos| (s, pos)))
                .collect();
            if members.is_empty() { continue; }
            let rec = &self.loop_recs[lid];
            let mut ws = Poly::zero();
            for &(s, pos) in &members {
                ws = ws.add(&inner(&lines, s, &self.sites[s].path, pos));
            }
            // the working set may vary with this loop's own variable (a triangular inner loop):
            // it fits when its largest value does, taken at the two ends of the range
            let ws_max: Option<f64> = match rec.atom {
                Some(a) if ws.mentions(a) => {
                    let last = rec.lo.add(&rec.trip.sub(&Poly::constant(1)).scale(Rat::int(rec.step)));
                    match (self.numeric(&ws.subst(a, &rec.lo)), self.numeric(&ws.subst(a, &last))) {
                        (Some(x), Some(y)) => Some(x.max(y)),
                        _ => None,
                    }
                }
                _ => self.numeric(&ws),
            };
            let fits = ws_max.is_some_and(|l| (l * m.b_bytes as f64) < (m.m_bytes as f64));
            for &(s, pos) in &members {
                let site = &self.sites[s];
                let in_lines = inner(&lines, s, &site.path, pos);
                let was_contig = if pos + 1 < site.path.len() { contig[&(s, site.path[pos + 1])] } else { true };
                // Σ over this loop of the inner lines, always eliminating this loop's atom
                let summed = rec.sum(&in_lines);
                let same_set = || if rec.atom.is_some_and(|a| in_lines.mentions(a)) { summed.clone() } else { in_lines.clone() };
                let (total, now_contig) = if !fits {
                    (summed.clone(), false)
                } else {
                    let stride = site.aff.as_ref().map(|a| rec.var.and_then(|v| a.coeffs.get(&v).cloned()).unwrap_or_else(Poly::zero).scale(Rat::int(site.es)));
                    match stride {
                        None => (summed.clone(), false),
                        // the same lines every iteration
                        Some(st) if st.is_zero() => (same_set(), was_contig),
                        Some(st) => match self.numeric(&st) {
                            // a whole line or more: fresh lines every iteration
                            Some(sb) if sb.abs() >= m.b_bytes as f64 => (summed.clone(), false),
                            Some(sb) => {
                                // the inner set slides by |s| bytes per iteration. A contiguous set
                                // (sequential inner loops) grows by the slide: in + Σ|s|/B. Separate
                                // lines (a strided inner loop) each slide: in × Σ|s|/B. Never less
                                // than one new line for the whole level.
                                let slide = rec.sum(&Poly::constant(1)).scale(Rat::new(sb.abs() as i128, 1)).mul_atom_pow(Atom::B, Rat::int(-1));
                                let slide = match self.numeric(&slide) { Some(v) if v < 1.0 => Poly::constant(1), _ => slide };
                                if was_contig { (same_set().add(&slide), true) } else { (same_set().mul(&slide), false) }
                            }
                            None => (summed.clone(), false),
                        },
                    }
                };
                lines.insert((s, lid), total);
                contig.insert((s, lid), now_contig);
            }
        }
        let mut total = Poly::zero();
        for (s, site) in self.sites.iter().enumerate() {
            let l = match site.path.first() { Some(&l0) => lines[&(s, l0)].clone(), None => Poly::constant(1) };
            total = total.add(&l);
        }
        self.moves = self.moves.add(&total.mul_atom_pow(Atom::B, Rat::int(1)));
    }

    /// The catalogue, entry one: is this statement a multiply-accumulate whose two indices form
    /// a contraction over the enclosing loops? Then the Hong–Kung bound applies to the whole nest.
    fn recognise(&mut self, s: &Stmt) {
        let Some(mac) = bounds::as_mac(s) else { return };
        let (Some(aa), Some(ab)) = (self.affine(mac.ia), self.affine(mac.ib)) else { return };
        let va: Vec<LocalId> = aa.coeffs.keys().copied().collect();
        let vb: Vec<LocalId> = ab.coeffs.keys().copied().collect();
        if !bounds::is_contraction(&va, &vb) { return; }
        let es = self.elem_bytes(mac.a);
        let n = self.times_here();
        let nest: Vec<LocalId> = self.loops.iter().filter_map(|l| l.var).collect();
        self.bounds.push(Bound {
            kind: "matrix product", citation: "Hong–Kung 1981",
            moves: bounds::matmul_bound(&n, es), line: mac.line, operands: [mac.a, mac.b], nest,
        });
        // what each operand does in the innermost loop
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

    /// A sequential pass over `n` elements of `es` bytes, once per enclosing iteration.
    fn stream(&mut self, n: &Poly, es: i128) {
        self.moves = self.moves.add(&n.scale(Rat::int(es)));
    }

    // ---- the walk ----

    fn block(&mut self, b: &Block) -> Result<(), Fail> {
        for s in &b.stmts { self.stmt(s)?; }
        if let Some(t) = &b.tail { self.expr(t)?; }
        Ok(())
    }

    fn stmt(&mut self, s: &Stmt) -> Result<(), Fail> {
        match s {
            Stmt::Let(id, e) => {
                self.expr(e)?;
                let l = &self.f.locals[*id];
                if l.ty.is_arrayish() {
                    let src = match &e.kind { ExprKind::Ref(s, _) | ExprKind::Local(s) => Some(*s), _ => None };
                    if let Some(sz) = src.and_then(|s| self.local_size.get(&s).cloned()) {
                        self.local_size.insert(*id, sz);
                    }
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
                let atom = self.new_atom(&self.f.locals[*var].name.clone());
                self.enter_loop(Loop { id: 0, var: Some(*var), atom: Some(atom), trip: size, lo: Poly::zero(), step: 1, offset: Some(Affine::constant(Poly::zero())) });
                self.add_work_n(3); // store, increment, compare-and-branch
                let r = self.block(body);
                self.leave_loop();
                r
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
                Ok(())
            }
            Stmt::LetArray(id, elems) => {
                for e in elems { self.expr(e)?; }
                let k = elems.len() as i128;
                self.add_work_n(k);
                let es = self.elem_bytes(*id);
                self.stream(&Poly::constant(k), es);
                self.local_size.insert(*id, Poly::constant(k));
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
                        self.access(*arr, idx);
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
                self.leave_loop();
                r
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
                self.leave_loop();
                r
            }
            Stmt::Expr(e) => self.expr(e),
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
                    Stmt::Assign(LValue::Var(_), _, _) | Stmt::Assign(LValue::Index(..), _, _) | Stmt::Let(..) | Stmt::LetArray(..) | Stmt::LetRepeat(..) | Stmt::LetBuild { .. } => {}
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
            ExprKind::Println(a) => { self.expr(a)?; self.add_work_n(1); self.io = true; Ok(()) }
            ExprKind::Index(arr, idx) => {
                self.expr(idx)?;
                self.add_work_n(1);
                self.access(*arr, idx);
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
                self.block(t)?;
                let then_calls: Vec<_> = self.rec_calls.drain(before..).collect();
                let then_init = std::mem::replace(&mut self.initial, entry_init);
                if let Some(b) = els { self.block(b)?; }
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
                let cf = &self.an.m.funcs[*fid];
                // every argument's size, when this call site knows them all — unless the callee
                // recurses, whose cost is a recurrence in its own parameters and must stay symbolic
                let bindings: Option<Vec<Poly>> = if calls_itself(cf, *fid) { None } else { args.iter().zip(&cf.params).map(|(a, &p)| {
                    if cf.locals[p].ty.is_arrayish() {
                        match &a.kind { ExprKind::Ref(s, _) | ExprKind::Local(s) => self.local_size.get(s).cloned(), _ => None }
                    } else if cf.locals[p].ty == Ty::I64 {
                        self.size_of(a, Dir::Upper)
                    } else {
                        Some(Poly::zero()) // not a size; never read
                    }
                }).collect() };
                if let Some(b) = bindings {
                    if self.an.active[*fid] {
                        return Err(Fail::Unknown(format!("calls `{}`, whose cost is unknown (mutually recursive with this function; only self-recursion is solved)", cf.name), e.line));
                    }
                    // the caller's loops go with the call when no argument depends on them,
                    // as loops without a variable: every access inside reuses across them if it fits
                    let invariant = args.iter().all(|a| matches!(a.kind, ExprKind::Ref(..)) || self.affine(a).is_some_and(|af| af.is_const()));
                    // an inherited loop's trip may mention this function's loop atoms; the callee
                    // is analysed in the same atom space, and its result is summed here as usual
                    let inherited: Vec<Loop> = if invariant {
                        self.loops.iter().map(|l| Loop { id: 0, var: None, atom: None, trip: l.trip.clone(), lo: Poly::zero(), step: 1, offset: None }).collect()
                    } else { vec![] };
                    let inherits = !inherited.is_empty();
                    self.an.active[*fid] = true;
                    let fc = Fa::new(self.an, cf, Some(&b), inherited, Some(*fid)).run();
                    self.an.active[*fid] = false;
                    if fc.effects.contains(&"io") { self.io = true; }
                    // a bound found in the callee counts once per time this call runs
                    let o = if inherits { Poly::constant(1) } else { self.times_here() };
                    for bd in fc.bounds {
                        self.bounds.push(Bound { moves: bd.moves.mul(&o), ..bd });
                    }
                    for n in fc.notes {
                        if !self.notes.contains(&n) { self.notes.push(format!("in `{}`: {n}", cf.name)); }
                    }
                    let (w, mv) = match fc.result {
                        CostResult::Exact { work, moves } => (work, moves),
                        CostResult::Unknown { reason, .. } => {
                            let reason = if reason.starts_with("calls `") { reason }
                                else { format!("calls `{}`, whose cost is unknown ({reason})", cf.name) };
                            return Err(Fail::Unknown(reason, e.line));
                        }
                    };
                    // an inherited context already summed the callee's cost over these loops
                    if inherits { self.add_at_root(&w, &mv); } else { self.work = self.work.add(&w); self.moves = self.moves.add(&mv); }
                    return Ok(());
                }
                // otherwise: the callee's symbolic cost, with the atoms it uses substituted
                let callee = self.an.func(*fid).clone();
                if callee.effects.contains(&"io") { self.io = true; }
                let (cw, cm) = match &callee.result {
                    CostResult::Exact { work, moves } => (work.clone(), moves.clone()),
                    CostResult::Unknown { reason, .. } => {
                        let reason = if reason.starts_with("calls `") { reason.clone() }
                            else { format!("calls `{}`, whose cost is unknown ({reason})", callee.name) };
                        return Err(Fail::Unknown(reason, e.line));
                    }
                };
                let mut w = cw;
                let mut mv = cm;
                let cf = &self.an.m.funcs[*fid];
                for (i, (a, &p)) in args.iter().zip(&cf.params).enumerate() {
                    let used = w.terms.keys().chain(mv.terms.keys()).any(|m| m.factors.contains_key(&Atom::Var(i)));
                    if !used { continue; }
                    let by = if cf.locals[p].ty.is_arrayish() {
                        match &a.kind { ExprKind::Ref(s, _) | ExprKind::Local(s) => self.local_size.get(s).cloned(), _ => None }
                    } else {
                        self.size_of(a, Dir::Upper)
                    };
                    let Some(by) = by else {
                        return Err(Fail::Unknown(
                            format!("argument {} to `{}` is not a size expression, and `{}`'s cost depends on it", i + 1, callee.name, callee.name),
                            a.line,
                        ));
                    };
                    w = w.subst(i, &by);
                    mv = mv.subst(i, &by);
                }
                self.work = self.work.add(&w);
                self.moves = self.moves.add(&mv);
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

/// Does `f`'s body call function `fid` — itself?
fn calls_itself(f: &Func, fid: FuncId) -> bool {
    fn in_block(b: &Block, fid: FuncId) -> bool {
        b.stmts.iter().any(|s| in_stmt(s, fid)) || b.tail.as_ref().is_some_and(|e| in_expr(e, fid))
    }
    fn in_stmt(s: &Stmt, fid: FuncId) -> bool {
        match s {
            Stmt::Let(_, e) | Stmt::Expr(e) | Stmt::Return(Some(e)) => in_expr(e, fid),
            Stmt::LetRepeat(_, a, b) => in_expr(a, fid) || in_expr(b, fid),
            Stmt::LetArray(_, es) => es.iter().any(|e| in_expr(e, fid)),
            Stmt::LetBuild { len, body, .. } => in_expr(len, fid) || in_block(body, fid),
            Stmt::Assign(lv, _, e) => in_expr(e, fid) || matches!(lv, LValue::Index(_, i, _) if in_expr(i, fid)),
            Stmt::For { start, end, body, .. } => in_expr(start, fid) || in_expr(end, fid) || in_block(body, fid),
            Stmt::While { cond, decreasing, body, .. } => in_expr(cond, fid) || decreasing.as_ref().is_some_and(|d| in_expr(d, fid)) || in_block(body, fid),
            Stmt::Break | Stmt::Return(None) => false,
        }
    }
    fn in_expr(e: &Expr, fid: FuncId) -> bool {
        match &e.kind {
            ExprKind::Call(f, args) => *f == fid || args.iter().any(|a| in_expr(a, fid)),
            ExprKind::Binary(_, a, b) | ExprKind::MinMax(_, a, b) => in_expr(a, fid) || in_expr(b, fid),
            ExprKind::Unary(_, a) | ExprKind::Cast(a, _) | ExprKind::Println(a) => in_expr(a, fid),
            ExprKind::Index(_, i) => in_expr(i, fid),
            ExprKind::If(c, t, els) => in_expr(c, fid) || in_block(t, fid) || els.as_ref().is_some_and(|b| in_block(b, fid)),
            ExprKind::Block(b) => in_block(b, fid),
            _ => false,
        }
    }
    in_block(&f.body, fid)
}

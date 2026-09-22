//! The cost calculus: work and moves for every function, in one walk over the typed IR.
//! The rules are specified in docs/cost-model.md; this file is their implementation and the
//! comments here say which rule each piece is.
//!
//! Work counts primitive operations. Moves counts bytes crossing the cache boundary in the
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
            let r = Fa::new(&mut an, &g, None, vec![]).run().result;
            sugg.push(Suggestion { label: format!("tile by {t}"), flag: format!("{}:tile", f.name), result: r });
        }
        if let Some(g) = rewrite::transpose(f) {
            let r = Fa::new(&mut an, &g, None, vec![]).run().result;
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
                    result: CostResult::Unknown { reason: "recursive; recurrences are not solved yet".into(), line: f.line },
                    bounds: vec![], notes: vec![], suggestions: vec![],
                });
            } else {
                self.active[fid] = true;
                let fc = Fa::new(self, f, None, vec![]).run();
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
    trip: Poly,
    start: Poly,
    end: Poly,
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
    trip: Poly,
}

struct Fa<'a, 'b, 'c> {
    an: &'b mut Analyzer<'a>,
    f: &'c Func,
    names: Vec<String>,
    sites: Vec<Site>,
    loop_recs: Vec<LoopRec>,
    bounds: Vec<Bound>,
    notes: Vec<String>,
    /// size in elements of every array/slice local
    local_size: HashMap<LocalId, Poly>,
    /// value of every immutable i64 local that is an affine expression
    local_affine: HashMap<LocalId, Affine>,
    loops: Vec<Loop>,
    work: Poly,
    moves: Poly,
}

impl<'a, 'b, 'c> Fa<'a, 'b, 'c> {
    /// With `bindings`, the function is analysed for one call site: each parameter's size is
    /// the caller's polynomial rather than the parameter's own atom, so every fit and stride
    /// decision inside is made with the caller's numbers. The result is then already in the
    /// caller's atom space.
    fn new(an: &'b mut Analyzer<'a>, f: &'c Func, bindings: Option<&[Poly]>, inherited: Vec<Loop>) -> Self {
        let mut loop_recs = Vec::new();
        let mut loops = Vec::new();
        for l in inherited {
            loop_recs.push(LoopRec { var: None, trip: l.trip.clone() });
            loops.push(Loop { id: loop_recs.len() - 1, ..l });
        }
        let mut fa = Fa {
            an, f, names: param_names(f), sites: vec![], loop_recs, bounds: vec![], notes: vec![],
            local_size: HashMap::new(), local_affine: HashMap::new(),
            loops, work: Poly::zero(), moves: Poly::zero(),
        };
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
        let result = match self.block(&self.f.body) {
            Ok(()) => {
                self.settle_moves();
                CostResult::Exact { work: self.work.clone(), moves: self.moves.clone() }
            }
            Err(Fail::Unknown(reason, line)) => CostResult::Unknown { reason, line },
        };
        FuncCost { name: self.f.name.clone(), names: self.names, result, bounds: self.bounds, notes: self.notes, suggestions: vec![] }
    }

    fn machine(&self) -> Machine { self.an.machine }

    /// Product of the trip counts of the enclosing loops: how many times the current point runs.
    fn outer(&self) -> Poly {
        self.loops.iter().fold(Poly::constant(1), |acc, l| acc.mul(&l.trip))
    }
    fn add_work(&mut self, p: Poly) {
        let o = self.outer();
        self.work = self.work.add(&p.mul(&o));
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
            Atom::Var(_) => None,
        })
    }

    // ---- size expressions ----

    /// An `i64` expression as a size polynomial: literals, size parameters, `.len()`, immutable
    /// lets bound to such, and `+ - *` of them. A loop variable is replaced by the bound of its
    /// range that maximises the result (`Upper`) or minimises it (`Lower`), so a triangular
    /// loop is bounded by its rectangular hull.
    fn size_of(&self, e: &Expr, bound: Dir) -> Option<Poly> {
        let flip = |b: Dir| if b == Dir::Upper { Dir::Lower } else { Dir::Upper };
        match &e.kind {
            ExprKind::Int(v) => Some(Poly::constant(*v as i128)),
            ExprKind::Local(l) => {
                if let Some(lp) = self.loops.iter().find(|lp| lp.var == Some(*l)) {
                    return Some(if bound == Dir::Upper { lp.end.clone() } else { lp.start.clone() });
                }
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
        match self.f.locals[l].ty.elem() {
            Some(Ty::Bool) => 1,
            _ => 8,
        }
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
            let mut ws = Poly::zero();
            for &(s, pos) in &members {
                ws = ws.add(&inner(&lines, s, &self.sites[s].path, pos));
            }
            let fits = self.numeric(&ws).is_some_and(|l| (l * m.b_bytes as f64) < (m.m_bytes as f64));
            let rec = &self.loop_recs[lid];
            for &(s, pos) in &members {
                let site = &self.sites[s];
                let in_lines = inner(&lines, s, &site.path, pos);
                let factor = if !fits {
                    rec.trip.clone()
                } else {
                    let stride = site.aff.as_ref().map(|a| rec.var.and_then(|v| a.coeffs.get(&v).cloned()).unwrap_or_else(Poly::zero).scale(Rat::int(site.es)));
                    match stride {
                        None => rec.trip.clone(),
                        Some(st) if st.is_zero() => Poly::constant(1),
                        Some(st) => match self.numeric(&st) {
                            Some(sb) if sb.abs() >= m.b_bytes as f64 => rec.trip.clone(),
                            Some(sb) => {
                                let f = rec.trip.scale(Rat::new(sb.abs() as i128, 1)).mul_atom_pow(Atom::B, Rat::int(-1));
                                match self.numeric(&f) { Some(v) if v < 1.0 => Poly::constant(1), _ => f }
                            }
                            None => rec.trip.clone(),
                        },
                    }
                };
                lines.insert((s, lid), in_lines.mul(&factor));
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
        let n = self.outer();
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
        let bytes = n.scale(Rat::int(es));
        let o = self.outer();
        self.moves = self.moves.add(&bytes.mul(&o));
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
                self.add_work_n(1);
                let l = &self.f.locals[*id];
                if l.ty.is_arrayish() {
                    let src = match &e.kind { ExprKind::Ref(s, _) | ExprKind::Local(s) => Some(*s), _ => None };
                    if let Some(sz) = src.and_then(|s| self.local_size.get(&s).cloned()) {
                        self.local_size.insert(*id, sz);
                    }
                } else if l.ty == Ty::I64 && !l.mutable {
                    if let Some(a) = self.affine(e) { self.local_affine.insert(*id, a); }
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
                self.loop_recs.push(LoopRec { var: Some(*var), trip: size.clone() });
                self.loops.push(Loop { id: self.loop_recs.len() - 1, var: Some(*var), trip: size.clone(), start: Poly::zero(), end: size, offset: Some(Affine::constant(Poly::zero())) });
                self.add_work_n(2);
                let r = self.block(body);
                self.loops.pop();
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
                self.add_work_n(if op.is_some() { 2 } else { 1 });
                if let LValue::Index(arr, idx, _) = lv {
                    self.expr(idx)?;
                    self.add_work_n(1);
                    self.access(*arr, idx);
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
                self.loop_recs.push(LoopRec { var: Some(*var), trip: trip.clone() });
                self.loops.push(Loop { id: self.loop_recs.len() - 1, var: Some(*var), trip, start: lo, end: hi, offset: a_lo });
                self.add_work_n(1); // increment and compare, per iteration
                let r = self.block(body);
                self.loops.pop();
                r
            }
            Stmt::Expr(e) => self.expr(e),
            Stmt::Return(Some(e)) => self.expr(e),
            Stmt::Return(None) => Ok(()),
        }
    }

    fn expr(&mut self, e: &Expr) -> Result<(), Fail> {
        match &e.kind {
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Local(_) | ExprKind::Ref(..) => Ok(()),
            ExprKind::Len(_) => { self.add_work_n(1); Ok(()) }
            ExprKind::Binary(_, a, b) | ExprKind::MinMax(_, a, b) => { self.expr(a)?; self.expr(b)?; self.add_work_n(1); Ok(()) }
            ExprKind::Unary(_, a) | ExprKind::Cast(a, _) => { self.expr(a)?; self.add_work_n(1); Ok(()) }
            ExprKind::Println(a) => { self.expr(a)?; self.add_work_n(1); Ok(()) }
            ExprKind::Index(arr, idx) => {
                self.expr(idx)?;
                self.add_work_n(1);
                self.access(*arr, idx);
                Ok(())
            }
            ExprKind::If(c, t, els) => {
                self.expr(c)?;
                self.add_work_n(1);
                // both branches are charged: an upper bound, tight when one is empty
                self.block(t)?;
                if let Some(b) = els { self.block(b)?; }
                Ok(())
            }
            ExprKind::Block(b) => self.block(b),
            ExprKind::Call(fid, args) => {
                for a in args { self.expr(a)?; }
                self.add_work_n(1);
                let cf = &self.an.m.funcs[*fid];
                // every argument's size, when this call site knows them all
                let bindings: Option<Vec<Poly>> = args.iter().zip(&cf.params).map(|(a, &p)| {
                    if cf.locals[p].ty.is_arrayish() {
                        match &a.kind { ExprKind::Ref(s, _) | ExprKind::Local(s) => self.local_size.get(s).cloned(), _ => None }
                    } else if cf.locals[p].ty == Ty::I64 {
                        self.size_of(a, Dir::Upper)
                    } else {
                        Some(Poly::zero()) // not a size; never read
                    }
                }).collect();
                if let Some(b) = bindings {
                    if self.an.active[*fid] {
                        return Err(Fail::Unknown(format!("calls `{}`, whose cost is unknown (recursive; recurrences are not solved yet)", cf.name), e.line));
                    }
                    // the caller's loops go with the call when no argument depends on them,
                    // as loops without a variable: every access inside reuses across them if it fits
                    let invariant = args.iter().all(|a| self.affine(a).is_none_or(|af| af.is_const()) || matches!(a.kind, ExprKind::Ref(..) | ExprKind::Local(_)));
                    let inherited: Vec<Loop> = if invariant {
                        self.loops.iter().map(|l| Loop { id: 0, var: None, trip: l.trip.clone(), start: l.start.clone(), end: l.end.clone(), offset: None }).collect()
                    } else { vec![] };
                    let inherits = !inherited.is_empty();
                    self.an.active[*fid] = true;
                    let fc = Fa::new(self.an, cf, Some(&b), inherited).run();
                    self.an.active[*fid] = false;
                    let o = if inherits { Poly::constant(1) } else { self.outer() };
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
                    // an inherited context already multiplied the callee's cost by the trips
                    self.work = self.work.add(&w.mul(&o));
                    self.moves = self.moves.add(&mv.mul(&o));
                    return Ok(());
                }
                // otherwise: the callee's symbolic cost, with the atoms it uses substituted
                let callee = self.an.func(*fid).clone();
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
                let o = self.outer();
                self.work = self.work.add(&w.mul(&o));
                self.moves = self.moves.add(&mv.mul(&o));
                Ok(())
            }
        }
    }
}

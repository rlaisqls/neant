//! Export one function as a polyhedral SCoP in C, the input IOLB (and PET) reads: loop nests,
//! affine indices, `#pragma scop` around the body. A flat row-major index `i·n + k` with a
//! parametric row length is not affine in the polyhedral sense — the product of a parameter and
//! an iterator is not linear — so such an array is **delinearised** into a two-dimensional VLA
//! parameter `double a[a_rows][n]` and the access into `a[i][k]`. Anything the polyhedral model
//! cannot take (a call, a `while`, a data-dependent index or condition) refuses the export with
//! the reason.

use std::collections::HashMap;
use std::fmt::Write;

use crate::ast::BinOp;
use crate::ir::*;

/// The exported C, and the assumptions under which it is the same computation as `f`: untiling
/// runs the inner loop over `0..n` rather than `0..T·(n/T)`, because a floor in a loop bound
/// makes IOLB's search run for hours, so the bound holds when `T | n`.
pub fn export(m: &Module, f: &Func) -> Result<(String, Vec<String>), String> {
    let body = f.body.as_ref().ok_or("an extern has no body to export")?;
    let mut ex = Ex { m, f, dims: HashMap::new(), loop_vars: vec![], loop_ids: vec![], consts: HashMap::new(), outer: HashMap::new(), inner: HashMap::new(), out: String::new(), indent: 1 };
    // first pass: how each array parameter is indexed, to choose its shape
    ex.scan_block(body)?;
    ex.find_tiles(body);
    // signature: scalars, then the sizes the export introduces, then the arrays (VLA parameters
    // may only mention what precedes them)
    let mut scalars: Vec<String> = Vec::new();
    let mut sizes: Vec<String> = Vec::new();
    let mut arrays: Vec<String> = Vec::new();
    for &p in &f.params {
        let l = &f.locals[p];
        match &l.ty {
            Ty::Slice(elem, _, _) => {
                let cty = c_ty(elem);
                match ex.dims.get(&p) {
                    Some(Some(row)) => { sizes.push(format!("long {}_rows", l.name)); arrays.push(format!("{cty} {}[{}_rows][{row}]", l.name, l.name)); }
                    _ => { sizes.push(format!("long {}_n", l.name)); arrays.push(format!("{cty} {}[{}_n]", l.name, l.name)); }
                }
            }
            t => scalars.push(format!("{} {}", c_ty(t), l.name)),
        }
    }
    let mut out = String::new();
    let _ = writeln!(out, "// exported by neant for IOLB: {}", f.name);
    let _ = writeln!(out, "void nt_{}({})", f.name, scalars.into_iter().chain(sizes).chain(arrays).collect::<Vec<_>>().join(", "));
    out.push_str("{\n");
    // locals other than loop variables, which the `for` declares
    for (i, l) in f.locals.iter().enumerate() {
        if f.params.contains(&i) || ex.loop_ids.contains(&i) || ex.consts.contains_key(&i) || l.ty.is_arrayish() || l.ty == Ty::Unit { continue; }
        let _ = writeln!(out, "  {} {};", c_ty(&l.ty), ex.name(i));
    }
    out.push_str("#pragma scop\n");
    ex.block(body)?;
    out.push_str(&ex.out);
    out.push_str("#pragma endscop\n}\n");
    let mut assumptions: Vec<String> = ex.outer.values().map(|(t, n)| format!("{t} | {}", ex.expr(n).trim_matches(|c| c == '(' || c == ')'))).collect();
    assumptions.sort();
    assumptions.dedup();
    Ok((out, assumptions))
}

fn c_ty(t: &Ty) -> &'static str {
    match t { Ty::I64 => "long", Ty::F64 => "double", Ty::Bool => "int", Ty::U8 => "unsigned char", Ty::Unit => "void", Ty::Array(e, _) | Ty::Slice(e, _, _) => c_ty(e) }
}

struct Ex<'a> {
    m: &'a Module,
    f: &'a Func,
    /// array param → Some(row length expression) when every index is `v·row + w`, None when 1-D
    dims: HashMap<LocalId, Option<String>>,
    loop_vars: Vec<LocalId>,
    /// every loop variable seen, for the declarations
    loop_ids: Vec<LocalId>,
    /// immutable scalars bound to a literal, written as the literal: a tile side `let t = 64`
    /// must be a constant for the loop bounds to be affine
    consts: HashMap<LocalId, String>,
    /// **Untiling.** A lower bound is a property of the computation, not of the loop order, and
    /// IOLB does not see through a tiled nest. A tile-outer loop `for ii in 0..n/T` whose variable
    /// appears only in the bounds of one inner loop `for i in ii·T..ii·T+T` is dropped, and the
    /// inner loop runs `0..T·(n/T)` — the same iterations, in one loop. `outer[ii] = (T, n)`;
    /// `inner[i] = ii`.
    outer: HashMap<LocalId, (String, Expr)>,
    inner: HashMap<LocalId, LocalId>,
    out: String,
    indent: usize,
}

impl<'a> Ex<'a> {
    fn name(&self, l: LocalId) -> String { format!("{}_{}", self.f.locals[l].name.replace('#', "_"), l) }
    fn line(&mut self, s: &str) { for _ in 0..self.indent { self.out.push_str("  "); } self.out.push_str(s); self.out.push('\n'); }

    // ---- shapes ----
    fn scan_block(&mut self, b: &Block) -> Result<(), String> {
        for s in &b.stmts { self.scan_stmt(s)?; }
        if let Some(t) = &b.tail { self.scan_expr(t)?; }
        Ok(())
    }
    fn scan_stmt(&mut self, s: &Stmt) -> Result<(), String> {
        match s {
            Stmt::Let(id, e) => {
                if !self.f.locals[*id].mutable && self.loop_vars.is_empty() {
                    if let ExprKind::Int(v) = e.kind { self.consts.insert(*id, v.to_string()); }
                }
                self.scan_expr(e)
            }
            Stmt::Expr(e) | Stmt::Return(Some(e)) => self.scan_expr(e),
            Stmt::Assign(lv, _, e) => { if let LValue::Index(a, i, _) = lv { self.note(*a, i)?; self.scan_expr(i)?; } self.scan_expr(e) }
            Stmt::For { var, start, end, body } => { self.scan_expr(start)?; self.scan_expr(end)?; self.loop_ids.push(*var); self.loop_vars.push(*var); let r = self.scan_block(body); self.loop_vars.pop(); r }
            Stmt::While { line, .. } => Err(format!("a `while` (line {line}) is not static control; the polyhedral model needs `for` with affine bounds")),
            Stmt::LetRepeat(..) | Stmt::LetArray(..) | Stmt::LetBuild { .. } => Err("an array born inside the function is not part of a SCoP".into()),
            Stmt::Break => Err("`break` is not static control".into()),
            Stmt::Return(None) => Ok(()),
        }
    }
    fn scan_expr(&mut self, e: &Expr) -> Result<(), String> {
        match &e.kind {
            ExprKind::Index(a, i) => { self.note(*a, i)?; self.scan_expr(i) }
            ExprKind::Binary(_, x, y) | ExprKind::MinMax(_, x, y) => { self.scan_expr(x)?; self.scan_expr(y) }
            ExprKind::Unary(_, x) | ExprKind::Cast(x, _) => self.scan_expr(x),
            ExprKind::Call(fid, _) => Err(format!("a call to `{}` is not part of a SCoP", self.m.funcs[*fid].name)),
            ExprKind::If(c, t, els) => { self.scan_expr(c)?; self.scan_block(t)?; if let Some(b) = els { self.scan_block(b)?; } Ok(()) }
            ExprKind::Block(b) => self.scan_block(b),
            ExprKind::Println(_) => Err("`println` is not part of a SCoP".into()),
            _ => Ok(()),
        }
    }
    /// Record the shape an index implies for its array: `v·row + w` → 2-D with that row length.
    fn note(&mut self, arr: LocalId, idx: &Expr) -> Result<(), String> {
        let root = arr;
        if !self.f.params.contains(&root) { return Err(format!("`{}` is not a parameter; only parameter arrays are exported", self.f.locals[root].name)); }
        let shape = match self.split_2d(idx) { Some((row, _, _)) => Some(row), None => None };
        match self.dims.get(&root) {
            None => { self.dims.insert(root, shape); }
            Some(prev) => if *prev != shape { return Err(format!("`{}` is indexed both as one- and two-dimensional; the export cannot choose a shape", self.f.locals[root].name)); }
        }
        Ok(())
    }
    /// `v·row + w` or `row·v + w` (either order of the sum) with `v` a loop variable and `row`
    /// free of loop variables.
    fn split_2d(&self, idx: &Expr) -> Option<(String, Expr, Expr)> {
        let ExprKind::Binary(BinOp::Add, a, b) = &idx.kind else { return None };
        for (prod, rest) in [(a, b), (b, a)] {
            if let ExprKind::Binary(BinOp::Mul, x, y) = &prod.kind {
                for (v, row) in [(x, y), (y, x)] {
                    if let ExprKind::Local(l) = &v.kind {
                        if self.loop_vars.contains(l) && !self.mentions_loop_var(row) {
                            return Some((self.expr(row), (**v).clone(), (**rest).clone()));
                        }
                    }
                }
            }
        }
        None
    }
    fn mentions_loop_var(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Local(l) => self.loop_vars.contains(l),
            ExprKind::Binary(_, a, b) | ExprKind::MinMax(_, a, b) => self.mentions_loop_var(a) || self.mentions_loop_var(b),
            ExprKind::Unary(_, a) | ExprKind::Cast(a, _) => self.mentions_loop_var(a),
            _ => false,
        }
    }

    // ---- untiling ----
    fn find_tiles(&mut self, b: &Block) {
        let mut cands: Vec<(LocalId, String, Expr)> = Vec::new();
        self.tile_outer_loops(b, &mut cands);
        for (ii, t, n) in cands {
            // exactly one inner loop `ii·t .. ii·t + t`, and no other use of ii anywhere
            let mut inners: Vec<LocalId> = Vec::new();
            let mut other_uses = 0usize;
            self.tile_inner_loops(b, ii, &t, &mut inners, &mut other_uses);
            if inners.len() == 1 && other_uses == 0 {
                self.inner.insert(inners[0], ii);
                self.outer.insert(ii, (t, n));
            }
        }
    }
    /// loops `for ii in 0..n/T` with `T` a literal or a constant
    fn tile_outer_loops(&self, b: &Block, out: &mut Vec<(LocalId, String, Expr)>) {
        for st in &b.stmts {
            if let Stmt::For { var, start, end, body } = st {
                if matches!(start.kind, ExprKind::Int(0)) {
                    if let ExprKind::Binary(BinOp::Div, n, t) = &end.kind {
                        if let Some(t) = self.literal(t) { out.push((*var, t, (**n).clone())); }
                    }
                }
                self.tile_outer_loops(body, out);
            }
        }
    }
    fn literal(&self, e: &Expr) -> Option<String> {
        match &e.kind { ExprKind::Int(v) => Some(v.to_string()), ExprKind::Local(l) => self.consts.get(l).cloned(), _ => None }
    }
    /// inner loops `for i in ii·T .. ii·T + T` (or `.. min(ii·T + T, n)`), and every other mention of ii
    fn tile_inner_loops(&self, b: &Block, ii: LocalId, t: &str, inners: &mut Vec<LocalId>, other: &mut usize) {
        for st in &b.stmts {
            match st {
                Stmt::For { var, start, end, body } => {
                    let is_start = self.is_mul(start, ii, t);
                    let end = if let ExprKind::MinMax(true, a, _) = &end.kind { a } else { end };
                    let is_end = matches!(&end.kind, ExprKind::Binary(BinOp::Add, a, tt) if self.is_mul(a, ii, t) && self.literal(tt).as_deref() == Some(t));
                    if is_start && is_end { inners.push(*var); } else { *other += self.mentions(start, ii) as usize + self.mentions(end, ii) as usize; }
                    self.tile_inner_loops(body, ii, t, inners, other);
                }
                Stmt::Let(_, e) | Stmt::Expr(e) => *other += self.mentions(e, ii) as usize,
                Stmt::Assign(lv, _, e) => { *other += self.mentions(e, ii) as usize; if let LValue::Index(_, i, _) = lv { *other += self.mentions(i, ii) as usize; } }
                _ => *other += 1,
            }
        }
        if let Some(tail) = &b.tail { *other += self.mentions(tail, ii) as usize; }
    }
    fn is_mul(&self, e: &Expr, ii: LocalId, t: &str) -> bool {
        if let ExprKind::Binary(BinOp::Mul, a, b) = &e.kind {
            for (x, y) in [(a, b), (b, a)] {
                if matches!(x.kind, ExprKind::Local(l) if l == ii) && self.literal(y).as_deref() == Some(t) { return true; }
            }
        }
        false
    }
    fn mentions(&self, e: &Expr, v: LocalId) -> bool {
        match &e.kind {
            ExprKind::Local(l) => *l == v,
            ExprKind::Binary(_, a, b) | ExprKind::MinMax(_, a, b) => self.mentions(a, v) || self.mentions(b, v),
            ExprKind::Unary(_, a) | ExprKind::Cast(a, _) | ExprKind::Println(a) => self.mentions(a, v),
            ExprKind::Index(_, i) => self.mentions(i, v),
            ExprKind::Call(_, args) => args.iter().any(|a| self.mentions(a, v)),
            ExprKind::If(c, t, e2) => self.mentions(c, v) || t.tail.as_ref().is_some_and(|x| self.mentions(x, v)) || e2.as_ref().and_then(|b| b.tail.as_ref()).is_some_and(|x| self.mentions(x, v)),
            ExprKind::Block(b) => b.tail.as_ref().is_some_and(|x| self.mentions(x, v)),
            _ => false,
        }
    }

    // ---- emission ----
    fn block(&mut self, b: &Block) -> Result<(), String> {
        for s in &b.stmts { self.stmt(s)?; }
        // a tail that only reads a local is the return value, not a statement
        if let Some(t) = &b.tail { if !matches!(t.kind, ExprKind::Local(_) | ExprKind::Int(_) | ExprKind::Float(_)) { let v = self.expr(t); self.line(&format!("(void)({v});")); } }
        Ok(())
    }
    fn stmt(&mut self, s: &Stmt) -> Result<(), String> {
        match s {
            Stmt::Let(id, _) if self.consts.contains_key(id) => Ok(()),
            Stmt::Let(id, e) => { let v = self.expr(e); let n = self.name(*id); self.line(&format!("{n} = {v};")); Ok(()) }
            Stmt::Assign(lv, op, e) => {
                let v = self.expr(e);
                let o = op.map_or(String::new(), |o| o.c_str().to_string());
                let target = match lv { LValue::Var(l) => self.name(*l), LValue::Index(a, i, _) => self.index(*a, i) };
                self.line(&format!("{target} {o}= {v};"));
                Ok(())
            }
            Stmt::For { var, .. } if self.outer.contains_key(var) => {
                // the tile-outer loop: its body runs once, the inner loop covers the range
                let Stmt::For { body, .. } = s else { unreachable!() };
                self.loop_vars.push(*var);
                let r = self.block(body);
                self.loop_vars.pop();
                r
            }
            Stmt::For { var, start, end, body } => {
                let (lo, hi) = match self.inner.get(var).and_then(|ii| self.outer.get(ii)) {
                    Some((_, n)) => ("0".to_string(), self.expr(n)),
                    None => (self.expr(start), self.expr(end)),
                };
                let v = self.name(*var);
                self.line(&format!("for (long {v} = {lo}; {v} < {hi}; {v}++) {{"));
                self.indent += 1;
                self.loop_vars.push(*var);
                let r = self.block(body);
                self.loop_vars.pop();
                self.indent -= 1;
                self.line("}");
                r
            }
            Stmt::Expr(e) => { let v = self.expr(e); self.line(&format!("(void)({v});")); Ok(()) }
            Stmt::Return(_) => Err("`return` inside a SCoP".into()),
            other => Err(format!("statement not exportable: {other:?}")),
        }
    }
    fn index(&self, arr: LocalId, idx: &Expr) -> String {
        let name = self.f.locals[arr].name.clone();
        match self.dims.get(&arr) {
            Some(Some(_)) => match self.split_2d(idx) {
                Some((_, v, rest)) => format!("{name}[{}][{}]", self.expr(&v), self.expr(&rest)),
                None => format!("{name}[0][{}]", self.expr(idx)),
            },
            _ => format!("{name}[{}]", self.expr(idx)),
        }
    }
    fn expr(&self, e: &Expr) -> String {
        match &e.kind {
            ExprKind::Int(v) => v.to_string(),
            ExprKind::Float(v) => format!("{v:?}"),
            ExprKind::Bool(v) => if *v { "1".into() } else { "0".into() },
            ExprKind::Byte(v) => v.to_string(),
            ExprKind::Local(l) => match self.consts.get(l) {
                Some(v) => v.clone(),
                None => if self.f.params.contains(l) { self.f.locals[*l].name.clone() } else { self.name(*l) },
            },
            ExprKind::Binary(op, a, b) => format!("({} {} {})", self.expr(a), op.c_str(), self.expr(b)),
            ExprKind::Unary(crate::ast::UnOp::Neg, a) => format!("(-{})", self.expr(a)),
            ExprKind::Unary(crate::ast::UnOp::Not, a) => format!("(!{})", self.expr(a)),
            ExprKind::Cast(a, t) => format!("(({})({}))", c_ty(t), self.expr(a)),
            ExprKind::Index(a, i) => self.index(*a, i),
            ExprKind::Len(a) => format!("{}_n", self.f.locals[*a].name),
            ExprKind::MinMax(is_min, a, b) => format!("({} {} {} ? {} : {})", self.expr(a), if *is_min { "<" } else { ">" }, self.expr(b), self.expr(a), self.expr(b)),
            ExprKind::If(c, t, els) => format!("({} ? {} : {})", self.expr(c), t.tail.as_ref().map_or("0".to_string(), |x| self.expr(x)), els.as_ref().and_then(|b| b.tail.as_ref()).map_or("0".to_string(), |x| self.expr(x))),
            ExprKind::Block(b) => b.tail.as_ref().map_or("0".to_string(), |x| self.expr(x)),
            ExprKind::Ref(a, _) => self.f.locals[*a].name.clone(),
            ExprKind::Call(..) | ExprKind::Println(_) => "0".into(),
        }
    }
}

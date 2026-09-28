//! Recursion over a tree in an arena, across functions (docs/cost-model.md § Recursion, a forest).
//!
//! A component of the call graph whose members call one another on the nodes of one array: each
//! member has an `i64` parameter that is its node and an array parameter that is the arena, and
//! every call inside the component hands on the arena and, for the node, the caller's own node, a
//! child of it (`xs[t].f`, or `xs[t]` of an `[i64]`), or the variable of a walk down a list
//! (`while s >= 0 { …; s = xs[s].f }`, read as the recursion `W(s) = body(s) + W(xs[s].f)`).
//!
//! An invocation and the calls it makes on its own node, and theirs, is a **group**. The shape
//! found here is what bounds the groups: along any one path through a group, every call down a
//! link goes down a different link, and no chain of calls on the same node comes back to where it
//! started. Then, over an arena that is a tree, each node has one link in and so is entered by at
//! most one group — the one on its parent — and every group but the first is entered from a group
//! on a node of the array: at most `L·xs.len() + 1` groups, `L` the most links one group goes
//! down, each of at most `m_G` invocations of each member `G`.

use std::collections::HashMap;

use crate::ast::BinOp;
use crate::ir::*;

/// Where a call goes: a member, or the `k`th walk loop of one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Node { F(FuncId), W(FuncId, usize) }

impl Node {
    fn owner(self) -> FuncId { match self { Node::F(f) | Node::W(f, _) => f } }
}

/// The links a call goes down from its caller's node, one a step: `Some(f)` the field `f` of
/// `xs[t]`, `None` the element `xs[t]` of an `[i64]`; none at all is the node itself.
type Link = Vec<Option<usize>>;

#[derive(Clone, Default)]
struct Path { calls: Vec<(Node, Link)>, stop: bool }

/// A member parameter's value, in the start's parameters: not yet seen, the start's `i`th, or not
/// one value (two calls hand it different things, or something that is not a parameter).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum From { Unset, Param(usize), Bad }

impl From {
    fn join(self, o: From) -> From {
        match (self, o) {
            (From::Unset, x) | (x, From::Unset) => x,
            (From::Param(a), From::Param(b)) if a == b => From::Param(a),
            _ => From::Bad,
        }
    }
}

pub struct Shape {
    /// each member's (node, arena) parameter positions
    pub node: HashMap<FuncId, (usize, usize)>,
    /// each member parameter as the start's
    pub map: HashMap<FuncId, Vec<From>>,
    /// the most links one group goes down
    pub links: usize,
    /// the most invocations of each member in one group, its walks' laps counted to it
    pub mult: HashMap<FuncId, usize>,
}

const CAP: usize = 4096;

struct Walker<'a> {
    m: &'a Module,
    members: &'a [FuncId],
    node: HashMap<FuncId, (usize, usize)>,
    queue: Vec<FuncId>,
    paths: HashMap<Node, Vec<Path>>,
    /// (caller, callee, for each callee parameter the caller parameter handed to it unchanged)
    hands: Vec<(FuncId, FuncId, Vec<Option<usize>>)>,
    /// the walk loops found in each member, numbered in the order met
    walks: HashMap<FuncId, usize>,
    ok: bool,
    /// an immutable `i64` bound to a node below another: `let c = xs[t].f`
    alias: HashMap<LocalId, (LocalId, Link)>,
}

/// Where the component `members` (every function that reaches `start` and is reached by it,
/// `start` included) is a recursion over a tree in one arena, its shape; `None` when it is not.
pub fn shape(m: &Module, start: FuncId, members: &[FuncId]) -> Option<Shape> {
    let f = &m.funcs[start];
    for (pi, &p) in f.params.iter().enumerate() {
        if f.locals[p].ty != Ty::I64 { continue; }
        for (ai, &a) in f.params.iter().enumerate() {
            if !f.locals[a].ty.is_arrayish() { continue; }
            if let Some(s) = try_shape(m, start, members, pi, ai) { return Some(s); }
        }
    }
    None
}

fn try_shape(m: &Module, start: FuncId, members: &[FuncId], pi: usize, ai: usize) -> Option<Shape> {
    let mut w = Walker { m, members, node: HashMap::new(), queue: vec![start], paths: HashMap::new(), hands: vec![], walks: HashMap::new(), ok: true, alias: HashMap::new() };
    w.node.insert(start, (pi, ai));
    while let Some(g) = w.queue.pop() {
        if w.paths.contains_key(&Node::F(g)) { continue; }
        let gf = &m.funcs[g];
        let Some(body) = &gf.body else { return None };
        let (p, a) = w.node[&g];
        let mut st = vec![Path::default()];
        w.alias.clear();
        w.block(g, Node::F(g), gf.params[p], gf.params[a], body, &mut st);
        if !w.ok { if std::env::var("NEANT_DEBUG_FOREST").is_ok() { eprintln!("  in {}", gf.name); } return None; }
        w.paths.insert(Node::F(g), st);
    }
    // every member is reached from the start, or it is not this shape
    if members.iter().any(|g| !w.paths.contains_key(&Node::F(*g))) { if std::env::var("NEANT_DEBUG_FOREST").is_ok() { eprintln!("forest: a member not reached"); } return None; }
    // no chain of calls on the same node returns to where it began
    let mut state: HashMap<Node, u8> = HashMap::new();
    let nodes: Vec<Node> = w.paths.keys().copied().collect();
    for &n in &nodes { if !acyclic(n, &w.paths, &mut state) { if std::env::var("NEANT_DEBUG_FOREST").is_ok() { eprintln!("forest: a cycle on one node"); } return None; } }
    // every group: each link at most once along any path through it
    let mut memo: HashMap<Node, Vec<(Vec<Link>, HashMap<FuncId, usize>)>> = HashMap::new();
    let mut links = 0;
    let mut mult: HashMap<FuncId, usize> = HashMap::new();
    for &n in &nodes {
        let Some(cl) = closure(n, &w.paths, &mut memo) else { if std::env::var("NEANT_DEBUG_FOREST").is_ok() { eprintln!("forest: too many paths"); } return None };
        for (labels, ms) in cl {
            // no node entered twice: no path of links a prefix of another, the same one included
            let prefix = |a: &Link, b: &Link| a.len() <= b.len() && b[..a.len()] == a[..];
            if (0..labels.len()).any(|i| (0..i).any(|j| prefix(&labels[i], &labels[j]) || prefix(&labels[j], &labels[i]))) { if std::env::var("NEANT_DEBUG_FOREST").is_ok() { eprintln!("forest: a link twice from {n:?}: {labels:?}"); } return None; }
            links = links.max(labels.len());
            for (g, c) in ms { let e = mult.entry(g).or_insert(0); *e = (*e).max(c); }
        }
    }
    // each member parameter as the start's, through the calls that hand it on
    let mut map: HashMap<FuncId, Vec<From>> = members.iter().map(|&g| (g, vec![From::Unset; m.funcs[g].params.len()])).collect();
    map.insert(start, (0..f_params(m, start)).map(From::Param).collect());
    loop {
        let mut changed = false;
        for (caller, callee, hand) in &w.hands {
            let from: Vec<From> = hand.iter().map(|h| match h { Some(q) => map[caller][*q], None => From::Bad }).collect();
            let row = map.get_mut(callee).unwrap();
            for (j, v) in from.into_iter().enumerate() {
                let nv = row[j].join(v);
                if nv != row[j] { row[j] = nv; changed = true; }
            }
        }
        if !changed { break; }
    }
    Some(Shape { node: w.node, map, links, mult })
}

fn f_params(m: &Module, f: FuncId) -> usize { m.funcs[f].params.len() }

fn acyclic(n: Node, paths: &HashMap<Node, Vec<Path>>, state: &mut HashMap<Node, u8>) -> bool {
    match state.get(&n) { Some(1) => return false, Some(2) => return true, _ => {} }
    state.insert(n, 1);
    for p in &paths[&n] {
        for (t, l) in &p.calls {
            if l.is_empty() && !acyclic(*t, paths, state) { return false; }
        }
    }
    state.insert(n, 2);
    true
}

/// Every way a group begun at `n` can go: the links it goes down, and how many invocations of
/// each member it makes. `None` past `CAP` of them.
#[allow(clippy::type_complexity)]
fn closure(n: Node, paths: &HashMap<Node, Vec<Path>>, memo: &mut HashMap<Node, Vec<(Vec<Link>, HashMap<FuncId, usize>)>>) -> Option<Vec<(Vec<Link>, HashMap<FuncId, usize>)>> {
    if let Some(c) = memo.get(&n) { return Some(c.clone()); }
    let mut out = Vec::new();
    for p in &paths[&n] {
        let mut here = HashMap::new();
        // a walk's lap is paid inside its owner's cost, so it counts as one of the owner's
        here.insert(n.owner(), 1usize);
        let mut combos = vec![(Vec::new(), here)];
        for (t, l) in &p.calls {
            match l.is_empty() {
                false => for c in &mut combos { c.0.push(l.clone()); },
                true => {
                    let sub = closure(*t, paths, memo)?;
                    let mut next = Vec::new();
                    for (cl, cm) in &combos {
                        for (sl, sm) in &sub {
                            let mut l2 = cl.clone();
                            l2.extend(sl.iter().cloned());
                            let mut m2 = cm.clone();
                            for (g, c) in sm { *m2.entry(*g).or_insert(0) += c; }
                            next.push((l2, m2));
                            if next.len() > CAP { return None; }
                        }
                    }
                    combos = next;
                }
            }
        }
        out.extend(combos);
        if out.len() > CAP { return None; }
    }
    memo.insert(n, out.clone());
    Some(out)
}

impl<'a> Walker<'a> {
    fn fail(&mut self, k: u32) {
        if self.ok && std::env::var("NEANT_DEBUG_FOREST").is_ok() { eprintln!("forest: refused at {k}"); }
        self.ok = false;
    }
    fn member(&self, g: FuncId) -> bool { self.members.contains(&g) }

    fn calls_member_block(&self, b: &Block) -> bool {
        let mut out = Vec::new();
        super::analyze::callees_block_pub(b, &mut out);
        out.iter().any(|g| self.member(*g))
    }
    fn calls_member_expr(&self, e: &Expr) -> bool {
        self.calls_member_block(&Block { stmts: vec![Stmt::Expr(e.clone())], tail: None, ty: Ty::Unit })
    }

    fn block(&mut self, owner: FuncId, cur: Node, t: LocalId, arena: LocalId, b: &Block, st: &mut Vec<Path>) {
        for (k, s) in b.stmts.iter().enumerate() {
            if !self.ok { return; }
            self.stmt(owner, cur, t, arena, &b.stmts[..k], s, st);
        }
        if let Some(e) = &b.tail { self.expr(owner, cur, t, arena, e, st); }
    }

    #[allow(clippy::too_many_arguments)]
    fn stmt(&mut self, owner: FuncId, cur: Node, t: LocalId, arena: LocalId, before: &[Stmt], s: &Stmt, st: &mut Vec<Path>) {
        match s {
            Stmt::Let(x, e) => {
                self.expr(owner, cur, t, arena, e, st);
                let l = &self.m.funcs[owner].locals[*x];
                if !l.mutable && l.ty == Ty::I64 {
                    if let Some(path) = route(e, t, arena, &self.alias) { if !path.is_empty() { self.alias.insert(*x, (t, path)); } }
                }
            }
            Stmt::Expr(e) => self.expr(owner, cur, t, arena, e, st),
            Stmt::LetRepeat(_, e, n) => { self.expr(owner, cur, t, arena, e, st); self.expr(owner, cur, t, arena, n, st); }
            Stmt::LetArray(_, es) => for e in es { self.expr(owner, cur, t, arena, e, st); },
            Stmt::Assign(lv, _, e) => {
                match lv {
                    LValue::Index(_, i, _) | LValue::IndexField(_, i, _, _) | LValue::FieldIndex(_, i, _, _) => self.expr(owner, cur, t, arena, i, st),
                    _ => {}
                }
                self.expr(owner, cur, t, arena, e, st);
            }
            Stmt::Reassign(_) => {}
            Stmt::Return(e) => {
                if let Some(e) = e { self.expr(owner, cur, t, arena, e, st); }
                for p in st.iter_mut() { p.stop = true; }
            }
            Stmt::Break => for p in st.iter_mut() { p.stop = true; },
            Stmt::For { start, end, body, .. } => {
                if self.calls_member_expr(start) || self.calls_member_expr(end) || self.calls_member_block(body) { self.fail(1); }
            }
            Stmt::ParFor { end, body, .. } => {
                if self.calls_member_expr(end) || self.calls_member_block(body) { self.fail(2); }
            }
            Stmt::LetBuild { len, body, .. } => {
                if self.calls_member_expr(len) || self.calls_member_block(body) { self.fail(3); }
            }
            Stmt::While { cond, body, line, .. } => {
                if self.calls_member_expr(cond) { self.fail(4); return; }
                if !self.calls_member_block(body) { return; }
                // a walk down a list in the arena, from this node or a child of it
                let Some((v, step)) = walk_of(cond, body, arena, &self.m.funcs[owner]) else {
                    if std::env::var("NEANT_DEBUG_FOREST").is_ok() && self.ok { eprintln!("forest: while in {} line {line} not a walk (arena {})", self.m.funcs[owner].name, self.m.funcs[owner].locals[arena].name); }
                    self.fail(5); return };
                let Some(init) = entry_of(before, v) else { self.fail(6); return };
                let Some(from) = route(init, t, arena, &self.alias) else { self.fail(7); return };
                let k = { let e = self.walks.entry(owner).or_insert(0); *e += 1; *e - 1 };
                let wn = Node::W(owner, k);
                push(st, (wn, from));
                let mut ws = vec![Path::default()];
                // inside, the node is the walk's variable; nothing may be called on the outer one,
                // which each lap would enter again
                self.block(owner, wn, v, arena, body, &mut ws);
                for p in ws.iter_mut() { if !p.stop { p.calls.push((wn, step.clone())); } }
                for p in ws.iter_mut() { p.stop = false; }
                if self.paths.insert(wn, ws).is_some() { self.fail(8); }
                let _ = cur;
            }
        }
    }

    fn expr(&mut self, owner: FuncId, cur: Node, t: LocalId, arena: LocalId, e: &Expr, st: &mut Vec<Path>) {
        if !self.ok { return; }
        match &e.kind {
            ExprKind::Call(g, args) => {
                for a in args { self.expr(owner, cur, t, arena, a, st); }
                if !self.member(*g) { return; }
                let gf = &self.m.funcs[*g];
                let arenas: Vec<usize> = args.iter().enumerate().filter(|(_, a)| matches!(a.kind, ExprKind::Local(x) | ExprKind::Ref(x, _) if x == arena)).map(|(j, _)| j).collect();
                let nodes: Vec<(usize, Link)> = args.iter().enumerate().filter(|(j, _)| gf.locals[gf.params[*j]].ty == Ty::I64)
                    .filter_map(|(j, a)| route(a, t, arena, &self.alias).map(|l| (j, l))).collect();
                let ([aj], [(pj, link)]) = (&arenas[..], &nodes[..]) else {
                    if std::env::var("NEANT_DEBUG_FOREST").is_ok() && self.ok { eprintln!("forest: call to {} line {} (arena {}, node {}): arenas {:?} nodes {:?}", gf.name, e.line, self.m.funcs[owner].locals[arena].name, self.m.funcs[owner].locals[t].name, arenas, nodes.iter().map(|x| x.0).collect::<Vec<_>>()); }
                    self.fail(9); return };
                match self.node.get(g) {
                    Some(&(p0, a0)) if (p0, a0) != (*pj, *aj) => { self.fail(10); return; }
                    Some(_) => {}
                    None => { self.node.insert(*g, (*pj, *aj)); self.queue.push(*g); }
                }
                let of = &self.m.funcs[owner];
                let hand: Vec<Option<usize>> = args.iter().map(|a| match a.kind {
                    ExprKind::Local(x) | ExprKind::Ref(x, _) if !of.locals[x].mutable => of.params.iter().position(|&p| p == x),
                    _ => None,
                }).collect();
                self.hands.push((owner, *g, hand));
                push(st, (Node::F(*g), link.clone()));
            }
            ExprKind::If(c, tb, eb) => {
                self.expr(owner, cur, t, arena, c, st);
                if !self.calls_member_block(tb) && !eb.as_ref().is_some_and(|b| self.calls_member_block(b)) { return; }
                let mut results = Vec::new();
                let mut ts = vec![Path::default()];
                self.block(owner, cur, t, arena, tb, &mut ts);
                results.extend(ts);
                match eb {
                    Some(b) => { let mut es = vec![Path::default()]; self.block(owner, cur, t, arena, b, &mut es); results.extend(es); }
                    None => results.push(Path::default()),
                }
                let mut next: Vec<Path> = st.iter().filter(|p| p.stop).cloned().collect();
                for l in st.iter().filter(|p| !p.stop) {
                    for r in &results {
                        let mut c = l.calls.clone();
                        c.extend(r.calls.iter().cloned());
                        next.push(Path { calls: c, stop: r.stop });
                    }
                }
                if next.len() > CAP { self.fail(11); return; }
                *st = next;
            }
            ExprKind::Block(b) => self.block(owner, cur, t, arena, b, st),
            ExprKind::Binary(_, a, b) | ExprKind::MinMax(_, a, b) | ExprKind::FieldIndex(a, b, _) | ExprKind::InRow(a, b) => {
                self.expr(owner, cur, t, arena, a, st);
                self.expr(owner, cur, t, arena, b, st);
            }
            ExprKind::Unary(_, a) | ExprKind::Field(a, _) | ExprKind::Cast(a, _) | ExprKind::Println(a) | ExprKind::Index(_, a) => self.expr(owner, cur, t, arena, a, st),
            ExprKind::StructLit(_, es) | ExprKind::ArrayVal(es) => for x in es { self.expr(owner, cur, t, arena, x, st); },
            _ => {}
        }
    }
}

fn push(st: &mut [Path], c: (Node, Link)) {
    for p in st.iter_mut() { if !p.stop { p.calls.push(c.clone()); } }
}

/// The links from node `t` down to the node `e` is: `t` itself, a name bound to one below it, or
/// `xs[e'].f` / `xs[e']` of one.
fn route(e: &Expr, t: LocalId, arena: LocalId, alias: &HashMap<LocalId, (LocalId, Link)>) -> Option<Link> {
    let step = |i: &Expr, f: Option<usize>| -> Option<Link> {
        let mut p = route(i, t, arena, alias)?;
        p.push(f);
        Some(p)
    };
    match &e.kind {
        ExprKind::Local(l) if *l == t => Some(vec![]),
        ExprKind::Local(l) => alias.get(l).filter(|(b, _)| *b == t).map(|(_, p)| p.clone()),
        ExprKind::Field(inner, f) => match &inner.kind {
            ExprKind::Index(arr, i) if *arr == arena => step(i, Some(*f)),
            _ => None,
        },
        ExprKind::Index(arr, i) if *arr == arena => step(i, None),
        _ => None,
    }
}

/// `while s >= 0 [&& …] { …; s = xs[s].f; … }`, `xs` the arena: the variable and the link it steps
/// down, when it is stepped that once, at the top level, and assigned nowhere else in the body.
fn walk_of(cond: &Expr, body: &Block, arena: LocalId, f: &Func) -> Option<(LocalId, Link)> {
    fn var_of(c: &Expr) -> Option<LocalId> {
        match &c.kind {
            ExprKind::Binary(BinOp::Ge, l, r) => match (&l.kind, &r.kind) { (ExprKind::Local(v), ExprKind::Int(0)) => Some(*v), _ => None },
            ExprKind::Binary(BinOp::Le, l, r) => match (&l.kind, &r.kind) { (ExprKind::Int(0), ExprKind::Local(v)) => Some(*v), _ => None },
            ExprKind::Binary(BinOp::And, l, r) => var_of(l).or_else(|| var_of(r)),
            _ => None,
        }
    }
    let v = var_of(cond)?;
    if !f.locals[v].mutable || f.locals[v].ty != Ty::I64 { return None; }
    let mut step = None;
    let mut n = 0;
    for s in &body.stmts {
        if let Stmt::Assign(LValue::Var(x), None, e) = s {
            if *x == v {
                n += 1;
                let l = route(e, v, arena, &HashMap::new())?;
                if l.is_empty() { return None; }
                step = Some(l);
            }
        }
    }
    if n != 1 { return None; }
    if assigns_nested(body, v) != 1 { return None; }
    step.map(|s| (v, s))
}

fn assigns_nested(b: &Block, v: LocalId) -> usize {
    fn blk(b: &Block, v: LocalId) -> usize { b.stmts.iter().map(|s| st(s, v)).sum::<usize>() + b.tail.as_ref().map_or(0, |e| ex(e, v)) }
    fn st(s: &Stmt, v: LocalId) -> usize {
        match s {
            Stmt::Assign(LValue::Var(x), _, e) => (*x == v) as usize + ex(e, v),
            Stmt::Assign(_, _, e) | Stmt::Let(_, e) | Stmt::Expr(e) | Stmt::Return(Some(e)) => ex(e, v),
            Stmt::For { body, .. } | Stmt::While { body, .. } | Stmt::ParFor { body, .. } | Stmt::LetBuild { body, .. } => blk(body, v),
            _ => 0,
        }
    }
    fn ex(e: &Expr, v: LocalId) -> usize {
        match &e.kind {
            ExprKind::If(c, t, e2) => ex(c, v) + blk(t, v) + e2.as_ref().map_or(0, |b| blk(b, v)),
            ExprKind::Block(b) => blk(b, v),
            _ => 0,
        }
    }
    blk(b, v)
}

/// The value `v` was last given before the loop, at the top level of the block that holds it.
fn entry_of(before: &[Stmt], v: LocalId) -> Option<&Expr> {
    for s in before.iter().rev() {
        match s {
            Stmt::Let(x, e) if *x == v => return Some(e),
            Stmt::Assign(LValue::Var(x), None, e) if *x == v => return Some(e),
            Stmt::Assign(LValue::Var(x), Some(_), _) if *x == v => return None,
            _ => {}
        }
        if assigns_nested(&Block { stmts: vec![s.clone()], tail: None, ty: Ty::Unit }, v) > 0 { return None; }
    }
    None
}

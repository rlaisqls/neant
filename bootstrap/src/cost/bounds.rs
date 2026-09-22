//! The lower-bound catalogue. Entry one: the matrix product.
//!
//! A statement `acc += A[ia] * B[ib]` inside a loop nest, where the two indices are affine in
//! the loop variables, share at least one of them (the reduction) and each has one the other
//! lacks, performs a contraction. Whatever the loop order or tiling around it, the number of
//! multiply-adds is the product of the enclosing trip counts, and for `N` multiply-adds with
//! `w`-byte elements a cache of `M` bytes must move at least `w·N/√M` bytes
//! (Hong–Kung 1981; the constant is Irony–Toledo–Tiskin's, with `M` in bytes and `√(M/w)·w`
//! folded in for 8-byte words: `N/(2√2·√(M/8))` words is `N/√M` words).

use crate::ast::BinOp;
use crate::ir::*;

use super::size::{Atom, Poly, Rat};

/// A recognised contraction and what the catalogue says about it.
#[derive(Debug, Clone)]
pub struct Bound {
    pub kind: &'static str,
    pub citation: &'static str,
    /// The bound on moves, in bytes, over the function's atoms.
    pub moves: Poly,
    pub line: u32,
    /// The two operand arrays, by local id, with their index expressions' lines.
    pub operands: [LocalId; 2],
    /// The loop nest around the statement, outermost first.
    pub nest: Vec<LocalId>,
}

/// The shape of a multiply-accumulate statement, if `s` is one.
pub struct Mac<'a> {
    pub a: LocalId,
    pub ia: &'a Expr,
    pub b: LocalId,
    pub ib: &'a Expr,
    pub line: u32,
}

pub fn as_mac(s: &Stmt) -> Option<Mac<'_>> {
    let (op, e) = match s {
        Stmt::Assign(LValue::Var(_), Some(BinOp::Add), e) | Stmt::Assign(LValue::Index(_, _, _), Some(BinOp::Add), e) => (BinOp::Add, e),
        _ => return None,
    };
    let _ = op;
    if let ExprKind::Binary(BinOp::Mul, l, r) = &e.kind {
        if let (ExprKind::Index(a, ia), ExprKind::Index(b, ib)) = (&l.kind, &r.kind) {
            return Some(Mac { a: *a, ia, b: *b, ib, line: e.line });
        }
    }
    None
}

/// `w·N/√M` bytes, for `N` multiply-adds on `w`-byte elements.
pub fn matmul_bound(n_macs: &Poly, elem_bytes: i128) -> Poly {
    n_macs.scale(Rat::int(elem_bytes)).mul_atom_pow(Atom::M, Rat::new(-1, 2))
}

/// Do the two index dependence sets make this a contraction: a shared reduction variable and a
/// free variable on each side?
pub fn is_contraction(vars_a: &[LocalId], vars_b: &[LocalId]) -> bool {
    let shared = vars_a.iter().any(|v| vars_b.contains(v));
    let free_a = vars_a.iter().any(|v| !vars_b.contains(v));
    let free_b = vars_b.iter().any(|v| !vars_a.contains(v));
    shared && free_a && free_b
}

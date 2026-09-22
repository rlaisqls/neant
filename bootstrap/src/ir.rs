#![allow(dead_code)] // sizes and names are read by the cost calculus from M1 on

//! The typed IR. Names are resolved to local ids, every expression carries its type, and every
//! array- or slice-typed value carries a size — a constant or a size variable — because the
//! cost calculus is written over sizes. Nothing here is lowered yet; it is the tree, typed.

use std::fmt;

pub type LocalId = usize;
pub type FuncId = usize;
pub type SizeVar = usize;

#[derive(Debug, Clone, PartialEq)]
pub enum Size {
    Const(i64),
    Var(SizeVar),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    I64,
    F64,
    Bool,
    U8,
    Unit,
    /// An owned array `[T; n]`. Only ever the type of a local.
    Array(Box<Ty>, Size),
    /// A view `&[T]` / `&mut [T]`. Carries the size of what it views.
    Slice(Box<Ty>, bool, Size),
}

impl Ty {
    pub fn is_scalar(&self) -> bool { matches!(self, Ty::I64 | Ty::F64 | Ty::Bool | Ty::U8) }
    pub fn is_numeric(&self) -> bool { matches!(self, Ty::I64 | Ty::F64 | Ty::U8) }
    pub fn elem_bytes(&self) -> i128 { match self { Ty::Bool | Ty::U8 => 1, _ => 8 } }
    pub fn is_arrayish(&self) -> bool { matches!(self, Ty::Array(..) | Ty::Slice(..)) }
    pub fn elem(&self) -> Option<&Ty> {
        match self { Ty::Array(t, _) | Ty::Slice(t, _, _) => Some(t), _ => None }
    }
    pub fn size(&self) -> Option<&Size> {
        match self { Ty::Array(_, s) | Ty::Slice(_, _, s) => Some(s), _ => None }
    }
    /// Same shape, ignoring sizes: sizes are tracked, not enforced, for now.
    pub fn same_shape(&self, other: &Ty) -> bool {
        match (self, other) {
            (Ty::Array(a, _), Ty::Array(b, _)) => a.same_shape(b),
            (Ty::Slice(a, m1, _), Ty::Slice(b, m2, _)) => m1 == m2 && a.same_shape(b),
            (a, b) => a == b,
        }
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::I64 => write!(f, "i64"),
            Ty::F64 => write!(f, "f64"),
            Ty::Bool => write!(f, "bool"),
            Ty::U8 => write!(f, "u8"),
            Ty::Unit => write!(f, "()"),
            Ty::Array(t, Size::Const(n)) => write!(f, "[{t}; {n}]"),
            Ty::Array(t, Size::Var(_)) => write!(f, "[{t}; n]"),
            Ty::Slice(t, false, _) => write!(f, "&[{t}]"),
            Ty::Slice(t, true, _) => write!(f, "&mut [{t}]"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Module {
    pub funcs: Vec<Func>,
}

#[derive(Debug, Clone)]
pub struct Func {
    pub name: String,
    pub params: Vec<LocalId>,
    pub ret: Ty,
    pub locals: Vec<Local>,
    pub sizes: Vec<SizeInfo>,
    pub body: Block,
    /// `#[cost(...)]` bounds: (key, expression text, line, col)
    pub asserts: Vec<(String, String, u32, u32)>,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct Local {
    pub name: String,
    pub ty: Ty,
    pub mutable: bool,
}

/// What a size variable is called when it appears in a report: `a.len()`, `n`, or a generated
/// name when it came from an expression with no name of its own.
#[derive(Debug, Clone)]
pub struct SizeInfo {
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Box<Expr>>,
    pub ty: Ty,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let(LocalId, Expr),
    /// `let xs = [e; n]` — the array is built in place, `n` is evaluated once.
    LetRepeat(LocalId, Expr, Expr),
    /// `let xs = [a, b, c]`
    LetArray(LocalId, Vec<Expr>),
    /// `let ys = [e for x in xs]`: an array of `len` elements, element `k` computed by `body`
    /// with `var` bound to `k`. One pass, no intermediate.
    LetBuild { id: LocalId, len: Expr, var: LocalId, body: Block },
    Assign(LValue, Option<crate::ast::BinOp>, Expr),
    For { var: LocalId, start: Expr, end: Expr, body: Block },
    /// `decreasing` is the programmer's measure, when the compiler needs one.
    While { cond: Expr, decreasing: Option<Expr>, body: Block, line: u32 },
    Break,
    Expr(Expr),
    Return(Option<Expr>),
}

#[derive(Debug, Clone)]
pub enum LValue {
    Var(LocalId),
    Index(LocalId, Expr, u32),
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Ty,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    Byte(u8),
    Local(LocalId),
    Binary(crate::ast::BinOp, Box<Expr>, Box<Expr>),
    Unary(crate::ast::UnOp, Box<Expr>),
    Index(LocalId, Box<Expr>),
    Call(FuncId, Vec<Expr>),
    Println(Box<Expr>),
    Len(LocalId),
    /// A view of a local array or a reborrow of a local slice.
    Ref(LocalId, bool),
    Cast(Box<Expr>, Ty),
    /// `min(a, b)` / `max(a, b)`
    MinMax(bool, Box<Expr>, Box<Expr>),
    If(Box<Expr>, Block, Option<Block>),
    Block(Block),
}

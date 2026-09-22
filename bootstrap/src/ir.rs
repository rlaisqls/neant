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
    /// A struct value, by index into the module's table. Passed and returned by copy.
    Struct(StructId),
}

pub type StructId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout { Aos, Soa }

#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: String,
    /// scalar fields, in declaration order
    pub fields: Vec<(String, Ty)>,
    /// the layout of every array of this struct in the program: fixed by `#[layout]` when
    /// `fixed`, else the compiler's choice (AoS until the layout analysis runs)
    pub layout: Layout,
    pub fixed: bool,
}

impl StructDef {
    /// C's rules for the AoS element: each field aligned to its size, the whole padded to the
    /// largest alignment.
    pub fn size(&self) -> i128 {
        let mut off = 0i128;
        let mut align = 1i128;
        for (_, t) in &self.fields {
            let sz = t.elem_bytes();
            off = (off + sz - 1) / sz * sz;
            off += sz;
            align = align.max(sz);
        }
        (off + align - 1) / align * align
    }
    pub fn field(&self, name: &str) -> Option<usize> { self.fields.iter().position(|(f, _)| f == name) }
}

impl Ty {
    pub fn is_scalar(&self) -> bool { matches!(self, Ty::I64 | Ty::F64 | Ty::Bool | Ty::U8) }
    /// A value that lives in registers and is copied: a scalar or a struct.
    pub fn is_value(&self) -> bool { self.is_scalar() || matches!(self, Ty::Struct(_)) }
    pub fn is_numeric(&self) -> bool { matches!(self, Ty::I64 | Ty::F64 | Ty::U8) }
    /// Bytes of a scalar; a struct's size needs the module (`Module::size_of`).
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
            Ty::Struct(i) => write!(f, "struct#{i}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Module {
    pub funcs: Vec<Func>,
    pub structs: Vec<StructDef>,
}

impl Module {
    /// Bytes of one element of type `t` in memory: a scalar's size, or the struct's AoS size.
    pub fn size_of(&self, t: &Ty) -> i128 {
        match t { Ty::Struct(i) => self.structs[*i].size(), Ty::Array(e, _) | Ty::Slice(e, _, _) => self.size_of(e), t => t.elem_bytes() }
    }
    pub fn type_name(&self, t: &Ty) -> String {
        match t {
            Ty::Struct(i) => self.structs[*i].name.clone(),
            Ty::Array(e, s) => format!("[{}; {}]", self.type_name(e), match s { Size::Const(n) => n.to_string(), Size::Var(_) => "n".into() }),
            Ty::Slice(e, m, _) => format!("&{}[{}]", if *m { "mut " } else { "" }, self.type_name(e)),
            t => t.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Func {
    pub name: String,
    pub params: Vec<LocalId>,
    pub ret: Ty,
    pub locals: Vec<Local>,
    pub sizes: Vec<SizeInfo>,
    /// `None` for an `extern`: no body, cost and effects from the declaration
    pub body: Option<Block>,
    /// declared effects of an extern
    pub uses: Vec<String>,
    /// `#[cost(...)]` bounds: (key, expression text, line, col)
    pub asserts: Vec<(String, String, u32, u32)>,
    pub line: u32,
    /// `ys = xs;` sites, indexed by `Stmt::Reassign`; resolved once the whole body is checked
    pub reassigns: Vec<Reassign>,
}

/// `ys = xs;`: `src` is always dead after, `in_place` says whether `ys`'s buffer became `src`'s
/// (no bytes moved) or `src` was copied into `ys`'s own buffer (a full write) — decided once, from
/// the program text, in `types.rs` (docs/m5-design.md §2). `conflict_line` is the later use of a
/// view rooted at `src` that forced the copy, when there is a single line to name; the loop case
/// forces a copy with no such line.
#[derive(Debug, Clone)]
pub struct Reassign {
    pub target: LocalId,
    pub src: LocalId,
    pub line: u32,
    pub in_place: bool,
    pub conflict_line: Option<u32>,
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
    /// `ys = xs;` — index into the function's `reassigns`
    Reassign(usize),
    For { var: LocalId, start: Expr, end: Expr, body: Block },
    /// A `.par()` chain's loop (m5-span-design.md): `0..end`, `acc` combined with `op` — moves,
    /// footprint and residue cost it exactly as the same chain's sequential `For` would (the
    /// bytes touched do not change); only work's `Σ_v` becomes span's `O(log end)`, and emission
    /// adds `#pragma omp parallel for reduction(op:acc)`.
    ParFor { var: LocalId, end: Expr, body: Block, acc: LocalId, op: ParOp },
    /// `decreasing` is the programmer's measure, when the compiler needs one.
    While { cond: Expr, decreasing: Option<Expr>, body: Block, line: u32 },
    Break,
    Expr(Expr),
    Return(Option<Expr>),
}

/// A `.par()` chain's combine, restricted to the terminals with a clean identity element
/// (m5-span-design.md §1 — `max`/`min`'s "first element seen wins" has no OpenMP-native identity
/// and is deferred, not implemented as a silent approximation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParOp { Add, Or, And }

#[derive(Debug, Clone)]
pub enum LValue {
    Var(LocalId),
    Index(LocalId, Expr, u32),
    /// `v.f`, `v` a struct local
    Field(LocalId, usize),
    /// `xs[i].f`
    IndexField(LocalId, Expr, usize, u32),
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
    /// `e.f` by field position
    Field(Box<Expr>, usize),
    /// `S { … }` with every field, in declaration order
    StructLit(StructId, Vec<Expr>),
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

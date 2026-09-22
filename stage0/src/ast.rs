//! The syntax tree, as parsed. Names are strings, nothing is typed yet.

#[derive(Debug, Clone)]
pub struct Program {
    pub funcs: Vec<Func>,
}

#[derive(Debug, Clone)]
pub struct Func {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: TypeExpr,
    pub body: Block,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: TypeExpr,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone)]
pub enum TypeExpr {
    Named(String),
    Unit,
    /// `[T; n]`
    Array(Box<TypeExpr>, Box<Expr>),
    /// `&[T]` / `&mut [T]`
    Slice(Box<TypeExpr>, bool),
}

#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Box<Expr>>,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let { name: String, mutable: bool, ty: Option<TypeExpr>, init: Expr, line: u32, col: u32 },
    /// `target = value`, or `target op= value` when `op` is set.
    Assign { target: Expr, op: Option<BinOp>, value: Expr, line: u32, col: u32 },
    For { var: String, start: Expr, end: Expr, body: Block, line: u32, col: u32 },
    Expr(Expr),
    Return(Option<Expr>, u32, u32),
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    Var(String),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Unary(UnOp, Box<Expr>),
    Index(Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
    MethodCall(Box<Expr>, String, Vec<Expr>),
    /// `&e` / `&mut e`
    Ref(Box<Expr>, bool),
    Cast(Box<Expr>, TypeExpr),
    If(Box<Expr>, Block, Option<Block>),
    Block(Block),
    ArrayLit(Vec<Expr>),
    /// `[e; n]`
    ArrayRepeat(Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp { Add, Sub, Mul, Div, Rem, Eq, Ne, Lt, Le, Gt, Ge, And, Or }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp { Neg, Not }

impl BinOp {
    pub fn c_str(self) -> &'static str {
        match self {
            BinOp::Add => "+", BinOp::Sub => "-", BinOp::Mul => "*", BinOp::Div => "/", BinOp::Rem => "%",
            BinOp::Eq => "==", BinOp::Ne => "!=", BinOp::Lt => "<", BinOp::Le => "<=", BinOp::Gt => ">",
            BinOp::Ge => ">=", BinOp::And => "&&", BinOp::Or => "||",
        }
    }
    pub fn is_arith(self) -> bool {
        matches!(self, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem)
    }
    pub fn is_cmp(self) -> bool {
        matches!(self, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
    }
}

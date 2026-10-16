use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Int,
    Float,
    Bool,
    Str,
    Unit,
    Array(Box<Type>),
    Named(String),
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Type::Int => write!(f, "int"),
            Type::Float => write!(f, "float"),
            Type::Bool => write!(f, "bool"),
            Type::Str => write!(f, "str"),
            Type::Unit => write!(f, "()"),
            Type::Array(t) => write!(f, "[{}]", t),
            Type::Named(n) => write!(f, "{}", n),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

impl fmt::Display for BinOp {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "&&",
            BinOp::Or => "||",
        };
        write!(f, "{}", s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

/// `{ stmts; tail }` — the optional tail expression (no trailing `;`) is the block's value.
#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Box<Expr>>,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Var(String),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Unary(UnOp, Box<Expr>),
    Call(String, Vec<Expr>),
    MethodCall(Box<Expr>, String, Vec<Expr>),
    /// `Type::name` — an enum variant without data.
    Path(String, String),
    /// `Type::name(args)` — an enum variant with data, or an associated function.
    PathCall(String, String, Vec<Expr>),
    Index(Box<Expr>, Box<Expr>),
    Field(Box<Expr>, String),
    Array(Vec<Expr>),
    StructLit(String, Vec<(String, Expr)>),
    Block(Block),
    /// `if cond { } else ...` — the else branch is a Block or another If expression.
    If(Box<Expr>, Block, Option<Box<Expr>>),
    Match(Box<Expr>, Vec<MatchArm>),
    Loop(Block),
    Return(Option<Box<Expr>>),
    Break(Option<Box<Expr>>),
    Continue,
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
}

#[derive(Debug, Clone)]
pub enum Pattern {
    Wildcard,
    Bind(String),
    Int(i64),
    Str(String),
    Bool(bool),
    /// `lo..hi` or `lo..=hi`
    Range(i64, i64, bool),
    /// `Enum::Variant` or `Enum::Variant(p1, p2, ...)`
    Variant { enum_name: String, variant: String, fields: Option<Vec<Pattern>>, span: Span },
    Or(Vec<Pattern>),
}

#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ForIter {
    /// `start..end` or `start..=end`
    Range(Expr, Expr, bool),
    /// `for x in array` / `for c in string`
    Each(Expr),
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    Let { name: String, mutable: bool, ty: Option<Type>, value: Expr },
    Assign { target: Expr, op: Option<BinOp>, value: Expr },
    Expr(Expr),
    /// REPL only: a trailing expression whose value is printed.
    Echo(Expr),
    While { cond: Expr, body: Block },
    For { var: String, iter: ForIter, body: Block },
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Type,
    pub mutable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SelfKind {
    /// `self` or `mut self`
    Value { mutable: bool },
    /// `&self`
    Ref,
    /// `&mut self`
    RefMut,
}

#[derive(Debug, Clone)]
pub struct FnDecl {
    pub name: String,
    /// Set for methods declared inside `impl` that take `self`.
    pub self_kind: Option<SelfKind>,
    pub params: Vec<Param>,
    pub ret: Type,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct StructDecl {
    pub name: String,
    pub fields: Vec<(String, Type)>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct EnumDecl {
    pub name: String,
    pub variants: Vec<(String, Vec<Type>)>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ImplDecl {
    pub type_name: String,
    pub methods: Vec<FnDecl>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ConstDecl {
    pub name: String,
    pub ty: Option<Type>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Item {
    Fn(FnDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Impl(ImplDecl),
    Const(ConstDecl),
}

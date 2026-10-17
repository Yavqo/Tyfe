use std::fmt;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub line: usize,
    pub col: usize,
    /// Index of the source file (0 = the main file; modules get their own).
    pub file: u16,
}

impl Span {
    pub const NONE: Span = Span { line: 0, col: 0, file: 0 };
}

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Int,
    Float,
    Bool,
    Str,
    Unit,
    Array(Box<Type>),
    Tuple(Vec<Type>),
    Option(Box<Type>),
    Result(Box<Type>, Box<Type>),
    /// `fn(int, str) -> bool`
    Fn(Vec<Type>, Box<Type>),
    Named(String),
    /// `HashMap<K, V>`
    Map(Box<Type>, Box<Type>),
    /// `HashSet<T>`
    Set(Box<Type>),
    /// A generic parameter `T` with its trait bounds (`T: Display + Clone`).
    Generic(String, Vec<String>),
    /// `impl Trait` / `dyn Trait` / `&dyn A + B`
    Trait(Vec<String>),
    /// Internal: the payload type of the built-in `Option` / `Result` enums.
    Any,
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
            Type::Tuple(ts) => {
                let parts: Vec<String> = ts.iter().map(|t| t.to_string()).collect();
                write!(f, "({})", parts.join(", "))
            }
            Type::Option(t) => write!(f, "Option<{}>", t),
            Type::Result(t, e) => write!(f, "Result<{}, {}>", t, e),
            Type::Fn(params, ret) => {
                let parts: Vec<String> = params.iter().map(|t| t.to_string()).collect();
                write!(f, "fn({}) -> {}", parts.join(", "), ret)
            }
            Type::Named(n) => write!(f, "{}", n),
            Type::Map(k, v) => write!(f, "HashMap<{}, {}>", k, v),
            Type::Set(t) => write!(f, "HashSet<{}>", t),
            Type::Generic(n, _) => write!(f, "{}", n),
            Type::Trait(ts) => write!(f, "impl {}", ts.join(" + ")),
            Type::Any => write!(f, "_"),
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
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
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
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
        };
        write!(f, "{}", s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnOp {
    Neg,
    Not,
    /// `*x` — values are copies, so this just reads `x`.
    Deref,
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
    /// `Name { field: value, ..base }`
    StructLit(String, Vec<(String, Expr)>, Option<Box<Expr>>),
    Block(Block),
    /// `if cond { } else ...` — the else branch is a Block or another If expression.
    If(Box<Expr>, Block, Option<Box<Expr>>),
    Match(Box<Expr>, Vec<MatchArm>),
    Loop(Block, Option<String>),
    Return(Option<Box<Expr>>),
    /// `break 'label value`
    Break(Option<String>, Option<Box<Expr>>),
    Continue(Option<String>),
    /// `()`
    Unit,
    /// `(a, b, ...)`
    Tuple(Vec<Expr>),
    /// `tuple.0`
    TupleField(Box<Expr>, usize),
    /// `|x, y| body`
    Closure(Rc<ClosureDef>),
    /// Calling the result of an expression, e.g. `make_adder(1)(2)` or a closure in a variable.
    CallExpr(Box<Expr>, Vec<Expr>),
    /// `expr?`
    Try(Box<Expr>),
    /// `if let PATTERN = value { } else ...`
    IfLet(Box<Pattern>, Box<Expr>, Block, Option<Box<Expr>>),
    /// `&x` / `&mut x`
    Ref(bool, Box<Expr>),
    /// `x as float`
    Cast(Box<Expr>, Type),
    /// `a..b`, `a..=b`, `a..`, `..b`
    Range(Option<Box<Expr>>, Option<Box<Expr>>, bool),
    /// `matches!(value, pattern)` or `matches!(value, pattern if guard)`
    Matches(Box<Expr>, Box<Pattern>, Option<Box<Expr>>),
    /// `[value; count]`
    ArrayRepeat(Box<Expr>, Box<Expr>),
}

#[derive(Debug)]
pub struct ClosureDef {
    pub params: Vec<(String, Option<Type>)>,
    pub ret: Option<Type>,
    pub body: Expr,
    /// Variable names used in the body; those in scope are copied into the closure.
    pub captures: Vec<String>,
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
    /// `mut x` inside a pattern, e.g. `let (mut a, b) = ...`
    MutBind(String),
    Int(i64),
    Str(String),
    Bool(bool),
    /// `lo..hi` or `lo..=hi`
    Range(i64, i64, bool),
    /// `'a'..='z'`
    CharRange(char, char, bool),
    /// `Enum::Variant` or `Enum::Variant(p1, p2, ...)`
    Variant { enum_name: String, variant: String, fields: Option<Vec<Pattern>>, span: Span },
    Tuple(Vec<Pattern>),
    /// `Point { x, y: 0, .. }`
    Struct(String, Vec<(String, Pattern)>, Span),
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
    Let { pattern: Pattern, mutable: bool, ty: Option<Type>, value: Expr },
    Assign { target: Expr, op: Option<BinOp>, value: Expr },
    Expr(Expr),
    /// REPL only: a trailing expression whose value is printed.
    Echo(Expr),
    While { cond: Expr, body: Block, label: Option<String> },
    WhileLet { pattern: Pattern, value: Expr, body: Block, label: Option<String> },
    For { pattern: Pattern, iter: ForIter, body: Block, label: Option<String> },
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Type,
    pub mutable: bool,
    /// `x: &mut T` — the caller passes `&mut place` and sees the changes.
    pub by_ref_mut: bool,
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
    /// Marked `#[test]`; run by `tyfe test`.
    pub is_test: bool,
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
    /// Field names of struct-like variants, e.g. `Move { x: int, y: int }`.
    pub named: Vec<(String, Vec<String>)>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ImplDecl {
    pub type_name: String,
    /// `impl Trait for Type`
    pub trait_name: Option<String>,
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
pub struct TraitDecl {
    pub name: String,
    /// Methods every implementor must define.
    pub required: Vec<String>,
    /// Methods with a default body.
    pub defaults: Vec<FnDecl>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Item {
    Fn(FnDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Impl(ImplDecl),
    Const(ConstDecl),
    Trait(TraitDecl),
    /// `mod name;` — loads `name.ty` next to the current file.
    Mod(String, Span),
}

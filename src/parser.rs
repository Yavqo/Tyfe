use crate::ast::*;
use crate::error::{Error, Result};
use crate::lexer::{lex, Tok, Token};

/// One top-level entry typed into the REPL.
pub enum ReplEntry {
    Item(Item),
    Stmt(Stmt),
}

pub fn parse_program(src: &str) -> Result<Vec<Item>> {
    let mut p = Parser::new(lex(src)?, false);
    let mut items = Vec::new();
    while !p.at(&Tok::Eof) {
        items.push(p.item()?);
    }
    Ok(items)
}

pub fn parse_repl(src: &str) -> Result<Vec<ReplEntry>> {
    let mut p = Parser::new(lex(src)?, true);
    let mut entries = Vec::new();
    while !p.at(&Tok::Eof) {
        if p.at(&Tok::Fn) || p.at(&Tok::Struct) {
            entries.push(ReplEntry::Item(p.item()?));
        } else {
            entries.push(ReplEntry::Stmt(p.stmt()?));
        }
    }
    Ok(entries)
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    /// When true, `Name {` is not parsed as a struct literal
    /// (so `if x == y { ... }` works, like in Rust).
    no_struct: bool,
    repl: bool,
}

impl Parser {
    fn new(toks: Vec<Token>, repl: bool) -> Self {
        Parser { toks, pos: 0, no_struct: false, repl }
    }

    // ---------- token helpers ----------

    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn peek_at(&self, n: usize) -> &Tok {
        let i = (self.pos + n).min(self.toks.len() - 1);
        &self.toks[i].tok
    }

    fn span(&self) -> Span {
        self.toks[self.pos].span
    }

    fn at(&self, t: &Tok) -> bool {
        self.peek() == t
    }

    fn advance(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.at(t) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: Tok, context: &str) -> Result<Token> {
        if self.at(&t) {
            Ok(self.advance())
        } else {
            Err(Error::syntax(
                self.span(),
                format!("expected {} {}, found {}", t.describe(), context, self.peek().describe()),
            ))
        }
    }

    fn ident(&mut self, what: &str) -> Result<(String, Span)> {
        let span = self.span();
        match self.peek().clone() {
            Tok::Ident(name) => {
                self.advance();
                Ok((name, span))
            }
            other => Err(Error::syntax(span, format!("expected {}, found {}", what, other.describe()))),
        }
    }

    /// Parse with struct literals temporarily allowed/disallowed.
    fn with_struct<T>(&mut self, allowed: bool, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        let saved = self.no_struct;
        self.no_struct = !allowed;
        let r = f(self);
        self.no_struct = saved;
        r
    }

    // ---------- items ----------

    fn item(&mut self) -> Result<Item> {
        match self.peek() {
            Tok::Fn => Ok(Item::Fn(self.fn_decl()?)),
            Tok::Struct => Ok(Item::Struct(self.struct_decl()?)),
            other => Err(Error::syntax(
                self.span(),
                format!("expected `fn` or `struct` at top level, found {}", other.describe()),
            )),
        }
    }

    fn fn_decl(&mut self) -> Result<FnDecl> {
        let span = self.expect(Tok::Fn, "")?.span;
        let (name, _) = self.ident("function name")?;
        self.expect(Tok::LParen, "after function name")?;
        let mut params = Vec::new();
        while !self.at(&Tok::RParen) {
            let mutable = self.eat(&Tok::Mut);
            let (pname, pspan) = self.ident("parameter name")?;
            if params.iter().any(|p: &Param| p.name == pname) {
                return Err(Error::syntax(pspan, format!("parameter `{}` is declared more than once", pname)));
            }
            self.expect(Tok::Colon, "after parameter name (parameters need a type, e.g. `x: int`)")?;
            let ty = self.ty()?;
            params.push(Param { name: pname, ty, mutable });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen, "to close parameter list")?;
        let ret = if self.eat(&Tok::Arrow) { self.ty()? } else { Type::Unit };
        let body = self.block()?;
        Ok(FnDecl { name, params, ret, body, span })
    }

    fn struct_decl(&mut self) -> Result<StructDecl> {
        let span = self.expect(Tok::Struct, "")?.span;
        let (name, _) = self.ident("struct name")?;
        self.expect(Tok::LBrace, "after struct name")?;
        let mut fields: Vec<(String, Type)> = Vec::new();
        while !self.at(&Tok::RBrace) {
            let (fname, fspan) = self.ident("field name")?;
            if fields.iter().any(|(n, _)| *n == fname) {
                return Err(Error::syntax(fspan, format!("field `{}` is declared more than once", fname)));
            }
            self.expect(Tok::Colon, "after field name")?;
            fields.push((fname, self.ty()?));
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RBrace, "to close struct")?;
        Ok(StructDecl { name, fields, span })
    }

    fn ty(&mut self) -> Result<Type> {
        let span = self.span();
        if self.eat(&Tok::LBracket) {
            let inner = self.ty()?;
            self.expect(Tok::RBracket, "to close array type")?;
            return Ok(Type::Array(Box::new(inner)));
        }
        if self.eat(&Tok::LParen) {
            self.expect(Tok::RParen, "in unit type `()`")?;
            return Ok(Type::Unit);
        }
        let (name, _) = self.ident("a type").map_err(|_| {
            Error::syntax(span, format!("expected a type, found {}", self.peek().describe()))
        })?;
        Ok(match name.as_str() {
            "int" | "i64" => Type::Int,
            "float" | "f64" => Type::Float,
            "bool" => Type::Bool,
            "str" | "String" => Type::Str,
            _ => Type::Named(name),
        })
    }

    // ---------- statements ----------

    fn block(&mut self) -> Result<Vec<Stmt>> {
        self.expect(Tok::LBrace, "to start a block")?;
        let mut stmts = Vec::new();
        self.with_struct(true, |p| {
            while !p.at(&Tok::RBrace) {
                if p.at(&Tok::Eof) {
                    return Err(Error::syntax(p.span(), "unexpected end of file: missing `}`"));
                }
                stmts.push(p.stmt()?);
            }
            Ok(())
        })?;
        self.expect(Tok::RBrace, "to close block")?;
        Ok(stmts)
    }

    fn stmt(&mut self) -> Result<Stmt> {
        let span = self.span();
        let kind = match self.peek() {
            Tok::Let => {
                self.advance();
                let mutable = self.eat(&Tok::Mut);
                let (name, _) = self.ident("variable name after `let`")?;
                let ty = if self.eat(&Tok::Colon) { Some(self.ty()?) } else { None };
                self.expect(Tok::Eq, "in `let` (variables must be initialized)")?;
                let value = self.expr()?;
                self.expect(Tok::Semi, "after `let` statement")?;
                StmtKind::Let { name, mutable, ty, value }
            }
            Tok::If => return self.if_stmt(),
            Tok::While => {
                self.advance();
                let cond = self.with_struct(false, |p| p.expr())?;
                let body = self.block()?;
                StmtKind::While { cond, body }
            }
            Tok::For => {
                self.advance();
                let (var, _) = self.ident("loop variable after `for`")?;
                self.expect(Tok::In, "after loop variable")?;
                let iter = self.with_struct(false, |p| {
                    let start = p.expr()?;
                    if p.eat(&Tok::DotDot) {
                        Ok(ForIter::Range(start, p.expr()?, false))
                    } else if p.eat(&Tok::DotDotEq) {
                        Ok(ForIter::Range(start, p.expr()?, true))
                    } else {
                        Ok(ForIter::Each(start))
                    }
                })?;
                let body = self.block()?;
                StmtKind::For { var, iter, body }
            }
            Tok::Return => {
                self.advance();
                let value = if self.at(&Tok::Semi) { None } else { Some(self.expr()?) };
                self.expect(Tok::Semi, "after `return`")?;
                StmtKind::Return(value)
            }
            Tok::Break => {
                self.advance();
                self.expect(Tok::Semi, "after `break`")?;
                StmtKind::Break
            }
            Tok::Continue => {
                self.advance();
                self.expect(Tok::Semi, "after `continue`")?;
                StmtKind::Continue
            }
            Tok::LBrace => StmtKind::Block(self.block()?),
            _ => {
                let e = self.expr()?;
                let op = match self.peek() {
                    Tok::Eq => Some(None),
                    Tok::PlusEq => Some(Some(BinOp::Add)),
                    Tok::MinusEq => Some(Some(BinOp::Sub)),
                    Tok::StarEq => Some(Some(BinOp::Mul)),
                    Tok::SlashEq => Some(Some(BinOp::Div)),
                    Tok::PercentEq => Some(Some(BinOp::Rem)),
                    _ => None,
                };
                if let Some(op) = op {
                    let op_span = self.span();
                    self.advance();
                    if !matches!(e.kind, ExprKind::Var(_) | ExprKind::Index(..) | ExprKind::Field(..)) {
                        return Err(Error::syntax(op_span, "invalid left-hand side of assignment"));
                    }
                    let value = self.expr()?;
                    self.expect(Tok::Semi, "after assignment")?;
                    StmtKind::Assign { target: e, op, value }
                } else if self.repl && self.at(&Tok::Eof) {
                    StmtKind::Echo(e)
                } else {
                    self.expect(Tok::Semi, "after expression")?;
                    StmtKind::Expr(e)
                }
            }
        };
        Ok(Stmt { kind, span })
    }

    fn if_stmt(&mut self) -> Result<Stmt> {
        let span = self.expect(Tok::If, "")?.span;
        let cond = self.with_struct(false, |p| p.expr())?;
        let then = self.block()?;
        let else_ = if self.eat(&Tok::Else) {
            if self.at(&Tok::If) {
                Some(vec![self.if_stmt()?])
            } else {
                Some(self.block()?)
            }
        } else {
            None
        };
        Ok(Stmt { kind: StmtKind::If { cond, then, else_ }, span })
    }

    // ---------- expressions (lowest to highest precedence) ----------

    fn expr(&mut self) -> Result<Expr> {
        self.or()
    }

    fn binary_level(
        &mut self,
        next: fn(&mut Self) -> Result<Expr>,
        ops: &[(Tok, BinOp)],
    ) -> Result<Expr> {
        let mut left = next(self)?;
        'outer: loop {
            for (tok, op) in ops {
                if self.at(tok) {
                    let span = self.advance().span;
                    let right = next(self)?;
                    left = Expr { kind: ExprKind::Binary(*op, Box::new(left), Box::new(right)), span };
                    continue 'outer;
                }
            }
            return Ok(left);
        }
    }

    fn or(&mut self) -> Result<Expr> {
        self.binary_level(Self::and, &[(Tok::OrOr, BinOp::Or)])
    }

    fn and(&mut self) -> Result<Expr> {
        self.binary_level(Self::equality, &[(Tok::AndAnd, BinOp::And)])
    }

    fn equality(&mut self) -> Result<Expr> {
        self.binary_level(Self::comparison, &[(Tok::EqEq, BinOp::Eq), (Tok::NotEq, BinOp::Ne)])
    }

    fn comparison(&mut self) -> Result<Expr> {
        self.binary_level(
            Self::additive,
            &[(Tok::Lt, BinOp::Lt), (Tok::Le, BinOp::Le), (Tok::Gt, BinOp::Gt), (Tok::Ge, BinOp::Ge)],
        )
    }

    fn additive(&mut self) -> Result<Expr> {
        self.binary_level(Self::multiplicative, &[(Tok::Plus, BinOp::Add), (Tok::Minus, BinOp::Sub)])
    }

    fn multiplicative(&mut self) -> Result<Expr> {
        self.binary_level(
            Self::unary,
            &[(Tok::Star, BinOp::Mul), (Tok::Slash, BinOp::Div), (Tok::Percent, BinOp::Rem)],
        )
    }

    fn unary(&mut self) -> Result<Expr> {
        let span = self.span();
        let op = match self.peek() {
            Tok::Minus => UnOp::Neg,
            Tok::Bang => UnOp::Not,
            _ => return self.postfix(),
        };
        self.advance();
        let operand = self.unary()?;
        Ok(Expr { kind: ExprKind::Unary(op, Box::new(operand)), span })
    }

    fn postfix(&mut self) -> Result<Expr> {
        let mut e = self.primary()?;
        loop {
            let span = self.span();
            if self.eat(&Tok::LBracket) {
                let idx = self.with_struct(true, |p| p.expr())?;
                self.expect(Tok::RBracket, "to close index")?;
                e = Expr { kind: ExprKind::Index(Box::new(e), Box::new(idx)), span };
            } else if self.eat(&Tok::Dot) {
                let (name, nspan) = self.ident("field or method name after `.`")?;
                if self.at(&Tok::LParen) {
                    let args = self.args()?;
                    e = Expr { kind: ExprKind::MethodCall(Box::new(e), name, args), span: nspan };
                } else {
                    e = Expr { kind: ExprKind::Field(Box::new(e), name), span: nspan };
                }
            } else {
                return Ok(e);
            }
        }
    }

    fn args(&mut self) -> Result<Vec<Expr>> {
        self.expect(Tok::LParen, "")?;
        let args = self.with_struct(true, |p| {
            let mut args = Vec::new();
            while !p.at(&Tok::RParen) {
                args.push(p.expr()?);
                if !p.eat(&Tok::Comma) {
                    break;
                }
            }
            Ok(args)
        })?;
        self.expect(Tok::RParen, "to close argument list")?;
        Ok(args)
    }

    fn looks_like_struct_lit(&self) -> bool {
        !self.no_struct
            && self.peek_at(1) == &Tok::LBrace
            && (self.peek_at(2) == &Tok::RBrace
                || (matches!(self.peek_at(2), Tok::Ident(_)) && self.peek_at(3) == &Tok::Colon))
    }

    fn primary(&mut self) -> Result<Expr> {
        let span = self.span();
        let kind = match self.peek().clone() {
            Tok::Int(n) => {
                self.advance();
                ExprKind::Int(n)
            }
            Tok::Float(f) => {
                self.advance();
                ExprKind::Float(f)
            }
            Tok::Str(s) => {
                self.advance();
                ExprKind::Str(s)
            }
            Tok::True => {
                self.advance();
                ExprKind::Bool(true)
            }
            Tok::False => {
                self.advance();
                ExprKind::Bool(false)
            }
            Tok::Ident(name) => {
                if self.peek_at(1) == &Tok::LParen {
                    self.advance();
                    ExprKind::Call(name, self.args()?)
                } else if self.looks_like_struct_lit() {
                    self.advance();
                    self.advance(); // {
                    let fields = self.with_struct(true, |p| {
                        let mut fields: Vec<(String, Expr)> = Vec::new();
                        while !p.at(&Tok::RBrace) {
                            let (fname, _) = p.ident("field name")?;
                            p.expect(Tok::Colon, "after field name")?;
                            fields.push((fname, p.expr()?));
                            if !p.eat(&Tok::Comma) {
                                break;
                            }
                        }
                        Ok(fields)
                    })?;
                    self.expect(Tok::RBrace, "to close struct literal")?;
                    ExprKind::StructLit(name, fields)
                } else {
                    self.advance();
                    ExprKind::Var(name)
                }
            }
            Tok::LParen => {
                self.advance();
                let e = self.with_struct(true, |p| p.expr())?;
                self.expect(Tok::RParen, "to close parenthesis")?;
                return Ok(e);
            }
            Tok::LBracket => {
                self.advance();
                let elems = self.with_struct(true, |p| {
                    let mut elems = Vec::new();
                    while !p.at(&Tok::RBracket) {
                        elems.push(p.expr()?);
                        if !p.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    Ok(elems)
                })?;
                self.expect(Tok::RBracket, "to close array literal")?;
                ExprKind::Array(elems)
            }
            other => {
                return Err(Error::syntax(span, format!("expected an expression, found {}", other.describe())))
            }
        };
        Ok(Expr { kind, span })
    }
}

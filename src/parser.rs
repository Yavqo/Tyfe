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
        if p.starts_item() {
            entries.push(ReplEntry::Item(p.item()?));
        } else {
            match p.stmt_or_tail(true)? {
                Parsed::Stmt(s) => entries.push(ReplEntry::Stmt(s)),
                Parsed::Tail(e) => return Err(Error::syntax(e.span, "unexpected `}`")),
            }
        }
    }
    Ok(entries)
}

enum Parsed {
    Stmt(Stmt),
    /// The final expression of a block, with no `;` — the block's value.
    Tail(Expr),
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    /// When true, `Name {` is not parsed as a struct literal
    /// (so `if x == y { ... }` works, like in Rust).
    no_struct: bool,
    repl: bool,
    /// The type `Self` refers to while parsing an `impl` block.
    impl_type: Option<String>,
}

impl Parser {
    fn new(toks: Vec<Token>, repl: bool) -> Self {
        Parser { toks, pos: 0, no_struct: false, repl, impl_type: None }
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

    fn at_ident(&self, name: &str) -> bool {
        matches!(self.peek(), Tok::Ident(n) if n == name)
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

    /// Resolve `Self` to the type of the enclosing `impl` block.
    fn resolve_self(&self, name: String, span: Span) -> Result<String> {
        if name != "Self" {
            return Ok(name);
        }
        self.impl_type
            .clone()
            .ok_or_else(|| Error::syntax(span, "`Self` can only be used inside an `impl` block"))
    }

    /// Parse with struct literals temporarily allowed/disallowed.
    fn with_struct<T>(&mut self, allowed: bool, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        let saved = self.no_struct;
        self.no_struct = !allowed;
        let r = f(self);
        self.no_struct = saved;
        r
    }

    /// Comma-separated list up to (and consuming) `close`.
    fn list<T>(&mut self, close: Tok, what: &str, mut f: impl FnMut(&mut Self) -> Result<T>) -> Result<Vec<T>> {
        let mut out = Vec::new();
        while !self.at(&close) {
            out.push(f(self)?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(close, what)?;
        Ok(out)
    }

    // ---------- items ----------

    fn starts_item(&self) -> bool {
        matches!(self.peek(), Tok::Fn | Tok::Struct | Tok::Enum | Tok::Impl | Tok::Const)
    }

    fn item(&mut self) -> Result<Item> {
        match self.peek() {
            Tok::Fn => Ok(Item::Fn(self.fn_decl(false)?)),
            Tok::Struct => Ok(Item::Struct(self.struct_decl()?)),
            Tok::Enum => Ok(Item::Enum(self.enum_decl()?)),
            Tok::Impl => Ok(Item::Impl(self.impl_decl()?)),
            Tok::Const => Ok(Item::Const(self.const_decl()?)),
            other => Err(Error::syntax(
                self.span(),
                format!("expected `fn`, `struct`, `enum`, `impl` or `const` at top level, found {}", other.describe()),
            )),
        }
    }

    fn fn_decl(&mut self, in_impl: bool) -> Result<FnDecl> {
        let span = self.expect(Tok::Fn, "")?.span;
        let (name, _) = self.ident("function name")?;
        self.expect(Tok::LParen, "after function name")?;

        // Optional `self`, `mut self`, `&self` or `&mut self` receiver.
        let self_span = self.span();
        let self_kind = if self.at(&Tok::Amp) {
            self.advance();
            let mutable = self.eat(&Tok::Mut);
            if !self.at_ident("self") {
                return Err(Error::syntax(self.span(), "expected `self` after `&`"));
            }
            self.advance();
            Some(if mutable { SelfKind::RefMut } else { SelfKind::Ref })
        } else if self.at(&Tok::Mut) && matches!(self.peek_at(1), Tok::Ident(n) if n == "self") {
            self.advance();
            self.advance();
            Some(SelfKind::Value { mutable: true })
        } else if self.at_ident("self") {
            self.advance();
            Some(SelfKind::Value { mutable: false })
        } else {
            None
        };
        if self_kind.is_some() {
            if !in_impl {
                return Err(Error::syntax(self_span, "`self` parameters are only allowed in `impl` blocks"));
            }
            if !self.at(&Tok::RParen) {
                self.expect(Tok::Comma, "after `self`")?;
            }
        }

        let mut params: Vec<Param> = Vec::new();
        while !self.at(&Tok::RParen) {
            let mutable = self.eat(&Tok::Mut);
            let (pname, pspan) = self.ident("parameter name")?;
            if pname == "self" {
                return Err(Error::syntax(pspan, "`self` must be the first parameter"));
            }
            if params.iter().any(|p| p.name == pname) {
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
        Ok(FnDecl { name, self_kind, params, ret, body, span })
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

    fn enum_decl(&mut self) -> Result<EnumDecl> {
        let span = self.expect(Tok::Enum, "")?.span;
        let (name, _) = self.ident("enum name")?;
        self.expect(Tok::LBrace, "after enum name")?;
        let mut variants: Vec<(String, Vec<Type>)> = Vec::new();
        while !self.at(&Tok::RBrace) {
            let (vname, vspan) = self.ident("variant name")?;
            if variants.iter().any(|(n, _)| *n == vname) {
                return Err(Error::syntax(vspan, format!("variant `{}` is declared more than once", vname)));
            }
            let types = if self.eat(&Tok::LParen) {
                self.list(Tok::RParen, "to close variant fields", |p| p.ty())?
            } else {
                Vec::new()
            };
            variants.push((vname, types));
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RBrace, "to close enum")?;
        Ok(EnumDecl { name, variants, span })
    }

    fn impl_decl(&mut self) -> Result<ImplDecl> {
        let span = self.expect(Tok::Impl, "")?.span;
        let (type_name, _) = self.ident("type name after `impl`")?;
        self.expect(Tok::LBrace, "after impl type")?;
        let saved = self.impl_type.replace(type_name.clone());
        let mut methods: Vec<FnDecl> = Vec::new();
        let result = (|| {
            while !self.at(&Tok::RBrace) {
                if !self.at(&Tok::Fn) {
                    return Err(Error::syntax(
                        self.span(),
                        format!("expected `fn` inside `impl`, found {}", self.peek().describe()),
                    ));
                }
                let f = self.fn_decl(true)?;
                if methods.iter().any(|m| m.name == f.name) {
                    return Err(Error::syntax(f.span, format!("method `{}` is defined more than once", f.name)));
                }
                methods.push(f);
            }
            Ok(())
        })();
        self.impl_type = saved;
        result?;
        self.expect(Tok::RBrace, "to close impl")?;
        Ok(ImplDecl { type_name, methods, span })
    }

    fn const_decl(&mut self) -> Result<ConstDecl> {
        let span = self.expect(Tok::Const, "")?.span;
        let (name, _) = self.ident("constant name")?;
        let ty = if self.eat(&Tok::Colon) { Some(self.ty()?) } else { None };
        self.expect(Tok::Eq, "in `const`")?;
        let value = self.expr()?;
        self.expect(Tok::Semi, "after `const`")?;
        Ok(ConstDecl { name, ty, value, span })
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
        let name = match self.peek().clone() {
            Tok::Ident(name) => {
                self.advance();
                name
            }
            other => return Err(Error::syntax(span, format!("expected a type, found {}", other.describe()))),
        };
        Ok(match name.as_str() {
            "int" | "i64" => Type::Int,
            "float" | "f64" => Type::Float,
            "bool" => Type::Bool,
            "str" | "String" => Type::Str,
            _ => Type::Named(self.resolve_self(name, span)?),
        })
    }

    // ---------- statements ----------

    fn block(&mut self) -> Result<Block> {
        self.expect(Tok::LBrace, "to start a block")?;
        let block = self.with_struct(true, |p| {
            let mut stmts = Vec::new();
            let mut tail = None;
            while !p.at(&Tok::RBrace) {
                if p.at(&Tok::Eof) {
                    return Err(Error::syntax(p.span(), "unexpected end of file: missing `}`"));
                }
                match p.stmt_or_tail(false)? {
                    Parsed::Stmt(s) => stmts.push(s),
                    Parsed::Tail(e) => {
                        tail = Some(Box::new(e));
                        break;
                    }
                }
            }
            Ok(Block { stmts, tail })
        })?;
        self.expect(Tok::RBrace, "to close block")?;
        Ok(block)
    }

    fn stmt_or_tail(&mut self, top_level: bool) -> Result<Parsed> {
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
            Tok::Semi => {
                // A stray `;` is an empty statement.
                self.advance();
                let empty = Expr { kind: ExprKind::Block(Block { stmts: vec![], tail: None }), span };
                StmtKind::Expr(empty)
            }
            _ => {
                // `if`, `match`, `loop` and `{}` don't need a `;` when used as statements.
                let block_like = matches!(self.peek(), Tok::If | Tok::Match | Tok::Loop | Tok::LBrace);
                let e = if block_like { self.block_expr()? } else { self.expr()? };

                let op = match self.peek() {
                    Tok::Eq => Some(None),
                    Tok::PlusEq => Some(Some(BinOp::Add)),
                    Tok::MinusEq => Some(Some(BinOp::Sub)),
                    Tok::StarEq => Some(Some(BinOp::Mul)),
                    Tok::SlashEq => Some(Some(BinOp::Div)),
                    Tok::PercentEq => Some(Some(BinOp::Rem)),
                    _ => None,
                };
                if let (Some(op), false) = (op, block_like) {
                    let op_span = self.span();
                    self.advance();
                    if !matches!(e.kind, ExprKind::Var(_) | ExprKind::Index(..) | ExprKind::Field(..)) {
                        return Err(Error::syntax(op_span, "invalid left-hand side of assignment"));
                    }
                    let value = self.expr()?;
                    self.expect(Tok::Semi, "after assignment")?;
                    StmtKind::Assign { target: e, op, value }
                } else if !top_level && self.at(&Tok::RBrace) {
                    return Ok(Parsed::Tail(e));
                } else if top_level && self.repl && self.at(&Tok::Eof) {
                    StmtKind::Echo(e)
                } else if block_like {
                    self.eat(&Tok::Semi);
                    StmtKind::Expr(e)
                } else {
                    self.expect(Tok::Semi, "after expression")?;
                    StmtKind::Expr(e)
                }
            }
        };
        Ok(Parsed::Stmt(Stmt { kind, span }))
    }

    // ---------- expressions (lowest to highest precedence) ----------

    fn expr(&mut self) -> Result<Expr> {
        self.or()
    }

    fn binary_level(&mut self, next: fn(&mut Self) -> Result<Expr>, ops: &[(Tok, BinOp)]) -> Result<Expr> {
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
        self.with_struct(true, |p| p.list(Tok::RParen, "to close argument list", |p| p.expr()))
    }

    fn looks_like_struct_lit(&self) -> bool {
        !self.no_struct
            && self.peek_at(1) == &Tok::LBrace
            && (self.peek_at(2) == &Tok::RBrace
                || (matches!(self.peek_at(2), Tok::Ident(_)) && self.peek_at(3) == &Tok::Colon))
    }

    /// Can the current token start an expression? (Used after `return` / `break`.)
    fn starts_expr(&self) -> bool {
        !matches!(
            self.peek(),
            Tok::Semi | Tok::RBrace | Tok::RParen | Tok::RBracket | Tok::Comma | Tok::Eof
        )
    }

    /// `if`, `match`, `loop` or a `{ block }`.
    fn block_expr(&mut self) -> Result<Expr> {
        let span = self.span();
        let kind = match self.peek() {
            Tok::If => return self.if_expr(),
            Tok::Match => return self.match_expr(),
            Tok::Loop => {
                self.advance();
                ExprKind::Loop(self.block()?)
            }
            _ => ExprKind::Block(self.block()?),
        };
        Ok(Expr { kind, span })
    }

    fn if_expr(&mut self) -> Result<Expr> {
        let span = self.expect(Tok::If, "")?.span;
        let cond = self.with_struct(false, |p| p.expr())?;
        let then = self.block()?;
        let else_ = if self.eat(&Tok::Else) {
            if self.at(&Tok::If) {
                Some(Box::new(self.if_expr()?))
            } else {
                let espan = self.span();
                Some(Box::new(Expr { kind: ExprKind::Block(self.block()?), span: espan }))
            }
        } else {
            None
        };
        Ok(Expr { kind: ExprKind::If(Box::new(cond), then, else_), span })
    }

    fn match_expr(&mut self) -> Result<Expr> {
        let span = self.expect(Tok::Match, "")?.span;
        let scrutinee = self.with_struct(false, |p| p.expr())?;
        self.expect(Tok::LBrace, "after `match` value")?;
        let arms = self.with_struct(true, |p| {
            let mut arms = Vec::new();
            while !p.at(&Tok::RBrace) {
                let pattern = p.pattern()?;
                let guard = if p.eat(&Tok::If) { Some(p.expr()?) } else { None };
                p.expect(Tok::FatArrow, "after match pattern")?;
                let body = if p.at(&Tok::LBrace) {
                    let b = p.block_expr()?;
                    p.eat(&Tok::Comma);
                    b
                } else {
                    let b = p.expr()?;
                    if !p.at(&Tok::RBrace) {
                        p.expect(Tok::Comma, "after match arm")?;
                    }
                    b
                };
                arms.push(MatchArm { pattern, guard, body });
            }
            Ok(arms)
        })?;
        self.expect(Tok::RBrace, "to close `match`")?;
        if arms.is_empty() {
            return Err(Error::syntax(span, "`match` needs at least one arm"));
        }
        Ok(Expr { kind: ExprKind::Match(Box::new(scrutinee), arms), span })
    }

    fn pattern(&mut self) -> Result<Pattern> {
        let first = self.pattern_atom()?;
        if !self.at(&Tok::Pipe) {
            return Ok(first);
        }
        let mut alts = vec![first];
        while self.eat(&Tok::Pipe) {
            alts.push(self.pattern_atom()?);
        }
        Ok(Pattern::Or(alts))
    }

    fn pattern_int(&mut self) -> Result<Option<i64>> {
        let neg = self.at(&Tok::Minus) && matches!(self.peek_at(1), Tok::Int(_));
        if neg {
            self.advance();
        }
        match *self.peek() {
            Tok::Int(n) => {
                self.advance();
                Ok(Some(if neg { -n } else { n }))
            }
            _ => Ok(None),
        }
    }

    fn pattern_atom(&mut self) -> Result<Pattern> {
        let span = self.span();
        if let Some(lo) = self.pattern_int()? {
            let inclusive = if self.eat(&Tok::DotDotEq) {
                true
            } else if self.eat(&Tok::DotDot) {
                false
            } else {
                return Ok(Pattern::Int(lo));
            };
            let hi = self
                .pattern_int()?
                .ok_or_else(|| Error::syntax(self.span(), "expected an integer to end the range pattern"))?;
            return Ok(Pattern::Range(lo, hi, inclusive));
        }
        match self.peek().clone() {
            Tok::Str(s) => {
                self.advance();
                Ok(Pattern::Str(s))
            }
            Tok::True => {
                self.advance();
                Ok(Pattern::Bool(true))
            }
            Tok::False => {
                self.advance();
                Ok(Pattern::Bool(false))
            }
            Tok::Ident(name) if name == "_" => {
                self.advance();
                Ok(Pattern::Wildcard)
            }
            Tok::Ident(name) => {
                self.advance();
                if !self.eat(&Tok::ColonColon) {
                    return Ok(Pattern::Bind(name));
                }
                let enum_name = self.resolve_self(name, span)?;
                let (variant, _) = self.ident("variant name")?;
                let fields = if self.eat(&Tok::LParen) {
                    Some(self.list(Tok::RParen, "to close pattern", |p| p.pattern())?)
                } else {
                    None
                };
                Ok(Pattern::Variant { enum_name, variant, fields, span })
            }
            other => Err(Error::syntax(span, format!("expected a pattern, found {}", other.describe()))),
        }
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
            Tok::If | Tok::Match | Tok::Loop | Tok::LBrace => return self.block_expr(),
            Tok::Return => {
                self.advance();
                let value = if self.starts_expr() { Some(Box::new(self.expr()?)) } else { None };
                ExprKind::Return(value)
            }
            Tok::Break => {
                self.advance();
                let value = if self.starts_expr() { Some(Box::new(self.expr()?)) } else { None };
                ExprKind::Break(value)
            }
            Tok::Continue => {
                self.advance();
                ExprKind::Continue
            }
            Tok::Ident(name) => {
                if self.peek_at(1) == &Tok::ColonColon {
                    let ty = self.resolve_self(name, span)?;
                    self.advance();
                    self.advance();
                    let (item, _) = self.ident("name after `::`")?;
                    if self.at(&Tok::LParen) {
                        ExprKind::PathCall(ty, item, self.args()?)
                    } else {
                        ExprKind::Path(ty, item)
                    }
                } else if self.peek_at(1) == &Tok::LParen {
                    self.advance();
                    ExprKind::Call(name, self.args()?)
                } else if self.looks_like_struct_lit() {
                    let name = self.resolve_self(name, span)?;
                    self.advance();
                    self.advance(); // {
                    let fields = self.with_struct(true, |p| {
                        p.list(Tok::RBrace, "to close struct literal", |p| {
                            let (fname, _) = p.ident("field name")?;
                            p.expect(Tok::Colon, "after field name")?;
                            Ok((fname, p.expr()?))
                        })
                    })?;
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
                let elems = self.with_struct(true, |p| p.list(Tok::RBracket, "to close array literal", |p| p.expr()))?;
                ExprKind::Array(elems)
            }
            other => {
                return Err(Error::syntax(span, format!("expected an expression, found {}", other.describe())))
            }
        };
        Ok(Expr { kind, span })
    }
}

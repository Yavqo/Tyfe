use crate::ast::*;
use crate::error::{Error, Result};
use crate::lexer::{lex, Tok, Token};
use std::collections::HashMap;
use std::rc::Rc;

/// One top-level entry typed into the REPL.
pub enum ReplEntry {
    Item(Item),
    Stmt(Stmt),
}

/// Parse a whole source file. `file` is the index used in spans for error messages.
pub fn parse_program(src: &str, file: u16) -> Result<Vec<Item>> {
    let mut p = Parser::new(lex(src, file)?, false);
    let mut items = Vec::new();
    while !p.at(&Tok::Eof) {
        items.extend(p.items()?);
    }
    Ok(items)
}

pub fn parse_repl(src: &str) -> Result<Vec<ReplEntry>> {
    let mut p = Parser::new(lex(src, 0)?, true);
    let mut entries = Vec::new();
    while !p.at(&Tok::Eof) {
        if p.starts_item() {
            entries.extend(p.items()?.into_iter().map(ReplEntry::Item));
        } else {
            match p.stmt_or_tail(true)? {
                Parsed::Stmt(s) => entries.push(ReplEntry::Stmt(s)),
                Parsed::Tail(e) => return Err(Error::syntax(e.span, "unexpected `}`")),
            }
        }
    }
    Ok(entries)
}

// Short-lived parse result, so the size difference between variants does not matter.
#[allow(clippy::large_enum_variant)]
enum Parsed {
    Stmt(Stmt),
    /// The final expression of a block, with no `;` — the block's value.
    Tail(Expr),
}

/// Macros that are just calls to the built-in function of the same name.
const CALL_MACROS: &[&str] = &[
    "println", "print", "eprintln", "eprint", "format", "panic", "assert", "assert_eq", "assert_ne", "todo",
    "unreachable", "unimplemented", "dbg", "write", "writeln",
];

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    /// When true, `Name {` is not parsed as a struct literal
    /// (so `if x == y { ... }` works, like in Rust).
    no_struct: bool,
    repl: bool,
    /// The type `Self` refers to while parsing an `impl` block.
    impl_type: Option<String>,
    /// Generic parameters in scope (`T`, `U`, ...) with their trait bounds.
    generics: Vec<(String, Vec<String>)>,
    /// `type Name = ...;` aliases seen so far.
    aliases: HashMap<String, Type>,
}

impl Parser {
    fn new(toks: Vec<Token>, repl: bool) -> Self {
        Parser {
            toks,
            pos: 0,
            no_struct: false,
            repl,
            impl_type: None,
            generics: Vec::new(),
            aliases: HashMap::new(),
        }
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

    /// Expect a `>`, splitting a `>>` token in two (for `Option<Vec<int>>`).
    fn expect_gt(&mut self, context: &str) -> Result<()> {
        if self.at(&Tok::Shr) {
            self.toks[self.pos].tok = Tok::Gt;
            return Ok(());
        }
        if self.at(&Tok::Ge) {
            self.toks[self.pos].tok = Tok::Eq;
            return Ok(());
        }
        self.expect(Tok::Gt, context).map(|_| ())
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

    /// Skip a balanced `<...>` group (generic arguments we don't need).
    fn skip_angle_brackets(&mut self) -> Result<()> {
        let start = self.span();
        let mut depth = 0;
        loop {
            match self.peek() {
                Tok::Lt => depth += 1,
                Tok::Gt => depth -= 1,
                Tok::Shr => depth -= 2,
                Tok::Eof => return Err(Error::syntax(start, "unclosed `<`")),
                _ => {}
            }
            self.advance();
            if depth <= 0 {
                return Ok(());
            }
        }
    }

    // ---------- items ----------

    fn starts_item(&self) -> bool {
        matches!(
            self.peek(),
            Tok::Fn | Tok::Struct | Tok::Enum | Tok::Impl | Tok::Const | Tok::Trait | Tok::Mod | Tok::Use | Tok::Pub | Tok::Hash
        ) || self.at_ident("type")
            || self.at_ident("static")
    }

    /// `#[...]` attributes. Returns true if one of them was `#[test]`.
    fn attributes(&mut self) -> Result<bool> {
        let mut is_test = false;
        while self.at(&Tok::Hash) {
            self.advance();
            self.eat(&Tok::Bang);
            self.expect(Tok::LBracket, "after `#`")?;
            let mut depth = 1;
            while depth > 0 {
                match self.advance().tok {
                    Tok::LBracket => depth += 1,
                    Tok::RBracket => depth -= 1,
                    Tok::Ident(n) if n == "test" && depth == 1 => is_test = true,
                    Tok::Eof => return Err(Error::syntax(self.span(), "unclosed attribute `#[...]`")),
                    _ => {}
                }
            }
        }
        Ok(is_test)
    }

    /// `pub`, `pub(crate)`, ... — accepted and ignored.
    fn visibility(&mut self) {
        if self.eat(&Tok::Pub) && self.at(&Tok::LParen) {
            while !self.at(&Tok::RParen) && !self.at(&Tok::Eof) {
                self.advance();
            }
            self.advance();
        }
    }

    /// One item; `use` produces nothing and an inline `mod { }` produces several.
    fn items(&mut self) -> Result<Vec<Item>> {
        let is_test = self.attributes()?;
        self.visibility();
        let span = self.span();
        let item = match self.peek() {
            Tok::Fn => {
                let mut f = self.fn_decl(false, false)?.0;
                f.is_test = is_test;
                Item::Fn(f)
            }
            Tok::Struct => Item::Struct(self.struct_decl()?),
            Tok::Enum => Item::Enum(self.enum_decl()?),
            Tok::Impl => Item::Impl(self.impl_decl()?),
            Tok::Const => Item::Const(self.const_decl()?),
            Tok::Ident(n) if n == "static" => Item::Const(self.const_decl()?),
            Tok::Trait => Item::Trait(self.trait_decl()?),
            Tok::Ident(n) if n == "type" => {
                self.advance();
                let (name, _) = self.ident("type alias name")?;
                self.expect(Tok::Eq, "in `type` alias")?;
                let ty = self.ty()?;
                self.expect(Tok::Semi, "after `type` alias")?;
                self.aliases.insert(name, ty);
                return Ok(Vec::new());
            }
            Tok::Use => {
                // `use std::collections::HashMap;` etc. — everything is already in scope.
                while !self.at(&Tok::Semi) && !self.at(&Tok::Eof) {
                    self.advance();
                }
                self.expect(Tok::Semi, "after `use`")?;
                return Ok(Vec::new());
            }
            Tok::Mod => {
                self.advance();
                let (name, _) = self.ident("module name")?;
                if self.eat(&Tok::LBrace) {
                    let mut items = Vec::new();
                    while !self.at(&Tok::RBrace) {
                        if self.at(&Tok::Eof) {
                            return Err(Error::syntax(self.span(), "unexpected end of file: missing `}`"));
                        }
                        items.extend(self.items()?);
                    }
                    self.advance();
                    return Ok(items);
                }
                self.expect(Tok::Semi, "after `mod` name")?;
                Item::Mod(name, span)
            }
            other => {
                return Err(Error::syntax(
                    span,
                    format!(
                        "expected an item (`fn`, `struct`, `enum`, `impl`, `trait`, `const`, `mod` or `use`) at top level, found {}",
                        other.describe()
                    ),
                ))
            }
        };
        Ok(vec![item])
    }

    /// `<T, U: Display + Clone>` after a fn/struct/enum/impl name.
    fn generic_params(&mut self) -> Result<Vec<(String, Vec<String>)>> {
        if !self.eat(&Tok::Lt) {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        while !self.at(&Tok::Gt) {
            if let Tok::Label(_) = self.peek() {
                self.advance(); // lifetimes are ignored
            } else {
                let (name, _) = self.ident("generic parameter name")?;
                let bounds = if self.eat(&Tok::Colon) { self.bounds()? } else { Vec::new() };
                out.push((name, bounds));
            }
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect_gt("to close generic parameters")?;
        Ok(out)
    }

    /// `Display + Clone` (lifetimes and `?Sized` are skipped).
    fn bounds(&mut self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        loop {
            match self.peek().clone() {
                Tok::Label(_) => {
                    self.advance();
                }
                Tok::Question => {
                    self.advance();
                    self.ident("trait name")?;
                }
                _ => out.push(self.path_name("trait name")?),
            }
            if !self.eat(&Tok::Plus) {
                return Ok(out);
            }
        }
    }

    /// `where T: A + B, U: C` — merges bounds into the generic parameters in scope.
    fn where_clause(&mut self) -> Result<()> {
        if !self.eat(&Tok::Where) {
            return Ok(());
        }
        while !self.at(&Tok::LBrace) && !self.at(&Tok::Semi) {
            let (name, span) = self.ident("type in `where` clause")?;
            self.expect(Tok::Colon, "in `where` clause")?;
            let bounds = self.bounds()?;
            match self.generics.iter_mut().rev().find(|(n, _)| *n == name) {
                Some((_, b)) => b.extend(bounds),
                None => return Err(Error::syntax(span, format!("`{}` is not a generic parameter here", name))),
            }
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        Ok(())
    }

    /// Fill in trait bounds for every generic parameter used in `t`.
    fn apply_bounds(&self, t: &mut Type) {
        match t {
            Type::Generic(name, bounds) => {
                if let Some((_, b)) = self.generics.iter().rev().find(|(n, _)| n == name) {
                    *bounds = b.clone();
                }
            }
            Type::Array(inner) | Type::Option(inner) | Type::Set(inner) => self.apply_bounds(inner),
            Type::Result(a, b) | Type::Map(a, b) => {
                self.apply_bounds(a);
                self.apply_bounds(b);
            }
            Type::Tuple(ts) => ts.iter_mut().for_each(|t| self.apply_bounds(t)),
            Type::Fn(ps, r) => {
                ps.iter_mut().for_each(|t| self.apply_bounds(t));
                self.apply_bounds(r);
            }
            _ => {}
        }
    }

    /// Parse a function. With `allow_no_body` (inside a trait), a signature
    /// ending in `;` is accepted; the bool result says whether a body was present.
    fn fn_decl(&mut self, in_impl: bool, allow_no_body: bool) -> Result<(FnDecl, bool)> {
        let span = self.expect(Tok::Fn, "")?.span;
        let (name, _) = self.ident("function name")?;
        let gens = self.generic_params()?;
        let n_gens = gens.len();
        self.generics.extend(gens);
        let result = self.fn_rest(name, span, in_impl, allow_no_body);
        self.generics.truncate(self.generics.len() - n_gens);
        result
    }

    fn fn_rest(&mut self, name: String, span: Span, in_impl: bool, allow_no_body: bool) -> Result<(FnDecl, bool)> {
        self.expect(Tok::LParen, "after function name")?;

        // Optional `self`, `mut self`, `&self` or `&mut self` receiver.
        let self_span = self.span();
        let self_kind = if self.at(&Tok::Amp) && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Mut | Tok::Label(_)) && {
            let mut i = 1;
            if matches!(self.peek_at(i), Tok::Label(_)) {
                i += 1;
            }
            if self.peek_at(i) == &Tok::Mut {
                i += 1;
            }
            matches!(self.peek_at(i), Tok::Ident(n) if n == "self")
        } {
            self.advance();
            if let Tok::Label(_) = self.peek() {
                self.advance();
            }
            let mutable = self.eat(&Tok::Mut);
            self.advance(); // self
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
                return Err(Error::syntax(self_span, "`self` parameters are only allowed in `impl` and `trait` blocks"));
            }
            if self.eat(&Tok::Colon) {
                self.ty()?; // `self: &Self`
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
            if pname != "_" && params.iter().any(|p| p.name == pname) {
                return Err(Error::syntax(pspan, format!("parameter `{}` is declared more than once", pname)));
            }
            self.expect(Tok::Colon, "after parameter name (parameters need a type, e.g. `x: int`)")?;
            let mut by_ref_mut = false;
            if self.at(&Tok::Amp) {
                let mut i = 1;
                if matches!(self.peek_at(i), Tok::Label(_)) {
                    i += 1;
                }
                by_ref_mut = self.peek_at(i) == &Tok::Mut;
            }
            let mut ty = self.ty()?;
            self.apply_bounds(&mut ty);
            params.push(Param { name: pname, ty, mutable: mutable || by_ref_mut, by_ref_mut });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen, "to close parameter list")?;
        let mut ret = if self.eat(&Tok::Arrow) { self.ty()? } else { Type::Unit };
        self.where_clause()?;
        self.apply_bounds(&mut ret);
        for p in &mut params {
            let mut t = std::mem::replace(&mut p.ty, Type::Unit);
            self.apply_bounds(&mut t);
            p.ty = t;
        }
        let has_body = !(allow_no_body && self.eat(&Tok::Semi));
        let body = if has_body { self.block()? } else { Block { stmts: Vec::new(), tail: None } };
        Ok((FnDecl { name, self_kind, params, ret, body, span, is_test: false }, has_body))
    }

    fn struct_decl(&mut self) -> Result<StructDecl> {
        let span = self.expect(Tok::Struct, "")?.span;
        let (name, _) = self.ident("struct name")?;
        let gens = self.generic_params()?;
        let n_gens = gens.len();
        self.generics.extend(gens);
        let result = (|| {
            self.where_clause()?;
            let mut fields: Vec<(String, Type)> = Vec::new();
            if self.eat(&Tok::Semi) {
                return Ok(fields); // unit struct: `struct Marker;`
            }
            self.expect(Tok::LBrace, "after struct name")?;
            while !self.at(&Tok::RBrace) {
                self.attributes()?;
                self.visibility();
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
            Ok(fields)
        })();
        self.generics.truncate(self.generics.len() - n_gens);
        Ok(StructDecl { name, fields: result?, span })
    }

    fn enum_decl(&mut self) -> Result<EnumDecl> {
        let span = self.expect(Tok::Enum, "")?.span;
        let (name, _) = self.ident("enum name")?;
        let gens = self.generic_params()?;
        let n_gens = gens.len();
        self.generics.extend(gens);
        let result = (|| {
            self.expect(Tok::LBrace, "after enum name")?;
            let mut variants: Vec<(String, Vec<Type>)> = Vec::new();
            let mut named: Vec<(String, Vec<String>)> = Vec::new();
            while !self.at(&Tok::RBrace) {
                self.attributes()?;
                let (vname, vspan) = self.ident("variant name")?;
                if variants.iter().any(|(n, _)| *n == vname) {
                    return Err(Error::syntax(vspan, format!("variant `{}` is declared more than once", vname)));
                }
                let types = if self.eat(&Tok::LParen) {
                    self.list(Tok::RParen, "to close variant fields", |p| p.ty())?
                } else if self.eat(&Tok::LBrace) {
                    let fields = self.list(Tok::RBrace, "to close variant fields", |p| {
                        let (f, _) = p.ident("field name")?;
                        p.expect(Tok::Colon, "after field name")?;
                        Ok((f, p.ty()?))
                    })?;
                    named.push((vname.clone(), fields.iter().map(|(f, _)| f.clone()).collect()));
                    fields.into_iter().map(|(_, t)| t).collect()
                } else {
                    Vec::new()
                };
                if self.eat(&Tok::Eq) {
                    self.expr()?; // explicit discriminants (`A = 1`) are accepted and ignored
                }
                variants.push((vname, types));
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(Tok::RBrace, "to close enum")?;
            Ok((variants, named))
        })();
        self.generics.truncate(self.generics.len() - n_gens);
        let (variants, named) = result?;
        Ok(EnumDecl { name, variants, named, span })
    }

    /// `impl Type { }`, `impl<T> Type<T> { }` or `impl Trait for Type { }`
    fn impl_decl(&mut self) -> Result<ImplDecl> {
        let span = self.expect(Tok::Impl, "")?.span;
        let gens = self.generic_params()?;
        let n_gens = gens.len();
        self.generics.extend(gens);
        let result = self.impl_rest(span);
        self.generics.truncate(self.generics.len() - n_gens);
        result
    }

    fn impl_rest(&mut self, span: Span) -> Result<ImplDecl> {
        let first = self.path_name("type name after `impl`")?;
        let (trait_name, type_name) = if self.eat(&Tok::For) {
            self.eat(&Tok::Amp);
            let ty = self.path_name("type name after `for`")?;
            (Some(first), ty)
        } else {
            (None, first)
        };
        self.where_clause()?;
        self.expect(Tok::LBrace, "after impl type")?;
        let saved = self.impl_type.replace(type_name.clone());
        let mut methods: Vec<FnDecl> = Vec::new();
        let result = (|| {
            while !self.at(&Tok::RBrace) {
                self.attributes()?;
                self.visibility();
                if !self.at(&Tok::Fn) {
                    return Err(Error::syntax(
                        self.span(),
                        format!("expected `fn` inside `impl`, found {}", self.peek().describe()),
                    ));
                }
                let f = self.fn_decl(true, false)?.0;
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
        Ok(ImplDecl { type_name, trait_name, methods, span })
    }

    /// `fmt::Display` / `Pair<T>` -> the last name, ignoring generic arguments.
    fn path_name(&mut self, what: &str) -> Result<String> {
        let (mut name, _) = self.ident(what)?;
        while self.eat(&Tok::ColonColon) {
            name = self.ident(what)?.0;
        }
        if self.at(&Tok::Lt) {
            self.skip_angle_brackets()?;
        }
        Ok(name)
    }

    fn trait_decl(&mut self) -> Result<TraitDecl> {
        let span = self.expect(Tok::Trait, "")?.span;
        let (name, _) = self.ident("trait name")?;
        if self.at(&Tok::Lt) {
            self.skip_angle_brackets()?;
        }
        if self.eat(&Tok::Colon) {
            self.bounds()?; // supertraits are accepted but not enforced
        }
        self.expect(Tok::LBrace, "after trait name")?;
        let saved = self.impl_type.replace("Self".to_string());
        let mut required = Vec::new();
        let mut defaults = Vec::new();
        let result = (|| {
            while !self.at(&Tok::RBrace) {
                self.attributes()?;
                if !self.at(&Tok::Fn) {
                    return Err(Error::syntax(
                        self.span(),
                        format!("expected `fn` inside `trait`, found {}", self.peek().describe()),
                    ));
                }
                let (f, has_body) = self.fn_decl(true, true)?;
                if has_body {
                    defaults.push(f);
                } else {
                    required.push(f.name);
                }
            }
            Ok(())
        })();
        self.impl_type = saved;
        result?;
        self.expect(Tok::RBrace, "to close trait")?;
        Ok(TraitDecl { name, required, defaults, span })
    }

    fn const_decl(&mut self) -> Result<ConstDecl> {
        let span = self.advance().span; // `const` or `static`
        self.eat(&Tok::Mut);
        let (name, _) = self.ident("constant name")?;
        let ty = if self.eat(&Tok::Colon) { Some(self.ty()?) } else { None };
        self.expect(Tok::Eq, "in `const`")?;
        let value = self.expr()?;
        self.expect(Tok::Semi, "after `const`")?;
        Ok(ConstDecl { name, ty, value, span })
    }

    fn ty(&mut self) -> Result<Type> {
        let span = self.span();
        // References are accepted in types; values are copied (see `&mut` params).
        if self.eat(&Tok::Amp) {
            if let Tok::Label(_) = self.peek() {
                self.advance();
            }
            self.eat(&Tok::Mut);
            return self.ty();
        }
        if self.at(&Tok::AndAnd) {
            self.advance();
            return self.ty();
        }
        if self.eat(&Tok::Dyn) || self.eat(&Tok::Impl) {
            return Ok(Type::Trait(self.bounds()?));
        }
        if self.eat(&Tok::LBracket) {
            let inner = self.ty()?;
            if self.eat(&Tok::Semi) {
                self.expr()?; // `[int; 3]` — the length isn't part of the type
            }
            self.expect(Tok::RBracket, "to close array type")?;
            return Ok(Type::Array(Box::new(inner)));
        }
        if self.eat(&Tok::LParen) {
            let mut types = self.list(Tok::RParen, "to close tuple type", |p| p.ty())?;
            return Ok(match types.len() {
                0 => Type::Unit,
                1 => types.pop().unwrap(),
                _ => Type::Tuple(types),
            });
        }
        if self.eat(&Tok::Fn) {
            self.expect(Tok::LParen, "after `fn` in a function type")?;
            let params = self.list(Tok::RParen, "to close function type", |p| p.ty())?;
            let ret = if self.eat(&Tok::Arrow) { self.ty()? } else { Type::Unit };
            return Ok(Type::Fn(params, Box::new(ret)));
        }
        let mut name = match self.peek().clone() {
            Tok::Ident(name) => {
                self.advance();
                name
            }
            other => return Err(Error::syntax(span, format!("expected a type, found {}", other.describe()))),
        };
        // `std::collections::HashMap` -> `HashMap`
        while self.eat(&Tok::ColonColon) {
            name = self.ident("type name after `::`")?.0;
        }
        // Generic arguments, e.g. `Vec<int>`.
        let mut args = Vec::new();
        if self.eat(&Tok::Lt) {
            args = self.type_args()?;
        }
        let arg = |args: &mut Vec<Type>, i: usize| args.get(i).cloned().unwrap_or(Type::Any);
        Ok(match name.as_str() {
            "int" | "i64" | "i32" | "i16" | "i8" | "isize" | "u64" | "u32" | "u16" | "u8" | "usize" | "i128" | "u128" => {
                Type::Int
            }
            "float" | "f64" | "f32" => Type::Float,
            "bool" => Type::Bool,
            "str" | "String" | "char" => Type::Str,
            "_" | "Formatter" => Type::Any,
            "Option" => Type::Option(Box::new(arg(&mut args, 0))),
            "Result" => Type::Result(Box::new(arg(&mut args, 0)), Box::new(arg(&mut args, 1))),
            "Vec" | "VecDeque" => Type::Array(Box::new(arg(&mut args, 0))),
            "Box" | "Rc" | "RefCell" | "Cell" | "Arc" => arg(&mut args, 0),
            "HashMap" | "BTreeMap" => Type::Map(Box::new(arg(&mut args, 0)), Box::new(arg(&mut args, 1))),
            "HashSet" | "BTreeSet" => Type::Set(Box::new(arg(&mut args, 0))),
            _ => {
                if let Some(t) = self.aliases.get(&name) {
                    t.clone()
                } else if self.generics.iter().any(|(g, _)| *g == name) {
                    Type::Generic(name, Vec::new())
                } else {
                    Type::Named(self.resolve_self(name, span)?)
                }
            }
        })
    }

    /// The inside of `<...>` in a type, after the `<`.
    fn type_args(&mut self) -> Result<Vec<Type>> {
        let mut args = Vec::new();
        while !self.at(&Tok::Gt) && !self.at(&Tok::Shr) {
            if let Tok::Label(_) = self.peek() {
                self.advance();
            } else {
                args.push(self.ty()?);
            }
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect_gt("to close type arguments")?;
        Ok(args)
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

    fn while_stmt(&mut self, label: Option<String>) -> Result<StmtKind> {
        self.expect(Tok::While, "")?;
        if self.eat(&Tok::Let) {
            let pattern = self.pattern()?;
            self.expect(Tok::Eq, "in `while let`")?;
            let value = self.with_struct(false, |p| p.expr())?;
            let body = self.block()?;
            Ok(StmtKind::WhileLet { pattern, value, body, label })
        } else {
            let cond = self.with_struct(false, |p| p.expr())?;
            let body = self.block()?;
            Ok(StmtKind::While { cond, body, label })
        }
    }

    fn for_stmt(&mut self, label: Option<String>) -> Result<StmtKind> {
        self.expect(Tok::For, "")?;
        let pattern = self.pattern()?;
        self.expect(Tok::In, "after loop variable")?;
        let iterable = self.with_struct(false, |p| p.expr())?;
        let iter = match iterable.kind {
            // `for i in a..b` counts without building an array.
            ExprKind::Range(Some(start), Some(end), inclusive) => ForIter::Range(*start, *end, inclusive),
            _ => ForIter::Each(iterable),
        };
        let body = self.block()?;
        Ok(StmtKind::For { pattern, iter, body, label })
    }

    fn stmt_or_tail(&mut self, top_level: bool) -> Result<Parsed> {
        let span = self.span();
        let kind = match self.peek().clone() {
            Tok::Let => {
                self.advance();
                let mutable = self.eat(&Tok::Mut);
                if !matches!(self.peek(), Tok::Ident(_) | Tok::LParen) {
                    return Err(Error::syntax(
                        self.span(),
                        format!("expected variable name after `let`, found {}", self.peek().describe()),
                    ));
                }
                let pattern = self.pattern()?;
                let ty = if self.eat(&Tok::Colon) { Some(self.ty()?) } else { None };
                self.expect(Tok::Eq, "in `let` (variables must be initialized)")?;
                let value = self.expr()?;
                self.expect(Tok::Semi, "after `let` statement")?;
                StmtKind::Let { pattern, mutable, ty, value }
            }
            Tok::While => self.while_stmt(None)?,
            Tok::For => self.for_stmt(None)?,
            Tok::Label(label) if self.peek_at(1) == &Tok::Colon => match self.peek_at(2) {
                Tok::While => {
                    self.advance();
                    self.advance();
                    self.while_stmt(Some(label))?
                }
                Tok::For => {
                    self.advance();
                    self.advance();
                    self.for_stmt(Some(label))?
                }
                _ => {
                    let e = self.expr()?;
                    return self.finish_expr_stmt(e, true, top_level, span);
                }
            },
            Tok::Use => {
                while !self.at(&Tok::Semi) && !self.at(&Tok::Eof) {
                    self.advance();
                }
                self.expect(Tok::Semi, "after `use`")?;
                StmtKind::Expr(Expr { kind: ExprKind::Unit, span })
            }
            Tok::Semi => {
                // A stray `;` is an empty statement.
                self.advance();
                StmtKind::Expr(Expr { kind: ExprKind::Unit, span })
            }
            _ => {
                // `if`, `match`, `loop` and `{}` don't need a `;` when used as statements.
                let block_like = matches!(self.peek(), Tok::If | Tok::Match | Tok::Loop | Tok::LBrace);
                let e = if block_like { self.block_expr(None)? } else { self.expr()? };
                return self.finish_expr_stmt(e, block_like, top_level, span);
            }
        };
        Ok(Parsed::Stmt(Stmt { kind, span }))
    }

    /// After an expression at statement start: assignment, tail value, or `expr;`.
    fn finish_expr_stmt(&mut self, e: Expr, block_like: bool, top_level: bool, span: Span) -> Result<Parsed> {
        let op = match self.peek() {
            Tok::Eq => Some(None),
            Tok::PlusEq => Some(Some(BinOp::Add)),
            Tok::MinusEq => Some(Some(BinOp::Sub)),
            Tok::StarEq => Some(Some(BinOp::Mul)),
            Tok::SlashEq => Some(Some(BinOp::Div)),
            Tok::PercentEq => Some(Some(BinOp::Rem)),
            Tok::AmpEq => Some(Some(BinOp::BitAnd)),
            Tok::PipeEq => Some(Some(BinOp::BitOr)),
            Tok::CaretEq => Some(Some(BinOp::BitXor)),
            Tok::ShlEq => Some(Some(BinOp::Shl)),
            Tok::ShrEq => Some(Some(BinOp::Shr)),
            _ => None,
        };
        let kind = if let (Some(op), false) = (op, block_like) {
            let op_span = self.span();
            self.advance();
            if !is_place(&e) {
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
        };
        Ok(Parsed::Stmt(Stmt { kind, span }))
    }

    // ---------- expressions (lowest to highest precedence) ----------

    fn expr(&mut self) -> Result<Expr> {
        let span = self.span();
        // `..end` / `..`
        if self.at(&Tok::DotDot) || self.at(&Tok::DotDotEq) {
            let inclusive = self.advance().tok == Tok::DotDotEq;
            let end = if self.range_end_follows() { Some(Box::new(self.or()?)) } else { None };
            return Ok(Expr { kind: ExprKind::Range(None, end, inclusive), span });
        }
        let left = self.or()?;
        if self.at(&Tok::DotDot) || self.at(&Tok::DotDotEq) {
            let span = self.span();
            let inclusive = self.advance().tok == Tok::DotDotEq;
            let end = if self.range_end_follows() { Some(Box::new(self.or()?)) } else { None };
            return Ok(Expr { kind: ExprKind::Range(Some(Box::new(left)), end, inclusive), span });
        }
        Ok(left)
    }

    fn range_end_follows(&self) -> bool {
        self.starts_expr() && !(self.no_struct && self.at(&Tok::LBrace)) && !self.at(&Tok::FatArrow)
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
            Self::bit_or,
            &[(Tok::Lt, BinOp::Lt), (Tok::Le, BinOp::Le), (Tok::Gt, BinOp::Gt), (Tok::Ge, BinOp::Ge)],
        )
    }

    fn bit_or(&mut self) -> Result<Expr> {
        self.binary_level(Self::bit_xor, &[(Tok::Pipe, BinOp::BitOr)])
    }

    fn bit_xor(&mut self) -> Result<Expr> {
        self.binary_level(Self::bit_and, &[(Tok::Caret, BinOp::BitXor)])
    }

    fn bit_and(&mut self) -> Result<Expr> {
        self.binary_level(Self::shift, &[(Tok::Amp, BinOp::BitAnd)])
    }

    fn shift(&mut self) -> Result<Expr> {
        self.binary_level(Self::additive, &[(Tok::Shl, BinOp::Shl), (Tok::Shr, BinOp::Shr)])
    }

    fn additive(&mut self) -> Result<Expr> {
        self.binary_level(Self::multiplicative, &[(Tok::Plus, BinOp::Add), (Tok::Minus, BinOp::Sub)])
    }

    fn multiplicative(&mut self) -> Result<Expr> {
        self.binary_level(
            Self::cast,
            &[(Tok::Star, BinOp::Mul), (Tok::Slash, BinOp::Div), (Tok::Percent, BinOp::Rem)],
        )
    }

    /// `expr as Type`
    fn cast(&mut self) -> Result<Expr> {
        let mut e = self.unary()?;
        while self.at(&Tok::As) {
            let span = self.advance().span;
            let ty = self.ty()?;
            e = Expr { kind: ExprKind::Cast(Box::new(e), ty), span };
        }
        Ok(e)
    }

    fn unary(&mut self) -> Result<Expr> {
        let span = self.span();
        let kind = match self.peek() {
            Tok::Minus => {
                self.advance();
                ExprKind::Unary(UnOp::Neg, Box::new(self.unary()?))
            }
            Tok::Bang => {
                self.advance();
                ExprKind::Unary(UnOp::Not, Box::new(self.unary()?))
            }
            Tok::Star => {
                self.advance();
                ExprKind::Unary(UnOp::Deref, Box::new(self.unary()?))
            }
            Tok::Amp => {
                self.advance();
                let mutable = self.eat(&Tok::Mut);
                ExprKind::Ref(mutable, Box::new(self.unary()?))
            }
            Tok::AndAnd => {
                // `&&x` is a reference to a reference.
                self.advance();
                let inner = self.unary()?;
                ExprKind::Ref(false, Box::new(Expr { kind: ExprKind::Ref(false, Box::new(inner)), span }))
            }
            _ => return self.postfix(),
        };
        Ok(Expr { kind, span })
    }

    fn postfix(&mut self) -> Result<Expr> {
        let mut e = self.primary()?;
        loop {
            let span = self.span();
            if self.eat(&Tok::LBracket) {
                let idx = self.with_struct(true, |p| p.expr())?;
                self.expect(Tok::RBracket, "to close index")?;
                e = Expr { kind: ExprKind::Index(Box::new(e), Box::new(idx)), span };
            } else if self.eat(&Tok::Question) {
                e = Expr { kind: ExprKind::Try(Box::new(e)), span };
            } else if self.at(&Tok::LParen) {
                let args = self.args()?;
                e = Expr { kind: ExprKind::CallExpr(Box::new(e), args), span };
            } else if self.eat(&Tok::Dot) {
                if let Tok::Int(n) = *self.peek() {
                    self.advance();
                    e = Expr { kind: ExprKind::TupleField(Box::new(e), n as usize), span };
                    continue;
                }
                let (name, nspan) = self.ident("field or method name after `.`")?;
                // Turbofish on methods, e.g. `.collect::<Vec<_>>()` / `.parse::<i64>()`.
                let mut turbofish = None;
                if self.at(&Tok::ColonColon) && self.peek_at(1) == &Tok::Lt {
                    self.advance();
                    self.advance();
                    turbofish = self.type_args()?.into_iter().next();
                }
                if self.at(&Tok::LParen) {
                    let mut args = self.args()?;
                    // `s.parse::<int>()` -> `s.parse_int()`
                    let name = match (name.as_str(), &turbofish) {
                        ("parse", Some(Type::Int)) => "parse_int".to_string(),
                        ("parse", Some(Type::Float)) => "parse_float".to_string(),
                        // `chars.collect::<String>()` joins the pieces into one string.
                        ("collect", Some(Type::Str)) => {
                            args = vec![Expr { kind: ExprKind::Str(String::new()), span: nspan }];
                            "join".to_string()
                        }
                        _ => name,
                    };
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

    fn looks_like_struct_lit(&self, name: &str) -> bool {
        if self.no_struct || self.peek_at(1) != &Tok::LBrace {
            return false;
        }
        match (self.peek_at(2), self.peek_at(3)) {
            (Tok::RBrace, _) => name.starts_with(|c: char| c.is_uppercase()),
            (Tok::Ident(_), Tok::Colon) => true,
            (Tok::DotDot, _) => true,
            // shorthand `Point { x, y }` — only for capitalized type names
            (Tok::Ident(_), Tok::Comma | Tok::RBrace) => name.starts_with(|c: char| c.is_uppercase()),
            _ => false,
        }
    }

    /// Can the current token start an expression? (Used after `return` / `break`.)
    fn starts_expr(&self) -> bool {
        !matches!(
            self.peek(),
            Tok::Semi | Tok::RBrace | Tok::RParen | Tok::RBracket | Tok::Comma | Tok::Eof
        )
    }

    /// `if`, `match`, `loop` or a `{ block }`.
    fn block_expr(&mut self, label: Option<String>) -> Result<Expr> {
        let span = self.span();
        let kind = match self.peek() {
            Tok::If => return self.if_expr(),
            Tok::Match => return self.match_expr(),
            Tok::Loop => {
                self.advance();
                ExprKind::Loop(self.block()?, label)
            }
            _ => ExprKind::Block(self.block()?),
        };
        Ok(Expr { kind, span })
    }

    fn if_expr(&mut self) -> Result<Expr> {
        let span = self.expect(Tok::If, "")?.span;
        let pattern = if self.eat(&Tok::Let) {
            let pat = self.pattern()?;
            self.expect(Tok::Eq, "in `if let`")?;
            Some(pat)
        } else {
            None
        };
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
        let kind = match pattern {
            Some(pat) => ExprKind::IfLet(Box::new(pat), Box::new(cond), then, else_),
            None => ExprKind::If(Box::new(cond), then, else_),
        };
        Ok(Expr { kind, span })
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
                    let b = p.block_expr(None)?;
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
        self.eat(&Tok::Pipe); // leading `|` is allowed
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
            Tok::Amp => {
                // `&x` patterns: values are copies, so the `&` doesn't change anything.
                self.advance();
                self.eat(&Tok::Mut);
                self.pattern_atom()
            }
            Tok::Str(s) => {
                self.advance();
                let single = |s: &str| {
                    let mut it = s.chars();
                    match (it.next(), it.next()) {
                        (Some(c), None) => Some(c),
                        _ => None,
                    }
                };
                if self.at(&Tok::DotDotEq) || self.at(&Tok::DotDot) {
                    let inclusive = self.advance().tok == Tok::DotDotEq;
                    let hi = match self.advance().tok {
                        Tok::Str(h) => h,
                        other => {
                            return Err(Error::syntax(span, format!("expected a character to end the range, found {}", other.describe())))
                        }
                    };
                    return match (single(&s), single(&hi)) {
                        (Some(lo), Some(hi)) => Ok(Pattern::CharRange(lo, hi, inclusive)),
                        _ => Err(Error::syntax(span, "range patterns need single characters, like 'a'..='z'")),
                    };
                }
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
            Tok::Mut => {
                self.advance();
                match self.pattern_atom()? {
                    Pattern::Bind(name) => Ok(Pattern::MutBind(name)),
                    _ => Err(Error::syntax(span, "`mut` must be followed by a variable name")),
                }
            }
            Tok::LParen => {
                self.advance();
                Ok(Pattern::Tuple(self.list(Tok::RParen, "to close tuple pattern", |p| p.pattern())?))
            }
            Tok::Ident(name) if name == "None" => {
                self.advance();
                Ok(Pattern::Variant { enum_name: "Option".into(), variant: name, fields: None, span })
            }
            Tok::Ident(name) if builtin_variant(&name).is_some() && self.peek_at(1) == &Tok::LParen => {
                self.advance();
                self.advance();
                let fields = self.list(Tok::RParen, "to close pattern", |p| p.pattern())?;
                let enum_name = builtin_variant(&name).unwrap().to_string();
                Ok(Pattern::Variant { enum_name, variant: name, fields: Some(fields), span })
            }
            Tok::Ident(name) => {
                self.advance();
                if self.at(&Tok::LBrace) && name.starts_with(|c: char| c.is_uppercase()) {
                    let name = self.resolve_self(name, span)?;
                    return self.struct_pattern(name, span);
                }
                if !self.at(&Tok::ColonColon) {
                    return Ok(Pattern::Bind(name));
                }
                let mut path = vec![name];
                while self.eat(&Tok::ColonColon) {
                    path.push(self.ident("name after `::`")?.0);
                }
                let variant = path.pop().unwrap();
                let enum_name = self.resolve_self(path.pop().unwrap(), span)?;
                if self.at(&Tok::LBrace) {
                    return self.struct_pattern(format!("{}::{}", enum_name, variant), span);
                }
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

    /// `Point { x, y: 0, .. }`
    fn struct_pattern(&mut self, name: String, span: Span) -> Result<Pattern> {
        self.expect(Tok::LBrace, "")?;
        let mut fields = Vec::new();
        while !self.at(&Tok::RBrace) {
            if self.eat(&Tok::DotDot) {
                break;
            }
            self.eat(&Tok::Mut);
            let (field, _) = self.ident("field name")?;
            let pat = if self.eat(&Tok::Colon) { self.pattern()? } else { Pattern::Bind(field.clone()) };
            fields.push((field, pat));
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RBrace, "to close struct pattern")?;
        Ok(Pattern::Struct(name, fields, span))
    }

    /// `|a, b: int| expr` or `|x| -> int { ... }` or `|| expr`
    fn closure(&mut self) -> Result<Expr> {
        let span = self.span();
        let mut params: Vec<(String, Option<Type>)> = Vec::new();
        if !self.eat(&Tok::OrOr) {
            self.expect(Tok::Pipe, "")?;
            params = self.list(Tok::Pipe, "to close closure parameters", |p| {
                p.eat(&Tok::Amp);
                p.eat(&Tok::Mut);
                let name = match p.pattern_atom()? {
                    Pattern::Bind(n) => n,
                    Pattern::Wildcard => "_".to_string(),
                    _ => return Err(Error::syntax(p.span(), "closure parameters must be simple names")),
                };
                let ty = if p.eat(&Tok::Colon) { Some(p.ty()?) } else { None };
                Ok((name, ty))
            })?;
        }
        let ret = if self.eat(&Tok::Arrow) { Some(self.ty()?) } else { None };
        let start = self.pos;
        let body = if ret.is_some() {
            if !self.at(&Tok::LBrace) {
                return Err(Error::syntax(self.span(), "a closure with a return type needs a `{ }` body"));
            }
            self.block_expr(None)?
        } else {
            self.with_struct(true, |p| p.expr())?
        };
        // Every identifier in the body is a capture candidate; the ones that
        // name a variable in scope when the closure is created get copied in.
        let mut captures: Vec<String> = Vec::new();
        for t in &self.toks[start..self.pos] {
            if let Tok::Ident(n) = &t.tok {
                if !captures.contains(n) && !params.iter().any(|(p, _)| p == n) {
                    captures.push(n.clone());
                }
            }
        }
        let def = ClosureDef { params, ret, body, captures };
        Ok(Expr { kind: ExprKind::Closure(Rc::new(def)), span })
    }

    /// `name!(...)` / `vec![...]` / `matches!(value, pattern)`
    fn macro_call(&mut self, name: String, span: Span) -> Result<ExprKind> {
        self.advance(); // name
        self.advance(); // !
        if name == "vec" {
            let close = match self.advance().tok {
                Tok::LBracket => Tok::RBracket,
                Tok::LParen => Tok::RParen,
                other => return Err(Error::syntax(span, format!("expected `[` after `vec!`, found {}", other.describe()))),
            };
            return self.array_body(close);
        }
        if name == "matches" {
            self.expect(Tok::LParen, "after `matches!`")?;
            let value = self.with_struct(true, |p| p.expr())?;
            self.expect(Tok::Comma, "in `matches!(value, pattern)`")?;
            let pattern = self.pattern()?;
            let guard = if self.eat(&Tok::If) { Some(Box::new(self.expr()?)) } else { None };
            self.eat(&Tok::Comma);
            self.expect(Tok::RParen, "to close `matches!`")?;
            return Ok(ExprKind::Matches(Box::new(value), Box::new(pattern), guard));
        }
        if !CALL_MACROS.contains(&name.as_str()) {
            return Err(Error::syntax(span, format!("unknown macro `{}!`", name)));
        }
        if !self.at(&Tok::LParen) {
            return Err(Error::syntax(self.span(), format!("expected `(` after `{}!`", name)));
        }
        Ok(ExprKind::Call(name, self.args()?))
    }

    /// Elements of an array literal after the opening bracket: `a, b, c]` or `value; count]`.
    fn array_body(&mut self, close: Tok) -> Result<ExprKind> {
        self.with_struct(true, |p| {
            if p.at(&close) {
                p.advance();
                return Ok(ExprKind::Array(Vec::new()));
            }
            let first = p.expr()?;
            if p.eat(&Tok::Semi) {
                let count = p.expr()?;
                p.expect(close, "to close array literal")?;
                return Ok(ExprKind::ArrayRepeat(Box::new(first), Box::new(count)));
            }
            let mut elems = vec![first];
            if p.eat(&Tok::Comma) {
                elems.extend(p.list(close, "to close array literal", |p| p.expr())?);
            } else {
                p.expect(close, "to close array literal")?;
            }
            Ok(ExprKind::Array(elems))
        })
    }

    /// After a path, is the next `{` the start of struct fields (not a block)?
    fn struct_body_follows(&self) -> bool {
        !self.no_struct
            && self.at(&Tok::LBrace)
            && matches!(
                (self.peek_at(1), self.peek_at(2)),
                (Tok::RBrace | Tok::DotDot, _) | (Tok::Ident(_), Tok::Colon | Tok::Comma | Tok::RBrace)
            )
    }

    fn struct_lit(&mut self, name: String) -> Result<ExprKind> {
        self.advance(); // name
        self.struct_body(name)
    }

    /// `{ field: value, short, ..base }` — the current token is the `{`.
    fn struct_body(&mut self, name: String) -> Result<ExprKind> {
        self.advance(); // {
        self.with_struct(true, |p| {
            let mut fields = Vec::new();
            let mut base = None;
            while !p.at(&Tok::RBrace) {
                if p.eat(&Tok::DotDot) {
                    base = Some(Box::new(p.expr()?));
                    break;
                }
                let (fname, fspan) = p.ident("field name")?;
                let value = if p.eat(&Tok::Colon) {
                    p.expr()?
                } else {
                    Expr { kind: ExprKind::Var(fname.clone()), span: fspan }
                };
                fields.push((fname, value));
                if !p.eat(&Tok::Comma) {
                    break;
                }
            }
            p.expect(Tok::RBrace, "to close struct literal")?;
            Ok(ExprKind::StructLit(name, fields, base))
        })
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
            Tok::If | Tok::Match | Tok::Loop | Tok::LBrace => return self.block_expr(None),
            Tok::Label(label) if self.peek_at(1) == &Tok::Colon && self.peek_at(2) == &Tok::Loop => {
                self.advance();
                self.advance();
                return self.block_expr(Some(label));
            }
            Tok::Return => {
                self.advance();
                let value = if self.starts_expr() { Some(Box::new(self.expr()?)) } else { None };
                ExprKind::Return(value)
            }
            Tok::Break => {
                self.advance();
                let label = match self.peek().clone() {
                    Tok::Label(l) => {
                        self.advance();
                        Some(l)
                    }
                    _ => None,
                };
                let value = if self.starts_expr() { Some(Box::new(self.expr()?)) } else { None };
                ExprKind::Break(label, value)
            }
            Tok::Continue => {
                self.advance();
                let label = match self.peek().clone() {
                    Tok::Label(l) => {
                        self.advance();
                        Some(l)
                    }
                    _ => None,
                };
                ExprKind::Continue(label)
            }
            Tok::Ident(name) if self.peek_at(1) == &Tok::Bang && matches!(self.peek_at(2), Tok::LParen | Tok::LBracket) => {
                self.macro_call(name, span)?
            }
            Tok::Ident(name) if name == "move" && matches!(self.peek_at(1), Tok::Pipe | Tok::OrOr) => {
                self.advance();
                return self.closure();
            }
            Tok::Ident(name) if name == "None" && self.peek_at(1) != &Tok::ColonColon => {
                self.advance();
                ExprKind::Path("Option".into(), name)
            }
            Tok::Ident(name) if builtin_variant(&name).is_some() && self.peek_at(1) == &Tok::LParen => {
                self.advance();
                let enum_name = builtin_variant(&name).unwrap().to_string();
                ExprKind::PathCall(enum_name, name, self.args()?)
            }
            Tok::Ident(name) => {
                if self.peek_at(1) == &Tok::ColonColon {
                    // `a::b::c` — the last two segments matter (`std::f64::consts::PI` -> consts::PI).
                    let mut path = vec![self.resolve_self(name, span)?];
                    self.advance();
                    while self.eat(&Tok::ColonColon) {
                        if self.at(&Tok::Lt) {
                            self.skip_angle_brackets()?; // `Vec::<int>::new()`
                            continue;
                        }
                        path.push(self.ident("name after `::`")?.0);
                    }
                    if self.at(&Tok::Lt) && path.len() == 1 {
                        return Err(Error::syntax(self.span(), "expected a name after `::`"));
                    }
                    let item = path.pop().unwrap();
                    let ty = path.pop().unwrap_or_default();
                    if self.at(&Tok::LParen) {
                        ExprKind::PathCall(ty, item, self.args()?)
                    } else if self.struct_body_follows() {
                        // `Enum::Variant { .. }` or `module::Point { .. }`
                        let name = if ty.is_empty() { item } else { format!("{}::{}", ty, item) };
                        self.struct_body(name)?
                    } else {
                        ExprKind::Path(ty, item)
                    }
                } else if self.peek_at(1) == &Tok::LParen {
                    self.advance();
                    ExprKind::Call(name, self.args()?)
                } else if self.looks_like_struct_lit(&name) {
                    let name = self.resolve_self(name, span)?;
                    self.struct_lit(name)?
                } else {
                    self.advance();
                    ExprKind::Var(name)
                }
            }
            Tok::LParen => {
                self.advance();
                if self.eat(&Tok::RParen) {
                    ExprKind::Unit
                } else {
                    let first = self.with_struct(true, |p| p.expr())?;
                    if !self.eat(&Tok::Comma) {
                        self.expect(Tok::RParen, "to close parenthesis")?;
                        return Ok(first);
                    }
                    let mut elems = vec![first];
                    elems.extend(self.with_struct(true, |p| p.list(Tok::RParen, "to close tuple", |p| p.expr()))?);
                    ExprKind::Tuple(elems)
                }
            }
            Tok::Pipe | Tok::OrOr => return self.closure(),
            Tok::LBracket => {
                self.advance();
                self.array_body(Tok::RBracket)?
            }
            other => {
                return Err(Error::syntax(span, format!("expected an expression, found {}", other.describe())))
            }
        };
        Ok(Expr { kind, span })
    }
}

/// Can this expression be assigned to?
fn is_place(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Var(_) => true,
        ExprKind::Index(base, _) | ExprKind::Field(base, _) | ExprKind::TupleField(base, _) => is_place(base),
        ExprKind::Unary(UnOp::Deref, inner) => is_place(inner),
        // `*map.entry(k).or_insert(0) += 1`
        ExprKind::MethodCall(inner, name, _) => match name.as_str() {
            "or_insert" | "or_insert_with" | "or_default" => true,
            // `*map.get_mut(&k).unwrap() = v`
            "unwrap" | "expect" => matches!(&inner.kind, ExprKind::MethodCall(_, g, _) if matches!(g.as_str(), "get_mut" | "first_mut" | "last_mut")),
            _ => false,
        },
        _ => false,
    }
}

/// `Some`/`Ok`/`Err` can be written without their enum name, like in Rust.
fn builtin_variant(name: &str) -> Option<&'static str> {
    match name {
        "Some" => Some("Option"),
        "Ok" | "Err" => Some("Result"),
        _ => None,
    }
}

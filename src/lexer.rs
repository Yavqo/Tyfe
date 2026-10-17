use crate::ast::Span;
use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Int(i64),
    Float(f64),
    Str(String),
    Ident(String),
    // keywords
    Fn,
    Let,
    Mut,
    If,
    Else,
    While,
    For,
    In,
    Return,
    True,
    False,
    Struct,
    Break,
    Continue,
    Enum,
    Impl,
    Match,
    Loop,
    Const,
    As,
    Trait,
    Mod,
    Use,
    Dyn,
    Where,
    Pub,
    /// `'outer` — a loop label
    Label(String),
    // punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Colon,
    ColonColon,
    FatArrow,
    Pipe,
    Amp,
    Question,
    Hash,
    Caret,
    Shl,
    Shr,
    AmpEq,
    PipeEq,
    CaretEq,
    ShlEq,
    ShrEq,
    Dot,
    DotDot,
    DotDotEq,
    Arrow,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eq,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    AndAnd,
    OrOr,
    Bang,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    Eof,
}

impl Tok {
    pub fn describe(&self) -> String {
        match self {
            Tok::Int(n) => format!("integer `{}`", n),
            Tok::Float(f) => format!("float `{}`", f),
            Tok::Str(_) => "string literal".to_string(),
            Tok::Ident(s) => format!("identifier `{}`", s),
            Tok::Label(s) => format!("label `'{}`", s),
            Tok::Eof => "end of file".to_string(),
            other => format!("`{}`", other.symbol()),
        }
    }

    fn symbol(&self) -> &'static str {
        match self {
            Tok::Fn => "fn",
            Tok::Let => "let",
            Tok::Mut => "mut",
            Tok::If => "if",
            Tok::Else => "else",
            Tok::While => "while",
            Tok::For => "for",
            Tok::In => "in",
            Tok::Return => "return",
            Tok::True => "true",
            Tok::False => "false",
            Tok::Struct => "struct",
            Tok::Break => "break",
            Tok::Continue => "continue",
            Tok::Enum => "enum",
            Tok::Impl => "impl",
            Tok::Match => "match",
            Tok::Loop => "loop",
            Tok::Const => "const",
            Tok::As => "as",
            Tok::Trait => "trait",
            Tok::Mod => "mod",
            Tok::Use => "use",
            Tok::Dyn => "dyn",
            Tok::Where => "where",
            Tok::Pub => "pub",
            Tok::Hash => "#",
            Tok::Caret => "^",
            Tok::Shl => "<<",
            Tok::Shr => ">>",
            Tok::AmpEq => "&=",
            Tok::PipeEq => "|=",
            Tok::CaretEq => "^=",
            Tok::ShlEq => "<<=",
            Tok::ShrEq => ">>=",
            Tok::ColonColon => "::",
            Tok::FatArrow => "=>",
            Tok::Pipe => "|",
            Tok::Amp => "&",
            Tok::Question => "?",
            Tok::LParen => "(",
            Tok::RParen => ")",
            Tok::LBrace => "{",
            Tok::RBrace => "}",
            Tok::LBracket => "[",
            Tok::RBracket => "]",
            Tok::Comma => ",",
            Tok::Semi => ";",
            Tok::Colon => ":",
            Tok::Dot => ".",
            Tok::DotDot => "..",
            Tok::DotDotEq => "..=",
            Tok::Arrow => "->",
            Tok::Plus => "+",
            Tok::Minus => "-",
            Tok::Star => "*",
            Tok::Slash => "/",
            Tok::Percent => "%",
            Tok::Eq => "=",
            Tok::EqEq => "==",
            Tok::NotEq => "!=",
            Tok::Lt => "<",
            Tok::Le => "<=",
            Tok::Gt => ">",
            Tok::Ge => ">=",
            Tok::AndAnd => "&&",
            Tok::OrOr => "||",
            Tok::Bang => "!",
            Tok::PlusEq => "+=",
            Tok::MinusEq => "-=",
            Tok::StarEq => "*=",
            Tok::SlashEq => "/=",
            Tok::PercentEq => "%=",
            _ => "?",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

struct Lexer {
    file: u16,
    chars: Vec<char>,
    i: usize,
    line: usize,
    col: usize,
}

impl Lexer {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn peek2(&self) -> Option<char> {
        self.chars.get(self.i + 1).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.get(self.i).copied()?;
        self.i += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn span(&self) -> Span {
        Span { line: self.line, col: self.col, file: self.file }
    }

    fn skip_trivia(&mut self) -> Result<()> {
        loop {
            match (self.peek(), self.peek2()) {
                (Some(c), _) if c.is_whitespace() => {
                    self.bump();
                }
                (Some('/'), Some('/')) => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                (Some('/'), Some('*')) => {
                    let start = self.span();
                    self.bump();
                    self.bump();
                    let mut depth = 1;
                    while depth > 0 {
                        match (self.peek(), self.peek2()) {
                            (Some('*'), Some('/')) => {
                                self.bump();
                                self.bump();
                                depth -= 1;
                            }
                            (Some('/'), Some('*')) => {
                                self.bump();
                                self.bump();
                                depth += 1;
                            }
                            (Some(_), _) => {
                                self.bump();
                            }
                            (None, _) => return Err(Error::syntax(start, "unterminated block comment")),
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    /// Skip a Rust type suffix like `u8`, `i64`, `usize`, `f32`; returns it.
    fn suffix(&mut self) -> String {
        const SUFFIXES: &[&str] = &[
            "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64",
        ];
        let start = if self.peek() == Some('_') { 1 } else { 0 };
        let mut word = String::new();
        while let Some(&c) = self.chars.get(self.i + start + word.len()) {
            if c.is_ascii_alphanumeric() {
                word.push(c);
            } else {
                break;
            }
        }
        if !SUFFIXES.contains(&word.as_str()) {
            return String::new();
        }
        for _ in 0..start + word.len() {
            self.bump();
        }
        word
    }

    /// `allow_float` is false right after a `.`, so `t.0.1` lexes as two field accesses.
    fn number(&mut self, span: Span, allow_float: bool) -> Result<Tok> {
        // 0x.. / 0b.. / 0o..
        if self.peek() == Some('0') && matches!(self.peek2(), Some('x' | 'b' | 'o')) {
            let (radix, prefix) = match self.peek2() {
                Some('x') => (16, "0x"),
                Some('b') => (2, "0b"),
                _ => (8, "0o"),
            };
            self.bump();
            self.bump();
            let mut text = String::new();
            while let Some(c) = self.peek() {
                if c.is_digit(radix) {
                    text.push(c);
                    self.bump();
                } else if c == '_' {
                    self.bump();
                } else {
                    break;
                }
            }
            if text.is_empty() {
                return Err(Error::syntax(span, format!("expected digits after `{}`", prefix)));
            }
            self.suffix();
            return i64::from_str_radix(&text, radix)
                .or_else(|_| u64::from_str_radix(&text, radix).map(|n| n as i64))
                .map(Tok::Int)
                .map_err(|_| Error::syntax(span, format!("integer literal `{}{}` is too large for int", prefix, text)));
        }
        let mut text = String::new();
        let mut is_float = false;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                text.push(c);
                self.bump();
            } else if c == '_' && self.peek2().is_some_and(|d| d.is_ascii_digit() || d == '_') {
                self.bump();
            } else if c == '.' && allow_float && !is_float && self.peek2().is_some_and(|d| d.is_ascii_digit()) {
                is_float = true;
                text.push(c);
                self.bump();
            } else {
                break;
            }
        }
        // Exponent: 1e6, 2.5E-3
        if allow_float && matches!(self.peek(), Some('e' | 'E')) {
            let next = self.peek2();
            let after = self.chars.get(self.i + 2).copied();
            let has_exp = next.is_some_and(|d| d.is_ascii_digit())
                || (matches!(next, Some('+' | '-')) && after.is_some_and(|d| d.is_ascii_digit()));
            if has_exp {
                is_float = true;
                text.push('e');
                self.bump();
                if let Some(sign @ ('+' | '-')) = self.peek() {
                    text.push(sign);
                    self.bump();
                }
                while let Some(c) = self.peek().filter(|c| c.is_ascii_digit()) {
                    text.push(c);
                    self.bump();
                }
            }
        }
        if self.suffix().starts_with('f') {
            is_float = true;
        }
        if is_float {
            text.parse::<f64>()
                .map(Tok::Float)
                .map_err(|_| Error::syntax(span, format!("invalid float literal `{}`", text)))
        } else {
            text.parse::<i64>()
                .map(Tok::Int)
                .map_err(|_| Error::syntax(span, format!("integer literal `{}` is too large for int", text)))
        }
    }

    fn string(&mut self, span: Span) -> Result<Tok> {
        self.bump(); // opening quote
        let mut s = String::new();
        loop {
            match self.bump() {
                None => return Err(Error::syntax(span, "unterminated string literal")),
                Some('"') => return Ok(Tok::Str(s)),
                Some('\\') => s.push(self.escape(span)?),
                Some(c) => s.push(c),
            }
        }
    }

    fn escape(&mut self, span: Span) -> Result<char> {
        let esc_span = self.span();
        Ok(match self.bump() {
            Some('n') => '\n',
            Some('t') => '\t',
            Some('r') => '\r',
            Some('0') => '\0',
            Some('\\') => '\\',
            Some('"') => '"',
            Some('\'') => '\'',
            Some('u') => {
                let mut hex = String::new();
                if self.bump() != Some('{') {
                    return Err(Error::syntax(esc_span, "expected `{` in `\\u{...}` escape"));
                }
                loop {
                    match self.bump() {
                        Some('}') => break,
                        Some(c) if c.is_ascii_hexdigit() => hex.push(c),
                        _ => return Err(Error::syntax(esc_span, "invalid `\\u{...}` escape")),
                    }
                }
                u32::from_str_radix(&hex, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| Error::syntax(esc_span, format!("invalid unicode escape `\\u{{{}}}`", hex)))?
            }
            Some(c) => return Err(Error::syntax(esc_span, format!("unknown escape sequence `\\{}`", c))),
            None => return Err(Error::syntax(span, "unterminated literal")),
        })
    }

    /// `'a'` (a one-character string) or `'label`.
    fn quote(&mut self, span: Span) -> Result<Tok> {
        self.bump(); // '
        let is_char = matches!((self.peek(), self.peek2()), (Some('\\'), _) | (Some(_), Some('\'')));
        if is_char {
            let c = match self.bump() {
                Some('\\') => self.escape(span)?,
                Some(c) => c,
                None => return Err(Error::syntax(span, "unterminated character literal")),
            };
            if self.bump() != Some('\'') {
                return Err(Error::syntax(span, "character literal must contain exactly one character"));
            }
            return Ok(Tok::Str(c.to_string()));
        }
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' {
                name.push(c);
                self.bump();
            } else {
                break;
            }
        }
        if name.is_empty() {
            return Err(Error::syntax(span, "expected a character literal like 'a' or a label like 'outer"));
        }
        Ok(Tok::Label(name))
    }
}

pub fn lex(src: &str, file: u16) -> Result<Vec<Token>> {
    let mut lx = Lexer { file, chars: src.chars().collect(), i: 0, line: 1, col: 1 };
    let mut toks = Vec::new();
    loop {
        lx.skip_trivia()?;
        let span = lx.span();
        let c = match lx.peek() {
            None => {
                toks.push(Token { tok: Tok::Eof, span });
                return Ok(toks);
            }
            Some(c) => c,
        };

        let tok = if c.is_ascii_digit() {
            let after_dot = matches!(toks.last(), Some(Token { tok: Tok::Dot, .. }));
            lx.number(span, !after_dot)?
        } else if c == '"' {
            lx.string(span)?
        } else if c == '\'' {
            lx.quote(span)?
        } else if c.is_alphabetic() || c == '_' {
            let mut word = String::new();
            while let Some(c) = lx.peek() {
                if c.is_alphanumeric() || c == '_' {
                    word.push(c);
                    lx.bump();
                } else {
                    break;
                }
            }
            match word.as_str() {
                "fn" => Tok::Fn,
                "let" => Tok::Let,
                "mut" => Tok::Mut,
                "if" => Tok::If,
                "else" => Tok::Else,
                "while" => Tok::While,
                "for" => Tok::For,
                "in" => Tok::In,
                "return" => Tok::Return,
                "true" => Tok::True,
                "false" => Tok::False,
                "struct" => Tok::Struct,
                "break" => Tok::Break,
                "continue" => Tok::Continue,
                "enum" => Tok::Enum,
                "impl" => Tok::Impl,
                "match" => Tok::Match,
                "loop" => Tok::Loop,
                "const" => Tok::Const,
                "as" => Tok::As,
                "trait" => Tok::Trait,
                "mod" => Tok::Mod,
                "use" => Tok::Use,
                "dyn" => Tok::Dyn,
                "where" => Tok::Where,
                "pub" => Tok::Pub,
                _ => Tok::Ident(word),
            }
        } else {
            lx.bump();
            let next = lx.peek();
            let two = |t: Tok, lx: &mut Lexer| {
                lx.bump();
                t
            };
            match (c, next) {
                ('(', _) => Tok::LParen,
                (')', _) => Tok::RParen,
                ('{', _) => Tok::LBrace,
                ('}', _) => Tok::RBrace,
                ('[', _) => Tok::LBracket,
                (']', _) => Tok::RBracket,
                (',', _) => Tok::Comma,
                (';', _) => Tok::Semi,
                (':', Some(':')) => two(Tok::ColonColon, &mut lx),
                ('<', Some('<')) => {
                    lx.bump();
                    if lx.peek() == Some('=') {
                        lx.bump();
                        Tok::ShlEq
                    } else {
                        Tok::Shl
                    }
                }
                ('>', Some('>')) => {
                    lx.bump();
                    if lx.peek() == Some('=') {
                        lx.bump();
                        Tok::ShrEq
                    } else {
                        Tok::Shr
                    }
                }
                ('&', Some('=')) => two(Tok::AmpEq, &mut lx),
                ('|', Some('=')) => two(Tok::PipeEq, &mut lx),
                ('^', Some('=')) => two(Tok::CaretEq, &mut lx),
                ('^', _) => Tok::Caret,
                ('#', _) => Tok::Hash,
                (':', _) => Tok::Colon,
                ('.', Some('.')) => {
                    lx.bump();
                    if lx.peek() == Some('=') {
                        lx.bump();
                        Tok::DotDotEq
                    } else {
                        Tok::DotDot
                    }
                }
                ('.', _) => Tok::Dot,
                ('-', Some('>')) => two(Tok::Arrow, &mut lx),
                ('+', Some('=')) => two(Tok::PlusEq, &mut lx),
                ('-', Some('=')) => two(Tok::MinusEq, &mut lx),
                ('*', Some('=')) => two(Tok::StarEq, &mut lx),
                ('/', Some('=')) => two(Tok::SlashEq, &mut lx),
                ('%', Some('=')) => two(Tok::PercentEq, &mut lx),
                ('=', Some('=')) => two(Tok::EqEq, &mut lx),
                ('=', Some('>')) => two(Tok::FatArrow, &mut lx),
                ('!', Some('=')) => two(Tok::NotEq, &mut lx),
                ('<', Some('=')) => two(Tok::Le, &mut lx),
                ('>', Some('=')) => two(Tok::Ge, &mut lx),
                ('&', Some('&')) => two(Tok::AndAnd, &mut lx),
                ('|', Some('|')) => two(Tok::OrOr, &mut lx),
                ('|', _) => Tok::Pipe,
                ('&', _) => Tok::Amp,
                ('?', _) => Tok::Question,
                ('+', _) => Tok::Plus,
                ('-', _) => Tok::Minus,
                ('*', _) => Tok::Star,
                ('/', _) => Tok::Slash,
                ('%', _) => Tok::Percent,
                ('=', _) => Tok::Eq,
                ('!', _) => Tok::Bang,
                ('<', _) => Tok::Lt,
                ('>', _) => Tok::Gt,
                _ => return Err(Error::syntax(span, format!("unexpected character `{}`", c))),
            }
        };
        toks.push(Token { tok, span });
    }
}

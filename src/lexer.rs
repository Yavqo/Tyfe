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
            Tok::ColonColon => "::",
            Tok::FatArrow => "=>",
            Tok::Pipe => "|",
            Tok::Amp => "&",
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
        Span { line: self.line, col: self.col }
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

    fn number(&mut self, span: Span) -> Result<Tok> {
        let mut text = String::new();
        let mut is_float = false;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                text.push(c);
                self.bump();
            } else if c == '_' {
                self.bump();
            } else if c == '.' && !is_float && self.peek2().is_some_and(|d| d.is_ascii_digit()) {
                is_float = true;
                text.push(c);
                self.bump();
            } else {
                break;
            }
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
                Some('\\') => {
                    let esc_span = self.span();
                    match self.bump() {
                        Some('n') => s.push('\n'),
                        Some('t') => s.push('\t'),
                        Some('r') => s.push('\r'),
                        Some('0') => s.push('\0'),
                        Some('\\') => s.push('\\'),
                        Some('"') => s.push('"'),
                        Some(c) => {
                            return Err(Error::syntax(esc_span, format!("unknown escape sequence `\\{}`", c)))
                        }
                        None => return Err(Error::syntax(span, "unterminated string literal")),
                    }
                }
                Some(c) => s.push(c),
            }
        }
    }
}

pub fn lex(src: &str) -> Result<Vec<Token>> {
    let mut lx = Lexer { chars: src.chars().collect(), i: 0, line: 1, col: 1 };
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
            lx.number(span)?
        } else if c == '"' {
            lx.string(span)?
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

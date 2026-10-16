use crate::ast::Span;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ErrorKind {
    Syntax,
    Runtime,
}

#[derive(Debug, Clone)]
pub struct Error {
    pub kind: ErrorKind,
    pub msg: String,
    pub span: Span,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn syntax(span: Span, msg: impl Into<String>) -> Self {
        Error { kind: ErrorKind::Syntax, msg: msg.into(), span }
    }

    pub fn runtime(span: Span, msg: impl Into<String>) -> Self {
        Error { kind: ErrorKind::Runtime, msg: msg.into(), span }
    }

    /// Render the error with the offending source line and a caret, rustc-style.
    pub fn render(&self, file: &str, src: &str) -> String {
        let label = match self.kind {
            ErrorKind::Syntax => "syntax error",
            ErrorKind::Runtime => "runtime error",
        };
        let line_no = self.span.line.to_string();
        let pad = " ".repeat(line_no.len());
        let mut out = format!(
            "{}: {}\n{}--> {}:{}:{}\n",
            label, self.msg, pad, file, self.span.line, self.span.col
        );
        if let Some(text) = src.lines().nth(self.span.line.saturating_sub(1)) {
            let caret_pad: String = text
                .chars()
                .take(self.span.col.saturating_sub(1))
                .map(|c| if c == '\t' { '\t' } else { ' ' })
                .collect();
            out.push_str(&format!("{} |\n{} | {}\n{} | {}^\n", pad, line_no, text, pad, caret_pad));
        }
        out
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}:{}: {}", self.span.line, self.span.col, self.msg)
    }
}

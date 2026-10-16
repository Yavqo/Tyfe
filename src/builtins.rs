//! Built-in functions (`println`, `len`, ...) and methods (`arr.push(x)`, `s.len()`, ...).

use crate::ast::Span;
use crate::error::{Error, Result};
use crate::value::Value;
use std::io::{self, BufRead, Write};

fn arity(name: &str, args: &[Value], expected: usize, span: Span) -> Result<()> {
    if args.len() != expected {
        return Err(Error::runtime(
            span,
            format!(
                "`{}` takes {} argument{} but {} were given",
                name,
                expected,
                if expected == 1 { "" } else { "s" },
                args.len()
            ),
        ));
    }
    Ok(())
}

fn type_err(span: Span, what: &str, v: &Value) -> Error {
    Error::runtime(span, format!("{} cannot be used with a value of type {}", what, v.type_name()))
}

/// Rust-style formatting: `"x = {}, name = {:?}"`. `{{` and `}}` are literal braces.
pub fn format(args: &[Value], span: Span) -> Result<String> {
    let (fmt, rest) = match args.split_first() {
        None => return Ok(String::new()),
        Some((Value::Str(s), rest)) => (s, rest),
        Some((v, [])) => return Ok(v.render(false)),
        Some(_) => {
            return Err(Error::runtime(span, "the first argument must be a format string, e.g. println(\"{} {}\", a, b)"))
        }
    };
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    let mut next_arg = 0;
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let mut spec = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => spec.push(c),
                        None => return Err(Error::runtime(span, "unterminated `{` in format string (use `{{` for a literal brace)")),
                    }
                }
                let debug = match spec.as_str() {
                    "" => false,
                    ":?" => true,
                    _ => return Err(Error::runtime(span, format!("unsupported format spec `{{{}}}` (use `{{}}` or `{{:?}}`)", spec))),
                };
                let arg = rest.get(next_arg).ok_or_else(|| {
                    Error::runtime(span, format!("format string has more `{{}}` placeholders than arguments ({} given)", rest.len()))
                })?;
                out.push_str(&arg.render(debug));
                next_arg += 1;
            }
            '}' => return Err(Error::runtime(span, "unmatched `}` in format string (use `}}` for a literal brace)")),
            c => out.push(c),
        }
    }
    if next_arg != rest.len() {
        return Err(Error::runtime(
            span,
            format!("format string has {} placeholder(s) but {} argument(s) were given", next_arg, rest.len()),
        ));
    }
    Ok(out)
}

pub fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "print" | "println" | "eprintln" | "format" | "len" | "str" | "int" | "float" | "input" | "assert"
            | "assert_eq" | "panic" | "type_of" | "abs" | "sqrt" | "min" | "max"
    )
}

pub fn call(name: &str, args: Vec<Value>, span: Span) -> Result<Value> {
    match name {
        "print" => {
            print!("{}", format(&args, span)?);
            io::stdout().flush().ok();
            Ok(Value::Unit)
        }
        "println" => {
            println!("{}", format(&args, span)?);
            Ok(Value::Unit)
        }
        "eprintln" => {
            eprintln!("{}", format(&args, span)?);
            Ok(Value::Unit)
        }
        "format" => Ok(Value::Str(format(&args, span)?)),
        "len" => {
            arity(name, &args, 1, span)?;
            method(args.into_iter().next().unwrap(), "len", vec![], span)
        }
        "str" => {
            arity(name, &args, 1, span)?;
            Ok(Value::Str(args[0].render(false)))
        }
        "int" => {
            arity(name, &args, 1, span)?;
            match &args[0] {
                Value::Int(n) => Ok(Value::Int(*n)),
                Value::Float(f) => {
                    if f.is_finite() && *f >= i64::MIN as f64 && *f < i64::MAX as f64 {
                        Ok(Value::Int(f.trunc() as i64))
                    } else {
                        Err(Error::runtime(span, format!("float {} does not fit in an int", f)))
                    }
                }
                Value::Bool(b) => Ok(Value::Int(*b as i64)),
                Value::Str(s) => s
                    .trim()
                    .parse::<i64>()
                    .map(Value::Int)
                    .map_err(|_| Error::runtime(span, format!("cannot parse {:?} as int", s))),
                v => Err(type_err(span, "`int()`", v)),
            }
        }
        "float" => {
            arity(name, &args, 1, span)?;
            match &args[0] {
                Value::Int(n) => Ok(Value::Float(*n as f64)),
                Value::Float(f) => Ok(Value::Float(*f)),
                Value::Str(s) => s
                    .trim()
                    .parse::<f64>()
                    .map(Value::Float)
                    .map_err(|_| Error::runtime(span, format!("cannot parse {:?} as float", s))),
                v => Err(type_err(span, "`float()`", v)),
            }
        }
        "input" => {
            if args.len() > 1 {
                return Err(Error::runtime(span, "`input` takes at most 1 argument (a prompt)"));
            }
            if let Some(prompt) = args.first() {
                print!("{}", prompt.render(false));
                io::stdout().flush().ok();
            }
            let mut line = String::new();
            io::stdin().lock().read_line(&mut line).map_err(|e| Error::runtime(span, e.to_string()))?;
            while line.ends_with('\n') || line.ends_with('\r') {
                line.pop();
            }
            Ok(Value::Str(line))
        }
        "assert" => {
            if args.is_empty() || args.len() > 2 {
                return Err(Error::runtime(span, "`assert` takes a condition and an optional message"));
            }
            match &args[0] {
                Value::Bool(true) => Ok(Value::Unit),
                Value::Bool(false) => {
                    let msg = args.get(1).map(|m| m.render(false)).unwrap_or_else(|| "condition was false".into());
                    Err(Error::runtime(span, format!("assertion failed: {}", msg)))
                }
                v => Err(Error::runtime(span, format!("`assert` expects a bool, got {}", v.type_name()))),
            }
        }
        "assert_eq" => {
            arity(name, &args, 2, span)?;
            if args[0].equals(&args[1]) {
                Ok(Value::Unit)
            } else {
                Err(Error::runtime(
                    span,
                    format!(
                        "assertion failed: left == right\n  left: {}\n right: {}",
                        args[0].render(true),
                        args[1].render(true)
                    ),
                ))
            }
        }
        "panic" => {
            let msg = format(&args, span)?;
            Err(Error::runtime(span, format!("panicked: {}", msg)))
        }
        "type_of" => {
            arity(name, &args, 1, span)?;
            Ok(Value::Str(args[0].type_name()))
        }
        "abs" | "sqrt" => {
            arity(name, &args, 1, span)?;
            method(args.into_iter().next().unwrap(), name, vec![], span)
        }
        "min" | "max" => {
            arity(name, &args, 2, span)?;
            let pick_first = match (&args[0], &args[1]) {
                (Value::Int(a), Value::Int(b)) => (a <= b) == (name == "min"),
                (Value::Float(a), Value::Float(b)) => (a <= b) == (name == "min"),
                (a, b) => {
                    return Err(Error::runtime(
                        span,
                        format!("`{}` needs two ints or two floats, got {} and {}", name, a.type_name(), b.type_name()),
                    ))
                }
            };
            Ok(args.into_iter().nth(if pick_first { 0 } else { 1 }).unwrap())
        }
        _ => {
            let hint = match name {
                "push" | "pop" | "insert" | "remove" | "clear" => format!(" (did you mean `array.{}(...)`?)", name),
                _ => String::new(),
            };
            Err(Error::runtime(span, format!("cannot find function `{}`{}", name, hint)))
        }
    }
}

/// Methods that modify the receiver in place.
pub fn is_mutating_method(name: &str) -> bool {
    matches!(name, "push" | "pop" | "insert" | "remove" | "clear")
}

fn to_index(v: &Value, len: usize, allow_end: bool, span: Span) -> Result<usize> {
    match v {
        Value::Int(i) if *i >= 0 && ((*i as usize) < len || (allow_end && *i as usize == len)) => Ok(*i as usize),
        Value::Int(i) => Err(Error::runtime(span, format!("index out of bounds: the length is {} but the index is {}", len, i))),
        other => Err(Error::runtime(span, format!("index must be an int, got {}", other.type_name()))),
    }
}

pub fn index(base: &Value, idx: &Value, span: Span) -> Result<Value> {
    match base {
        Value::Array(items) => Ok(items[to_index(idx, items.len(), false, span)?].clone()),
        Value::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            Ok(Value::Str(chars[to_index(idx, chars.len(), false, span)?].to_string()))
        }
        other => Err(Error::runtime(span, format!("cannot index into a value of type {}", other.type_name()))),
    }
}

pub fn index_mut<'a>(base: &'a mut Value, idx: &Value, span: Span) -> Result<&'a mut Value> {
    match base {
        Value::Array(items) => {
            let i = to_index(idx, items.len(), false, span)?;
            Ok(&mut items[i])
        }
        Value::Str(_) => Err(Error::runtime(span, "cannot assign to a character of a string; strings are immutable")),
        other => Err(Error::runtime(span, format!("cannot index into a value of type {}", other.type_name()))),
    }
}

pub fn method_mut(recv: &mut Value, name: &str, args: Vec<Value>, span: Span) -> Result<Value> {
    let tn = recv.type_name();
    let items = match recv {
        Value::Array(items) => items,
        _ => return Err(Error::runtime(span, format!("no method `{}` on type {}", name, tn))),
    };
    let check_kind = |items: &Vec<Value>, v: &Value| -> Result<()> {
        match items.first() {
            Some(first) if !first.same_kind(v) => Err(Error::runtime(
                span,
                format!("mismatched types: cannot put a {} into an array of {}", v.type_name(), first.type_name()),
            )),
            _ => Ok(()),
        }
    };
    match name {
        "push" => {
            arity(name, &args, 1, span)?;
            let v = args.into_iter().next().unwrap();
            check_kind(items, &v)?;
            items.push(v);
            Ok(Value::Unit)
        }
        "pop" => {
            arity(name, &args, 0, span)?;
            items.pop().ok_or_else(|| Error::runtime(span, "cannot pop from an empty array"))
        }
        "insert" => {
            arity(name, &args, 2, span)?;
            let i = to_index(&args[0], items.len(), true, span)?;
            let v = args.into_iter().nth(1).unwrap();
            check_kind(items, &v)?;
            items.insert(i, v);
            Ok(Value::Unit)
        }
        "remove" => {
            arity(name, &args, 1, span)?;
            let i = to_index(&args[0], items.len(), false, span)?;
            Ok(items.remove(i))
        }
        "clear" => {
            arity(name, &args, 0, span)?;
            items.clear();
            Ok(Value::Unit)
        }
        _ => Err(Error::runtime(span, format!("no method `{}` on type {}", name, tn))),
    }
}

pub fn method(recv: Value, name: &str, args: Vec<Value>, span: Span) -> Result<Value> {
    let no_method = |recv: &Value| Error::runtime(span, format!("no method `{}` on type {}", name, recv.type_name()));
    match (name, &recv) {
        ("to_string", _) => {
            arity(name, &args, 0, span)?;
            Ok(Value::Str(recv.render(false)))
        }
        ("len", Value::Array(items)) => {
            arity(name, &args, 0, span)?;
            Ok(Value::Int(items.len() as i64))
        }
        ("len", Value::Str(s)) => {
            arity(name, &args, 0, span)?;
            Ok(Value::Int(s.chars().count() as i64))
        }
        ("is_empty", Value::Array(items)) => Ok(Value::Bool(items.is_empty())),
        ("is_empty", Value::Str(s)) => Ok(Value::Bool(s.is_empty())),
        ("contains", Value::Array(items)) => {
            arity(name, &args, 1, span)?;
            Ok(Value::Bool(items.iter().any(|v| v.equals(&args[0]))))
        }
        ("contains", Value::Str(s)) => {
            arity(name, &args, 1, span)?;
            match &args[0] {
                Value::Str(sub) => Ok(Value::Bool(s.contains(sub.as_str()))),
                v => Err(type_err(span, "`str.contains`", v)),
            }
        }
        ("join", Value::Array(items)) => {
            arity(name, &args, 1, span)?;
            let sep = match &args[0] {
                Value::Str(s) => s.clone(),
                v => return Err(type_err(span, "`join` separator", v)),
            };
            let parts: Vec<String> = items.iter().map(|v| v.render(false)).collect();
            Ok(Value::Str(parts.join(&sep)))
        }
        ("reverse", Value::Array(items)) => Ok(Value::Array(items.iter().rev().cloned().collect())),
        ("reverse", Value::Str(s)) => Ok(Value::Str(s.chars().rev().collect())),
        ("to_upper", Value::Str(s)) => Ok(Value::Str(s.to_uppercase())),
        ("to_lower", Value::Str(s)) => Ok(Value::Str(s.to_lowercase())),
        ("trim", Value::Str(s)) => Ok(Value::Str(s.trim().to_string())),
        ("chars", Value::Str(s)) => Ok(Value::Array(s.chars().map(|c| Value::Str(c.to_string())).collect())),
        ("split", Value::Str(s)) => {
            arity(name, &args, 1, span)?;
            match &args[0] {
                Value::Str(sep) if sep.is_empty() => Err(Error::runtime(span, "`split` separator must not be empty")),
                Value::Str(sep) => Ok(Value::Array(s.split(sep.as_str()).map(|p| Value::Str(p.to_string())).collect())),
                v => Err(type_err(span, "`split` separator", v)),
            }
        }
        ("abs", Value::Int(n)) => n
            .checked_abs()
            .map(Value::Int)
            .ok_or_else(|| Error::runtime(span, "attempt to take abs with overflow")),
        ("abs", Value::Float(f)) => Ok(Value::Float(f.abs())),
        ("sqrt", Value::Float(f)) => Ok(Value::Float(f.sqrt())),
        ("sqrt", Value::Int(_)) => Err(Error::runtime(span, "`sqrt` needs a float; convert with float(x) first")),
        ("pow", Value::Int(n)) => {
            arity(name, &args, 1, span)?;
            match &args[0] {
                Value::Int(e) if *e >= 0 && *e <= u32::MAX as i64 => n
                    .checked_pow(*e as u32)
                    .map(Value::Int)
                    .ok_or_else(|| Error::runtime(span, "attempt to compute power with overflow")),
                Value::Int(_) => Err(Error::runtime(span, "int `pow` needs a non-negative exponent")),
                v => Err(type_err(span, "int `pow` exponent", v)),
            }
        }
        ("pow", Value::Float(f)) => {
            arity(name, &args, 1, span)?;
            match &args[0] {
                Value::Float(e) => Ok(Value::Float(f.powf(*e))),
                Value::Int(e) => Ok(Value::Float(f.powi(*e as i32))),
                v => Err(type_err(span, "float `pow` exponent", v)),
            }
        }
        _ => Err(no_method(&recv)),
    }
}

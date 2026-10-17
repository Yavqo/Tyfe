//! Built-in functions (`len`, `read_file`, ...), formatting, and methods
//! (`arr.push(x)`, `s.trim()`, `map.insert(k, v)`, ...).

use crate::ast::Span;
use crate::error::{Error, Result};
use crate::value::{self, compare, Key, Value};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
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

fn str_arg<'a>(args: &'a [Value], i: usize, what: &str, span: Span) -> Result<&'a str> {
    match args.get(i) {
        Some(Value::Str(s)) => Ok(s),
        Some(v) => Err(type_err(span, what, v)),
        None => Err(Error::runtime(span, format!("{} is missing", what))),
    }
}

fn int_arg(args: &[Value], i: usize, what: &str, span: Span) -> Result<i64> {
    match args.get(i) {
        Some(Value::Int(n)) => Ok(*n),
        Some(v) => Err(type_err(span, what, v)),
        None => Err(Error::runtime(span, format!("{} is missing", what))),
    }
}

fn float_arg(args: &[Value], i: usize, what: &str, span: Span) -> Result<f64> {
    match args.get(i) {
        Some(Value::Float(f)) => Ok(*f),
        Some(Value::Int(_)) => Err(Error::runtime(span, format!("{} must be a float; convert with `as float`", what))),
        Some(v) => Err(type_err(span, what, v)),
        None => Err(Error::runtime(span, format!("{} is missing", what))),
    }
}

fn single_char(s: &str) -> Option<char> {
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

// ---------- formatting ----------

/// What the formatter needs from the interpreter: variables for `{name}`,
/// and user `to_string` methods for `{}`.
pub trait FormatEnv {
    fn lookup(&self, name: &str) -> Option<Value>;
    fn display(&mut self, v: &Value) -> Result<Option<String>>;
}

/// Rust-style formatting. Supports `{}`, `{:?}`, `{0}`, `{name}`, and specs like
/// `{:>8}`, `{:<5}`, `{:^9}`, `{:08.3}`, `{:+}`, `{:x}`, `{:#b}`, `{:e}`.
pub fn format_with(args: &[Value], span: Span, env: &mut dyn FormatEnv) -> Result<String> {
    let (fmt, rest) = match args.split_first() {
        None => return Ok(String::new()),
        Some((Value::Str(s), rest)) => (s, rest),
        Some((v, [])) => return display(v, env),
        Some(_) => {
            return Err(Error::runtime(span, "the first argument must be a format string, e.g. println!(\"{} {}\", a, b)"))
        }
    };
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    let mut next_arg = 0;
    let mut used = vec![false; rest.len()];
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
                let mut inner = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => inner.push(c),
                        None => {
                            return Err(Error::runtime(span, "unterminated `{` in format string (use `{{` for a literal brace)"))
                        }
                    }
                }
                let (name, spec) = match inner.find(':') {
                    Some(i) => (inner[..i].trim(), &inner[i + 1..]),
                    None => (inner.trim(), ""),
                };
                let value = if name.is_empty() {
                    let v = rest.get(next_arg).ok_or_else(|| {
                        Error::runtime(span, format!("format string has more `{{}}` placeholders than arguments ({} given)", rest.len()))
                    })?;
                    used[next_arg] = true;
                    next_arg += 1;
                    v.clone()
                } else if let Ok(i) = name.parse::<usize>() {
                    let v = rest
                        .get(i)
                        .ok_or_else(|| Error::runtime(span, format!("format argument {{{}}} does not exist ({} given)", i, rest.len())))?;
                    used[i] = true;
                    v.clone()
                } else {
                    env.lookup(name).ok_or_else(|| {
                        Error::runtime(span, format!("cannot find variable `{}` used in format string", name))
                    })?
                };
                out.push_str(&apply_spec(&value, spec, span, env)?);
            }
            '}' => return Err(Error::runtime(span, "unmatched `}` in format string (use `}}` for a literal brace)")),
            c => out.push(c),
        }
    }
    if let Some(i) = used.iter().position(|u| !u) {
        return Err(Error::runtime(
            span,
            format!("format argument {} is never used ({} placeholder(s), {} argument(s))", i + 1, next_arg, rest.len()),
        ));
    }
    Ok(out)
}

fn display(v: &Value, env: &mut dyn FormatEnv) -> Result<String> {
    Ok(match env.display(v)? {
        Some(s) => s,
        None => v.render(false),
    })
}

/// Apply a format spec: `[[fill]align][+][#][0][width][.precision][type]`.
fn apply_spec(v: &Value, spec: &str, span: Span, env: &mut dyn FormatEnv) -> Result<String> {
    let bad = || Error::runtime(span, format!("invalid format spec `{{:{}}}`", spec));
    let chars: Vec<char> = spec.chars().collect();
    let mut i = 0;
    let mut fill = ' ';
    let mut align: Option<char> = None;
    let is_align = |c: char| matches!(c, '<' | '>' | '^');
    if chars.len() >= 2 && is_align(chars[1]) {
        fill = chars[0];
        align = Some(chars[1]);
        i = 2;
    } else if !chars.is_empty() && is_align(chars[0]) {
        align = Some(chars[0]);
        i = 1;
    }
    let plus = chars.get(i) == Some(&'+');
    if plus {
        i += 1;
    }
    let alt = chars.get(i) == Some(&'#');
    if alt {
        i += 1;
    }
    let zero = chars.get(i) == Some(&'0') && align.is_none();
    if zero {
        i += 1;
    }
    let mut width = String::new();
    while i < chars.len() && chars[i].is_ascii_digit() {
        width.push(chars[i]);
        i += 1;
    }
    let mut precision: Option<usize> = None;
    if chars.get(i) == Some(&'.') {
        i += 1;
        let mut p = String::new();
        while i < chars.len() && chars[i].is_ascii_digit() {
            p.push(chars[i]);
            i += 1;
        }
        precision = Some(p.parse().map_err(|_| bad())?);
    }
    let kind: String = chars[i..].iter().collect();
    let width: usize = if width.is_empty() { 0 } else { width.parse().map_err(|_| bad())? };

    let numeric = matches!(v, Value::Int(_) | Value::Float(_));
    let mut body = match (kind.as_str(), v) {
        ("?" | "#?", _) => v.render(true),
        ("x", Value::Int(n)) => format!("{}{:x}", if alt { "0x" } else { "" }, n),
        ("X", Value::Int(n)) => format!("{}{:X}", if alt { "0x" } else { "" }, n),
        ("b", Value::Int(n)) => format!("{}{:b}", if alt { "0b" } else { "" }, n),
        ("o", Value::Int(n)) => format!("{}{:o}", if alt { "0o" } else { "" }, n),
        ("e", Value::Float(f)) => match precision {
            Some(p) => format!("{:.*e}", p, f),
            None => format!("{:e}", f),
        },
        ("", Value::Float(f)) if precision.is_some() => format!("{:.*}", precision.unwrap(), f),
        ("", Value::Str(s)) if precision.is_some() => s.chars().take(precision.unwrap()).collect(),
        ("", _) => display(v, env)?,
        _ => return Err(bad()),
    };
    if plus && numeric && !body.starts_with('-') {
        body.insert(0, '+');
    }
    let len = body.chars().count();
    if len >= width {
        return Ok(body);
    }
    let pad = width - len;
    if zero && numeric {
        let (sign, digits) = if body.starts_with(['-', '+']) { body.split_at(1) } else { ("", body.as_str()) };
        return Ok(format!("{}{}{}", sign, "0".repeat(pad), digits));
    }
    let fill_str = |n: usize| fill.to_string().repeat(n);
    Ok(match align.unwrap_or(if numeric { '>' } else { '<' }) {
        '<' => format!("{}{}", body, fill_str(pad)),
        '>' => format!("{}{}", fill_str(pad), body),
        _ => format!("{}{}{}", fill_str(pad / 2), body, fill_str(pad - pad / 2)),
    })
}

// ---------- functions ----------

pub fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "print" | "println" | "eprint" | "eprintln" | "format" | "panic" | "assert" | "assert_eq" | "assert_ne"
            | "todo" | "unreachable" | "unimplemented" | "dbg" | "len" | "str" | "int" | "float" | "input"
            | "read_line" | "type_of" | "abs" | "sqrt" | "min" | "max" | "random" | "random_float" | "time_ms"
            | "now" | "sleep" | "read_file" | "write_file" | "append_file" | "file_exists" | "exit" | "env_var"
            | "args"
    )
}

thread_local! {
    static RNG: Cell<u64> = const { Cell::new(0) };
}

/// xorshift64*, seeded from the clock on first use.
fn next_random() -> u64 {
    RNG.with(|r| {
        let mut x = r.get();
        if x == 0 {
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x2545F4914F6CDD1D);
            x = t | 1;
        }
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        r.set(x);
        x.wrapping_mul(0x2545F4914F6CDD1D)
    })
}

fn io_result(r: std::io::Result<Value>) -> Value {
    match r {
        Ok(v) => value::ok(v),
        Err(e) => value::err(Value::Str(e.to_string())),
    }
}

pub fn call(name: &str, args: Vec<Value>, span: Span) -> Result<Value> {
    match name {
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
                Value::Str(s) => s
                    .trim()
                    .parse::<i64>()
                    .map(Value::Int)
                    .map_err(|_| Error::runtime(span, format!("cannot parse {:?} as int", s))),
                v => cast(v, "int", span),
            }
        }
        "float" => {
            arity(name, &args, 1, span)?;
            match &args[0] {
                Value::Str(s) => s
                    .trim()
                    .parse::<f64>()
                    .map(Value::Float)
                    .map_err(|_| Error::runtime(span, format!("cannot parse {:?} as float", s))),
                v => cast(v, "float", span),
            }
        }
        "input" | "read_line" => {
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
            let pick_first = match compare(&args[0], &args[1]) {
                Some(o) if args[0].same_kind(&args[1]) => (o.is_le()) == (name == "min"),
                _ => {
                    return Err(Error::runtime(
                        span,
                        format!("`{}` needs two comparable values, got {} and {}", name, args[0].type_name(), args[1].type_name()),
                    ))
                }
            };
            Ok(args.into_iter().nth(if pick_first { 0 } else { 1 }).unwrap())
        }
        "random" => {
            arity(name, &args, 2, span)?;
            let lo = int_arg(&args, 0, "`random` low bound", span)?;
            let hi = int_arg(&args, 1, "`random` high bound", span)?;
            if lo >= hi {
                return Err(Error::runtime(span, format!("`random({}, {})` needs low < high (high is exclusive)", lo, hi)));
            }
            let range = (hi as i128 - lo as i128) as u128;
            Ok(Value::Int((lo as i128 + (next_random() as u128 % range) as i128) as i64))
        }
        "random_float" => {
            arity(name, &args, 0, span)?;
            Ok(Value::Float((next_random() >> 11) as f64 / (1u64 << 53) as f64))
        }
        "time_ms" => {
            arity(name, &args, 0, span)?;
            let ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            Ok(Value::Int(ms))
        }
        "now" => {
            arity(name, &args, 0, span)?;
            let s = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            Ok(Value::Float(s))
        }
        "sleep" => {
            arity(name, &args, 1, span)?;
            let ms = int_arg(&args, 0, "`sleep` milliseconds", span)?;
            std::thread::sleep(std::time::Duration::from_millis(ms.max(0) as u64));
            Ok(Value::Unit)
        }
        "read_file" => {
            arity(name, &args, 1, span)?;
            let path = str_arg(&args, 0, "`read_file` path", span)?;
            Ok(io_result(std::fs::read_to_string(path).map(Value::Str)))
        }
        "write_file" | "append_file" => {
            arity(name, &args, 2, span)?;
            let path = str_arg(&args, 0, "file path", span)?;
            let text = args[1].render(false);
            let r = if name == "write_file" {
                std::fs::write(path, text)
            } else {
                std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(path)
                    .and_then(|mut f| f.write_all(text.as_bytes()))
            };
            Ok(io_result(r.map(|_| Value::Unit)))
        }
        "file_exists" => {
            arity(name, &args, 1, span)?;
            Ok(Value::Bool(std::path::Path::new(str_arg(&args, 0, "path", span)?).exists()))
        }
        "env_var" => {
            arity(name, &args, 1, span)?;
            Ok(value::option(std::env::var(str_arg(&args, 0, "variable name", span)?).ok().map(Value::Str)))
        }
        "exit" => {
            arity(name, &args, 1, span)?;
            let code = int_arg(&args, 0, "exit code", span)?;
            io::stdout().flush().ok();
            std::process::exit(code as i32);
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

/// Constants reached through a path, like `i64::MAX` or `std::f64::consts::PI`.
pub fn path_const(ty: &str, item: &str) -> Option<Value> {
    Some(match (ty, item) {
        ("consts", "PI") | ("f64", "PI") => Value::Float(std::f64::consts::PI),
        ("consts", "E") => Value::Float(std::f64::consts::E),
        ("consts", "TAU") => Value::Float(std::f64::consts::TAU),
        ("consts", "SQRT_2") => Value::Float(std::f64::consts::SQRT_2),
        ("consts", "LN_2") => Value::Float(std::f64::consts::LN_2),
        ("consts", "LN_10") => Value::Float(std::f64::consts::LN_10),
        ("f64" | "float" | "f32", "MAX") => Value::Float(f64::MAX),
        ("f64" | "float" | "f32", "MIN") => Value::Float(f64::MIN),
        ("f64" | "float" | "f32", "INFINITY") => Value::Float(f64::INFINITY),
        ("f64" | "float" | "f32", "NEG_INFINITY") => Value::Float(f64::NEG_INFINITY),
        ("f64" | "float" | "f32", "NAN") => Value::Float(f64::NAN),
        ("f64" | "float" | "f32", "EPSILON") => Value::Float(f64::EPSILON),
        ("i64" | "int" | "isize" | "i32" | "usize" | "u64" | "u32", "MAX") => Value::Int(i64::MAX),
        ("i64" | "int" | "isize", "MIN") => Value::Int(i64::MIN),
        ("i32", "MIN") => Value::Int(i32::MIN as i64),
        ("usize" | "u64" | "u32" | "u8", "MIN") => Value::Int(0),
        ("u8", "MAX") => Value::Int(255),
        _ => return None,
    })
}

/// Associated functions of built-in types, like `String::from` or `HashMap::new`.
pub fn path_call(ty: &str, item: &str, args: &[Value], span: Span) -> Option<Result<Value>> {
    let r = match (ty, item) {
        ("String", "new") => Ok(Value::Str(String::new())),
        ("String", "from") | ("String", "from_str") => arity(item, args, 1, span).map(|_| Value::Str(args[0].render(false))),
        ("String" | "Vec" | "HashMap" | "HashSet" | "BTreeMap" | "BTreeSet", "with_capacity") => {
            return path_call(ty, "new", &[], span)
        }
        ("Vec" | "VecDeque", "new") => Ok(Value::Array(Vec::new())),
        ("Vec", "from") => arity(item, args, 1, span).and_then(|_| match &args[0] {
            Value::Array(_) => Ok(args[0].clone()),
            v => Err(type_err(span, "`Vec::from`", v)),
        }),
        ("HashMap" | "BTreeMap", "new") => Ok(Value::Map(BTreeMap::new())),
        ("HashSet" | "BTreeSet", "new") => Ok(Value::Set(BTreeSet::new())),
        ("HashMap" | "BTreeMap", "from") => arity(item, args, 1, span).and_then(|_| match &args[0] {
            Value::Array(items) => {
                let mut m = BTreeMap::new();
                for it in items {
                    match it {
                        Value::Tuple(kv) if kv.len() == 2 => {
                            m.insert(Key(kv[0].clone()), kv[1].clone());
                        }
                        v => return Err(type_err(span, "`HashMap::from` (needs an array of (key, value) tuples)", v)),
                    }
                }
                Ok(Value::Map(m))
            }
            v => Err(type_err(span, "`HashMap::from`", v)),
        }),
        ("HashSet" | "BTreeSet", "from") => arity(item, args, 1, span).and_then(|_| match &args[0] {
            Value::Array(items) => Ok(Value::Set(items.iter().cloned().map(Key).collect())),
            v => Err(type_err(span, "`HashSet::from`", v)),
        }),
        ("Box" | "Rc" | "Arc" | "RefCell" | "Cell", "new") => arity(item, args, 1, span).map(|_| args[0].clone()),
        ("char", "from_u32") => arity(item, args, 1, span).map(|_| match &args[0] {
            Value::Int(n) => value::option(char::from_u32(*n as u32).map(|c| Value::Str(c.to_string()))),
            _ => value::none(),
        }),
        ("char", "from_digit") => arity(item, args, 2, span).map(|_| match (&args[0], &args[1]) {
            (Value::Int(d), Value::Int(r)) => {
                value::option(char::from_digit(*d as u32, *r as u32).map(|c| Value::Str(c.to_string())))
            }
            _ => value::none(),
        }),
        ("fs", "read_to_string") => call("read_file", args.to_vec(), span),
        ("fs", "write") => call("write_file", args.to_vec(), span),
        ("process", "exit") => call("exit", args.to_vec(), span),
        ("thread", "sleep") => call("sleep", args.to_vec(), span),
        ("cmp", "max") => call("max", args.to_vec(), span),
        ("cmp", "min") => call("min", args.to_vec(), span),
        ("mem", "swap") => return None,
        _ => return None,
    };
    Some(r)
}

/// `value as type`
pub fn cast(v: &Value, ty: &str, span: Span) -> Result<Value> {
    Ok(match (v, ty) {
        (Value::Int(n), "int") => Value::Int(*n),
        (Value::Int(n), "float") => Value::Float(*n as f64),
        (Value::Float(f), "float") => Value::Float(*f),
        (Value::Float(f), "int") => {
            if f.is_nan() {
                Value::Int(0)
            } else {
                Value::Int(*f as i64) // saturates, like Rust
            }
        }
        (Value::Bool(b), "int") => Value::Int(*b as i64),
        (Value::Bool(b), "float") => Value::Float(*b as i64 as f64),
        (Value::Str(s), "int") if single_char(s).is_some() => Value::Int(single_char(s).unwrap() as i64),
        (Value::Int(n), "str") => match u32::try_from(*n).ok().and_then(char::from_u32) {
            Some(c) => Value::Str(c.to_string()),
            None => return Err(Error::runtime(span, format!("{} is not a valid character code", n))),
        },
        (Value::Str(s), "str") => Value::Str(s.clone()),
        (v, ty) => return Err(Error::runtime(span, format!("cannot cast a value of type {} as {}", v.type_name(), ty))),
    })
}

/// Methods that modify the receiver in place.
pub fn is_mutating_method(name: &str) -> bool {
    matches!(
        name,
        "push" | "pop" | "insert" | "remove" | "clear" | "sort" | "sort_unstable" | "reverse" | "extend" | "append"
            | "truncate" | "swap" | "dedup" | "push_str" | "drain"
    )
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
        Value::Map(m) => m
            .get(&Key(idx.clone()))
            .cloned()
            .ok_or_else(|| Error::runtime(span, format!("key {} not found in map", idx.render(true)))),
        other => Err(Error::runtime(span, format!("cannot index into a value of type {}", other.type_name()))),
    }
}

/// `base[lo..hi]`
pub fn slice(base: &Value, lo: Option<i64>, hi: Option<i64>, inclusive: bool, span: Span) -> Result<Value> {
    let len = match base {
        Value::Array(items) => items.len(),
        Value::Str(s) => s.chars().count(),
        other => return Err(Error::runtime(span, format!("cannot slice a value of type {}", other.type_name()))),
    } as i64;
    let start = lo.unwrap_or(0);
    let end = match hi {
        Some(h) if inclusive => h + 1,
        Some(h) => h,
        None => len,
    };
    if start < 0 || end > len || start > end {
        return Err(Error::runtime(span, format!("slice range {}..{} is out of bounds for length {}", start, end, len)));
    }
    let (s, e) = (start as usize, end as usize);
    Ok(match base {
        Value::Array(items) => Value::Array(items[s..e].to_vec()),
        Value::Str(st) => Value::Str(st.chars().skip(s).take(e - s).collect()),
        _ => unreachable!(),
    })
}

pub fn index_mut<'a>(base: &'a mut Value, idx: &Value, span: Span) -> Result<&'a mut Value> {
    match base {
        Value::Array(items) => {
            let i = to_index(idx, items.len(), false, span)?;
            Ok(&mut items[i])
        }
        Value::Map(m) => m
            .get_mut(&Key(idx.clone()))
            .ok_or_else(|| Error::runtime(span, format!("key {} not found in map", idx.render(true)))),
        Value::Str(_) => Err(Error::runtime(span, "cannot assign to a character of a string; strings are immutable")),
        other => Err(Error::runtime(span, format!("cannot index into a value of type {}", other.type_name()))),
    }
}

/// Arrays may mix different struct/enum types (for trait objects), but not other kinds.
pub fn array_compatible(a: &Value, b: &Value) -> bool {
    let user = |v: &Value| matches!(v, Value::Struct(..)) || matches!(v, Value::Enum(e, _, _) if e != "Option" && e != "Result");
    a.same_kind(b) || (user(a) && user(b))
}

pub fn method_mut(recv: &mut Value, name: &str, args: Vec<Value>, span: Span) -> Result<Value> {
    let tn = recv.type_name();
    let no_method = || Error::runtime(span, format!("no method `{}` on type {}", name, tn));
    match recv {
        Value::Array(items) => array_method_mut(items, name, args, span).ok_or_else(no_method)?,
        Value::Str(s) => match name {
            "push_str" | "push" => {
                arity(name, &args, 1, span)?;
                s.push_str(str_arg(&args, 0, "pushed text", span)?);
                Ok(Value::Unit)
            }
            "pop" => Ok(value::option(s.pop().map(|c| Value::Str(c.to_string())))),
            "clear" => {
                s.clear();
                Ok(Value::Unit)
            }
            "truncate" => {
                let n = int_arg(&args, 0, "`truncate` length", span)?.max(0) as usize;
                *s = s.chars().take(n).collect();
                Ok(Value::Unit)
            }
            "insert" | "insert_str" => {
                arity(name, &args, 2, span)?;
                let mut chars: Vec<char> = s.chars().collect();
                let i = to_index(&args[0], chars.len(), true, span)?;
                let text = str_arg(&args, 1, "inserted text", span)?;
                chars.splice(i..i, text.chars());
                *s = chars.into_iter().collect();
                Ok(Value::Unit)
            }
            "remove" => {
                arity(name, &args, 1, span)?;
                let mut chars: Vec<char> = s.chars().collect();
                let i = to_index(&args[0], chars.len(), false, span)?;
                let c = chars.remove(i);
                *s = chars.into_iter().collect();
                Ok(Value::Str(c.to_string()))
            }
            _ => Err(no_method()),
        },
        Value::Map(m) => match name {
            "insert" => {
                arity(name, &args, 2, span)?;
                let mut it = args.into_iter();
                let (k, v) = (it.next().unwrap(), it.next().unwrap());
                if let Some((_, existing)) = m.iter().next() {
                    if !existing.same_kind(&v) {
                        return Err(Error::runtime(
                            span,
                            format!("mismatched types: cannot put a {} into a map of {}", v.type_name(), existing.type_name()),
                        ));
                    }
                }
                Ok(value::option(m.insert(Key(k), v)))
            }
            "remove" => {
                arity(name, &args, 1, span)?;
                Ok(value::option(m.remove(&Key(args[0].clone()))))
            }
            "clear" => {
                m.clear();
                Ok(Value::Unit)
            }
            "extend" => {
                arity(name, &args, 1, span)?;
                match &args[0] {
                    Value::Map(other) => m.extend(other.iter().map(|(k, v)| (k.clone(), v.clone()))),
                    Value::Array(items) => {
                        for it in items {
                            match it {
                                Value::Tuple(kv) if kv.len() == 2 => {
                                    m.insert(Key(kv[0].clone()), kv[1].clone());
                                }
                                v => return Err(type_err(span, "`extend` (needs (key, value) tuples)", v)),
                            }
                        }
                    }
                    v => return Err(type_err(span, "`extend`", v)),
                }
                Ok(Value::Unit)
            }
            _ => Err(no_method()),
        },
        Value::Set(set) => match name {
            "insert" => {
                arity(name, &args, 1, span)?;
                Ok(Value::Bool(set.insert(Key(args[0].clone()))))
            }
            "remove" => {
                arity(name, &args, 1, span)?;
                Ok(Value::Bool(set.remove(&Key(args[0].clone()))))
            }
            "clear" => {
                set.clear();
                Ok(Value::Unit)
            }
            "extend" => {
                arity(name, &args, 1, span)?;
                match &args[0] {
                    Value::Array(items) => set.extend(items.iter().cloned().map(Key)),
                    Value::Set(other) => set.extend(other.iter().cloned()),
                    v => return Err(type_err(span, "`extend`", v)),
                }
                Ok(Value::Unit)
            }
            _ => Err(no_method()),
        },
        _ => Err(no_method()),
    }
}

fn array_method_mut(items: &mut Vec<Value>, name: &str, args: Vec<Value>, span: Span) -> Option<Result<Value>> {
    if !is_mutating_method(name) || name == "push_str" {
        return None;
    }
    let check_kind = |items: &Vec<Value>, v: &Value| -> Result<()> {
        match items.first() {
            Some(first) if !array_compatible(first, v) => Err(Error::runtime(
                span,
                format!("mismatched types: cannot put a {} into an array of {}", v.type_name(), first.type_name()),
            )),
            _ => Ok(()),
        }
    };
    Some((|| match name {
        "push" => {
            arity(name, &args, 1, span)?;
            let v = args.into_iter().next().unwrap();
            check_kind(items, &v)?;
            items.push(v);
            Ok(Value::Unit)
        }
        "pop" => {
            arity(name, &args, 0, span)?;
            Ok(value::option(items.pop()))
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
        "sort" | "sort_unstable" => {
            arity(name, &args, 0, span)?;
            *items = sort_values(std::mem::take(items), span)?;
            Ok(Value::Unit)
        }
        "reverse" => {
            items.reverse();
            Ok(Value::Unit)
        }
        "extend" | "append" => {
            arity(name, &args, 1, span)?;
            match &args[0] {
                Value::Array(other) => {
                    for v in other {
                        check_kind(items, v)?;
                    }
                    items.extend(other.iter().cloned());
                    Ok(Value::Unit)
                }
                v => Err(type_err(span, "`extend`", v)),
            }
        }
        "truncate" => {
            let n = int_arg(&args, 0, "`truncate` length", span)?.max(0) as usize;
            items.truncate(n);
            Ok(Value::Unit)
        }
        "swap" => {
            arity(name, &args, 2, span)?;
            let a = to_index(&args[0], items.len(), false, span)?;
            let b = to_index(&args[1], items.len(), false, span)?;
            items.swap(a, b);
            Ok(Value::Unit)
        }
        "dedup" => {
            items.dedup_by(|a, b| a.equals(b));
            Ok(Value::Unit)
        }
        "drain" => {
            // `v.drain(..)` takes everything out.
            Ok(Value::Array(std::mem::take(items)))
        }
        _ => unreachable!(),
    })())
}

pub fn sort_values(mut items: Vec<Value>, span: Span) -> Result<Vec<Value>> {
    for w in items.windows(2) {
        if compare(&w[0], &w[1]).is_none() {
            return Err(Error::runtime(
                span,
                format!("cannot sort values of type {} (only ints, floats, strings, bools and tuples of them)", w[0].type_name()),
            ));
        }
    }
    items.sort_by(|a, b| compare(a, b).unwrap_or(std::cmp::Ordering::Equal));
    Ok(items)
}

fn extreme(items: &[Value], want: std::cmp::Ordering, span: Span) -> Result<Value> {
    let mut best: Option<&Value> = None;
    for v in items {
        best = match best {
            None => Some(v),
            Some(b) => match compare(v, b) {
                Some(o) if o == want => Some(v),
                Some(_) => Some(b),
                None => return Err(Error::runtime(span, format!("cannot compare values of type {}", v.type_name()))),
            },
        };
    }
    Ok(value::option(best.cloned()))
}

fn option_result_method(recv: Value, name: &str, args: Vec<Value>, span: Span) -> Result<Value> {
    let Value::Enum(en, variant, mut data) = recv else { unreachable!() };
    let has_value = variant == "Some" || variant == "Ok";
    let payload = data.pop();
    match name {
        "is_some" | "is_ok" => Ok(Value::Bool(has_value)),
        "is_none" | "is_err" => Ok(Value::Bool(!has_value)),
        "unwrap" | "expect" => {
            if has_value {
                return Ok(payload.unwrap());
            }
            let msg = if name == "expect" {
                arity(name, &args, 1, span)?;
                match payload {
                    Some(e) => format!("{}: {}", args[0].render(false), e.render(true)),
                    None => args[0].render(false),
                }
            } else if variant == "None" {
                "called `unwrap()` on a `None` value".to_string()
            } else {
                format!("called `unwrap()` on an `Err` value: {}", payload.unwrap().render(true))
            };
            Err(Error::runtime(span, format!("panicked: {}", msg)))
        }
        "unwrap_err" | "expect_err" if en == "Result" => {
            if has_value {
                Err(Error::runtime(span, format!("panicked: called `unwrap_err()` on an `Ok` value: {}", payload.unwrap().render(true))))
            } else {
                Ok(payload.unwrap())
            }
        }
        "unwrap_or" => {
            arity(name, &args, 1, span)?;
            Ok(if has_value { payload.unwrap() } else { args.into_iter().next().unwrap() })
        }
        "unwrap_or_default" => Ok(match payload {
            Some(v) if has_value => v,
            _ => Value::Int(0),
        }),
        "ok" if en == "Result" => Ok(value::option(if has_value { payload } else { None })),
        "err" if en == "Result" => Ok(value::option(if has_value { None } else { payload })),
        "ok_or" if en == "Option" => {
            arity(name, &args, 1, span)?;
            Ok(match payload {
                Some(v) => value::ok(v),
                None => value::err(args.into_iter().next().unwrap()),
            })
        }
        "iter" | "clone" | "cloned" | "copied" | "as_ref" | "as_mut" | "as_deref" => {
            Ok(Value::Enum(en, variant, payload.into_iter().collect()))
        }
        "to_string" => Ok(Value::Str(Value::Enum(en, variant, payload.into_iter().collect()).render(false))),
        _ => Err(Error::runtime(span, format!("no method `{}` on type {}", name, en))),
    }
}

fn int_method(n: i64, name: &str, args: &[Value], span: Span) -> Option<Result<Value>> {
    let overflow = || Error::runtime(span, format!("attempt to compute `{}` with overflow", name));
    let r = match name {
        "abs" => n.checked_abs().map(Value::Int).ok_or_else(overflow),
        "pow" => match args.first() {
            Some(Value::Int(e)) if *e >= 0 && *e <= u32::MAX as i64 => {
                n.checked_pow(*e as u32).map(Value::Int).ok_or_else(overflow)
            }
            Some(Value::Int(_)) => Err(Error::runtime(span, "int `pow` needs a non-negative exponent")),
            _ => Err(Error::runtime(span, "int `pow` needs an int exponent")),
        },
        "min" | "max" => int_arg(args, 0, "the other value", span).map(|m| Value::Int(if name == "min" { n.min(m) } else { n.max(m) })),
        "clamp" => (|| {
            let lo = int_arg(args, 0, "`clamp` low", span)?;
            let hi = int_arg(args, 1, "`clamp` high", span)?;
            if lo > hi {
                return Err(Error::runtime(span, "`clamp` needs low <= high"));
            }
            Ok(Value::Int(n.clamp(lo, hi)))
        })(),
        "signum" => Ok(Value::Int(n.signum())),
        "is_positive" => Ok(Value::Bool(n > 0)),
        "is_negative" => Ok(Value::Bool(n < 0)),
        "rem_euclid" | "div_euclid" => int_arg(args, 0, "divisor", span).and_then(|d| {
            if d == 0 {
                return Err(Error::runtime(span, "attempt to divide by zero"));
            }
            let r = if name == "rem_euclid" { n.checked_rem_euclid(d) } else { n.checked_div_euclid(d) };
            r.map(Value::Int).ok_or_else(overflow)
        }),
        "count_ones" => Ok(Value::Int(n.count_ones() as i64)),
        "leading_zeros" => Ok(Value::Int(n.leading_zeros() as i64)),
        "trailing_zeros" => Ok(Value::Int(n.trailing_zeros() as i64)),
        "checked_add" | "checked_sub" | "checked_mul" | "checked_div" => int_arg(args, 0, "the other value", span).map(|m| {
            value::option(
                match name {
                    "checked_add" => n.checked_add(m),
                    "checked_sub" => n.checked_sub(m),
                    "checked_mul" => n.checked_mul(m),
                    _ => n.checked_div(m),
                }
                .map(Value::Int),
            )
        }),
        "wrapping_add" | "wrapping_sub" | "wrapping_mul" => int_arg(args, 0, "the other value", span).map(|m| {
            Value::Int(match name {
                "wrapping_add" => n.wrapping_add(m),
                "wrapping_sub" => n.wrapping_sub(m),
                _ => n.wrapping_mul(m),
            })
        }),
        "saturating_add" | "saturating_sub" | "saturating_mul" => int_arg(args, 0, "the other value", span).map(|m| {
            Value::Int(match name {
                "saturating_add" => n.saturating_add(m),
                "saturating_sub" => n.saturating_sub(m),
                _ => n.saturating_mul(m),
            })
        }),
        "sqrt" => Err(Error::runtime(span, "`sqrt` needs a float; convert with `x as float` first")),
        "to_float" => Ok(Value::Float(n as f64)),
        _ => return None,
    };
    Some(r)
}

fn float_method(f: f64, name: &str, args: &[Value], span: Span) -> Option<Result<Value>> {
    let unary = |g: fn(f64) -> f64| Ok(Value::Float(g(f)));
    let r = match name {
        "abs" => unary(f64::abs),
        "sqrt" => unary(f64::sqrt),
        "cbrt" => unary(f64::cbrt),
        "floor" => unary(f64::floor),
        "ceil" => unary(f64::ceil),
        "round" => unary(f64::round),
        "trunc" => unary(f64::trunc),
        "fract" => unary(f64::fract),
        "sin" => unary(f64::sin),
        "cos" => unary(f64::cos),
        "tan" => unary(f64::tan),
        "asin" => unary(f64::asin),
        "acos" => unary(f64::acos),
        "atan" => unary(f64::atan),
        "exp" => unary(f64::exp),
        "ln" => unary(f64::ln),
        "log10" => unary(f64::log10),
        "log2" => unary(f64::log2),
        "signum" => unary(f64::signum),
        "to_degrees" => unary(f64::to_degrees),
        "to_radians" => unary(f64::to_radians),
        "recip" => unary(f64::recip),
        "is_nan" => Ok(Value::Bool(f.is_nan())),
        "is_finite" => Ok(Value::Bool(f.is_finite())),
        "is_infinite" => Ok(Value::Bool(f.is_infinite())),
        "log" => float_arg(args, 0, "`log` base", span).map(|b| Value::Float(f.log(b))),
        "atan2" => float_arg(args, 0, "`atan2` argument", span).map(|o| Value::Float(f.atan2(o))),
        "hypot" => float_arg(args, 0, "`hypot` argument", span).map(|o| Value::Float(f.hypot(o))),
        "powf" => float_arg(args, 0, "`powf` exponent", span).map(|e| Value::Float(f.powf(e))),
        "powi" | "pow" => match args.first() {
            Some(Value::Int(e)) => Ok(Value::Float(f.powi(*e as i32))),
            Some(Value::Float(e)) => Ok(Value::Float(f.powf(*e))),
            _ => Err(Error::runtime(span, "`pow` needs an exponent")),
        },
        "min" => float_arg(args, 0, "the other value", span).map(|o| Value::Float(f.min(o))),
        "max" => float_arg(args, 0, "the other value", span).map(|o| Value::Float(f.max(o))),
        "clamp" => (|| {
            let lo = float_arg(args, 0, "`clamp` low", span)?;
            let hi = float_arg(args, 1, "`clamp` high", span)?;
            Ok(Value::Float(f.clamp(lo, hi)))
        })(),
        "to_int" => cast(&Value::Float(f), "int", span),
        _ => return None,
    };
    Some(r)
}

fn str_method(s: &str, name: &str, args: &[Value], span: Span) -> Option<Result<Value>> {
    let strs = |it: &mut dyn Iterator<Item = &str>| Value::Array(it.map(|p| Value::Str(p.to_string())).collect());
    let all_chars = |pred: fn(char) -> bool| Ok(Value::Bool(!s.is_empty() && s.chars().all(pred)));
    let r = match name {
        "len" => Ok(Value::Int(s.chars().count() as i64)),
        "is_empty" => Ok(Value::Bool(s.is_empty())),
        "to_upper" | "to_uppercase" | "to_ascii_uppercase" => Ok(Value::Str(s.to_uppercase())),
        "to_lower" | "to_lowercase" | "to_ascii_lowercase" => Ok(Value::Str(s.to_lowercase())),
        "trim" => Ok(Value::Str(s.trim().to_string())),
        "trim_start" => Ok(Value::Str(s.trim_start().to_string())),
        "trim_end" => Ok(Value::Str(s.trim_end().to_string())),
        "chars" => Ok(Value::Array(s.chars().map(|c| Value::Str(c.to_string())).collect())),
        "bytes" => Ok(Value::Array(s.bytes().map(|b| Value::Int(b as i64)).collect())),
        "lines" => Ok(strs(&mut s.lines())),
        "split_whitespace" => Ok(strs(&mut s.split_whitespace())),
        "split" => match args.first() {
            Some(Value::Str(sep)) if sep.is_empty() => Err(Error::runtime(span, "`split` separator must not be empty")),
            Some(Value::Str(sep)) => Ok(strs(&mut s.split(sep.as_str()))),
            _ => Err(Error::runtime(span, "`split` needs a string separator")),
        },
        "splitn" => (|| {
            let n = int_arg(args, 0, "`splitn` count", span)?.max(0) as usize;
            let sep = str_arg(args, 1, "`splitn` separator", span)?;
            Ok(strs(&mut s.splitn(n, sep)))
        })(),
        "split_once" => str_arg(args, 0, "`split_once` separator", span).map(|sep| {
            value::option(s.split_once(sep).map(|(a, b)| Value::Tuple(vec![Value::Str(a.into()), Value::Str(b.into())])))
        }),
        "contains" => str_arg(args, 0, "`contains` argument", span).map(|sub| Value::Bool(s.contains(sub))),
        "starts_with" => str_arg(args, 0, "`starts_with` argument", span).map(|p| Value::Bool(s.starts_with(p))),
        "ends_with" => str_arg(args, 0, "`ends_with` argument", span).map(|p| Value::Bool(s.ends_with(p))),
        "strip_prefix" => str_arg(args, 0, "prefix", span).map(|p| value::option(s.strip_prefix(p).map(|r| Value::Str(r.into())))),
        "strip_suffix" => str_arg(args, 0, "suffix", span).map(|p| value::option(s.strip_suffix(p).map(|r| Value::Str(r.into())))),
        "find" => str_arg(args, 0, "`find` argument", span)
            .map(|sub| value::option(s.find(sub).map(|b| Value::Int(s[..b].chars().count() as i64)))),
        "replace" => (|| {
            let from = str_arg(args, 0, "`replace` pattern", span)?;
            let to = str_arg(args, 1, "`replace` replacement", span)?;
            Ok(Value::Str(s.replace(from, to)))
        })(),
        "repeat" => int_arg(args, 0, "`repeat` count", span).map(|n| Value::Str(s.repeat(n.max(0) as usize))),
        "reverse" | "rev" => Ok(Value::Str(s.chars().rev().collect())),
        "parse_int" => Ok(match s.trim().parse::<i64>() {
            Ok(n) => value::ok(Value::Int(n)),
            Err(_) => value::err(Value::Str(format!("invalid int: {:?}", s))),
        }),
        "parse_float" => Ok(match s.trim().parse::<f64>() {
            Ok(f) => value::ok(Value::Float(f)),
            Err(_) => value::err(Value::Str(format!("invalid float: {:?}", s))),
        }),
        "parse" => Ok(match (s.trim().parse::<i64>(), s.trim().parse::<f64>()) {
            (Ok(n), _) => value::ok(Value::Int(n)),
            (_, Ok(f)) => value::ok(Value::Float(f)),
            _ => value::err(Value::Str(format!("invalid number: {:?}", s))),
        }),
        "is_digit" | "is_numeric" | "is_ascii_digit" => all_chars(|c| c.is_ascii_digit()),
        "is_alphabetic" | "is_ascii_alphabetic" => all_chars(char::is_alphabetic),
        "is_alphanumeric" | "is_ascii_alphanumeric" => all_chars(char::is_alphanumeric),
        "is_whitespace" | "is_ascii_whitespace" => all_chars(char::is_whitespace),
        "is_uppercase" | "is_ascii_uppercase" => all_chars(char::is_uppercase),
        "is_lowercase" | "is_ascii_lowercase" => all_chars(char::is_lowercase),
        "is_ascii_punctuation" => all_chars(|c| c.is_ascii_punctuation()),
        "to_digit" => {
            let radix = match args.first() {
                Some(Value::Int(r)) => *r as u32,
                _ => 10,
            };
            Ok(value::option(single_char(s).and_then(|c| c.to_digit(radix)).map(|d| Value::Int(d as i64))))
        }
        "char_at" | "nth" => int_arg(args, 0, "index", span)
            .map(|i| value::option(s.chars().nth(i.max(0) as usize).map(|c| Value::Str(c.to_string())))),
        "as_str" | "to_owned" | "clone" | "as_bytes_str" => Ok(Value::Str(s.to_string())),
        _ => return None,
    };
    Some(r)
}

fn array_method(items: &[Value], name: &str, args: &[Value], span: Span) -> Option<Result<Value>> {
    let r = match name {
        "len" => Ok(Value::Int(items.len() as i64)),
        "is_empty" => Ok(Value::Bool(items.is_empty())),
        "contains" => arity(name, args, 1, span).map(|_| Value::Bool(items.iter().any(|v| v.equals(&args[0])))),
        "get" => arity(name, args, 1, span).and_then(|_| match &args[0] {
            Value::Int(i) if *i >= 0 => Ok(value::option(items.get(*i as usize).cloned())),
            Value::Int(_) => Ok(value::none()),
            v => Err(type_err(span, "`get` index", v)),
        }),
        "first" | "first_mut" => Ok(value::option(items.first().cloned())),
        "last" | "last_mut" => Ok(value::option(items.last().cloned())),
        "get_mut" => return array_method(items, "get", args, span),
        "enumerate" => Ok(Value::Array(
            items.iter().enumerate().map(|(i, v)| Value::Tuple(vec![Value::Int(i as i64), v.clone()])).collect(),
        )),
        "zip" => arity(name, args, 1, span).and_then(|_| match &args[0] {
            Value::Array(other) => Ok(Value::Array(
                items.iter().zip(other).map(|(a, b)| Value::Tuple(vec![a.clone(), b.clone()])).collect(),
            )),
            v => Err(type_err(span, "`zip`", v)),
        }),
        "sum" | "product" => {
            let is_sum = name == "sum";
            let mut int_total: i64 = if is_sum { 0 } else { 1 };
            let mut float_total: Option<f64> = None;
            (|| {
                for v in items {
                    match v {
                        Value::Int(n) => {
                            int_total = if is_sum { int_total.checked_add(*n) } else { int_total.checked_mul(*n) }
                                .ok_or_else(|| Error::runtime(span, format!("attempt to compute `{}` with overflow", name)))?
                        }
                        Value::Float(f) => {
                            let t = float_total.get_or_insert(if is_sum { 0.0 } else { 1.0 });
                            if is_sum {
                                *t += f
                            } else {
                                *t *= f
                            }
                        }
                        v => return Err(type_err(span, &format!("`{}`", name), v)),
                    }
                }
                Ok(float_total.map_or(Value::Int(int_total), Value::Float))
            })()
        }
        "sorted" => sort_values(items.to_vec(), span).map(Value::Array),
        "min" => extreme(items, std::cmp::Ordering::Less, span),
        "max" => extreme(items, std::cmp::Ordering::Greater, span),
        "join" | "concat" if name == "join" || items.first().is_some_and(|v| matches!(v, Value::Str(_))) => {
            let sep = if name == "join" { str_arg(args, 0, "`join` separator", span).map(|s| s.to_string()) } else { Ok(String::new()) };
            sep.map(|sep| Value::Str(items.iter().map(|v| v.render(false)).collect::<Vec<_>>().join(&sep)))
        }
        "concat" | "flatten" => {
            let mut out = Vec::new();
            (|| {
                for v in items {
                    match v {
                        Value::Array(inner) => out.extend(inner.iter().cloned()),
                        Value::Enum(e, variant, data) if e == "Option" => {
                            if variant == "Some" {
                                out.extend(data.iter().cloned())
                            }
                        }
                        v => return Err(type_err(span, &format!("`{}` (needs an array of arrays)", name), v)),
                    }
                }
                Ok(Value::Array(out))
            })()
        }
        "rev" => Ok(Value::Array(items.iter().rev().cloned().collect())),
        "take" => int_arg(args, 0, "`take` count", span).map(|n| Value::Array(items.iter().take(n.max(0) as usize).cloned().collect())),
        "skip" => int_arg(args, 0, "`skip` count", span).map(|n| Value::Array(items.iter().skip(n.max(0) as usize).cloned().collect())),
        "step_by" => int_arg(args, 0, "`step_by` step", span).and_then(|n| {
            if n <= 0 {
                return Err(Error::runtime(span, "`step_by` needs a positive step"));
            }
            Ok(Value::Array(items.iter().step_by(n as usize).cloned().collect()))
        }),
        "chunks" | "windows" => int_arg(args, 0, "size", span).and_then(|n| {
            if n <= 0 {
                return Err(Error::runtime(span, format!("`{}` needs a positive size", name)));
            }
            let n = n as usize;
            let parts: Vec<Value> = if name == "chunks" {
                items.chunks(n).map(|c| Value::Array(c.to_vec())).collect()
            } else {
                items.windows(n).map(|c| Value::Array(c.to_vec())).collect()
            };
            Ok(Value::Array(parts))
        }),
        "dedup" | "unique" => {
            let mut out: Vec<Value> = Vec::new();
            for v in items {
                if name == "dedup" {
                    if out.last().is_some_and(|l| l.equals(v)) {
                        continue;
                    }
                } else if out.iter().any(|o| o.equals(v)) {
                    continue;
                }
                out.push(v.clone());
            }
            Ok(Value::Array(out))
        }
        "count" if args.is_empty() => Ok(Value::Int(items.len() as i64)),
        "split_at" => int_arg(args, 0, "`split_at` index", span).and_then(|i| {
            let i = to_index(&Value::Int(i), items.len(), true, span)?;
            Ok(Value::Tuple(vec![Value::Array(items[..i].to_vec()), Value::Array(items[i..].to_vec())]))
        }),
        "index_of" => arity(name, args, 1, span).map(|_| value::option(items.iter().position(|v| v.equals(&args[0])).map(|i| Value::Int(i as i64)))),
        "to_set" => Ok(Value::Set(items.iter().cloned().map(Key).collect())),
        "to_map" => (|| {
            let mut m = BTreeMap::new();
            for it in items {
                match it {
                    Value::Tuple(kv) if kv.len() == 2 => {
                        m.insert(Key(kv[0].clone()), kv[1].clone());
                    }
                    v => return Err(type_err(span, "`to_map` (needs (key, value) tuples)", v)),
                }
            }
            Ok(Value::Map(m))
        })(),
        _ => return None,
    };
    Some(r)
}

fn map_method(m: &BTreeMap<Key, Value>, name: &str, args: &[Value], span: Span) -> Option<Result<Value>> {
    let r = match name {
        "len" => Ok(Value::Int(m.len() as i64)),
        "is_empty" => Ok(Value::Bool(m.is_empty())),
        "get" | "get_mut" => arity(name, args, 1, span).map(|_| value::option(m.get(&Key(args[0].clone())).cloned())),
        "contains_key" => arity(name, args, 1, span).map(|_| Value::Bool(m.contains_key(&Key(args[0].clone())))),
        "keys" | "into_keys" => Ok(Value::Array(m.keys().map(|k| k.0.clone()).collect())),
        "values" | "into_values" | "values_mut" => Ok(Value::Array(m.values().cloned().collect())),
        "iter" | "into_iter" | "iter_mut" | "to_array" => {
            Ok(Value::Array(m.iter().map(|(k, v)| Value::Tuple(vec![k.0.clone(), v.clone()])).collect()))
        }
        _ => return None,
    };
    Some(r)
}

fn set_method(s: &BTreeSet<Key>, name: &str, args: &[Value], span: Span) -> Option<Result<Value>> {
    let other_set = || match args.first() {
        Some(Value::Set(o)) => Ok(o.clone()),
        Some(Value::Array(items)) => Ok(items.iter().cloned().map(Key).collect()),
        _ => Err(Error::runtime(span, format!("`{}` needs another set", name))),
    };
    let r = match name {
        "len" => Ok(Value::Int(s.len() as i64)),
        "is_empty" => Ok(Value::Bool(s.is_empty())),
        "contains" => arity(name, args, 1, span).map(|_| Value::Bool(s.contains(&Key(args[0].clone())))),
        "iter" | "into_iter" | "to_array" => Ok(Value::Array(s.iter().map(|k| k.0.clone()).collect())),
        "union" => other_set().map(|o| Value::Set(s.union(&o).cloned().collect())),
        "intersection" => other_set().map(|o| Value::Set(s.intersection(&o).cloned().collect())),
        "difference" => other_set().map(|o| Value::Set(s.difference(&o).cloned().collect())),
        "symmetric_difference" => other_set().map(|o| Value::Set(s.symmetric_difference(&o).cloned().collect())),
        "is_subset" => other_set().map(|o| Value::Bool(s.is_subset(&o))),
        "is_superset" => other_set().map(|o| Value::Bool(s.is_superset(&o))),
        "is_disjoint" => other_set().map(|o| Value::Bool(s.is_disjoint(&o))),
        _ => return None,
    };
    Some(r)
}

pub fn method(recv: Value, name: &str, args: Vec<Value>, span: Span) -> Result<Value> {
    if let Value::Enum(en, _, _) = &recv {
        if en == "Option" || en == "Result" {
            return option_result_method(recv, name, args, span);
        }
    }
    // Methods that work on every value.
    match name {
        "to_string" => return Ok(Value::Str(recv.render(false))),
        "clone" | "to_owned" | "into" | "to_vec" | "iter" | "into_iter" | "iter_mut" | "collect" | "cloned" | "copied"
        | "as_ref" | "borrow" | "borrow_mut" | "unwrap_ref"
            if !matches!(recv, Value::Map(_) | Value::Set(_)) =>
        {
            return Ok(recv)
        }
        "cmp" | "partial_cmp" => {
            arity(name, &args, 1, span)?;
            let o = compare(&recv, &args[0])
                .filter(|_| recv.same_kind(&args[0]))
                .ok_or_else(|| Error::runtime(span, format!("cannot compare {} with {}", recv.type_name(), args[0].type_name())))?;
            let ord = value::ordering(o);
            return Ok(if name == "partial_cmp" { value::some(ord) } else { ord });
        }
        "eq" | "ne" => {
            arity(name, &args, 1, span)?;
            return Ok(Value::Bool(recv.equals(&args[0]) == (name == "eq")));
        }
        _ => {}
    }
    let found = match &recv {
        Value::Int(n) => int_method(*n, name, &args, span),
        Value::Float(f) => float_method(*f, name, &args, span),
        Value::Str(s) => str_method(s, name, &args, span),
        Value::Array(items) => array_method(items, name, &args, span),
        Value::Map(m) => map_method(m, name, &args, span),
        Value::Set(s) => set_method(s, name, &args, span),
        Value::Enum(en, variant, _) if en == "Ordering" => match name {
            "reverse" => Some(Ok(Value::Enum(
                en.clone(),
                match variant.as_str() {
                    "Less" => "Greater",
                    "Greater" => "Less",
                    v => v,
                }
                .into(),
                vec![],
            ))),
            "then" => Some(match args.first() {
                Some(other @ Value::Enum(e, _, _)) if e == "Ordering" => {
                    Ok(if variant == "Equal" { other.clone() } else { recv.clone() })
                }
                _ => Err(Error::runtime(span, "`then` needs another Ordering")),
            }),
            "is_lt" | "is_le" | "is_gt" | "is_ge" | "is_eq" | "is_ne" => Some(Ok(Value::Bool(match name {
                "is_lt" => variant == "Less",
                "is_le" => variant != "Greater",
                "is_gt" => variant == "Greater",
                "is_ge" => variant != "Less",
                "is_eq" => variant == "Equal",
                _ => variant != "Equal",
            }))),
            _ => None,
        },
        _ => None,
    };
    found.unwrap_or_else(|| Err(Error::runtime(span, format!("no method `{}` on type {}", name, recv.type_name()))))
}

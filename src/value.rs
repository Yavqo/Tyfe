use crate::ast::{ClosureDef, FnDecl, Type};
use std::cmp::Ordering;
use std::fmt;
use std::rc::Rc;

/// Something that can be called: a named function or a closure.
#[derive(Debug, Clone)]
pub enum Callable {
    Named(Rc<FnDecl>),
    Closure(Rc<ClosureVal>),
}

#[derive(Debug)]
pub struct ClosureVal {
    pub def: Rc<ClosureDef>,
    /// Copies of the captured variables, taken when the closure was created.
    pub env: Vec<(String, Value)>,
}

impl Callable {
    pub fn arity(&self) -> usize {
        match self {
            Callable::Named(f) => f.params.len(),
            Callable::Closure(c) => c.def.params.len(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Array(Vec<Value>),
    /// Struct name and its fields, in declaration order.
    Struct(String, Vec<(String, Value)>),
    /// Enum name, variant name, and the variant's data.
    Enum(String, String, Vec<Value>),
    Tuple(Vec<Value>),
    Func(Callable),
    Unit,
}

pub fn some(v: Value) -> Value {
    Value::Enum("Option".into(), "Some".into(), vec![v])
}

pub fn none() -> Value {
    Value::Enum("Option".into(), "None".into(), vec![])
}

pub fn ok(v: Value) -> Value {
    Value::Enum("Result".into(), "Ok".into(), vec![v])
}

pub fn err(v: Value) -> Value {
    Value::Enum("Result".into(), "Err".into(), vec![v])
}

pub fn option(v: Option<Value>) -> Value {
    v.map_or_else(none, some)
}

impl Value {
    pub fn type_name(&self) -> String {
        match self {
            Value::Int(_) => "int".into(),
            Value::Float(_) => "float".into(),
            Value::Bool(_) => "bool".into(),
            Value::Str(_) => "str".into(),
            Value::Array(items) => match items.first() {
                Some(v) => format!("[{}]", v.type_name()),
                None => "[]".into(),
            },
            Value::Enum(en, variant, data) if en == "Option" || en == "Result" => {
                let inner = data.first().map_or("_".to_string(), |v| v.type_name());
                match variant.as_str() {
                    "Some" | "None" => format!("Option<{}>", inner),
                    "Ok" => format!("Result<{}, _>", inner),
                    _ => format!("Result<_, {}>", inner),
                }
            }
            Value::Struct(name, _) | Value::Enum(name, _, _) => name.clone(),
            Value::Tuple(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.type_name()).collect();
                format!("({})", parts.join(", "))
            }
            Value::Func(f) => format!("fn({})", vec!["_"; f.arity()].join(", ")),
            Value::Unit => "()".into(),
        }
    }

    /// Does this value fit the declared type?
    pub fn matches(&self, ty: &Type) -> bool {
        match (self, ty) {
            (Value::Int(_), Type::Int)
            | (Value::Float(_), Type::Float)
            | (Value::Bool(_), Type::Bool)
            | (Value::Str(_), Type::Str)
            | (Value::Unit, Type::Unit) => true,
            (Value::Array(items), Type::Array(inner)) => items.iter().all(|v| v.matches(inner)),
            (Value::Struct(name, _) | Value::Enum(name, _, _), Type::Named(n)) => name == n,
            (Value::Tuple(items), Type::Tuple(types)) => {
                items.len() == types.len() && items.iter().zip(types).all(|(v, t)| v.matches(t))
            }
            (Value::Enum(e, variant, data), Type::Option(t)) if e == "Option" => {
                variant == "None" || data.iter().all(|v| v.matches(t))
            }
            (Value::Enum(e, variant, data), Type::Result(t, er)) if e == "Result" => {
                let inner = if variant == "Ok" { t } else { er };
                data.iter().all(|v| v.matches(inner))
            }
            (Value::Func(f), Type::Fn(params, _)) => f.arity() == params.len(),
            (_, Type::Any) => true,
            _ => false,
        }
    }

    /// Same "shape" of type: used to keep variables, arrays and fields from
    /// silently changing type at runtime.
    pub fn same_kind(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Array(a), Value::Array(b)) => match (a.first(), b.first()) {
                (Some(x), Some(y)) => x.same_kind(y),
                _ => true,
            },
            (Value::Struct(a, _), Value::Struct(b, _)) => a == b,
            (Value::Enum(a, _, _), Value::Enum(b, _, _)) => a == b,
            (Value::Tuple(a), Value::Tuple(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.same_kind(y)),
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }

    pub fn equals(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.equals(y))
            }
            (Value::Struct(n1, f1), Value::Struct(n2, f2)) => {
                n1 == n2 && f1.iter().zip(f2).all(|((_, x), (_, y))| x.equals(y))
            }
            (Value::Enum(e1, v1, d1), Value::Enum(e2, v2, d2)) => {
                e1 == e2 && v1 == v2 && d1.iter().zip(d2).all(|(x, y)| x.equals(y))
            }
            (Value::Tuple(a), Value::Tuple(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.equals(y)),
            (Value::Unit, Value::Unit) => true,
            _ => false,
        }
    }

    /// `debug` quotes strings (used for `{:?}` and for values nested inside arrays/structs).
    pub fn render(&self, debug: bool) -> String {
        match self {
            Value::Int(n) => n.to_string(),
            Value::Float(f) => {
                if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e16 {
                    format!("{:.1}", f)
                } else {
                    f.to_string()
                }
            }
            Value::Bool(b) => b.to_string(),
            Value::Str(s) => {
                if debug {
                    format!("{:?}", s)
                } else {
                    s.clone()
                }
            }
            Value::Array(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.render(true)).collect();
                format!("[{}]", parts.join(", "))
            }
            Value::Struct(name, fields) => {
                if fields.is_empty() {
                    return format!("{} {{}}", name);
                }
                let parts: Vec<String> =
                    fields.iter().map(|(n, v)| format!("{}: {}", n, v.render(true))).collect();
                format!("{} {{ {} }}", name, parts.join(", "))
            }
            Value::Enum(_, variant, data) => {
                if data.is_empty() {
                    return variant.clone();
                }
                let parts: Vec<String> = data.iter().map(|v| v.render(true)).collect();
                format!("{}({})", variant, parts.join(", "))
            }
            Value::Tuple(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.render(true)).collect();
                if parts.len() == 1 {
                    format!("({},)", parts[0])
                } else {
                    format!("({})", parts.join(", "))
                }
            }
            Value::Func(Callable::Named(f)) => format!("<fn {}>", f.name),
            Value::Func(Callable::Closure(_)) => "<closure>".into(),
            Value::Unit => "()".into(),
        }
    }
}

/// Ordering for sorting and min/max: ints, floats, strings, and tuples/arrays of those.
pub fn compare(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        (Value::Tuple(x), Value::Tuple(y)) | (Value::Array(x), Value::Array(y)) => {
            for (p, q) in x.iter().zip(y) {
                match compare(p, q)? {
                    Ordering::Equal => continue,
                    o => return Some(o),
                }
            }
            Some(x.len().cmp(&y.len()))
        }
        _ => None,
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.render(false))
    }
}

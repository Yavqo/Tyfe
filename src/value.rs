use crate::ast::Type;
use std::fmt;

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
    Unit,
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
            Value::Struct(name, _) | Value::Enum(name, _, _) => name.clone(),
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
            Value::Unit => "()".into(),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.render(false))
    }
}

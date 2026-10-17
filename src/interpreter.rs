//! Tree-walking interpreter for Tyfe.

use crate::ast::*;
use crate::builtins::{self, FormatEnv};
use crate::error::{Error, Result};
use crate::value::{self, Callable, ClosureVal, Key, Value};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::rc::Rc;

const MAX_CALL_DEPTH: usize = 100_000;

/// Stack the interpreter thread runs on (see main.rs), and how much of it we
/// allow ourselves to use before reporting a Tyfe-level stack overflow.
pub const STACK_SIZE: usize = 1024 * 1024 * 1024;
const STACK_LIMIT: usize = STACK_SIZE - 64 * 1024 * 1024;

thread_local! {
    static STACK_BASE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Call once at the top of the interpreter thread.
pub fn mark_stack_base() {
    let marker = 0u8;
    STACK_BASE.with(|b| b.set(&marker as *const u8 as usize));
}

fn stack_exhausted() -> bool {
    let marker = 0u8;
    let here = &marker as *const u8 as usize;
    STACK_BASE.with(|b| b.get() != 0 && b.get().abs_diff(here) > STACK_LIMIT)
}

/// Traits that come with the language; implementing them needs no declaration.
const BUILTIN_TRAITS: &[&str] = &[
    "Display", "Debug", "Clone", "Copy", "PartialEq", "Eq", "PartialOrd", "Ord", "Hash", "Default", "Sized", "Send",
    "Sync",
];

struct Binding {
    value: Value,
    mutable: bool,
}

/// Anything that interrupts normal evaluation: an error, or `return` / `break` /
/// `continue` unwinding to the function or loop that handles it.
enum Ctrl {
    Err(Error),
    Return(Value),
    Break(Option<String>, Value),
    Continue(Option<String>),
}

impl From<Error> for Ctrl {
    fn from(e: Error) -> Self {
        Ctrl::Err(e)
    }
}

type R<T> = std::result::Result<T, Ctrl>;

fn rt(span: Span, msg: impl Into<String>) -> Ctrl {
    Ctrl::Err(Error::runtime(span, msg))
}

/// Convert control flow that escaped to the top level into an error.
fn into_error(c: Ctrl, span: Span) -> Error {
    match c {
        Ctrl::Err(e) => e,
        Ctrl::Return(_) => Error::runtime(span, "`return` outside of a function"),
        Ctrl::Break(Some(l), _) | Ctrl::Continue(Some(l)) => {
            Error::runtime(span, format!("no enclosing loop is labeled `'{}`", l))
        }
        Ctrl::Break(None, _) | Ctrl::Continue(None) => Error::runtime(span, "`break`/`continue` outside of a loop"),
    }
}

/// Decide what a loop does after one iteration: Ok(true) means stop.
fn loop_ctrl(r: R<Value>, label: &Option<String>) -> R<bool> {
    match r {
        Ok(_) => Ok(false),
        Err(Ctrl::Continue(l)) if l.is_none() || l == *label => Ok(false),
        Err(Ctrl::Break(l, _)) if l.is_none() || l == *label => Ok(true),
        Err(e) => Err(e),
    }
}

/// The result of calling a user function.
struct CallOut {
    ret: Value,
    /// For `&mut self` methods: `self` after the call.
    new_self: Option<Value>,
    /// For `&mut` parameters: (parameter index, value after the call).
    outs: Vec<(usize, Value)>,
}

/// The name methods are registered under for a value (`impl Trait for int` etc).
fn type_key(v: &Value) -> String {
    match v {
        Value::Struct(n, _) | Value::Enum(n, _, _) => n.clone(),
        Value::Int(_) => "int".into(),
        Value::Float(_) => "float".into(),
        Value::Str(_) => "str".into(),
        Value::Bool(_) => "bool".into(),
        Value::Array(_) => "array".into(),
        Value::Map(_) => "map".into(),
        Value::Set(_) => "set".into(),
        Value::Tuple(_) => "tuple".into(),
        Value::Func(_) => "fn".into(),
        Value::Unit => "()".into(),
    }
}

/// `impl X for i64` / `impl X for String` — map Rust type names to Tyfe's.
fn normalize_type_name(name: &str) -> String {
    match name {
        "int" | "i64" | "i32" | "i16" | "i8" | "isize" | "u64" | "u32" | "u16" | "u8" | "usize" => "int",
        "float" | "f64" | "f32" => "float",
        "str" | "String" | "char" => "str",
        "Vec" => "array",
        "HashMap" | "BTreeMap" => "map",
        "HashSet" | "BTreeSet" => "set",
        other => other,
    }
    .to_string()
}

const PRIMITIVE_KEYS: &[&str] = &["int", "float", "str", "bool", "array", "map", "set"];

pub struct Interpreter {
    fns: HashMap<String, Rc<FnDecl>>,
    structs: HashMap<String, Rc<StructDecl>>,
    enums: HashMap<String, Rc<EnumDecl>>,
    traits: HashMap<String, Rc<TraitDecl>>,
    /// (type key, trait name)
    trait_impls: HashSet<(String, String)>,
    /// Methods and associated functions, keyed by (type key, function name).
    methods: HashMap<(String, String), Rc<FnDecl>>,
    method_names: HashSet<String>,
    consts: HashMap<String, Value>,
    scopes: Vec<HashMap<String, Binding>>,
    depth: usize,
    /// Output buffers for `write!` inside `fmt` methods.
    fmt_buffers: Vec<String>,
    /// Command-line arguments after the program file, for `args()`.
    program_args: Vec<String>,
}

impl Interpreter {
    pub fn new(program_args: Vec<String>) -> Self {
        let builtin = |name: &str, variants: &[(&str, usize)]| {
            let decl = EnumDecl {
                name: name.to_string(),
                variants: variants.iter().map(|(v, n)| (v.to_string(), vec![Type::Any; *n])).collect(),
                named: Vec::new(),
                span: Span::NONE,
            };
            (name.to_string(), Rc::new(decl))
        };
        Interpreter {
            fns: HashMap::new(),
            structs: HashMap::new(),
            enums: HashMap::from([
                builtin("Option", &[("Some", 1), ("None", 0)]),
                builtin("Result", &[("Ok", 1), ("Err", 1)]),
                builtin("Ordering", &[("Less", 0), ("Equal", 0), ("Greater", 0)]),
            ]),
            traits: HashMap::new(),
            trait_impls: HashSet::new(),
            methods: HashMap::new(),
            method_names: HashSet::new(),
            consts: HashMap::new(),
            scopes: vec![HashMap::new()],
            depth: 0,
            fmt_buffers: Vec::new(),
            program_args,
        }
    }

    /// Register items. With `allow_redefine` (the REPL), later definitions
    /// replace earlier ones; otherwise duplicates are an error.
    pub fn load(&mut self, items: Vec<Item>, allow_redefine: bool) -> Result<()> {
        let dup = |span: Span, what: &str, name: &str| {
            Err(Error::syntax(span, format!("{} `{}` is defined more than once", what, name)))
        };
        let mut consts = Vec::new();
        let mut to_check: Vec<(Type, Span)> = Vec::new();
        let mut impls: Vec<(String, Option<String>, Vec<String>, Span)> = Vec::new();

        for item in items {
            match item {
                Item::Fn(f) => {
                    if builtins::is_builtin(&f.name) {
                        return Err(Error::syntax(f.span, format!("`{}` is a built-in function and cannot be redefined", f.name)));
                    }
                    if !allow_redefine && self.fns.contains_key(&f.name) {
                        return dup(f.span, "function", &f.name);
                    }
                    fn_types(&f, &mut to_check);
                    self.fns.insert(f.name.clone(), Rc::new(f));
                }
                Item::Struct(s) => {
                    if !allow_redefine && self.type_exists(&s.name) {
                        return dup(s.span, "type", &s.name);
                    }
                    to_check.extend(s.fields.iter().map(|(_, t)| (t.clone(), s.span)));
                    self.enums.remove(&s.name);
                    self.structs.insert(s.name.clone(), Rc::new(s));
                }
                Item::Enum(e) => {
                    if !allow_redefine && self.type_exists(&e.name) {
                        return dup(e.span, "type", &e.name);
                    }
                    for (_, types) in &e.variants {
                        to_check.extend(types.iter().map(|t| (t.clone(), e.span)));
                    }
                    self.structs.remove(&e.name);
                    self.enums.insert(e.name.clone(), Rc::new(e));
                }
                Item::Trait(t) => {
                    if !allow_redefine && self.traits.contains_key(&t.name) {
                        return dup(t.span, "trait", &t.name);
                    }
                    for d in &t.defaults {
                        self.method_names.insert(d.name.clone());
                    }
                    self.traits.insert(t.name.clone(), Rc::new(t));
                }
                Item::Impl(imp) => {
                    let type_name = normalize_type_name(&imp.type_name);
                    let names = imp.methods.iter().map(|m| m.name.clone()).collect();
                    impls.push((type_name.clone(), imp.trait_name.clone(), names, imp.span));
                    for m in imp.methods {
                        let key = (type_name.clone(), m.name.clone());
                        if !allow_redefine && self.methods.contains_key(&key) {
                            return dup(m.span, "method", &format!("{}::{}", type_name, m.name));
                        }
                        fn_types(&m, &mut to_check);
                        self.method_names.insert(m.name.clone());
                        self.methods.insert(key, Rc::new(m));
                    }
                }
                Item::Const(c) => {
                    if let Some(t) = &c.ty {
                        to_check.push((t.clone(), c.span));
                    }
                    consts.push(c);
                }
                Item::Mod(name, span) => {
                    return Err(Error::syntax(span, format!("`mod {};` only works when running a file", name)));
                }
            }
        }

        // Checked after registration so declaration order doesn't matter.
        for (type_name, trait_name, names, span) in impls {
            if !self.type_exists(&type_name) && !PRIMITIVE_KEYS.contains(&type_name.as_str()) {
                return Err(Error::syntax(span, format!("cannot find type `{}` to implement", type_name)));
            }
            let Some(trait_name) = trait_name else { continue };
            if let Some(tr) = self.traits.get(&trait_name).cloned() {
                for req in &tr.required {
                    if !names.contains(req) {
                        return Err(Error::syntax(
                            span,
                            format!("`impl {} for {}` is missing the method `{}`", trait_name, type_name, req),
                        ));
                    }
                }
                for d in &tr.defaults {
                    let key = (type_name.clone(), d.name.clone());
                    if !names.contains(&d.name) {
                        self.methods.insert(key, Rc::new(d.clone()));
                    }
                }
            } else if !BUILTIN_TRAITS.contains(&trait_name.as_str()) {
                return Err(Error::syntax(span, format!("cannot find trait `{}`", trait_name)));
            }
            self.trait_impls.insert((type_name, trait_name));
        }
        for (ty, span) in to_check {
            self.check_type(&ty, span)?;
        }

        for c in consts {
            if !allow_redefine && self.consts.contains_key(&c.name) {
                return dup(c.span, "constant", &c.name);
            }
            let v = self.eval(&c.value).map_err(|e| into_error(e, c.span))?;
            if let Some(t) = &c.ty {
                if !self.type_ok(&v, t) {
                    return Err(Error::runtime(
                        c.value.span,
                        format!("mismatched types: constant `{}` is declared as {} but the value is {}", c.name, t, v.type_name()),
                    ));
                }
            }
            self.consts.insert(c.name, v);
        }
        Ok(())
    }

    fn type_exists(&self, name: &str) -> bool {
        self.structs.contains_key(name) || self.enums.contains_key(name)
    }

    fn trait_exists(&self, name: &str) -> bool {
        self.traits.contains_key(name) || BUILTIN_TRAITS.contains(&name)
    }

    fn check_type(&self, ty: &Type, span: Span) -> Result<()> {
        match ty {
            Type::Array(inner) | Type::Option(inner) | Type::Set(inner) => self.check_type(inner, span),
            Type::Result(a, b) | Type::Map(a, b) => {
                self.check_type(a, span)?;
                self.check_type(b, span)
            }
            Type::Tuple(ts) => ts.iter().try_for_each(|t| self.check_type(t, span)),
            Type::Fn(params, ret) => {
                params.iter().try_for_each(|t| self.check_type(t, span))?;
                self.check_type(ret, span)
            }
            Type::Generic(_, bounds) | Type::Trait(bounds) => match bounds.iter().find(|b| !self.trait_exists(b)) {
                Some(b) => Err(Error::syntax(span, format!("cannot find trait `{}`", b))),
                None => Ok(()),
            },
            Type::Named(n) if !self.type_exists(n) => Err(Error::syntax(span, format!("cannot find type `{}`", n))),
            _ => Ok(()),
        }
    }

    /// Does the value's type implement the trait? Built-in traits are assumed.
    fn implements(&self, v: &Value, trait_name: &str) -> bool {
        if !self.traits.contains_key(trait_name) {
            return true;
        }
        self.trait_impls.contains(&(type_key(v), trait_name.to_string()))
    }

    /// Like `Value::matches`, but also checks trait bounds.
    fn type_ok(&self, v: &Value, t: &Type) -> bool {
        match (v, t) {
            (_, Type::Trait(bounds) | Type::Generic(_, bounds)) => bounds.iter().all(|b| self.implements(v, b)),
            (Value::Array(items), Type::Array(inner)) => items.iter().all(|x| self.type_ok(x, inner)),
            (Value::Tuple(items), Type::Tuple(ts)) => {
                items.len() == ts.len() && items.iter().zip(ts).all(|(x, t)| self.type_ok(x, t))
            }
            (Value::Enum(e, variant, data), Type::Option(inner)) if e == "Option" => {
                variant == "None" || data.iter().all(|x| self.type_ok(x, inner))
            }
            _ => v.matches(t),
        }
    }

    fn type_error(&self, v: &Value, t: &Type) -> String {
        match t {
            Type::Trait(bounds) | Type::Generic(_, bounds) => match bounds.iter().find(|b| !self.implements(v, b)) {
                Some(b) => format!("{} does not implement the trait `{}`", v.type_name(), b),
                None => v.type_name(),
            },
            _ => v.type_name(),
        }
    }

    pub fn run_main(&mut self) -> Result<()> {
        let start = Span { line: 1, col: 1, file: 0 };
        let main = self
            .fns
            .get("main")
            .cloned()
            .ok_or_else(|| Error::runtime(start, "no `main` function found; add `fn main() { ... }`"))?;
        if !main.params.is_empty() {
            return Err(Error::runtime(main.span, "`main` must not take any parameters"));
        }
        let span = main.span;
        let out = self.call_fn(main, None, Vec::new(), span).map_err(|c| into_error(c, span))?;
        // `fn main() -> Result<(), E>` reports an `Err` like Rust does.
        if let Value::Enum(en, variant, data) = &out.ret {
            if en == "Result" && variant == "Err" {
                return Err(Error::runtime(span, format!("main returned an error: {}", data[0].render(true))));
            }
        }
        Ok(())
    }

    /// Names of the `#[test]` functions, in source order.
    pub fn test_names(&self) -> Vec<String> {
        let mut tests: Vec<&Rc<FnDecl>> = self.fns.values().filter(|f| f.is_test).collect();
        tests.sort_by_key(|f| (f.span.file, f.span.line));
        tests.iter().map(|f| f.name.clone()).collect()
    }

    pub fn run_test(&mut self, name: &str) -> Result<()> {
        let f = self.fns.get(name).cloned().expect("test exists");
        let span = f.span;
        let out = self.call_fn(f, None, Vec::new(), span).map_err(|c| into_error(c, span))?;
        if let Value::Enum(en, variant, data) = &out.ret {
            if en == "Result" && variant == "Err" {
                return Err(Error::runtime(span, format!("test returned an error: {}", data[0].render(true))));
            }
        }
        Ok(())
    }

    /// Run a REPL statement in the persistent top-level scope.
    pub fn exec_repl(&mut self, stmt: &Stmt) -> Result<()> {
        self.exec(stmt).map_err(|c| into_error(c, stmt.span))
    }

    // ---------- functions ----------

    fn call_fn(&mut self, f: Rc<FnDecl>, self_val: Option<Value>, args: Vec<Value>, span: Span) -> R<CallOut> {
        if args.len() != f.params.len() {
            return Err(rt(
                span,
                format!(
                    "`{}` takes {} argument{} but {} were given",
                    f.name,
                    f.params.len(),
                    if f.params.len() == 1 { "" } else { "s" },
                    args.len()
                ),
            ));
        }
        let mut frame = HashMap::new();
        if let (Some(v), Some(kind)) = (self_val, f.self_kind) {
            let mutable = matches!(kind, SelfKind::RefMut | SelfKind::Value { mutable: true });
            frame.insert("self".to_string(), Binding { value: v, mutable });
        }
        for (p, v) in f.params.iter().zip(args) {
            if !self.type_ok(&v, &p.ty) {
                return Err(rt(
                    span,
                    format!(
                        "mismatched types: parameter `{}` of `{}` expects {}, got {}",
                        p.name,
                        f.name,
                        p.ty,
                        self.type_error(&v, &p.ty)
                    ),
                ));
            }
            frame.insert(p.name.clone(), Binding { value: v, mutable: p.mutable });
        }
        if self.depth >= MAX_CALL_DEPTH || stack_exhausted() {
            return Err(rt(span, format!("stack overflow: too much recursion ({} nested calls)", self.depth)));
        }

        let saved = std::mem::replace(&mut self.scopes, vec![frame]);
        self.depth += 1;
        let result = self.eval_block(&f.body);
        let new_self = match f.self_kind {
            Some(SelfKind::RefMut) => self.scopes[0].remove("self").map(|b| b.value),
            _ => None,
        };
        let mut outs = Vec::new();
        for (i, p) in f.params.iter().enumerate() {
            if p.by_ref_mut {
                if let Some(b) = self.scopes[0].remove(&p.name) {
                    outs.push((i, b.value));
                }
            }
        }
        self.depth -= 1;
        self.scopes = saved;

        let ret = match result {
            // A trailing expression in a function returning `()` is discarded.
            Ok(_) if f.ret == Type::Unit => Value::Unit,
            Ok(v) | Err(Ctrl::Return(v)) => v,
            Err(c @ (Ctrl::Break(..) | Ctrl::Continue(_))) => return Err(Ctrl::Err(into_error(c, f.span))),
            Err(e) => return Err(e),
        };
        if !self.type_ok(&ret, &f.ret) {
            return Err(rt(
                span,
                format!(
                    "mismatched types: `{}` should return {}, but returned {}",
                    f.name,
                    f.ret,
                    self.type_error(&ret, &f.ret)
                ),
            ));
        }
        Ok(CallOut { ret, new_self, outs })
    }

    /// Call a user function from source arguments, handling `&mut` parameters:
    /// the argument must be `&mut place`, and the place gets the final value.
    fn call_with_args(&mut self, f: Rc<FnDecl>, self_val: Option<Value>, arg_exprs: &[Expr], span: Span) -> R<CallOut> {
        for (p, a) in f.params.iter().zip(arg_exprs) {
            if p.by_ref_mut && !matches!(&a.kind, ExprKind::Ref(true, inner) if is_place(inner)) {
                return Err(rt(
                    a.span,
                    format!("`{}` takes `{}` as `&mut`, so pass a mutable variable like `&mut x`", f.name, p.name),
                ));
            }
        }
        let args = self.eval_args(arg_exprs)?;
        let out = self.call_fn(f, self_val, args, span)?;
        for (i, v) in &out.outs {
            if let ExprKind::Ref(true, inner) = &arg_exprs[*i].kind {
                *self.place(inner)? = v.clone();
            }
        }
        Ok(out)
    }

    /// Call a function value (named function or closure).
    fn call_value(&mut self, f: &Value, args: Vec<Value>, span: Span) -> R<Value> {
        match f {
            Value::Func(Callable::Named(decl)) => Ok(self.call_fn(decl.clone(), None, args, span)?.ret),
            Value::Func(Callable::Closure(c)) => self.call_closure(c.clone(), args, span),
            v => Err(rt(span, format!("a value of type {} is not a function", v.type_name()))),
        }
    }

    fn call_closure(&mut self, c: Rc<ClosureVal>, args: Vec<Value>, span: Span) -> R<Value> {
        let def = &c.def;
        if args.len() != def.params.len() {
            return Err(rt(span, format!("closure takes {} argument(s) but {} were given", def.params.len(), args.len())));
        }
        let mut frame: HashMap<String, Binding> =
            c.env.iter().map(|(n, v)| (n.clone(), Binding { value: v.clone(), mutable: false })).collect();
        for ((name, ty), v) in def.params.iter().zip(args) {
            if let Some(t) = ty {
                if !self.type_ok(&v, t) {
                    return Err(rt(span, format!("mismatched types: closure parameter `{}` expects {}, got {}", name, t, v.type_name())));
                }
            }
            frame.insert(name.clone(), Binding { value: v, mutable: false });
        }
        if self.depth >= MAX_CALL_DEPTH || stack_exhausted() {
            return Err(rt(span, format!("stack overflow: too much recursion ({} nested calls)", self.depth)));
        }
        let saved = std::mem::replace(&mut self.scopes, vec![frame]);
        self.depth += 1;
        let result = self.eval(&def.body);
        self.depth -= 1;
        self.scopes = saved;
        let ret = match result {
            Ok(v) | Err(Ctrl::Return(v)) => v,
            Err(c @ (Ctrl::Break(..) | Ctrl::Continue(_))) => return Err(Ctrl::Err(into_error(c, def.body.span))),
            Err(e) => return Err(e),
        };
        if let Some(t) = &def.ret {
            if !self.type_ok(&ret, t) {
                return Err(rt(span, format!("mismatched types: closure should return {}, but returned {}", t, ret.type_name())));
            }
        }
        Ok(ret)
    }

    // ---------- formatting ----------

    /// A user `Display` implementation (`fmt` with `write!`) or `to_string` method.
    fn user_display(&mut self, v: &Value) -> R<Option<String>> {
        if !matches!(v, Value::Struct(..) | Value::Enum(..)) {
            return Ok(None);
        }
        let key = type_key(v);
        if let Some(m) = self.methods.get(&(key.clone(), "fmt".to_string())).cloned() {
            if m.self_kind.is_some() && m.params.len() == 1 {
                self.fmt_buffers.push(String::new());
                let r = self.call_fn(m, Some(v.clone()), vec![Value::Unit], Span::NONE);
                let text = self.fmt_buffers.pop().unwrap_or_default();
                r?;
                return Ok(Some(text));
            }
        }
        if let Some(m) = self.methods.get(&(key, "to_string".to_string())).cloned() {
            if m.self_kind.is_some() && m.params.is_empty() {
                return match self.call_fn(m, Some(v.clone()), vec![], Span::NONE)?.ret {
                    Value::Str(s) => Ok(Some(s)),
                    other => Ok(Some(other.render(false))),
                };
            }
        }
        Ok(None)
    }

    fn format_args(&mut self, args: &[Value], span: Span) -> R<String> {
        struct Env<'a>(&'a mut Interpreter);
        impl FormatEnv for Env<'_> {
            fn lookup(&self, name: &str) -> Option<Value> {
                self.0.lookup(name).cloned()
            }
            fn display(&mut self, v: &Value) -> Result<Option<String>> {
                self.0.user_display(v).map_err(|c| into_error(c, Span::NONE))
            }
        }
        Ok(builtins::format_with(args, span, &mut Env(self))?)
    }

    /// Built-ins that need the interpreter (formatting, assertions, `args`, ...).
    fn call_builtin(&mut self, name: &str, args: Vec<Value>, span: Span) -> R<Value> {
        let message = |this: &mut Self, args: &[Value]| -> R<Option<String>> {
            if args.is_empty() {
                Ok(None)
            } else {
                Ok(Some(this.format_args(args, span)?))
            }
        };
        match name {
            "print" | "println" | "eprint" | "eprintln" => {
                let mut text = self.format_args(&args, span)?;
                if name.ends_with("ln") {
                    text.push('\n');
                }
                if name.starts_with('e') {
                    std::io::stdout().flush().ok();
                    eprint!("{}", text);
                } else {
                    print!("{}", text);
                    if name == "print" {
                        std::io::stdout().flush().ok();
                    }
                }
                Ok(Value::Unit)
            }
            "format" => Ok(Value::Str(self.format_args(&args, span)?)),
            "write" | "writeln" => {
                if self.fmt_buffers.is_empty() {
                    return Err(rt(span, format!("`{}!` can only be used inside a `fmt` method", name)));
                }
                let mut text = self.format_args(args.get(1..).unwrap_or(&[]), span)?;
                if name == "writeln" {
                    text.push('\n');
                }
                self.fmt_buffers.last_mut().unwrap().push_str(&text);
                Ok(value::ok(Value::Unit))
            }
            "panic" => {
                let msg = message(self, &args)?.unwrap_or_else(|| "explicit panic".into());
                Err(rt(span, format!("panicked: {}", msg)))
            }
            "todo" | "unimplemented" | "unreachable" => {
                let what = match name {
                    "todo" => "not yet implemented",
                    "unimplemented" => "not implemented",
                    _ => "entered unreachable code",
                };
                let msg = match message(self, &args)? {
                    Some(m) => format!("{}: {}", what, m),
                    None => what.to_string(),
                };
                Err(rt(span, format!("panicked: {}", msg)))
            }
            "assert" => {
                let cond = match args.first() {
                    Some(Value::Bool(b)) => *b,
                    Some(v) => return Err(rt(span, format!("`assert!` expects a bool, got {}", v.type_name()))),
                    None => return Err(rt(span, "`assert!` needs a condition")),
                };
                if cond {
                    return Ok(Value::Unit);
                }
                let msg = message(self, &args[1..])?.unwrap_or_else(|| "condition was false".into());
                Err(rt(span, format!("assertion failed: {}", msg)))
            }
            "assert_eq" | "assert_ne" => {
                if args.len() < 2 {
                    return Err(rt(span, format!("`{}!` needs two values to compare", name)));
                }
                let equal = args[0].equals(&args[1]);
                if equal == (name == "assert_eq") {
                    return Ok(Value::Unit);
                }
                let op = if name == "assert_eq" { "==" } else { "!=" };
                let extra = match message(self, &args[2..])? {
                    Some(m) => format!(": {}", m),
                    None => String::new(),
                };
                Err(rt(
                    span,
                    format!(
                        "assertion failed: left {} right{}\n  left: {}\n right: {}",
                        op,
                        extra,
                        args[0].render(true),
                        args[1].render(true)
                    ),
                ))
            }
            "dbg" => {
                let v = args.into_iter().next().unwrap_or(Value::Unit);
                std::io::stdout().flush().ok();
                eprintln!("[line {}] {}", span.line, v.render(true));
                Ok(v)
            }
            "args" => Ok(Value::Array(self.program_args.iter().cloned().map(Value::Str).collect())),
            _ => Ok(builtins::call(name, args, span)?),
        }
    }

    // ---------- statements ----------

    fn eval_block(&mut self, b: &Block) -> R<Value> {
        self.scopes.push(HashMap::new());
        let r = self.eval_block_inner(b);
        self.scopes.pop();
        r
    }

    fn eval_block_inner(&mut self, b: &Block) -> R<Value> {
        for s in &b.stmts {
            self.exec(s)?;
        }
        match &b.tail {
            Some(e) => self.eval(e),
            None => Ok(Value::Unit),
        }
    }

    /// Run one loop iteration with `pattern` bound to `value`.
    fn loop_body(&mut self, pattern: &Pattern, value: Value, body: &Block) -> R<Value> {
        self.scopes.push(HashMap::new());
        let r = self.bind_pattern(pattern, value, false, body_span(body)).and_then(|_| self.eval_block_inner(body));
        self.scopes.pop();
        r
    }

    /// Bind an irrefutable pattern (`x`, `(a, b)`, ...) in the current scope.
    fn bind_pattern(&mut self, pattern: &Pattern, v: Value, mutable: bool, span: Span) -> R<()> {
        if let Pattern::Bind(name) = pattern {
            self.scopes.last_mut().unwrap().insert(name.clone(), Binding { value: v, mutable });
            return Ok(());
        }
        let mut binds = Vec::new();
        if !self.match_pattern(pattern, &v, &mut binds)? {
            return Err(rt(span, format!("pattern does not match the value {}", v.render(true))));
        }
        let scope = self.scopes.last_mut().unwrap();
        for (name, value, m) in binds {
            scope.insert(name, Binding { value, mutable: mutable || m });
        }
        Ok(())
    }

    fn exec(&mut self, stmt: &Stmt) -> R<()> {
        let span = stmt.span;
        match &stmt.kind {
            StmtKind::Let { pattern, mutable, ty, value } => {
                let v = self.eval(value)?;
                if let Some(t) = ty {
                    if !self.type_ok(&v, t) {
                        let what = match pattern {
                            Pattern::Bind(name) => format!("`{}`", name),
                            _ => "the pattern".to_string(),
                        };
                        return Err(rt(
                            value.span,
                            format!("mismatched types: {} is declared as {} but the value is {}", what, t, self.type_error(&v, t)),
                        ));
                    }
                }
                self.bind_pattern(pattern, v, *mutable, value.span)?;
            }
            StmtKind::Assign { target, op, value } => {
                let v = self.eval(value)?;
                // `map[key] = value` inserts.
                if let (None, ExprKind::Index(base, idx)) = (op, &target.kind) {
                    let k = self.eval(idx)?;
                    let b = self.place(base)?;
                    if let Value::Map(m) = b {
                        if let Some((_, existing)) = m.iter().next() {
                            if !existing.same_kind(&v) {
                                return Err(rt(
                                    value.span,
                                    format!("mismatched types: cannot put a {} into a map of {}", v.type_name(), existing.type_name()),
                                ));
                            }
                        }
                        m.insert(Key(k), v);
                        return Ok(());
                    }
                    let p = builtins::index_mut(b, &k, target.span)?;
                    return assign_checked(p, v, value.span);
                }
                let place = self.place(target)?;
                let new = match op {
                    Some(op) => binary(*op, place.clone(), v, span)?,
                    None => v,
                };
                assign_checked(place, new, value.span)?;
            }
            StmtKind::Expr(e) => {
                self.eval(e)?;
            }
            StmtKind::Echo(e) => {
                let v = self.eval(e)?;
                if !matches!(v, Value::Unit) {
                    println!("{}", v.render(true));
                }
            }
            StmtKind::While { cond, body, label } => {
                while self.eval_bool(cond, "`while` condition")? {
                    if loop_ctrl(self.eval_block(body), label)? {
                        break;
                    }
                }
            }
            StmtKind::WhileLet { pattern, value, body, label } => loop {
                let v = self.eval(value)?;
                let mut binds = Vec::new();
                if !self.match_pattern(pattern, &v, &mut binds)? {
                    break;
                }
                self.scopes.push(to_scope(binds, false));
                let r = self.eval_block_inner(body);
                self.scopes.pop();
                if loop_ctrl(r, label)? {
                    break;
                }
            },
            StmtKind::For { pattern, iter, body, label } => match iter {
                ForIter::Range(start, end, inclusive) => {
                    let (start, end) = (self.eval(start)?, self.eval(end)?);
                    match (&start, &end) {
                        (Value::Int(s), Value::Int(e)) => {
                            let mut i = *s;
                            while if *inclusive { i <= *e } else { i < *e } {
                                if loop_ctrl(self.loop_body(pattern, Value::Int(i), body), label)? {
                                    break;
                                }
                                match i.checked_add(1) {
                                    Some(n) => i = n,
                                    None => break,
                                }
                            }
                        }
                        _ => {
                            for item in range_values(&start, &end, *inclusive, span)? {
                                if loop_ctrl(self.loop_body(pattern, item, body), label)? {
                                    break;
                                }
                            }
                        }
                    }
                }
                ForIter::Each(e) => {
                    let mutable_base = match &e.kind {
                        ExprKind::MethodCall(base, m, args) if args.is_empty() && matches!(m.as_str(), "iter_mut" | "values_mut") => Some(base),
                        ExprKind::Ref(true, base) => Some(base),
                        _ => None,
                    };
                    if let Some(base) = mutable_base.filter(|b| is_place(b)) {
                        return self.for_each_mut(pattern, base, body, label, e.span);
                    }
                    let items = iter_values(self.eval(e)?, e.span)?;
                    for item in items {
                        if loop_ctrl(self.loop_body(pattern, item, body), label)? {
                            break;
                        }
                    }
                }
            },
        }
        Ok(())
    }

    // ---------- places (assignable locations) ----------

    fn lookup(&self, name: &str) -> Option<&Value> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name))
            .map(|b| &b.value)
            .or_else(|| self.consts.get(name))
    }

    /// Resolve `x`, `a[i]`, `p.x`, `t.0`, `*x`, `map.entry(k).or_insert(v)` to a mutable location.
    fn place(&mut self, e: &Expr) -> R<&mut Value> {
        match &e.kind {
            ExprKind::Var(name) => {
                for scope in self.scopes.iter_mut().rev() {
                    if let Some(b) = scope.get_mut(name) {
                        if !b.mutable {
                            let msg = if name == "self" {
                                "cannot mutate `self` here; take `&mut self` (or `mut self`) in the method signature".to_string()
                            } else {
                                format!("cannot mutate immutable variable `{}` (declare it with `let mut {}`)", name, name)
                            };
                            return Err(rt(e.span, msg));
                        }
                        return Ok(&mut b.value);
                    }
                }
                if self.consts.contains_key(name) {
                    return Err(rt(e.span, format!("cannot assign to constant `{}`", name)));
                }
                Err(rt(e.span, format!("cannot find variable `{}` in this scope", name)))
            }
            ExprKind::Index(base, idx) => {
                let i = self.eval(idx)?;
                let b = self.place(base)?;
                Ok(builtins::index_mut(b, &i, e.span)?)
            }
            ExprKind::Field(base, field) => {
                let b = self.place(base)?;
                let tn = b.type_name();
                match b {
                    Value::Struct(sname, fields) => match fields.iter_mut().find(|(n, _)| n == field) {
                        Some((_, v)) => Ok(v),
                        None => Err(rt(e.span, format!("struct `{}` has no field `{}`", sname, field))),
                    },
                    _ => Err(rt(e.span, format!("type {} has no fields", tn))),
                }
            }
            ExprKind::TupleField(base, i) => {
                let b = self.place(base)?;
                let tn = b.type_name();
                match b {
                    Value::Tuple(items) if *i < items.len() => Ok(&mut items[*i]),
                    _ => Err(rt(e.span, format!("`.{}` is not a field of {}", i, tn))),
                }
            }
            ExprKind::Unary(UnOp::Deref, inner) | ExprKind::Ref(_, inner) => self.place(inner),
            // `map.get_mut(&k).unwrap()` / `v.last_mut().expect(..)`
            ExprKind::MethodCall(inner, unwrap, _) if matches!(unwrap.as_str(), "unwrap" | "expect") => {
                let ExprKind::MethodCall(base, getter, args) = &inner.kind else {
                    return Err(rt(e.span, "cannot mutate a temporary value; store it in a `let mut` variable first"));
                };
                let args = self.eval_args(args)?;
                match self.element_place(base, getter, &args, e.span)? {
                    Some(p) => Ok(p),
                    None => Err(rt(e.span, format!("panicked: called `{}()` on a `None` value", unwrap))),
                }
            }
            ExprKind::MethodCall(entry, name, args) if matches!(name.as_str(), "or_insert" | "or_insert_with" | "or_default") => {
                let ExprKind::MethodCall(map_expr, entry_name, key_args) = &entry.kind else {
                    return Err(rt(e.span, format!("`{}` must follow `.entry(key)`", name)));
                };
                if entry_name != "entry" || key_args.len() != 1 {
                    return Err(rt(e.span, format!("`{}` must follow `.entry(key)`", name)));
                }
                let key = Key(self.eval(&key_args[0])?);
                let default_arg = self.eval_args(args)?;
                let needs_default = match self.place(map_expr)? {
                    Value::Map(m) => !m.contains_key(&key),
                    other => return Err(rt(e.span, format!("`entry` needs a HashMap, got {}", other.type_name()))),
                };
                if needs_default {
                    let default = match name.as_str() {
                        "or_insert" => default_arg.into_iter().next().ok_or_else(|| rt(e.span, "`or_insert` needs a value"))?,
                        "or_insert_with" => {
                            let f = default_arg.into_iter().next().ok_or_else(|| rt(e.span, "`or_insert_with` needs a function"))?;
                            self.call_value(&f, vec![], e.span)?
                        }
                        _ => match self.place(map_expr)? {
                            Value::Map(m) => m.values().next().map(default_like).unwrap_or(Value::Int(0)),
                            _ => Value::Int(0),
                        },
                    };
                    if let Value::Map(m) = self.place(map_expr)? {
                        m.insert(key.clone(), default);
                    }
                }
                match self.place(map_expr)? {
                    Value::Map(m) => Ok(m.get_mut(&key).unwrap()),
                    _ => unreachable!(),
                }
            }
            _ => Err(rt(e.span, "cannot mutate a temporary value; store it in a `let mut` variable first")),
        }
    }

    /// The element `get_mut(k)` / `first_mut()` / `last_mut()` refers to, if any.
    fn element_place(&mut self, base: &Expr, getter: &str, args: &[Value], span: Span) -> R<Option<&mut Value>> {
        let coll = self.place(base)?;
        let tn = coll.type_name();
        let unsupported = || rt(span, format!("cannot use `{}` on {} here", getter, tn));
        match coll {
            Value::Map(m) => match (getter, args) {
                ("get_mut", [k]) => Ok(m.get_mut(&Key(k.clone()))),
                _ => Err(unsupported()),
            },
            Value::Array(items) => match (getter, args) {
                ("get_mut", [Value::Int(i)]) => Ok(if *i >= 0 { items.get_mut(*i as usize) } else { None }),
                ("first_mut", []) => Ok(items.first_mut()),
                ("last_mut", []) => Ok(items.last_mut()),
                _ => Err(unsupported()),
            },
            _ => Err(unsupported()),
        }
    }

    /// `for x in v.iter_mut()` / `for x in &mut v` / `for (k, v) in map.iter_mut()`:
    /// run the body on each element, then store the element back.
    fn for_each_mut(&mut self, pattern: &Pattern, base: &Expr, body: &Block, label: &Option<String>, span: Span) -> R<()> {
        let elem_name = |p: &Pattern| match p {
            Pattern::Bind(n) | Pattern::MutBind(n) => Some(n.clone()),
            _ => None,
        };
        let coll = self.place(base)?.clone();
        match coll {
            Value::Array(items) => {
                let name = elem_name(pattern);
                for (i, item) in items.into_iter().enumerate() {
                    self.scopes.push(HashMap::new());
                    let r = self.bind_pattern(pattern, item, true, span).and_then(|_| self.eval_block_inner(body));
                    let updated = name.as_ref().and_then(|n| self.scopes.last().unwrap().get(n)).map(|b| b.value.clone());
                    self.scopes.pop();
                    if let (Some(v), Value::Array(items)) = (updated, self.place(base)?) {
                        if i < items.len() {
                            items[i] = v;
                        }
                    }
                    if loop_ctrl(r, label)? {
                        break;
                    }
                }
            }
            Value::Map(m) => {
                // Pattern is `(k, v)` (iter_mut) or `v` (values_mut).
                let value_name = match pattern {
                    Pattern::Tuple(ps) if ps.len() == 2 => elem_name(&ps[1]),
                    p => elem_name(p),
                };
                let pairs_pattern = matches!(pattern, Pattern::Tuple(_));
                for (k, v) in m {
                    let item = if pairs_pattern { Value::Tuple(vec![k.0.clone(), v]) } else { v };
                    self.scopes.push(HashMap::new());
                    let r = self.bind_pattern(pattern, item, true, span).and_then(|_| self.eval_block_inner(body));
                    let updated = value_name.as_ref().and_then(|n| self.scopes.last().unwrap().get(n)).map(|b| b.value.clone());
                    self.scopes.pop();
                    if let (Some(nv), Value::Map(m)) = (updated, self.place(base)?) {
                        if let Some(slot) = m.get_mut(&k) {
                            *slot = nv;
                        }
                    }
                    if loop_ctrl(r, label)? {
                        break;
                    }
                }
            }
            other => return Err(rt(span, format!("cannot iterate mutably over a value of type {}", other.type_name()))),
        }
        Ok(())
    }

    // ---------- expressions ----------

    fn eval_bool(&mut self, e: &Expr, what: &str) -> R<bool> {
        match self.eval(e)? {
            Value::Bool(b) => Ok(b),
            v => Err(rt(e.span, format!("mismatched types: {} must be a bool, got {}", what, v.type_name()))),
        }
    }

    fn eval_opt_int(&mut self, e: &Option<Box<Expr>>, what: &str) -> R<Option<i64>> {
        match e {
            None => Ok(None),
            Some(e) => match self.eval(e)? {
                Value::Int(n) => Ok(Some(n)),
                v => Err(rt(e.span, format!("mismatched types: {} must be an int, got {}", what, v.type_name()))),
            },
        }
    }

    fn eval_args(&mut self, args: &[Expr]) -> R<Vec<Value>> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            out.push(self.eval(a)?);
        }
        Ok(out)
    }

    fn eval(&mut self, e: &Expr) -> R<Value> {
        let span = e.span;
        match &e.kind {
            ExprKind::Int(n) => Ok(Value::Int(*n)),
            ExprKind::Float(f) => Ok(Value::Float(*f)),
            ExprKind::Str(s) => Ok(Value::Str(s.clone())),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Unit => Ok(Value::Unit),
            ExprKind::Var(name) => {
                if let Some(v) = self.lookup(name) {
                    return Ok(v.clone());
                }
                if let Some(f) = self.fns.get(name) {
                    return Ok(Value::Func(Callable::Named(f.clone())));
                }
                // A unit struct used as a value: `struct Marker;` ... `Marker`
                match self.structs.get(name) {
                    Some(s) if s.fields.is_empty() => Ok(Value::Struct(name.clone(), Vec::new())),
                    _ => Err(rt(span, format!("cannot find variable `{}` in this scope", name))),
                }
            }
            ExprKind::Unary(op, operand) => {
                let v = self.eval(operand)?;
                match (op, v) {
                    (UnOp::Deref, v) => Ok(v),
                    (UnOp::Neg, Value::Int(n)) => {
                        n.checked_neg().map(Value::Int).ok_or_else(|| rt(span, "attempt to negate with overflow"))
                    }
                    (UnOp::Neg, Value::Float(f)) => Ok(Value::Float(-f)),
                    (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                    (UnOp::Not, Value::Int(n)) => Ok(Value::Int(!n)),
                    (UnOp::Neg, v) => Err(rt(span, format!("cannot apply unary `-` to type {}", v.type_name()))),
                    (UnOp::Not, v) => Err(rt(span, format!("cannot apply unary `!` to type {}", v.type_name()))),
                }
            }
            ExprKind::Ref(_, inner) => self.eval(inner),
            ExprKind::Binary(BinOp::And, l, r) => {
                Ok(Value::Bool(self.eval_bool(l, "operand of `&&`")? && self.eval_bool(r, "operand of `&&`")?))
            }
            ExprKind::Binary(BinOp::Or, l, r) => {
                Ok(Value::Bool(self.eval_bool(l, "operand of `||`")? || self.eval_bool(r, "operand of `||`")?))
            }
            ExprKind::Binary(op, l, r) => {
                let l = self.eval(l)?;
                let r = self.eval(r)?;
                Ok(binary(*op, l, r, span)?)
            }
            ExprKind::Cast(inner, ty) => {
                let v = self.eval(inner)?;
                let name = match ty {
                    Type::Int => "int",
                    Type::Float => "float",
                    Type::Str => "str",
                    other => return Err(rt(span, format!("cannot cast to {} (only int, float and char casts are supported)", other))),
                };
                Ok(builtins::cast(&v, name, span)?)
            }
            ExprKind::Call(name, args) => {
                if let Some(f) = self.lookup(name).cloned() {
                    let args = self.eval_args(args)?;
                    return self.call_value(&f, args, span);
                }
                if let Some(f) = self.fns.get(name).cloned() {
                    return Ok(self.call_with_args(f, None, args, span)?.ret);
                }
                let args = self.eval_args(args)?;
                self.call_builtin(name, args, span)
            }
            ExprKind::CallExpr(callee, args) => {
                let f = self.eval(callee)?;
                let args = self.eval_args(args)?;
                self.call_value(&f, args, span)
            }
            ExprKind::MethodCall(recv, name, args) => self.eval_method_call(e, recv, name, args, span),
            ExprKind::Path(ty, item) => self.eval_path(ty, item, None, span),
            ExprKind::PathCall(ty, item, args) => self.eval_path(ty, item, Some(args), span),
            ExprKind::Index(base, idx) => {
                let b = self.eval(base)?;
                if let ExprKind::Range(lo, hi, inclusive) = &idx.kind {
                    let lo = self.eval_opt_int(lo, "slice start")?;
                    let hi = self.eval_opt_int(hi, "slice end")?;
                    return Ok(builtins::slice(&b, lo, hi, *inclusive, span)?);
                }
                let i = self.eval(idx)?;
                Ok(builtins::index(&b, &i, span)?)
            }
            ExprKind::Range(lo, hi, inclusive) => match (lo, hi) {
                (Some(lo), Some(hi)) => {
                    let (lo, hi) = (self.eval(lo)?, self.eval(hi)?);
                    Ok(Value::Array(range_values(&lo, &hi, *inclusive, span)?))
                }
                _ => Err(rt(span, "an open-ended range like `a..` can only be used for slicing, e.g. `xs[2..]`")),
            },
            ExprKind::Field(base, field) => match self.eval(base)? {
                Value::Struct(sname, fields) => fields
                    .into_iter()
                    .find(|(n, _)| n == field)
                    .map(|(_, v)| v)
                    .ok_or_else(|| rt(span, format!("struct `{}` has no field `{}`", sname, field))),
                v => Err(rt(span, format!("type {} has no fields", v.type_name()))),
            },
            ExprKind::Array(elems) => {
                let items = self.eval_args(elems)?;
                if let Some(first) = items.first() {
                    for (v, ex) in items.iter().zip(elems).skip(1) {
                        if !builtins::array_compatible(first, v) {
                            return Err(rt(
                                ex.span,
                                format!(
                                    "mismatched types: array elements must all have the same type, expected {}, found {}",
                                    first.type_name(),
                                    v.type_name()
                                ),
                            ));
                        }
                    }
                }
                Ok(Value::Array(items))
            }
            ExprKind::ArrayRepeat(value, count) => {
                let v = self.eval(value)?;
                match self.eval(count)? {
                    Value::Int(n) if (0..=100_000_000).contains(&n) => Ok(Value::Array(vec![v; n as usize])),
                    Value::Int(n) => Err(rt(count.span, format!("invalid array length {}", n))),
                    other => Err(rt(count.span, format!("array length must be an int, got {}", other.type_name()))),
                }
            }
            ExprKind::StructLit(name, inits, base) => self.eval_struct_lit(name, inits, base, span),
            ExprKind::Block(b) => self.eval_block(b),
            ExprKind::If(cond, then, else_) => {
                if self.eval_bool(cond, "`if` condition")? {
                    self.eval_block(then)
                } else if let Some(else_) = else_ {
                    self.eval(else_)
                } else {
                    Ok(Value::Unit)
                }
            }
            ExprKind::IfLet(pattern, value, then, else_) => {
                // `if let Some(x) = coll.get_mut(k) { *x += 1 }` writes `x` back.
                if let (Pattern::Variant { variant, fields: Some(fs), .. }, ExprKind::MethodCall(base, getter, args)) =
                    (pattern.as_ref(), &value.kind)
                {
                    if let (true, true, [Pattern::Bind(name) | Pattern::MutBind(name)]) = (
                        variant == "Some",
                        matches!(getter.as_str(), "get_mut" | "first_mut" | "last_mut") && is_place(base),
                        fs.as_slice(),
                    ) {
                        let args = self.eval_args(args)?;
                        let current = self.element_place(base, getter, &args, span)?.map(|p| p.clone());
                        let Some(current) = current else {
                            return match else_ {
                                Some(e) => self.eval(e),
                                None => Ok(Value::Unit),
                            };
                        };
                        self.scopes.push(to_scope(vec![(name.clone(), current, true)], true));
                        let r = self.eval_block(then);
                        let updated = self.scopes.pop().unwrap().remove(name).map(|b| b.value);
                        if let (Some(v), Some(p)) = (updated, self.element_place(base, getter, &args, span)?) {
                            *p = v;
                        }
                        return r;
                    }
                }
                let v = self.eval(value)?;
                let mut binds = Vec::new();
                if self.match_pattern(pattern, &v, &mut binds)? {
                    self.scopes.push(to_scope(binds, false));
                    let r = self.eval_block(then);
                    self.scopes.pop();
                    r
                } else if let Some(else_) = else_ {
                    self.eval(else_)
                } else {
                    Ok(Value::Unit)
                }
            }
            ExprKind::Matches(value, pattern, guard) => {
                let v = self.eval(value)?;
                let mut binds = Vec::new();
                if !self.match_pattern(pattern, &v, &mut binds)? {
                    return Ok(Value::Bool(false));
                }
                let Some(guard) = guard else { return Ok(Value::Bool(true)) };
                self.scopes.push(to_scope(binds, false));
                let r = self.eval_bool(guard, "`matches!` guard");
                self.scopes.pop();
                Ok(Value::Bool(r?))
            }
            ExprKind::Match(scrutinee, arms) => {
                let v = self.eval(scrutinee)?;
                for arm in arms {
                    let mut binds = Vec::new();
                    if !self.match_pattern(&arm.pattern, &v, &mut binds)? {
                        continue;
                    }
                    self.scopes.push(to_scope(binds, false));
                    let result = self.eval_arm(arm);
                    self.scopes.pop();
                    if let Some(v) = result? {
                        return Ok(v);
                    }
                }
                Err(rt(span, format!("no `match` arm matched the value {}", v.render(true))))
            }
            ExprKind::Loop(body, label) => loop {
                match self.eval_block(body) {
                    Ok(_) => {}
                    Err(Ctrl::Continue(l)) if l.is_none() || l == *label => {}
                    Err(Ctrl::Break(l, v)) if l.is_none() || l == *label => return Ok(v),
                    Err(e) => return Err(e),
                }
            },
            ExprKind::Return(v) => {
                let v = match v {
                    Some(v) => self.eval(v)?,
                    None => Value::Unit,
                };
                Err(Ctrl::Return(v))
            }
            ExprKind::Break(label, v) => {
                let v = match v {
                    Some(v) => self.eval(v)?,
                    None => Value::Unit,
                };
                Err(Ctrl::Break(label.clone(), v))
            }
            ExprKind::Continue(label) => Err(Ctrl::Continue(label.clone())),
            ExprKind::Tuple(elems) => Ok(Value::Tuple(self.eval_args(elems)?)),
            ExprKind::TupleField(base, i) => match self.eval(base)? {
                Value::Tuple(mut items) if *i < items.len() => Ok(items.swap_remove(*i)),
                Value::Tuple(items) => Err(rt(span, format!("tuple has {} elements, so `.{}` is out of range", items.len(), i))),
                v => Err(rt(span, format!("`.{}` needs a tuple, got {}", i, v.type_name()))),
            },
            ExprKind::Closure(def) => {
                let env = def
                    .captures
                    .iter()
                    .filter_map(|n| self.lookup(n).map(|v| (n.clone(), v.clone())))
                    .collect();
                Ok(Value::Func(Callable::Closure(Rc::new(ClosureVal { def: def.clone(), env }))))
            }
            ExprKind::Try(inner) => match self.eval(inner)? {
                Value::Enum(en, variant, mut data) if en == "Option" || en == "Result" => {
                    if variant == "Some" || variant == "Ok" {
                        Ok(data.pop().unwrap())
                    } else {
                        // Return the `None` / `Err(e)` from the enclosing function.
                        Err(Ctrl::Return(Value::Enum(en, variant, data)))
                    }
                }
                v => Err(rt(span, format!("`?` can only be used on an Option or Result, got {}", v.type_name()))),
            },
        }
    }

    /// Returns None if the arm's guard rejected the value.
    fn eval_arm(&mut self, arm: &MatchArm) -> R<Option<Value>> {
        if let Some(guard) = &arm.guard {
            if !self.eval_bool(guard, "`match` guard")? {
                return Ok(None);
            }
        }
        Ok(Some(self.eval(&arm.body)?))
    }

    fn match_pattern(&self, p: &Pattern, v: &Value, binds: &mut Vec<(String, Value, bool)>) -> R<bool> {
        Ok(match p {
            Pattern::Wildcard => true,
            Pattern::Bind(name) => {
                binds.push((name.clone(), v.clone(), false));
                true
            }
            Pattern::MutBind(name) => {
                binds.push((name.clone(), v.clone(), true));
                true
            }
            Pattern::Int(n) => matches!(v, Value::Int(x) if x == n),
            Pattern::Str(s) => matches!(v, Value::Str(x) if x == s),
            Pattern::Bool(b) => matches!(v, Value::Bool(x) if x == b),
            Pattern::Range(lo, hi, inclusive) => {
                matches!(v, Value::Int(x) if x >= lo && (if *inclusive { x <= hi } else { x < hi }))
            }
            Pattern::CharRange(lo, hi, inclusive) => match v {
                Value::Str(s) => {
                    let mut it = s.chars();
                    match (it.next(), it.next()) {
                        (Some(c), None) => c >= *lo && if *inclusive { c <= *hi } else { c < *hi },
                        _ => false,
                    }
                }
                _ => false,
            },
            Pattern::Variant { enum_name, variant, fields, span } => {
                let decl = self
                    .enums
                    .get(enum_name)
                    .ok_or_else(|| rt(*span, format!("cannot find enum `{}`", enum_name)))?;
                let (_, types) = decl
                    .variants
                    .iter()
                    .find(|(n, _)| n == variant)
                    .ok_or_else(|| rt(*span, format!("no variant named `{}` in enum `{}`", variant, enum_name)))?;
                let given = fields.as_ref().map_or(0, |f| f.len());
                if given != types.len() {
                    return Err(rt(
                        *span,
                        format!(
                            "pattern `{}::{}` has {} field(s) but the variant has {}",
                            enum_name,
                            variant,
                            given,
                            types.len()
                        ),
                    ));
                }
                match v {
                    Value::Enum(en, vn, data) if en == enum_name && vn == variant => {
                        for (p, d) in fields.iter().flatten().zip(data) {
                            if !self.match_pattern(p, d, binds)? {
                                return Ok(false);
                            }
                        }
                        true
                    }
                    _ => false,
                }
            }
            Pattern::Struct(path, field_pats, span) if path.contains("::") => {
                let (e, vname) = path.rsplit_once("::").unwrap();
                let Some(decl) = self.enums.get(e) else {
                    let inner = Pattern::Struct(vname.to_string(), field_pats.clone(), *span);
                    return self.match_pattern(&inner, v, binds);
                };
                let names = decl
                    .named
                    .iter()
                    .find(|(n, _)| n == vname)
                    .map(|(_, f)| f)
                    .ok_or_else(|| rt(*span, format!("`{}::{}` is not a variant with named fields", e, vname)))?;
                if let Some((f, _)) = field_pats.iter().find(|(f, _)| !names.contains(f)) {
                    return Err(rt(*span, format!("variant `{}::{}` has no field `{}`", e, vname, f)));
                }
                match v {
                    Value::Enum(en, vn, data) if en == e && vn == vname => {
                        let Some(Value::Struct(_, fields)) = data.first() else { return Ok(false) };
                        for (f, p) in field_pats {
                            let fv = &fields.iter().find(|(n, _)| n == f).unwrap().1;
                            if !self.match_pattern(p, fv, binds)? {
                                return Ok(false);
                            }
                        }
                        true
                    }
                    _ => false,
                }
            }
            Pattern::Struct(name, field_pats, span) => {
                let decl = self.structs.get(name).ok_or_else(|| rt(*span, format!("cannot find struct `{}`", name)))?;
                for (f, _) in field_pats {
                    if !decl.fields.iter().any(|(n, _)| n == f) {
                        return Err(rt(*span, format!("struct `{}` has no field `{}`", name, f)));
                    }
                }
                match v {
                    Value::Struct(sn, fields) if sn == name => {
                        for (f, p) in field_pats {
                            let fv = &fields.iter().find(|(n, _)| n == f).unwrap().1;
                            if !self.match_pattern(p, fv, binds)? {
                                return Ok(false);
                            }
                        }
                        true
                    }
                    _ => false,
                }
            }
            Pattern::Tuple(pats) => match v {
                Value::Tuple(items) if items.len() == pats.len() => {
                    for (p, item) in pats.iter().zip(items) {
                        if !self.match_pattern(p, item, binds)? {
                            return Ok(false);
                        }
                    }
                    true
                }
                Value::Unit => pats.is_empty(),
                _ => false,
            },
            Pattern::Or(alts) => {
                for alt in alts {
                    let mark = binds.len();
                    if self.match_pattern(alt, v, binds)? {
                        return Ok(true);
                    }
                    binds.truncate(mark);
                }
                false
            }
        })
    }

    fn eval_method_call(&mut self, whole: &Expr, recv: &Expr, name: &str, arg_exprs: &[Expr], span: Span) -> R<Value> {
        // `map.entry(k).or_insert(v)` as a value: insert if needed, then read.
        if matches!(name, "or_insert" | "or_insert_with" | "or_default") {
            return Ok(self.place(whole)?.clone());
        }
        // Built-in mutating methods (`push`, `pop`, ...) work on the variable in place.
        if builtins::is_mutating_method(name) && !self.method_names.contains(name) {
            let args = self.eval_args(arg_exprs)?;
            let place = self.place(recv)?;
            return Ok(builtins::method_mut(place, name, args, span)?);
        }
        let v = self.eval(recv)?;
        let user_method = self.methods.get(&(type_key(&v), name.to_string())).cloned();
        if let Some(m) = user_method {
            return match m.self_kind {
                None => Err(rt(
                    span,
                    format!(
                        "`{}` is an associated function, not a method; call it as `{}::{}(...)`",
                        name,
                        type_key(&v),
                        name
                    ),
                )),
                Some(SelfKind::RefMut) => {
                    self.place(recv)?; // must be a mutable variable
                    let out = self.call_with_args(m, Some(v), arg_exprs, span)?;
                    if let Some(new_self) = out.new_self {
                        *self.place(recv)? = new_self;
                    }
                    Ok(out.ret)
                }
                Some(_) => Ok(self.call_with_args(m, Some(v), arg_exprs, span)?.ret),
            };
        }
        if name == "to_string" && arg_exprs.is_empty() {
            if let Some(s) = self.user_display(&v)? {
                return Ok(Value::Str(s));
            }
        }
        let args = self.eval_args(arg_exprs)?;
        if let Some(r) = self.higher_order_method(recv, &v, name, &args, span) {
            return r;
        }
        if builtins::is_mutating_method(name) {
            let place = self.place(recv)?;
            return Ok(builtins::method_mut(place, name, args, span)?);
        }
        Ok(builtins::method(v, name, args, span)?)
    }

    /// Methods that take a function argument: `map`, `filter`, `fold`, ...
    /// Returns None if `name` isn't one of them for this receiver.
    fn higher_order_method(&mut self, recv_expr: &Expr, recv: &Value, name: &str, args: &[Value], span: Span) -> Option<R<Value>> {
        let expect_args = |n: usize| -> R<()> {
            if args.len() != n {
                return Err(rt(span, format!("`{}` takes {} argument(s) but {} were given", name, n, args.len())));
            }
            Ok(())
        };
        let truthy = |v: Value| -> R<bool> {
            match v {
                Value::Bool(b) => Ok(b),
                v => Err(rt(span, format!("the function passed to `{}` must return a bool, got {}", name, v.type_name()))),
            }
        };
        let to_ordering = |v: Value| -> R<std::cmp::Ordering> {
            match v {
                Value::Enum(e, variant, _) if e == "Ordering" => Ok(match variant.as_str() {
                    "Less" => std::cmp::Ordering::Less,
                    "Equal" => std::cmp::Ordering::Equal,
                    _ => std::cmp::Ordering::Greater,
                }),
                v => Err(rt(span, format!("the function passed to `{}` must return an Ordering, got {}", name, v.type_name()))),
            }
        };
        let items = match recv {
            Value::Enum(e, variant, _) if e == "Ordering" && name == "then_with" => {
                return Some(match (variant.as_str(), args.first()) {
                    ("Equal", Some(f)) => self.call_value(f, vec![], span),
                    (_, Some(_)) => Ok(recv.clone()),
                    _ => Err(rt(span, "`then_with` needs a function")),
                });
            }
            Value::Array(items) => items,
            Value::Enum(en, variant, data) if en == "Option" || en == "Result" => {
                let is_hof = matches!(
                    name,
                    "map" | "and_then" | "unwrap_or_else" | "map_err" | "ok_or_else" | "filter" | "is_some_and" | "map_or" | "or_else"
                );
                if !is_hof {
                    return None;
                }
                return Some((|| {
                    let has_value = variant == "Some" || variant == "Ok";
                    let f = args.last().cloned().ok_or_else(|| rt(span, format!("`{}` needs a function", name)))?;
                    match (name, has_value) {
                        ("map", true) => {
                            let out = self.call_value(&f, data.clone(), span)?;
                            Ok(Value::Enum(en.clone(), variant.clone(), vec![out]))
                        }
                        ("map_err", false) => {
                            let out = self.call_value(&f, data.clone(), span)?;
                            Ok(Value::Enum(en.clone(), variant.clone(), vec![out]))
                        }
                        ("and_then", true) => self.call_value(&f, data.clone(), span),
                        ("or_else", false) => self.call_value(&f, data.clone(), span),
                        ("unwrap_or_else", true) => Ok(data[0].clone()),
                        ("unwrap_or_else", false) => self.call_value(&f, data.clone(), span),
                        ("ok_or_else", true) => Ok(value::ok(data[0].clone())),
                        ("ok_or_else", false) => Ok(value::err(self.call_value(&f, vec![], span)?)),
                        ("filter", true) => {
                            let keep = truthy(self.call_value(&f, data.clone(), span)?)?;
                            Ok(if keep { recv.clone() } else { value::none() })
                        }
                        ("is_some_and", has) => Ok(Value::Bool(has && truthy(self.call_value(&f, data.clone(), span)?)?)),
                        ("map_or", true) => self.call_value(&f, data.clone(), span),
                        ("map_or", false) => {
                            expect_args(2)?;
                            Ok(args[0].clone())
                        }
                        _ => Ok(recv.clone()),
                    }
                })());
            }
            _ => return None,
        };
        let is_hof = matches!(
            name,
            "map" | "filter" | "for_each" | "fold" | "reduce" | "any" | "all" | "find" | "position" | "flat_map"
                | "filter_map" | "take_while" | "skip_while" | "partition" | "min_by_key" | "max_by_key" | "min_by"
                | "max_by" | "sort_by_key" | "sort_by" | "retain" | "sum_by" | "find_map" | "inspect"
        ) || (name == "count" && args.len() == 1);
        if !is_hof {
            return None;
        }
        let items = items.clone();
        Some((|| -> R<Value> {
            let call1 = |this: &mut Self, v: &Value| this.call_value(&args[0], vec![v.clone()], span);
            match name {
                "map" | "inspect" => {
                    expect_args(1)?;
                    let mut out = Vec::with_capacity(items.len());
                    for v in &items {
                        let r = call1(self, v)?;
                        out.push(if name == "map" { r } else { v.clone() });
                    }
                    Ok(Value::Array(out))
                }
                "filter" => {
                    expect_args(1)?;
                    let mut out = Vec::new();
                    for v in &items {
                        if truthy(call1(self, v)?)? {
                            out.push(v.clone());
                        }
                    }
                    Ok(Value::Array(out))
                }
                "filter_map" | "flat_map" => {
                    expect_args(1)?;
                    let mut out = Vec::new();
                    for v in &items {
                        match call1(self, v)? {
                            Value::Enum(e, variant, data) if e == "Option" => {
                                if variant == "Some" {
                                    out.extend(data)
                                }
                            }
                            Value::Array(inner) => out.extend(inner),
                            other => {
                                return Err(rt(span, format!("the function passed to `{}` must return an Option or array, got {}", name, other.type_name())))
                            }
                        }
                    }
                    Ok(Value::Array(out))
                }
                "find_map" => {
                    expect_args(1)?;
                    for v in &items {
                        let r = call1(self, v)?;
                        if matches!(&r, Value::Enum(e, variant, _) if e == "Option" && variant == "Some") {
                            return Ok(r);
                        }
                    }
                    Ok(value::none())
                }
                "take_while" | "skip_while" => {
                    expect_args(1)?;
                    let mut i = 0;
                    while i < items.len() && truthy(call1(self, &items[i])?)? {
                        i += 1;
                    }
                    Ok(Value::Array(if name == "take_while" { items[..i].to_vec() } else { items[i..].to_vec() }))
                }
                "partition" => {
                    expect_args(1)?;
                    let (mut yes, mut no) = (Vec::new(), Vec::new());
                    for v in &items {
                        if truthy(call1(self, v)?)? {
                            yes.push(v.clone())
                        } else {
                            no.push(v.clone())
                        }
                    }
                    Ok(Value::Tuple(vec![Value::Array(yes), Value::Array(no)]))
                }
                "for_each" => {
                    expect_args(1)?;
                    for v in &items {
                        call1(self, v)?;
                    }
                    Ok(Value::Unit)
                }
                "fold" => {
                    expect_args(2)?;
                    let mut acc = args[0].clone();
                    for v in &items {
                        acc = self.call_value(&args[1], vec![acc, v.clone()], span)?;
                    }
                    Ok(acc)
                }
                "reduce" => {
                    expect_args(1)?;
                    let mut it = items.iter();
                    let Some(first) = it.next() else { return Ok(value::none()) };
                    let mut acc = first.clone();
                    for v in it {
                        acc = self.call_value(&args[0], vec![acc, v.clone()], span)?;
                    }
                    Ok(value::some(acc))
                }
                "sum_by" => {
                    expect_args(1)?;
                    let mut mapped = Vec::with_capacity(items.len());
                    for v in &items {
                        mapped.push(call1(self, v)?);
                    }
                    Ok(builtins::method(Value::Array(mapped), "sum", vec![], span)?)
                }
                "any" | "all" => {
                    expect_args(1)?;
                    let want = name == "any";
                    for v in &items {
                        if truthy(call1(self, v)?)? == want {
                            return Ok(Value::Bool(want));
                        }
                    }
                    Ok(Value::Bool(!want))
                }
                "count" => {
                    let mut n = 0;
                    for v in &items {
                        if truthy(call1(self, v)?)? {
                            n += 1;
                        }
                    }
                    Ok(Value::Int(n))
                }
                "find" | "position" => {
                    expect_args(1)?;
                    for (i, v) in items.iter().enumerate() {
                        if truthy(call1(self, v)?)? {
                            let found = if name == "find" { v.clone() } else { Value::Int(i as i64) };
                            return Ok(value::some(found));
                        }
                    }
                    Ok(value::none())
                }
                "sort_by_key" | "min_by_key" | "max_by_key" => {
                    expect_args(1)?;
                    let mut keys = Vec::with_capacity(items.len());
                    for v in &items {
                        keys.push(call1(self, v)?);
                    }
                    if let Some(k) = keys.windows(2).find(|w| value::compare(&w[0], &w[1]).is_none()) {
                        return Err(rt(span, format!("cannot compare keys of type {} and {}", k[0].type_name(), k[1].type_name())));
                    }
                    let cmp = |a: &usize, b: &usize| value::compare(&keys[*a], &keys[*b]).unwrap();
                    let mut order: Vec<usize> = (0..items.len()).collect();
                    // Stable, so equal keys keep their original order (like Rust).
                    order.sort_by(cmp);
                    match name {
                        "sort_by_key" => {
                            let sorted = Value::Array(order.iter().map(|&i| items[i].clone()).collect());
                            *self.place(recv_expr)? = sorted;
                            Ok(Value::Unit)
                        }
                        // First minimum and last maximum, matching Rust's iterator methods.
                        "min_by_key" => Ok(value::option((0..items.len()).min_by(cmp).map(|i| items[i].clone()))),
                        _ => Ok(value::option((0..items.len()).max_by(cmp).map(|i| items[i].clone()))),
                    }
                }
                "sort_by" | "min_by" | "max_by" => {
                    expect_args(1)?;
                    // Merge sort with the user's comparator (errors propagate).
                    let mut sorted = items.clone();
                    if name == "sort_by" {
                        self.merge_sort(&mut sorted, &args[0], span, &to_ordering)?;
                        *self.place(recv_expr)? = Value::Array(sorted);
                        return Ok(Value::Unit);
                    }
                    let mut best: Option<Value> = None;
                    for v in &items {
                        best = Some(match best {
                            None => v.clone(),
                            Some(b) => {
                                let o = to_ordering(self.call_value(&args[0], vec![v.clone(), b.clone()], span)?)?;
                                let take = if name == "min_by" { o.is_lt() } else { o.is_ge() };
                                if take {
                                    v.clone()
                                } else {
                                    b
                                }
                            }
                        });
                    }
                    Ok(value::option(best))
                }
                "retain" => {
                    expect_args(1)?;
                    let mut kept = Vec::new();
                    for v in &items {
                        if truthy(call1(self, v)?)? {
                            kept.push(v.clone());
                        }
                    }
                    *self.place(recv_expr)? = Value::Array(kept);
                    Ok(Value::Unit)
                }
                _ => unreachable!(),
            }
        })())
    }

    fn merge_sort(
        &mut self,
        items: &mut Vec<Value>,
        f: &Value,
        span: Span,
        to_ordering: &dyn Fn(Value) -> R<std::cmp::Ordering>,
    ) -> R<()> {
        if items.len() <= 1 {
            return Ok(());
        }
        let mut right = items.split_off(items.len() / 2);
        self.merge_sort(items, f, span, to_ordering)?;
        self.merge_sort(&mut right, f, span, to_ordering)?;
        let left = std::mem::take(items);
        let (mut i, mut j) = (0, 0);
        while i < left.len() && j < right.len() {
            let o = to_ordering(self.call_value(f, vec![right[j].clone(), left[i].clone()], span)?)?;
            if o.is_lt() {
                items.push(right[j].clone());
                j += 1;
            } else {
                items.push(left[i].clone());
                i += 1;
            }
        }
        items.extend_from_slice(&left[i..]);
        items.extend_from_slice(&right[j..]);
        Ok(())
    }

    /// `Type::item` (args = None) or `Type::item(args)`.
    fn eval_path(&mut self, raw_ty: &str, item: &str, arg_exprs: Option<&Vec<Expr>>, span: Span) -> R<Value> {
        // Inside a trait default method, `Self` is the implementing type.
        let ty = if raw_ty == "Self" {
            self.lookup("self").map(type_key).unwrap_or_else(|| "Self".into())
        } else {
            normalize_type_name(raw_ty)
        };
        let ty = ty.as_str();

        if let Some(decl) = self.enums.get(ty).cloned() {
            if let Some((_, types)) = decl.variants.iter().find(|(n, _)| n == item) {
                if decl.named.iter().any(|(n, _)| n == item) {
                    return Err(rt(span, format!("`{}::{}` has named fields; write `{}::{} {{ ... }}`", ty, item, ty, item)));
                }
                let args = match arg_exprs {
                    Some(a) => self.eval_args(a)?,
                    None => Vec::new(),
                };
                if args.len() != types.len() {
                    let hint = if types.is_empty() {
                        format!("; write `{}::{}` without parentheses", ty, item)
                    } else {
                        String::new()
                    };
                    return Err(rt(
                        span,
                        format!("variant `{}::{}` takes {} value(s) but {} were given{}", ty, item, types.len(), args.len(), hint),
                    ));
                }
                for (i, (v, t)) in args.iter().zip(types).enumerate() {
                    if !self.type_ok(v, t) {
                        return Err(rt(
                            span,
                            format!("mismatched types: value {} of `{}::{}` expects {}, got {}", i + 1, ty, item, t, v.type_name()),
                        ));
                    }
                }
                return Ok(Value::Enum(ty.to_string(), item.to_string(), args));
            }
        }
        if let Some(m) = self.methods.get(&(ty.to_string(), item.to_string())).cloned() {
            let Some(arg_exprs) = arg_exprs else {
                return Ok(Value::Func(Callable::Named(m)));
            };
            return match m.self_kind {
                None => Ok(self.call_with_args(m, None, arg_exprs, span)?.ret),
                Some(SelfKind::RefMut) => Err(rt(
                    span,
                    format!("`{}::{}` takes `&mut self`; call it as `value.{}(...)`", ty, item, item),
                )),
                Some(_) => {
                    let Some((recv_expr, rest)) = arg_exprs.split_first() else {
                        return Err(rt(span, format!("`{}::{}` needs a `{}` value as its first argument", ty, item, ty)));
                    };
                    let recv = self.eval(recv_expr)?;
                    if type_key(&recv) != ty {
                        return Err(rt(span, format!("mismatched types: expected {} as `self`, got {}", ty, recv.type_name())));
                    }
                    Ok(self.call_with_args(m, Some(recv), rest, span)?.ret)
                }
            };
        }
        if let Some(arg_exprs) = arg_exprs {
            if (ty, item) == ("env", "args") {
                return self.call_builtin("args", vec![], span);
            }
            if (ty, item) == ("mem", "swap") && arg_exprs.len() == 2 {
                let (a, b) = (self.place(&arg_exprs[0])?.clone(), self.place(&arg_exprs[1])?.clone());
                *self.place(&arg_exprs[0])? = b;
                *self.place(&arg_exprs[1])? = a;
                return Ok(Value::Unit);
            }
            let args = self.eval_args(arg_exprs)?;
            if let Some(r) = builtins::path_call(raw_ty, item, &args, span) {
                return Ok(r?);
            }
            if !self.type_exists(ty) {
                // A module path like `utils::helper(...)`: modules share one namespace.
                if let Some(f) = self.fns.get(item).cloned() {
                    return Ok(self.call_fn(f, None, args, span)?.ret);
                }
                if builtins::is_builtin(item) {
                    return self.call_builtin(item, args, span);
                }
            }
        } else {
            if let Some(v) = builtins::path_const(raw_ty, item) {
                return Ok(v);
            }
            if !self.type_exists(ty) {
                if let Some(v) = self.consts.get(item) {
                    return Ok(v.clone());
                }
                if let Some(f) = self.fns.get(item) {
                    return Ok(Value::Func(Callable::Named(f.clone())));
                }
            }
        }
        if self.type_exists(ty) || PRIMITIVE_KEYS.contains(&ty) {
            Err(rt(span, format!("no variant or associated function named `{}` in `{}`", item, ty)))
        } else {
            Err(rt(span, format!("cannot find `{}::{}`", ty, item)))
        }
    }

    fn eval_struct_lit(&mut self, name: &str, inits: &[(String, Expr)], base: &Option<Box<Expr>>, span: Span) -> R<Value> {
        if let Some((e, v)) = name.rsplit_once("::") {
            let Some(decl) = self.enums.get(e).cloned() else {
                return self.eval_struct_lit(v, inits, base, span); // `module::Point { .. }`
            };
            let Some((_, names)) = decl.named.iter().find(|(n, _)| n == v) else {
                return Err(rt(span, format!("`{}::{}` is not a variant with named fields", e, v)));
            };
            let types = &decl.variants.iter().find(|(n, _)| n == v).unwrap().1;
            if let Some((bad, ex)) = inits.iter().find(|(f, _)| !names.contains(f)) {
                return Err(rt(ex.span, format!("variant `{}::{}` has no field named `{}`", e, v, bad)));
            }
            let mut fields = Vec::with_capacity(names.len());
            for (fname, fty) in names.iter().zip(types) {
                let ex = inits
                    .iter()
                    .find(|(n, _)| n == fname)
                    .map(|(_, ex)| ex)
                    .ok_or_else(|| rt(span, format!("missing field `{}` in `{}::{}`", fname, e, v)))?;
                let val = self.eval(ex)?;
                if !self.type_ok(&val, fty) {
                    return Err(rt(ex.span, format!("mismatched types: field `{}` expects {}, got {}", fname, fty, val.type_name())));
                }
                fields.push((fname.clone(), val));
            }
            return Ok(Value::Enum(e.to_string(), v.to_string(), vec![Value::Struct(v.to_string(), fields)]));
        }
        let decl = self.structs.get(name).cloned().ok_or_else(|| rt(span, format!("cannot find struct `{}`", name)))?;
        for (fname, ex) in inits {
            if !decl.fields.iter().any(|(n, _)| n == fname) {
                return Err(rt(ex.span, format!("struct `{}` has no field named `{}`", name, fname)));
            }
            if inits.iter().filter(|(n, _)| n == fname).count() > 1 {
                return Err(rt(ex.span, format!("field `{}` specified more than once", fname)));
            }
        }
        let base_fields = match base {
            Some(b) => match self.eval(b)? {
                Value::Struct(bn, fields) if bn == name => Some(fields),
                other => {
                    return Err(rt(b.span, format!("`..base` must be a `{}`, got {}", name, other.type_name())))
                }
            },
            None => None,
        };
        let mut fields = Vec::with_capacity(decl.fields.len());
        for (fname, fty) in &decl.fields {
            let v = match inits.iter().find(|(n, _)| n == fname) {
                Some((_, ex)) => {
                    let v = self.eval(ex)?;
                    if !self.type_ok(&v, fty) {
                        return Err(rt(
                            ex.span,
                            format!("mismatched types: field `{}.{}` expects {}, got {}", name, fname, fty, v.type_name()),
                        ));
                    }
                    v
                }
                None => match &base_fields {
                    Some(bf) => bf.iter().find(|(n, _)| n == fname).unwrap().1.clone(),
                    None => return Err(rt(span, format!("missing field `{}` in initializer of `{}`", fname, name))),
                },
            };
            fields.push((fname.clone(), v));
        }
        Ok(Value::Struct(name.to_string(), fields))
    }
}

fn assign_checked(place: &mut Value, new: Value, span: Span) -> R<()> {
    if !place.same_kind(&new) && !builtins::array_compatible(place, &new) {
        return Err(rt(
            span,
            format!("mismatched types: cannot assign a {} to a place of type {}", new.type_name(), place.type_name()),
        ));
    }
    *place = new;
    Ok(())
}

/// An "empty" value of the same kind, for `entry(k).or_default()`.
fn default_like(v: &Value) -> Value {
    match v {
        Value::Int(_) => Value::Int(0),
        Value::Float(_) => Value::Float(0.0),
        Value::Str(_) => Value::Str(String::new()),
        Value::Bool(_) => Value::Bool(false),
        Value::Array(_) => Value::Array(Vec::new()),
        Value::Map(_) => Value::Map(Default::default()),
        Value::Set(_) => Value::Set(Default::default()),
        other => other.clone(),
    }
}

/// Values produced by `a..b` for ints or single characters.
fn range_values(lo: &Value, hi: &Value, inclusive: bool, span: Span) -> R<Vec<Value>> {
    const LIMIT: i64 = 50_000_000;
    match (lo, hi) {
        (Value::Int(a), Value::Int(b)) => {
            let end = if inclusive { b.saturating_add(1) } else { *b };
            if end.saturating_sub(*a) > LIMIT {
                return Err(rt(span, format!("range {}..{} is too large to build as an array", a, b)));
            }
            Ok((*a..end.max(*a)).map(Value::Int).collect())
        }
        (Value::Str(a), Value::Str(b)) if a.chars().count() == 1 && b.chars().count() == 1 => {
            let (a, b) = (a.chars().next().unwrap(), b.chars().next().unwrap());
            let r: Vec<Value> = if inclusive {
                (a..=b).map(|c| Value::Str(c.to_string())).collect()
            } else {
                (a..b).map(|c| Value::Str(c.to_string())).collect()
            };
            Ok(r)
        }
        _ => Err(rt(span, format!("a range needs two ints or two characters, got {} and {}", lo.type_name(), hi.type_name()))),
    }
}

/// What a `for` loop iterates over for each kind of value.
fn iter_values(v: Value, span: Span) -> R<Vec<Value>> {
    Ok(match v {
        Value::Array(items) => items,
        Value::Str(s) => s.chars().map(|c| Value::Str(c.to_string())).collect(),
        Value::Map(m) => m.into_iter().map(|(k, v)| Value::Tuple(vec![k.0, v])).collect(),
        Value::Set(s) => s.into_iter().map(|k| k.0).collect(),
        Value::Enum(e, variant, data) if e == "Option" => {
            if variant == "Some" {
                data
            } else {
                Vec::new()
            }
        }
        other => return Err(rt(span, format!("cannot iterate over a value of type {}", other.type_name()))),
    })
}

/// Can this expression be assigned to / passed as `&mut`?
fn is_place(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Var(_) => true,
        ExprKind::Index(base, _) | ExprKind::Field(base, _) | ExprKind::TupleField(base, _) => is_place(base),
        ExprKind::Unary(UnOp::Deref, inner) => is_place(inner),
        _ => false,
    }
}

fn fn_types(f: &FnDecl, out: &mut Vec<(Type, Span)>) {
    out.extend(f.params.iter().map(|p| (p.ty.clone(), f.span)));
    out.push((f.ret.clone(), f.span));
}

fn to_scope(binds: Vec<(String, Value, bool)>, mutable: bool) -> HashMap<String, Binding> {
    binds.into_iter().map(|(n, value, m)| (n, Binding { value, mutable: mutable || m })).collect()
}

fn body_span(b: &Block) -> Span {
    b.stmts
        .first()
        .map(|s| s.span)
        .or(b.tail.as_ref().map(|e| e.span))
        .unwrap_or(Span::NONE)
}

fn binary(op: BinOp, l: Value, r: Value, span: Span) -> Result<Value> {
    use Value::*;
    let overflow = || Error::runtime(span, format!("attempt to compute `{}` with overflow", op));
    match (op, &l, &r) {
        (BinOp::Add, Int(a), Int(b)) => a.checked_add(*b).map(Int).ok_or_else(overflow),
        (BinOp::Sub, Int(a), Int(b)) => a.checked_sub(*b).map(Int).ok_or_else(overflow),
        (BinOp::Mul, Int(a), Int(b)) => a.checked_mul(*b).map(Int).ok_or_else(overflow),
        (BinOp::Div | BinOp::Rem, Int(_), Int(0)) => Err(Error::runtime(span, "attempt to divide by zero")),
        (BinOp::Div, Int(a), Int(b)) => a.checked_div(*b).map(Int).ok_or_else(overflow),
        (BinOp::Rem, Int(a), Int(b)) => a.checked_rem(*b).map(Int).ok_or_else(overflow),
        (BinOp::BitAnd, Int(a), Int(b)) => Ok(Int(a & b)),
        (BinOp::BitOr, Int(a), Int(b)) => Ok(Int(a | b)),
        (BinOp::BitXor, Int(a), Int(b)) => Ok(Int(a ^ b)),
        (BinOp::Shl | BinOp::Shr, Int(a), Int(b)) => {
            if !(0..64).contains(b) {
                return Err(overflow());
            }
            Ok(Int(if op == BinOp::Shl { a << b } else { a >> b }))
        }
        (BinOp::BitAnd, Bool(a), Bool(b)) => Ok(Bool(*a & *b)),
        (BinOp::BitOr, Bool(a), Bool(b)) => Ok(Bool(*a | *b)),
        (BinOp::BitXor, Bool(a), Bool(b)) => Ok(Bool(*a ^ *b)),

        (BinOp::Add, Float(a), Float(b)) => Ok(Float(a + b)),
        (BinOp::Sub, Float(a), Float(b)) => Ok(Float(a - b)),
        (BinOp::Mul, Float(a), Float(b)) => Ok(Float(a * b)),
        (BinOp::Div, Float(a), Float(b)) => Ok(Float(a / b)),
        (BinOp::Rem, Float(a), Float(b)) => Ok(Float(a % b)),

        (BinOp::Add, Str(a), Str(b)) => Ok(Str(format!("{}{}", a, b))),
        (BinOp::Add, Array(a), Array(b)) if l.same_kind(&r) => Ok(Array(a.iter().chain(b).cloned().collect())),
        (BinOp::BitOr, Set(a), Set(b)) => Ok(Set(a.union(b).cloned().collect())),
        (BinOp::BitAnd, Set(a), Set(b)) => Ok(Set(a.intersection(b).cloned().collect())),
        (BinOp::Sub, Set(a), Set(b)) => Ok(Set(a.difference(b).cloned().collect())),

        (BinOp::Eq | BinOp::Ne, _, _) if l.same_kind(&r) || builtins::array_compatible(&l, &r) => {
            let eq = l.equals(&r);
            Ok(Bool(if op == BinOp::Eq { eq } else { !eq }))
        }

        (BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge, _, _) => {
            let ord = if l.same_kind(&r) { value::compare(&l, &r) } else { None };
            let Some(o) = ord else {
                // Floats with NaN compare false; anything else is a type error.
                if matches!((&l, &r), (Float(_), Float(_))) {
                    return Ok(Bool(false));
                }
                return Err(mismatch(op, &l, &r, span));
            };
            Ok(Bool(match op {
                BinOp::Lt => o.is_lt(),
                BinOp::Le => o.is_le(),
                BinOp::Gt => o.is_gt(),
                _ => o.is_ge(),
            }))
        }
        _ => Err(mismatch(op, &l, &r, span)),
    }
}

fn mismatch(op: BinOp, l: &Value, r: &Value, span: Span) -> Error {
    let hint = match (l, r) {
        (Value::Int(_), Value::Float(_)) | (Value::Float(_), Value::Int(_)) => {
            " (Tyfe never mixes int and float implicitly; convert with `x as float` or `x as int`)"
        }
        (Value::Str(_), _) | (_, Value::Str(_)) if op == BinOp::Add => " (convert with `.to_string()` or use format!)",
        _ => "",
    };
    Error::runtime(span, format!("cannot apply `{}` to {} and {}{}", op, l.type_name(), r.type_name(), hint))
}

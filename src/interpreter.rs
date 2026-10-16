//! Tree-walking interpreter for Tyfe.

use crate::ast::*;
use crate::builtins;
use crate::error::{Error, Result};
use crate::value::{self, Callable, ClosureVal, Value};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

const MAX_CALL_DEPTH: usize = 20_000;

struct Binding {
    value: Value,
    mutable: bool,
}

/// Anything that interrupts normal evaluation: an error, or `return` / `break` /
/// `continue` unwinding to the function or loop that handles it.
enum Ctrl {
    Err(Error),
    Return(Value),
    Break(Value),
    Continue,
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
        Ctrl::Break(_) | Ctrl::Continue => Error::runtime(span, "`break`/`continue` outside of a loop"),
    }
}

/// Decide what a loop does after one iteration: Ok(true) means stop.
fn loop_ctrl(r: R<Value>) -> R<bool> {
    match r {
        Ok(_) | Err(Ctrl::Continue) => Ok(false),
        Err(Ctrl::Break(_)) => Ok(true),
        Err(e) => Err(e),
    }
}

pub struct Interpreter {
    fns: HashMap<String, Rc<FnDecl>>,
    structs: HashMap<String, Rc<StructDecl>>,
    enums: HashMap<String, Rc<EnumDecl>>,
    /// Methods and associated functions, keyed by (type name, function name).
    methods: HashMap<(String, String), Rc<FnDecl>>,
    method_names: HashSet<String>,
    consts: HashMap<String, Value>,
    scopes: Vec<HashMap<String, Binding>>,
    depth: usize,
}

impl Interpreter {
    pub fn new() -> Self {
        let builtin = |name: &str, variants: &[(&str, usize)]| {
            let decl = EnumDecl {
                name: name.to_string(),
                variants: variants.iter().map(|(v, n)| (v.to_string(), vec![Type::Any; *n])).collect(),
                span: Span { line: 0, col: 0 },
            };
            (name.to_string(), Rc::new(decl))
        };
        Interpreter {
            fns: HashMap::new(),
            structs: HashMap::new(),
            enums: HashMap::from([
                builtin("Option", &[("Some", 1), ("None", 0)]),
                builtin("Result", &[("Ok", 1), ("Err", 1)]),
            ]),
            methods: HashMap::new(),
            method_names: HashSet::new(),
            consts: HashMap::new(),
            scopes: vec![HashMap::new()],
            depth: 0,
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
        let mut impls: Vec<(String, Span)> = Vec::new();

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
                    if !allow_redefine && (self.structs.contains_key(&s.name) || self.enums.contains_key(&s.name)) {
                        return dup(s.span, "type", &s.name);
                    }
                    to_check.extend(s.fields.iter().map(|(_, t)| (t.clone(), s.span)));
                    self.enums.remove(&s.name);
                    self.structs.insert(s.name.clone(), Rc::new(s));
                }
                Item::Enum(e) => {
                    if !allow_redefine && (self.structs.contains_key(&e.name) || self.enums.contains_key(&e.name)) {
                        return dup(e.span, "type", &e.name);
                    }
                    for (_, types) in &e.variants {
                        to_check.extend(types.iter().map(|t| (t.clone(), e.span)));
                    }
                    self.structs.remove(&e.name);
                    self.enums.insert(e.name.clone(), Rc::new(e));
                }
                Item::Impl(imp) => {
                    impls.push((imp.type_name.clone(), imp.span));
                    for m in imp.methods {
                        let key = (imp.type_name.clone(), m.name.clone());
                        if !allow_redefine && self.methods.contains_key(&key) {
                            return dup(m.span, "method", &format!("{}::{}", imp.type_name, m.name));
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
            }
        }

        // Every named type must exist (checked after registration so order doesn't matter).
        for (name, span) in impls {
            if !self.type_exists(&name) {
                return Err(Error::syntax(span, format!("cannot find type `{}` to implement", name)));
            }
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
                if !v.matches(t) {
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

    fn check_type(&self, ty: &Type, span: Span) -> Result<()> {
        match ty {
            Type::Array(inner) | Type::Option(inner) => self.check_type(inner, span),
            Type::Result(t, e) => {
                self.check_type(t, span)?;
                self.check_type(e, span)
            }
            Type::Tuple(ts) => ts.iter().try_for_each(|t| self.check_type(t, span)),
            Type::Fn(params, ret) => {
                params.iter().try_for_each(|t| self.check_type(t, span))?;
                self.check_type(ret, span)
            }
            Type::Named(n) if !self.type_exists(n) => Err(Error::syntax(span, format!("cannot find type `{}`", n))),
            _ => Ok(()),
        }
    }

    pub fn run_main(&mut self) -> Result<()> {
        let start = Span { line: 1, col: 1 };
        let main = self
            .fns
            .get("main")
            .cloned()
            .ok_or_else(|| Error::runtime(start, "no `main` function found; add `fn main() { ... }`"))?;
        if !main.params.is_empty() {
            return Err(Error::runtime(main.span, "`main` must not take any parameters"));
        }
        let span = main.span;
        let (ret, _) = self.call_fn(main, None, Vec::new(), span).map_err(|c| into_error(c, span))?;
        // `fn main() -> Result<(), E>` reports an `Err` like Rust does.
        if let Value::Enum(en, variant, data) = &ret {
            if en == "Result" && variant == "Err" {
                return Err(Error::runtime(span, format!("main returned an error: {}", data[0].render(true))));
            }
        }
        Ok(())
    }

    /// Run a REPL statement in the persistent top-level scope.
    pub fn exec_repl(&mut self, stmt: &Stmt) -> Result<()> {
        self.exec(stmt).map_err(|c| into_error(c, stmt.span))
    }

    // ---------- functions ----------

    /// Call a function or method. For `&mut self` methods, also returns the
    /// updated `self` so the caller can write it back.
    fn call_fn(&mut self, f: Rc<FnDecl>, self_val: Option<Value>, args: Vec<Value>, span: Span) -> R<(Value, Option<Value>)> {
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
            if !v.matches(&p.ty) {
                return Err(rt(
                    span,
                    format!("mismatched types: parameter `{}` of `{}` expects {}, got {}", p.name, f.name, p.ty, v.type_name()),
                ));
            }
            frame.insert(p.name.clone(), Binding { value: v, mutable: p.mutable });
        }
        if self.depth >= MAX_CALL_DEPTH {
            return Err(rt(span, format!("stack overflow: more than {} nested calls", MAX_CALL_DEPTH)));
        }

        let saved = std::mem::replace(&mut self.scopes, vec![frame]);
        self.depth += 1;
        let result = self.eval_block(&f.body);
        let new_self = match f.self_kind {
            Some(SelfKind::RefMut) => self.scopes[0].remove("self").map(|b| b.value),
            _ => None,
        };
        self.depth -= 1;
        self.scopes = saved;

        let ret = match result {
            // A trailing expression in a function returning `()` is discarded.
            Ok(_) if f.ret == Type::Unit => Value::Unit,
            Ok(v) | Err(Ctrl::Return(v)) => v,
            Err(Ctrl::Break(_) | Ctrl::Continue) => {
                return Err(rt(f.span, "`break`/`continue` outside of a loop"))
            }
            Err(e) => return Err(e),
        };
        if !ret.matches(&f.ret) {
            return Err(rt(
                span,
                format!("mismatched types: `{}` should return {}, but returned {}", f.name, f.ret, ret.type_name()),
            ));
        }
        Ok((ret, new_self))
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
        for (name, value) in binds {
            scope.insert(name, Binding { value, mutable });
        }
        Ok(())
    }

    fn exec(&mut self, stmt: &Stmt) -> R<()> {
        let span = stmt.span;
        match &stmt.kind {
            StmtKind::Let { pattern, mutable, ty, value } => {
                let v = self.eval(value)?;
                if let Some(t) = ty {
                    if !v.matches(t) {
                        let what = match pattern {
                            Pattern::Bind(name) => format!("`{}`", name),
                            _ => "the pattern".to_string(),
                        };
                        return Err(rt(
                            value.span,
                            format!("mismatched types: {} is declared as {} but the value is {}", what, t, v.type_name()),
                        ));
                    }
                }
                self.bind_pattern(pattern, v, *mutable, value.span)?;
            }
            StmtKind::Assign { target, op, value } => {
                let v = self.eval(value)?;
                let place = self.place(target)?;
                let new = match op {
                    Some(op) => binary(*op, place.clone(), v, span)?,
                    None => v,
                };
                if !place.same_kind(&new) {
                    return Err(rt(
                        value.span,
                        format!("mismatched types: cannot assign a {} to a place of type {}", new.type_name(), place.type_name()),
                    ));
                }
                *place = new;
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
            StmtKind::While { cond, body } => {
                while self.eval_bool(cond, "`while` condition")? {
                    if loop_ctrl(self.eval_block(body))? {
                        break;
                    }
                }
            }
            StmtKind::WhileLet { pattern, value, body } => loop {
                let v = self.eval(value)?;
                let mut binds = Vec::new();
                if !self.match_pattern(pattern, &v, &mut binds)? {
                    break;
                }
                self.scopes.push(to_scope(binds, false));
                let r = self.eval_block_inner(body);
                self.scopes.pop();
                if loop_ctrl(r)? {
                    break;
                }
            },
            StmtKind::For { pattern, iter, body } => match iter {
                ForIter::Range(start, end, inclusive) => {
                    let start = self.eval_int(start, "range start")?;
                    let end = self.eval_int(end, "range end")?;
                    let mut i = start;
                    while if *inclusive { i <= end } else { i < end } {
                        if loop_ctrl(self.loop_body(pattern, Value::Int(i), body))? {
                            break;
                        }
                        match i.checked_add(1) {
                            Some(n) => i = n,
                            None => break,
                        }
                    }
                }
                ForIter::Each(e) => {
                    let items = match self.eval(e)? {
                        Value::Array(items) => items,
                        Value::Str(s) => s.chars().map(|c| Value::Str(c.to_string())).collect(),
                        other => return Err(rt(e.span, format!("cannot iterate over a value of type {}", other.type_name()))),
                    };
                    for item in items {
                        if loop_ctrl(self.loop_body(pattern, item, body))? {
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

    /// Resolve `x`, `a[i]`, `p.x`, ... to a mutable location.
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
            _ => Err(rt(e.span, "cannot mutate a temporary value; store it in a `let mut` variable first")),
        }
    }

    // ---------- expressions ----------

    fn eval_bool(&mut self, e: &Expr, what: &str) -> R<bool> {
        match self.eval(e)? {
            Value::Bool(b) => Ok(b),
            v => Err(rt(e.span, format!("mismatched types: {} must be a bool, got {}", what, v.type_name()))),
        }
    }

    fn eval_int(&mut self, e: &Expr, what: &str) -> R<i64> {
        match self.eval(e)? {
            Value::Int(n) => Ok(n),
            v => Err(rt(e.span, format!("mismatched types: {} must be an int, got {}", what, v.type_name()))),
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
            ExprKind::Var(name) => {
                if let Some(v) = self.lookup(name) {
                    return Ok(v.clone());
                }
                match self.fns.get(name) {
                    Some(f) => Ok(Value::Func(Callable::Named(f.clone()))),
                    None => Err(rt(span, format!("cannot find variable `{}` in this scope", name))),
                }
            }
            ExprKind::Unary(op, operand) => {
                let v = self.eval(operand)?;
                match (op, v) {
                    (UnOp::Neg, Value::Int(n)) => {
                        n.checked_neg().map(Value::Int).ok_or_else(|| rt(span, "attempt to negate with overflow"))
                    }
                    (UnOp::Neg, Value::Float(f)) => Ok(Value::Float(-f)),
                    (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                    (UnOp::Neg, v) => Err(rt(span, format!("cannot apply unary `-` to type {}", v.type_name()))),
                    (UnOp::Not, v) => Err(rt(span, format!("cannot apply unary `!` to type {}", v.type_name()))),
                }
            }
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
            ExprKind::Call(name, args) => {
                let args = self.eval_args(args)?;
                if let Some(f) = self.lookup(name).cloned() {
                    return self.call_value(&f, args, span);
                }
                match self.fns.get(name).cloned() {
                    Some(f) => Ok(self.call_fn(f, None, args, span)?.0),
                    None => Ok(builtins::call(name, args, span)?),
                }
            }
            ExprKind::CallExpr(callee, args) => {
                let f = self.eval(callee)?;
                let args = self.eval_args(args)?;
                self.call_value(&f, args, span)
            }
            ExprKind::Unit => Ok(Value::Unit),
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
            ExprKind::IfLet(pattern, value, then, else_) => {
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
            ExprKind::MethodCall(recv, name, args) => self.eval_method_call(recv, name, args, span),
            ExprKind::Path(ty, item) => self.eval_path(ty, item, None, span),
            ExprKind::PathCall(ty, item, args) => {
                let args = self.eval_args(args)?;
                self.eval_path(ty, item, Some(args), span)
            }
            ExprKind::Index(base, idx) => {
                let b = self.eval(base)?;
                let i = self.eval(idx)?;
                Ok(builtins::index(&b, &i, span)?)
            }
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
                        if !first.same_kind(v) {
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
            ExprKind::StructLit(name, inits) => self.eval_struct_lit(name, inits, span),
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
            ExprKind::Loop(body) => loop {
                match self.eval_block(body) {
                    Ok(_) | Err(Ctrl::Continue) => {}
                    Err(Ctrl::Break(v)) => return Ok(v),
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
            ExprKind::Break(v) => {
                let v = match v {
                    Some(v) => self.eval(v)?,
                    None => Value::Unit,
                };
                Err(Ctrl::Break(v))
            }
            ExprKind::Continue => Err(Ctrl::Continue),
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

    fn match_pattern(&self, p: &Pattern, v: &Value, binds: &mut Vec<(String, Value)>) -> R<bool> {
        Ok(match p {
            Pattern::Wildcard => true,
            Pattern::Bind(name) => {
                binds.push((name.clone(), v.clone()));
                true
            }
            Pattern::Int(n) => matches!(v, Value::Int(x) if x == n),
            Pattern::Str(s) => matches!(v, Value::Str(x) if x == s),
            Pattern::Bool(b) => matches!(v, Value::Bool(x) if x == b),
            Pattern::Range(lo, hi, inclusive) => {
                matches!(v, Value::Int(x) if x >= lo && (if *inclusive { x <= hi } else { x < hi }))
            }
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

    fn eval_method_call(&mut self, recv: &Expr, name: &str, args: &[Expr], span: Span) -> R<Value> {
        let args = self.eval_args(args)?;
        // Built-in mutating methods (`push`, `pop`, ...) work on the variable in place.
        if builtins::is_mutating_method(name) && !self.method_names.contains(name) {
            let place = self.place(recv)?;
            return Ok(builtins::method_mut(place, name, args, span)?);
        }
        let v = self.eval(recv)?;
        if let Some(r) = self.higher_order_method(&v, name, &args, span) {
            return r;
        }
        let user_method = match &v {
            Value::Struct(t, _) | Value::Enum(t, _, _) => self.methods.get(&(t.clone(), name.to_string())).cloned(),
            _ => None,
        };
        match user_method {
            Some(m) => match m.self_kind {
                None => Err(rt(
                    span,
                    format!(
                        "`{}` is an associated function, not a method; call it as `{}::{}(...)`",
                        name,
                        v.type_name(),
                        name
                    ),
                )),
                Some(SelfKind::RefMut) => {
                    self.place(recv)?; // must be a mutable variable
                    let (ret, new_self) = self.call_fn(m, Some(v), args, span)?;
                    if let Some(new_self) = new_self {
                        *self.place(recv)? = new_self;
                    }
                    Ok(ret)
                }
                Some(_) => Ok(self.call_fn(m, Some(v), args, span)?.0),
            },
            None if builtins::is_mutating_method(name) => {
                let place = self.place(recv)?;
                Ok(builtins::method_mut(place, name, args, span)?)
            }
            None => Ok(builtins::method(v, name, args, span)?),
        }
    }

    /// Call a function value (named function or closure).
    fn call_value(&mut self, f: &Value, args: Vec<Value>, span: Span) -> R<Value> {
        match f {
            Value::Func(Callable::Named(decl)) => Ok(self.call_fn(decl.clone(), None, args, span)?.0),
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
                if !v.matches(t) {
                    return Err(rt(span, format!("mismatched types: closure parameter `{}` expects {}, got {}", name, t, v.type_name())));
                }
            }
            frame.insert(name.clone(), Binding { value: v, mutable: false });
        }
        if self.depth >= MAX_CALL_DEPTH {
            return Err(rt(span, format!("stack overflow: more than {} nested calls", MAX_CALL_DEPTH)));
        }
        let saved = std::mem::replace(&mut self.scopes, vec![frame]);
        self.depth += 1;
        let result = self.eval(&def.body);
        self.depth -= 1;
        self.scopes = saved;
        let ret = match result {
            Ok(v) | Err(Ctrl::Return(v)) => v,
            Err(Ctrl::Break(_) | Ctrl::Continue) => return Err(rt(def.body.span, "`break`/`continue` outside of a loop")),
            Err(e) => return Err(e),
        };
        if let Some(t) = &def.ret {
            if !ret.matches(t) {
                return Err(rt(span, format!("mismatched types: closure should return {}, but returned {}", t, ret.type_name())));
            }
        }
        Ok(ret)
    }

    /// Methods that take a function argument: `map`, `filter`, `fold`, ...
    /// Returns None if `name` isn't one of them for this receiver.
    fn higher_order_method(&mut self, recv: &Value, name: &str, args: &[Value], span: Span) -> Option<R<Value>> {
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
        let items = match recv {
            Value::Array(items) => items,
            Value::Enum(en, variant, data) if en == "Option" || en == "Result" => {
                if name != "map" && name != "and_then" && name != "unwrap_or_else" {
                    return None;
                }
                let f = args.first().cloned();
                return Some((|| {
                    expect_args(1)?;
                    let f = f.unwrap();
                    let has_value = variant == "Some" || variant == "Ok";
                    match (name, has_value) {
                        ("map", true) => {
                            let out = self.call_value(&f, data.clone(), span)?;
                            Ok(Value::Enum(en.clone(), variant.clone(), vec![out]))
                        }
                        ("and_then", true) => self.call_value(&f, data.clone(), span),
                        ("unwrap_or_else", true) => Ok(data[0].clone()),
                        ("unwrap_or_else", false) => self.call_value(&f, data.clone(), span),
                        _ => Ok(recv.clone()),
                    }
                })());
            }
            _ => return None,
        };
        let is_hof = matches!(
            name,
            "map" | "filter" | "for_each" | "fold" | "any" | "all" | "count" | "find" | "position" | "sort_by_key"
                | "min_by_key" | "max_by_key"
        );
        if !is_hof {
            return None;
        }
        Some((|| -> R<Value> {
            match name {
                "map" => {
                    expect_args(1)?;
                    let mut out = Vec::with_capacity(items.len());
                    for v in items {
                        out.push(self.call_value(&args[0], vec![v.clone()], span)?);
                    }
                    Ok(Value::Array(out))
                }
                "filter" => {
                    expect_args(1)?;
                    let mut out = Vec::new();
                    for v in items {
                        if truthy(self.call_value(&args[0], vec![v.clone()], span)?)? {
                            out.push(v.clone());
                        }
                    }
                    Ok(Value::Array(out))
                }
                "for_each" => {
                    expect_args(1)?;
                    for v in items {
                        self.call_value(&args[0], vec![v.clone()], span)?;
                    }
                    Ok(Value::Unit)
                }
                "fold" => {
                    expect_args(2)?;
                    let mut acc = args[0].clone();
                    for v in items {
                        acc = self.call_value(&args[1], vec![acc, v.clone()], span)?;
                    }
                    Ok(acc)
                }
                "any" | "all" => {
                    expect_args(1)?;
                    let want = name == "any";
                    for v in items {
                        if truthy(self.call_value(&args[0], vec![v.clone()], span)?)? == want {
                            return Ok(Value::Bool(want));
                        }
                    }
                    Ok(Value::Bool(!want))
                }
                "count" => {
                    expect_args(1)?;
                    let mut n = 0;
                    for v in items {
                        if truthy(self.call_value(&args[0], vec![v.clone()], span)?)? {
                            n += 1;
                        }
                    }
                    Ok(Value::Int(n))
                }
                "find" | "position" => {
                    expect_args(1)?;
                    for (i, v) in items.iter().enumerate() {
                        if truthy(self.call_value(&args[0], vec![v.clone()], span)?)? {
                            let found = if name == "find" { v.clone() } else { Value::Int(i as i64) };
                            return Ok(value::some(found));
                        }
                    }
                    Ok(value::none())
                }
                "sort_by_key" | "min_by_key" | "max_by_key" => {
                    expect_args(1)?;
                    let mut keys = Vec::with_capacity(items.len());
                    for v in items {
                        keys.push(self.call_value(&args[0], vec![v.clone()], span)?);
                    }
                    if let Some(k) = keys.windows(2).find(|w| value::compare(&w[0], &w[1]).is_none()) {
                        return Err(rt(span, format!("cannot compare keys of type {} and {}", k[0].type_name(), k[1].type_name())));
                    }
                    let cmp = |a: &usize, b: &usize| value::compare(&keys[*a], &keys[*b]).unwrap();
                    let mut order: Vec<usize> = (0..items.len()).collect();
                    // Stable, so equal keys keep their original order (like Rust).
                    order.sort_by(cmp);
                    Ok(match name {
                        "sort_by_key" => Value::Array(order.iter().map(|&i| items[i].clone()).collect()),
                        // First minimum and last maximum, matching Rust's iterator methods.
                        "min_by_key" => value::option((0..items.len()).min_by(cmp).map(|i| items[i].clone())),
                        _ => value::option((0..items.len()).max_by(cmp).map(|i| items[i].clone())),
                    })
                }
                _ => unreachable!(),
            }
        })())
    }

    /// `Type::item` (args = None) or `Type::item(args)`.
    fn eval_path(&mut self, ty: &str, item: &str, args: Option<Vec<Value>>, span: Span) -> R<Value> {
        if let Some(decl) = self.enums.get(ty).cloned() {
            if let Some((_, types)) = decl.variants.iter().find(|(n, _)| n == item) {
                let args = args.unwrap_or_default();
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
                    if !v.matches(t) {
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
            let Some(mut args) = args else {
                return Err(rt(span, format!("`{}::{}` is a function; call it with `{}::{}(...)`", ty, item, ty, item)));
            };
            return match m.self_kind {
                None => Ok(self.call_fn(m, None, args, span)?.0),
                Some(SelfKind::RefMut) => Err(rt(
                    span,
                    format!("`{}::{}` takes `&mut self`; call it as `value.{}(...)`", ty, item, item),
                )),
                Some(_) => {
                    if args.is_empty() {
                        return Err(rt(span, format!("`{}::{}` needs a `{}` value as its first argument", ty, item, ty)));
                    }
                    let recv = args.remove(0);
                    if !recv.matches(&Type::Named(ty.to_string())) {
                        return Err(rt(span, format!("mismatched types: expected {} as `self`, got {}", ty, recv.type_name())));
                    }
                    Ok(self.call_fn(m, Some(recv), args, span)?.0)
                }
            };
        }
        if self.type_exists(ty) {
            Err(rt(span, format!("no variant or associated function named `{}` in `{}`", item, ty)))
        } else {
            Err(rt(span, format!("cannot find type `{}`", ty)))
        }
    }

    fn eval_struct_lit(&mut self, name: &str, inits: &[(String, Expr)], span: Span) -> R<Value> {
        let decl = self.structs.get(name).cloned().ok_or_else(|| rt(span, format!("cannot find struct `{}`", name)))?;
        for (fname, ex) in inits {
            if !decl.fields.iter().any(|(n, _)| n == fname) {
                return Err(rt(ex.span, format!("struct `{}` has no field named `{}`", name, fname)));
            }
            if inits.iter().filter(|(n, _)| n == fname).count() > 1 {
                return Err(rt(ex.span, format!("field `{}` specified more than once", fname)));
            }
        }
        let mut fields = Vec::with_capacity(decl.fields.len());
        for (fname, fty) in &decl.fields {
            let ex = inits
                .iter()
                .find(|(n, _)| n == fname)
                .map(|(_, ex)| ex)
                .ok_or_else(|| rt(span, format!("missing field `{}` in initializer of `{}`", fname, name)))?;
            let v = self.eval(ex)?;
            if !v.matches(fty) {
                return Err(rt(
                    ex.span,
                    format!("mismatched types: field `{}.{}` expects {}, got {}", name, fname, fty, v.type_name()),
                ));
            }
            fields.push((fname.clone(), v));
        }
        Ok(Value::Struct(name.to_string(), fields))
    }
}

fn fn_types(f: &FnDecl, out: &mut Vec<(Type, Span)>) {
    out.extend(f.params.iter().map(|p| (p.ty.clone(), f.span)));
    out.push((f.ret.clone(), f.span));
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

        (BinOp::Add, Float(a), Float(b)) => Ok(Float(a + b)),
        (BinOp::Sub, Float(a), Float(b)) => Ok(Float(a - b)),
        (BinOp::Mul, Float(a), Float(b)) => Ok(Float(a * b)),
        (BinOp::Div, Float(a), Float(b)) => Ok(Float(a / b)),
        (BinOp::Rem, Float(a), Float(b)) => Ok(Float(a % b)),

        (BinOp::Add, Str(a), Str(b)) => Ok(Str(format!("{}{}", a, b))),
        (BinOp::Add, Array(a), Array(b)) if l.same_kind(&r) => Ok(Array(a.iter().chain(b).cloned().collect())),

        (BinOp::Eq | BinOp::Ne, _, _) if l.same_kind(&r) => {
            let eq = l.equals(&r);
            Ok(Bool(if op == BinOp::Eq { eq } else { !eq }))
        }

        (BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge, _, _) => {
            let ord = match (&l, &r) {
                (Int(a), Int(b)) => a.partial_cmp(b),
                (Float(a), Float(b)) => a.partial_cmp(b),
                (Str(a), Str(b)) => a.partial_cmp(b),
                _ => return Err(mismatch(op, &l, &r, span)),
            };
            let res = match ord {
                None => false, // NaN
                Some(o) => match op {
                    BinOp::Lt => o.is_lt(),
                    BinOp::Le => o.is_le(),
                    BinOp::Gt => o.is_gt(),
                    _ => o.is_ge(),
                },
            };
            Ok(Bool(res))
        }
        _ => Err(mismatch(op, &l, &r, span)),
    }
}

fn mismatch(op: BinOp, l: &Value, r: &Value, span: Span) -> Error {
    let hint = match (l, r) {
        (Value::Int(_), Value::Float(_)) | (Value::Float(_), Value::Int(_)) => {
            " (Tyfe never mixes int and float implicitly; convert with float(x) or int(x))"
        }
        (Value::Str(_), _) | (_, Value::Str(_)) if op == BinOp::Add => " (convert with str(x) or use format(...))",
        _ => "",
    };
    Error::runtime(span, format!("cannot apply `{}` to {} and {}{}", op, l.type_name(), r.type_name(), hint))
}

fn to_scope(binds: Vec<(String, Value)>, mutable: bool) -> HashMap<String, Binding> {
    binds.into_iter().map(|(n, value)| (n, Binding { value, mutable })).collect()
}

fn body_span(b: &Block) -> Span {
    b.stmts
        .first()
        .map(|s| s.span)
        .or(b.tail.as_ref().map(|e| e.span))
        .unwrap_or(Span { line: 0, col: 0 })
}

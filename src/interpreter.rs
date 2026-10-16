//! Tree-walking interpreter for Tyfe.

use crate::ast::*;
use crate::builtins;
use crate::error::{Error, Result};
use crate::value::Value;
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
        Interpreter {
            fns: HashMap::new(),
            structs: HashMap::new(),
            enums: HashMap::new(),
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
            Type::Array(inner) => self.check_type(inner, span),
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
        self.call_fn(main, None, Vec::new(), span).map_err(|c| into_error(c, span))?;
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

    /// Run one loop iteration with `var` bound to `value`.
    fn loop_body(&mut self, var: &str, value: Value, body: &Block) -> R<Value> {
        let mut scope = HashMap::new();
        scope.insert(var.to_string(), Binding { value, mutable: false });
        self.scopes.push(scope);
        let r = self.eval_block_inner(body);
        self.scopes.pop();
        r
    }

    fn exec(&mut self, stmt: &Stmt) -> R<()> {
        let span = stmt.span;
        match &stmt.kind {
            StmtKind::Let { name, mutable, ty, value } => {
                let v = self.eval(value)?;
                if let Some(t) = ty {
                    if !v.matches(t) {
                        return Err(rt(
                            value.span,
                            format!("mismatched types: `{}` is declared as {} but the value is {}", name, t, v.type_name()),
                        ));
                    }
                }
                self.scopes.last_mut().unwrap().insert(name.clone(), Binding { value: v, mutable: *mutable });
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
            StmtKind::For { var, iter, body } => match iter {
                ForIter::Range(start, end, inclusive) => {
                    let start = self.eval_int(start, "range start")?;
                    let end = self.eval_int(end, "range end")?;
                    let mut i = start;
                    while if *inclusive { i <= end } else { i < end } {
                        if loop_ctrl(self.loop_body(var, Value::Int(i), body))? {
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
                        if loop_ctrl(self.loop_body(var, item, body))? {
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
            ExprKind::Var(name) => self
                .lookup(name)
                .cloned()
                .ok_or_else(|| rt(span, format!("cannot find variable `{}` in this scope", name))),
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
                match self.fns.get(name).cloned() {
                    Some(f) => Ok(self.call_fn(f, None, args, span)?.0),
                    None => Ok(builtins::call(name, args, span)?),
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
                    let scope = binds.into_iter().map(|(n, value)| (n, Binding { value, mutable: false })).collect();
                    self.scopes.push(scope);
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

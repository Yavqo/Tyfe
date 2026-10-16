//! Tree-walking interpreter for Tyfe.

use crate::ast::*;
use crate::builtins;
use crate::error::{Error, Result};
use crate::value::Value;
use std::collections::HashMap;
use std::rc::Rc;

const MAX_CALL_DEPTH: usize = 20_000;

struct Binding {
    value: Value,
    mutable: bool,
}

/// How a statement finished.
enum Flow {
    Normal,
    Return(Value),
    Break,
    Continue,
}

pub struct Interpreter {
    fns: HashMap<String, Rc<FnDecl>>,
    structs: HashMap<String, Rc<StructDecl>>,
    scopes: Vec<HashMap<String, Binding>>,
    depth: usize,
}

impl Interpreter {
    pub fn new() -> Self {
        Interpreter { fns: HashMap::new(), structs: HashMap::new(), scopes: vec![HashMap::new()], depth: 0 }
    }

    /// Register functions and structs. With `allow_redefine` (the REPL), later
    /// definitions replace earlier ones; otherwise duplicates are an error.
    pub fn load(&mut self, items: Vec<Item>, allow_redefine: bool) -> Result<()> {
        for item in items {
            match item {
                Item::Fn(f) => {
                    if builtins::is_builtin(&f.name) {
                        return Err(Error::syntax(f.span, format!("`{}` is a built-in function and cannot be redefined", f.name)));
                    }
                    if !allow_redefine && self.fns.contains_key(&f.name) {
                        return Err(Error::syntax(f.span, format!("function `{}` is defined more than once", f.name)));
                    }
                    self.fns.insert(f.name.clone(), Rc::new(f));
                }
                Item::Struct(s) => {
                    if !allow_redefine && self.structs.contains_key(&s.name) {
                        return Err(Error::syntax(s.span, format!("struct `{}` is defined more than once", s.name)));
                    }
                    self.structs.insert(s.name.clone(), Rc::new(s));
                }
            }
        }
        Ok(())
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
        self.call_fn(main.clone(), Vec::new(), main.span)?;
        Ok(())
    }

    /// Run REPL statements in the persistent top-level scope.
    pub fn exec_repl(&mut self, stmt: &Stmt) -> Result<()> {
        match self.exec(stmt)? {
            Flow::Normal => Ok(()),
            Flow::Return(_) => Err(Error::runtime(stmt.span, "`return` outside of a function")),
            Flow::Break | Flow::Continue => Err(Error::runtime(stmt.span, "`break`/`continue` outside of a loop")),
        }
    }

    // ---------- functions ----------

    fn call_fn(&mut self, f: Rc<FnDecl>, args: Vec<Value>, span: Span) -> Result<Value> {
        if args.len() != f.params.len() {
            return Err(Error::runtime(
                span,
                format!(
                    "function `{}` takes {} argument{} but {} were given",
                    f.name,
                    f.params.len(),
                    if f.params.len() == 1 { "" } else { "s" },
                    args.len()
                ),
            ));
        }
        let mut frame = HashMap::new();
        for (p, v) in f.params.iter().zip(args) {
            if !v.matches(&p.ty) {
                return Err(Error::runtime(
                    span,
                    format!("mismatched types: parameter `{}` of `{}` expects {}, got {}", p.name, f.name, p.ty, v.type_name()),
                ));
            }
            frame.insert(p.name.clone(), Binding { value: v, mutable: p.mutable });
        }
        if self.depth >= MAX_CALL_DEPTH {
            return Err(Error::runtime(span, format!("stack overflow: more than {} nested calls", MAX_CALL_DEPTH)));
        }

        let saved = std::mem::replace(&mut self.scopes, vec![frame]);
        self.depth += 1;
        let result = self.exec_stmts(&f.body);
        self.depth -= 1;
        self.scopes = saved;

        let ret = match result? {
            Flow::Return(v) => v,
            Flow::Normal => Value::Unit,
            Flow::Break | Flow::Continue => {
                return Err(Error::runtime(f.span, "`break`/`continue` outside of a loop"))
            }
        };
        if !ret.matches(&f.ret) {
            return Err(Error::runtime(
                span,
                format!("mismatched types: function `{}` should return {}, but returned {}", f.name, f.ret, ret.type_name()),
            ));
        }
        Ok(ret)
    }

    // ---------- statements ----------

    fn exec_block(&mut self, stmts: &[Stmt]) -> Result<Flow> {
        self.scopes.push(HashMap::new());
        let r = self.exec_stmts(stmts);
        self.scopes.pop();
        r
    }

    fn exec_stmts(&mut self, stmts: &[Stmt]) -> Result<Flow> {
        for s in stmts {
            match self.exec(s)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    /// Run one loop iteration with `var` bound to `value` in a fresh scope.
    fn exec_loop_body(&mut self, var: &str, value: Value, body: &[Stmt]) -> Result<Flow> {
        let mut scope = HashMap::new();
        scope.insert(var.to_string(), Binding { value, mutable: false });
        self.scopes.push(scope);
        let r = self.exec_stmts(body);
        self.scopes.pop();
        r
    }

    fn exec(&mut self, stmt: &Stmt) -> Result<Flow> {
        let span = stmt.span;
        match &stmt.kind {
            StmtKind::Let { name, mutable, ty, value } => {
                let v = self.eval(value)?;
                if let Some(t) = ty {
                    if !v.matches(t) {
                        return Err(Error::runtime(
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
                    return Err(Error::runtime(
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
            StmtKind::If { cond, then, else_ } => {
                if self.eval_bool(cond, "`if` condition")? {
                    return self.exec_block(then);
                } else if let Some(else_) = else_ {
                    return self.exec_block(else_);
                }
            }
            StmtKind::While { cond, body } => {
                while self.eval_bool(cond, "`while` condition")? {
                    match self.exec_block(body)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Normal | Flow::Continue => {}
                    }
                }
            }
            StmtKind::For { var, iter, body } => match iter {
                ForIter::Range(start, end, inclusive) => {
                    let start = self.eval_int(start, "range start")?;
                    let end = self.eval_int(end, "range end")?;
                    let mut i = start;
                    while if *inclusive { i <= end } else { i < end } {
                        match self.exec_loop_body(var, Value::Int(i), body)? {
                            Flow::Break => break,
                            Flow::Return(v) => return Ok(Flow::Return(v)),
                            Flow::Normal | Flow::Continue => {}
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
                        other => {
                            return Err(Error::runtime(e.span, format!("cannot iterate over a value of type {}", other.type_name())))
                        }
                    };
                    for item in items {
                        match self.exec_loop_body(var, item, body)? {
                            Flow::Break => break,
                            Flow::Return(v) => return Ok(Flow::Return(v)),
                            Flow::Normal | Flow::Continue => {}
                        }
                    }
                }
            },
            StmtKind::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e)?,
                    None => Value::Unit,
                };
                return Ok(Flow::Return(v));
            }
            StmtKind::Break => return Ok(Flow::Break),
            StmtKind::Continue => return Ok(Flow::Continue),
            StmtKind::Block(stmts) => return self.exec_block(stmts),
        }
        Ok(Flow::Normal)
    }

    // ---------- expressions ----------

    fn lookup(&self, name: &str) -> Option<&Value> {
        self.scopes.iter().rev().find_map(|s| s.get(name)).map(|b| &b.value)
    }

    /// Resolve an assignable location (`x`, `a[i]`, `p.x`, ...) for mutation.
    fn place(&mut self, e: &Expr) -> Result<&mut Value> {
        match &e.kind {
            ExprKind::Var(name) => {
                for scope in self.scopes.iter_mut().rev() {
                    if let Some(b) = scope.get_mut(name) {
                        if !b.mutable {
                            return Err(Error::runtime(
                                e.span,
                                format!("cannot mutate immutable variable `{}` (declare it with `let mut {}`)", name, name),
                            ));
                        }
                        return Ok(&mut b.value);
                    }
                }
                Err(Error::runtime(e.span, format!("cannot find variable `{}` in this scope", name)))
            }
            ExprKind::Index(base, idx) => {
                let i = self.eval(idx)?;
                let b = self.place(base)?;
                builtins::index_mut(b, &i, e.span)
            }
            ExprKind::Field(base, field) => {
                let b = self.place(base)?;
                let tn = b.type_name();
                match b {
                    Value::Struct(sname, fields) => match fields.iter_mut().find(|(n, _)| n == field) {
                        Some((_, v)) => Ok(v),
                        None => Err(Error::runtime(e.span, format!("struct `{}` has no field `{}`", sname, field))),
                    },
                    _ => Err(Error::runtime(e.span, format!("type {} has no fields", tn))),
                }
            }
            _ => Err(Error::runtime(e.span, "cannot mutate a temporary value; store it in a `let mut` variable first")),
        }
    }

    fn eval_bool(&mut self, e: &Expr, what: &str) -> Result<bool> {
        match self.eval(e)? {
            Value::Bool(b) => Ok(b),
            v => Err(Error::runtime(e.span, format!("mismatched types: {} must be a bool, got {}", what, v.type_name()))),
        }
    }

    fn eval_int(&mut self, e: &Expr, what: &str) -> Result<i64> {
        match self.eval(e)? {
            Value::Int(n) => Ok(n),
            v => Err(Error::runtime(e.span, format!("mismatched types: {} must be an int, got {}", what, v.type_name()))),
        }
    }

    fn eval_args(&mut self, args: &[Expr]) -> Result<Vec<Value>> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            out.push(self.eval(a)?);
        }
        Ok(out)
    }

    fn eval(&mut self, e: &Expr) -> Result<Value> {
        let span = e.span;
        match &e.kind {
            ExprKind::Int(n) => Ok(Value::Int(*n)),
            ExprKind::Float(f) => Ok(Value::Float(*f)),
            ExprKind::Str(s) => Ok(Value::Str(s.clone())),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Var(name) => self
                .lookup(name)
                .cloned()
                .ok_or_else(|| Error::runtime(span, format!("cannot find variable `{}` in this scope", name))),
            ExprKind::Unary(op, operand) => {
                let v = self.eval(operand)?;
                match (op, v) {
                    (UnOp::Neg, Value::Int(n)) => n
                        .checked_neg()
                        .map(Value::Int)
                        .ok_or_else(|| Error::runtime(span, "attempt to negate with overflow")),
                    (UnOp::Neg, Value::Float(f)) => Ok(Value::Float(-f)),
                    (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                    (UnOp::Neg, v) => Err(Error::runtime(span, format!("cannot apply unary `-` to type {}", v.type_name()))),
                    (UnOp::Not, v) => Err(Error::runtime(span, format!("cannot apply unary `!` to type {}", v.type_name()))),
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
                binary(*op, l, r, span)
            }
            ExprKind::Call(name, args) => {
                let args = self.eval_args(args)?;
                match self.fns.get(name).cloned() {
                    Some(f) => self.call_fn(f, args, span),
                    None => builtins::call(name, args, span),
                }
            }
            ExprKind::MethodCall(recv, name, args) => {
                let args = self.eval_args(args)?;
                if builtins::is_mutating_method(name) {
                    let place = self.place(recv)?;
                    builtins::method_mut(place, name, args, span)
                } else {
                    let v = self.eval(recv)?;
                    builtins::method(v, name, args, span)
                }
            }
            ExprKind::Index(base, idx) => {
                let b = self.eval(base)?;
                let i = self.eval(idx)?;
                builtins::index(&b, &i, span)
            }
            ExprKind::Field(base, field) => match self.eval(base)? {
                Value::Struct(sname, fields) => fields
                    .into_iter()
                    .find(|(n, _)| n == field)
                    .map(|(_, v)| v)
                    .ok_or_else(|| Error::runtime(span, format!("struct `{}` has no field `{}`", sname, field))),
                v => Err(Error::runtime(span, format!("type {} has no fields", v.type_name()))),
            },
            ExprKind::Array(elems) => {
                let items = self.eval_args(elems)?;
                if let Some(first) = items.first() {
                    for (v, ex) in items.iter().zip(elems).skip(1) {
                        if !first.same_kind(v) {
                            return Err(Error::runtime(
                                ex.span,
                                format!("mismatched types: array elements must all have the same type, expected {}, found {}", first.type_name(), v.type_name()),
                            ));
                        }
                    }
                }
                Ok(Value::Array(items))
            }
            ExprKind::StructLit(name, inits) => {
                let decl = self
                    .structs
                    .get(name)
                    .cloned()
                    .ok_or_else(|| Error::runtime(span, format!("cannot find struct `{}`", name)))?;
                for (fname, ex) in inits {
                    if !decl.fields.iter().any(|(n, _)| n == fname) {
                        return Err(Error::runtime(ex.span, format!("struct `{}` has no field named `{}`", name, fname)));
                    }
                    if inits.iter().filter(|(n, _)| n == fname).count() > 1 {
                        return Err(Error::runtime(ex.span, format!("field `{}` specified more than once", fname)));
                    }
                }
                let mut fields = Vec::with_capacity(decl.fields.len());
                for (fname, fty) in &decl.fields {
                    let ex = inits
                        .iter()
                        .find(|(n, _)| n == fname)
                        .map(|(_, ex)| ex)
                        .ok_or_else(|| Error::runtime(span, format!("missing field `{}` in initializer of `{}`", fname, name)))?;
                    let v = self.eval(ex)?;
                    if !v.matches(fty) {
                        return Err(Error::runtime(
                            ex.span,
                            format!("mismatched types: field `{}.{}` expects {}, got {}", name, fname, fty, v.type_name()),
                        ));
                    }
                    fields.push((fname.clone(), v));
                }
                Ok(Value::Struct(name.clone(), fields))
            }
        }
    }
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
    Error::runtime(
        span,
        format!("cannot apply `{}` to {} and {}{}", op, l.type_name(), r.type_name(), hint),
    )
}

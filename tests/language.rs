use std::io::Write;
use std::process::{Command, Stdio};

/// Run a Tyfe program from source, returning (exit code, stdout, stderr).
fn run(src: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tyfe"))
        .args(["run", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start tyfe");
    child.stdin.take().unwrap().write_all(src.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn ok(src: &str) -> String {
    let (code, out, err) = run(src);
    assert_eq!(code, 0, "program failed:\n{}", err);
    out
}

fn fails_with(src: &str, msg: &str) {
    let (code, _, err) = run(src);
    assert_eq!(code, 1, "expected failure");
    assert!(err.contains(msg), "expected error containing {:?}, got:\n{}", msg, err);
}

#[test]
fn examples_match_expected_output() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/examples");
    let mut checked = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().map_or(true, |e| e != "ty") {
            continue;
        }
        let expected = std::fs::read_to_string(path.with_extension("out"))
            .unwrap_or_else(|_| panic!("missing .out file for {}", path.display()));
        let (code, out, err) = run(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(code, 0, "{} failed:\n{}", path.display(), err);
        assert_eq!(out, expected, "output mismatch for {}", path.display());
        checked += 1;
    }
    assert!(checked > 0);
}

#[test]
fn arithmetic_and_precedence() {
    assert_eq!(ok("fn main() { println(1 + 2 * 3 - 4 / 2); }"), "5\n");
    assert_eq!(ok("fn main() { println((1 + 2) * 3); }"), "9\n");
    assert_eq!(ok("fn main() { println(-7 / 2, ); }"), "-3\n");
    assert_eq!(ok("fn main() { println(-7 % 3); }"), "-1\n");
    assert_eq!(ok("fn main() { println(1.5 * 2.0); }"), "3.0\n");
    assert_eq!(ok("fn main() { println(!true || 1 < 2 && 2 >= 2); }"), "true\n");
}

#[test]
fn short_circuit() {
    let src = "fn boom() -> bool { panic(\"evaluated\"); return true; }
               fn main() { println(false && boom()); println(true || boom()); }";
    assert_eq!(ok(src), "false\ntrue\n");
}

#[test]
fn scoping_and_shadowing() {
    let src = "fn main() {
        let x = 1;
        { let x = 2; println(x); }
        let x = x + 10;
        println(x);
    }";
    assert_eq!(ok(src), "2\n11\n");
}

#[test]
fn arguments_are_passed_by_value() {
    let src = "fn change(mut a: [int]) { a[0] = 99; }
               fn main() { let a = [1]; change(a); println(a); }";
    assert_eq!(ok(src), "[1]\n");
}

#[test]
fn loops_break_continue_return() {
    let src = "fn find(xs: [int], t: int) -> int {
        for i in 0..xs.len() { if xs[i] == t { return i; } }
        return -1;
    }
    fn main() {
        let mut out = [];
        for i in 0..10 { if i % 2 == 0 { continue; } if i > 7 { break; } out.push(i); }
        println(out);
        println(find([4, 5, 6], 6));
        println(find([4, 5, 6], 7));
    }";
    assert_eq!(ok(src), "[1, 3, 5, 7]\n2\n-1\n");
}

#[test]
fn strings_and_formatting() {
    assert_eq!(ok("fn main() { println(\"{} + {} = {}\", 1, 2, 3); }"), "1 + 2 = 3\n");
    assert_eq!(ok("fn main() { println(\"{:?} {{x}}\", \"q\"); }"), "\"q\" {x}\n");
    assert_eq!(ok("fn main() { let s = format(\"{}-{}\", \"a\", 1); println(s.len()); }"), "3\n");
    assert_eq!(ok("fn main() { for c in \"héy\" { print(c); } println(); }"), "héy\n");
    assert_eq!(ok("fn main() { println(int(\"42\") + 1, ); }"), "43\n");
}

#[test]
fn structs_nested_update() {
    let src = "struct V { x: int, y: int }
               struct Box { v: V, tags: [str] }
               fn main() {
                   let mut b = Box { v: V { x: 1, y: 2 }, tags: [\"a\"] };
                   b.v.y *= 10;
                   b.tags.push(\"b\");
                   println(\"{:?}\", b);
                   println(b.v == V { x: 1, y: 20 });
               }";
    assert_eq!(ok(src), "Box { v: V { x: 1, y: 20 }, tags: [\"a\", \"b\"] }\ntrue\n");
}

#[test]
fn runtime_errors() {
    fails_with("fn main() { let x = 1; x = 2; }", "cannot mutate immutable variable `x`");
    fails_with("fn main() { let a = [1]; a.push(2); }", "cannot mutate immutable variable `a`");
    fails_with("fn main() { let x = 1 + 1.0; }", "cannot apply `+` to int and float");
    fails_with("fn main() { let x: str = 5; }", "declared as str but the value is int");
    fails_with("fn main() { let mut x = 5; x = \"s\"; }", "cannot assign a str to a place of type int");
    fails_with("fn main() { println([1][3]); }", "index out of bounds");
    fails_with("fn main() { println(10 / 0); }", "divide by zero");
    fails_with("fn main() { println(9223372036854775807 * 2); }", "overflow");
    fails_with("fn f(a: int) {} fn main() { f(\"x\"); }", "parameter `a` of `f` expects int, got str");
    fails_with("fn f(a: int) {} fn main() { f(); }", "takes 1 argument but 0 were given");
    fails_with("fn f() -> int { } fn main() { f(); }", "should return int, but returned ()");
    fails_with("fn main() { nope(); }", "cannot find function `nope`");
    fails_with("fn main() { while 1 {} }", "must be a bool");
    fails_with("struct P { x: int } fn main() { let p = P { }; }", "missing field `x`");
    fails_with("struct P { x: int } fn main() { let p = P { x: 1 }; println(p.y); }", "has no field `y`");
    fails_with("fn main() { println(\"{} {}\", 1); }", "more `{}` placeholders than arguments");
    fails_with("fn f() { f(); } fn main() { f(); }", "stack overflow");
    fails_with("fn helper() {}", "no `main` function");
}

#[test]
fn syntax_errors() {
    fails_with("fn main() { let x = 1 }", "expected `;` after `let` statement");
    fails_with("fn main() { let = 1; }", "expected variable name");
    fails_with("fn main() { println(\"unterminated); }", "unterminated string");
    fails_with("fn main() { 1 = 2; }", "invalid left-hand side");
    fails_with("fn add(a, b) {}", "parameters need a type");
    fails_with("fn main() {} fn main() {}", "defined more than once");
    fails_with("let x = 1;", "expected `fn`, `struct`, `enum`, `impl` or `const` at top level");
    fails_with("fn main() { let x = 1 @ 2; }", "unexpected character `@`");
}

#[test]
fn impl_methods_and_associated_functions() {
    let src = "struct Stack { items: [int] }
    impl Stack {
        fn new() -> Self { Self { items: [] } }
        fn push(&mut self, x: int) { self.items.push(x); }
        fn push_twice(&mut self, x: int) { self.push(x); self.push(x); }
        fn len(&self) -> int { self.items.len() }
        fn with(mut self, x: int) -> Stack { self.items.push(x); self }
    }
    fn main() {
        let mut s = Stack::new();
        s.push_twice(7);
        let t = s.with(1).with(2);
        println(\"{} {} {:?}\", s.len(), Stack::len(t), s);
    }";
    assert_eq!(ok(src), "2 4 Stack { items: [7, 7] }\n");
}

#[test]
fn enums_and_match() {
    let src = "enum Opt { Some(int), None }
    fn find(xs: [int], t: int) -> Opt {
        for i in 0..xs.len() { if xs[i] == t { return Opt::Some(i); } }
        Opt::None
    }
    fn size(n: int) -> str {
        match n { 0 => \"zero\", 1 | 2 => \"small\", 3..=9 => \"medium\", x if x < 0 => \"negative\", _ => \"big\" }
    }
    fn main() {
        match find([4, 5, 6], 6) { Opt::Some(i) => println(\"at {}\", i), Opt::None => println(\"none\") }
        println(\"{:?}\", find([], 1));
        println(\"{} {} {} {} {}\", size(0), size(2), size(5), size(-1), size(50));
        println(Opt::Some(1) == Opt::Some(1));
    }";
    assert_eq!(ok(src), "at 2\nNone\nzero small medium negative big\ntrue\n");
}

#[test]
fn expressions_and_implicit_returns() {
    let src = "const BASE: int = 10;
    fn pick(c: bool) -> int { if c { BASE } else { BASE * 2 } }
    fn first_even(xs: [int]) -> int {
        let mut i = 0;
        loop {
            if xs[i] % 2 == 0 { break xs[i]; }
            i += 1;
        }
    }
    fn classify(x: int) -> str {
        let s = match x { 0 => return \"early\", _ => \"late\" };
        s
    }
    fn main() {
        let v = { let a = 2; a * 3 };
        println(\"{} {} {} {} {} {}\", pick(true), pick(false), first_even([3, 5, 8, 9]), classify(0), classify(1), v);
    }";
    assert_eq!(ok(src), "10 20 8 early late 6\n");
}

#[test]
fn new_feature_errors() {
    fails_with("struct C { n: int } impl C { fn f(&self) { self.n = 1; } } fn main() { C { n: 0 }.f(); }", "cannot mutate `self`");
    fails_with("struct C { n: int } impl C { fn f(&mut self) {} } fn main() { let c = C { n: 0 }; c.f(); }", "cannot mutate immutable variable `c`");
    fails_with("fn f(x: Foo) {} fn main() {}", "cannot find type `Foo`");
    fails_with("impl Foo {} fn main() {}", "cannot find type `Foo` to implement");
    fails_with("enum E { A(int) } fn main() { let e = E::A(); }", "takes 1 value(s) but 0 were given");
    fails_with("enum E { A } fn main() { let e = E::B; }", "no variant or associated function named `B`");
    fails_with("fn main() { let x = match 5 { 1 => 1, }; }", "no `match` arm matched the value 5");
    fails_with("enum E { A(int) } fn main() { match E::A(1) { E::A => {} } }", "has 0 field(s) but the variant has 1");
    fails_with("const X: int = 1; fn main() { X = 2; }", "cannot assign to constant `X`");
    fails_with("fn main() { break; }", "outside of a loop");
    fails_with("fn f() { return 5; } fn main() { f(); }", "should return (), but returned int");
    fails_with("fn main() { let x = Self::new(); }", "`Self` can only be used inside an `impl` block");
    fails_with("fn f(&self) {} fn main() {}", "only allowed in `impl` blocks");
    fails_with("struct C {} impl C { fn new() -> C { C {} } } fn main() { C::new().new(); }", "is an associated function, not a method");
}

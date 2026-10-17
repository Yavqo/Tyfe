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
    fails_with("let x = 1;", "expected an item");
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
    fails_with("fn f(&self) {} fn main() {}", "only allowed in `impl` and `trait` blocks");
    fails_with("struct C {} impl C { fn new() -> C { C {} } } fn main() { C::new().new(); }", "is an associated function, not a method");
}

#[test]
fn option_result_and_try() {
    let src = "fn half(n: int) -> Result<int, str> {
        if n % 2 == 1 { return Err(format(\"{} is odd\", n)); }
        Ok(n / 2)
    }
    fn quarter(n: int) -> Result<int, str> { let h = half(n)?; half(h) }
    fn first_neg(xs: [int]) -> Option<int> { xs.find(|x| x < 0) }
    fn main() {
        println(\"{:?} {:?} {:?}\", quarter(20), quarter(6), quarter(7));
        println(\"{:?} {:?}\", first_neg([1, -2, -3]), first_neg([1]));
        let mut s = [1, 2];
        let mut out = 0;
        while let Some(x) = s.pop() { out = out * 10 + x; }
        println(out);
        if let Ok(n) = \"12\".parse_int() { println(n + 1); }
        println(\"{} {}\", None.unwrap_or(5), Some(1).map(|x| x + 1).unwrap());
    }";
    assert_eq!(ok(src), "Ok(5) Err(\"3 is odd\") Err(\"7 is odd\")\nSome(-2) None\n21\n13\n5 2\n");
}

#[test]
fn closures_and_function_values() {
    let src = "fn twice(f: fn(int) -> int, x: int) -> int { f(f(x)) }
    fn adder(n: int) -> fn(int) -> int { |x| x + n }
    fn sq(x: int) -> int { x * x }
    fn main() {
        let k = 3;
        let times_k = |x| x * k;
        println(\"{} {} {} {}\", twice(times_k, 2), twice(sq, 3), adder(5)(1), [1, 2].map(sq));
        let xs = [5, 2, 8, 1];
        println(xs.iter().filter(|x| x > 1).map(|x| x * 10).collect());
        println(\"{} {} {} {}\", xs.fold(0, |a, b| a + b), xs.any(|x| x == 8), xs.all(|x| x > 1), xs.count(|x| x > 1));
        println(\"{:?} {:?} {:?}\", xs.position(|x| x == 8), xs.sorted(), xs.min());
        let mut words = [\"ccc\", \"a\", \"bb\"];
        words.sort_by_key(|s| s.len());
        println(\"{:?}\", words);
    }";
    assert_eq!(ok(src), "18 81 6 [1, 4]\n[50, 20, 80]\n16 true false 3\nSome(2) [1, 2, 5, 8] Some(1)\n[\"a\", \"bb\", \"ccc\"]\n");
}

#[test]
fn tuples_and_destructuring() {
    let src = "fn swap(p: (int, str)) -> (str, int) { (p.1, p.0) }
    fn main() {
        let (a, b) = swap((1, \"x\"));
        println(\"{} {}\", a, b);
        let mut total = 0;
        for (i, v) in [10, 20].enumerate() { total += i * v; }
        println(total);
        let nested = ((1, 2), 3);
        println(nested.0.1);
        let m = match (1, 0) { (0, _) => \"a\", (_, 0) => \"b\", _ => \"c\" };
        println(\"{} {:?}\", m, [1, 2].zip([\"x\", \"y\"]));
    }";
    assert_eq!(ok(src), "x 1\n20\n2\nb [(1, \"x\"), (2, \"y\")]\n");
}

#[test]
fn newer_feature_errors() {
    fails_with("fn main() { let x: Option<int> = None; x.unwrap(); }", "called `unwrap()` on a `None` value");
    fails_with("fn main() { let r: Result<int, str> = Err(\"bad\"); r.unwrap(); }", "on an `Err` value: \"bad\"");
    fails_with("fn main() -> Result<(), str> { let n = \"x\".parse_int()?; Ok(()) }", "main returned an error");
    fails_with("fn main() { let x = 5; let y = x?; }", "`?` can only be used on an Option or Result");
    fails_with("fn main() { let x: Option<int> = Some(\"a\"); }", "declared as Option<int> but the value is Option<str>");
    fails_with("fn main() { let (a, b) = (1, 2, 3); }", "pattern does not match");
    fails_with("fn main() { let t = (1, 2); println(t.5); }", "out of range");
    fails_with("fn main() { let x = 5; x(1); }", "is not a function");
    fails_with("fn main() { let f = |a, b| a + b; f(1); }", "closure takes 2 argument(s) but 1 were given");
    fails_with("fn ap(f: fn(int) -> int) -> int { f(1) } fn main() { ap(|a, b| a); }", "expects fn(int) -> int, got fn(_, _)");
    fails_with("fn main() { [1].filter(|x| x); }", "must return a bool");
}

/// Run the binary with command-line arguments (for files, `tyfe test`, ...).
fn run_args(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_tyfe")).args(args).output().expect("failed to start tyfe");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn macros_and_format_specs() {
    let src = r#"fn main() {
        let name = "ty"; let n = 7;
        println!("{name}:{n:>3}|{:<4}|{:^5}|{:05}|{:+}|{:.2}", "a", "b", 42, 3, 2.0);
        println!("{0}{1}{0} {2:x} {3:#b} {4:e}", "x", "y", 255, 5, 1500.0);
        let v = vec![0; 3];
        let s = format!("{v:?}");
        assert_eq!(s, "[0, 0, 0]");
        assert!(s.len() == 9, "len {}", s.len());
        print!("{}", 'c');
        println!("{}", '\n' == "\n");
    }"#;
    assert_eq!(ok(src), "ty:  7|a   |  b  |00042|+3|2.00\nxyx ff 0b101 1.5e3\nctrue\n");
}

#[test]
fn traits_generics_and_display() {
    let src = r#"use std::fmt;
    trait Speak { fn word(&self) -> String; fn twice(&self) -> String { format!("{0} {0}", self.word()) } }
    struct A; struct B { n: i64 }
    impl Speak for A { fn word(&self) -> String { "a".into() } }
    impl Speak for B { fn word(&self) -> String { self.n.to_string() } fn twice(&self) -> String { "custom".into() } }
    fn all(xs: &Vec<Box<dyn Speak>>) -> Vec<String> { xs.iter().map(|x| x.twice()).collect() }
    fn biggest<T: PartialOrd + Copy>(xs: &[T]) -> T { let mut b = xs[0]; for &x in xs { if x > b { b = x; } } b }
    struct W<T> { v: T }
    impl<T: fmt::Display> fmt::Display for W<T> {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, "<{}>", self.v) }
    }
    impl Speak for i64 { fn word(&self) -> String { format!("int {}", self) } }
    fn main() {
        println!("{:?}", all(&vec![Box::new(A), Box::new(B { n: 5 })]));
        println!("{} {}", biggest(&[1, 9, 3]), biggest(&["x", "b"]));
        let w = W { v: 2.5 };
        println!("{} [{:>6}] {}", w, w, w.to_string());
        println!("{}", 3.twice());
    }"#;
    assert_eq!(ok(src), "[\"a a\", \"custom\"]\n9 x\n<2.5> [ <2.5>] <2.5>\nint 3 int 3\n");
}

#[test]
fn mutable_references() {
    let src = "fn inc(x: &mut i64) { *x += 1; }
    fn fill(v: &mut Vec<i64>, n: i64) { for i in 0..n { v.push(i); } }
    struct C { n: i64 }
    fn bump(c: &mut C) { c.n *= 10; }
    fn main() {
        let mut a = 1; inc(&mut a); inc(&mut a);
        let mut v = Vec::new(); fill(&mut v, 3);
        let mut c = C { n: 2 }; bump(&mut c);
        let mut arr = [1, 2]; inc(&mut arr[1]);
        let (mut x, mut y) = (1, 2); std::mem::swap(&mut x, &mut y);
        println!(\"{a} {v:?} {} {arr:?} {x}{y}\", c.n);
    }";
    assert_eq!(ok(src), "3 [0, 1, 2] 20 [1, 3] 21\n");
}

#[test]
fn hashmaps_and_sets() {
    let src = r#"use std::collections::{HashMap, HashSet};
    fn main() {
        let mut m = HashMap::new();
        for w in "b a b c b".split_whitespace() { *m.entry(w).or_insert(0) += 1; }
        m["z"] = 9;
        println!("{:?} {} {:?} {:?}", m, m["b"], m.get("q"), m.remove("a"));
        let mut g: HashMap<i64, Vec<i64>> = HashMap::new();
        for x in [1, 2, 3, 4] { g.entry(x % 2).or_insert(Vec::new()).push(x); }
        let total: i64 = g.values().map(|v| v.len() as i64).sum();
        println!("{:?} {} {:?}", g, total, g.keys());
        let s: HashSet<i64> = HashSet::from([3, 1, 2]);
        let t = HashSet::from([2, 5]);
        println!("{:?} {} {:?} {:?}", s, s.contains(&2), s.union(&t), &s - &t);
    }"#;
    assert_eq!(
        ok(src),
        "{\"a\": 1, \"b\": 3, \"c\": 1, \"z\": 9} 3 None Some(1)\n{0: [2, 4], 1: [1, 3]} 4 [0, 1]\n{1, 2, 3} true {1, 2, 3, 5} {1, 3}\n"
    );
}

#[test]
fn ranges_slices_casts_and_bits() {
    let src = r#"fn main() {
        let xs = [1, 2, 3, 4, 5];
        println!("{:?} {:?} {:?} {}", &xs[1..3], &xs[..2], &xs[3..], &"hello"[1..=3]);
        let r: Vec<i64> = (1..=4).rev().collect();
        println!("{:?} {:?} {}", r, (0..10).step_by(4).collect::<Vec<_>>(), ('a'..'d').collect::<String>());
        println!("{} {} {} {} {}", 7 as f64 / 2.0, -2.7 as i64, 'a' as u8, 98 as char, true as i64);
        println!("{} {} {} {} {} {}", 0xF0 & 0x3C, 0b1010 | 1, 6 ^ 3, 1 << 4, -16 >> 2, 1_000_000);
        println!("{} {}", 2.5e3, 10u8 + 5i64);
    }"#;
    assert_eq!(ok(src), "[2, 3] [1, 2] [4, 5] ell\n[4, 3, 2, 1] [0, 4, 8] abc\n3.5 -2 97 b 1\n48 11 5 16 -4 1000000\n2500.0 15\n");
}

#[test]
fn labels_patterns_and_struct_sugar() {
    let src = r#"#[derive(Debug, Clone)]
    struct P { x: i64, y: i64 }
    fn main() {
        let mut hits = 0;
        'outer: for i in 0..5 {
            for j in 0..5 {
                if j > i { continue 'outer; }
                if i == 3 { break 'outer; }
                hits += 1;
            }
        }
        let v = 'find: loop { break 'find 99; };
        let x = 1; let y = 2;
        let p = P { x, y };
        let q = P { y: 7, ..p.clone() };
        let P { x: qx, .. } = q.clone();
        let kind = match q { P { x: 0, .. } => "on axis", P { y, .. } if y > 5 => "high", _ => "other" };
        println!("{hits} {v} {:?} {qx} {kind} {}", p, matches!('k', 'a'..='m'));
    }"#;
    assert_eq!(ok(src), "6 99 P { x: 1, y: 2 } 1 high true\n");
}

#[test]
fn modules_and_test_runner() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let (code, out, err) = run_args(&[&format!("{}/examples/modules/main.ty", dir)]);
    assert_eq!(code, 0, "{}", err);
    assert!(out.contains("3.0x4.0 rect has area 12.0"), "{}", out);

    let (code, out, _) = run_args(&["test", &format!("{}/examples/testing.ty", dir)]);
    assert_eq!(code, 0);
    assert!(out.contains("test result: ok. 2 passed; 0 failed"), "{}", out);

    let failing = std::env::temp_dir().join("tyfe_failing_test.ty");
    std::fs::write(&failing, "#[test]\nfn good() { assert!(true); }\n#[test]\nfn bad() { assert_eq!(1, 2); }\n").unwrap();
    let (code, out, err) = run_args(&["test", failing.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(out.contains("test bad ... FAILED") && out.contains("1 passed; 1 failed"), "{}", out);
    assert!(err.contains("assertion failed"), "{}", err);
}

#[test]
fn batch_three_errors() {
    fails_with("trait A { fn f(&self); } struct S; impl A for S {} fn main() {}", "is missing the method `f`");
    fails_with("struct S; impl Nope for S {} fn main() {}", "cannot find trait `Nope`");
    fails_with("trait A { fn f(&self); } struct T; fn g(x: impl A) {} fn main() { g(T); }", "T does not implement the trait `A`");
    fails_with("fn inc(x: &mut i64) {} fn main() { let mut n = 1; inc(n); }", "pass a mutable variable like `&mut x`");
    fails_with("fn main() { loop { break 'nope; } }", "no enclosing loop is labeled `'nope`");
    fails_with("fn main() { foo!(1); }", "unknown macro `foo!`");
    fails_with("fn main() { let a = [1]; println!(\"{:?}\", &a[0..5]); }", "out of bounds");
    fails_with("use std::collections::HashMap; fn main() { let m: HashMap<i64, i64> = HashMap::new(); m[&1]; }", "not found in map");
    fails_with("fn main() { println!(\"{nope}\"); }", "cannot find variable `nope` used in format string");
    fails_with("fn main() { println!(\"{}\", 1, 2); }", "is never used");
    fails_with("fn f() -> i64 { todo!() } fn main() { f(); }", "not yet implemented");
    fails_with("fn main() { assert_ne!(1, 1); }", "left != right");
    fails_with("mod nothere; fn main() {}", "cannot find module `nothere`");
    fails_with("fn main() { let x = 0b; }", "expected digits after `0b`");
}

#[test]
fn rust_mutation_idioms_and_named_variants() {
    let src = r#"use std::collections::HashMap;
    #[derive(Debug)]
    enum Msg { Move { x: i64, y: i64 }, Quit }
    fn main() {
        let mut v = vec![1, 2, 3];
        for x in v.iter_mut() { *x *= 10; }
        for x in &mut v { *x += 1; }
        *v.last_mut().unwrap() = 0;
        let mut m: HashMap<&str, Vec<i64>> = HashMap::new();
        m.insert("a", vec![]);
        m.get_mut("a").unwrap().push(5);
        if let Some(list) = m.get_mut("a") { list.push(6); }
        let mut s = HashMap::from([("x", 1)]);
        for (_, n) in s.iter_mut() { *n *= 7; }
        let msgs = [Msg::Move { x: 1, y: 2 }, Msg::Quit];
        for msg in &msgs {
            match msg {
                Msg::Move { x, y: 2 } => println!("move {x}"),
                Msg::Move { .. } => println!("other move"),
                Msg::Quit => println!("quit"),
            }
        }
        println!("{:?} {:?} {:?} {:?}", v, m, s, msgs[0]);
    }"#;
    assert_eq!(ok(src), "move 1\nquit\n[11, 21, 0] {\"a\": [5, 6]} {\"x\": 7} Move { x: 1, y: 2 }\n");
}

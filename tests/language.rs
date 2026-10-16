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
    fails_with("let x = 1;", "expected `fn` or `struct` at top level");
    fails_with("fn main() { let x = 1 @ 2; }", "unexpected character `@`");
}

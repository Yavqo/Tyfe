# Tyfe

Tyfe is a programming language that looks and feels like Rust: `fn`, `let mut`, structs, enums, `match`, traits, generics, closures, `Option` / `Result` with `?`, `HashMap`, `println!` and more. A lot of everyday Rust code runs in Tyfe unchanged, but there's no borrow checker to fight, so it's easy to learn and quick to experiment with.

```rust
use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone)]
struct Point<T> {
    x: T,
    y: T,
}

impl<T: fmt::Display> fmt::Display for Point<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}

fn main() {
    let p = Point { x: 3, y: 4 };
    println!("{p} {p:?}");                       // (3, 4) Point { x: 3, y: 4 }

    let mut counts = HashMap::new();
    for word in "a b a c a".split_whitespace() {
        *counts.entry(word).or_insert(0) += 1;
    }
    println!("{:?}", counts);                     // {"a": 3, "b": 1, "c": 1}
}
```

Tyfe is a **tree-walking interpreter written in Rust**.

## Getting started

You need a Rust toolchain (`cargo`).

```sh
cargo build --release
./target/release/tyfe examples/hello.ty            # run a program
./target/release/tyfe examples/hello.ty a b c      # ...with arguments (see args())
./target/release/tyfe test examples/testing.ty     # run #[test] functions
./target/release/tyfe check examples/traits.ty     # check without running
./target/release/tyfe                              # interactive REPL
cargo test                                         # the interpreter's own tests
```

Source files use the `.ty` extension. Every program needs a `fn main()`. Use the release build for real programs: it is much faster and allows deeper recursion.

## How Tyfe differs from Rust

- **No borrow checker or lifetimes.** Values are copied when you pass or assign them. `&` is accepted and ignored. Lifetimes like `'a` and `'static` are accepted in types.
- **`&mut` still works.** A `fn f(x: &mut T)` called as `f(&mut x)` really updates `x`. So do `&mut self` methods, `for x in v.iter_mut()`, `for x in &mut v`, `map.get_mut(k).unwrap().push(..)`, `if let Some(x) = map.get_mut(k)` and `*map.entry(k).or_insert(0) += 1`.
- **Fewer number types.** `int` is a 64-bit signed integer and `float` is 64-bit. Rust names like `i32`, `u8`, `usize` and `f64` are aliases for them. Integer overflow is an error, as in Rust's debug mode.
- **`char` is a one-character string.** `'a'` and `"a"` are the same value, so `'a' as u8` and `97 as char` both work.
- **Types are checked while the program runs.** You get clear errors at the exact line, but only when that line runs.
- **Closures capture copies.** A closure keeps the values it uses from when it was created.
- **`HashMap` and `HashSet` are kept sorted**, so printing and iterating give the same order every time.

## Language tour

### Variables and types

```rust
let name = "Tyfe";            // immutable, type inferred
let year: int = 2026;         // with a type (i64 works too)
let mut count = 0;            // `mut` makes it changeable
count += 1;                   // also -= *= /= %= &= |= ^= <<= >>=
let (a, mut b) = (1, 2);      // destructuring
```

| Type | Example | Notes |
|---|---|---|
| `int` / `i64` | `42`, `1_000_000`, `0xFF`, `0b1010`, `7u8` | 64-bit signed |
| `float` / `f64` | `3.14`, `2.5e3` | 64-bit |
| `bool` | `true`, `false` | |
| `str` / `String` / `char` | `"hi\n"`, `'c'`, `'\u{1F600}'` | |
| `[T]` / `Vec<T>` | `[1, 2]`, `vec![0; 10]` | growable arrays |
| `(A, B)` | `(1, "one")` | tuples |
| `Option<T>` | `Some(5)`, `None` | |
| `Result<T, E>` | `Ok(5)`, `Err("bad")` | |
| `HashMap<K, V>` | `HashMap::new()` | |
| `HashSet<T>` | `HashSet::from([1, 2])` | |
| `fn(A) -> B` | `\|x\| x + 1`, `square` | closures and functions |
| your structs and enums | `Point { x: 1, y: 2 }` | |

Tyfe never mixes types silently. `1 + 2.0` is an error, so write `1 as f64 + 2.0`. Conditions must be `bool`, and a variable keeps its type.

### Functions

```rust
fn add(a: i64, b: i64) -> i64 {
    a + b                          // last expression = return value
}

fn largest<T: PartialOrd + Copy>(xs: &[T]) -> T { ... }   // generics

fn bump(x: &mut i64) { *x += 1; }  // called as bump(&mut n)
```

### Control flow

`if`, `match`, `loop` and blocks are expressions.

```rust
let size = if n > 100 { "big" } else { "small" };

'outer: for i in 0..10 {           // labeled loops
    for j in 0..10 {
        if i * j == 42 { break 'outer; }
    }
}

let found = loop { if done() { break 42; } };
while let Some(top) = stack.pop() { ... }
for (i, x) in xs.iter().enumerate() { ... }
for (key, value) in &map { ... }
```

### Structs, enums and `match`

```rust
#[derive(Debug, Clone, PartialEq)]          // attributes are accepted
struct Point { x: i64, y: i64 }

let p = Point { x, y };                     // field shorthand
let q = Point { x: 0, ..p.clone() };        // update syntax

enum Shape {
    Circle(f64),
    Rect { w: f64, h: f64 },
    Empty,
}

fn area(s: &Shape) -> f64 {
    match s {
        Shape::Circle(r) => 3.14 * r * r,
        Shape::Rect { w, h } => w * h,
        Shape::Empty => 0.0,
    }
}
```

Patterns include literals, `1..=9`, `'a'..='z'`, `A | B`, `_`, bindings, `mut x`, tuples `(a, _)`, structs `Point { x: 0, .. }`, enum variants, and guards (`x if x > 0`). `matches!(value, pattern)` tests a pattern.

### Methods, traits and generics

```rust
impl Point {
    fn new(x: i64, y: i64) -> Self { Self { x, y } }
    fn len2(&self) -> i64 { self.x * self.x + self.y * self.y }
    fn shift(&mut self, dx: i64) { self.x += dx; }
}

trait Animal {
    fn name(&self) -> String;
    fn greet(&self) -> String {                    // default method
        format!("Hi, I'm {}", self.name())
    }
}

impl Animal for Dog { fn name(&self) -> String { "Rex".into() } }

fn intro(a: &impl Animal) { ... }                  // impl Trait
fn all(zoo: &Vec<Box<dyn Animal>>) { ... }         // trait objects
fn show<T: Animal>(a: T) { ... }                   // generic bounds (also `where`)
impl Animal for i64 { ... }                        // traits on built-in types
```

To control how `{}` prints your type, implement `fmt::Display` as in Rust, or define `fn to_string(&self) -> String`.

### Closures and iterators

```rust
let double = |x| x * 2;
let add = |a: i64, b: i64| -> i64 { a + b };
fn make_adder(n: i64) -> fn(i64) -> i64 { move |x| x + n }

let evens: Vec<i64> = nums.iter().filter(|&x| x % 2 == 0).map(|x| x * x).collect();
let total: i64 = nums.iter().sum();
people.sort_by(|a, b| b.age.cmp(&a.age).then(a.name.cmp(&b.name)));
```

Array and iterator methods:
- `map` `filter` `filter_map` `flat_map` `fold` `reduce` `for_each` `any` `all` `count` `find` `find_map` `position`
- `take` `skip` `take_while` `skip_while` `step_by` `chain` (via `+`) `zip` `enumerate` `rev` `chunks` `windows` `partition`
- `sum` `product` `min` `max` `min_by_key` `max_by_key` `min_by` `max_by` `sorted`
- in place: `sort` `sort_by` `sort_by_key` `retain` `dedup` `reverse` `push` `pop` `insert` `remove` `extend` `truncate` `swap` `clear`
- `len` `is_empty` `contains` `get` `first` `last` `join` `concat` `flatten` `split_at`

`iter()`, `into_iter()`, `iter_mut()`, `collect()`, `cloned()` and `copied()` are all accepted, so Rust iterator chains work as written.

### `Option`, `Result` and `?`

```rust
fn parse_pair(a: &str, b: &str) -> Result<(i64, i64), String> {
    let x = a.parse::<i64>().map_err(|e| format!("bad a: {e}"))?;
    let y = b.parse::<i64>().map_err(|e| format!("bad b: {e}"))?;
    Ok((x, y))
}
```

Methods: `unwrap` `expect` `unwrap_or` `unwrap_or_else` `unwrap_or_default` `map` `map_err` `and_then` `or_else` `ok_or` `ok_or_else` `filter` `map_or` `is_some` `is_none` `is_ok` `is_err` `ok` `err`. `main` may return a `Result`.

### Collections

```rust
use std::collections::{HashMap, HashSet};

let mut ages = HashMap::new();
ages.insert("bob", 30);
ages["amy"] = 25;                              // index assignment inserts
if let Some(a) = ages.get("bob") { ... }
*ages.entry("bob").or_insert(0) += 1;
for (name, age) in &ages { ... }

let a: HashSet<i64> = HashSet::from([1, 2, 3]);
let both = a.intersection(&b);                 // also union, difference, &a | &b
```

### Strings

```rust
let s = String::from("Hello, World");
s.len()  s.to_uppercase()  s.trim()  s.contains("lo")  s.starts_with("He")
s.replace("World", "Tyfe")  s.split(", ")  s.split_whitespace()  s.lines()
s.find("lo")  s.chars().rev().collect::<String>()  &s[0..5]  s.repeat(2)
"42".parse::<i64>()  'x'.is_alphabetic()  '7'.to_digit(10)
let mut t = String::new(); t.push_str("abc"); t.push('d');
```

### Printing and formatting

```rust
println!("{} + {} = {}", 1, 2, 3);
println!("Hello {name}, you are {age}");         // inline variables
println!("{:?} {:#?}", vec![1], point);          // debug
println!("[{:>8}] [{:<8}] [{:^8}] [{:*^8}]", a, b, c, d);
println!("{:.2} {:08.3} {:+} {:e}", pi, pi, 5, 1500.0);
println!("{:x} {:#X} {:#b} {:o}", 255, 255, 5, 8);
let s = format!("{0}-{1}-{0}", "a", "b");
eprintln!("to stderr");
```

Macros: `println!` `print!` `eprintln!` `eprint!` `format!` `vec!` `panic!` `assert!` `assert_eq!` `assert_ne!` `matches!` `todo!` `unimplemented!` `unreachable!` `dbg!` `write!` `writeln!`.

### Numbers

```rust
7 as f64 / 2.0    3.9 as i64    'A' as u8    65 as char
x.abs()  x.pow(3)  x.min(y)  x.clamp(0, 9)  x.rem_euclid(5)  x.checked_add(y)
f.sqrt() f.floor() f.round() f.sin() f.powf(2.5) f.ln() f.to_degrees()
6 & 3   6 | 3   6 ^ 3   1 << 10   !0
i64::MAX   f64::EPSILON   std::f64::consts::PI
```

### Modules

```rust
// main.ty
mod geometry;               // loads geometry.ty (or geometry/mod.ty)
use geometry::Rect;

fn main() { println!("{}", geometry::area(&Rect::new(2.0, 3.0))); }
```

All modules share one namespace, so `geometry::area(...)` and `area(...)` both work. `pub` is accepted. Inline `mod name { ... }` blocks work too.

### Tests

```rust
#[test]
fn adds() {
    assert_eq!(add(2, 2), 4);
}
```

`tyfe test file.ty` runs every `#[test]` function and prints a report like `cargo test`.

### Built-in functions

| | |
|---|---|
| input | `input()` / `input("prompt")` / `read_line()`, `args()` |
| files | `read_file(path)` and `write_file(path, text)` return `Result`; also `append_file`, `file_exists`, `std::fs::read_to_string` |
| random & time | `random(lo, hi)` (hi exclusive), `random_float()`, `time_ms()`, `now()`, `sleep(ms)` |
| other | `exit(code)` / `std::process::exit`, `env_var(name)`, `type_of(x)`, `std::mem::swap(&mut a, &mut b)` |

## Error messages

Errors point at the exact spot, rustc-style, including errors inside modules:

```
runtime error: mismatched types: parameter `x` of `g` expects impl Shape, got Dog does not implement the trait `Shape`
 --> src/zoo.ty:14:5
  |
14 |     g(Dog);
  |     ^
```

## Project layout

```
src/
  main.rs         CLI, module loading, test runner, REPL
  lexer.rs        source text -> tokens
  parser.rs       tokens -> AST (recursive descent)
  ast.rs          syntax tree definitions
  interpreter.rs  runs the AST
  value.rs        runtime values
  builtins.rs     built-in functions, methods, and formatting
  error.rs        error type + pretty printing
examples/         sample programs (.ty) and their expected output (.out)
tests/            integration tests
```

## Roadmap ideas

- a static type checker that runs before the program does
- real shared references (`Rc<RefCell<T>>` semantics)
- iterator laziness (infinite ranges, `.iter().take(n)` on huge inputs)
- a bytecode VM for speed
- a package manager

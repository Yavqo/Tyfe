# Tyfe

Tyfe is a small programming language with Rust-like syntax. It has `fn`, `let` / `let mut`, type annotations, structs and arrays. The goal is to feel like Rust and C++ while staying easy to learn.

```rust
struct Point {
    x: float,
    y: float,
}

fn distance(a: Point, b: Point) -> float {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    return (dx * dx + dy * dy).sqrt();
}

fn main() {
    let origin = Point { x: 0.0, y: 0.0 };
    let p = Point { x: 3.0, y: 4.0 };
    println("distance = {}", distance(origin, p)); // distance = 5.0
}
```

Tyfe is currently a **tree-walking interpreter written in Rust**. A compiler backend can come later.

## Getting started

You need a Rust toolchain (`cargo`).

```sh
cargo build --release
./target/release/tyfe examples/hello.ty      # run a program
./target/release/tyfe check examples/arrays.ty   # syntax-check only
./target/release/tyfe                        # interactive REPL
cargo test                                   # run the test suite
```

Source files use the `.ty` extension. Every program needs a `fn main()`.

## Language tour

### Variables and types

```rust
let name = "Tyfe";        // immutable, type inferred (str)
let year: int = 2026;     // with a type annotation
let mut count = 0;        // `mut` makes it changeable
count += 1;               // also -=, *=, /=, %=
```

| Type        | Example                | Notes                                   |
|-------------|------------------------|-----------------------------------------|
| `int`       | `42`, `1_000_000`      | 64-bit signed; overflow is an error     |
| `float`     | `3.14`                 | 64-bit                                  |
| `bool`      | `true`, `false`        |                                         |
| `str`       | `"hi\n"`               | escapes: `\n \t \r \\ \" \0`            |
| `[T]`       | `[1, 2, 3]`            | arrays; all elements share one type     |
| `StructName`| `Point { x: 1.0, y: 2.0 }` | user-defined structs                |
| `()`        |                        | "nothing", returned by functions with no return type |

`i64`, `f64` and `String` work as aliases for `int`, `float` and `str`.

Like Rust, Tyfe is strict about types:
- `int` and `float` never mix implicitly. `1 + 2.0` is an error; write `float(1) + 2.0`.
- Conditions must be `bool`. `if 1 { }` is an error.
- A variable keeps its type. `let mut x = 5; x = "hi";` is an error.
- Assigning to a variable that isn't `mut` is an error.

### Functions

Parameters need types. A return type is optional; leave it out for `()`.

```rust
fn add(a: int, b: int) -> int {
    return a + b;
}

fn greet(name: str) {
    println("Hi, {}!", name);
}
```

Arguments are passed **by value** (copied). To modify a parameter inside the function, mark it `mut` and return the result:

```rust
fn double_all(mut xs: [int]) -> [int] {
    for i in 0..xs.len() {
        xs[i] *= 2;
    }
    return xs;
}
```

### Control flow

```rust
if x < 0 {
    println("negative");
} else if x == 0 {
    println("zero");
} else {
    println("positive");
}

while n > 0 { n -= 1; }

for i in 0..10 { }       // 0 to 9
for i in 1..=10 { }      // 1 to 10
for x in [1, 2, 3] { }   // each element
for c in "abc" { }       // each character

// `break` and `continue` work in every loop
```

### Arrays

```rust
let mut nums = [5, 3, 8];
nums.push(1);              // [5, 3, 8, 1]
nums[0] = 50;
let last = nums.pop();     // 1
println(nums.len());       // 3
println(nums.contains(8)); // true
let grid = [[1, 2], [3, 4]];
println(grid[1][0]);       // 3
```

Array methods: `len`, `is_empty`, `push`, `pop`, `insert(i, x)`, `remove(i)`, `clear`, `contains`, `reverse`, `join(sep)`. `a + b` joins two arrays.

### Strings

```rust
let s = "hello" + " world";
s.len()  s.to_upper()  s.to_lower()  s.trim()  s.contains("lo")
s.split(" ")  s.chars()  s.reverse()  s[0]
```

### Structs

```rust
struct Player { name: str, hp: int }

let mut p = Player { name: "Ferris", hp: 70 };
p.hp += 10;
println("{:?}", p);   // Player { name: "Ferris", hp: 80 }
```

### Printing and formatting

```rust
println("x = {}, y = {}", x, y);  // {} is a placeholder
println("{:?}", "quoted");        // {:?} is the debug view: "quoted"
println("{{literal braces}}");
print("no newline");
let s = format("{}-{}", a, b);    // builds a string
```

### Built-in functions

| Function | What it does |
|---|---|
| `print`, `println`, `eprintln` | print (with optional format string) |
| `format(fmt, ...)` | format into a `str` |
| `len(x)` | length of an array or string |
| `str(x)`, `int(x)`, `float(x)` | conversions (`int("42")` parses) |
| `input()` / `input("prompt")` | read a line from stdin |
| `abs`, `sqrt`, `min`, `max` | math (`x.pow(n)` also works) |
| `assert(cond)`, `assert_eq(a, b)`, `panic(msg)` | checks and errors |
| `type_of(x)` | the type name as a string |

### Comments

```rust
// line comment
/* block comment /* can nest */ */
```

## Error messages

Errors point at the exact spot, rustc-style:

```
runtime error: cannot mutate immutable variable `x` (declare it with `let mut x`)
 --> examples/oops.ty:3:5
  |
3 |     x = 2;
  |     ^
```

## Project layout

```
src/
  main.rs         CLI + REPL
  lexer.rs        source text -> tokens
  parser.rs       tokens -> AST (recursive descent)
  ast.rs          syntax tree definitions
  interpreter.rs  runs the AST
  value.rs        runtime values
  builtins.rs     built-in functions and methods
  error.rs        error type + pretty printing
examples/         sample programs (.ty) and their expected output (.out)
tests/            integration tests
```

## Roadmap ideas

- `impl` blocks and methods on structs
- `enum` and `match`
- references (`&`, `&mut`) instead of copy-only arguments
- a static type checker that runs before the program does
- a bytecode VM or native compilation for speed
- modules / `use`

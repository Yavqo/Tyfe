mod ast;
mod builtins;
mod error;
mod interpreter;
mod lexer;
mod parser;
mod value;

use interpreter::Interpreter;
use parser::ReplEntry;
use std::io::{self, BufRead, Read, Write};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
Tyfe - a small, Rust-flavored programming language

USAGE:
    tyfe                  start the interactive REPL
    tyfe <file.ty>        run a program (same as `tyfe run`)
    tyfe run <file.ty>    run a program (use `-` to read from stdin)
    tyfe check <file.ty>  parse a program and report syntax errors
    tyfe --version        print the version
    tyfe --help           print this help";

fn main() {
    // Tree-walking interpreters recurse deeply; give them a big stack.
    let child = std::thread::Builder::new()
        .stack_size(1024 * 1024 * 1024)
        .spawn(real_main)
        .expect("failed to spawn interpreter thread");
    let code = child.join().unwrap_or(101);
    std::process::exit(code);
}

fn real_main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(|s| s.as_str()).collect::<Vec<_>>().as_slice() {
        [] => repl(),
        ["-h" | "--help" | "help"] => {
            println!("{}", USAGE);
            0
        }
        ["-V" | "--version" | "version"] => {
            println!("tyfe {}", VERSION);
            0
        }
        ["run", path] => run_file(path),
        ["check", path] => check_file(path),
        [path] if !path.starts_with('-') => run_file(path),
        _ => {
            eprintln!("{}", USAGE);
            2
        }
    }
}

fn read_source(path: &str) -> Option<String> {
    let result = if path == "-" {
        let mut s = String::new();
        io::stdin().read_to_string(&mut s).map(|_| s)
    } else {
        std::fs::read_to_string(path)
    };
    match result {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("error: could not read `{}`: {}", path, e);
            None
        }
    }
}

fn run_file(path: &str) -> i32 {
    let Some(src) = read_source(path) else { return 1 };
    let name = if path == "-" { "<stdin>" } else { path };
    let mut interp = Interpreter::new();
    let result = parser::parse_program(&src)
        .and_then(|items| interp.load(items, false))
        .and_then(|_| interp.run_main());
    io::stdout().flush().ok();
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprint!("{}", e.render(name, &src));
            1
        }
    }
}

fn check_file(path: &str) -> i32 {
    let Some(src) = read_source(path) else { return 1 };
    match parser::parse_program(&src).and_then(|items| Interpreter::new().load(items, false)) {
        Ok(()) => {
            println!("{}: ok", path);
            0
        }
        Err(e) => {
            eprint!("{}", e.render(path, &src));
            1
        }
    }
}

/// True once every `(`, `[` and `{` typed so far has been closed.
fn is_complete(src: &str) -> bool {
    let mut depth: i32 = 0;
    let mut chars = src.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            match c {
                '\\' => {
                    chars.next();
                }
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '/' if chars.peek() == Some(&'/') => {
                while let Some(&c) = chars.peek() {
                    if c == '\n' {
                        break;
                    }
                    chars.next();
                }
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    depth <= 0 && !in_str
}

fn repl() -> i32 {
    println!("Tyfe {} REPL. Type :help for help, :quit to exit.", VERSION);
    let mut interp = Interpreter::new();
    let stdin = io::stdin();
    let mut buf = String::new();
    loop {
        print!("{}", if buf.is_empty() { "tyfe> " } else { "  ... " });
        io::stdout().flush().ok();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => {
                println!();
                return 0;
            }
            Ok(_) => {}
        }
        if buf.is_empty() {
            match line.trim() {
                "" => continue,
                ":q" | ":quit" | ":exit" => return 0,
                ":help" | ":h" => {
                    println!("Enter statements (`let x = 5;`), expressions (`x * 2`),");
                    println!("or definitions (`fn sq(n: int) -> int {{ return n * n; }}`).");
                    println!("Multi-line input continues until all brackets are closed.");
                    println!(":quit exits.");
                    continue;
                }
                _ => {}
            }
        }
        buf.push_str(&line);
        if !is_complete(&buf) {
            continue;
        }
        let src = std::mem::take(&mut buf);
        let result = parser::parse_repl(&src).and_then(|entries| {
            for entry in entries {
                match entry {
                    ReplEntry::Item(item) => interp.load(vec![item], true)?,
                    ReplEntry::Stmt(stmt) => interp.exec_repl(&stmt)?,
                }
            }
            Ok(())
        });
        io::stdout().flush().ok();
        if let Err(e) = result {
            eprint!("{}", e.render("<repl>", &src));
        }
    }
}

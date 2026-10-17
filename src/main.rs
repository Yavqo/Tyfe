mod ast;
mod builtins;
mod error;
mod interpreter;
mod lexer;
mod parser;
mod value;

use ast::Item;
use error::Error;
use interpreter::Interpreter;
use parser::ReplEntry;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
Tyfe - a small, Rust-flavored programming language

USAGE:
    tyfe                          start the interactive REPL
    tyfe <file.ty> [args...]      run a program (same as `tyfe run`)
    tyfe run <file.ty> [args...]  run a program (use `-` to read from stdin)
    tyfe test <file.ty>           run the program's #[test] functions
    tyfe check <file.ty>          parse a program and report errors
    tyfe --version                print the version
    tyfe --help                   print this help";

fn main() {
    // Tree-walking interpreters recurse deeply; give them a big stack.
    let child = std::thread::Builder::new()
        .stack_size(interpreter::STACK_SIZE)
        .spawn(|| {
            interpreter::mark_stack_base();
            real_main()
        })
        .expect("failed to spawn interpreter thread");
    let code = child.join().unwrap_or(101);
    std::process::exit(code);
}

fn real_main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = |n: usize| args.get(n..).unwrap_or(&[]).to_vec();
    match args.first().map(|s| s.as_str()) {
        None => repl(),
        Some("-h" | "--help" | "help") => {
            println!("{}", USAGE);
            0
        }
        Some("-V" | "--version" | "version") => {
            println!("tyfe {}", VERSION);
            0
        }
        Some("run") if args.len() >= 2 => run_file(&args[1], rest(2)),
        Some("test") if args.len() == 2 => test_file(&args[1]),
        Some("check") if args.len() == 2 => check_file(&args[1]),
        Some(path) if !path.starts_with('-') && !matches!(path, "run" | "test" | "check") => run_file(path, rest(1)),
        _ => {
            eprintln!("{}", USAGE);
            2
        }
    }
}

/// All source files of a program, so errors can show the right file.
struct Sources {
    files: Vec<(String, String)>,
}

impl Sources {
    fn report(&self, e: &Error) {
        let (name, src) = self.files.get(e.span.file as usize).map(|(n, s)| (n.as_str(), s.as_str())).unwrap_or(("?", ""));
        eprint!("{}", e.render(name, src));
    }
}

fn read_source(path: &str) -> std::result::Result<String, String> {
    if path == "-" {
        let mut s = String::new();
        io::stdin().read_to_string(&mut s).map_err(|e| e.to_string())?;
        Ok(s)
    } else {
        std::fs::read_to_string(path).map_err(|e| e.to_string())
    }
}

/// Parse the main file plus every `mod name;` it (recursively) declares.
fn load_program(path: &str, sources: &mut Sources) -> Result<Vec<Item>, i32> {
    let src = match read_source(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not read `{}`: {}", path, e);
            return Err(1);
        }
    };
    let name = if path == "-" { "<stdin>".to_string() } else { path.to_string() };
    let dir = Path::new(path).parent().map(Path::to_path_buf).unwrap_or_default();
    sources.files.push((name, src.clone()));
    let mut seen = Vec::new();
    parse_with_modules(&src, 0, &dir, sources, &mut seen).map_err(|e| {
        sources.report(&e);
        1
    })
}

fn parse_with_modules(
    src: &str,
    file: u16,
    dir: &Path,
    sources: &mut Sources,
    seen: &mut Vec<PathBuf>,
) -> Result<Vec<Item>, Error> {
    let mut out = Vec::new();
    for item in parser::parse_program(src, file)? {
        let Item::Mod(name, span) = item else {
            out.push(item);
            continue;
        };
        let candidates = [dir.join(format!("{}.ty", name)), dir.join(&name).join("mod.ty")];
        let Some(mod_path) = candidates.iter().find(|p| p.exists()).cloned() else {
            return Err(Error::syntax(
                span,
                format!("cannot find module `{}`: expected `{}`", name, candidates[0].display()),
            ));
        };
        let canonical = mod_path.canonicalize().unwrap_or(mod_path.clone());
        if seen.contains(&canonical) {
            continue; // each module is loaded once
        }
        seen.push(canonical);
        let mod_src = std::fs::read_to_string(&mod_path)
            .map_err(|e| Error::syntax(span, format!("could not read `{}`: {}", mod_path.display(), e)))?;
        let id = sources.files.len() as u16;
        sources.files.push((mod_path.display().to_string(), mod_src.clone()));
        let mod_dir = mod_path.parent().map(Path::to_path_buf).unwrap_or_default();
        out.extend(parse_with_modules(&mod_src, id, &mod_dir, sources, seen)?);
    }
    Ok(out)
}

fn run_file(path: &str, program_args: Vec<String>) -> i32 {
    let mut sources = Sources { files: Vec::new() };
    let items = match load_program(path, &mut sources) {
        Ok(items) => items,
        Err(code) => return code,
    };
    let mut interp = Interpreter::new(program_args);
    let result = interp.load(items, false).and_then(|_| interp.run_main());
    io::stdout().flush().ok();
    match result {
        Ok(()) => 0,
        Err(e) => {
            sources.report(&e);
            1
        }
    }
}

fn test_file(path: &str) -> i32 {
    let mut sources = Sources { files: Vec::new() };
    let items = match load_program(path, &mut sources) {
        Ok(items) => items,
        Err(code) => return code,
    };
    let mut interp = Interpreter::new(Vec::new());
    if let Err(e) = interp.load(items, false) {
        sources.report(&e);
        return 1;
    }
    let tests = interp.test_names();
    println!("running {} test{}", tests.len(), if tests.len() == 1 { "" } else { "s" });
    let mut failures = Vec::new();
    for name in &tests {
        match interp.run_test(name) {
            Ok(()) => println!("test {} ... ok", name),
            Err(e) => {
                println!("test {} ... FAILED", name);
                failures.push((name.clone(), e));
            }
        }
        io::stdout().flush().ok();
    }
    if !failures.is_empty() {
        println!("\nfailures:");
        for (name, e) in &failures {
            println!("\n---- {} ----", name);
            io::stdout().flush().ok();
            sources.report(e);
        }
    }
    let passed = tests.len() - failures.len();
    println!(
        "\ntest result: {}. {} passed; {} failed",
        if failures.is_empty() { "ok" } else { "FAILED" },
        passed,
        failures.len()
    );
    if failures.is_empty() {
        0
    } else {
        1
    }
}

fn check_file(path: &str) -> i32 {
    let mut sources = Sources { files: Vec::new() };
    let items = match load_program(path, &mut sources) {
        Ok(items) => items,
        Err(code) => return code,
    };
    match Interpreter::new(Vec::new()).load(items, false) {
        Ok(()) => {
            println!("{}: ok", path);
            0
        }
        Err(e) => {
            sources.report(&e);
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
    let mut interp = Interpreter::new(Vec::new());
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
                    println!("or definitions (`fn sq(n: int) -> int {{ n * n }}`).");
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

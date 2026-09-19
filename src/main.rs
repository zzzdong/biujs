use biujs::Compiler;
use biujs::VM;
use std::io::{self, BufRead, Write};

fn main() {
    env_logger::init();

    let args = std::env::args().collect::<Vec<_>>();
    if args.len() > 1 {
        if args[1] == "-h" || args[1] == "--help" {
            eprintln!("Usage: biujs [script_file]");
            return;
        }

        // Run a script file. Errors are reported and turned into a non-zero
        // exit status instead of panicking.
        let script_file = &args[1];
        let content = match std::fs::read_to_string(script_file) {
            Ok(content) => content,
            Err(err) => {
                eprintln!("biujs: cannot read {script_file}: {err}");
                std::process::exit(2);
            }
        };

        let mut compiler = Compiler::new();
        let module = match compiler.compile(&content) {
            Ok(module) => module,
            Err(err) => {
                eprintln!("Compile error: {err}");
                std::process::exit(1);
            }
        };

        if std::env::var("BIUJS_DUMP").is_ok() {
            eprintln!("{module}");
        }

        let mut vm = VM::new();
        match vm.run(&module) {
            Ok(result) => println!("{result}"),
            Err(err) => {
                eprintln!("Runtime error: {err}");
                std::process::exit(1);
            }
        }

        return;
    }

    println!("BiuJS - JavaScript Engine v0.1.0");
    println!("Type JavaScript code to execute, or 'exit' to quit.");
    println!();

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut handle = stdout.lock();

    let mut compiler = Compiler::new();
    let mut vm = VM::new();

    loop {
        write!(handle, ">>> ").unwrap();
        handle.flush().unwrap();

        let mut line = String::new();
        let mut reader = stdin.lock();
        if reader.read_line(&mut line).unwrap() == 0 {
            break;
        }

        let line = line.trim();
        if line == "exit" || line == "quit" {
            break;
        }

        if line.is_empty() {
            continue;
        }

        match compiler.compile(line) {
            Ok(module) => match vm.run(&module) {
                Ok(result) => {
                    if !result.is_undefined() {
                        println!("{}", result);
                    }
                }
                Err(e) => {
                    eprintln!("Runtime error: {}", e);
                }
            },
            Err(e) => {
                eprintln!("Compile error: {}", e);
            }
        }
    }
}

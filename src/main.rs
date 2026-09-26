use biujs::Compiler;
use biujs::VM;
use std::io::{self, BufRead, Write};
use std::time::Duration;

/// A VM carrying the wall-clock guard, whose budget comes from
/// `BIUJS_TIMEOUT_MS` (0 = no limit) and defaults to `fallback_ms`.
///
/// `VM::run` arms the deadline on every call, so one VM can serve a whole REPL
/// session without the budget leaking between lines.
fn configured_vm(fallback_ms: u64) -> VM {
    let ms: u64 = std::env::var("BIUJS_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(fallback_ms);
    if ms == 0 {
        VM::new()
    } else {
        VM::new().with_timeout(Some(Duration::from_millis(ms)))
    }
}

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

        // A counted step budget does not bound *wall-clock* time: a single step
        // can sit in a native routine for a long while. The timed guard is what
        // keeps `biujs runaway.js` from hanging the caller forever;
        // `BIUJS_TIMEOUT_MS=0` turns it off for genuinely long-running scripts.
        let mut vm = configured_vm(30_000);
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
    // The REPL gets its own (shorter) budget: a typed-in infinite loop should
    // come back with an error, not swallow the session.
    let mut vm = configured_vm(10_000);

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

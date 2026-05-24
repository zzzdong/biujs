use std::io::{self, BufRead, Write};
use biujs::Compiler;
use biujs::VM;

fn main() {
    env_logger::init();
    
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
            Ok(module) => {
                match vm.run(&module) {
                    Ok(result) => {
                        if !result.is_undefined() {
                            println!("{}", result);
                        }
                    }
                    Err(e) => {
                        eprintln!("Runtime error: {}", e);
                    }
                }
            }
            Err(e) => {
                eprintln!("Compile error: {}", e);
            }
        }
    }
}
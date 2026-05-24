//! Simplified test262 runner for biujs
//!
//! This runner directly executes test262 test files without using harness files.
//! It focuses on tests that don't require assert.js/sta.js infrastructure.

use biujs::{Compiler, VM};
use std::fs;
use std::path::PathBuf;

/// Root directory for test262 tests
fn test262_test_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/test262/test")
}

/// Check if test262 directory exists
fn has_tests() -> bool {
    test262_test_dir().exists() && test262_test_dir().join("language").exists()
}

/// Extract the code portion from a test file, stripping the YAML metadata
fn extract_test_code(source: &str) -> String {
    // Look for /*--- ... ---*/ pattern
    if source.find("/*---").is_some() {
        if let Some(end) = source.find("---*/") {
            let after_metadata = &source[end + 5..];
            return after_metadata.trim().to_string();
        }
    }
    source.to_string()
}

/// Parse basic metadata from test file
struct TestMeta {
    description: Option<String>,
    negative: bool,
    negative_type: Option<String>,
    flags: Vec<String>,
    features: Vec<String>,
    includes: Vec<String>,
}

fn parse_meta(source: &str) -> TestMeta {
    let mut meta = TestMeta {
        description: None,
        negative: false,
        negative_type: None,
        flags: Vec::new(),
        features: Vec::new(),
        includes: Vec::new(),
    };

    if let Some(start) = source.find("/*---") {
        if let Some(end) = source.find("---*/") {
            let yaml = &source[start + 5..end];

            for line in yaml.lines() {
                let line = line.trim();
                if line.starts_with("description:") {
                    meta.description = Some(line["description:".len()..].trim().to_string());
                } else if line.starts_with("negative:") {
                    meta.negative = true;
                } else if line.starts_with("type:") && meta.negative {
                    meta.negative_type = Some(line["type:".len()..].trim().to_string());
                } else if line.starts_with("flags:") {
                    // Parse flags array like [onlyStrict, noStrict]
                    let flags_str = line["flags:".len()..].trim();
                    if flags_str.starts_with('[') && flags_str.ends_with(']') {
                        let inner = &flags_str[1..flags_str.len() - 1];
                        for flag in inner.split(',') {
                            meta.flags.push(flag.trim().to_string());
                        }
                    }
                } else if line.starts_with("features:") {
                    let features_str = line["features:".len()..].trim();
                    if features_str.starts_with('[') && features_str.ends_with(']') {
                        let inner = &features_str[1..features_str.len() - 1];
                        for feat in inner.split(',') {
                            meta.features.push(feat.trim().to_string());
                        }
                    }
                } else if line.starts_with("includes:") {
                    let includes_str = line["includes:".len()..].trim();
                    if includes_str.starts_with('[') && includes_str.ends_with(']') {
                        let inner = &includes_str[1..includes_str.len() - 1];
                        for inc in inner.split(',') {
                            meta.includes.push(inc.trim().to_string());
                        }
                    }
                }
            }
        }
    }

    meta
}

/// Check if a test should be skipped
fn should_skip(meta: &TestMeta) -> Option<String> {
    // Skip tests that require harness includes
    if !meta.includes.is_empty() {
        return Some(format!("requires harness: {:?}", meta.includes));
    }

    // Skip tests with unsupported flags
    for flag in &meta.flags {
        match flag.as_str() {
            "async" | "module" | "generated" => {
                return Some(format!("unsupported flag: {}", flag));
            }
            _ => {}
        }
    }

    // Skip tests with unsupported features
    let unsupported_features = [
        "async-iteration",
        "async-functions",
        "generators",
        "modules",
        "class",
        "arrow-function",
        "destructuring-binding",
        "for-of",
        "Symbol",
        "template",
        "let",       // Some let tests may work, but skip for safety
        "const",
        "default-parameters",
        "Proxy",
        "Promise",
        "Map",
        "Set",
        "WeakRef",
        "WeakMap",
        "WeakSet",
    ];

    for feature in &meta.features {
        for uf in &unsupported_features {
            if feature.contains(uf) {
                return Some(format!("unsupported feature: {}", feature));
            }
        }
    }

    None
}

/// Run a test file and return Ok(()) on success, Err(msg) on failure
fn run_test_file(path: &PathBuf) -> Result<(), String> {
    let source = fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;

    let meta = parse_meta(&source);
    let code = extract_test_code(&source);

    if code.is_empty() {
        return Err("Empty test code".to_string());
    }

    let mut compiler = Compiler::new();
    match compiler.compile(&code) {
        Ok(module) => {
            let mut vm = VM::new();
            match vm.run(&module) {
                Ok(_) => {
                    if meta.negative {
                        Err("Expected error but test succeeded".to_string())
                    } else {
                        Ok(())
                    }
                }
                Err(e) => {
                    if meta.negative {
                        Ok(()) // Expected failure
                    } else {
                        Err(format!("Runtime error: {}", e))
                    }
                }
            }
        }
        Err(e) => {
            if meta.negative {
                Ok(()) // Expected failure
            } else {
                Err(format!("Compilation error: {}", e))
            }
        }
    }
}

/// Recursively find all .js test files
fn find_test_files(dir: &PathBuf, files: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                find_test_files(&path, files);
            } else if path.extension().map_or(false, |ext| ext == "js") {
                if !path
                    .file_name()
                    .map_or(false, |name| name.to_string_lossy().contains("_FIXTURE"))
                {
                    files.push(path);
                }
            }
        }
    }
}

#[test]
fn test262_conformance() {
    if !has_tests() {
        eprintln!("test262 test directory not found at {:?}, skipping", test262_test_dir());
        eprintln!("To run test262 tests, create test files in tests/test262/test/language/");
        return;
    }

    let mut test_files = Vec::new();
    find_test_files(&test262_test_dir(), &mut test_files);

    let mut total = 0u32;
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let mut failures: Vec<(String, String)> = Vec::new();

    // Limit number of tests to run (test262 has 50000+ tests)
    const MAX_TESTS: u32 = 500;

    for path in &test_files {
        if total >= MAX_TESTS {
            break;
        }
        total += 1;

        let source = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                failed += 1;
                failures.push((path.display().to_string(), format!("Read error: {}", e)));
                continue;
            }
        };

        let meta = parse_meta(&source);

        if let Some(_reason) = should_skip(&meta) {
            skipped += 1;
            continue;
        }

        // Skip tests that require harness infrastructure or unsupported features
        let code = extract_test_code(&source);
        if code.contains("assert") || code.contains("$ERROR") || code.contains("Test262Error")
            || code.contains("Function") || code.contains("new ") || code.contains("throw")
            || code.contains("try ") || code.contains("catch") || code.contains("class ")
        {
            skipped += 1;
            continue;
        }

        match run_test_file(path) {
            Ok(()) => passed += 1,
            Err(msg) => {
                failed += 1;
                let desc = meta
                    .description
                    .unwrap_or_else(|| path.file_name().unwrap().to_string_lossy().to_string());
                failures.push((desc, msg));
            }
        }
    }

    // Print summary
    println!("\n=== test262 Conformance Results ===");
    println!("Total:   {}", total);
    println!("Passed:  {}", passed);
    println!("Failed:  {}", failed);
    println!("Skipped: {}", skipped);

    if !failures.is_empty() {
        println!("\n--- Failures ---");
        for (desc, msg) in &failures {
            println!("  FAIL: {} - {}", desc, msg);
        }
    }

    if total == 0 {
        println!("\nNo tests found. Create test files in tests/test262/test/language/");
    }
}

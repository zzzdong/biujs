//! Golden tests for the bytecode dumper.
//!
//! Each `tests/golden/bytecode/*.js` is compiled and its deterministic dump is
//! compared with the matching `.dump` file. The dumper is intentionally
//! bytecode-only (no IR annotations), so a golden diff means the emitted
//! bytecode changed — which is exactly what a compiler refactor must make
//! visible.
//!
//! Regenerate after an intentional bytecode change:
//!
//! ```sh
//! UPDATE_GOLDEN=1 cargo test --release --test bytecode_dump
//! ```
//!
//! Then review the diff: a dump change is a contract change, not noise.

use std::fs;
use std::path::Path;

use biujs::Compiler;

#[test]
fn bytecode_dump_matches_golden_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/bytecode");
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    let mut checked = 0;

    let mut sources: Vec<_> = fs::read_dir(&root)
        .expect("tests/golden/bytecode must exist")
        .map(|entry| entry.expect("read_dir entry").path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("js"))
        .collect();
    sources.sort();

    for source_path in sources {
        let source = fs::read_to_string(&source_path)
            .unwrap_or_else(|err| panic!("cannot read {}: {err}", source_path.display()));
        let module = Compiler::new()
            .compile(&source)
            .unwrap_or_else(|err| panic!("{} does not compile: {err}", source_path.display()));
        let actual = module.dump_bytecode();
        let golden_path = source_path.with_extension("dump");

        if update {
            fs::write(&golden_path, &actual)
                .unwrap_or_else(|err| panic!("cannot write {}: {err}", golden_path.display()));
        }

        let expected = fs::read_to_string(&golden_path).unwrap_or_else(|err| {
            panic!(
                "missing golden file {} ({err}); run with UPDATE_GOLDEN=1",
                golden_path.display()
            )
        });
        assert_eq!(
            actual,
            expected,
            "bytecode dump changed for {}; re-run with UPDATE_GOLDEN=1 after reviewing",
            source_path.display()
        );
        checked += 1;
    }

    assert!(checked >= 10, "expected at least 10 golden sources, got {checked}");
}

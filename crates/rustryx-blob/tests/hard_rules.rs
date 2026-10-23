//! CONTRIBUTING hard rules 1 and 2 as a test: the production sources of this crate
//! must not contain the constructs those rules forbid in async code. The scan
//! ignores comments, so documentation may explain why a construct is avoided.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

const FORBIDDEN: &[(&str, &str)] = &[
    (
        "spawn_blocking",
        "hashing and file work must not hop to the blocking pool by hand",
    ),
    ("std::fs", "use tokio::fs in async code"),
    ("block_on", "never block a worker on a future"),
    ("thread::sleep", "never sleep a worker thread"),
    (
        "to_vec()",
        "artifact bodies are never collected into a Vec<u8>",
    ),
    (
        "Vec<u8>",
        "artifact bodies are never collected into a Vec<u8>",
    ),
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Production code of a file: everything before its inline `#[cfg(test)]` module,
/// with comments removed.
fn production_code(source: &str) -> String {
    let end = source.find("#[cfg(test)]").unwrap_or(source.len());
    source[..end]
        .lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

fn found_in(code: &str) -> Vec<&'static str> {
    FORBIDDEN
        .iter()
        .filter(|(needle, _)| code.contains(needle))
        .map(|(needle, _)| *needle)
        .collect()
}

#[test]
fn the_scan_itself_detects_each_forbidden_construct() {
    // A scan that cannot fail proves nothing, so check it against known offenders.
    for (needle, _) in FORBIDDEN {
        let offender = production_code(&format!("fn f() {{ let _ = {needle}; }}"));
        assert_eq!(found_in(&offender), vec![*needle], "scan missed `{needle}`");
    }
    let commented = production_code("// std::fs is mentioned only in a comment\nfn f() {}");
    assert_eq!(found_in(&commented), Vec::<&str>::new());
    let after_tests =
        production_code("fn f() {}\n#[cfg(test)]\nmod t { fn g() { std::fs::read(\"x\"); } }");
    assert_eq!(found_in(&after_tests), Vec::<&str>::new());
}

#[test]
fn must_not_use_blocking_constructs_in_production_code() {
    let mut files = Vec::new();
    rust_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(files.len() >= 10, "scan found only {} files", files.len());
    let mut violations = Vec::new();
    for file in files {
        // Test-only modules: `tests.rs` files and the `tests/` directories next to them.
        let in_test_module = file.file_name().is_some_and(|name| name == "tests.rs")
            || file.components().any(|part| part.as_os_str() == "tests");
        if in_test_module {
            continue;
        }
        let code = production_code(&std::fs::read_to_string(&file).unwrap());
        for needle in found_in(&code) {
            let why = FORBIDDEN
                .iter()
                .find(|(n, _)| *n == needle)
                .map_or("", |(_, why)| *why);
            violations.push(format!("{}: `{needle}` ({why})", file.display()));
        }
    }
    assert!(
        violations.is_empty(),
        "forbidden constructs:\n{}",
        violations.join("\n")
    );
}

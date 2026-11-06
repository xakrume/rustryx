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

/// Production code of a file: everything before its inline `#[cfg(test)] mod`, with comments
/// removed. Only a test MODULE ends the production code: a test-only helper in the middle of
/// a file must not hide the rest of the file from the scan.
fn production_code(source: &str) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let end = lines
        .iter()
        .enumerate()
        .find(|(i, line)| {
            line.trim() == "#[cfg(test)]"
                && lines.get(i + 1).is_some_and(|next| {
                    let next = next.trim_start();
                    next.starts_with("mod ")
                        || next.starts_with("pub mod ")
                        || next.starts_with("pub(crate) mod ")
                })
        })
        .map_or(lines.len(), |(i, _)| i);
    lines[..end]
        .iter()
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
    // A test-only helper in the middle of a file does not end the production code.
    let helper_then_offender = production_code(
        "#[cfg(test)]\nfn helper() {}\nfn f() { let _ = std::fs::read(\"x\"); }\n#[cfg(test)]\nmod t {}",
    );
    assert_eq!(found_in(&helper_then_offender), vec!["std::fs"]);
}

/// Threads. The only thread this crate owns is the disk monitor (`disk.rs`), which exists
/// because `statvfs` has no async form and must not occupy the blocking pool. Any other
/// thread would be a fire-and-forget task in disguise: nothing waits for it at shutdown.
const THREAD_CONSTRUCTS: &[&str] = &["std::thread", "thread::Builder", "thread::spawn"];

fn threads_in(code: &str) -> Vec<&'static str> {
    THREAD_CONSTRUCTS
        .iter()
        .filter(|needle| code.contains(**needle))
        .copied()
        .collect()
}

#[test]
fn the_thread_scan_detects_each_construct_and_ignores_comments() {
    for needle in THREAD_CONSTRUCTS {
        let offender = production_code(&format!("fn f() {{ let _ = {needle}; }}"));
        assert!(
            threads_in(&offender).contains(needle),
            "scan missed `{needle}`"
        );
    }
    let commented = production_code("// std::thread is only mentioned here\nfn f() {}");
    assert_eq!(threads_in(&commented), Vec::<&str>::new());
}

#[test]
fn must_not_start_threads_outside_the_disk_monitor() {
    let mut files = Vec::new();
    rust_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let mut violations = Vec::new();
    let mut monitor_found = false;
    for file in files {
        let in_test_module = file.file_name().is_some_and(|name| name == "tests.rs")
            || file.components().any(|part| part.as_os_str() == "tests");
        if in_test_module {
            continue;
        }
        let found = threads_in(&production_code(&std::fs::read_to_string(&file).unwrap()));
        if file.file_name().is_some_and(|name| name == "disk.rs") {
            monitor_found = !found.is_empty();
        } else if !found.is_empty() {
            violations.push(format!("{}: {found:?}", file.display()));
        }
    }
    assert!(
        monitor_found,
        "the scan must see the monitor thread in disk.rs, or it checks nothing"
    );
    assert!(
        violations.is_empty(),
        "threads outside the disk monitor:\n{}",
        violations.join("\n")
    );
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
    let mut dedup_found = false;
    for file in files {
        // Test-only modules: `tests.rs` files and the `tests/` directories next to them.
        let in_test_module = file.file_name().is_some_and(|name| name == "tests.rs")
            || file.components().any(|part| part.as_os_str() == "tests");
        if in_test_module {
            continue;
        }
        let code = production_code(&std::fs::read_to_string(&file).unwrap());
        let found = found_in(&code);
        // Sync batch hardlink pass (`dedup_directory`): not on the async request path,
        // same exception shape as the disk monitor for threads.
        if file.file_name().is_some_and(|name| name == "dedup.rs") {
            dedup_found = found.contains(&"std::fs");
            let other: Vec<_> = found.into_iter().filter(|n| *n != "std::fs").collect();
            for needle in other {
                let why = FORBIDDEN
                    .iter()
                    .find(|(n, _)| *n == needle)
                    .map_or("", |(_, why)| *why);
                violations.push(format!("{}: `{needle}` ({why})", file.display()));
            }
            continue;
        }
        for needle in found {
            let why = FORBIDDEN
                .iter()
                .find(|(n, _)| *n == needle)
                .map_or("", |(_, why)| *why);
            violations.push(format!("{}: `{needle}` ({why})", file.display()));
        }
    }
    assert!(
        dedup_found,
        "the scan must see std::fs in dedup.rs, or the allowlist checks nothing"
    );
    assert!(
        violations.is_empty(),
        "forbidden constructs:\n{}",
        violations.join("\n")
    );
}

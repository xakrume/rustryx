//! Entry points of the fuzz targets (`fuzz/fuzz_targets`), kept in the crate so that the same
//! code runs under `cargo fuzz` (nightly, libFuzzer) and under plain `cargo test`, which replays
//! the seed corpus (`tests/fuzz_corpus.rs`) on every CI run. A target is a one line call here.
//!
//! Each function takes arbitrary bytes, must never panic, and asserts the invariant of the
//! parser it exercises: a panic from an `assert!` is a finding.

use std::cmp::Ordering;

use crate::checksum::parse_checksum_file;
use crate::layout::{ChecksumAlgo, MavenPath};
use crate::metadata::{merge_documents, parse_metadata, write_metadata};
use crate::version::{compare_versions, sort_versions};

/// `maven-metadata.xml` parsing: no panic, and a document that parses has one canonical form
/// (`write(parse(write(m))) == write(m)`).
pub fn metadata_parse(data: &[u8]) {
    let Ok(parsed) = parse_metadata(data) else {
        return;
    };
    let once = write_metadata(&parsed);
    let Ok(reparsed) = parse_metadata(once.as_bytes()) else {
        panic_invariant("a written document must parse");
    };
    assert_eq!(write_metadata(&reparsed), once, "the writer is canonical");
}

/// Layout paths: no panic, and an accepted path is its own canonical spelling.
pub fn layout_parse(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(parsed) = MavenPath::parse(text) {
        assert_eq!(
            parsed.to_path_string(),
            text,
            "an accepted path has one spelling"
        );
        let _ = parsed.coordinate();
        let _ = parsed.group_and_artifact();
    }
}

/// Version ordering: no panic, antisymmetry, and sorting terminates. Input is split into
/// versions at NUL bytes.
pub fn version_order(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    let mut versions: Vec<String> = text.split('\0').take(16).map(str::to_owned).collect();
    if let [a, b, ..] = versions.as_slice() {
        let forward = compare_versions(a, b);
        let backward = compare_versions(b, a);
        assert_eq!(forward, backward.reverse(), "antisymmetric");
        if forward == Ordering::Equal {
            assert_eq!(compare_versions(a, a), Ordering::Equal);
        }
    }
    sort_versions(&mut versions);
}

/// Merging member documents (split at NUL bytes, at most six) at every level: no panic, and a
/// merged document is itself parseable.
pub fn merge(data: &[u8]) {
    let documents: Vec<_> = data
        .split(|byte| *byte == 0)
        .take(6)
        .filter_map(|part| parse_metadata(part).ok())
        .collect();
    for path in [
        "org/acme/lib/maven-metadata.xml",
        "org/acme/lib/1.0-SNAPSHOT/maven-metadata.xml",
        "junit/maven-metadata.xml",
    ] {
        let Ok(MavenPath::Metadata { level, .. }) = MavenPath::parse(path) else {
            continue;
        };
        if let Some(merged) = merge_documents(&level, &documents) {
            let text = write_metadata(&merged);
            assert!(
                parse_metadata(text.as_bytes()).is_ok(),
                "a merged document parses"
            );
        }
    }
}

/// Supplied checksum files: no panic for any algorithm.
pub fn checksum_file(data: &[u8]) {
    for algo in ChecksumAlgo::ALL {
        let _ = parse_checksum_file(data, algo);
    }
}

/// A broken invariant that cannot be expressed as an `assert!` on a comparison.
#[allow(
    clippy::panic,
    reason = "fuzz support: a panic is exactly the signal the fuzzer and the corpus replay test look for"
)]
fn panic_invariant(message: &str) -> ! {
    panic!("{message}")
}

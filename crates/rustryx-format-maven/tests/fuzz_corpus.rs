//! Replays the seed corpus of every fuzz target through the same entry points `cargo fuzz`
//! uses, on stable and on every CI run, so a regression the nightly fuzz job would find on a
//! known input fails an ordinary `cargo test` first.
#![allow(
    clippy::unwrap_used,
    clippy::unused_self,
    clippy::format_collect,
    clippy::duration_suboptimal_units,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::large_futures,
    clippy::format_push_string,
    clippy::case_sensitive_file_extension_comparisons,
    reason = "tests: a panic is the failure signal and scenarios read best as straight-line code"
)]

use std::path::Path;

use rustryx_format_maven::fuzzing;

fn replay(target: &str, entry: fn(&[u8])) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fuzz/corpus")
        .join(target);
    let mut count = 0;
    for file in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let path = file.unwrap().path();
        let data = std::fs::read(&path).unwrap();
        entry(&data);
        count += 1;
    }
    assert!(
        count >= 5,
        "the {target} corpus has {count} seeds, expected at least 5"
    );
}

#[test]
fn metadata_parse_corpus() {
    replay("metadata_parse", fuzzing::metadata_parse);
}

#[test]
fn layout_parse_corpus() {
    replay("layout_parse", fuzzing::layout_parse);
}

#[test]
fn version_order_corpus() {
    replay("version_order", fuzzing::version_order);
}

#[test]
fn merge_corpus() {
    replay("merge", fuzzing::merge);
}

#[test]
fn checksum_file_corpus() {
    replay("checksum_file", fuzzing::checksum_file);
}

#[test]
fn truncations_and_single_byte_flips_of_every_metadata_seed_do_not_panic() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/corpus/metadata_parse");
    for file in std::fs::read_dir(dir).unwrap() {
        let data = std::fs::read(file.unwrap().path()).unwrap();
        for cut in (0..data.len()).step_by(7) {
            fuzzing::metadata_parse(&data[..cut]);
        }
        for index in (0..data.len()).step_by(11) {
            let mut flipped = data.clone();
            flipped[index] ^= 0x5a;
            fuzzing::metadata_parse(&flipped);
            fuzzing::merge(&flipped);
        }
    }
}

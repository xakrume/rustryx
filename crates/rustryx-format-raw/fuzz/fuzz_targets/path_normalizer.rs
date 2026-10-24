//! Feeds arbitrary request paths to the raw path normalizer and the side file recognizer.
//!
//! The properties are the ones the format relies on for SEC-19 and SEC-31: the parser never
//! panics, and whatever it accepts has no dot segment, no separator inside a segment, no
//! control or bidirectional character and a bounded depth.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rustryx_format_raw::target::MAX_DEPTH;
use rustryx_format_raw::{Target, parse_target, sidecar::sidecar_of};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(target) = parse_target(text) else {
        return;
    };
    let path = target.path();
    assert!(path.segments().count() <= MAX_DEPTH);
    for segment in path.segments() {
        assert!(segment != "." && segment != "..");
        assert!(!segment.contains(['/', '\\', '\0']));
        assert!(!segment.chars().any(char::is_control));
        assert!(!segment.chars().any(|c| ('\u{202A}'..='\u{202E}').contains(&c)));
    }
    if matches!(target, Target::File(_)) {
        assert!(!path.is_root());
    }
    // A side file names a base file that is itself a valid path.
    if let Some((base, _)) = sidecar_of(path) {
        assert!(!base.is_root());
        assert!(base.as_str().len() < path.as_str().len());
    }
    // Normalizing what was accepted, re-encoded, gives the same path.
    let mut again = path.encode();
    if target.is_directory() && !path.is_root() {
        again.push('/');
    }
    assert_eq!(parse_target(&again).as_ref().ok(), Some(&target));
});

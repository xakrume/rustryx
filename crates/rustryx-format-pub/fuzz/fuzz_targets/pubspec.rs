//! Fuzz target: `pubspec.yaml` of an untrusted publisher.
//!
//! The property is "returns, never panics, never hangs": the parser must answer every byte string
//! with a manifest or a refusal. The limits are the production defaults, so a document that finds
//! a way around the node budget shows up as a timeout.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rustryx_format_pub::pubspec::{PubspecLimits, parse};

fuzz_target!(|data: &[u8]| {
    let _ = parse(data, &PubspecLimits::default());
});

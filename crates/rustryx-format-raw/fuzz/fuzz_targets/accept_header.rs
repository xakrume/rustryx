//! Feeds arbitrary `Accept` header values to the listing negotiation: it must never panic,
//! whatever the bytes.
#![no_main]

use http::{HeaderMap, HeaderValue, header};
use libfuzzer_sys::fuzz_target;
use rustryx_format_raw::accept::prefers_json;

fuzz_target!(|data: &[u8]| {
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_bytes(data) {
        headers.append(header::ACCEPT, value);
    }
    let _ = prefers_json(&headers);
});

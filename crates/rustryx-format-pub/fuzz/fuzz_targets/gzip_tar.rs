//! Fuzz target: the gzip-compressed tar of a package.
//!
//! The limits are tightened so that one input costs milliseconds (a decompression bomb is
//! refused at a few megabytes instead of a quarter of a gigabyte); the code under test is the
//! same. The property is "returns, never panics, never allocates past the limits".
#![no_main]

use libfuzzer_sys::fuzz_target;
use rustryx_format_pub::archive::{ArchiveLimits, inspect};

fuzz_target!(|data: &[u8]| {
    let limits = ArchiveLimits {
        max_uncompressed_bytes: 4 * 1024 * 1024,
        max_entry_bytes: 2 * 1024 * 1024,
        ratio_floor_bytes: 256 * 1024,
        ..ArchiveLimits::default()
    };
    let _ = inspect(data, data.len() as u64, &limits);
});

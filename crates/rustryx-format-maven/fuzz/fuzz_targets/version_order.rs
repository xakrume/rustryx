#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    rustryx_format_maven::fuzzing::version_order(data);
});

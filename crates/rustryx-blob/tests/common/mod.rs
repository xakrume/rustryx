//! Helpers shared by the integration tests. Each test binary compiles its own copy,
//! so items one binary does not use are expected.
#![allow(dead_code)]

use bytes::Bytes;

/// Deterministic content: byte `i` of the stream. Lets a test check any range of a
/// large object without keeping the object in memory.
pub fn byte_at(index: u64) -> u8 {
    u8::try_from(index % 251).unwrap_or(0)
}

/// `len` bytes of the stream starting at `offset`.
pub fn pattern(offset: u64, len: usize) -> Bytes {
    Bytes::from(
        (0..len as u64)
            .map(|i| byte_at(offset + i))
            .collect::<Vec<u8>>(),
    )
}

/// Resident set size of this process in KiB.
///
/// `/proc` where it exists, `ps` elsewhere (macOS has no `/proc`). Page cache of
/// files written by the test is not part of RSS, which is the point of the check.
pub fn rss_kib() -> u64 {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        let line = status.lines().find(|line| line.starts_with("VmRSS:"));
        if let Some(kib) = line.and_then(|l| l.split_whitespace().nth(1)) {
            return kib.parse().unwrap_or(0);
        }
    }
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps must be runnable to measure RSS");
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("ps printed an RSS number")
}

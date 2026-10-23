//! Helpers shared by the in-memory implementations.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use md5::Md5;
use rustryx_core::model::Checksums;
use sha1::{Digest, Sha1};
use sha2::Sha256;
use time::OffsetDateTime;

/// Largest slice fed to a hasher in one call (CONTRIBUTING rule 2). Hashing stays in
/// the calling task, so one call must stay short enough not to starve the executor.
pub(crate) const HASH_SLICE: usize = 64 * 1024;

/// Locks a std mutex and recovers from poisoning.
///
/// Poisoning only means another test thread panicked while holding the lock. That
/// panic is already the failure of its own test, and turning it into a second panic in
/// every other test sharing the store would bury the real cause.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Splits `data` into slices of at most [`HASH_SLICE`] bytes.
pub(crate) fn slices(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    data.chunks(HASH_SLICE)
}

/// Adds a std duration without panicking at the representable extremes.
pub(crate) fn add_std(at: OffsetDateTime, by: Duration) -> OffsetDateTime {
    let delta = time::Duration::try_from(by).unwrap_or(time::Duration::MAX);
    at.saturating_add(delta)
}

/// SHA-1, SHA-256 and MD5 computed side by side while data streams through.
pub(crate) struct StreamHasher {
    sha1: Sha1,
    sha256: Sha256,
    md5: Md5,
    size: u64,
}

impl StreamHasher {
    pub(crate) fn new() -> Self {
        Self {
            sha1: Sha1::new(),
            sha256: Sha256::new(),
            md5: Md5::new(),
            size: 0,
        }
    }

    /// Feeds `data` in slices of at most [`HASH_SLICE`] bytes.
    pub(crate) fn update(&mut self, data: &[u8]) {
        for slice in slices(data) {
            self.sha1.update(slice);
            self.sha256.update(slice);
            self.md5.update(slice);
        }
        self.size = self
            .size
            .saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
    }

    /// Returns the byte count and lowercase hex digests.
    pub(crate) fn finish(self) -> (u64, Checksums) {
        let checksums = Checksums {
            sha1: hex::encode(self.sha1.finalize()),
            sha256: hex::encode(self.sha256.finalize()),
            md5: hex::encode(self.md5.finalize()),
        };
        (self.size, checksums)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slices_never_exceed_the_limit_and_cover_all_bytes() {
        let data = vec![7_u8; HASH_SLICE * 3 + 11];
        let lens: Vec<usize> = slices(&data).map(<[u8]>::len).collect();
        assert_eq!(lens, vec![HASH_SLICE, HASH_SLICE, HASH_SLICE, 11]);
        assert_eq!(slices(&[]).count(), 0);
    }

    #[test]
    fn hasher_matches_known_digests_for_abc() {
        let mut hasher = StreamHasher::new();
        hasher.update(b"a");
        hasher.update(b"bc");
        let (size, sums) = hasher.finish();
        assert_eq!(size, 3);
        assert_eq!(sums.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            sums.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(sums.md5, "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn hasher_of_nothing_matches_empty_digests() {
        let (size, sums) = StreamHasher::new().finish();
        assert_eq!(size, 0);
        assert_eq!(sums.sha1, "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sums.md5, "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn add_std_saturates_instead_of_panicking() {
        let at = OffsetDateTime::UNIX_EPOCH;
        assert_eq!(
            add_std(at, Duration::from_secs(5)),
            at + time::Duration::seconds(5)
        );
        assert!(add_std(at, Duration::MAX) > at);
    }

    #[test]
    #[allow(
        clippy::panic,
        reason = "the test must panic inside a thread to poison the mutex"
    )]
    fn lock_recovers_from_a_poisoned_mutex() {
        let mutex = std::sync::Arc::new(Mutex::new(1_u8));
        let clone = std::sync::Arc::clone(&mutex);
        let _ = std::thread::spawn(move || {
            let _guard = clone.lock().expect("lock");
            panic!("poison the mutex");
        })
        .join();
        assert!(mutex.is_poisoned());
        assert_eq!(*lock(&mutex), 1);
    }
}

//! Expected checksums: what a client or an upstream says the content hashes to.
//!
//! The digests themselves are computed by the blob writer (`BlobCommit`), in the
//! same task that writes the bytes. This module only describes the expectation and
//! compares it with the result.

use http::HeaderMap;
use rustryx_core::model::Checksums;

/// Digest algorithm of an [`ExpectedChecksum`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HashAlgo {
    /// SHA-1 (Maven sidecars, legacy).
    Sha1,
    /// SHA-256 (pub, npm integrity, OCI digests, `PyPI`).
    Sha256,
    /// MD5 (Maven sidecars, legacy).
    Md5,
}

impl HashAlgo {
    /// Length of the lowercase hex digest.
    #[must_use]
    pub const fn hex_len(self) -> usize {
        match self {
            Self::Sha1 => 40,
            Self::Sha256 => 64,
            Self::Md5 => 32,
        }
    }
}

/// Why an expected checksum could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a valid hex digest for the algorithm")]
pub struct InvalidChecksum;

/// A digest that the content must match before it may be published or served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedChecksum {
    algo: HashAlgo,
    hex: String,
}

impl ExpectedChecksum {
    /// Builds an expectation from a hex digest (any case).
    ///
    /// # Errors
    /// [`InvalidChecksum`] when the text is not hex of the right length.
    pub fn new(algo: HashAlgo, hex: &str) -> Result<Self, InvalidChecksum> {
        let hex = hex.trim();
        if hex.len() != algo.hex_len() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(InvalidChecksum);
        }
        Ok(Self {
            algo,
            hex: hex.to_ascii_lowercase(),
        })
    }

    /// The algorithm.
    #[must_use]
    pub fn algo(&self) -> HashAlgo {
        self.algo
    }

    /// The lowercase hex digest.
    #[must_use]
    pub fn hex(&self) -> &str {
        &self.hex
    }

    /// True when the computed checksums agree with the expectation.
    #[must_use]
    pub fn matches(&self, actual: &Checksums) -> bool {
        let computed = match self.algo {
            HashAlgo::Sha1 => &actual.sha1,
            HashAlgo::Sha256 => &actual.sha256,
            HashAlgo::Md5 => &actual.md5,
        };
        computed.eq_ignore_ascii_case(&self.hex)
    }
}

/// Reads a checksum that the upstream announces in its response headers.
///
/// Implemented by formats whose upstream protocol carries digests in headers. The
/// pipeline calls it once, when the response head arrives. When it returns `Some`,
/// the body is written to the blob store and verified before a single byte reaches
/// the client; when it returns `None` (and no checksum was known up front), the
/// body is tee'd to the client while it is cached.
pub trait ChecksumSource: Send + Sync {
    /// The expectation derived from the upstream response headers, if any.
    fn expected_from_headers(&self, headers: &HeaderMap) -> Option<ExpectedChecksum>;
}

/// Reads `X-Checksum-Sha256`, `X-Checksum-Sha1` and `X-Checksum-Md5` (strongest first).
#[derive(Debug, Clone, Copy, Default)]
pub struct StandardChecksumHeaders;

impl ChecksumSource for StandardChecksumHeaders {
    fn expected_from_headers(&self, headers: &HeaderMap) -> Option<ExpectedChecksum> {
        [
            ("x-checksum-sha256", HashAlgo::Sha256),
            ("x-checksum-sha1", HashAlgo::Sha1),
            ("x-checksum-md5", HashAlgo::Md5),
        ]
        .into_iter()
        .find_map(|(name, algo)| {
            let value = headers.get(name)?.to_str().ok()?;
            ExpectedChecksum::new(algo, value).ok()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn sums() -> Checksums {
        Checksums {
            sha1: "a".repeat(40),
            sha256: "b".repeat(64),
            md5: "c".repeat(32),
        }
    }

    #[test]
    fn matches_each_algorithm_case_insensitively() {
        let s = sums();
        assert!(
            ExpectedChecksum::new(HashAlgo::Sha1, &"A".repeat(40))
                .unwrap()
                .matches(&s)
        );
        assert!(
            ExpectedChecksum::new(HashAlgo::Sha256, &"b".repeat(64))
                .unwrap()
                .matches(&s)
        );
        assert!(
            ExpectedChecksum::new(HashAlgo::Md5, &"c".repeat(32))
                .unwrap()
                .matches(&s)
        );
        assert!(
            !ExpectedChecksum::new(HashAlgo::Sha256, &"d".repeat(64))
                .unwrap()
                .matches(&s)
        );
    }

    #[test]
    fn rejects_malformed_digests() {
        assert!(ExpectedChecksum::new(HashAlgo::Sha256, "abc").is_err());
        assert!(ExpectedChecksum::new(HashAlgo::Sha1, &"g".repeat(40)).is_err());
        assert!(ExpectedChecksum::new(HashAlgo::Md5, "").is_err());
    }

    #[test]
    fn accessors_expose_normalized_values() {
        let e = ExpectedChecksum::new(HashAlgo::Md5, &format!(" {} ", "AB".repeat(16))).unwrap();
        assert_eq!(e.algo(), HashAlgo::Md5);
        assert_eq!(e.hex(), "ab".repeat(16));
    }

    #[test]
    fn standard_headers_prefer_the_strongest_valid_digest() {
        let mut h = HeaderMap::new();
        h.insert(
            "x-checksum-md5",
            HeaderValue::from_str(&"1".repeat(32)).unwrap(),
        );
        h.insert(
            "x-checksum-sha1",
            HeaderValue::from_str(&"2".repeat(40)).unwrap(),
        );
        let got = StandardChecksumHeaders.expected_from_headers(&h).unwrap();
        assert_eq!(got.algo(), HashAlgo::Sha1);
        h.insert(
            "x-checksum-sha256",
            HeaderValue::from_str(&"3".repeat(64)).unwrap(),
        );
        assert_eq!(
            StandardChecksumHeaders
                .expected_from_headers(&h)
                .unwrap()
                .algo(),
            HashAlgo::Sha256
        );
        // A malformed strong digest must not hide a valid weaker one.
        h.insert("x-checksum-sha256", HeaderValue::from_static("zzz"));
        assert_eq!(
            StandardChecksumHeaders
                .expected_from_headers(&h)
                .unwrap()
                .algo(),
            HashAlgo::Sha1
        );
        assert!(
            StandardChecksumHeaders
                .expected_from_headers(&HeaderMap::new())
                .is_none()
        );
    }
}

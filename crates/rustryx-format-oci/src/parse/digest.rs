//! Content digests (`sha256:<hex>`, `sha512:<hex>`).
//!
//! Grammar: the digest section of the OCI Image Specification, descriptor.md
//! (<https://github.com/opencontainers/image-spec/blob/v1.1.0/descriptor.md>):
//! `algorithm ":" encoded`, algorithm components `[a-z0-9]+` joined by `[+._-]`, encoded
//! `[a-zA-Z0-9=_-]+`. The registered algorithms are stricter: `sha256` is exactly 64
//! lowercase hex characters and `sha512` exactly 128. Uppercase hex is refused, not
//! repaired: a digest is part of a storage path and of a security decision, so two
//! spellings of one digest would be two different paths.
//!
//! A string that fits the general grammar but names another algorithm is
//! [`DigestError::UnsupportedAlgorithm`], which lets a caller answer "not supported"
//! instead of "malformed".

use std::fmt;

use sha2::{Digest as _, Sha256, Sha512};

/// Longest digest string accepted at all. The longest supported one is 135 bytes; the
/// bound only keeps a hostile input from being scanned.
const MAX_DIGEST_LEN: usize = 255;

/// A digest algorithm this registry can compute and verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DigestAlgorithm {
    /// SHA-256: the algorithm every registry must support and the only one blobs use.
    Sha256,
    /// SHA-512: accepted for manifests, which the registry hashes itself.
    Sha512,
}

impl DigestAlgorithm {
    /// The name used before the colon.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        }
    }

    /// Length of the lowercase hex encoding.
    #[must_use]
    pub const fn hex_len(self) -> usize {
        match self {
            Self::Sha256 => 64,
            Self::Sha512 => 128,
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "sha256" => Some(Self::Sha256),
            "sha512" => Some(Self::Sha512),
            _ => None,
        }
    }
}

/// Why a digest string was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DigestError {
    /// Does not fit the digest grammar, or the encoding is wrong for the algorithm.
    #[error("malformed digest")]
    Malformed,
    /// Fits the grammar but names an algorithm this registry does not support.
    #[error("unsupported digest algorithm")]
    UnsupportedAlgorithm,
}

/// A validated digest. Two digests are equal exactly when their text is equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest {
    algorithm: DigestAlgorithm,
    hex: String,
}

impl Digest {
    /// Parses `algorithm:hex`.
    ///
    /// # Errors
    /// [`DigestError::Malformed`] for anything outside the grammar or with the wrong hex
    /// length or alphabet, [`DigestError::UnsupportedAlgorithm`] for a well formed digest
    /// of another algorithm.
    pub fn parse(text: &str) -> Result<Self, DigestError> {
        if text.len() > MAX_DIGEST_LEN {
            return Err(DigestError::Malformed);
        }
        let (algorithm, encoded) = text.split_once(':').ok_or(DigestError::Malformed)?;
        if !valid_algorithm_grammar(algorithm) || !valid_encoded_grammar(encoded) {
            return Err(DigestError::Malformed);
        }
        let known =
            DigestAlgorithm::from_name(algorithm).ok_or(DigestError::UnsupportedAlgorithm)?;
        let lower_hex = encoded.len() == known.hex_len()
            && encoded
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !lower_hex {
            return Err(DigestError::Malformed);
        }
        Ok(Self {
            algorithm: known,
            hex: encoded.to_owned(),
        })
    }

    /// Builds a SHA-256 digest from its lowercase hex form.
    ///
    /// # Errors
    /// [`DigestError::Malformed`] when `hex` is not 64 lowercase hex characters.
    pub fn from_sha256_hex(hex: &str) -> Result<Self, DigestError> {
        Self::parse(&format!("sha256:{hex}"))
    }

    /// Computes the digest of `data`.
    #[must_use]
    pub fn of(algorithm: DigestAlgorithm, data: &[u8]) -> Self {
        let hex = match algorithm {
            DigestAlgorithm::Sha256 => hex::encode(Sha256::digest(data)),
            DigestAlgorithm::Sha512 => hex::encode(Sha512::digest(data)),
        };
        Self { algorithm, hex }
    }

    /// The algorithm.
    #[must_use]
    pub const fn algorithm(&self) -> DigestAlgorithm {
        self.algorithm
    }

    /// The lowercase hex encoding, without the algorithm.
    #[must_use]
    pub fn hex(&self) -> &str {
        &self.hex
    }

    /// True when `data` hashes to this digest. The comparison is over public values (a
    /// digest is not a secret), so it is not constant time.
    #[must_use]
    pub fn verifies(&self, data: &[u8]) -> bool {
        Self::of(self.algorithm, data) == *self
    }

    /// The referrers tag of the tag schema: `<alg>-<hex>`, with the encoded part cut to 64
    /// characters as the specification requires.
    #[must_use]
    pub fn referrers_tag(&self) -> String {
        let end = self.hex.len().min(64);
        format!("{}-{}", self.algorithm.as_str(), &self.hex[..end])
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algorithm.as_str(), self.hex)
    }
}

/// `[a-z0-9]+([+._-][a-z0-9]+)*`
fn valid_algorithm_grammar(text: &str) -> bool {
    let mut expect_component = true;
    let mut component_len = 0usize;
    for byte in text.bytes() {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            component_len += 1;
            expect_component = false;
        } else if matches!(byte, b'+' | b'.' | b'_' | b'-') {
            if component_len == 0 {
                return false;
            }
            component_len = 0;
            expect_component = true;
        } else {
            return false;
        }
    }
    !expect_component
}

/// `[a-zA-Z0-9=_-]+`
fn valid_encoded_grammar(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'=' | b'_' | b'-'))
}

#[cfg(test)]
mod tests;

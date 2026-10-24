//! Checksum files of Maven: digests of stored bytes, and supplied digests.
//!
//! Spec: <https://maven.apache.org/resolver/configuration.html> (checksum algorithms) and the
//! sidecar convention of the repository layout, `<file>.sha1`, `.md5`, `.sha256`, `.sha512`,
//! whose body is the lowercase hex digest, optionally followed by the file name.
//!
//! Checksum files are **derived, never stored**. A GET computes the digest from the stored
//! file (MD5, SHA-1 and SHA-256 are already on the asset, SHA-512 is streamed from the blob), a
//! PUT is verified against the stored file and dropped. Why: a stored sidecar can disagree
//! with the file it describes, which is exactly the integrity failure SEC-37 is about; and a
//! derived one cannot be overwritten under `allow_once` or go missing for one algorithm.

use bytes::Bytes;
use futures::{Stream, StreamExt};
use md5::Md5;
use rustryx_core::error::BlobError;
use rustryx_core::model::Checksums;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

use crate::layout::ChecksumAlgo;

/// Longest checksum file accepted. A SHA-512 digest with a file name fits comfortably.
pub const MAX_CHECKSUM_FILE_BYTES: u64 = 1024;

/// Why a supplied checksum file is not usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChecksumFileError {
    /// Not UTF-8, or no token at all.
    #[error("the checksum file is empty or not text")]
    Empty,
    /// The first token is not a hex digest of the right length.
    #[error("the checksum file does not hold a hex digest of the right length")]
    NotADigest,
}

/// Parses `hex`, `hex  name` or `hex *name` (the output of `sha1sum` and `md5sum`) into a
/// lowercase digest of the length `algo` has.
///
/// # Errors
/// [`ChecksumFileError`] for anything else.
pub fn parse_checksum_file(body: &[u8], algo: ChecksumAlgo) -> Result<String, ChecksumFileError> {
    let text = std::str::from_utf8(body).map_err(|_| ChecksumFileError::Empty)?;
    let token = text
        .split_whitespace()
        .next()
        .ok_or(ChecksumFileError::Empty)?;
    if token.len() == algo.hex_len() && token.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(token.to_ascii_lowercase())
    } else {
        Err(ChecksumFileError::NotADigest)
    }
}

/// The digest of in-memory bytes (a generated or cached metadata document).
#[must_use]
pub fn digest_of_bytes(algo: ChecksumAlgo, data: &[u8]) -> String {
    match algo {
        ChecksumAlgo::Md5 => hex::encode(Md5::digest(data)),
        ChecksumAlgo::Sha1 => hex::encode(Sha1::digest(data)),
        ChecksumAlgo::Sha256 => hex::encode(Sha256::digest(data)),
        ChecksumAlgo::Sha512 => hex::encode(Sha512::digest(data)),
    }
}

/// The digest the blob store already computed, `None` for SHA-512 which it does not.
#[must_use]
pub fn stored_digest(algo: ChecksumAlgo, checksums: &Checksums) -> Option<String> {
    match algo {
        ChecksumAlgo::Md5 => Some(checksums.md5.to_ascii_lowercase()),
        ChecksumAlgo::Sha1 => Some(checksums.sha1.to_ascii_lowercase()),
        ChecksumAlgo::Sha256 => Some(checksums.sha256.to_ascii_lowercase()),
        ChecksumAlgo::Sha512 => None,
    }
}

/// SHA-512 of a blob stream, hashed in slices of at most 64 KiB in the calling task (the rule of
/// README 8.1: checksums are not moved to `spawn_blocking`). Memory is one chunk.
///
/// # Errors
/// The store's error when the stream breaks.
pub async fn sha512_of_stream<S>(mut stream: S) -> Result<String, BlobError>
where
    S: Stream<Item = Result<Bytes, BlobError>> + Unpin,
{
    let mut hasher = Sha512::new();
    while let Some(chunk) = stream.next().await {
        for slice in rustryx_format_core::stream::hash_slices(chunk?) {
            hasher.update(&slice);
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Constant time comparison of two hex digests. A digest is public data, but a comparison
/// that does not short circuit costs nothing and never becomes an oracle.
#[must_use]
pub fn digests_equal(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    #[test]
    fn digests_of_the_empty_input_match_the_published_values() {
        assert_eq!(
            digest_of_bytes(ChecksumAlgo::Md5, b""),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
        assert_eq!(
            digest_of_bytes(ChecksumAlgo::Sha1, b""),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        assert_eq!(
            digest_of_bytes(ChecksumAlgo::Sha256, b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(digest_of_bytes(ChecksumAlgo::Sha512, b"abc").starts_with("ddaf35a193617aba"));
        assert_eq!(digest_of_bytes(ChecksumAlgo::Sha512, b"abc").len(), 128);
    }

    #[test]
    fn parses_bare_digests_and_sum_tool_output() {
        let sha1 = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
        for body in [
            sha1.to_owned(),
            format!("{sha1}\n"),
            format!("{sha1}  lib-1.0.jar\n"),
            format!("{sha1} *lib-1.0.jar"),
            sha1.to_ascii_uppercase(),
        ] {
            assert_eq!(
                parse_checksum_file(body.as_bytes(), ChecksumAlgo::Sha1).unwrap(),
                sha1
            );
        }
    }

    #[test]
    fn rejects_wrong_length_non_hex_empty_and_binary() {
        assert_eq!(
            parse_checksum_file(b"", ChecksumAlgo::Sha1),
            Err(ChecksumFileError::Empty)
        );
        assert_eq!(
            parse_checksum_file(b"  \n", ChecksumAlgo::Md5),
            Err(ChecksumFileError::Empty)
        );
        assert_eq!(
            parse_checksum_file(&[0xff, 0xfe], ChecksumAlgo::Md5),
            Err(ChecksumFileError::Empty)
        );
        assert_eq!(
            parse_checksum_file(b"abc", ChecksumAlgo::Sha1),
            Err(ChecksumFileError::NotADigest)
        );
        assert_eq!(
            parse_checksum_file(&[b'z'; 40], ChecksumAlgo::Sha1),
            Err(ChecksumFileError::NotADigest)
        );
        // A sha1 digest is not a valid md5 file.
        assert_eq!(
            parse_checksum_file(
                b"da39a3ee5e6b4b0d3255bfef95601890afd80709",
                ChecksumAlgo::Md5
            ),
            Err(ChecksumFileError::NotADigest)
        );
    }

    #[test]
    fn stored_digests_exist_for_three_algorithms() {
        let checksums = Checksums {
            sha1: "A".repeat(40),
            sha256: "b".repeat(64),
            md5: "c".repeat(32),
        };
        assert_eq!(
            stored_digest(ChecksumAlgo::Sha1, &checksums),
            Some("a".repeat(40))
        );
        assert_eq!(
            stored_digest(ChecksumAlgo::Sha256, &checksums),
            Some("b".repeat(64))
        );
        assert_eq!(
            stored_digest(ChecksumAlgo::Md5, &checksums),
            Some("c".repeat(32))
        );
        assert_eq!(stored_digest(ChecksumAlgo::Sha512, &checksums), None);
    }

    #[tokio::test]
    async fn sha512_of_a_stream_matches_the_one_shot_digest() {
        let big = vec![7u8; 200_000];
        let chunks: Vec<Result<Bytes, BlobError>> = big
            .chunks(70_000)
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        let streamed = sha512_of_stream(stream::iter(chunks)).await.unwrap();
        assert_eq!(streamed, digest_of_bytes(ChecksumAlgo::Sha512, &big));
    }

    #[tokio::test]
    async fn a_broken_stream_is_an_error_not_a_digest() {
        let chunks: Vec<Result<Bytes, BlobError>> = vec![
            Ok(Bytes::from_static(b"a")),
            Err(BlobError::Unavailable("x".into())),
        ];
        assert!(sha512_of_stream(stream::iter(chunks)).await.is_err());
    }

    #[test]
    fn digests_compare_in_constant_time_shape() {
        assert!(digests_equal("abc", "abc"));
        assert!(!digests_equal("abc", "abd"));
        assert!(!digests_equal("abc", "ab"));
    }
}

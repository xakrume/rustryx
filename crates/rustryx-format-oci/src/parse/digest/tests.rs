use proptest::prelude::*;

use super::*;

const EMPTY_SHA256: &str =
    "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[test]
fn parses_a_sha256_digest() {
    let digest = Digest::parse(EMPTY_SHA256).unwrap();
    assert_eq!(digest.algorithm(), DigestAlgorithm::Sha256);
    assert_eq!(digest.hex().len(), 64);
    assert_eq!(digest.to_string(), EMPTY_SHA256);
}

#[test]
fn computes_known_digests() {
    assert_eq!(
        Digest::of(DigestAlgorithm::Sha256, b"").to_string(),
        EMPTY_SHA256
    );
    let sha512 = Digest::of(DigestAlgorithm::Sha512, b"");
    assert_eq!(sha512.hex().len(), 128);
    assert!(sha512.hex().starts_with("cf83e1357eefb8bd"));
    assert!(sha512.verifies(b""));
    assert!(!sha512.verifies(b"x"));
}

#[test]
fn rejects_malformed_digests() {
    for bad in [
        "",
        "sha256",
        "sha256:",
        ":abc",
        "sha256:abc",
        // uppercase hex is refused, not repaired
        "sha256:E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855",
        // one hex digit short and one too long
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b85",
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b8555",
        // not hex
        "sha256:g3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        // algorithm grammar
        "Sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "sha256-:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "-sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "sha 256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855:x",
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n",
    ] {
        assert!(Digest::parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn an_unknown_algorithm_is_unsupported_not_malformed() {
    assert_eq!(
        Digest::parse("blake3:abcdef0123456789"),
        Err(DigestError::UnsupportedAlgorithm)
    );
    assert_eq!(
        Digest::parse("sha384+x.y:abc_-="),
        Err(DigestError::UnsupportedAlgorithm)
    );
    assert_eq!(Digest::parse("blake3:ab cd"), Err(DigestError::Malformed));
}

#[test]
fn an_oversized_input_is_refused_without_scanning() {
    let huge = format!("sha256:{}", "a".repeat(10_000));
    assert_eq!(Digest::parse(&huge), Err(DigestError::Malformed));
}

#[test]
fn referrers_tag_cuts_the_encoded_part_to_64_characters() {
    let sha256 = Digest::parse(EMPTY_SHA256).unwrap();
    assert_eq!(
        sha256.referrers_tag(),
        "sha256-e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    let sha512 = Digest::of(DigestAlgorithm::Sha512, b"");
    let tag = sha512.referrers_tag();
    assert_eq!(tag.len(), "sha512-".len() + 64);
    assert!(crate::parse::reference::is_tag(&tag));
}

#[test]
fn from_sha256_hex_round_trips() {
    let digest = Digest::from_sha256_hex(&"ab".repeat(32)).unwrap();
    assert_eq!(digest.to_string(), format!("sha256:{}", "ab".repeat(32)));
    assert!(Digest::from_sha256_hex("zz").is_err());
}

proptest! {
    #[test]
    fn parse_never_panics(text in ".{0,300}") {
        let _ = Digest::parse(&text);
    }

    #[test]
    fn computed_digests_round_trip(data in proptest::collection::vec(any::<u8>(), 0..256), sha512 in any::<bool>()) {
        let algorithm = if sha512 { DigestAlgorithm::Sha512 } else { DigestAlgorithm::Sha256 };
        let digest = Digest::of(algorithm, &data);
        let parsed = Digest::parse(&digest.to_string()).unwrap();
        prop_assert_eq!(&parsed, &digest);
        prop_assert!(parsed.verifies(&data));
    }

    #[test]
    fn a_changed_byte_never_verifies(mut data in proptest::collection::vec(any::<u8>(), 1..64), index in any::<usize>()) {
        let digest = Digest::of(DigestAlgorithm::Sha256, &data);
        let at = index % data.len();
        data[at] ^= 0x01;
        prop_assert!(!digest.verifies(&data));
    }
}

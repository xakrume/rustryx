use proptest::prelude::*;

use super::*;

#[test]
fn tags_follow_the_grammar() {
    let longest = "a".repeat(128);
    for ok in [
        "latest",
        "1.0",
        "v1.2.3-rc.1",
        "_x",
        "A",
        "a_b-c.d",
        longest.as_str(),
    ] {
        assert!(Tag::parse(ok).is_ok(), "{ok}");
    }
    let too_long = "a".repeat(129);
    for bad in [
        "",
        "-x",
        ".x",
        "a b",
        "a/b",
        "a:b",
        "a@b",
        too_long.as_str(),
        "caf\u{e9}",
        "a\n",
    ] {
        assert!(Tag::parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn a_colon_makes_a_reference_a_digest_attempt() {
    let digest = format!("sha256:{}", "a".repeat(64));
    assert!(matches!(
        Reference::parse(&digest),
        Ok(Reference::Digest(_))
    ));
    assert!(matches!(Reference::parse("latest"), Ok(Reference::Tag(_))));
    assert!(matches!(
        Reference::parse("sha256:short"),
        Err(ReferenceError::InvalidDigest(DigestError::Malformed))
    ));
    assert!(matches!(
        Reference::parse("blake3:abc"),
        Err(ReferenceError::InvalidDigest(
            DigestError::UnsupportedAlgorithm
        ))
    ));
    assert_eq!(Reference::parse("a b"), Err(ReferenceError::InvalidTag));
}

#[test]
fn reference_displays_as_it_was_parsed() {
    let digest = format!("sha256:{}", "b".repeat(64));
    for text in ["latest", "1.0", digest.as_str()] {
        let reference = Reference::parse(text).unwrap();
        assert_eq!(reference.to_string(), text);
        assert_eq!(reference.as_path_segment(), text);
        assert_eq!(reference.digest().is_some(), text.starts_with("sha256"));
    }
}

proptest! {
    #[test]
    fn parse_never_panics(text in ".{0,200}") {
        let _ = Reference::parse(&text);
        let _ = Tag::parse(&text);
    }

    #[test]
    fn valid_tags_round_trip(tag in "[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}") {
        let parsed = Reference::parse(&tag).unwrap();
        prop_assert_eq!(parsed.to_string(), tag);
    }
}

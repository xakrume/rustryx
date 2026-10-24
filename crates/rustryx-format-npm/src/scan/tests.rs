use base64::engine::general_purpose::STANDARD;
use proptest::prelude::*;
use serde_json::{Value, json};

use super::*;

fn document(payload: &[u8]) -> String {
    let data = STANDARD.encode(payload);
    format!(
        concat!(
            r#"{{"_id":"p","name":"p","description":"a \"quoted\" word, a \\ and é","#,
            r#""dist-tags":{{"latest":"1.0.0"}},"#,
            r#""versions":{{"1.0.0":{{"name":"p","version":"1.0.0","scripts":{{"data":"x"}}}}}},"#,
            r#""access":null,"_attachments":{{"p-1.0.0.tgz":{{"content_type":"application/octet-stream","#,
            r#""data":"{data}","length":{length}}}}}}}"#
        ),
        data = data,
        length = payload.len()
    )
}

/// Feeds `input` in pieces of `step` bytes and returns the scanner and everything decoded.
fn scan(input: &[u8], step: usize) -> Result<(Scanner, Vec<u8>), ScanError> {
    let mut scanner = Scanner::new(ScanLimits::default());
    let mut decoded = Vec::new();
    for piece in input.chunks(step.max(1)) {
        scanner.feed(piece)?;
        decoded.extend(scanner.take_decoded());
    }
    scanner.finish()?;
    Ok((scanner, decoded))
}

#[test]
fn splits_a_publish_into_metadata_and_payload() {
    let payload: Vec<u8> = (0..=255u8).cycle().take(10_000).collect();
    let text = document(&payload);
    let (scanner, decoded) = scan(text.as_bytes(), 4096).unwrap();
    assert_eq!(decoded, payload);
    assert!(scanner.is_complete());
    assert!(scanner.data_started() && scanner.data_finished());
    assert_eq!(scanner.attachment_name(), Some("p-1.0.0.tgz"));
    let meta: Value = serde_json::from_slice(scanner.meta()).unwrap();
    assert_eq!(meta["name"], "p");
    assert_eq!(meta["_attachments"]["p-1.0.0.tgz"]["data"], "");
    assert_eq!(meta["_attachments"]["p-1.0.0.tgz"]["length"], 10_000);
    assert_eq!(meta["description"], "a \"quoted\" word, a \\ and \u{e9}");
    assert!(
        scanner.meta().len() < 600,
        "the payload must not be in the metadata"
    );
}

#[test]
fn every_chunking_gives_the_same_result() {
    let payload = b"hello tarball, a little longer than three bytes";
    let text = document(payload);
    let reference = scan(text.as_bytes(), text.len()).unwrap();
    for step in [1, 2, 3, 4, 5, 7, 13, 64] {
        let (scanner, decoded) = scan(text.as_bytes(), step).unwrap();
        assert_eq!(decoded, payload, "step {step}");
        assert_eq!(scanner.meta(), reference.0.meta(), "step {step}");
    }
}

#[test]
fn the_head_document_is_valid_json_at_the_moment_the_payload_starts() {
    let text = document(b"abcdef");
    let mut scanner = Scanner::new(ScanLimits::default());
    let mut started = false;
    for byte in text.as_bytes() {
        scanner.feed(&[*byte]).unwrap();
        if scanner.data_started() && !started {
            started = true;
            let head: Value = serde_json::from_slice(&scanner.head_document()).unwrap();
            assert_eq!(head["versions"]["1.0.0"]["version"], "1.0.0");
            assert_eq!(
                head["_attachments"]["p-1.0.0.tgz"]["content_type"],
                "application/octet-stream"
            );
            assert_eq!(head["_attachments"]["p-1.0.0.tgz"]["data"], "");
        }
    }
    assert!(started);
}

#[test]
fn a_document_without_an_attachment_is_metadata_only() {
    let text = r#"{"name":"p","versions":{"1.0.0":{"name":"p"}},"_attachments":{}}"#;
    let (scanner, decoded) = scan(text.as_bytes(), 3).unwrap();
    assert_eq!(decoded, Vec::<u8>::new());
    assert!(!scanner.data_started());
    assert_eq!(scanner.meta(), text.as_bytes());
    assert_eq!(scanner.attachment_name(), None);
}

#[test]
fn a_data_key_elsewhere_is_ordinary_metadata() {
    // `scripts.data` is not the attachment, and so is `data` inside `versions`.
    let text = r#"{"name":"p","versions":{"1.0.0":{"data":"AAAA"}}}"#;
    let (scanner, decoded) = scan(text.as_bytes(), 5).unwrap();
    assert_eq!(decoded, Vec::<u8>::new());
    assert_eq!(scanner.meta(), text.as_bytes());
}

#[test]
fn strings_that_look_like_structure_do_not_confuse_the_scanner() {
    let tricky = r#"{"name":"p","description":"} \" { \\\" \\\\ \"_attachments\":{","x":["]","\\"],"versions":{}}"#;
    let (scanner, _) = scan(tricky.as_bytes(), 1).unwrap();
    let meta: Value = serde_json::from_slice(scanner.meta()).unwrap();
    assert_eq!(meta, serde_json::from_str::<Value>(tricky).unwrap());
}

#[test]
fn whitespace_around_the_document_is_tolerated() {
    let text = format!(" \n\t{}\r\n  ", document(b"xyz"));
    let (scanner, decoded) = scan(text.as_bytes(), 6).unwrap();
    assert_eq!(decoded, b"xyz");
    assert!(scanner.is_complete());
}

fn scan_error(text: &str) -> ScanError {
    scan(text.as_bytes(), 7).unwrap_err()
}

#[test]
fn must_not_accept_escapes_in_the_keys_that_matter() {
    // The escape is spelled with a backslash character so that no tool between the author and the
    // compiler can turn the sequence into the letter it stands for.
    const BS: char = '\x5c';
    // To a JSON parser this key is `_attachments`; to a scanner that compares bytes it is not.
    let top = format!(r#"{{"name":"p","_attach{BS}u006dents":{{"a.tgz":{{"data":"AAAA"}}}}}}"#);
    assert!(matches!(scan_error(&top), ScanError::Rule(_)));
    let nested = format!(r#"{{"_attachments":{{"a.tgz":{{"da{BS}u0074a":"AAAA"}}}}}}"#);
    assert!(matches!(scan_error(&nested), ScanError::Rule(_)));
    // Below the third level keys are not interpreted, so an escape there is ordinary JSON.
    let deeper = format!(r#"{{"versions":{{"1.0.0":{{"dependencies":{{"a{BS}"b":"1"}}}}}}}}"#);
    assert!(scan(deeper.as_bytes(), 3).is_ok());
}

#[test]
fn must_not_accept_duplicate_top_level_keys() {
    assert!(matches!(
        scan_error(r#"{"name":"a","name":"b"}"#),
        ScanError::Rule(_)
    ));
    assert!(matches!(
        scan_error(r#"{"_attachments":{},"_attachments":{}}"#),
        ScanError::Rule(_)
    ));
}

#[test]
fn must_not_accept_decisions_after_the_attachment() {
    for late in ["name", "_id", "versions", "dist-tags"] {
        let text = format!(r#"{{"_attachments":{{}},"{late}":"x"}}"#);
        assert!(matches!(scan_error(&text), ScanError::Rule(_)), "{late}");
    }
    // Other keys may follow: only what the decision rests on must come first.
    assert!(scan(br#"{"_attachments":{},"readme":"x"}"#, 4).is_ok());
}

#[test]
fn must_not_accept_a_second_attachment_or_a_second_data() {
    let two = r#"{"_attachments":{"a.tgz":{"data":"AAAA"},"b.tgz":{"data":"AAAA"}}}"#;
    assert!(matches!(scan_error(two), ScanError::Rule(_)));
    let twice = r#"{"_attachments":{"a.tgz":{"data":"AAAA","data":"AAAA"}}}"#;
    assert!(matches!(scan_error(twice), ScanError::Rule(_)));
}

#[test]
fn must_not_accept_attachments_of_the_wrong_shape() {
    for text in [
        r#"{"_attachments":[]}"#,
        r#"{"_attachments":"x"}"#,
        r#"{"_attachments":null}"#,
        r#"{"_attachments":{"a.tgz":"x"}}"#,
        r#"{"_attachments":{"a.tgz":[]}}"#,
        r#"{"_attachments":{"a.tgz":{"data":1}}}"#,
        r#"{"_attachments":{"a.tgz":{"data":null}}}"#,
        r#"{"_attachments":{"a.tgz":{"data":{}}}}"#,
        r#"{"_attachments":{"a.tgz":{"data":["AAAA"]}}}"#,
    ] {
        assert!(matches!(scan_error(text), ScanError::Rule(_)), "{text}");
    }
}

#[test]
fn must_not_accept_malformed_base64() {
    for bad in [
        "AAA",      // incomplete group
        "AA=A",     // padding in the middle
        "AAAA====", // too much padding
        "AAAA=AAA", // data after padding
        "AA==AAAA", // data after padding, spread over groups
        "AA\\nAA",  // an escape inside the payload
        "AA AA",    // a space
        "A_AA",     // url safe alphabet
        "A-AA",
        "AAAA\u{e9}",
        "AB==", // non canonical trailing bits
    ] {
        let text = format!(r#"{{"_attachments":{{"a.tgz":{{"data":"{bad}"}}}}}}"#);
        assert_eq!(scan_error(&text), ScanError::BadBase64, "{bad:?}");
    }
    // The valid neighbours of those cases.
    for good in ["", "AAAA", "AA==", "AAA=", "AAAAAAA="] {
        let text = format!(r#"{{"_attachments":{{"a.tgz":{{"data":"{good}"}}}}}}"#);
        assert!(scan(text.as_bytes(), 3).is_ok(), "{good:?}");
    }
}

#[test]
fn must_not_accept_padding_followed_by_more_pieces() {
    // The one-shot decoder rejects data after padding; the streaming one must too, however the
    // pieces fall.
    let text = r#"{"_attachments":{"a.tgz":{"data":"AA==AAAA"}}}"#;
    for step in 1..text.len() {
        assert_eq!(
            scan(text.as_bytes(), step).unwrap_err(),
            ScanError::BadBase64,
            "step {step}"
        );
    }
}

#[test]
fn must_not_accept_truncated_or_trailing_input() {
    let whole = document(b"abc");
    for cut in [1, 10, whole.len() / 2, whole.len() - 1] {
        let mut scanner = Scanner::new(ScanLimits::default());
        // A cut inside the payload may already fail on its own (a partial group is fine until
        // the closing quote), so only the end of the document decides.
        let _ = scanner.feed(&whole.as_bytes()[..cut]);
        assert!(scanner.finish().is_err(), "cut {cut}");
    }
    let trailing = format!("{whole} x");
    assert!(matches!(
        scan_error(&trailing),
        ScanError::Malformed("data after the document")
    ));
    assert_eq!(scan_error(""), ScanError::Truncated);
}

#[test]
fn must_not_accept_non_object_documents_or_broken_structure() {
    for text in [
        "[]",
        "1",
        "\"x\"",
        "null",
        "}",
        "{]",
        "{\"a\":}",
        "{\"a\" \"b\"}",
        "{,}",
    ] {
        assert!(scan(text.as_bytes(), 2).is_err(), "{text}");
    }
}

#[test]
fn must_not_exceed_the_metadata_limit() {
    let limits = ScanLimits {
        max_meta_bytes: 64,
        ..ScanLimits::default()
    };
    let text = format!(r#"{{"name":"{}"}}"#, "x".repeat(100));
    let mut scanner = Scanner::new(limits);
    assert_eq!(
        scanner.feed(text.as_bytes()).unwrap_err(),
        ScanError::MetaTooLarge { limit: 64 }
    );
    // A huge payload is not metadata and does not count against the limit.
    let big = document(&vec![7u8; 100_000]);
    let mut scanner = Scanner::new(ScanLimits {
        max_meta_bytes: 1024,
        ..ScanLimits::default()
    });
    scanner.feed(big.as_bytes()).unwrap();
    scanner.finish().unwrap();
}

#[test]
fn must_not_nest_beyond_the_depth_limit() {
    let deep = format!("{}{}", "{\"a\":".repeat(70), "1");
    assert_eq!(scan(deep.as_bytes(), 9).unwrap_err(), ScanError::TooDeep);
    let arrays = format!(r#"{{"a":{}{}}}"#, "[".repeat(70), "]".repeat(70));
    assert_eq!(scan(arrays.as_bytes(), 9).unwrap_err(), ScanError::TooDeep);
}

#[test]
fn must_not_accept_keys_that_are_not_utf8_or_too_long() {
    let mut bad = br"{".to_vec();
    bad.extend_from_slice(b"\"\xff\":1}");
    assert!(matches!(
        scan(&bad, 3).unwrap_err(),
        ScanError::Malformed(_)
    ));
    let long = format!(r#"{{"{}":1}}"#, "k".repeat(2000));
    assert!(matches!(scan_error(&long), ScanError::Rule(_)));
}

#[test]
fn a_large_payload_in_large_pieces_is_decoded_exactly() {
    let payload: Vec<u8> = (0..1_000_003u32).map(|n| (n % 251) as u8).collect();
    let text = document(&payload);
    let (_, decoded) = scan(text.as_bytes(), 65_536).unwrap();
    assert_eq!(decoded, payload);
}

#[test]
fn the_head_document_closes_every_open_container() {
    let mut scanner = Scanner::new(ScanLimits::default());
    scanner.feed(br#"{"a":{"b":[1,2,"#).unwrap();
    assert_eq!(scanner.head_document(), br#"{"a":{"b":[1,2,]}}"#);
}

fn arbitrary_text() -> impl Strategy<Value = String> {
    proptest::collection::vec(any::<char>(), 0..24).prop_map(|c| c.into_iter().collect())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn never_panics_on_arbitrary_bytes_in_arbitrary_pieces(
        input in proptest::collection::vec(any::<u8>(), 0..400),
        step in 1usize..40,
    ) {
        let mut scanner = Scanner::new(ScanLimits { max_meta_bytes: 256, ..ScanLimits::default() });
        for piece in input.chunks(step) {
            if scanner.feed(piece).is_err() {
                break;
            }
            let _ = scanner.take_decoded();
        }
        let _ = scanner.finish();
        let _ = scanner.head_document();
    }

    #[test]
    fn never_panics_on_mutated_publish_documents(
        payload in proptest::collection::vec(any::<u8>(), 0..64),
        at in 0usize..400,
        replacement in any::<u8>(),
        step in 1usize..50,
    ) {
        let mut bytes = document(&payload).into_bytes();
        if let Some(slot) = bytes.get_mut(at) {
            *slot = replacement;
        }
        let _ = scan(&bytes, step);
    }

    #[test]
    fn round_trip_of_random_documents(
        payload in proptest::collection::vec(any::<u8>(), 0..2048),
        description in arbitrary_text(),
        step in 1usize..300,
    ) {
        // Key order matters to the scanner (`versions` before `_attachments`), and a `Value`
        // would sort the keys, so the document is written by hand.
        let text = format!(
            r#"{{"name":"p","description":{},"versions":{{"1.0.0":{{"name":"p","version":"1.0.0"}}}},"_attachments":{{"p-1.0.0.tgz":{{"content_type":"x","data":"{}","length":{}}}}}}}"#,
            serde_json::to_string(&description).unwrap(),
            STANDARD.encode(&payload),
            payload.len()
        );
        let (scanner, decoded) = scan(text.as_bytes(), step).unwrap();
        prop_assert_eq!(decoded, payload.clone());
        let meta: Value = serde_json::from_slice(scanner.meta()).unwrap();
        let expected = json!({
            "name": "p",
            "description": description,
            "versions": { "1.0.0": { "name": "p", "version": "1.0.0" } },
            "_attachments": { "p-1.0.0.tgz": { "content_type": "x", "data": "", "length": payload.len() } },
        });
        prop_assert_eq!(meta, expected);
    }

    #[test]
    fn base64_stream_equals_one_shot_decoding(
        payload in proptest::collection::vec(any::<u8>(), 0..512),
        step in 1usize..17,
    ) {
        let encoded = STANDARD.encode(&payload);
        let mut decoder = Base64Stream::default();
        let mut out = Vec::new();
        for piece in encoded.as_bytes().chunks(step) {
            decoder.push(piece, &mut out).unwrap();
        }
        decoder.finish().unwrap();
        prop_assert_eq!(out, payload);
    }
}

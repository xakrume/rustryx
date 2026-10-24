use super::*;

fn key(byte: u8) -> CsrfKey {
    let mut bytes = [byte; 32];
    bytes[0] = byte.wrapping_add(1);
    CsrfKey::from_cookie_key(&Secret::new(STANDARD.encode(bytes))).expect("key")
}

#[test]
fn the_token_is_a_function_of_the_session_id_and_the_key() {
    let a = key(1);
    assert_eq!(a.token_for("tid-1"), a.token_for("tid-1"));
    assert_ne!(a.token_for("tid-1"), a.token_for("tid-2"));
    assert_ne!(a.token_for("tid-1"), key(2).token_for("tid-1"));
    assert_eq!(a.token_for("tid-1").len(), 43, "a 256 bit tag in base64url");
}

#[test]
fn only_the_token_of_that_very_session_verifies() {
    let a = key(1);
    let token = a.token_for("tid-1");
    assert!(a.verifies("tid-1", &token));
    assert!(!a.verifies("tid-2", &token), "a token of another session");
    assert!(!a.verifies("tid-1", ""));
    assert!(!a.verifies("tid-1", &token[1..]));
    assert!(!key(2).verifies("tid-1", &token), "another key");
}

#[test]
fn the_cookie_key_may_be_base64_in_any_alphabet_or_hex() {
    let raw: Vec<u8> = (1..=32).collect();
    let base = |engine: &dyn Fn(&[u8]) -> String| {
        CsrfKey::from_cookie_key(&Secret::new(engine(&raw))).expect("key")
    };
    let reference = base(&|b| STANDARD.encode(b)).token_for("t");
    assert_eq!(
        base(&|b| STANDARD_NO_PAD.encode(b)).token_for("t"),
        reference
    );
    assert_eq!(base(&|b| URL_SAFE.encode(b)).token_for("t"), reference);
    assert_eq!(
        base(&|b| URL_SAFE_NO_PAD.encode(b)).token_for("t"),
        reference
    );
    let hex = raw.iter().fold(String::new(), |mut text, byte| {
        use std::fmt::Write;
        let _ = write!(text, "{byte:02x}");
        text
    });
    assert_eq!(
        CsrfKey::from_cookie_key(&Secret::new(hex))
            .expect("hex")
            .token_for("t"),
        reference
    );
}

#[test]
fn must_not_accept_a_template_or_malformed_cookie_key() {
    for bad in [
        String::new(),
        "short".to_owned(),
        STANDARD.encode([7_u8; 31]),
        STANDARD.encode([7_u8; 33]),
        STANDARD.encode([0_u8; 32]),
        STANDARD.encode([0xff_u8; 32]),
        "z".repeat(64),
    ] {
        assert_eq!(
            CsrfKey::from_cookie_key(&Secret::new(bad)).err(),
            Some(CsrfKeyError)
        );
    }
}

#[test]
fn must_not_print_the_key() {
    let rendered = format!("{:?}", key(3));
    assert_eq!(rendered, "CsrfKey([REDACTED])");
    assert_eq!(
        CsrfKeyError.to_string(),
        "the cookie key must be 32 bytes as base64 or 64 hexadecimal characters"
    );
}

#[test]
fn hex_decoding_is_exact() {
    assert_eq!(hex_decode("00ff10"), Some(vec![0, 255, 16]));
    assert_eq!(hex_decode("0g"), None);
    assert_eq!(hex_decode("abc"), None, "an odd length is not bytes");
}

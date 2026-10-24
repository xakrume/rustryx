use proptest::prelude::*;

use super::*;

#[test]
fn accepts_the_shape_of_a_chunked_session_token() {
    let token = format!(
        "{}.{}.handle-123_x~y",
        "018f3c1e-7b2a-7c00-8000-0123456789ab", "018f3c1e-7b2a-7c00-8000-0123456789ac"
    );
    assert_eq!(UploadSessionId::parse(&token).unwrap().as_str(), token);
}

#[test]
fn rejects_what_cannot_be_a_session() {
    let long = "a".repeat(MAX_SESSION_ID_LEN + 1);
    for bad in [
        "",
        "a b",
        "a/b",
        "a%2fb",
        "a\0b",
        "a\n",
        "\u{e9}",
        "a:b",
        "a?x=1",
        long.as_str(),
    ] {
        assert!(UploadSessionId::parse(bad).is_err(), "{bad:?}");
    }
    assert!(UploadSessionId::parse(&"a".repeat(MAX_SESSION_ID_LEN)).is_ok());
}

proptest! {
    #[test]
    fn parse_never_panics(text in ".{0,400}") {
        let _ = UploadSessionId::parse(&text);
    }

    #[test]
    fn valid_ids_round_trip(id in "[A-Za-z0-9._~-]{1,256}") {
        prop_assert_eq!(UploadSessionId::parse(&id).unwrap().to_string(), id);
    }
}

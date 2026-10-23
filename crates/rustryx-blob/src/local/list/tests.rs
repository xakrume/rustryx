use super::*;

#[test]
fn shard_names_are_two_lowercase_hex_digits() {
    for ok in ["00", "ff", "a1", "09"] {
        assert!(is_shard_name(ok), "{ok}");
    }
    for bad in ["", "0", "000", "FF", "g1", "_s", "..", "a/", "é1"] {
        assert!(!is_shard_name(bad), "{bad:?}");
    }
}

#[test]
fn a_cursor_must_be_the_location_of_a_valid_key() {
    let key = "0192f3a4-7b1c-7d2e-8f00-000000000001";
    let location = crate::key::relative_location(key);
    let cursor = Cursor::parse(&location).unwrap();
    assert_eq!(cursor.key, key);
    assert_eq!(
        format!("{}/{}/{}", cursor.first, cursor.second, cursor.key),
        location
    );

    for bad in [
        "",
        key,
        "00/11",
        "00/11/",
        // right shape, wrong shard for the key
        &format!("00/00/{key}"),
        // a traversal attempt dressed as a cursor
        "00/00/../../etc/passwd",
        "../../etc/passwd",
    ] {
        assert!(
            matches!(Cursor::parse(bad), Err(BlobError::InvalidKey(_))),
            "{bad:?}"
        );
    }
}

use proptest::prelude::*;

use super::*;

#[test]
fn accepts_names_of_the_specification() {
    for ok in [
        "a",
        "app",
        "team/app",
        "a/b/c/d",
        "my-app",
        "my--app",
        "my.app",
        "my_app",
        "my__app",
        "a1.b2_c3-d4",
        "0/1/2",
        "library/ubuntu",
        "x--y__z.w",
    ] {
        assert!(RepositoryName::parse(ok).is_ok(), "{ok}");
    }
}

#[test]
fn rejects_what_the_grammar_forbids() {
    let long = "a".repeat(256);
    for bad in [
        "",
        "/",
        "a/",
        "/a",
        "a//b",
        "A",
        "team/App",
        "-a",
        "a-",
        ".a",
        "a.",
        "_a",
        "a_",
        "a___b",
        "a_.b",
        "a.-b",
        "a-.b",
        "a..b",
        "a b",
        "a:b",
        "a@b",
        "a\\b",
        "a/../b",
        "..",
        "a/./b",
        "caf\u{e9}",
        "a\0b",
        long.as_str(),
    ] {
        assert!(RepositoryName::parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn the_longest_name_is_accepted() {
    let longest = "a".repeat(MAX_NAME_LEN);
    assert!(RepositoryName::parse(&longest).is_ok());
}

#[test]
fn leaf_is_the_last_component() {
    assert_eq!(RepositoryName::parse("team/app").unwrap().leaf(), "app");
    assert_eq!(RepositoryName::parse("app").unwrap().leaf(), "app");
}

#[test]
fn splits_a_full_name_into_repository_and_image() {
    let (repository, image) = split_full_name("docker-hosted/team/app").unwrap();
    assert_eq!(repository.as_str(), "docker-hosted");
    assert_eq!(image.as_str(), "team/app");
}

#[test]
fn a_full_name_needs_both_parts_and_a_lowercase_repository() {
    assert_eq!(
        split_full_name("docker-hosted"),
        Err(NameError::MissingImage)
    );
    assert_eq!(split_full_name("Docker/app"), Err(NameError::Invalid));
    assert_eq!(split_full_name("docker/App"), Err(NameError::Invalid));
    assert_eq!(split_full_name("/app"), Err(NameError::Invalid));
    assert_eq!(split_full_name("repo/"), Err(NameError::Invalid));
    assert_eq!(split_full_name(&"a/".repeat(300)), Err(NameError::Invalid));
}

proptest! {
    #[test]
    fn parse_never_panics(text in ".{0,300}") {
        let _ = RepositoryName::parse(&text);
        let _ = split_full_name(&text);
    }

    #[test]
    fn valid_names_round_trip(name in "[a-z0-9]{1,6}([._-][a-z0-9]{1,6}){0,3}(/[a-z0-9]{1,6}([._-][a-z0-9]{1,6}){0,3}){0,3}") {
        let parsed = RepositoryName::parse(&name).unwrap();
        prop_assert_eq!(parsed.as_str(), name.as_str());
        prop_assert_eq!(RepositoryName::parse(&parsed.to_string()).unwrap(), parsed);
    }

    #[test]
    fn an_uppercase_letter_always_fails(prefix in "[a-z]{0,5}", upper in "[A-Z]", suffix in "[a-z]{0,5}") {
        let name = format!("{prefix}{upper}{suffix}");
        prop_assert!(RepositoryName::parse(&name).is_err());
    }
}

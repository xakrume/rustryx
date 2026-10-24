//! Tests of the manifest parser.

use serde_json::json;

use super::*;
use crate::name::NameError;
use crate::testing::pubspec_text;
use crate::version::VersionError;

fn parse_text(text: &str) -> Result<Pubspec, PubspecError> {
    parse(text.as_bytes(), &PubspecLimits::default())
}

#[test]
fn reads_a_normal_manifest_into_json() {
    let spec = parse_text(&pubspec_text("demo", "1.2.3-beta.1+7")).unwrap();
    assert_eq!(spec.name.as_str(), "demo");
    assert_eq!(spec.version.to_string(), "1.2.3-beta.1+7");
    assert_eq!(spec.json["environment"], json!({ "sdk": "^3.0.0" }));
    assert_eq!(
        spec.json["dependencies"],
        json!({ "collection": "^1.17.0" })
    );
    assert_eq!(spec.json["name"], "demo");
}

#[test]
fn keeps_the_real_structure_of_a_richer_manifest() {
    let spec = parse_text(
        "name: rich\nversion: 2.0.0\nhomepage: https://example.com\n\
         flutter:\n  uses-material-design: true\n  assets:\n    - a.png\n    - b.png\n\
         dependency_overrides: {}\nissue_tracker: ~\nretries: 3\nratio: 0.5\n\
         dependencies:\n  foo:\n    version: ^1.0.0\n    hosted: https://example.com\n",
    )
    .unwrap();
    assert_eq!(spec.json["flutter"]["assets"], json!(["a.png", "b.png"]));
    assert_eq!(spec.json["flutter"]["uses-material-design"], json!(true));
    assert_eq!(spec.json["issue_tracker"], json!(null));
    assert_eq!(spec.json["retries"], json!(3));
    assert_eq!(spec.json["ratio"], json!(0.5));
    assert_eq!(
        spec.json["dependencies"]["foo"]["hosted"],
        "https://example.com"
    );
}

#[test]
fn a_byte_order_mark_is_ignored() {
    let text = format!("\u{feff}{}", pubspec_text("demo", "1.0.0"));
    assert!(parse_text(&text).is_ok());
}

#[test]
fn must_not_accept_a_manifest_that_is_not_text() {
    assert_eq!(
        parse(&[0xff, 0xfe, b'n'], &PubspecLimits::default()),
        Err(PubspecError::Encoding)
    );
    assert_eq!(
        parse_text("name: a\0b\nversion: 1.0.0\n"),
        Err(PubspecError::Encoding)
    );
}

#[test]
fn must_not_accept_an_oversized_manifest() {
    let limits = PubspecLimits {
        max_bytes: 64,
        ..PubspecLimits::default()
    };
    assert_eq!(
        parse(pubspec_text("demo", "1.0.0").as_bytes(), &limits),
        Err(PubspecError::TooLarge)
    );
}

#[test]
fn the_top_level_must_be_a_mapping_with_a_name_and_a_version() {
    assert_eq!(parse_text("- a\n- b\n"), Err(PubspecError::NotAMapping));
    assert_eq!(parse_text("just text\n"), Err(PubspecError::NotAMapping));
    assert_eq!(parse_text(""), Err(PubspecError::NotAMapping));
    assert_eq!(
        parse_text("version: 1.0.0\n"),
        Err(PubspecError::MissingName)
    );
    assert_eq!(
        parse_text("name: 12\nversion: 1.0.0\n"),
        Err(PubspecError::MissingName)
    );
    assert_eq!(
        parse_text("name: demo\n"),
        Err(PubspecError::MissingVersion)
    );
    // `1.0` is a YAML number, not a version string.
    assert_eq!(
        parse_text("name: demo\nversion: 1.0\n"),
        Err(PubspecError::MissingVersion)
    );
}

#[test]
fn must_not_accept_an_invalid_name_or_version() {
    assert_eq!(
        parse_text("name: Demo\nversion: 1.0.0\n"),
        Err(PubspecError::InvalidName(NameError::BadStart))
    );
    assert_eq!(
        parse_text("name: demo-pkg\nversion: 1.0.0\n"),
        Err(PubspecError::InvalidName(NameError::BadChar))
    );
    assert_eq!(
        parse_text("name: demo\nversion: \"1.0\"\n"),
        Err(PubspecError::InvalidVersion(VersionError::Invalid))
    );
    assert_eq!(
        parse_text("name: demo\nversion: \"01.0.0\"\n"),
        Err(PubspecError::InvalidVersion(VersionError::Invalid))
    );
    assert_eq!(
        parse_text("name: demo\nversion: \"\"\n"),
        Err(PubspecError::InvalidVersion(VersionError::Empty))
    );
}

#[test]
fn syntax_errors_report_a_position_and_never_the_content() {
    let error = parse_text("name: demo\nversion: [SECRETVALUE\n").unwrap_err();
    let PubspecError::Yaml { line, column } = &error else {
        panic!("{error:?}");
    };
    assert!(line.is_some() && column.is_some());
    assert!(error.to_string().contains("line"));
    assert!(!error.to_string().contains("SECRETVALUE"));
    // More than one document is refused by the library.
    assert!(matches!(
        parse_text("name: a\nversion: 1.0.0\n---\nname: b\n"),
        Err(PubspecError::Yaml { .. })
    ));
}

#[test]
fn must_not_accept_ambiguous_documents() {
    assert_eq!(
        parse_text("name: demo\nversion: 1.0.0\nname: other\n"),
        Err(PubspecError::DuplicateKey)
    );
    assert_eq!(
        parse_text("name: demo\nversion: 1.0.0\n1: one\n"),
        Err(PubspecError::NonStringKey)
    );
    assert_eq!(
        parse_text("name: demo\nversion: 1.0.0\ntrue: yes\n"),
        Err(PubspecError::NonStringKey)
    );
    assert_eq!(
        parse_text("name: demo\nversion: 1.0.0\n? [a, b]\n: c\n"),
        Err(PubspecError::NonStringKey)
    );
    assert_eq!(
        parse_text("name: demo\nversion: 1.0.0\nx: .nan\n"),
        Err(PubspecError::NonFiniteNumber)
    );
    assert_eq!(
        parse_text("name: demo\nversion: 1.0.0\nx: .inf\n"),
        Err(PubspecError::NonFiniteNumber)
    );
    // A tagged value is not guessed at.
    for text in ["x: !custom 1\n", "x: !custom {a: 1}\n", "x: !custom [a]\n"] {
        assert!(
            matches!(
                parse_text(&format!("name: demo\nversion: 1.0.0\n{text}")),
                Err(PubspecError::Yaml { .. })
            ),
            "{text}"
        );
    }
    // The tags of the YAML core schema are resolved by the library into plain scalars.
    let spec = parse_text("name: demo\nversion: 1.0.0\nx: !!str 5\ny: !!int '7'\n").unwrap();
    assert_eq!(spec.json["x"], json!("5"));
    assert_eq!(spec.json["y"], json!(7));
}

#[test]
fn must_not_expand_an_alias_bomb() {
    // Each level is ten references to the one below: 10^9 values from a few hundred bytes.
    let mut text =
        String::from("name: demo\nversion: 1.0.0\nl0: &l0 [x, x, x, x, x, x, x, x, x, x]\n");
    for level in 1..=9 {
        let refs = vec![format!("*l{}", level - 1); 10].join(", ");
        text.push_str(&format!("l{level}: &l{level} [{refs}]\n"));
    }
    assert!(text.len() < 1024);
    let started = std::time::Instant::now();
    assert_eq!(parse_text(&text), Err(PubspecError::Complexity));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "the bomb must be stopped, not slowly expanded"
    );
}

#[test]
fn small_aliases_still_work() {
    let spec = parse_text("name: demo\nversion: 1.0.0\na: &x [1, 2]\nb: *x\n").unwrap();
    assert_eq!(spec.json["b"], json!([1, 2]));
}

#[test]
fn must_not_accept_deep_nesting_or_huge_strings() {
    let deep = format!(
        "name: demo\nversion: 1.0.0\nx: {}{}\n",
        "[".repeat(40),
        "]".repeat(40)
    );
    assert_eq!(parse_text(&deep), Err(PubspecError::Depth));
    let limits = PubspecLimits {
        max_string_bytes: 8,
        ..PubspecLimits::default()
    };
    assert_eq!(
        parse(
            b"name: demo\nversion: 1.0.0\ndescription: a long long text\n",
            &limits
        ),
        Err(PubspecError::StringTooLong)
    );
    let few = PubspecLimits {
        max_nodes: 5,
        ..PubspecLimits::default()
    };
    assert_eq!(
        parse(pubspec_text("demo", "1.0.0").as_bytes(), &few),
        Err(PubspecError::Complexity)
    );
}

#[test]
fn reasons_are_a_closed_set() {
    let errors = [
        PubspecError::TooLarge,
        PubspecError::Encoding,
        PubspecError::Yaml {
            line: None,
            column: None,
        },
        PubspecError::Complexity,
        PubspecError::Depth,
        PubspecError::StringTooLong,
        PubspecError::DuplicateKey,
        PubspecError::NonStringKey,
        PubspecError::NonFiniteNumber,
        PubspecError::NotAMapping,
        PubspecError::MissingName,
        PubspecError::InvalidName(NameError::Empty),
        PubspecError::MissingVersion,
        PubspecError::InvalidVersion(VersionError::Empty),
    ];
    for error in errors {
        assert!(error.reason().starts_with("pubspec_"));
        assert!(error.to_string().starts_with("pubspec.yaml"));
    }
    assert_eq!(
        PubspecError::Yaml {
            line: None,
            column: None
        }
        .to_string(),
        "pubspec.yaml is not valid YAML"
    );
}

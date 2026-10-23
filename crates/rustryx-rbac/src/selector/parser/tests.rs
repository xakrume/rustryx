//! Tests of the parser: grammar, typing and the limits that apply at compile time.

use super::*;

fn limits() -> SelectorLimits {
    SelectorLimits::default()
}

fn err(src: &str) -> SelectorCompileError {
    parse(src, &limits()).unwrap_err()
}

#[test]
fn parses_precedence_and_flattens_chains() {
    let root = parse(
        "format == \"a\" || format == \"b\" && path == \"c\" || !(path == \"d\")",
        &limits(),
    )
    .unwrap();
    let BoolExpr::Or(items) = root else {
        unreachable!("top level is an or");
    };
    assert_eq!(items.len(), 3);
    assert!(matches!(items[1], BoolExpr::And(ref v) if v.len() == 2));
    assert!(matches!(items[2], BoolExpr::Not(_)));
}

#[test]
fn reports_type_errors() {
    for src in [
        "path",
        "path && true",
        "true || path",
        "!path",
        "path == true",
        "format in path",
        "coordinate == \"x\"",
        "coordinate.name == coordinate",
        "path.startsWith(true)",
        "true.startsWith(\"a\")",
        "path =~ format",
        "path.matches(format)",
        "path =~ 1",
    ] {
        let e = err(src);
        assert!(
            matches!(
                e,
                SelectorCompileError::Type { .. } | SelectorCompileError::Syntax { .. }
            ),
            "{src}: {e:?}"
        );
    }
    assert!(matches!(
        err("path && true"),
        SelectorCompileError::Type { .. }
    ));
    assert!(matches!(
        err("path =~ format"),
        SelectorCompileError::Type { .. }
    ));
}

#[test]
fn reports_syntax_errors() {
    for src in [
        "format ==",
        "path.startsWith(",
        "path.startsWith()",
        "path.startsWith(\"a\", \"b\")",
        "path.startsWith \"a\"",
        "(true",
        "true)",
        "true true",
        "unknown == \"x\"",
        "coordinate.packaging == \"jar\"",
        "coordinate.Extension == \"jar\"",
        "coordinate.extension()",
        "path.length == 1",
        "path.substring(1) == \"x\"",
        "format in []",
        "format in [\"a\",]",
        "format in [path]",
        "format in [\"a\"",
        "1 + 1 == 2",
        "path.",
        "path.(",
        "== true",
        "in",
    ] {
        assert!(
            matches!(err(src), SelectorCompileError::Syntax { .. }),
            "{src}: {:?}",
            err(src)
        );
    }
}

#[test]
fn empty_and_oversized_expressions_are_rejected() {
    assert_eq!(err(""), SelectorCompileError::Empty);
    assert_eq!(err(" \t\n"), SelectorCompileError::Empty);
    let long = format!("path == \"{}\"", "a".repeat(2000));
    assert!(matches!(
        err(&long),
        SelectorCompileError::TooLong { max: 1024, .. }
    ));
}

#[test]
fn nesting_is_capped_for_parentheses_negations_and_calls() {
    let deep_parens = format!("{}true{}", "(".repeat(500), ")".repeat(500));
    assert!(matches!(
        err(&deep_parens),
        SelectorCompileError::TooDeep { max: 32 }
    ));
    let deep_not = format!("{}true", "!".repeat(500));
    assert!(matches!(
        err(&deep_not),
        SelectorCompileError::TooDeep { max: 32 }
    ));
    // The depth error comes while descending, before the type error of the
    // innermost call is noticed.
    let deep_call = format!("{}path{}", "path.contains(".repeat(40), ")".repeat(40));
    assert!(matches!(
        err(&deep_call),
        SelectorCompileError::TooDeep { max: 32 }
    ));
    // Just inside the limit still parses.
    let ok = format!("{}true{}", "(".repeat(30), ")".repeat(30));
    assert!(parse(&ok, &limits()).is_ok());
}

#[test]
fn list_size_is_capped() {
    let items = (0..65)
        .map(|i| format!("\"{i}\""))
        .collect::<Vec<_>>()
        .join(",");
    assert!(matches!(
        err(&format!("format in [{items}]")),
        SelectorCompileError::Syntax { .. }
    ));
    let items = (0..64)
        .map(|i| format!("\"{i}\""))
        .collect::<Vec<_>>()
        .join(",");
    assert!(parse(&format!("format in [{items}]"), &limits()).is_ok());
}

#[test]
fn regex_limits_are_enforced_when_the_selector_is_saved() {
    assert!(matches!(
        err("path =~ \"(\""),
        SelectorCompileError::Regex { .. }
    ));
    assert!(matches!(
        err("path.matches(\"[\")"),
        SelectorCompileError::Regex { .. }
    ));
    // Exceeds the compiled size limit: 100 x 100 x 100 repetitions.
    assert!(matches!(
        err("path.matches(\"((a{100}){100}){100}\")"),
        SelectorCompileError::Regex { .. }
    ));
    let long_pattern = format!("path.matches(\"{}\")", "a".repeat(600));
    assert!(matches!(
        err(&long_pattern),
        SelectorCompileError::Regex { .. }
    ));
    // Nesting beyond the parser limit.
    let nested = format!("path.matches(\"{}a{}\")", "(".repeat(40), ")".repeat(40));
    assert!(matches!(err(&nested), SelectorCompileError::Regex { .. }));
}

#[test]
fn regex_that_only_works_inside_the_anchor_wrapper_is_rejected() {
    // Valid once wrapped as \A(?:...)\z, invalid on its own.
    assert!(matches!(
        err("path =~ \"a)|(b\""),
        SelectorCompileError::Regex { .. }
    ));
}

#[test]
fn typical_unicode_class_pattern_fits_the_default_limits() {
    assert!(parse("path =~ \"^/com/[\\\\w.-]+/.*$\"", &limits()).is_ok());
}

#[test]
fn extension_and_classifier_are_coordinate_fields() {
    for (src, field) in [
        ("coordinate.extension == \"jar\"", CoordField::Extension),
        (
            "coordinate.classifier == \"sources\"",
            CoordField::Classifier,
        ),
    ] {
        let root = parse(src, &limits()).unwrap();
        let BoolExpr::Eq(StrExpr::Coord(got), StrExpr::Lit(_)) = root else {
            unreachable!("{src} parses as an equality on a coordinate field");
        };
        assert_eq!(got, field, "{src}");
    }
}

#[test]
fn the_error_for_an_unknown_coordinate_field_lists_every_field() {
    let message = err("coordinate.packaging == \"jar\"").to_string();
    for field in [
        "group",
        "groupId",
        "name",
        "artifactId",
        "version",
        "extension",
        "classifier",
    ] {
        assert!(message.contains(field), "{message}");
    }
}

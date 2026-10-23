//! `coordinate.extension` and `coordinate.classifier` (core gap 12): the maven style fields
//! of a content selector, from the language level down to a privilege decision.
//!
//! The contract these tests pin: a coordinate field that is `None` is UNKNOWN, not empty. A
//! format that can tell that a component has no classifier passes `Some("")`; a selector then
//! matches on the empty string. Anything else would let `!(coordinate.classifier == "x")`
//! grant access on data nobody supplied.

// Tests may unwrap and expect: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Env, principal, resource, selector_grant};
use rustryx_core::identity::Action;
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::auth::{Coordinate, Decision, RbacChecker};
use rustryx_rbac::{RestrictedEngine, SelectorEngine, SelectorEvalError, SelectorInput};

fn maven(extension: Option<&str>, classifier: Option<&str>) -> Coordinate {
    let mut coordinate = Coordinate::new(
        Some("com.acme".into()),
        Some("lib".into()),
        Some("1.0".into()),
    );
    coordinate.extension = extension.map(str::to_owned);
    coordinate.classifier = classifier.map(str::to_owned);
    coordinate
}

/// Compiles and evaluates a selector with the restricted engine of this crate (a typed parser
/// and evaluator, not a general purpose `eval`).
fn eval(expression: &str, coordinate: Option<&Coordinate>) -> Result<bool, SelectorEvalError> {
    let engine = RestrictedEngine::default();
    let program = engine.compile(expression).unwrap();
    engine.evaluate(
        &program,
        &SelectorInput {
            format: RepoFormat::Maven2,
            path: "/com/acme/lib/1.0/lib-1.0.jar",
            repository: "releases",
            coordinate,
        },
    )
}

#[test]
fn extension_and_classifier_compare_like_any_other_coordinate_field() {
    let jar = maven(Some("jar"), Some(""));
    let sources = maven(Some("jar"), Some("sources"));
    let pom = maven(Some("pom"), Some(""));
    assert_eq!(
        eval("coordinate.extension == \"jar\"", Some(&jar)),
        Ok(true)
    );
    assert_eq!(
        eval("coordinate.extension == \"jar\"", Some(&pom)),
        Ok(false)
    );
    assert_eq!(
        eval("coordinate.classifier == \"sources\"", Some(&sources)),
        Ok(true)
    );
    assert_eq!(
        eval("coordinate.classifier == \"sources\"", Some(&jar)),
        Ok(false)
    );
    assert_eq!(eval("coordinate.classifier == \"\"", Some(&jar)), Ok(true));
    // Both are exposed next to the older fields, in one expression.
    let release_jar = "coordinate.groupId == \"com.acme\" && coordinate.extension == \"jar\" && coordinate.classifier == \"\"";
    assert_eq!(eval(release_jar, Some(&jar)), Ok(true));
    assert_eq!(eval(release_jar, Some(&sources)), Ok(false));
    assert_eq!(eval(release_jar, Some(&pom)), Ok(false));
}

#[test]
fn the_string_methods_and_regexes_work_on_both_fields() {
    let javadoc = maven(Some("jar"), Some("javadoc"));
    assert_eq!(
        eval("coordinate.classifier.startsWith(\"java\")", Some(&javadoc)),
        Ok(true)
    );
    assert_eq!(
        eval("coordinate.classifier.endsWith(\"doc\")", Some(&javadoc)),
        Ok(true)
    );
    assert_eq!(
        eval("coordinate.classifier.contains(\"vad\")", Some(&javadoc)),
        Ok(true)
    );
    assert_eq!(
        eval("coordinate.extension in [\"jar\", \"war\"]", Some(&javadoc)),
        Ok(true)
    );
    assert_eq!(
        eval("coordinate.extension in [\"pom\", \"war\"]", Some(&javadoc)),
        Ok(false)
    );
    // `=~` matches the whole string, `matches` any part of it.
    assert_eq!(
        eval("coordinate.classifier =~ \"java\"", Some(&javadoc)),
        Ok(false)
    );
    assert_eq!(
        eval("coordinate.classifier =~ \"java.*\"", Some(&javadoc)),
        Ok(true)
    );
    assert_eq!(
        eval("coordinate.classifier.matches(\"vad\")", Some(&javadoc)),
        Ok(true)
    );
    assert_eq!(
        eval("coordinate.extension =~ \"jar|war\"", Some(&javadoc)),
        Ok(true)
    );
}

#[test]
fn a_missing_field_is_unknown_and_named_in_the_error() {
    let no_extension = maven(None, Some(""));
    let no_classifier = maven(Some("jar"), None);
    assert_eq!(
        eval("coordinate.extension == \"jar\"", Some(&no_extension)),
        Err(SelectorEvalError::MissingInput("coordinate.extension"))
    );
    assert_eq!(
        eval("coordinate.classifier == \"\"", Some(&no_classifier)),
        Err(SelectorEvalError::MissingInput("coordinate.classifier"))
    );
    assert_eq!(
        eval("coordinate.classifier == \"\"", None),
        Err(SelectorEvalError::MissingInput("coordinate"))
    );
    // The other fields are still evaluated: only what the expression needs must be known.
    assert_eq!(
        eval("coordinate.extension == \"jar\"", Some(&no_classifier)),
        Ok(true)
    );
}

#[test]
fn unknown_absorbs_like_cel_errors() {
    let no_classifier = maven(Some("jar"), None);
    // A decisive operand wins over an unknown one, in either position.
    assert_eq!(
        eval(
            "coordinate.classifier == \"sources\" && coordinate.extension == \"pom\"",
            Some(&no_classifier)
        ),
        Ok(false)
    );
    assert_eq!(
        eval(
            "coordinate.extension == \"jar\" || coordinate.classifier == \"sources\"",
            Some(&no_classifier)
        ),
        Ok(true)
    );
    // Without a decisive operand the unknown survives.
    assert!(
        eval(
            "coordinate.extension == \"jar\" && coordinate.classifier == \"sources\"",
            Some(&no_classifier)
        )
        .is_err()
    );
}

#[test]
fn must_not_match_a_negated_comparison_on_an_unknown_classifier() {
    // "Everything except sources" must not grant a component whose classifier nobody told us.
    let no_classifier = maven(Some("jar"), None);
    for expression in [
        "!(coordinate.classifier == \"sources\")",
        "coordinate.classifier != \"sources\"",
        "!(coordinate.extension == \"pom\") && coordinate.classifier != \"sources\"",
    ] {
        assert!(
            eval(expression, Some(&no_classifier)).is_err(),
            "{expression} must have no definite answer"
        );
    }
    let no_extension = maven(None, Some(""));
    for expression in [
        "!(coordinate.extension == \"pom\")",
        "coordinate.extension != \"pom\"",
    ] {
        assert!(
            eval(expression, Some(&no_extension)).is_err(),
            "{expression}"
        );
    }
}

#[test]
fn oversized_extension_and_classifier_are_evaluation_errors() {
    let huge = "x".repeat(8193);
    for (coordinate, field) in [
        (maven(Some(&huge), Some("")), "coordinate.extension"),
        (maven(Some("jar"), Some(&huge)), "coordinate.classifier"),
    ] {
        let err = eval("true", Some(&coordinate)).unwrap_err();
        assert!(
            matches!(err, SelectorEvalError::InputTooLarge { field: got, .. } if got == field),
            "{field}: {err:?}"
        );
    }
    let at_limit = "x".repeat(8192);
    assert_eq!(
        eval("true", Some(&maven(Some(&at_limit), Some("")))),
        Ok(true)
    );
}

#[tokio::test]
async fn a_selector_privilege_can_hand_out_release_jars_but_not_source_jars() {
    let env = Env::new();
    let selector = env
        .selector(
            "release-jars",
            "coordinate.extension == \"jar\" && coordinate.classifier == \"\"",
        )
        .await;
    let role = env
        .simple_role(
            "release-reader",
            selector_grant("releases", selector),
            &[Action::Read],
        )
        .await;
    let user = principal(&[(role, "release-reader")]);
    let check = |extension: Option<&str>, classifier: Option<&str>| {
        let resource = resource("releases", RepoFormat::Maven2, "/p")
            .with_coordinate(maven(extension, classifier));
        let (env, user) = (&env, &user);
        async move { env.rbac.check(user, Action::Read, &resource).await.unwrap() }
    };
    assert_eq!(check(Some("jar"), Some("")).await, Decision::Allow);
    assert_eq!(check(Some("jar"), Some("sources")).await, Decision::Deny);
    assert_eq!(check(Some("pom"), Some("")).await, Decision::Deny);
    assert_eq!(
        check(Some("jar"), None).await,
        Decision::Deny,
        "an unknown classifier is not the empty classifier"
    );
}

#[tokio::test]
async fn group_filtering_passes_extension_and_classifier_to_every_member() {
    let env = Env::new();
    let selector = env
        .selector("sources-only", "coordinate.classifier == \"sources\"")
        .await;
    let role = env
        .simple_role(
            "sources-reader",
            selector_grant("*", selector),
            &[Action::Read],
        )
        .await;
    let user = principal(&[(role, "sources-reader")]);
    let members = vec![
        common::repo("m1", RepoFormat::Maven2),
        common::repo("m2", RepoFormat::Maven2),
    ];
    let hit = maven(Some("jar"), Some("sources"));
    let miss = maven(Some("jar"), Some(""));
    let visible = env
        .rbac
        .filter_visible_members(&user, Action::Read, &members, "/p", Some(&hit))
        .await
        .unwrap();
    assert_eq!(visible.len(), 2);
    let visible = env
        .rbac
        .filter_visible_members(&user, Action::Read, &members, "/p", Some(&miss))
        .await
        .unwrap();
    assert_eq!(visible.len(), 0);
}

#[test]
fn selectors_that_name_the_fields_compile_and_unknown_fields_still_do_not() {
    let engine = RestrictedEngine::default();
    assert!(engine.compile("coordinate.extension == \"jar\"").is_ok());
    assert!(
        engine
            .compile("coordinate.classifier == \"sources\"")
            .is_ok()
    );
    for expression in [
        "coordinate.packaging == \"jar\"",
        "coordinate.Extension == \"jar\"",
        "coordinate.extensions == \"jar\"",
        "coordinate.extension",
        "coordinate.classifier()",
    ] {
        assert!(engine.compile(expression).is_err(), "{expression}");
    }
}

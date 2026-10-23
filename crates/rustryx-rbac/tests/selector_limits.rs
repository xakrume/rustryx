//! Safety limits and semantics of the selector engine that the corpus does not
//! pin down: size and depth caps, regex cost, unknown inputs, anchoring.

// Tests may unwrap and expect: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use rustryx_core::model::RepoFormat;
use rustryx_core::ports::auth::Coordinate;
use rustryx_rbac::{
    RestrictedEngine, SelectorCompileError, SelectorEngine, SelectorEvalError, SelectorInput,
    SelectorLimits,
};

fn eval_with(
    engine: &RestrictedEngine,
    expression: &str,
    path: &str,
    coordinate: Option<&Coordinate>,
) -> Result<bool, SelectorEvalError> {
    let program = engine.compile(expression).unwrap();
    engine.evaluate(
        &program,
        &SelectorInput {
            format: RepoFormat::Raw,
            path,
            repository: "r",
            coordinate,
        },
    )
}

fn eval(expression: &str, path: &str) -> Result<bool, SelectorEvalError> {
    eval_with(&RestrictedEngine::default(), expression, path, None)
}

#[test]
fn default_limits_match_the_documented_values() {
    let limits = SelectorLimits::default();
    assert_eq!(limits.max_expression_bytes, 1024);
    assert_eq!(limits.max_depth, 32);
    assert_eq!(limits.max_list_items, 64);
    assert_eq!(limits.max_regex_pattern_bytes, 512);
    assert_eq!(limits.max_regex_program_bytes, 1024 * 1024);
    assert_eq!(limits.max_input_bytes, 8192);
    assert_eq!(limits.max_eval_steps, 4096);
    assert_eq!(RestrictedEngine::default().limits(), &limits);
}

#[test]
fn expression_length_limit_is_exactly_one_kilobyte() {
    let engine = RestrictedEngine::default();
    let prefix = "path == \"";
    let at_limit = format!("{prefix}{}\"", "a".repeat(1024 - prefix.len() - 1));
    assert_eq!(at_limit.len(), 1024);
    assert!(engine.compile(&at_limit).is_ok());
    let over = format!("{prefix}{}\"", "a".repeat(1024 - prefix.len()));
    assert_eq!(
        engine.compile(&over).unwrap_err(),
        SelectorCompileError::TooLong {
            len: 1025,
            max: 1024
        }
    );
}

#[test]
fn limits_are_configurable() {
    let engine = RestrictedEngine::new(SelectorLimits {
        max_expression_bytes: 20,
        max_depth: 2,
        ..SelectorLimits::default()
    });
    assert!(
        engine
            .compile("format == \"npm\" && path == \"/x/y/z\"")
            .is_err()
    );
    assert!(engine.compile("((true))").is_ok());
    assert!(matches!(
        engine.compile("(((true)))").unwrap_err(),
        SelectorCompileError::TooDeep { max: 2 }
    ));
}

#[test]
fn nested_expression_beyond_the_stack_budget_is_a_compile_error_not_a_crash() {
    // 1 KB of parentheses: a recursive parser without a depth cap would overflow
    // the stack of a worker thread (the cel crate does in a debug build).
    let hostile = format!("{}true{}", "(".repeat(500), ")".repeat(500));
    assert!(matches!(
        RestrictedEngine::default().compile(&hostile).unwrap_err(),
        SelectorCompileError::TooDeep { .. }
    ));
}

#[test]
fn catastrophic_backtracking_patterns_stay_linear() {
    let input = format!("/{}b", "a".repeat(6000));
    for pattern in ["(a+)+$", "(a*)*c", "(a|aa)+$", "^(a+)+$", "(.*a){12}x"] {
        for expression in [
            format!("path.matches(\"{pattern}\")"),
            format!("path =~ \"{pattern}\""),
        ] {
            let started = Instant::now();
            let result = eval(&expression, &input);
            assert!(result.is_ok(), "{expression}");
            // Backtracking would need years; a linear engine needs milliseconds.
            // The bound is generous so that a loaded CI machine cannot flake it.
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "{expression} took {:?}",
                started.elapsed()
            );
        }
    }
}

#[test]
fn oversized_inputs_are_evaluation_errors() {
    let engine = RestrictedEngine::default();
    let huge = "a".repeat(8193);
    assert_eq!(
        eval_with(&engine, "true", &huge, None).unwrap_err(),
        SelectorEvalError::InputTooLarge {
            field: "path",
            len: 8193,
            max: 8192
        }
    );
    let at_limit = "a".repeat(8192);
    assert_eq!(eval_with(&engine, "true", &at_limit, None), Ok(true));
    let coordinate = Coordinate::new(Some(huge.clone()), None, None);
    let err = eval_with(&engine, "true", "/x", Some(&coordinate)).unwrap_err();
    assert!(matches!(
        err,
        SelectorEvalError::InputTooLarge {
            field: "coordinate.group",
            ..
        }
    ));
    let program = engine.compile("true").unwrap();
    let long_repository = "r".repeat(9000);
    let err = engine
        .evaluate(
            &program,
            &SelectorInput {
                format: RepoFormat::Raw,
                path: "/x",
                repository: &long_repository,
                coordinate: None,
            },
        )
        .unwrap_err();
    assert!(matches!(
        err,
        SelectorEvalError::InputTooLarge {
            field: "repository",
            ..
        }
    ));
}

#[test]
fn step_budget_stops_an_evaluation() {
    let engine = RestrictedEngine::new(SelectorLimits {
        max_eval_steps: 3,
        ..SelectorLimits::default()
    });
    assert_eq!(eval_with(&engine, "true", "/x", None), Ok(true));
    let program = engine
        .compile("format == \"raw\" && path == \"/x\"")
        .unwrap();
    let err = engine
        .evaluate(
            &program,
            &SelectorInput {
                format: RepoFormat::Raw,
                path: "/x",
                repository: "r",
                coordinate: None,
            },
        )
        .unwrap_err();
    assert_eq!(err, SelectorEvalError::StepBudgetExceeded { max: 3 });
    assert!(err.to_string().contains("budget"));
}

#[test]
fn default_step_budget_is_out_of_reach_of_a_maximal_expression() {
    // About 340 comparisons fit in 1 KB; each visits three nodes.
    let clause = "path==\"a\"||";
    let expression = format!("{}path==\"a\"", clause.repeat(90));
    assert!(expression.len() <= 1024);
    assert_eq!(eval(&expression, "/b"), Ok(false));
}

#[test]
fn unknown_coordinate_parts_name_what_is_missing() {
    let engine = RestrictedEngine::default();
    let partial = Coordinate::new(None, Some("n".into()), None);
    assert_eq!(
        eval_with(&engine, "coordinate.group == \"g\"", "/x", Some(&partial)),
        Err(SelectorEvalError::MissingInput("coordinate.group"))
    );
    assert_eq!(
        eval_with(&engine, "coordinate.version == \"1\"", "/x", Some(&partial)),
        Err(SelectorEvalError::MissingInput("coordinate.version"))
    );
    assert_eq!(
        eval_with(&engine, "coordinate.name == \"n\"", "/x", Some(&partial)),
        Ok(true)
    );
    assert_eq!(
        eval_with(&engine, "coordinate.name == \"n\"", "/x", None),
        Err(SelectorEvalError::MissingInput("coordinate"))
    );
}

#[test]
fn unknown_cannot_become_a_match_through_negation_or_comparison() {
    let engine = RestrictedEngine::default();
    for expression in [
        "!(coordinate.group == \"evil\")",
        "coordinate.group != \"evil\"",
        "!(coordinate.group in [\"evil\"])",
        "!coordinate.group.startsWith(\"evil\")",
        "!coordinate.group.matches(\"evil\")",
        "!(coordinate.group =~ \"evil\")",
        "path.startsWith(\"/ok/\") && !(coordinate.group == \"evil\")",
        "false || !(coordinate.group == \"evil\")",
    ] {
        assert!(
            eval_with(&engine, expression, "/ok/a", None).is_err(),
            "{expression} must not decide without a coordinate"
        );
    }
    // Short circuiting that the other operand makes decisive is still sound.
    assert_eq!(
        eval_with(
            &engine,
            "path.startsWith(\"/ok/\") || coordinate.group == \"x\"",
            "/ok/a",
            None
        ),
        Ok(true)
    );
    assert_eq!(
        eval_with(
            &engine,
            "path.startsWith(\"/no/\") && coordinate.group == \"x\"",
            "/ok/a",
            None
        ),
        Ok(false)
    );
}

#[test]
fn equals_regex_operator_must_match_the_whole_string() {
    // `=~` is anchored as a whole, including alternations: a|ab on "ab" matches
    // only because the pattern is wrapped in a group before it is anchored.
    assert_eq!(eval("path =~ \"/a|/ab\"", "/ab"), Ok(true));
    // Without the group the pattern would read \A/a|/b\z and match "/ax" through
    // its first alternative and "/x/b" through its second.
    assert_eq!(eval("path =~ \"/a|/b\"", "/ax"), Ok(false));
    assert_eq!(eval("path =~ \"/a|/b\"", "/x/b"), Ok(false));
    assert_eq!(eval("path =~ \"/a|/b\"", "/b"), Ok(true));
    assert_eq!(eval("path =~ \"/a\"", "/ab"), Ok(false));
    assert_eq!(eval("path =~ \"a\"", "/a"), Ok(false));
    // `matches` may match a substring.
    assert_eq!(eval("path.matches(\"a\")", "/xay"), Ok(true));
}

#[test]
fn regex_flags_inside_the_pattern_cannot_move_the_anchors() {
    assert_eq!(eval("path =~ \"(?m)/a\"", "/a\n/b"), Ok(false));
    assert_eq!(eval("path =~ \"(?s)/a.b\"", "/a\nb"), Ok(true));
    assert_eq!(eval("path =~ \"(?i)/ABC\"", "/abc"), Ok(true));
    // The case-insensitive flag is local to its group.
    assert_eq!(eval("path =~ \"(?:(?i)/A)bc\"", "/aBC"), Ok(false));
    assert_eq!(eval("path =~ \"(?:(?i)/A)bc\"", "/Abc"), Ok(true));
}

#[test]
fn comparison_is_case_sensitive_and_exact() {
    assert_eq!(eval("path == \"/Org\"", "/org"), Ok(false));
    assert_eq!(eval("path.startsWith(\"/org/\")", "/ORG/x"), Ok(false));
    assert_eq!(eval("path == \"/a\"", "/a "), Ok(false));
    assert_eq!(eval("path.contains(\"\")", "/a"), Ok(true));
}

#[test]
fn non_ascii_text_is_handled_by_characters_not_bytes() {
    assert_eq!(
        eval(
            "path.startsWith(\"/\u{43f}\u{443}\u{442}\u{44c}/\")",
            "/\u{43f}\u{443}\u{442}\u{44c}/x"
        ),
        Ok(true)
    );
    assert_eq!(eval("path =~ \"/.\"", "/\u{44f}"), Ok(true));
    assert_eq!(eval("path =~ \"/..\"", "/\u{44f}"), Ok(false));
}

#[test]
fn every_repository_format_label_is_visible_to_selectors() {
    let engine = RestrictedEngine::default();
    for &format in RepoFormat::ALL {
        let program = engine
            .compile(&format!("format == \"{}\"", format.as_str()))
            .unwrap();
        let input = SelectorInput {
            format,
            path: "/x",
            repository: "r",
            coordinate: None,
        };
        assert_eq!(engine.evaluate(&program, &input), Ok(true), "{format}");
    }
}

#[test]
fn compiled_programs_are_shared_between_threads() {
    let engine = RestrictedEngine::default();
    let program = engine
        .compile("path.startsWith(\"/a/\") && format == \"raw\"")
        .unwrap();
    std::thread::scope(|scope| {
        for i in 0..4 {
            let (engine, program) = (&engine, &program);
            scope.spawn(move || {
                let path = format!("/a/{i}");
                let input = SelectorInput {
                    format: RepoFormat::Raw,
                    path: &path,
                    repository: "r",
                    coordinate: None,
                };
                assert_eq!(engine.evaluate(program, &input), Ok(true));
            });
        }
    });
}

#[test]
fn error_messages_are_readable_and_carry_positions() {
    let engine = RestrictedEngine::default();
    for (expression, needle) in [
        ("", "empty"),
        ("path ==", "end of expression"),
        ("path.trim() == \"x\"", "unsupported method 'trim'"),
        ("path == 1", "unexpected character '1'"),
        (
            "coordinate.packaging == \"jar\"",
            "unknown coordinate field 'packaging'",
        ),
        ("unknown", "unknown identifier 'unknown'"),
        ("path && true", "must be a boolean"),
        ("path =~ \"(\"", "regular expression"),
        ("path == \"\\q\"", "unknown escape"),
    ] {
        let message = engine.compile(expression).unwrap_err().to_string();
        assert!(message.contains(needle), "{expression}: {message}");
    }
    let message = engine
        .compile("path == \"a\" && %")
        .unwrap_err()
        .to_string();
    assert!(message.contains("byte 15"), "{message}");
}

#[test]
fn programs_have_a_debug_form_that_does_not_panic() {
    let program = RestrictedEngine::default().compile("true").unwrap();
    assert!(format!("{program:?}").contains("Program"));
}

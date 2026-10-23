//! The content selector corpus (`tests/fixtures/selector_corpus.json`).
//!
//! The same file drove the engine decision in ADR-RBAC-1: every expression was
//! first run through `cel` 0.15 (the verdict is recorded in the `cel_0_15`
//! field), and it is now the specification of the shipped engine.

// Tests may unwrap and expect: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use rustryx_core::model::RepoFormat;
use rustryx_core::ports::auth::Coordinate;
use rustryx_rbac::{RestrictedEngine, SelectorEngine, SelectorInput};
use serde::Deserialize;
use serde_json::Value;

const CORPUS: &str = include_str!("fixtures/selector_corpus.json");

#[derive(Deserialize)]
struct Corpus {
    valid: Vec<ValidCase>,
    invalid: Vec<InvalidCase>,
}

#[derive(Deserialize)]
struct ValidCase {
    id: String,
    expression: String,
    cel_0_15: String,
    evaluations: Vec<Evaluation>,
}

#[derive(Deserialize)]
struct InvalidCase {
    id: String,
    expression: String,
    cel_0_15: String,
}

#[derive(Deserialize)]
struct Evaluation {
    format: String,
    path: String,
    repository: String,
    coordinate: Option<CoordinateFixture>,
    expect: Value,
}

#[derive(Deserialize)]
struct CoordinateFixture {
    group: Option<String>,
    name: Option<String>,
    version: Option<String>,
}

fn corpus() -> Corpus {
    serde_json::from_str(CORPUS).expect("corpus is valid json")
}

#[test]
fn corpus_expressions_compile_and_evaluate_as_expected() {
    let engine = RestrictedEngine::default();
    let mut failures = Vec::new();
    for case in corpus().valid {
        let program = match engine.compile(&case.expression) {
            Ok(program) => program,
            Err(error) => {
                failures.push(format!("{}: does not compile: {error}", case.id));
                continue;
            }
        };
        for evaluation in &case.evaluations {
            let coordinate = evaluation
                .coordinate
                .as_ref()
                .map(|c| Coordinate::new(c.group.clone(), c.name.clone(), c.version.clone()));
            let input = SelectorInput {
                format: evaluation.format.parse::<RepoFormat>().unwrap(),
                path: &evaluation.path,
                repository: &evaluation.repository,
                coordinate: coordinate.as_ref(),
            };
            let got = engine.evaluate(&program, &input);
            let ok = match (&evaluation.expect, &got) {
                (Value::Bool(want), Ok(have)) => want == have,
                (Value::String(word), Err(_)) => word == "error",
                _ => false,
            };
            if !ok {
                failures.push(format!(
                    "{}: path {:?} expected {}, got {got:?}",
                    case.id, evaluation.path, evaluation.expect
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn corpus_invalid_expressions_are_rejected_when_compiled() {
    let engine = RestrictedEngine::default();
    let accepted: Vec<String> = corpus()
        .invalid
        .into_iter()
        .filter(|case| engine.compile(&case.expression).is_ok())
        .map(|case| format!("{}: {:?}", case.id, case.expression))
        .collect();
    assert!(accepted.is_empty(), "accepted: {accepted:?}");
}

#[test]
fn corpus_has_the_shape_the_adr_quotes() {
    let corpus = corpus();
    let valid = corpus.valid.len();
    let cel_ok = corpus.valid.iter().filter(|c| c.cel_0_15 == "ok").count();
    let cel_compile_errors = corpus
        .valid
        .iter()
        .filter(|c| c.cel_0_15.starts_with("compile error"))
        .count();
    // ADR-RBAC-1 states: 24 valid expressions, cel gives the expected answer for 20
    // and cannot even parse 4.
    assert_eq!((valid, cel_ok, cel_compile_errors), (24, 20, 4));
    // The four cel failures are exactly the expressions that use `=~`.
    for case in &corpus.valid {
        let uses_operator = case.expression.contains("=~");
        assert_eq!(
            uses_operator,
            case.cel_0_15.starts_with("compile error"),
            "{}",
            case.id
        );
    }
    // ADR-RBAC-1 also states: of 24 invalid expressions cel rejects 9 when compiling
    // (two of them only because `=~` does not exist) and accepts the other 15.
    let rejected = corpus
        .invalid
        .iter()
        .filter(|c| c.cel_0_15.starts_with("rejected"))
        .count();
    let accepted = corpus
        .invalid
        .iter()
        .filter(|c| c.cel_0_15.starts_with("accepted"))
        .count();
    assert_eq!((corpus.invalid.len(), rejected, accepted), (24, 9, 15));
    assert!(corpus.invalid.iter().all(|c| !c.cel_0_15.is_empty()));
    assert!(corpus.valid.iter().all(|c| !c.evaluations.is_empty()));
}

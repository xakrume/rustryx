//! Property tests of the selector engine: it never panics, whatever it is fed
//! (SEC-18 "fuzzing of the parser").

// Tests may unwrap and expect: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use proptest::prelude::*;
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::auth::Coordinate;
use rustryx_rbac::{RestrictedEngine, SelectorEngine, SelectorInput, selector_path};

fn arbitrary_format() -> impl Strategy<Value = RepoFormat> {
    prop::sample::select(RepoFormat::ALL)
}

/// Text that looks like the language: tokens in random order, so that the
/// parser sees many almost valid inputs and not only noise.
fn tokenish() -> impl Strategy<Value = String> {
    let token = prop::sample::select(vec![
        "format",
        "path",
        "repository",
        "coordinate",
        ".group",
        ".groupId",
        ".name",
        ".artifactId",
        ".version",
        ".extension",
        ".classifier",
        ".startsWith(",
        ".endsWith(",
        ".contains(",
        ".matches(",
        ")",
        "(",
        "[",
        "]",
        ",",
        "==",
        "!=",
        "=~",
        "&&",
        "||",
        "!",
        " in ",
        "true",
        "false",
        "\"/org/\"",
        "'a'",
        "\"\"",
        "r\"^/a.*\"",
        "\"(a+)+$\"",
        "\"\\\\.\"",
        " ",
        "\"",
        "\\",
        "1",
        "\u{43f}",
    ]);
    prop::collection::vec(token, 0..40).prop_map(|parts| parts.concat())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn compile_never_panics_on_arbitrary_text(source in ".{0,1500}") {
        let _ = RestrictedEngine::default().compile(&source);
    }

    #[test]
    fn compile_never_panics_on_token_soup(source in tokenish()) {
        let _ = RestrictedEngine::default().compile(&source);
    }

    #[test]
    fn evaluation_never_panics_and_is_deterministic(
        source in tokenish(),
        format in arbitrary_format(),
        path in ".{0,200}",
        repository in ".{0,40}",
        group in prop::option::of(".{0,40}"),
        name in prop::option::of(".{0,40}"),
        version in prop::option::of(".{0,40}"),
        extension in prop::option::of(".{0,40}"),
        classifier in prop::option::of(".{0,40}"),
        with_coordinate in any::<bool>(),
    ) {
        let engine = RestrictedEngine::default();
        if let Ok(program) = engine.compile(&source) {
            let mut coordinate = Coordinate::new(group, name, version);
            coordinate.extension = extension;
            coordinate.classifier = classifier;
            let input = SelectorInput {
                format,
                path: &path,
                repository: &repository,
                coordinate: with_coordinate.then_some(&coordinate),
            };
            let first = engine.evaluate(&program, &input);
            let second = engine.evaluate(&program, &input);
            prop_assert_eq!(first, second);
        }
    }

    #[test]
    fn arbitrary_regex_patterns_compile_or_are_rejected_without_panic(
        pattern in ".{0,300}",
        text in ".{0,300}",
    ) {
        let escaped = pattern.replace('\\', "\\\\").replace('"', "\\\"")
            .replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t");
        let escaped: String = escaped.chars().filter(|c| !c.is_control()).collect();
        let engine = RestrictedEngine::default();
        for expression in [
            format!("path.matches(\"{escaped}\")"),
            format!("path =~ \"{escaped}\""),
        ] {
            if let Ok(program) = engine.compile(&expression) {
                let input = SelectorInput {
                    format: RepoFormat::Raw,
                    path: &text,
                    repository: "r",
                    coordinate: None,
                };
                let _ = engine.evaluate(&program, &input);
            }
        }
    }

    #[test]
    fn a_full_match_is_never_wider_than_a_partial_match(
        pattern in "[a-c.*+?|()\\[\\]^$]{0,12}",
        text in "[a-c]{0,12}",
    ) {
        // `=~` must match the whole string, `matches` any part: whatever `=~`
        // accepts, `matches` accepts too.
        let engine = RestrictedEngine::default();
        let full = engine.compile(&format!("path =~ \"{pattern}\""));
        let partial = engine.compile(&format!("path.matches(\"{pattern}\")"));
        if let (Ok(full), Ok(partial)) = (full, partial) {
            let input = SelectorInput {
                format: RepoFormat::Raw,
                path: &text,
                repository: "r",
                coordinate: None,
            };
            if engine.evaluate(&full, &input) == Ok(true) {
                prop_assert_eq!(engine.evaluate(&partial, &input), Ok(true));
            }
        }
    }

    #[test]
    fn canonical_selector_paths_have_no_dot_segments_and_are_idempotent(path in ".{0,60}") {
        if let Some(canonical) = selector_path(&path) {
            prop_assert!(canonical.starts_with('/'));
            prop_assert!(!canonical.split('/').any(|s| s == "." || s == ".."));
            prop_assert!(!canonical.contains("//"));
            prop_assert!(!canonical.chars().any(char::is_control));
            let again = selector_path(&canonical).unwrap();
            prop_assert_eq!(again.as_ref(), canonical.as_ref());
        }
    }

    #[test]
    fn a_traversal_segment_is_never_canonical(
        before in "[a-z/]{0,10}",
        after in "[a-z/]{0,10}",
        dots in prop::sample::select(vec![".", ".."]),
    ) {
        let path = format!("{before}/{dots}/{after}");
        prop_assert!(selector_path(&path).is_none());
    }
}

/// A string valued node of the reference model.
#[derive(Debug, Clone)]
enum S {
    Lit(&'static str),
    Format,
    Path,
    Repository,
    Group,
    Name,
    Version,
    Extension,
    Classifier,
}

/// A boolean node of the reference model: an independent, deliberately naive
/// evaluator that the real engine is compared with.
#[derive(Debug, Clone)]
enum G {
    Lit(bool),
    Not(Box<G>),
    And(Vec<G>),
    Or(Vec<G>),
    Eq(S, S),
    Ne(S, S),
    Starts(S, S),
    Ends(S, S),
    Contains(S, S),
    In(S, Vec<&'static str>),
    Full(S, &'static str),
    Partial(S, &'static str),
}

const LITERALS: [&str; 7] = ["", "a", "ab", "/a/", "/a/b", "raw", "x"];
const PATTERNS: [&str; 7] = ["a", "a|ab", "a|b", "/a/.*", "[ab]+", "", "/a/b?"];

fn render_s(s: &S) -> String {
    match s {
        S::Lit(text) => format!("\"{text}\""),
        S::Format => "format".into(),
        S::Path => "path".into(),
        S::Repository => "repository".into(),
        S::Group => "coordinate.groupId".into(),
        S::Name => "coordinate.name".into(),
        S::Version => "coordinate.version".into(),
        S::Extension => "coordinate.extension".into(),
        S::Classifier => "coordinate.classifier".into(),
    }
}

fn render(g: &G) -> String {
    let join = |items: &[G], op: &str| {
        let parts: Vec<String> = items.iter().map(|i| format!("({})", render(i))).collect();
        parts.join(op)
    };
    match g {
        G::Lit(b) => b.to_string(),
        G::Not(inner) => format!("!({})", render(inner)),
        G::And(items) => join(items, " && "),
        G::Or(items) => join(items, " || "),
        G::Eq(a, b) => format!("{} == {}", render_s(a), render_s(b)),
        G::Ne(a, b) => format!("{} != {}", render_s(a), render_s(b)),
        G::Starts(a, b) => format!("{}.startsWith({})", render_s(a), render_s(b)),
        G::Ends(a, b) => format!("{}.endsWith({})", render_s(a), render_s(b)),
        G::Contains(a, b) => format!("{}.contains({})", render_s(a), render_s(b)),
        G::In(a, items) => {
            let list: Vec<String> = items.iter().map(|i| format!("\"{i}\"")).collect();
            format!("{} in [{}]", render_s(a), list.join(", "))
        }
        G::Full(a, p) => format!("{} =~ \"{p}\"", render_s(a)),
        G::Partial(a, p) => format!("{}.matches(\"{p}\")", render_s(a)),
    }
}

struct World<'a> {
    format: RepoFormat,
    path: &'a str,
    repository: &'a str,
    coordinate: Option<&'a Coordinate>,
}

fn model_s(s: &S, w: &World<'_>) -> Option<String> {
    match s {
        S::Lit(text) => Some((*text).to_owned()),
        S::Format => Some(w.format.as_str().to_owned()),
        S::Path => Some(w.path.to_owned()),
        S::Repository => Some(w.repository.to_owned()),
        S::Group => w.coordinate.and_then(|c| c.group.clone()),
        S::Name => w.coordinate.and_then(|c| c.name.clone()),
        S::Version => w.coordinate.and_then(|c| c.version.clone()),
        S::Extension => w.coordinate.and_then(|c| c.extension.clone()),
        S::Classifier => w.coordinate.and_then(|c| c.classifier.clone()),
    }
}

fn model_pair(a: &S, b: &S, w: &World<'_>, test: impl Fn(&str, &str) -> bool) -> Option<bool> {
    Some(test(&model_s(a, w)?, &model_s(b, w)?))
}

/// CEL style three valued logic: `None` is unknown.
fn model(g: &G, w: &World<'_>) -> Option<bool> {
    match g {
        G::Lit(b) => Some(*b),
        G::Not(inner) => model(inner, w).map(|b| !b),
        G::And(items) => {
            let values: Vec<_> = items.iter().map(|i| model(i, w)).collect();
            if values.contains(&Some(false)) {
                Some(false)
            } else if values.contains(&None) {
                None
            } else {
                Some(true)
            }
        }
        G::Or(items) => {
            let values: Vec<_> = items.iter().map(|i| model(i, w)).collect();
            if values.contains(&Some(true)) {
                Some(true)
            } else if values.contains(&None) {
                None
            } else {
                Some(false)
            }
        }
        G::Eq(a, b) => model_pair(a, b, w, |x, y| x == y),
        G::Ne(a, b) => model_pair(a, b, w, |x, y| x != y),
        G::Starts(a, b) => model_pair(a, b, w, |x, y| x.starts_with(y)),
        G::Ends(a, b) => model_pair(a, b, w, |x, y| x.ends_with(y)),
        G::Contains(a, b) => model_pair(a, b, w, |x, y| x.contains(y)),
        G::In(a, items) => model_s(a, w).map(|x| items.iter().any(|i| *i == x)),
        G::Full(a, p) => {
            let re = regex::Regex::new(&format!(r"\A(?:{p})\z")).unwrap();
            model_s(a, w).map(|x| re.is_match(&x))
        }
        G::Partial(a, p) => {
            let re = regex::Regex::new(p).unwrap();
            model_s(a, w).map(|x| re.is_match(&x))
        }
    }
}

fn s_strategy() -> impl Strategy<Value = S> {
    prop_oneof![
        prop::sample::select(LITERALS.to_vec()).prop_map(S::Lit),
        Just(S::Format),
        Just(S::Path),
        Just(S::Repository),
        Just(S::Group),
        Just(S::Name),
        Just(S::Version),
        Just(S::Extension),
        Just(S::Classifier),
    ]
}

fn g_strategy() -> impl Strategy<Value = G> {
    let leaf = prop_oneof![
        any::<bool>().prop_map(G::Lit),
        (s_strategy(), s_strategy()).prop_map(|(a, b)| G::Eq(a, b)),
        (s_strategy(), s_strategy()).prop_map(|(a, b)| G::Ne(a, b)),
        (s_strategy(), s_strategy()).prop_map(|(a, b)| G::Starts(a, b)),
        (s_strategy(), s_strategy()).prop_map(|(a, b)| G::Ends(a, b)),
        (s_strategy(), s_strategy()).prop_map(|(a, b)| G::Contains(a, b)),
        (
            s_strategy(),
            prop::collection::vec(prop::sample::select(LITERALS.to_vec()), 1..4)
        )
            .prop_map(|(a, items)| G::In(a, items)),
        (s_strategy(), prop::sample::select(PATTERNS.to_vec())).prop_map(|(a, p)| G::Full(a, p)),
        (s_strategy(), prop::sample::select(PATTERNS.to_vec())).prop_map(|(a, p)| G::Partial(a, p)),
    ];
    leaf.prop_recursive(4, 32, 3, |inner| {
        prop_oneof![
            inner.clone().prop_map(|g| G::Not(Box::new(g))),
            prop::collection::vec(inner.clone(), 2..4).prop_map(G::And),
            prop::collection::vec(inner, 2..4).prop_map(G::Or),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// Differential test: the engine agrees with the naive model on every
    /// generated expression, including the unknown (missing coordinate) cases.
    #[test]
    fn engine_agrees_with_the_reference_model(
        tree in g_strategy(),
        format in arbitrary_format(),
        path in prop::sample::select(LITERALS.to_vec()),
        repository in prop::sample::select(LITERALS.to_vec()),
        group in prop::option::of(prop::sample::select(LITERALS.to_vec())),
        name in prop::option::of(prop::sample::select(LITERALS.to_vec())),
        version in prop::option::of(prop::sample::select(LITERALS.to_vec())),
        extension in prop::option::of(prop::sample::select(LITERALS.to_vec())),
        classifier in prop::option::of(prop::sample::select(LITERALS.to_vec())),
        with_coordinate in any::<bool>(),
    ) {
        let source = render(&tree);
        prop_assume!(source.len() <= 1024);
        let engine = RestrictedEngine::default();
        let program = engine.compile(&source);
        prop_assert!(program.is_ok(), "{source}: {:?}", program.err());
        let program = program.unwrap();
        let mut coordinate = Coordinate::new(
            group.map(str::to_owned),
            name.map(str::to_owned),
            version.map(str::to_owned),
        );
        coordinate.extension = extension.map(str::to_owned);
        coordinate.classifier = classifier.map(str::to_owned);
        let world = World {
            format,
            path,
            repository,
            coordinate: with_coordinate.then_some(&coordinate),
        };
        let input = SelectorInput {
            format,
            path,
            repository,
            coordinate: world.coordinate,
        };
        let expected = model(&tree, &world);
        let got = engine.evaluate(&program, &input);
        match expected {
            Some(value) => prop_assert_eq!(got, Ok(value), "{}", source),
            None => prop_assert!(got.is_err(), "{source}: expected unknown, got {got:?}"),
        }
    }
}

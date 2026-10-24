use proptest::prelude::*;

use super::*;

#[test]
fn parses_a_repository_scope() {
    let scope = Scope::parse("repository:docker-hosted/team/app:pull,push").unwrap();
    assert_eq!(scope.actions, [Action::Pull, Action::Push]);
    assert_eq!(scope.name().as_deref(), Some("docker-hosted/team/app"));
    match &scope.resource {
        Resource::Repository { repository, image } => {
            assert_eq!(repository.as_str(), "docker-hosted");
            assert_eq!(image.as_str(), "team/app");
        }
        Resource::Catalog => panic!("expected a repository scope"),
    }
    assert_eq!(
        scope.to_string(),
        "repository:docker-hosted/team/app:pull,push"
    );
}

#[test]
fn actions_are_deduplicated_sorted_and_unknown_ones_dropped() {
    let scope = Scope::parse("repository:r/a:push,pull,push,mount,*").unwrap();
    assert_eq!(scope.actions, [Action::Pull, Action::Push, Action::Delete]);
    let none = Scope::parse("repository:r/a:").unwrap();
    assert!(none.actions.is_empty());
    let only_unknown = Scope::parse("repository:r/a:mount,admin").unwrap();
    assert!(only_unknown.actions.is_empty());
    let spaced = Scope::parse("repository:r/a: pull , delete ").unwrap();
    assert_eq!(spaced.actions, [Action::Pull, Action::Delete]);
}

#[test]
fn the_catalog_scope_is_exact() {
    let scope = Scope::parse("registry:catalog:*").unwrap();
    assert_eq!(scope.resource, Resource::Catalog);
    assert_eq!(scope.name(), None);
    assert_eq!(scope.to_string(), "registry:catalog:*");
    assert_eq!(
        Scope::parse("registry:catalog:pull"),
        Err(ScopeError::Malformed)
    );
    assert_eq!(Scope::parse("registry:other:*"), Err(ScopeError::Malformed));
}

#[test]
fn rejects_malformed_scopes() {
    let long = format!("repository:r/{}:pull", "a".repeat(600));
    for (bad, error) in [
        ("", ScopeError::Malformed),
        ("repository", ScopeError::Malformed),
        ("repository:r/a", ScopeError::Malformed),
        ("repository:onlyrepo:pull", ScopeError::InvalidName),
        ("repository:r/A:pull", ScopeError::InvalidName),
        ("repository:R/a:pull", ScopeError::InvalidName),
        ("repository:r/a/../b:pull", ScopeError::InvalidName),
        ("repository:r//a:pull", ScopeError::InvalidName),
        ("repository:r/a:b:pull", ScopeError::InvalidName),
        ("repository(plugin):r/a:pull", ScopeError::UnsupportedType),
        ("other:r/a:pull", ScopeError::UnsupportedType),
        (long.as_str(), ScopeError::Malformed),
    ] {
        assert_eq!(Scope::parse(bad), Err(error), "{bad:?}");
    }
}

#[test]
fn scopes_are_split_on_whitespace_and_capped() {
    let scopes = parse_scopes([
        "repository:r/a:pull repository:r/b:push",
        "registry:catalog:*",
    ])
    .unwrap();
    assert_eq!(scopes.len(), 3);
    assert!(parse_scopes([]).unwrap().is_empty());
    assert!(parse_scopes([""]).unwrap().is_empty());
    let too_many: Vec<String> = (0..=MAX_SCOPES)
        .map(|i| format!("repository:r/a{i}:pull"))
        .collect();
    assert_eq!(
        parse_scopes(too_many.iter().map(String::as_str)),
        Err(ScopeError::TooMany)
    );
    assert!(parse_scopes(["repository:r/a:pull bad"]).is_err());
}

proptest! {
    #[test]
    fn parse_never_panics(text in ".{0,300}") {
        let _ = Scope::parse(&text);
        let _ = parse_scopes([text.as_str()]);
    }

    #[test]
    fn a_formatted_scope_parses_back(
        repo in "[a-z0-9]{1,8}",
        image in "[a-z0-9]{1,8}(/[a-z0-9]{1,8}){0,2}",
        pull in any::<bool>(), push in any::<bool>(), delete in any::<bool>(),
    ) {
        let mut actions = Vec::new();
        if pull { actions.push("pull"); }
        if push { actions.push("push"); }
        if delete { actions.push("delete"); }
        let text = format!("repository:{repo}/{image}:{}", actions.join(","));
        let scope = Scope::parse(&text).unwrap();
        prop_assert_eq!(scope.to_string(), text);
    }
}

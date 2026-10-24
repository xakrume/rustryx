//! Tests of `classify`: the route, the action, the kind and the coordinate that authorization and
//! routing rules see before a body is read.

use rustryx_core::model::RepoType;
use rustryx_testkit::fixtures;
use url::Url;

use super::*;
use crate::testing::sha256_hex;

const ID: &str = "0199c9a4-7a58-7d3e-8c1a-5f6d2b9e4a10";

fn classify(method: Method, path: &str) -> Result<ArtifactRequest, FormatError> {
    classify_with(method, path, None)
}

fn classify_with(
    method: Method,
    path: &str,
    query: Option<&str>,
) -> Result<ArtifactRequest, FormatError> {
    let repository = fixtures::repository("pub-hosted", RepoFormat::Pub, RepoType::Hosted);
    let mut head = RequestHead::new(
        method,
        path,
        rustryx_core::identity::Principal::anonymous(),
        Url::parse("https://rustryx.test").unwrap(),
    );
    if let Some(query) = query {
        head = head.with_query(query);
    }
    PubHandler::default().classify(&repository, &head)
}

fn coordinate(request: &ArtifactRequest) -> (Option<&str>, Option<&str>) {
    let coordinate = request.coordinate.as_ref().unwrap();
    (coordinate.name.as_deref(), coordinate.version.as_deref())
}

#[test]
fn reads_are_reads_with_the_coordinate_of_their_path() {
    let list = classify(Method::GET, "api/packages/demo").unwrap();
    assert_eq!(list.action, Action::Read);
    assert_eq!(list.kind, rustryx_format_core::ContentKind::Index);
    assert_eq!(coordinate(&list), (Some("demo"), None));

    let version = classify(Method::GET, "api/packages/demo/versions/1.2.3").unwrap();
    assert_eq!(version.kind, rustryx_format_core::ContentKind::Index);
    assert_eq!(coordinate(&version), (Some("demo"), Some("1.2.3")));

    let archive = classify(Method::HEAD, "packages/demo/versions/1.2.3.tar.gz").unwrap();
    assert_eq!(archive.action, Action::Read);
    assert_eq!(archive.kind, rustryx_format_core::ContentKind::Artifact);
    assert_eq!(coordinate(&archive), (Some("demo"), Some("1.2.3")));

    let advisories = classify(Method::GET, "api/packages/demo/advisories").unwrap();
    assert_eq!(advisories.action, Action::Read);
    assert_eq!(coordinate(&advisories), (Some("demo"), None));
}

#[test]
fn the_publish_steps_need_the_write_privilege() {
    let new = classify(Method::GET, "api/packages/versions/new").unwrap();
    assert_eq!(new.action, Action::Write);
    assert!(new.coordinate.is_none());
    let upload = classify(Method::POST, &format!("api/packages/versions/upload/{ID}")).unwrap();
    assert_eq!(upload.action, Action::Write);
    assert!(upload.coordinate.is_none());
    let finish = classify(Method::GET, &format!("api/packages/versions/finish/{ID}")).unwrap();
    assert_eq!(finish.action, Action::Write);
}

#[test]
fn a_declared_coordinate_is_visible_before_the_body_is_read() {
    let new = classify_with(
        Method::GET,
        "api/packages/versions/new",
        Some("package=demo&version=1.0.0"),
    )
    .unwrap();
    assert_eq!(coordinate(&new), (Some("demo"), Some("1.0.0")));
    let upload = classify(
        Method::POST,
        &format!("api/packages/versions/upload/{ID}/demo/1.0.0"),
    )
    .unwrap();
    assert_eq!(coordinate(&upload), (Some("demo"), Some("1.0.0")));
    let finish = classify(
        Method::GET,
        &format!("api/packages/versions/finish/{ID}/demo/1.0.0"),
    )
    .unwrap();
    assert_eq!(coordinate(&finish), (Some("demo"), Some("1.0.0")));
}

#[test]
fn must_not_accept_a_half_or_invalid_declaration() {
    for query in [
        "package=demo",
        "version=1.0.0",
        "package=Demo&version=1.0.0",
        "package=demo&version=1.0",
    ] {
        assert!(
            matches!(
                classify_with(Method::GET, "api/packages/versions/new", Some(query)),
                Err(FormatError::BadRequest(_))
            ),
            "{query}"
        );
    }
    // Other query parameters are ignored.
    assert!(classify_with(Method::GET, "api/packages/versions/new", Some("x=1")).is_ok());
}

#[test]
fn must_not_allow_methods_a_route_does_not_have() {
    for path in [
        "api/packages/demo",
        "packages/demo/versions/1.0.0.tar.gz",
        "api/packages/demo/versions/1.0.0",
    ] {
        for method in [Method::PUT, Method::POST, Method::DELETE, Method::PATCH] {
            assert!(
                matches!(
                    classify(method.clone(), path),
                    Err(FormatError::MethodNotAllowed { allow: "GET, HEAD" })
                ),
                "{method} {path}"
            );
        }
    }
    for method in [Method::HEAD, Method::POST, Method::PUT, Method::DELETE] {
        assert!(matches!(
            classify(method, "api/packages/versions/new"),
            Err(FormatError::MethodNotAllowed { allow: "GET" })
        ));
    }
    for method in [Method::GET, Method::HEAD, Method::PUT, Method::DELETE] {
        assert!(matches!(
            classify(method, &format!("api/packages/versions/upload/{ID}")),
            Err(FormatError::MethodNotAllowed { allow: "POST" })
        ));
    }
    assert!(matches!(
        classify(Method::HEAD, &format!("api/packages/versions/finish/{ID}")),
        Err(FormatError::MethodNotAllowed { allow: "GET" })
    ));
}

#[test]
fn unknown_paths_are_not_found_and_bad_names_are_bad_requests() {
    for path in [
        "",
        "api",
        "api/packages",
        "api/other/demo",
        "packages/demo",
        "x/y/z",
    ] {
        assert!(
            matches!(classify(Method::GET, path), Err(FormatError::NotFound)),
            "{path:?}"
        );
    }
    for path in [
        "api/packages/Demo",
        "api/packages/de-mo",
        "api/packages/demo/versions/1.0",
        "packages/demo/versions/latest.tar.gz",
        "api/packages/versions/upload/not-a-uuid",
    ] {
        assert!(
            matches!(classify(Method::GET, path), Err(FormatError::BadRequest(_))),
            "{path:?}"
        );
    }
}

#[test]
fn must_not_let_a_path_trick_reach_a_different_route_than_the_one_authorized() {
    // Traversal, encoded slashes, double encoding, backslashes and empty segments in any spelling
    // are refused at normalisation, before a route exists.
    for path in [
        "api/packages/../packages/demo",
        "api/packages/%2e%2e/demo",
        "api/packages/%2E%2E%2Fdemo",
        "api/packages/demo%2Fversions%2F1.0.0",
        "api/packages/demo%252Fversions",
        "api/packages//demo",
        "api\\packages\\demo",
        "api/packages/demo/.",
        "api/packages/demo\0",
        "api/packages/demo/",
    ] {
        assert!(
            matches!(classify(Method::GET, path), Err(FormatError::BadRequest(_))),
            "{path:?}"
        );
    }
}

#[test]
fn the_authorized_path_is_the_decoded_path_the_route_was_read_from() {
    // `%61` is `a`: the engine authorizes `api/packages/demo` and that is what is read.
    let request = classify(Method::GET, "%61pi/packages/demo").unwrap();
    assert_eq!(request.path.as_str(), "api/packages/demo");
    assert_eq!(
        Route::parse(&request.path)
            .unwrap()
            .package()
            .unwrap()
            .as_str(),
        "demo"
    );
    // One leading slash is tolerated by format-core and dropped.
    assert_eq!(
        classify(Method::GET, "/api/packages/demo")
            .unwrap()
            .path
            .as_str(),
        "api/packages/demo"
    );
}

#[test]
fn the_handler_declares_itself_correctly() {
    let handler = PubHandler::default();
    assert_eq!(handler.formats(), &[RepoFormat::Pub]);
    assert!(handler.group_merge().is_some());
    assert!(handler.group_claims().is_some());
    // pub clients drop their token on 401: a hidden repository is 403, never 404.
    assert!(!handler.hide_inaccessible_repository());
    assert_eq!(handler.config().max_upload_bytes, 100 * 1024 * 1024);
    let repository = fixtures::repository("pub-hosted", RepoFormat::Pub, RepoType::Hosted);
    let rendered = handler.render_error(
        &FormatError::Forbidden,
        &ErrorInfo {
            request_id: Some("r1"),
            repository: &repository,
            anonymous: false,
        },
    );
    assert_eq!(rendered.status(), StatusCode::FORBIDDEN);
    assert!(
        rendered
            .headers()
            .contains_key(http::header::WWW_AUTHENTICATE)
    );
    assert_eq!(sha256_hex(b"").len(), 64);
}

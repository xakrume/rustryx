use proptest::prelude::*;

use super::*;

const DIGEST: &str = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn parse(path: &str) -> OciRoute {
    OciRoute::parse(path).unwrap()
}

#[test]
fn parses_every_endpoint() {
    assert_eq!(parse(""), OciRoute::Ping);
    assert_eq!(parse("_catalog"), OciRoute::Catalog);
    assert!(matches!(parse("team/app/tags/list"), OciRoute::Tags { .. }));
    assert!(matches!(
        parse("team/app/manifests/latest"),
        OciRoute::Manifest {
            reference: Reference::Tag(_),
            ..
        }
    ));
    assert!(matches!(
        parse(&format!("app/manifests/{DIGEST}")),
        OciRoute::Manifest {
            reference: Reference::Digest(_),
            ..
        }
    ));
    assert!(matches!(
        parse(&format!("app/blobs/{DIGEST}")),
        OciRoute::Blob { .. }
    ));
    assert!(matches!(
        parse("app/blobs/uploads"),
        OciRoute::UploadStart { .. }
    ));
    assert!(matches!(
        parse("a/b/blobs/uploads/abc.def"),
        OciRoute::Upload { .. }
    ));
    assert!(matches!(
        parse(&format!("app/referrers/{DIGEST}")),
        OciRoute::Referrers { .. }
    ));
}

#[test]
fn an_image_path_may_contain_the_words_of_the_protocol() {
    let route = parse("blobs/uploads/manifests/tags/manifests/latest");
    assert_eq!(
        route.image().unwrap().as_str(),
        "blobs/uploads/manifests/tags"
    );
    assert!(matches!(route, OciRoute::Manifest { .. }));

    let route = parse("a/manifests/blobs/uploads/session-1");
    assert_eq!(route.image().unwrap().as_str(), "a/manifests");
    assert!(matches!(route, OciRoute::Upload { .. }));

    let route = parse(&format!("a/blobs/uploads/blobs/{DIGEST}"));
    assert_eq!(route.image().unwrap().as_str(), "a/blobs/uploads");
    assert!(matches!(route, OciRoute::Blob { .. }));

    let route = parse("x/tags/tags/list");
    assert_eq!(route.image().unwrap().as_str(), "x/tags");
    assert!(matches!(route, OciRoute::Tags { .. }));

    let route = parse("blobs/blobs/uploads");
    assert_eq!(route.image().unwrap().as_str(), "blobs");
    assert!(matches!(route, OciRoute::UploadStart { .. }));
}

#[test]
fn bad_parts_are_reported_as_such() {
    assert!(matches!(
        OciRoute::parse("App/manifests/latest"),
        Err(RouteError::Name(_))
    ));
    assert!(matches!(
        OciRoute::parse("app/manifests/-bad"),
        Err(RouteError::Reference(_))
    ));
    assert!(matches!(
        OciRoute::parse("app/blobs/sha256:short"),
        Err(RouteError::Digest(_))
    ));
    assert!(matches!(
        OciRoute::parse("app/referrers/latest"),
        Err(RouteError::Digest(_))
    ));
    assert!(matches!(
        OciRoute::parse("app/blobs/uploads/a b"),
        Err(RouteError::Session(_))
    ));
    for not_a_route in [
        "app",
        "app/manifests",
        "app/tags/other",
        "tags/list",
        "a/b",
        "app/other/x",
        "manifests/latest",
    ] {
        assert!(
            matches!(OciRoute::parse(not_a_route), Err(RouteError::NotARoute)),
            "{not_a_route}"
        );
    }
}

#[test]
fn accessors_describe_the_route() {
    let route = parse(&format!("team/app/blobs/{DIGEST}"));
    assert_eq!(route.version().as_deref(), Some(DIGEST));
    assert_eq!(
        parse("team/app/manifests/1.0").version().as_deref(),
        Some("1.0")
    );
    assert_eq!(parse("team/app/tags/list").version(), None);
    assert_eq!(OciRoute::Ping.image(), None);
    assert_eq!(OciRoute::Catalog.image(), None);
}

#[test]
fn canonical_paths_parse_back_to_the_same_route() {
    for path in [
        "",
        "_catalog",
        "a/tags/list",
        "a/b/manifests/v1",
        "a/blobs/uploads",
        "a/blobs/uploads/id-1",
    ] {
        let route = parse(path);
        assert_eq!(route.canonical_path(), path);
        assert_eq!(parse(&route.canonical_path()), route);
    }
}

proptest! {
    #[test]
    fn parse_never_panics(path in ".{0,300}") {
        if let Ok(route) = OciRoute::parse(&path) {
            let again = OciRoute::parse(&route.canonical_path());
            prop_assert_eq!(again.ok(), Some(route));
        }
    }

    #[test]
    fn segments_soup_never_panics(segments in proptest::collection::vec("(blobs|manifests|tags|list|uploads|referrers|_catalog|[a-z0-9]{1,5}|sha256:[a-f0-9]{64})", 0..8)) {
        let path = segments.join("/");
        if let Ok(route) = OciRoute::parse(&path) {
            prop_assert_eq!(OciRoute::parse(&route.canonical_path()).ok(), Some(route));
        }
    }
}

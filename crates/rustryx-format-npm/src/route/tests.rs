use rustryx_format_core::PathRules;

use super::*;

fn path(raw: &str) -> NormalizedPath {
    NormalizedPath::parse_with(
        raw,
        &PathRules {
            allow_encoded_slash: true,
            ..PathRules::default()
        },
    )
    .unwrap()
}

fn parse(method: &Method, raw: &str) -> Result<Route, FormatError> {
    Route::parse(method, &path(raw))
}

fn package(text: &str) -> PackageName {
    PackageName::parse(text).unwrap()
}

#[test]
fn the_session_and_search_routes() {
    assert_eq!(parse(&Method::GET, "-/ping").unwrap(), Route::Ping);
    assert_eq!(parse(&Method::HEAD, "-/ping").unwrap(), Route::Ping);
    assert_eq!(parse(&Method::GET, "-/whoami").unwrap(), Route::Whoami);
    assert_eq!(parse(&Method::GET, "-/v1/search").unwrap(), Route::Search);
    for audit in [
        "-/npm/v1/security/advisories/bulk",
        "-/npm/v1/security/audits",
        "-/npm/v1/security/audits/quick",
    ] {
        assert_eq!(parse(&Method::POST, audit).unwrap(), Route::Audit);
        assert!(matches!(
            parse(&Method::GET, audit),
            Err(FormatError::MethodNotAllowed { allow: "POST" })
        ));
    }
    assert_eq!(
        parse(&Method::PUT, "-/user/org.couchdb.user:alice").unwrap(),
        Route::Login {
            user: "alice".to_owned()
        }
    );
    assert_eq!(
        parse(&Method::PUT, "-/user/org.couchdb.user%3Aa.b%40c.d").unwrap(),
        Route::Login {
            user: "a.b@c.d".to_owned()
        }
    );
    assert_eq!(
        parse(&Method::DELETE, "-/user/token/ryx_abc-1.2_x").unwrap(),
        Route::Logout {
            token: "ryx_abc-1.2_x".to_owned()
        }
    );
    assert!(matches!(
        parse(&Method::GET, "-/user/org.couchdb.user:alice"),
        Err(FormatError::MethodNotAllowed { .. })
    ));
}

#[test]
fn unknown_api_paths_are_404_not_a_package() {
    for raw in [
        "-",
        "-/",
        "-/unknown",
        "-/ping/extra",
        "-/user/nobody",
        "-/user/org.couchdb.user:",
        "-/user/org.couchdb.user:a b",
        "-/user/token/a b",
        "-/v1/search/x",
        "-/package/p",
        "-/package/p/dist-tag",
    ] {
        let raw = raw.trim_end_matches('/');
        let result = NormalizedPath::parse_with(raw, &PathRules::default())
            .map_err(FormatError::from)
            .and_then(|p| Route::parse(&Method::GET, &p));
        assert!(
            matches!(result, Err(FormatError::NotFound)),
            "{raw}: {result:?}"
        );
    }
    assert!(matches!(
        Route::parse(&Method::GET, &NormalizedPath::root()),
        Err(FormatError::NotFound)
    ));
}

#[test]
fn packument_routes_accept_both_spellings_of_a_scoped_name() {
    for raw in [
        "left-pad",
        "@scope/pkg",
        "@scope%2fpkg",
        "@scope%2Fpkg",
        "%40scope%2fpkg",
    ] {
        let expected = package(if raw.contains("scope") {
            "@scope/pkg"
        } else {
            "left-pad"
        });
        assert_eq!(
            parse(&Method::GET, raw).unwrap(),
            Route::Packument(expected.clone()),
            "{raw}"
        );
        assert_eq!(
            parse(&Method::PUT, raw).unwrap(),
            Route::Packument(expected)
        );
    }
    assert!(matches!(
        parse(&Method::DELETE, "left-pad"),
        Err(FormatError::MethodNotAllowed {
            allow: "GET, HEAD, PUT"
        })
    ));
    assert!(matches!(
        parse(&Method::POST, "left-pad"),
        Err(FormatError::MethodNotAllowed { .. })
    ));
}

#[test]
fn must_not_route_names_that_collide_with_the_registry() {
    for raw in [
        ".hidden",
        "_private",
        "@scope",
        "@scope/.x",
        "@/x",
        "node_modules",
        "a b",
    ] {
        let normalized = NormalizedPath::parse_with(
            raw,
            &PathRules {
                allow_encoded_slash: true,
                ..PathRules::default()
            },
        );
        let result = normalized
            .map_err(FormatError::from)
            .and_then(|p| Route::parse(&Method::GET, &p));
        assert!(result.is_err(), "{raw}");
    }
}

#[test]
fn version_documents_resolve_versions_and_tags_only() {
    assert_eq!(
        parse(&Method::GET, "pkg/1.2.3").unwrap(),
        Route::VersionDoc {
            package: package("pkg"),
            spec: "1.2.3".to_owned()
        }
    );
    assert_eq!(
        parse(&Method::GET, "@s%2fpkg/latest").unwrap(),
        Route::VersionDoc {
            package: package("@s/pkg"),
            spec: "latest".to_owned()
        }
    );
    for raw in ["pkg/not a tag", "pkg/-", "pkg/-rev", "pkg/a/b/c"] {
        let result = NormalizedPath::parse_with(raw.trim_end_matches('/'), &PathRules::default())
            .map_err(FormatError::from)
            .and_then(|p| Route::parse(&Method::GET, &p));
        assert!(result.is_err(), "{raw}");
    }
    assert!(matches!(
        parse(&Method::PUT, "pkg/1.2.3"),
        Err(FormatError::MethodNotAllowed { .. })
    ));
}

#[test]
fn tarball_routes_read_the_version_from_the_file_name() {
    assert_eq!(
        parse(&Method::GET, "pkg/-/pkg-1.0.0.tgz").unwrap(),
        Route::Tarball {
            package: package("pkg"),
            version: Version::new(1, 0, 0)
        }
    );
    assert_eq!(
        parse(&Method::GET, "@s/pkg/-/pkg-2.0.0-rc.1.tgz").unwrap(),
        Route::Tarball {
            package: package("@s/pkg"),
            version: Version::parse("2.0.0-rc.1").unwrap()
        }
    );
    for raw in [
        "pkg/-/other-1.0.0.tgz",
        "pkg/-/pkg-1.0.tgz",
        "pkg/-/pkg-1.0.0.zip",
        "pkg/-/pkg-1.0.0.tgz.sig",
        "pkg/-/doc/00000000000000000001.json",
        "pkg/-/abbreviated.json",
        "pkg/-/pkg-1.0.0.tgz/extra",
    ] {
        assert!(
            matches!(parse(&Method::GET, raw), Err(FormatError::NotFound)),
            "{raw}"
        );
    }
    assert!(matches!(
        parse(&Method::PUT, "pkg/-/pkg-1.0.0.tgz"),
        Err(FormatError::MethodNotAllowed { allow: "GET, HEAD" })
    ));
}

#[test]
fn dist_tag_routes() {
    assert_eq!(
        parse(&Method::GET, "-/package/@s%2fpkg/dist-tags").unwrap(),
        Route::DistTags(package("@s/pkg"))
    );
    assert_eq!(
        parse(&Method::PUT, "-/package/pkg/dist-tags/next").unwrap(),
        Route::DistTag {
            package: package("pkg"),
            tag: "next".to_owned()
        }
    );
    assert!(parse(&Method::DELETE, "-/package/pkg/dist-tags/next").is_ok());
    assert!(matches!(
        parse(&Method::GET, "-/package/pkg/dist-tags/next"),
        Err(FormatError::MethodNotAllowed { .. })
    ));
    assert!(matches!(
        parse(&Method::PUT, "-/package/pkg/dist-tags/1.0.0"),
        Err(FormatError::NotFound)
    ));
    assert!(matches!(
        parse(&Method::PUT, "-/package/pkg/dist-tags"),
        Err(FormatError::MethodNotAllowed { .. })
    ));
}

#[test]
fn unpublish_routes() {
    assert_eq!(
        parse(&Method::DELETE, "pkg/-rev/3-abc").unwrap(),
        Route::PackageRev(package("pkg"))
    );
    assert_eq!(
        parse(&Method::PUT, "@s%2fpkg/-rev/3-abc").unwrap(),
        Route::PackageRev(package("@s/pkg"))
    );
    assert_eq!(
        parse(&Method::DELETE, "@s/pkg/-/pkg-1.0.0.tgz/-rev/3-abc").unwrap(),
        Route::TarballRev {
            package: package("@s/pkg"),
            version: Version::new(1, 0, 0)
        }
    );
    assert!(matches!(
        parse(&Method::GET, "pkg/-rev/3-abc"),
        Err(FormatError::MethodNotAllowed { .. })
    ));
    assert!(matches!(
        parse(&Method::PUT, "pkg/-/pkg-1.0.0.tgz/-rev/3-abc"),
        Err(FormatError::MethodNotAllowed { allow: "DELETE" })
    ));
    for bad_rev in ["pkg/-rev/a b", "pkg/-rev/%2e%2e"] {
        let result = NormalizedPath::parse_with(bad_rev, &PathRules::default())
            .map_err(FormatError::from)
            .and_then(|p| Route::parse(&Method::DELETE, &p));
        assert!(result.is_err(), "{bad_rev}");
    }
    assert!(matches!(
        parse(&Method::DELETE, "pkg/-rev/%7e%7e"),
        Err(FormatError::BadRequest(_))
    ));
}

#[test]
fn actions_follow_what_the_request_can_change() {
    let pkg = package("pkg");
    let version = Version::new(1, 0, 0);
    let read = |route: &Route, method: Method| route.action(&method);
    for route in [
        Route::Ping,
        Route::Whoami,
        Route::Search,
        Route::Audit,
        Route::Login { user: "u".into() },
        Route::Logout { token: "t".into() },
        Route::VersionDoc {
            package: pkg.clone(),
            spec: "latest".into(),
        },
        Route::Tarball {
            package: pkg.clone(),
            version: version.clone(),
        },
        Route::DistTags(pkg.clone()),
    ] {
        assert_eq!(read(&route, Method::GET), Action::Read, "{route:?}");
    }
    let packument = Route::Packument(pkg.clone());
    assert_eq!(packument.action(&Method::GET), Action::Read);
    assert_eq!(packument.action(&Method::HEAD), Action::Read);
    assert_eq!(packument.action(&Method::PUT), Action::Write);
    let tag = Route::DistTag {
        package: pkg.clone(),
        tag: "next".into(),
    };
    assert_eq!(tag.action(&Method::PUT), Action::Write);
    assert_eq!(tag.action(&Method::DELETE), Action::Delete);
    // A PUT to the -rev form removes versions, so it needs the right to delete.
    let rev = Route::PackageRev(pkg.clone());
    assert_eq!(rev.action(&Method::PUT), Action::Delete);
    assert_eq!(rev.action(&Method::DELETE), Action::Delete);
    assert_eq!(
        Route::TarballRev {
            package: pkg,
            version
        }
        .action(&Method::DELETE),
        Action::Delete
    );
}

#[test]
fn only_reads_of_documents_are_index_requests() {
    let pkg = package("pkg");
    assert!(Route::Packument(pkg.clone()).is_index(&Method::GET));
    assert!(Route::Packument(pkg.clone()).is_index(&Method::HEAD));
    assert!(!Route::Packument(pkg.clone()).is_index(&Method::PUT));
    assert!(Route::DistTags(pkg.clone()).is_index(&Method::GET));
    assert!(Route::Search.is_index(&Method::GET));
    assert!(
        Route::VersionDoc {
            package: pkg.clone(),
            spec: "1.0.0".into()
        }
        .is_index(&Method::GET)
    );
    assert!(
        !Route::Tarball {
            package: pkg,
            version: Version::new(1, 0, 0)
        }
        .is_index(&Method::GET)
    );
    assert!(!Route::Ping.is_index(&Method::GET));
}

#[test]
fn the_coordinate_is_scope_name_and_version_in_lowercase() {
    let tarball = parse(&Method::GET, "@Acme/Billing/-/Billing-1.2.3.tgz").unwrap();
    let coordinate = tarball.coordinate().unwrap();
    assert_eq!(coordinate.group.as_deref(), Some("acme"));
    assert_eq!(coordinate.name.as_deref(), Some("billing"));
    assert_eq!(coordinate.version.as_deref(), Some("1.2.3"));
    let document = parse(&Method::GET, "plain").unwrap().coordinate().unwrap();
    assert_eq!(document.group, None);
    assert_eq!(document.name.as_deref(), Some("plain"));
    assert_eq!(document.version, None);
    assert!(Route::Ping.coordinate().is_none());
    assert!(Route::Search.package().is_none());
    assert_eq!(tarball.package().unwrap().full(), "@Acme/Billing");
}

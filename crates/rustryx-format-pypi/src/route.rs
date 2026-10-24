//! The URL space of a `PyPI` repository and the classification of a request, before its body is
//! read.
//!
//! ```text
//! POST   /                                      legacy upload (twine)
//! GET    /simple/                               root index (PEP 503, PEP 691)
//! GET    /simple/<project>/                     project page
//! GET    /packages/<project>/<file>             a wheel or an sdist
//! GET    /packages/<project>/<file>.metadata    the core metadata file of a wheel (PEP 658)
//! GET    /packages/<project>/<file>.yanked      yank state of a file
//! PUT    /packages/<project>/<file>.yanked      yank a file (body or `?reason=` is the reason)
//! DELETE /packages/<project>/<file>.yanked      unyank a file
//! DELETE /packages/<project>/<file>             delete a file
//! GET    /pypi/<project>/json                   minimal JSON API
//! GET    /pypi/<project>/<version>/json         the same for one version
//! ```
//!
//! Invariant (SEC-19): [`classify`] rebuilds the request path from the parsed route, with the
//! project name normalized (PEP 503) and the version canonical, and `handle` only ever reads that
//! path. A request for `/simple/Foo_Bar/` is therefore authorized, routed and stored as
//! `simple/foo-bar`: a spelling cannot reach content through a path rule written for another
//! spelling, and one project cannot have two cache keys.

use http::Method;
use rustryx_core::identity::Action;
use rustryx_core::model::Repository;
use rustryx_core::ports::auth::Coordinate;
use rustryx_format_core::{ArtifactRequest, FormatError, NormalizedPath, PathRules, RequestHead};

use crate::filename::DistFilename;
use crate::name::ProjectName;
use crate::version::Version;

/// Where a request is going, with every part already validated.
#[derive(Debug, Clone)]
pub(crate) enum Route {
    /// `POST /`.
    Upload,
    /// `/simple/`.
    Root,
    /// `/simple/<project>/`.
    Project { name: ProjectName },
    /// `/packages/<project>/<file>`.
    File {
        name: ProjectName,
        file: DistFilename,
    },
    /// `/packages/<project>/<file>.metadata`.
    FileMetadata {
        name: ProjectName,
        file: DistFilename,
    },
    /// `/packages/<project>/<file>.yanked`.
    Yank {
        name: ProjectName,
        file: DistFilename,
    },
    /// `/pypi/<project>/json` and `/pypi/<project>/<version>/json`.
    JsonApi {
        name: ProjectName,
        version: Option<Version>,
    },
}

pub(crate) const SIMPLE: &str = "simple";
pub(crate) const PACKAGES: &str = "packages";
const JSON_API: &str = "pypi";
const METADATA_SUFFIX: &str = ".metadata";
pub(crate) const YANKED_SUFFIX: &str = ".yanked";

/// Storage path of a distribution.
pub(crate) fn file_path(name: &ProjectName, filename: &str) -> String {
    format!("{PACKAGES}/{name}/{filename}")
}

/// Storage path of the PEP 658 metadata file of a distribution.
pub(crate) fn metadata_path(name: &ProjectName, filename: &str) -> String {
    format!("{}{METADATA_SUFFIX}", file_path(name, filename))
}

/// Storage path of the internal record of a distribution (what the index needs and the file
/// does not carry in the asset row).
pub(crate) fn info_path(name: &ProjectName, filename: &str) -> String {
    format!("{}.info.json", file_path(name, filename))
}

/// Storage path of the yank marker of a distribution.
pub(crate) fn yank_path(name: &ProjectName, filename: &str) -> String {
    format!("{}{YANKED_SUFFIX}", file_path(name, filename))
}

/// The prefix under which every asset of a project lives.
pub(crate) fn project_prefix(name: &ProjectName) -> String {
    format!("{PACKAGES}/{name}/")
}

/// Absolute URL of a distribution under the repository the client addressed.
pub(crate) fn file_url(origin_url: &str, name: &ProjectName, filename: &str) -> String {
    let path = NormalizedPath::from_segments([PACKAGES, name.as_str(), filename])
        .map_or_else(|_| file_path(name, filename), |path| path.encode());
    format!("{origin_url}/{path}")
}

fn not_found<T>() -> Result<T, FormatError> {
    Err(FormatError::NotFound)
}

pub(crate) fn parse_route(method: &Method, path: &NormalizedPath) -> Result<Route, FormatError> {
    let segments: Vec<&str> = path.segments().collect();
    match segments.as_slice() {
        [] => match *method {
            Method::POST => Ok(Route::Upload),
            Method::GET | Method::HEAD => not_found(),
            _ => Err(FormatError::MethodNotAllowed { allow: "POST" }),
        },
        [SIMPLE] => Ok(Route::Root),
        [SIMPLE, project] => Ok(Route::Project {
            name: ProjectName::parse(project).or_else(|_| not_found())?,
        }),
        [PACKAGES, project, file] => file_route(project, file),
        [JSON_API, project, "json"] => Ok(Route::JsonApi {
            name: ProjectName::parse(project).or_else(|_| not_found())?,
            version: None,
        }),
        [JSON_API, project, version, "json"] => Ok(Route::JsonApi {
            name: ProjectName::parse(project).or_else(|_| not_found())?,
            version: Some(Version::parse(version).or_else(|_| not_found())?),
        }),
        _ => not_found(),
    }
}

fn file_route(project: &str, file: &str) -> Result<Route, FormatError> {
    let name = ProjectName::parse(project).or_else(|_| not_found())?;
    let (base, kind) = if let Some(base) = file.strip_suffix(METADATA_SUFFIX) {
        (base, Kind::Metadata)
    } else if let Some(base) = file.strip_suffix(YANKED_SUFFIX) {
        (base, Kind::Yank)
    } else {
        (file, Kind::File)
    };
    let dist = DistFilename::parse(base).or_else(|_| not_found())?;
    // The project in the URL and the project in the file name are one project, or there is no
    // such file: otherwise the path rule, the coordinate and the storage key could disagree.
    if dist.project() != &name {
        return not_found();
    }
    Ok(match kind {
        Kind::File => Route::File { name, file: dist },
        Kind::Metadata => Route::FileMetadata { name, file: dist },
        Kind::Yank => Route::Yank { name, file: dist },
    })
}

enum Kind {
    File,
    Metadata,
    Yank,
}

/// The canonical path of a route: what is authorized and what `handle` reads.
fn canonical_path(route: &Route) -> Result<NormalizedPath, FormatError> {
    let segments: Vec<String> = match route {
        Route::Upload => Vec::new(),
        Route::Root => vec![SIMPLE.to_owned()],
        Route::Project { name } => vec![SIMPLE.to_owned(), name.to_string()],
        Route::File { name, file } => {
            vec![
                PACKAGES.to_owned(),
                name.to_string(),
                file.as_str().to_owned(),
            ]
        }
        Route::FileMetadata { name, file } => vec![
            PACKAGES.to_owned(),
            name.to_string(),
            format!("{}{METADATA_SUFFIX}", file.as_str()),
        ],
        Route::Yank { name, file } => vec![
            PACKAGES.to_owned(),
            name.to_string(),
            format!("{}{YANKED_SUFFIX}", file.as_str()),
        ],
        Route::JsonApi { name, version } => {
            let mut parts = vec![JSON_API.to_owned(), name.to_string()];
            if let Some(version) = version {
                parts.push(version.to_string());
            }
            parts.push("json".to_owned());
            parts
        }
    };
    NormalizedPath::from_segments(segments).map_err(FormatError::from)
}

impl Route {
    /// The coordinate that content selectors see, when the route names a project.
    fn coordinate(&self) -> Option<Coordinate> {
        match self {
            Self::Upload | Self::Root => None,
            Self::Project { name } => Some(Coordinate::new(None, Some(name.to_string()), None)),
            Self::JsonApi { name, version } => Some(Coordinate::new(
                None,
                Some(name.to_string()),
                version.as_ref().map(ToString::to_string),
            )),
            Self::File { name, file }
            | Self::FileMetadata { name, file }
            | Self::Yank { name, file } => {
                let mut coordinate = Coordinate::new(
                    None,
                    Some(name.to_string()),
                    Some(file.version().to_string()),
                );
                coordinate.extension = Some(file.extension().to_owned());
                Some(coordinate)
            }
        }
    }

    /// The action to authorize and whether the document is an index, or the method error.
    fn action(&self, method: &Method) -> Result<(Action, bool), FormatError> {
        let read = *method == Method::GET || *method == Method::HEAD;
        let refuse = |allow| Err(FormatError::MethodNotAllowed { allow });
        match self {
            Self::Upload => {
                if *method == Method::POST {
                    Ok((Action::Write, false))
                } else {
                    refuse("POST")
                }
            }
            Self::Root if read => Ok((Action::Browse, true)),
            Self::Project { .. } | Self::JsonApi { .. } if read => Ok((Action::Read, true)),
            Self::FileMetadata { .. } if read => Ok((Action::Read, false)),
            Self::File { .. } => match *method {
                Method::GET | Method::HEAD => Ok((Action::Read, false)),
                Method::DELETE => Ok((Action::Delete, false)),
                _ => refuse("GET, HEAD, DELETE"),
            },
            Self::Yank { .. } => match *method {
                Method::GET | Method::HEAD => Ok((Action::Read, false)),
                // Yanking is a change of state, not a removal of content: it takes the write
                // privilege, which is also what lets a maintainer undo it.
                Method::PUT | Method::DELETE => Ok((Action::Write, false)),
                _ => refuse("GET, HEAD, PUT, DELETE"),
            },
            Self::Root
            | Self::Project { .. }
            | Self::JsonApi { .. }
            | Self::FileMetadata { .. } => refuse("GET, HEAD"),
        }
    }
}

/// Parses the route of `head` without applying the method rules (the handler needs the route).
pub(crate) fn route_of(head: &RequestHead) -> Result<(Route, NormalizedPath), FormatError> {
    let rules = PathRules {
        allow_trailing_slash: true,
        ..PathRules::default()
    };
    let raw = NormalizedPath::parse_with(&head.path, &rules)?;
    let route = parse_route(&head.method, &raw)?;
    let path = canonical_path(&route)?;
    Ok((route, path))
}

/// Normalizes the request once and decides what it needs, before the body is read.
pub(crate) fn classify(
    _repository: &Repository,
    head: &RequestHead,
) -> Result<ArtifactRequest, FormatError> {
    let (route, path) = route_of(head)?;
    let (action, index) = route.action(&head.method)?;
    let mut request = ArtifactRequest::new(head, path, action);
    if let Some(coordinate) = route.coordinate() {
        request = request.with_coordinate(coordinate);
    }
    if index {
        request = request.index();
    }
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rustryx_core::identity::Principal;
    use rustryx_core::model::{RepoFormat, RepoType};
    use rustryx_format_core::ContentKind;
    use rustryx_testkit::fixtures;
    use url::Url;

    fn head(method: Method, path: &str) -> RequestHead {
        RequestHead::new(
            method,
            path,
            Principal::anonymous(),
            Url::parse("https://r.test").unwrap(),
        )
    }

    fn classify_path(method: Method, path: &str) -> Result<ArtifactRequest, FormatError> {
        let repository = fixtures::repository("pypi-hosted", RepoFormat::Pypi, RepoType::Hosted);
        classify(&repository, &head(method, path))
    }

    #[test]
    fn project_pages_are_canonicalized_and_carry_the_coordinate() {
        for raw in [
            "simple/Foo_Bar/",
            "simple/foo.bar",
            "/simple/FOO--bar/",
            "simple/foo-bar",
        ] {
            let request = classify_path(Method::GET, raw).unwrap();
            assert_eq!(request.path.as_str(), "simple/foo-bar", "{raw}");
            assert_eq!(request.action, Action::Read);
            assert_eq!(request.kind, ContentKind::Index);
            let coordinate = request.coordinate.unwrap();
            assert_eq!(coordinate.name.as_deref(), Some("foo-bar"));
            assert_eq!(coordinate.version, None);
        }
    }

    #[test]
    fn files_carry_name_version_and_extension_before_the_body_is_read() {
        let request = classify_path(
            Method::GET,
            "packages/foo-bar/Foo_Bar-1.0.0-py3-none-any.whl",
        )
        .unwrap();
        assert_eq!(
            request.path.as_str(),
            "packages/foo-bar/Foo_Bar-1.0.0-py3-none-any.whl"
        );
        assert_eq!(request.kind, ContentKind::Artifact);
        let coordinate = request.coordinate.unwrap();
        assert_eq!(coordinate.name.as_deref(), Some("foo-bar"));
        assert_eq!(coordinate.version.as_deref(), Some("1.0.0"));
        assert_eq!(coordinate.extension.as_deref(), Some("whl"));
        // A spelling of the project in the URL is canonicalized, so path rules see one form.
        let spelled = classify_path(
            Method::GET,
            "packages/Foo_Bar/Foo_Bar-1.0.0-py3-none-any.whl",
        )
        .unwrap();
        assert_eq!(spelled.path.as_str(), request.path.as_str());
    }

    #[test]
    fn actions_follow_the_method_and_the_route() {
        let action = |method, path| classify_path(method, path).unwrap().action;
        let file = "packages/foo/foo-1.0.tar.gz";
        assert_eq!(action(Method::GET, file), Action::Read);
        assert_eq!(action(Method::HEAD, file), Action::Read);
        assert_eq!(action(Method::DELETE, file), Action::Delete);
        assert_eq!(action(Method::POST, ""), Action::Write);
        assert_eq!(action(Method::POST, "/"), Action::Write);
        assert_eq!(
            action(Method::PUT, "packages/foo/foo-1.0.tar.gz.yanked"),
            Action::Write
        );
        assert_eq!(
            action(Method::DELETE, "packages/foo/foo-1.0.tar.gz.yanked"),
            Action::Write
        );
        assert_eq!(
            action(Method::GET, "packages/foo/foo-1.0.tar.gz.yanked"),
            Action::Read
        );
        assert_eq!(
            action(Method::GET, "packages/foo/foo-1.0.tar.gz.metadata"),
            Action::Read
        );
        assert_eq!(action(Method::GET, "simple"), Action::Browse);
        assert_eq!(action(Method::GET, "pypi/foo/json"), Action::Read);
        let json = classify_path(Method::GET, "pypi/Foo/1.0.0/json").unwrap();
        assert_eq!(json.path.as_str(), "pypi/foo/1.0.0/json");
        assert_eq!(json.coordinate.unwrap().version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn methods_a_route_does_not_take_are_405() {
        for (method, path, allow) in [
            (Method::PUT, "", "POST"),
            (Method::DELETE, "", "POST"),
            (Method::POST, "simple/foo", "GET, HEAD"),
            (Method::PUT, "simple", "GET, HEAD"),
            (Method::POST, "pypi/foo/json", "GET, HEAD"),
            (
                Method::PUT,
                "packages/foo/foo-1.0.tar.gz",
                "GET, HEAD, DELETE",
            ),
            (
                Method::POST,
                "packages/foo/foo-1.0.tar.gz.metadata",
                "GET, HEAD",
            ),
            (
                Method::PATCH,
                "packages/foo/foo-1.0.tar.gz.yanked",
                "GET, HEAD, PUT, DELETE",
            ),
        ] {
            match classify_path(method.clone(), path) {
                Err(FormatError::MethodNotAllowed { allow: got }) => {
                    assert_eq!(got, allow, "{method} {path}");
                }
                other => panic!("{method} {path}: {other:?}"),
            }
        }
        assert!(matches!(
            classify_path(Method::GET, ""),
            Err(FormatError::NotFound)
        ));
    }

    #[test]
    fn unknown_and_inconsistent_paths_are_404() {
        for path in [
            "foo",
            "simple/foo/extra",
            "simple/-bad-",
            "packages/foo",
            "packages/foo/foo-1.0.egg",
            "packages/foo/foo-1.0.tar.gz.info.json",
            "packages/foo/foo-1.0.tar.gz.asc",
            "packages/other/foo-1.0.tar.gz",
            "packages/foo/other-1.0.tar.gz",
            "packages/foo/foo-1.0.tar.gz/more",
            "pypi/foo",
            "pypi/foo/json/more",
            "pypi/foo/not-a-version/json",
            "pypi/-bad-/json",
        ] {
            assert!(
                matches!(classify_path(Method::GET, path), Err(FormatError::NotFound)),
                "{path}"
            );
        }
    }

    #[test]
    fn must_not_accept_traversal_in_any_encoding() {
        for path in [
            "simple/../packages",
            "packages/foo/../foo/foo-1.0.tar.gz",
            "packages/foo/%2e%2e/foo-1.0.tar.gz",
            "packages/foo/..%5cfoo-1.0.tar.gz",
            "packages/foo/%252e%252e/x",
            "packages/foo/foo-1.0.tar.gz%00",
            "simple//foo",
            "packages%2ffoo/foo-1.0.tar.gz",
            "simple/foo%2f..%2f..",
        ] {
            assert!(
                matches!(
                    classify_path(Method::GET, path),
                    Err(FormatError::BadRequest(_))
                ),
                "{path}"
            );
        }
    }

    #[test]
    fn storage_paths_are_derived_from_the_validated_name_only() {
        let name = ProjectName::parse("Foo.Bar").unwrap();
        assert_eq!(
            file_path(&name, "foo_bar-1.tar.gz"),
            "packages/foo-bar/foo_bar-1.tar.gz"
        );
        assert_eq!(
            metadata_path(&name, "f.whl"),
            "packages/foo-bar/f.whl.metadata"
        );
        assert_eq!(
            info_path(&name, "f.whl"),
            "packages/foo-bar/f.whl.info.json"
        );
        assert_eq!(yank_path(&name, "f.whl"), "packages/foo-bar/f.whl.yanked");
        assert_eq!(project_prefix(&name), "packages/foo-bar/");
        assert_eq!(
            file_url("https://r.test/repository/p", &name, "foo-1.0+cpu.tar.gz"),
            "https://r.test/repository/p/packages/foo-bar/foo-1.0%2Bcpu.tar.gz"
        );
    }

    proptest! {
        #[test]
        fn classify_never_panics(path in "\\PC{0,200}", method in prop::sample::select(vec![Method::GET, Method::POST, Method::PUT, Method::DELETE, Method::HEAD, Method::PATCH])) {
            let _ = classify_path(method, &path);
        }

        #[test]
        fn the_canonical_path_is_a_fixed_point(name in "[A-Za-z][A-Za-z0-9._-]{0,12}[A-Za-z0-9]") {
            if let Ok(first) = classify_path(Method::GET, &format!("simple/{name}/")) {
                let again = classify_path(Method::GET, &format!("{}/", first.path.as_str())).unwrap();
                prop_assert_eq!(first.path.as_str(), again.path.as_str());
            }
        }
    }
}

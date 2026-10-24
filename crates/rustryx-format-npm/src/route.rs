//! The URL space of an npm registry, below the repository root.
//!
//! [`Route::parse`] turns a normalized path and a method into one of the operations this crate
//! serves, or into the right refusal: `404` for a path that is not part of the protocol and `405`
//! (with `Allow`) for a known path with an unsupported method. It runs in `classify`, before the
//! body is read, so authorization sees the same operation the handler performs.
//!
//! Protocol sources: the registry API
//! (<https://github.com/npm/registry/blob/main/docs/REGISTRY-API.md>), the user and token routes
//! (<https://github.com/npm/registry/blob/main/docs/user/authentication.md>), and the routes the
//! npm client itself uses for dist-tags, unpublish and audit, which are described by what the
//! client sends rather than by a document.

use http::Method;
use rustryx_core::identity::Action;
use rustryx_core::ports::auth::Coordinate;
use rustryx_format_core::{FormatError, NormalizedPath};
use semver::Version;

use crate::name::PackageName;
use crate::version::validate_tag;

const GET_HEAD: &str = "GET, HEAD";

/// Prefix of the legacy login document id.
const COUCH_USER_PREFIX: &str = "org.couchdb.user:";

/// An operation of the registry protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// `GET /-/ping`
    Ping,
    /// `GET /-/whoami`
    Whoami,
    /// `PUT /-/user/org.couchdb.user:<name>`: the legacy `npm login`.
    Login {
        /// The user name of the URL.
        user: String,
    },
    /// `DELETE /-/user/token/<token>`: `npm logout`.
    Logout {
        /// The token to revoke.
        token: String,
    },
    /// `GET /-/v1/search`
    Search,
    /// `POST /-/npm/v1/security/advisories/bulk` and the two older audit routes.
    Audit,
    /// `GET /<package>`, `PUT /<package>`
    Packument(PackageName),
    /// `GET /<package>/<version or tag>`
    VersionDoc {
        /// The package.
        package: PackageName,
        /// A version or a dist-tag name.
        spec: String,
    },
    /// `GET /<package>/-/<file>.tgz`
    Tarball {
        /// The package.
        package: PackageName,
        /// The version the file name carries.
        version: Version,
    },
    /// `GET /-/package/<package>/dist-tags`
    DistTags(PackageName),
    /// `PUT` and `DELETE /-/package/<package>/dist-tags/<tag>`
    DistTag {
        /// The package.
        package: PackageName,
        /// The tag.
        tag: String,
    },
    /// `PUT` and `DELETE /<package>/-rev/<rev>`: unpublish a version through the document, or
    /// the whole package.
    PackageRev(PackageName),
    /// `DELETE /<package>/-/<file>.tgz/-rev/<rev>`: remove the tarball of a version.
    TarballRev {
        /// The package.
        package: PackageName,
        /// The version the file name carries.
        version: Version,
    },
}

fn not_found() -> FormatError {
    FormatError::NotFound
}

fn only(allowed: &'static str, method: &Method, list: &[Method]) -> Result<(), FormatError> {
    if list.contains(method) {
        Ok(())
    } else {
        Err(FormatError::MethodNotAllowed { allow: allowed })
    }
}

fn reads(method: &Method) -> bool {
    method == Method::GET || method == Method::HEAD
}

impl Route {
    /// Parses the normalized path of a request.
    ///
    /// # Errors
    /// [`FormatError::NotFound`] for a path outside the protocol,
    /// [`FormatError::MethodNotAllowed`] for a method the path does not take.
    pub fn parse(method: &Method, path: &NormalizedPath) -> Result<Self, FormatError> {
        let segments: Vec<&str> = path.segments().collect();
        match segments.split_first() {
            None => Err(not_found()),
            Some((&"-", rest)) => Self::parse_api(method, rest),
            Some((first, rest)) => Self::parse_package_route(method, first, rest),
        }
    }

    fn parse_api(method: &Method, rest: &[&str]) -> Result<Self, FormatError> {
        match rest {
            ["ping"] => {
                only(GET_HEAD, method, &[Method::GET, Method::HEAD])?;
                Ok(Self::Ping)
            }
            ["whoami"] => {
                only(GET_HEAD, method, &[Method::GET, Method::HEAD])?;
                Ok(Self::Whoami)
            }
            ["v1", "search"] => {
                only(GET_HEAD, method, &[Method::GET, Method::HEAD])?;
                Ok(Self::Search)
            }
            ["npm", "v1", "security", "advisories", "bulk"]
            | ["npm", "v1", "security", "audits"]
            | ["npm", "v1", "security", "audits", "quick"] => {
                only("POST", method, &[Method::POST])?;
                Ok(Self::Audit)
            }
            ["user", document] => {
                let user = document
                    .strip_prefix(COUCH_USER_PREFIX)
                    .filter(|user| valid_user(user))
                    .ok_or_else(not_found)?;
                only("PUT", method, &[Method::PUT])?;
                Ok(Self::Login {
                    user: user.to_owned(),
                })
            }
            ["user", "token", token] => {
                let valid = !token.is_empty()
                    && token.len() <= 256
                    && token.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~')
                    });
                if !valid {
                    return Err(not_found());
                }
                only("DELETE", method, &[Method::DELETE])?;
                Ok(Self::Logout {
                    token: (*token).to_owned(),
                })
            }
            ["package", tail @ ..] => Self::parse_dist_tags(method, tail),
            _ => Err(not_found()),
        }
    }

    fn parse_dist_tags(method: &Method, tail: &[&str]) -> Result<Self, FormatError> {
        let (package, rest) = take_package(tail)?;
        match rest {
            ["dist-tags"] => {
                only(GET_HEAD, method, &[Method::GET, Method::HEAD])?;
                Ok(Self::DistTags(package))
            }
            ["dist-tags", tag] => {
                validate_tag(tag).map_err(|_| not_found())?;
                only("PUT, DELETE", method, &[Method::PUT, Method::DELETE])?;
                Ok(Self::DistTag {
                    package,
                    tag: (*tag).to_owned(),
                })
            }
            _ => Err(not_found()),
        }
    }

    fn parse_package_route(
        method: &Method,
        first: &str,
        rest: &[&str],
    ) -> Result<Self, FormatError> {
        let mut all = vec![first];
        all.extend_from_slice(rest);
        let (package, tail) = take_package(&all)?;
        match tail {
            [] => {
                only(
                    "GET, HEAD, PUT",
                    method,
                    &[Method::GET, Method::HEAD, Method::PUT],
                )?;
                Ok(Self::Packument(package))
            }
            ["-", file] => {
                let version = package.version_of_tarball(file).ok_or_else(not_found)?;
                only(GET_HEAD, method, &[Method::GET, Method::HEAD])?;
                Ok(Self::Tarball { package, version })
            }
            ["-rev", rev] => {
                valid_rev(rev)?;
                only("PUT, DELETE", method, &[Method::PUT, Method::DELETE])?;
                Ok(Self::PackageRev(package))
            }
            ["-", file, "-rev", rev] => {
                valid_rev(rev)?;
                let version = package.version_of_tarball(file).ok_or_else(not_found)?;
                only("DELETE", method, &[Method::DELETE])?;
                Ok(Self::TarballRev { package, version })
            }
            [spec] if *spec != "-" && *spec != "-rev" => {
                let looks_like_spec =
                    crate::version::parse_version(spec).is_ok() || validate_tag(spec).is_ok();
                if !looks_like_spec {
                    return Err(not_found());
                }
                only(GET_HEAD, method, &[Method::GET, Method::HEAD])?;
                Ok(Self::VersionDoc {
                    package,
                    spec: (*spec).to_owned(),
                })
            }
            _ => Err(not_found()),
        }
    }

    /// The action authorization checks for this operation with this method.
    #[must_use]
    pub fn action(&self, method: &Method) -> Action {
        match self {
            // Logging in or out concerns the caller's own credentials, not the content of the
            // repository; read access is the least that lets a user have a token for it.
            Self::Ping
            | Self::Whoami
            | Self::Search
            | Self::Audit
            | Self::Login { .. }
            | Self::Logout { .. }
            | Self::VersionDoc { .. }
            | Self::Tarball { .. }
            | Self::DistTags(_) => Action::Read,
            Self::Packument(_) => {
                if *method == Method::PUT {
                    Action::Write
                } else {
                    Action::Read
                }
            }
            Self::DistTag { .. } => {
                if *method == Method::DELETE {
                    Action::Delete
                } else {
                    Action::Write
                }
            }
            // A `PUT` to the `-rev` form removes versions (that is how npm unpublishes one), so it
            // needs the right to delete, not only to write.
            Self::PackageRev(_) | Self::TarballRev { .. } => Action::Delete,
        }
    }

    /// True for documents a group merges and a proxy caches as metadata.
    #[must_use]
    pub fn is_index(&self, method: &Method) -> bool {
        match self {
            Self::Packument(_) => reads(method),
            Self::VersionDoc { .. } | Self::DistTags(_) | Self::Search => true,
            _ => false,
        }
    }

    /// The package the operation addresses, if it addresses one.
    #[must_use]
    pub fn package(&self) -> Option<&PackageName> {
        match self {
            Self::Packument(package)
            | Self::DistTags(package)
            | Self::PackageRev(package)
            | Self::VersionDoc { package, .. }
            | Self::Tarball { package, .. }
            | Self::DistTag { package, .. }
            | Self::TarballRev { package, .. } => Some(package),
            _ => None,
        }
    }

    /// The coordinate content selectors see: scope as group, name, version when the path names
    /// one. Lowercase, the form components are stored in.
    #[must_use]
    pub fn coordinate(&self) -> Option<Coordinate> {
        let package = self.package()?.canonical();
        let version = match self {
            Self::Tarball { version, .. } | Self::TarballRev { version, .. } => {
                Some(version.to_string())
            }
            _ => None,
        };
        Some(Coordinate::new(
            package.scope().map(str::to_owned),
            Some(package.name().to_owned()),
            version,
        ))
    }
}

/// Reads the package off the front of a segment list and returns the rest.
fn take_package<'a, 'b>(
    segments: &'b [&'a str],
) -> Result<(PackageName, &'b [&'a str]), FormatError> {
    match segments {
        [first, second, rest @ ..] if first.starts_with('@') => {
            let package =
                PackageName::from_segments(first, Some(second)).map_err(|_| not_found())?;
            Ok((package, rest))
        }
        [first, rest @ ..] if !first.starts_with('@') => {
            let package = PackageName::from_segments(first, None).map_err(|_| not_found())?;
            Ok((package, rest))
        }
        _ => Err(not_found()),
    }
}

fn valid_rev(rev: &str) -> Result<(), FormatError> {
    let valid = !rev.is_empty()
        && rev.len() <= 128
        && rev
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(FormatError::bad_request("invalid revision"))
    }
}

fn valid_user(user: &str) -> bool {
    !user.is_empty()
        && user.len() <= 128
        && user
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'@' | b'+'))
}

#[cfg(test)]
mod tests;

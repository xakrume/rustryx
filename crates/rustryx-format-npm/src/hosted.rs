//! Hosted npm repositories: the read routes and the dist-tags.
//!
//! Where things live (see [`crate::docstore`] for why):
//!
//! * `<package>/-/<name>-<version>.tgz`: the tarball, a component `(scope, name, version)`,
//!   published create-only, so a version can never be replaced;
//! * `<package>/-/doc/<generation>.json`: the package document, one immutable generation per
//!   change.

use std::sync::Arc;

use axum_core::body::Body;
use http::{HeaderMap, Method, Response, StatusCode};
use rustryx_core::model::RepoFormat;
use rustryx_format_core::conditional::if_none_match_hits;
use rustryx_format_core::stream::read_bounded;
use rustryx_format_core::{ArtifactRequest, FormatContext, FormatError};

use crate::disttags::LATEST;
use crate::docstore::{Change, DocStore};
use crate::handler::NpmHandler;
use crate::metrics;
use crate::name::PackageName;
use crate::packument::{Packument, ParseMode, rev_of};
use crate::respond;
use crate::route::Route;
use crate::variant::Variant;
use crate::version::parse_version;
use crate::view::{Part, RenderSource, document_response, not_modified, view_etag};

/// Largest body of a dist-tag `PUT`: a JSON string holding a version.
const MAX_TAG_BODY: u64 = 1024;

impl NpmHandler {
    /// Serves a request on a hosted repository.
    pub(crate) async fn hosted(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        route: Route,
        body: Body,
    ) -> Result<Response<Body>, FormatError> {
        let put = *req.method() == Method::PUT;
        match route {
            Route::Packument(package) if put => {
                self.hosted_put(cx, req, &package, body, false).await
            }
            Route::Packument(package) => self.hosted_packument(cx, req, &package).await,
            Route::VersionDoc { package, spec } => {
                self.hosted_part(cx, req, &package, Part::Version(spec))
                    .await
            }
            Route::DistTags(package) => self.hosted_part(cx, req, &package, Part::DistTags).await,
            Route::Tarball { .. } => cx.hosted().download(req).await,
            Route::DistTag { package, tag } if put => {
                self.hosted_set_tag(cx, &package, &tag, body).await
            }
            Route::DistTag { package, tag } => self.hosted_remove_tag(cx, &package, &tag).await,
            Route::PackageRev(package) if put => {
                self.hosted_put(cx, req, &package, body, true).await
            }
            Route::PackageRev(package) => self.unpublish_package(cx, &package).await,
            Route::TarballRev { package, version } => {
                self.unpublish_version(cx, &package, &version).await
            }
            Route::Search => self.hosted_search(cx, req).await,
            // Answered before the repository type is looked at.
            Route::Ping
            | Route::Whoami
            | Route::Login { .. }
            | Route::Logout { .. }
            | Route::Audit => Err(FormatError::NotFound),
        }
    }

    async fn hosted_packument(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        package: &PackageName,
    ) -> Result<Response<Body>, FormatError> {
        let store = DocStore::new(cx, package);
        let stored = store.latest().await?.ok_or(FormatError::NotFound)?;
        let variant = Variant::of_headers(&req.head.headers);
        let base = req.head.repository_url(&cx.origin_repository().name);
        let digest = stored.asset.checksums.sha256.clone();
        let etag = view_etag(&digest, variant, &base);
        if if_none_match_hits(&req.head.headers, &etag) {
            return Ok(not_modified(&etag, HeaderMap::new()));
        }
        let bytes = store.read(&stored, self.config.max_packument_bytes).await?;
        let body = self
            .render_document(
                cx.limits().blocking_permit_wait,
                RenderSource {
                    bytes,
                    mode: ParseMode::Strict,
                    package: package.clone(),
                    variant,
                    base,
                    digest,
                },
            )
            .await?;
        metrics::packument_served("hosted", variant.label());
        Ok(document_response(
            req.method(),
            variant,
            &etag,
            body,
            HeaderMap::new(),
        ))
    }

    async fn hosted_part(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        package: &PackageName,
        part: Part,
    ) -> Result<Response<Body>, FormatError> {
        let store = DocStore::new(cx, package);
        let stored = store.latest().await?.ok_or(FormatError::NotFound)?;
        let bytes = store.read(&stored, self.config.max_packument_bytes).await?;
        let rendered = self
            .render_part(
                cx.limits().blocking_permit_wait,
                RenderSource {
                    bytes,
                    mode: ParseMode::Strict,
                    package: package.clone(),
                    variant: Variant::Full,
                    base: req.head.repository_url(&cx.origin_repository().name),
                    digest: stored.asset.checksums.sha256.clone(),
                },
                part,
            )
            .await?
            .ok_or(FormatError::NotFound)?;
        let response = respond::bytes_with_type(StatusCode::OK, "application/json", rendered);
        Ok(respond::for_method(req.method(), response))
    }

    async fn hosted_set_tag(
        &self,
        cx: &FormatContext,
        package: &PackageName,
        tag: &str,
        body: Body,
    ) -> Result<Response<Body>, FormatError> {
        let bytes = read_bounded(body, MAX_TAG_BODY).await.map_err(|_| {
            FormatError::bad_request("the body must be a JSON string holding a version")
        })?;
        let text: String = serde_json::from_slice(&bytes).map_err(|_| {
            FormatError::bad_request("the body must be a JSON string holding a version")
        })?;
        let version = parse_version(&text)
            .map_err(|_| FormatError::bad_request("the body is not a valid semantic version"))?;
        let tag = tag.to_owned();
        let now = cx.now();
        let change = Arc::new(move |current: Option<Packument>, _applied: bool| {
            let mut document = current.ok_or(FormatError::NotFound)?;
            if !document.versions().contains_key(&version) {
                return Err(FormatError::NotFound);
            }
            if document.dist_tags().get(&tag) == Some(&version) {
                return Ok(Change::Keep(document));
            }
            document
                .dist_tags_mut()
                .set(&tag, version.clone())
                .map_err(|e| FormatError::bad_request(e.to_string()))?;
            document.touch(now);
            Ok(Change::Replace(document))
        });
        let updated = DocStore::new(cx, package)
            .update(&self.blocking, self.config.max_packument_bytes, change)
            .await?;
        if updated.changed {
            metrics::dist_tag_change("set");
        }
        Ok(respond::ok_with_id(
            StatusCode::OK,
            &package.full(),
            &rev_of(updated.document.versions().len(), &updated.digest),
        ))
    }

    async fn hosted_remove_tag(
        &self,
        cx: &FormatContext,
        package: &PackageName,
        tag: &str,
    ) -> Result<Response<Body>, FormatError> {
        if tag == LATEST {
            // Every package keeps a `latest`: `npm install <package>` resolves it and fails
            // without it, and the registry's own documents promise it.
            return Err(FormatError::bad_request("the latest tag cannot be removed"));
        }
        let tag = tag.to_owned();
        let now = cx.now();
        let change = Arc::new(move |current: Option<Packument>, applied: bool| {
            let mut document = current.ok_or(FormatError::NotFound)?;
            if !document.dist_tags_mut().remove(&tag) {
                // After an attempt of ours that was superseded, the tag may already be gone:
                // that is the change this call asked for.
                return if applied {
                    Ok(Change::Keep(document))
                } else {
                    Err(FormatError::NotFound)
                };
            }
            document.touch(now);
            Ok(Change::Replace(document))
        });
        let updated = DocStore::new(cx, package)
            .update(&self.blocking, self.config.max_packument_bytes, change)
            .await?;
        metrics::dist_tag_change("remove");
        Ok(respond::ok_with_id(
            StatusCode::OK,
            &package.full(),
            &rev_of(updated.document.versions().len(), &updated.digest),
        ))
    }
}

/// The component coordinate of a package version, in the form components are stored in.
pub(crate) fn component_key(
    cx: &FormatContext,
    package: &PackageName,
    version: Option<String>,
) -> rustryx_core::model::NewComponent {
    let canonical = package.canonical();
    let mut key = rustryx_core::model::NewComponent::new(
        cx.repository().id,
        RepoFormat::Npm,
        canonical.name(),
    );
    key.group_id = canonical.scope().map(str::to_owned);
    key.version = version;
    key
}

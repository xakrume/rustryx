//! Removing and deprecating versions of a hosted package.
//!
//! The npm client's flows, as the public client libraries implement them:
//!
//! * `npm unpublish pkg@1.0.0` reads the document, `PUT`s it back to `/<pkg>/-rev/<rev>` without
//!   the version, then `DELETE`s `/<pkg>/-/<file>.tgz/-rev/<rev>`;
//! * `npm unpublish pkg --force` (or the last version) `DELETE`s `/<pkg>/-rev/<rev>`;
//! * `npm deprecate pkg@range "message"` reads the document, sets `deprecated` on the versions and
//!   `PUT`s it back to `/<pkg>`; an empty message undeprecates.
//!
//! The `PUT` of a document is interpreted narrowly, because the document in the body is a snapshot
//! the client took earlier and may be stale: a plain `PUT` applies only the `deprecated` fields
//! (never the dist-tags, never a version the registry does not have), and the `-rev` form removes
//! the versions that are no longer in the body. Removing a version deletes its tarball and its
//! component at once, so the `DELETE` of the tarball that follows finds nothing and answers `404`,
//! which the npm client treats as done. `_rev` is not enforced: the document changes by compare
//! and set on its generation (see [`crate::docstore`]), which is the concurrency control this
//! registry has.

use std::sync::Arc;

use axum_core::body::Body;
use http::{Response, StatusCode};
use rustryx_core::audit::ActorRef;
use rustryx_core::error::{MutationError, StoreError};
use rustryx_core::model::{Component, WriteOptions};
use rustryx_core::ports::{ComponentFilter, PageRequest};
use rustryx_format_core::{FormatContext, FormatError, NormalizedPath};
use semver::Version;

use crate::docstore::{Change, DocStore};
use crate::handler::NpmHandler;
use crate::hosted::component_key;
use crate::metrics;
use crate::name::PackageName;
use crate::packument::{Packument, rev_of};
use crate::publish::{deprecation_patch, retained_versions};
use crate::respond;

/// Components read per page when a whole package is removed.
const COMPONENT_PAGE: u32 = 200;

/// Pages read at most; a package with more versions than this is unpublished in several calls.
const COMPONENT_PAGES: usize = 500;

impl NpmHandler {
    /// Deletes every asset of a component, then the component.
    async fn remove_component(
        &self,
        cx: &FormatContext,
        component: &Component,
    ) -> Result<(), FormatError> {
        for asset in cx.ports().assets.list_by_component(component.id).await? {
            let path = NormalizedPath::parse(&asset.path)?;
            match cx.hosted().delete(&path).await {
                Ok(_) | Err(FormatError::NotFound) => {}
                Err(error) => return Err(error),
            }
        }
        let options = WriteOptions::new().with_actor(ActorRef::of(cx.principal()));
        match cx
            .ports()
            .components
            .delete_checked(component.id, &options)
            .await
        {
            Ok(()) | Err(MutationError::Store(StoreError::NotFound(_))) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Removes the tarball and the component of one version. False when there was nothing to
    /// remove. The write policy `deny`, immutability rules and holds refuse here, before anything
    /// of the package document changes.
    pub(crate) async fn remove_version_content(
        &self,
        cx: &FormatContext,
        package: &PackageName,
        version: &Version,
    ) -> Result<bool, FormatError> {
        let key = component_key(cx, package, Some(version.to_string()));
        match cx.ports().components.find(&key).await? {
            Some(component) => {
                self.remove_component(cx, &component).await?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// `DELETE /<package>/-rev/<rev>`: the whole package.
    pub(crate) async fn unpublish_package(
        &self,
        cx: &FormatContext,
        package: &PackageName,
    ) -> Result<Response<Body>, FormatError> {
        let canonical = package.canonical();
        let filter = ComponentFilter {
            repository_id: Some(cx.repository().id),
            group_id: canonical.scope().map(str::to_owned),
            name: Some(canonical.name().to_owned()),
            ..ComponentFilter::default()
        };
        let mut removed_any = false;
        let mut after = None;
        for _ in 0..COMPONENT_PAGES {
            let page = cx
                .ports()
                .components
                .list(
                    &filter,
                    &PageRequest {
                        limit: COMPONENT_PAGE,
                        after: after.clone(),
                    },
                )
                .await?;
            for component in &page.items {
                self.remove_component(cx, component).await?;
                removed_any = true;
            }
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        let store = DocStore::new(cx, package);
        let had_document = store.latest().await?.is_some();
        store.delete_all().await?;
        if !removed_any && !had_document {
            return Err(FormatError::NotFound);
        }
        metrics::unpublish("package");
        Ok(respond::ok(StatusCode::OK))
    }

    /// `DELETE /<package>/-/<file>.tgz/-rev/<rev>`: one version.
    pub(crate) async fn unpublish_version(
        &self,
        cx: &FormatContext,
        package: &PackageName,
        version: &Version,
    ) -> Result<Response<Body>, FormatError> {
        if !self.remove_version_content(cx, package, version).await? {
            return Err(FormatError::NotFound);
        }
        self.drop_versions(cx, package, std::slice::from_ref(version), None)
            .await?;
        metrics::unpublish("version");
        Ok(respond::ok(StatusCode::OK))
    }

    /// Takes versions out of the package document (their content is already gone). When no
    /// version is left the whole document goes.
    async fn drop_versions(
        &self,
        cx: &FormatContext,
        package: &PackageName,
        versions: &[Version],
        preferred_latest: Option<Version>,
    ) -> Result<(), FormatError> {
        let store = DocStore::new(cx, package);
        let Some((current, _)) = store
            .current(&self.blocking, self.config.max_packument_bytes)
            .await?
        else {
            return Ok(());
        };
        let all_gone = current
            .versions()
            .keys()
            .all(|existing| versions.contains(existing));
        if all_gone {
            return store.delete_all().await;
        }
        let (versions, now) = (versions.to_vec(), cx.now());
        let change = Arc::new(move |current: Option<Packument>, _applied: bool| {
            let Some(mut document) = current else {
                return Err(FormatError::NotFound);
            };
            let mut changed = false;
            for version in &versions {
                changed |= document.remove_version(version, preferred_latest.as_ref());
            }
            if changed {
                document.touch(now);
                Ok(Change::Replace(document))
            } else {
                Ok(Change::Keep(document))
            }
        });
        store
            .update(&self.blocking, self.config.max_packument_bytes, change)
            .await
            .map(|_| ())
    }

    /// `PUT /<package>/-rev/<rev>`: the document without the versions that are to go.
    pub(crate) async fn unpublish_by_document(
        &self,
        cx: &FormatContext,
        package: &PackageName,
        document: Vec<u8>,
    ) -> Result<Response<Body>, FormatError> {
        let wait = cx.limits().blocking_permit_wait;
        let (retained, preferred, patch) = {
            let package = package.clone();
            self.blocking
                .run(wait, move || {
                    let (retained, preferred) = retained_versions(&document, &package)?;
                    let patch = deprecation_patch(&document, &package)?;
                    Ok::<_, FormatError>((retained, preferred, patch))
                })
                .await??
        };
        let store = DocStore::new(cx, package);
        let (current, _) = store
            .current(&self.blocking, self.config.max_packument_bytes)
            .await?
            .ok_or(FormatError::NotFound)?;
        let doomed: Vec<Version> = current
            .versions()
            .keys()
            .filter(|existing| !retained.contains(existing))
            .cloned()
            .collect();
        for version in &doomed {
            self.remove_version_content(cx, package, version).await?;
        }
        if !doomed.is_empty() {
            self.drop_versions(cx, package, &doomed, preferred).await?;
            metrics::unpublish("version");
        }
        // The same body may deprecate versions that stay.
        if doomed.len() < current.versions().len() && !patch.is_empty() {
            return self.apply_deprecations(cx, package, patch).await;
        }
        Ok(respond::ok(StatusCode::OK))
    }

    /// `PUT /<package>` without attachment: deprecation messages.
    pub(crate) async fn deprecate_by_document(
        &self,
        cx: &FormatContext,
        package: &PackageName,
        document: Vec<u8>,
    ) -> Result<Response<Body>, FormatError> {
        let wait = cx.limits().blocking_permit_wait;
        let patch = {
            let package = package.clone();
            self.blocking
                .run(wait, move || deprecation_patch(&document, &package))
                .await??
        };
        self.apply_deprecations(cx, package, patch).await
    }

    async fn apply_deprecations(
        &self,
        cx: &FormatContext,
        package: &PackageName,
        patch: Vec<(Version, Option<String>)>,
    ) -> Result<Response<Body>, FormatError> {
        let now = cx.now();
        let change = Arc::new(move |current: Option<Packument>, _applied: bool| {
            let mut document = current.ok_or(FormatError::NotFound)?;
            let mut changed = false;
            for (version, message) in &patch {
                if !document.versions().contains_key(version) {
                    // A version cannot be added without its tarball; a document that lists one
                    // is not a deprecation.
                    return Err(FormatError::bad_request(
                        "a version cannot be added without its tarball",
                    ));
                }
                let before = document
                    .versions()
                    .get(version)
                    .and_then(|m| m.get("deprecated"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                if before != *message {
                    document.set_deprecated(version, message.as_deref());
                    changed = true;
                }
            }
            if changed {
                document.touch(now);
                Ok(Change::Replace(document))
            } else {
                Ok(Change::Keep(document))
            }
        });
        let updated = DocStore::new(cx, package)
            .update(&self.blocking, self.config.max_packument_bytes, change)
            .await?;
        Ok(respond::ok_with_id(
            StatusCode::OK,
            &package.full(),
            &rev_of(updated.document.versions().len(), &updated.digest),
        ))
    }
}

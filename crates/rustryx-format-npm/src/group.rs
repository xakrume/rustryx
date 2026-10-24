//! npm groups: merging package documents and protecting names (dependency confusion).
//!
//! A group of npm repositories answers three kinds of requests differently:
//!
//! * a **tarball** is read from the first member that has it and streamed untouched (the format
//!   core does that; nothing here);
//! * a **package document** (`GET /<package>`), a **version document** (`GET /<package>/<spec>`),
//!   the **dist-tags** and a **search** are merged from every member that answers, by
//!   [`NpmMerger`] on the blocking pool of the format core;
//! * every request that names a package is **claimed** by [`NpmClaims`]: when a hosted member of
//!   the group has any version of the name, members that are not hosted and come after it are not
//!   asked at all, for documents and for tarballs. That is the defence against an attacker who
//!   publishes `acme-billing@99.0.0` on the public registry that a proxy member mirrors.
//!
//! The claim is made on the lowercase form of the name, which is the form components are stored
//! in, so a spelling with capitals does not get past it.

use bytes::Bytes;
use http::Method;
use rustryx_core::model::Repository;
use rustryx_format_core::{
    ArtifactRequest, FormatContext, FormatError, GroupIndexMerger, GroupNameClaimer,
    MemberDocument, MergeInput, MergedDocument, NameClaim,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::disttags::{DistTags, merge_dist_tags};
use crate::handler::NpmHandler;
use crate::metrics;
use crate::name::PackageName;
use crate::packument::{ABBREVIATED_MEDIA_TYPE, FULL_MEDIA_TYPE, Packument, ParseMode};
use crate::route::Route;
use crate::variant::Variant;

/// The claim of a request: the lowercase scope and name of the package it addresses.
#[derive(Debug, Clone, Copy, Default)]
pub struct NpmClaims;

impl GroupNameClaimer for NpmClaims {
    fn claim_for(&self, request: &ArtifactRequest) -> Option<NameClaim> {
        let route = Route::parse(request.method(), &request.path).ok()?;
        let package = route.package()?.canonical();
        Some(match package.scope() {
            Some(scope) => NameClaim::scoped(scope, package.name()),
            None => NameClaim::package(package.name()),
        })
    }
}

/// The merger of the documents of the members of an npm group.
#[derive(Debug, Clone, Copy, Default)]
pub struct NpmMerger;

fn unprocessable(message: &'static str) -> FormatError {
    FormatError::unprocessable(message)
}

fn json_document(content_type: &str, value: &Value) -> MergedDocument {
    MergedDocument {
        content_type: content_type.to_owned(),
        body: Bytes::from(value.to_string()),
    }
}

impl GroupIndexMerger for NpmMerger {
    fn merge(&self, input: MergeInput<'_>) -> Result<MergedDocument, FormatError> {
        let route = Route::parse(&Method::GET, &input.request.path)
            .map_err(|_| unprocessable("not a mergeable npm document"))?;
        match route {
            Route::Packument(package) => merge_packuments(&input, &package),
            Route::VersionDoc { .. } => first_document(&input),
            Route::DistTags(_) => merge_tag_documents(&input),
            Route::Search => Ok(merge_searches(&input)),
            _ => Err(unprocessable("not a mergeable npm document")),
        }
    }
}

fn merge_packuments(
    input: &MergeInput<'_>,
    package: &PackageName,
) -> Result<MergedDocument, FormatError> {
    let variant = Variant::of_headers(&input.request.head.headers);
    let mut parsed: Vec<(Packument, bool)> = Vec::with_capacity(input.documents.len());
    let mut skipped = false;
    for document in &input.documents {
        // A member that serves garbage must not take the group down: its document is left out.
        match Packument::from_slice(&document.body, ParseMode::Lenient, Some(package)) {
            Ok(doc) => parsed.push((doc, document.holds_claim)),
            Err(_) => skipped = true,
        }
    }
    metrics::group_merge(if skipped { "skipped_member" } else { "ok" });
    let merged = Packument::merge(&parsed)
        .ok_or_else(|| unprocessable("no member document could be read"))?;
    Ok(match variant {
        Variant::Abbreviated => json_document(ABBREVIATED_MEDIA_TYPE, &merged.abbreviated()),
        Variant::Full => {
            let digest = hex::encode(Sha256::digest(merged.to_stored_vec()));
            let rev = crate::packument::rev_of(merged.versions().len(), &digest);
            json_document(FULL_MEDIA_TYPE, &merged.to_value(Some(&rev)))
        }
    })
}

/// A version document comes from the first member that has the version, which is the member the
/// tarball lookup asks first, so the `dist` of the document and the tarball always agree.
fn first_document(input: &MergeInput<'_>) -> Result<MergedDocument, FormatError> {
    let first: &MemberDocument = input
        .documents
        .iter()
        .find(|d| serde_json::from_slice::<Value>(&d.body).is_ok_and(|v| v.is_object()))
        .ok_or_else(|| unprocessable("no member document could be read"))?;
    Ok(MergedDocument {
        content_type: FULL_MEDIA_TYPE.to_owned(),
        body: first.body.clone(),
    })
}

fn merge_tag_documents(input: &MergeInput<'_>) -> Result<MergedDocument, FormatError> {
    let sets: Vec<(DistTags, bool)> = input
        .documents
        .iter()
        .filter_map(|d| {
            serde_json::from_slice::<Value>(&d.body)
                .ok()
                .map(|v| (DistTags::from_value_lenient(&v), d.holds_claim))
        })
        .collect();
    if sets.is_empty() {
        return Err(unprocessable("no member document could be read"));
    }
    let refs: Vec<(&DistTags, bool)> = sets.iter().map(|(tags, holds)| (tags, *holds)).collect();
    Ok(json_document(
        FULL_MEDIA_TYPE,
        &merge_dist_tags(&refs).to_value(),
    ))
}

fn merge_searches(input: &MergeInput<'_>) -> MergedDocument {
    let mut seen: Vec<String> = Vec::new();
    let mut objects: Vec<Value> = Vec::new();
    for document in &input.documents {
        let Ok(Value::Object(result)) = serde_json::from_slice::<Value>(&document.body) else {
            continue;
        };
        let Some(Value::Array(found)) = result.get("objects") else {
            continue;
        };
        for object in found {
            let name = object
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(Value::as_str);
            if let Some(name) = name
                && !seen.iter().any(|s| s == name)
            {
                seen.push(name.to_owned());
                objects.push(object.clone());
            }
        }
    }
    let mut result = Map::new();
    result.insert("total".to_owned(), Value::from(objects.len()));
    result.insert("objects".to_owned(), Value::Array(objects));
    result.insert("time".to_owned(), Value::String(crate::search::now_text()));
    json_document(FULL_MEDIA_TYPE, &Value::Object(result))
}

impl NpmHandler {
    /// Reads from a group. Writes never get here: the engine answers `405` for them before the
    /// body is read.
    pub(crate) async fn grouped(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        route: &Route,
    ) -> Result<http::Response<axum_core::body::Body>, FormatError> {
        if let Route::Packument(_) = route {
            metrics::packument_served("group", Variant::of_headers(&req.head.headers).label());
        }
        cx.group().resolve(req).await
    }
}

/// True when a member is a hosted repository (the only kind that can hold a name claim).
#[must_use]
pub fn is_hosted(repository: &Repository) -> bool {
    repository.repo_type == rustryx_core::model::RepoType::Hosted
}

#[cfg(test)]
mod tests;

//! Groups: merging of listings and the name claim.
//!
//! * manifests and blobs: the first member that has them answers (format-core, streamed);
//! * `tags/list`: the union of the members' tags, sorted and paged again;
//! * `_catalog`: the union of the members' images;
//! * referrers: the union of the members' descriptors, first member wins per digest;
//! * a hosted member that has an image claims its name against later proxy members, for every
//!   endpoint: a group does not ask the public registry for `team/app` when an internal
//!   repository ahead of it holds `team/app` (dependency confusion).
//!
//! Members answer through the same `handle` as a repository addressed directly, so every
//! member document already uses the name of the group (`origin_repository`).

use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderValue, Method, Response, StatusCode};
use rustryx_format_core::stream::read_bounded;
use rustryx_format_core::{
    ArtifactRequest, FormatContext, FormatError, GroupIndexMerger, GroupNameClaimer, MergeInput,
    MergedDocument, NameClaim,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::OciConfig;
use crate::headers::{OCI_FILTERS_APPLIED, set_text};
use crate::parse::digest::Digest;
use crate::parse::link::{PageParams, next_link};
use crate::parse::manifest::{Descriptor, ParsedManifest};
use crate::parse::media_type::OCI_INDEX;
use crate::parse::route::OciRoute;

/// Merges the documents of the members of a group.
pub(crate) struct OciMerger {
    config: OciConfig,
}

impl OciMerger {
    pub(crate) fn new(config: OciConfig) -> Self {
        Self { config }
    }
}

impl GroupIndexMerger for OciMerger {
    fn merge(&self, input: MergeInput<'_>) -> Result<MergedDocument, FormatError> {
        let route = OciRoute::parse(input.request.path.as_str())
            .map_err(|_| FormatError::internal("a classified path failed to parse"))?;
        let params =
            PageParams::from_query(input.request.head.query.as_deref()).unwrap_or_default();
        match route {
            OciRoute::Tags { name } => {
                let tags = union_strings(&input, "tags");
                let (page, _) = params.apply(&tags?);
                Ok(json_document(
                    "application/json",
                    &json!({ "name": format!("{}/{name}", input.group.name), "tags": page }),
                ))
            }
            OciRoute::Catalog => {
                let images = union_strings(&input, "repositories");
                let (page, _) = params.apply(&images?);
                Ok(json_document(
                    "application/json",
                    &json!({ "repositories": page }),
                ))
            }
            OciRoute::Referrers { .. } => self.merge_referrers(&input),
            _ => Err(FormatError::internal(
                "an artifact request reached the index merger",
            )),
        }
    }
}

impl OciMerger {
    fn merge_referrers(&self, input: &MergeInput<'_>) -> Result<MergedDocument, FormatError> {
        let filter = input.request.head.query_param("artifactType");
        let mut by_digest: std::collections::BTreeMap<String, Descriptor> =
            std::collections::BTreeMap::new();
        let mut parsed_any = false;
        for document in &input.documents {
            let Ok(index) =
                ParsedManifest::parse(&document.body, Some(OCI_INDEX), self.config.max_descriptors)
            else {
                continue;
            };
            parsed_any = true;
            for descriptor in index.manifests {
                by_digest
                    .entry(descriptor.digest.to_string())
                    .or_insert(descriptor);
            }
        }
        if !parsed_any {
            return Err(FormatError::unprocessable(
                "no member returned a usable referrers index",
            ));
        }
        let manifests: Vec<Value> = by_digest
            .into_values()
            .filter(|d| {
                filter
                    .as_deref()
                    .is_none_or(|wanted| d.artifact_type.as_deref() == Some(wanted))
            })
            .take(self.config.referrers_page_size)
            .map(|d| d.to_json())
            .collect();
        Ok(json_document(
            OCI_INDEX,
            &json!({ "schemaVersion": 2, "mediaType": OCI_INDEX, "manifests": manifests }),
        ))
    }
}

/// The union of one string array member (`tags`, `repositories`) of the member documents,
/// sorted and deduplicated. A member that sent something else is skipped; when none is usable
/// the merge fails.
fn union_strings(input: &MergeInput<'_>, field: &str) -> Result<Vec<String>, FormatError> {
    #[derive(Deserialize)]
    struct Doc(serde_json::Map<String, Value>);
    let mut all = std::collections::BTreeSet::new();
    let mut parsed_any = false;
    for document in &input.documents {
        let Ok(Doc(object)) = serde_json::from_slice::<Doc>(&document.body) else {
            continue;
        };
        parsed_any = true;
        if let Some(Value::Array(items)) = object.get(field) {
            all.extend(items.iter().filter_map(Value::as_str).map(str::to_owned));
        }
    }
    if parsed_any {
        Ok(all.into_iter().collect())
    } else {
        Err(FormatError::unprocessable(
            "no member returned a usable listing",
        ))
    }
}

fn json_document(content_type: &str, body: &Value) -> MergedDocument {
    MergedDocument {
        content_type: content_type.to_owned(),
        body: Bytes::from(body.to_string()),
    }
}

/// Claims the image name of a request for the hosted members that have it.
pub(crate) struct OciClaimer;

impl GroupNameClaimer for OciClaimer {
    fn claim_for(&self, request: &ArtifactRequest) -> Option<NameClaim> {
        let route = OciRoute::parse(request.path.as_str()).ok()?;
        route
            .image()
            .map(|image| NameClaim::package(image.as_str()))
    }
}

/// Adds what the merger cannot say, because a merged document has no headers: the `Link` to
/// the next page and `OCI-Filters-Applied`. Only successful `GET` listings are touched.
pub(crate) async fn finish_listing(
    cx: &FormatContext,
    req: &ArtifactRequest,
    route: &OciRoute,
    config: &OciConfig,
    response: Response<Body>,
) -> Response<Body> {
    if response.status() != StatusCode::OK || req.head.method != Method::GET {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let Ok(bytes) = read_bounded(body, cx.limits().group_merge_cap_bytes).await else {
        // The body was already consumed by the failed read; the group answers 502 in that case
        // before it gets here, so this is unreachable in practice. Fail visibly.
        let mut broken = Response::new(Body::empty());
        *broken.status_mut() = StatusCode::BAD_GATEWAY;
        return broken;
    };
    let params = PageParams::from_query(req.head.query.as_deref()).unwrap_or_default();
    let origin = cx.origin_repository().name.as_str();
    let link = match (route, serde_json::from_slice::<Value>(&bytes).ok()) {
        (OciRoute::Tags { name }, Some(doc)) => {
            full_page_link(&doc, "tags", params.n).map(|last| {
                next_link(
                    &format!("/v2/{origin}/{name}/tags/list"),
                    params.n,
                    &last,
                    &[],
                )
            })
        }
        (OciRoute::Catalog, Some(doc)) => full_page_link(&doc, "repositories", params.n)
            .map(|last| next_link(&format!("/v2/{origin}/_catalog"), params.n, &last, &[])),
        (OciRoute::Referrers { name, digest }, Some(doc)) => {
            referrers_link(&doc, req, origin, name.as_str(), digest, config)
        }
        _ => None,
    };
    if let Some(link) = link {
        set_text(&mut parts.headers, http::header::LINK, &link);
    }
    if matches!(route, OciRoute::Referrers { .. }) && req.head.query_param("artifactType").is_some()
    {
        parts.headers.insert(
            OCI_FILTERS_APPLIED,
            HeaderValue::from_static("artifactType"),
        );
    }
    Response::from_parts(parts, Body::from(bytes))
}

/// The last entry of `field` when the page is as long as the `n` the client asked for: there
/// may be more, and the client should ask. (A group cannot know, because its members
/// return only a page each.)
fn full_page_link(doc: &Value, field: &str, n: Option<usize>) -> Option<String> {
    let n = n.filter(|n| *n > 0)?;
    let items = doc.get(field)?.as_array()?;
    if items.len() < n {
        return None;
    }
    items.last()?.as_str().map(str::to_owned)
}

fn referrers_link(
    doc: &Value,
    req: &ArtifactRequest,
    origin: &str,
    name: &str,
    subject: &Digest,
    config: &OciConfig,
) -> Option<String> {
    let manifests = doc.get("manifests")?.as_array()?;
    if manifests.len() < config.referrers_page_size {
        return None;
    }
    let last = manifests.last()?.get("digest")?.as_str()?;
    let filter = req.head.query_param("artifactType");
    let extra: Vec<(&str, &str)> = filter
        .as_deref()
        .map(|f| ("artifactType", f))
        .into_iter()
        .collect();
    Some(next_link(
        &format!("/v2/{origin}/{name}/referrers/{subject}"),
        None,
        last,
        &extra,
    ))
}

#[cfg(test)]
mod tests;

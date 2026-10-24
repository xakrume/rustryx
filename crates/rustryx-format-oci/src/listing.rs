//! Listings of a hosted (or cached) repository: tags, the repository's part of the catalog,
//! and the referrers of a digest.

use axum_core::body::Body;
use http::{HeaderValue, Response, StatusCode, header};
use rustryx_core::error::AuthError;
use rustryx_core::identity::{Action, Principal};
use rustryx_core::model::Repository;
use rustryx_core::ports::{ComponentRepo, Coordinate, Decision, RbacChecker, ResourceRef};
use rustryx_format_core::FormatError;
use serde_json::{Value, json};

use crate::error::OciError;
use crate::headers::{OCI_FILTERS_APPLIED, json_response, set_text};
use crate::ops::Op;
use crate::parse::digest::Digest;
use crate::parse::link::{PageParams, next_link};
use crate::parse::manifest::{Descriptor, ParsedManifest};
use crate::parse::media_type::OCI_INDEX;
use crate::parse::name::RepositoryName;
use crate::store;

/// Referrer manifests read at once.
const REFERRER_READ_CONCURRENCY: usize = 8;

/// The pagination parameters of a request, or `400`.
pub(crate) fn page_params(op: &Op<'_>) -> Result<PageParams, OciError> {
    PageParams::from_query(op.req.head.query.as_deref())
        .map_err(|error| OciError::unsupported(StatusCode::BAD_REQUEST, error.to_string()))
}

/// Puts a `Link: <...>; rel="next"` header on a response.
pub(crate) fn add_link(response: &mut Response<Body>, link: &str) {
    set_text(response.headers_mut(), header::LINK, link);
}

/// `GET <name>/tags/list`.
pub(crate) async fn tags(op: &Op<'_>, name: &RepositoryName) -> Result<Response<Body>, OciError> {
    let params = page_params(op)?;
    if !store::image_exists(op.cx, name).await? {
        return Err(OciError::name_unknown());
    }
    let all = store::list_tags(op.cx, name, op.config().tag_scan_cap).await?;
    let (page, more) = params.apply(&all);
    let body = json!({ "name": op.docker_name(name), "tags": page });
    let mut response = json_response(StatusCode::OK, "application/json", &body);
    if more && let Some(last) = page.last() {
        let link = next_link(
            &op.path_ref(&format!("{name}/tags/list")),
            params.n,
            last,
            &[],
        );
        add_link(&mut response, &link);
    }
    Ok(response)
}

/// `GET _catalog` below one repository: the images of that repository the caller may browse,
/// named `<repo>/<image>`.
pub(crate) async fn catalog(op: &Op<'_>) -> Result<Response<Body>, OciError> {
    let params = page_params(op)?;
    let images = visible_images(
        op.cx.ports().components.as_ref(),
        op.cx.ports().rbac.as_ref(),
        op.cx.principal(),
        op.cx.repository(),
        op.config().catalog_scan_cap,
    )
    .await?;
    let names: Vec<String> = images
        .into_iter()
        .map(|image| format!("{}/{image}", op.origin_name()))
        .collect();
    let (page, more) = params.apply(&names);
    let mut response = json_response(
        StatusCode::OK,
        "application/json",
        &json!({ "repositories": page }),
    );
    if more && let Some(last) = page.last() {
        add_link(
            &mut response,
            &next_link(&op.path_ref("_catalog"), params.n, last, &[]),
        );
    }
    Ok(response)
}

/// The images of `repository` that `principal` may browse, sorted.
///
/// The repository as a whole was authorized by the caller; each image is asked again,
/// because a content selector may allow some images of a repository and not others, and
/// the catalog must not name the ones the caller cannot browse.
///
/// # Errors
/// `UNAVAILABLE` when the scan cap is exceeded or authorization cannot decide.
pub(crate) async fn visible_images(
    components: &dyn ComponentRepo,
    rbac: &dyn RbacChecker,
    principal: &Principal,
    repository: &Repository,
    cap: usize,
) -> Result<Vec<String>, OciError> {
    let images = store::list_images(components, repository.id, cap).await?;
    let mut visible = Vec::with_capacity(images.len());
    for image in images {
        let Ok(name) = RepositoryName::parse(&image) else {
            continue;
        };
        if image_browsable(rbac, principal, repository, &name).await? {
            visible.push(image);
        }
    }
    Ok(visible)
}

/// True when `principal` may browse `image` of `repository`.
pub(crate) async fn image_browsable(
    rbac: &dyn RbacChecker,
    principal: &Principal,
    repository: &Repository,
    image: &RepositoryName,
) -> Result<bool, OciError> {
    let resource = ResourceRef::for_repository(repository, format!("{image}/tags/list"))
        .with_coordinate(Coordinate::new(
            Some(image.as_str().to_owned()),
            Some(image.leaf().to_owned()),
            None,
        ));
    match rbac.check(principal, Action::Browse, &resource).await {
        Ok(Decision::Allow) => Ok(true),
        Ok(Decision::Deny) => Ok(false),
        Err(AuthError::Overloaded) => Err(FormatError::Unavailable {
            reason: "authorization capacity exhausted, retry",
            retry_after: Some(std::time::Duration::from_secs(1)),
        }
        .into()),
        Err(error) => {
            // Not "nothing visible" and not "everything visible": the request fails.
            tracing::warn!(%error, "catalog authorization failed closed");
            Err(FormatError::Forbidden.into())
        }
    }
}

/// `GET <name>/referrers/<digest>` of a hosted repository.
pub(crate) async fn referrers(
    op: &Op<'_>,
    name: &RepositoryName,
    subject: &Digest,
) -> Result<Response<Body>, OciError> {
    let filter = op.req.head.query_param("artifactType");
    let mut descriptors = referrer_descriptors(op, name, subject).await?;
    descriptors.extend(tag_schema_descriptors(op, name, subject).await?);
    Ok(referrers_response(
        op,
        name,
        subject,
        descriptors,
        filter.as_deref(),
    ))
}

/// The referrers recorded as links, read from the referrer manifests.
async fn referrer_descriptors(
    op: &Op<'_>,
    name: &RepositoryName,
    subject: &Digest,
) -> Result<Vec<Descriptor>, OciError> {
    let prefix = store::referrer_prefix(name, subject);
    let mut assets = Vec::new();
    let mut after = None;
    loop {
        let page = op
            .cx
            .ports()
            .assets
            .list(
                op.cx.repository().id,
                &prefix,
                &rustryx_core::ports::PageRequest { limit: 500, after },
            )
            .await
            .map_err(FormatError::from)?;
        assets.extend(page.items);
        if assets.len() > op.config().referrers_scan_cap {
            return Err(OciError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                crate::error::OciErrorCode::Unavailable,
                "the subject has too many referrers to list",
            ));
        }
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    let prefix = prefix.as_str();
    let mut found = Vec::new();
    for chunk in assets.chunks(REFERRER_READ_CONCURRENCY) {
        let read =
            futures::future::join_all(chunk.iter().map(|asset| read_referrer(op, prefix, asset)))
                .await;
        found.extend(read.into_iter().flatten());
    }
    Ok(found)
}

/// One referrer link: the descriptor of the manifest it points at. A link whose manifest cannot
/// be read or parsed is left out, so one broken entry does not hide the others.
async fn read_referrer(
    op: &Op<'_>,
    prefix: &str,
    asset: &rustryx_core::model::Asset,
) -> Option<Descriptor> {
    let digest = asset
        .path
        .strip_prefix(prefix)
        .and_then(|rest| Digest::parse(rest).ok())?;
    let bytes = store::read_small(op.cx, asset, op.config().manifest_max_bytes)
        .await
        .ok()?;
    let manifest = ParsedManifest::parse(
        &bytes,
        Some(&asset.content_type),
        op.config().max_descriptors,
    )
    .ok()?;
    Some(manifest.as_referrer(&digest, asset.size_bytes))
}

/// Referrers that a client recorded with the tag schema (`sha256-<hex>` holding an index):
/// older clients and registries without the API push them there, and the answer must not
/// lose them.
async fn tag_schema_descriptors(
    op: &Op<'_>,
    name: &RepositoryName,
    subject: &Digest,
) -> Result<Vec<Descriptor>, OciError> {
    let path = store::manifest_path(name, &subject.referrers_tag());
    let Some(asset) = store::get_asset(op.cx, &path).await? else {
        return Ok(Vec::new());
    };
    let Ok(bytes) = store::read_small(op.cx, &asset, op.config().manifest_max_bytes).await else {
        return Ok(Vec::new());
    };
    Ok(ParsedManifest::parse(
        &bytes,
        Some(&asset.content_type),
        op.config().max_descriptors,
    )
    .map(|index| index.manifests)
    .unwrap_or_default())
}

/// The referrers response: an OCI index of the descriptors, filtered, deduplicated by digest
/// (a link wins over a tag schema entry), sorted by digest and paged by `last`.
pub(crate) fn referrers_response(
    op: &Op<'_>,
    name: &RepositoryName,
    subject: &Digest,
    descriptors: Vec<Descriptor>,
    filter: Option<&str>,
) -> Response<Body> {
    let page_size = op.config().referrers_page_size;
    let mut by_digest = std::collections::BTreeMap::new();
    for descriptor in descriptors {
        by_digest
            .entry(descriptor.digest.to_string())
            .or_insert(descriptor);
    }
    let last = op.req.head.query_param("last");
    let mut selected: Vec<Descriptor> = by_digest
        .into_iter()
        .filter(|(digest, _)| {
            last.as_ref()
                .is_none_or(|last| digest.as_str() > last.as_str())
        })
        .map(|(_, descriptor)| descriptor)
        .filter(|descriptor| {
            filter.is_none_or(|wanted| descriptor.artifact_type.as_deref() == Some(wanted))
        })
        .collect();
    let more = selected.len() > page_size;
    selected.truncate(page_size);
    let manifests: Vec<Value> = selected.iter().map(Descriptor::to_json).collect();
    let body = json!({
        "schemaVersion": 2,
        "mediaType": OCI_INDEX,
        "manifests": manifests,
    });
    let mut response = json_response(StatusCode::OK, OCI_INDEX, &body);
    if filter.is_some() {
        response.headers_mut().insert(
            OCI_FILTERS_APPLIED,
            HeaderValue::from_static("artifactType"),
        );
    }
    if more && let Some(tail) = selected.last() {
        let mut extra = Vec::new();
        if let Some(filter) = filter {
            extra.push(("artifactType", filter));
        }
        let link = next_link(
            &op.path_ref(&format!("{name}/referrers/{subject}")),
            None,
            &tail.digest.to_string(),
            &extra,
        );
        add_link(&mut response, &link);
    }
    response
}

#[cfg(test)]
mod tests;

//! Tag lists and referrers through a proxy repository: asked of the upstream live, bounded,
//! never cached.
//!
//! Both change whenever someone pushes upstream, the answers are small, and a stale list is
//! worse than a slow one (a client would conclude that a tag does not exist). The calls go
//! through the executor, so the SSRF policy, the breaker, the concurrency cap and the
//! timeouts apply as for any other upstream request; a failing upstream is a `502`/`504`,
//! which a group skips like any other failing member.

use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode, header};
use rustryx_core::error::UpstreamError as CoreUpstreamError;
use rustryx_format_core::proxy::executor::{CallClass, UpstreamCall};
use rustryx_format_core::stream::collect_bounded;
use rustryx_format_core::{FormatError, UpstreamFailureKind};
use serde::Deserialize;
use serde_json::json;

use super::auth::UpstreamAuth;
use super::names::{upstream_name, upstream_url};
use super::token_was_refused;
use crate::error::{OciError, OciErrorCode};
use crate::headers::json_response;
use crate::listing::{add_link, page_params, referrers_response};
use crate::ops::Op;
use crate::parse::digest::Digest;
use crate::parse::link::{next_link, parse_next_link};
use crate::parse::manifest::ParsedManifest;
use crate::parse::media_type::OCI_INDEX;
use crate::parse::name::RepositoryName;
use crate::parse::reference::is_tag;

/// The part of an upstream tag list that is used.
#[derive(Deserialize)]
struct UpstreamTags {
    tags: Option<Vec<String>>,
}

/// `GET <name>/tags/list`.
pub(crate) async fn tags(op: &Op<'_>, name: &RepositoryName) -> Result<Response<Body>, OciError> {
    let params = page_params(op)?;
    let upstream = upstream_name(op.cx, name)?;
    let n = params.n.map(|n| n.to_string());
    let mut query: Vec<(&str, &str)> = Vec::new();
    if let Some(n) = &n {
        query.push(("n", n));
    }
    if let Some(last) = &params.last {
        query.push(("last", last));
    }
    let (headers, body) =
        live_get_authorized(op, &upstream, "tags/list", &query, "application/json")
            .await
            .map_err(|error| {
                OciError::from(error).not_found_as(
                    OciErrorCode::NameUnknown,
                    "repository name not known to registry",
                )
            })?;
    let parsed: UpstreamTags = serde_json::from_slice(&body).map_err(|_| {
        OciError::from(FormatError::upstream(
            UpstreamFailureKind::BadStatus,
            "the upstream tag list is not valid JSON",
        ))
    })?;
    // Only what is a tag here is passed on, sorted: the upstream is not trusted to be tidy.
    let mut tags: Vec<String> = parsed
        .tags
        .unwrap_or_default()
        .into_iter()
        .filter(|tag| is_tag(tag))
        .collect();
    tags.sort();
    tags.dedup();
    let mut response = json_response(
        StatusCode::OK,
        "application/json",
        &json!({ "name": op.docker_name(name), "tags": tags }),
    );
    if let Some(last) = next_last(&headers)
        && !tags.is_empty()
    {
        let n = params.n.or_else(|| next_n(&headers));
        add_link(
            &mut response,
            &next_link(&op.path_ref(&format!("{name}/tags/list")), n, &last, &[]),
        );
    }
    Ok(response)
}

/// `GET <name>/referrers/<digest>`: the upstream's referrers API, else its tag schema index,
/// else an empty index (a registry that serves the API never answers 404 for it).
pub(crate) async fn referrers(
    op: &Op<'_>,
    name: &RepositoryName,
    subject: &Digest,
) -> Result<Response<Body>, OciError> {
    let filter = op.req.head.query_param("artifactType");
    let upstream = upstream_name(op.cx, name)?;
    let mut descriptors = Vec::new();
    for tail in [
        format!("referrers/{subject}"),
        format!("manifests/{}", subject.referrers_tag()),
    ] {
        let fetched = live_get_authorized(op, &upstream, &tail, &[], OCI_INDEX).await;
        match fetched {
            Ok((_, body)) => {
                if let Ok(index) =
                    ParsedManifest::parse(&body, Some(OCI_INDEX), op.config().max_descriptors)
                {
                    descriptors = index.manifests;
                    break;
                }
            }
            Err(FormatError::NotFound) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(referrers_response(
        op,
        name,
        subject,
        descriptors,
        filter.as_deref(),
    ))
}

/// `live_get` with the token dance: asks for authorization, and once more with a fresh token
/// when the upstream refuses the cached one.
async fn live_get_authorized(
    op: &Op<'_>,
    upstream: &str,
    tail: &str,
    query: &[(&str, &str)],
    accept: &str,
) -> Result<(HeaderMap, Bytes), FormatError> {
    let mut refresh = false;
    loop {
        let auth = op
            .shared
            .tokens
            .authorization(op.cx, upstream, refresh)
            .await?;
        match live_get(op, upstream, tail, query, &auth, accept).await {
            Err(error) if !refresh && token_was_refused(op, upstream, &auth, &error).await => {
                refresh = true;
            }
            other => return other,
        }
    }
}

/// One bounded `GET` of a document below `v2/<upstream name>/`.
async fn live_get(
    op: &Op<'_>,
    upstream: &str,
    tail: &str,
    query: &[(&str, &str)],
    auth: &UpstreamAuth,
    accept: &str,
) -> Result<(HeaderMap, Bytes), FormatError> {
    let repository = op.cx.repository();
    let config = repository
        .proxy
        .as_ref()
        .ok_or_else(|| FormatError::internal("proxy repository without proxy settings"))?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::ACCEPT,
        HeaderValue::from_str(accept)
            .map_err(|_| FormatError::internal("a constant accept value is not a header"))?,
    );
    // A token and Basic credentials never travel together: the token replaces them.
    let credentials = match auth {
        UpstreamAuth::Bearer(bearer) => {
            headers.insert(header::AUTHORIZATION, bearer.clone());
            None
        }
        UpstreamAuth::Direct => config.auth.clone(),
    };
    let result = op
        .cx
        .upstream()
        .execute(UpstreamCall {
            repository: repository.id,
            repo_name: repository.name.to_string(),
            method: Method::GET,
            url: upstream_url(op.cx, upstream, tail, query)?,
            headers,
            credentials,
            max_concurrent: config.max_concurrent_upstream,
            class: CallClass::Metadata,
        })
        .await
        .map_err(|error| FormatError::upstream(error.kind(), error.to_string()))?;
    match result.status.as_u16() {
        200 => {}
        404 | 410 => return Err(FormatError::NotFound),
        401 | 403 => {
            return Err(FormatError::upstream(
                UpstreamFailureKind::Rejected,
                "the upstream refused the request",
            ));
        }
        other => return Err(FormatError::from(CoreUpstreamError::Status(other))),
    }
    let headers = result.headers.clone();
    let body = collect_bounded(result.body, op.cx.limits().max_document_bytes)
        .await
        .map_err(|_| {
            FormatError::upstream(
                UpstreamFailureKind::BadStatus,
                "the upstream document could not be read within its limit",
            )
        })?;
    Ok((headers, body))
}

/// The `last` parameter of the upstream's `rel="next"` link.
fn next_last(headers: &HeaderMap) -> Option<String> {
    next_parameter(headers, "last")
}

/// The `n` parameter of the upstream's `rel="next"` link, as a number.
fn next_n(headers: &HeaderMap) -> Option<usize> {
    next_parameter(headers, "n")?.parse().ok()
}

fn next_parameter(headers: &HeaderMap, key: &str) -> Option<String> {
    let target = headers
        .get_all(header::LINK)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(parse_next_link)?;
    let query = target.split_once('?')?.1;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.into_owned())
}

#[cfg(test)]
mod tests;

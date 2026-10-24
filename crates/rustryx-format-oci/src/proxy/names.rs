//! Mapping of an image name to the name asked of the upstream.
//!
//! Two rules, both of the repository and not of the client:
//!
//! * `namespace_allowlist` (globs on the image path, `ProxyConfig`): when it is not empty an
//!   image that matches none of the globs does not exist as far as this proxy is concerned.
//!   Without it a proxy is an open relay to everything the upstream account can read;
//! * the `library/` namespace: Docker Hub keeps single segment names such as `nginx` in
//!   `library/`, so `nginx` is asked as `library/nginx`. The rule applies when the upstream is
//!   a Docker Hub host or the repository's `format_config` says `library_namespace: true`.

use regex::RegexBuilder;
use rustryx_core::glob;
use rustryx_core::model::ProxyConfig;
use rustryx_format_core::{
    FormatContext, FormatError, NormalizedPath, UpstreamFailureKind, UpstreamPath,
};
use url::Url;

use crate::config::RepositoryOptions;
use crate::error::OciError;
use crate::parse::name::RepositoryName;

/// Compiled size cap of one allow list pattern.
const PATTERN_SIZE_LIMIT: usize = 256 * 1024;

/// The upstream name of `image`, or `NAME_UNKNOWN` when the allow list excludes it.
pub(crate) fn upstream_name(
    cx: &FormatContext,
    image: &RepositoryName,
) -> Result<String, OciError> {
    let repository = cx.repository();
    let config = repository
        .proxy
        .as_ref()
        .ok_or_else(|| OciError::internal("proxy repository without proxy settings"))?;
    if !allowed(config, image.as_str()) {
        return Err(OciError::name_unknown());
    }
    let remote = Url::parse(&config.remote_url)
        .map_err(|_| OciError::internal("remote_url is not a valid url"))?;
    let options = RepositoryOptions::from_format_config(&repository.format_config);
    if options.uses_library_namespace(&remote) && !image.as_str().contains('/') {
        Ok(format!("library/{image}"))
    } else {
        Ok(image.to_string())
    }
}

/// True when the allow list is empty or one of its globs matches `image`. A glob that does
/// not compile matches nothing: a broken list must not become an open one.
fn allowed(config: &ProxyConfig, image: &str) -> bool {
    if config.namespace_allowlist.is_empty() {
        return true;
    }
    config.namespace_allowlist.iter().any(|pattern| {
        let Ok(expression) = glob::to_regex(pattern) else {
            return false;
        };
        match RegexBuilder::new(&expression)
            .size_limit(PATTERN_SIZE_LIMIT)
            .build()
        {
            Ok(regex) => regex.is_match(image),
            Err(error) => {
                tracing::warn!(%error, "a namespace allow list pattern does not compile; it matches nothing");
                false
            }
        }
    })
}

/// `v2/<upstream name>/<tail>` as the path below the upstream's `remote_url`.
pub(crate) fn upstream_path(upstream_name: &str, tail: &str) -> Result<UpstreamPath, FormatError> {
    NormalizedPath::parse(&format!("v2/{upstream_name}/{tail}"))
        .map(UpstreamPath::Decoded)
        .map_err(|_| {
            FormatError::internal("an upstream path built from a validated name is invalid")
        })
}

/// The absolute upstream URL of `v2/<upstream name>/<tail>` below `remote_url`, with `query`
/// appended. Used by the calls that do not go through the proxy pipeline (live listings and
/// the uncached pass-through); the pipeline builds its own from [`upstream_path`].
pub(crate) fn upstream_url(
    cx: &FormatContext,
    upstream_name: &str,
    tail: &str,
    query: &[(&str, &str)],
) -> Result<Url, FormatError> {
    let config = cx
        .repository()
        .proxy
        .as_ref()
        .ok_or_else(|| FormatError::internal("proxy repository without proxy settings"))?;
    let blocked =
        |detail: &str| FormatError::upstream(UpstreamFailureKind::Blocked, detail.to_owned());
    let mut url =
        Url::parse(&config.remote_url).map_err(|_| blocked("remote_url is not a valid url"))?;
    url.set_query(None);
    url.set_fragment(None);
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|()| blocked("remote_url cannot be used as a base"))?;
        segments.pop_if_empty().push("v2");
        for part in upstream_name.split('/').chain(tail.split('/')) {
            segments.push(part);
        }
    }
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query.iter().copied());
    }
    Ok(url)
}

#[cfg(test)]
mod tests;

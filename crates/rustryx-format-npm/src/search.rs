//! `GET /-/v1/search`, the minimal form `npm search` needs.
//!
//! Specification: the search endpoint of the registry API
//! (<https://github.com/npm/registry/blob/main/docs/REGISTRY-API.md#get-v1search>).
//!
//! What is supported: `text` (matched by the component search of the repository over scope, name
//! and version), `scope:<scope>`, `size` (default 20, at most `NpmConfig::search_max_results`) and
//! `from`. The scores are constants: this registry ranks nothing. A proxy repository answers an
//! empty result (it does not forward the search to the upstream), and a group merges the results
//! of its members by package name.

use std::collections::HashSet;

use axum_core::body::Body;
use http::{Response, StatusCode};
use rustryx_core::ports::{ComponentFilter, PageRequest};
use rustryx_format_core::{ArtifactRequest, FormatContext, FormatError};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::docstore::DocStore;
use crate::handler::NpmHandler;
use crate::name::PackageName;
use crate::packument::{Packument, iso_millis};
use crate::respond;

/// Components read per page of a search scan.
const SCAN_PAGE: u32 = 200;

/// The time the way a search result prints it.
#[must_use]
pub fn now_text() -> String {
    iso_millis(OffsetDateTime::now_utc())
}

/// The answer of a repository that has nothing to search (a proxy).
#[must_use]
pub fn empty_search(method: &http::Method) -> Response<Body> {
    let body = json!({ "objects": [], "total": 0, "time": now_text() });
    respond::for_method(method, respond::json(StatusCode::OK, &body))
}

/// The parts of a search text this registry understands.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SearchText {
    /// Words to match.
    pub words: String,
    /// `scope:<name>` qualifier, without `@`.
    pub scope: Option<String>,
}

impl SearchText {
    /// Splits a text into words and the `scope:` qualifier; the other qualifiers of the registry
    /// (`author:`, `keywords:`, `not:`, ...) have nothing to match here and are dropped.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut words = Vec::new();
        let mut scope = None;
        for token in text.split_whitespace() {
            match token.split_once(':') {
                Some(("scope", value)) if !value.is_empty() => {
                    scope = Some(value.trim_start_matches('@').to_ascii_lowercase());
                }
                Some((qualifier, _)) if !qualifier.is_empty() && !qualifier.contains('/') => {}
                _ => words.push(token),
            }
        }
        Self {
            words: words.join(" "),
            scope,
        }
    }
}

fn number(req: &ArtifactRequest, key: &str) -> Option<usize> {
    req.head
        .query_param(key)
        .and_then(|v| v.trim().parse().ok())
}

fn result_object(package: &PackageName, document: &Packument) -> Option<Value> {
    let latest = document.latest()?;
    let manifest = document.versions().get(latest)?;
    let mut details = serde_json::Map::new();
    details.insert("name".to_owned(), Value::String(package.full()));
    details.insert(
        "scope".to_owned(),
        Value::String(package.scope().unwrap_or("unscoped").to_owned()),
    );
    details.insert("version".to_owned(), Value::String(latest.to_string()));
    for field in ["description", "keywords"] {
        if let Some(value) = document.extra().get(field) {
            details.insert(field.to_owned(), value.clone());
        }
    }
    if let Some(date) = document.time().get(&latest.to_string()) {
        details.insert("date".to_owned(), Value::String(date.clone()));
    }
    if let Some(publisher) = manifest
        .get("_npmUser")
        .and_then(|u| u.get("name"))
        .and_then(Value::as_str)
    {
        details.insert("publisher".to_owned(), json!({ "username": publisher }));
    }
    details.insert("links".to_owned(), json!({}));
    Some(json!({
        "package": Value::Object(details),
        "score": { "final": 1.0, "detail": { "quality": 1.0, "popularity": 1.0, "maintenance": 1.0 } },
        "searchScore": 1.0,
    }))
}

impl NpmHandler {
    /// Search over a hosted repository.
    pub(crate) async fn hosted_search(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
    ) -> Result<Response<Body>, FormatError> {
        let text = SearchText::parse(&req.head.query_param("text").unwrap_or_default());
        let size = number(req, "size")
            .unwrap_or(20)
            .clamp(1, self.config.search_max_results.max(1));
        let from = number(req, "from").unwrap_or(0);
        let filter = ComponentFilter {
            repository_id: Some(cx.repository().id),
            group_id: text.scope.clone(),
            query: (!text.words.is_empty()).then(|| text.words.clone()),
            ..ComponentFilter::default()
        };
        let names = self.search_names(cx, &filter).await?;
        let total = names.len();
        let mut objects = Vec::with_capacity(size);
        for package in names.iter().skip(from).take(size) {
            let current = DocStore::new(cx, package)
                .current(&self.blocking, self.config.max_packument_bytes)
                .await?;
            if let Some((document, _)) = current
                && let Some(object) = result_object(package, &document)
            {
                objects.push(object);
            }
        }
        let body = json!({ "objects": objects, "total": total, "time": now_text() });
        Ok(respond::for_method(
            req.method(),
            respond::json(StatusCode::OK, &body),
        ))
    }

    /// Distinct package names of the matching components, in component order, until the scan
    /// limit is reached. Reading on past the requested page is what makes `total` honest for any
    /// repository the limit covers.
    async fn search_names(
        &self,
        cx: &FormatContext,
        filter: &ComponentFilter,
    ) -> Result<Vec<PackageName>, FormatError> {
        let mut names = Vec::new();
        let mut seen = HashSet::new();
        let mut scanned = 0usize;
        let mut after = None;
        loop {
            let page = cx
                .ports()
                .components
                .list(
                    filter,
                    &PageRequest {
                        limit: SCAN_PAGE,
                        after: after.clone(),
                    },
                )
                .await?;
            for component in &page.items {
                scanned += 1;
                let full = match &component.group_id {
                    Some(scope) => format!("@{scope}/{}", component.name),
                    None => component.name.clone(),
                };
                if seen.insert(full.clone())
                    && let Ok(package) = PackageName::parse(&full)
                {
                    names.push(package);
                }
            }
            let enough = scanned >= self.config.search_scan_limit;
            match page.next {
                Some(next) if !enough => after = Some(next),
                _ => return Ok(names),
            }
        }
    }
}

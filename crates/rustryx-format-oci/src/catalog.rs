//! `GET /v2/_catalog`: the images of every repository the caller may browse.
//!
//! The catalog is the one endpoint of the registry API that is not addressed to a repository,
//! so it cannot go through `FormatEngine::serve`; the server mounts [`catalog_response`] for
//! `/v2/_catalog`. (`GET /v2/<repo>/_catalog` is the same listing limited to one repository and
//! does go through the engine.)
//!
//! Rules:
//!
//! * a repository the caller cannot see contributes nothing and leaves no trace: its name is
//!   not in the answer and the answer does not change shape (`must_not_list_repositories_the_caller_cannot_browse`);
//! * inside a visible repository each image is asked again (`Browse` on that image), because a
//!   content selector can allow some images of a repository and not others;
//! * a group lists the union of its members' images under its own name, for the images that
//!   are browsable in the group AND in at least one member;
//! * anonymous callers only see repositories that carry the anonymous flag; the global switch
//!   is part of `RbacChecker::check`;
//! * names are sorted and paged with `n` and `last` like every listing, with a `Link`.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum_core::body::Body;
use http::{Response, StatusCode};
use rustryx_core::identity::Principal;
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_core::ports::{ComponentRepo, RbacChecker, RepositoryRepo};
use rustryx_format_core::FormatError;
use serde_json::json;

use crate::config::OciConfig;
use crate::error::OciError;
use crate::headers::json_response;
use crate::listing::{image_browsable, visible_images};
use crate::parse::link::{PageParams, next_link};
use crate::parse::name::RepositoryName;

/// Deepest group nesting followed, and most members read per group.
const MAX_GROUP_DEPTH: usize = 4;
const MAX_GROUP_MEMBERS: usize = 64;

/// One page of the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPage {
    /// The image names (`<repo>/<image>`) of the page.
    pub repositories: Vec<String>,
    /// True when more names follow the page.
    pub more: bool,
}

/// The catalog over the repository, component and authorization ports.
#[derive(Clone)]
pub struct CatalogService {
    repositories: Arc<dyn RepositoryRepo>,
    components: Arc<dyn ComponentRepo>,
    rbac: Arc<dyn RbacChecker>,
    config: OciConfig,
}

impl CatalogService {
    /// Builds the service.
    #[must_use]
    pub fn new(
        repositories: Arc<dyn RepositoryRepo>,
        components: Arc<dyn ComponentRepo>,
        rbac: Arc<dyn RbacChecker>,
        config: OciConfig,
    ) -> Self {
        Self {
            repositories,
            components,
            rbac,
            config,
        }
    }

    /// The page of the catalog `principal` may see.
    ///
    /// # Errors
    /// `UNAVAILABLE` when the stores or authorization cannot answer, or a repository has more
    /// manifests than the scan cap.
    pub async fn list(
        &self,
        principal: &Principal,
        params: &PageParams,
    ) -> Result<CatalogPage, OciError> {
        let all = self.repositories.list().await.map_err(FormatError::from)?;
        let mut names = BTreeSet::new();
        for repository in &all {
            if !is_registry(repository) || !self.visible(principal, repository).await? {
                continue;
            }
            for image in self.images_of(principal, repository, &all, 0).await? {
                names.insert(format!("{}/{image}", repository.name));
            }
        }
        let sorted: Vec<String> = names.into_iter().collect();
        let (page, more) = params.apply(&sorted);
        Ok(CatalogPage {
            repositories: page,
            more,
        })
    }

    /// True when `principal` may see that `repository` exists.
    async fn visible(
        &self,
        principal: &Principal,
        repository: &Repository,
    ) -> Result<bool, OciError> {
        if principal.is_anonymous() && !repository.allow_anonymous {
            return Ok(false);
        }
        match self
            .rbac
            .can_see_repository(principal, repository.name.as_str(), repository.format)
            .await
        {
            Ok(visible) => Ok(visible),
            Err(error) => {
                tracing::warn!(%error, repository = %repository.name, "catalog visibility failed closed");
                Err(FormatError::Unavailable {
                    reason: "authorization is temporarily unavailable",
                    retry_after: Some(std::time::Duration::from_secs(1)),
                }
                .into())
            }
        }
    }

    /// The browsable images of a repository; for a group the union over its members.
    async fn images_of(
        &self,
        principal: &Principal,
        repository: &Repository,
        all: &[Repository],
        depth: usize,
    ) -> Result<BTreeSet<String>, OciError> {
        if repository.repo_type != RepoType::Group {
            let images = visible_images(
                self.components.as_ref(),
                self.rbac.as_ref(),
                principal,
                repository,
                self.config.catalog_scan_cap,
            )
            .await?;
            return Ok(images.into_iter().collect());
        }
        let mut union = BTreeSet::new();
        if depth >= MAX_GROUP_DEPTH {
            return Ok(union);
        }
        let members = repository
            .group
            .as_ref()
            .map(|group| group.members.as_slice());
        for member_name in members.unwrap_or_default().iter().take(MAX_GROUP_MEMBERS) {
            let Some(member) = all.iter().find(|r| r.name == *member_name) else {
                continue;
            };
            if member.id == repository.id || !is_registry(member) || !member.online {
                continue;
            }
            if member.repo_type == RepoType::Group {
                let nested = Box::pin(self.images_of(principal, member, all, depth + 1)).await?;
                union.extend(nested);
            } else {
                union.extend(
                    visible_images(
                        self.components.as_ref(),
                        self.rbac.as_ref(),
                        principal,
                        member,
                        self.config.catalog_scan_cap,
                    )
                    .await?,
                );
            }
        }
        // Browsable in the group itself too: the group's rights are checked, not only the
        // members'.
        let mut visible = BTreeSet::new();
        for image in union {
            let Ok(name) = RepositoryName::parse(&image) else {
                continue;
            };
            if image_browsable(self.rbac.as_ref(), principal, repository, &name).await? {
                visible.insert(image);
            }
        }
        Ok(visible)
    }
}

fn is_registry(repository: &Repository) -> bool {
    matches!(repository.format, RepoFormat::Docker | RepoFormat::Oci)
}

/// The response of `GET /v2/_catalog` for `principal`: `{"repositories": [...]}` with a `Link`
/// header when more follow. 400 for bad `n` or `last`, 503 when the stores cannot answer.
pub async fn catalog_response(
    service: &CatalogService,
    principal: &Principal,
    query: Option<&str>,
) -> Response<Body> {
    let params = match PageParams::from_query(query) {
        Ok(params) => params,
        Err(error) => {
            return OciError::unsupported(StatusCode::BAD_REQUEST, error.to_string())
                .into_response();
        }
    };
    match service.list(principal, &params).await {
        Ok(page) => {
            let mut response = json_response(
                StatusCode::OK,
                "application/json",
                &json!({ "repositories": page.repositories }),
            );
            if page.more
                && let Some(last) = page.repositories.last()
            {
                let link = next_link("/v2/_catalog", params.n, last, &[]);
                crate::headers::set_text(response.headers_mut(), http::header::LINK, &link);
            }
            response
        }
        Err(error) => error.into_response(),
    }
}

#[cfg(test)]
mod tests;

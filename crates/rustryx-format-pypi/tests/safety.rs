//! Safety properties that need the engine: an anonymous upload is refused before the
//! handler reads a byte. The scripted checker allows every call, so a pass is the
//! engine's anonymous gate (the repository flag is off), not a deny from RBAC.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests: a panic is the failure signal"
)]

use std::sync::Arc;

use async_trait::async_trait;
use axum_core::body::Body;
use http::{Method, StatusCode, header};
use rustryx_core::error::{AuthError, BlobError};
use rustryx_core::identity::{Action, Principal};
use rustryx_core::ids::BlobStoreId;
use rustryx_core::model::{RepoFormat, RepoType};
use rustryx_core::ports::{BlobStore, Decision, RbacChecker, ResourceRef};
use rustryx_format_core::testing::FakeUpstream;
use rustryx_format_core::{BlobStoreRegistry, FormatEngine, FormatRegistry, Ports, RequestHead};
use rustryx_format_pypi::PypiHandler;
use rustryx_testkit::{FixedClock, MemAuditSink, MemBlobStore, MemStore, fixtures};
use time::OffsetDateTime;
use url::Url;

/// Allows every check. Anonymous refusal must not depend on this.
struct AllowAll;

#[async_trait]
impl RbacChecker for AllowAll {
    async fn check(
        &self,
        _principal: &Principal,
        _action: Action,
        _resource: &ResourceRef,
    ) -> Result<Decision, AuthError> {
        Ok(Decision::Allow)
    }

    async fn can_see_repository(
        &self,
        _principal: &Principal,
        _repository: &str,
        _format: RepoFormat,
    ) -> Result<bool, AuthError> {
        Ok(true)
    }
}

struct OneStore(Arc<dyn BlobStore>);

#[async_trait]
impl BlobStoreRegistry for OneStore {
    async fn store_by_id(&self, _id: BlobStoreId) -> Result<Arc<dyn BlobStore>, BlobError> {
        Ok(Arc::clone(&self.0))
    }
}

fn engine() -> FormatEngine {
    let mut registry = FormatRegistry::new();
    registry
        .register(Arc::new(PypiHandler::new()))
        .expect("pypi handler registers");
    let blobs = MemBlobStore::new();
    let clock = Arc::new(FixedClock::new(
        OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("timestamp"),
    ));
    let store = MemStore::with_builtin_roles()
        .with_clock(clock.clone())
        .with_default_blob_store();
    let ports = Ports::from_store(
        Arc::new(store),
        Arc::new(AllowAll),
        Arc::new(MemAuditSink::new()),
        clock,
        Arc::new(OneStore(Arc::new(blobs))),
    );
    FormatEngine::builder(ports, registry)
        .upstream_client(FakeUpstream::new())
        .build()
        .expect("engine builds")
}

#[tokio::test]
async fn must_not_accept_an_anonymous_upload_without_the_repository_flag() {
    let repository = fixtures::repository("pypi", RepoFormat::Pypi, RepoType::Hosted);
    assert!(
        !repository.allow_anonymous,
        "the fixture must stay closed; this test is the anonymous gate"
    );
    let head = RequestHead::new(
        Method::POST,
        "",
        Principal::anonymous(),
        Url::parse("https://r.test").expect("base url"),
    );
    let response = engine()
        .serve(Arc::new(repository), head, Body::from("not a form"))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let challenge = response
        .headers()
        .get(header::WWW_AUTHENTICATE)
        .expect("a 401 carries the challenge pip and twine need")
        .to_str()
        .expect("challenge is ascii");
    assert!(challenge.starts_with("Basic "), "{challenge}");
}

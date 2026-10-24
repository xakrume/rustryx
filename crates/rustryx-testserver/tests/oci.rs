//! The OCI entry points of the harness: the `/v2/` ping, the token endpoint and the mapping of
//! `/v2/<repository>/<image>/...` onto a repository.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests: a panic is the failure signal"
)]
#![allow(
    clippy::too_many_lines,
    reason = "test scenarios read best as straight-line code"
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{client, get, header_text};
use reqwest::StatusCode;
use rustryx_core::model::RepoFormat;
use rustryx_testserver::{NewTestRepo, RawLikeHandler, TestServer, TestServerBuilder};
use serde_json::Value;

/// A form-urlencoded body: reqwest is built without its `form` feature in this workspace.
fn form(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

fn docker_builder() -> TestServerBuilder {
    TestServer::builder().register(Arc::new(RawLikeHandler::for_format(RepoFormat::Docker)))
}

async fn token_for(server: &TestServer, authorization: Option<&str>) -> Value {
    let mut request = client().get(format!(
        "{}/v2/token?service=rustryx&scope=repository:docker-hosted/app:pull,push",
        server.base_url()
    ));
    if let Some(value) = authorization {
        request = request.header("authorization", value);
    }
    let response = request.send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn ping_always_challenges_a_client_without_a_valid_credential() {
    let server = docker_builder().anonymous_enabled(true).start().await;
    let http = client();
    let ping = get(&http, &format!("{}/v2/", server.base_url()), None).await;
    assert_eq!(ping.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        header_text(&ping, "www-authenticate").unwrap(),
        format!(
            r#"Bearer realm="{}/v2/token",service="rustryx""#,
            server.base_url()
        )
    );
    assert_eq!(
        header_text(&ping, "docker-distribution-api-version").as_deref(),
        Some("registry/2.0")
    );
    let body: Value = ping.json().await.unwrap();
    assert_eq!(body["errors"][0]["code"], "UNAUTHORIZED");

    // `/v2` without the slash is the same endpoint, and a wrong credential is challenged too.
    let bare = get(
        &http,
        &format!("{}/v2", server.base_url()),
        Some("Bearer v2t_wrong"),
    )
    .await;
    assert_eq!(bare.status(), StatusCode::UNAUTHORIZED);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn docker_login_flow_issues_a_token_that_the_ping_accepts() {
    let server = docker_builder().start().await;
    let alice = server.add_user("alice", &["nx-developer"]).await;
    let http = client();

    // What `docker login` does: follow the challenge to the token endpoint with Basic
    // credentials, then verify the token with `GET /v2/`.
    let issued = token_for(&server, Some(&alice.basic_header())).await;
    let token = issued["token"].as_str().unwrap();
    assert_eq!(issued["access_token"].as_str().unwrap(), token);
    assert!(token.starts_with("v2t_"));
    assert!(issued["expires_in"].as_u64().unwrap() >= 1);
    assert!(issued["issued_at"].as_str().unwrap().contains('T'));
    let ping = get(
        &http,
        &format!("{}/v2/", server.base_url()),
        Some(&format!("Bearer {token}")),
    )
    .await;
    assert_eq!(ping.status(), StatusCode::OK);
    assert_eq!(ping.text().await.unwrap(), "{}");

    // The API token in the password position works too (robot style login).
    let by_token = token_for(&server, Some(&alice.token_basic_header())).await;
    assert!(by_token["token"].as_str().unwrap().starts_with("v2t_"));

    let wrong = get(
        &http,
        &format!("{}/v2/token", server.base_url()),
        Some("Basic YWxpY2U6d3Jvbmc="),
    )
    .await;
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pushed_content_is_reachable_through_both_doors_with_a_registry_token() {
    let server = docker_builder().start().await;
    server
        .add_repository(NewTestRepo::hosted("docker-hosted", RepoFormat::Docker))
        .await;
    let alice = server.add_user("alice", &["nx-developer"]).await;
    let http = client();
    let token = token_for(&server, Some(&alice.basic_header())).await["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let bearer = format!("Bearer {token}");

    // `/v2/<repository>/<image>/...` and `/repository/<repository>/v2/<image>/...` are one
    // namespace: the format sees `v2/app/manifests/1` either way.
    let pushed = http
        .put(format!(
            "{}/v2/docker-hosted/app/manifests/1",
            server.base_url()
        ))
        .header("authorization", &bearer)
        .body("manifest bytes")
        .send()
        .await
        .unwrap();
    assert_eq!(pushed.status(), StatusCode::CREATED);
    let via_v2 = get(
        &http,
        &format!("{}/v2/docker-hosted/app/manifests/1", server.base_url()),
        Some(&bearer),
    )
    .await;
    assert_eq!(via_v2.text().await.unwrap(), "manifest bytes");
    let via_repository = get(
        &http,
        &format!(
            "{}/v2/app/manifests/1",
            server.repository_url("docker-hosted")
        ),
        Some(&bearer),
    )
    .await;
    assert_eq!(via_repository.text().await.unwrap(), "manifest bytes");

    let unknown = get(
        &http,
        &format!("{}/v2/no-such-repo/app/manifests/1", server.base_url()),
        Some(&bearer),
    )
    .await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    let body: Value = unknown.json().await.unwrap();
    assert_eq!(body["errors"][0]["code"], "NAME_UNKNOWN");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn anonymous_pull_goes_through_an_anonymous_token_and_needs_both_gates() {
    let server = docker_builder().start().await;
    server
        .add_repository(
            NewTestRepo::hosted("docker-hosted", RepoFormat::Docker).allow_anonymous(true),
        )
        .await;
    let alice = server.add_user("alice", &["nx-developer"]).await;
    let http = client();
    let pull = format!(
        "{}/v2/docker-hosted/team/app/manifests/1",
        server.base_url()
    );
    http.put(&pull)
        .header("authorization", alice.bearer_header())
        .body("m")
        .send()
        .await
        .unwrap();

    // No credentials at all: the challenge names the scope the client needs.
    let challenged = get(&http, &pull, None).await;
    assert_eq!(challenged.status(), StatusCode::UNAUTHORIZED);
    let challenge = header_text(&challenged, "www-authenticate").unwrap();
    assert!(
        challenge.contains(r#"scope="repository:docker-hosted/team/app:pull""#),
        "{challenge}"
    );
    let post_challenge = http
        .post(format!(
            "{}/v2/docker-hosted/team/app/blobs/uploads",
            server.base_url()
        ))
        .send()
        .await
        .unwrap();
    assert!(
        header_text(&post_challenge, "www-authenticate")
            .unwrap()
            .contains("pull,push")
    );

    // An anonymous token is issued, but it is worth nothing while the global switch is off.
    let anonymous = token_for(&server, None).await["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let bearer = format!("Bearer {anonymous}");
    assert_eq!(
        get(&http, &pull, Some(&bearer)).await.status(),
        StatusCode::UNAUTHORIZED
    );
    server.set_anonymous_enabled(true);
    assert_eq!(
        get(&http, &pull, Some(&bearer)).await.status(),
        StatusCode::OK
    );
    // The registry accepts the anonymous token at the ping, as `docker pull` expects.
    let ping = get(&http, &format!("{}/v2/", server.base_url()), Some(&bearer)).await;
    assert_eq!(ping.status(), StatusCode::OK);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn oauth_password_grant_and_the_endpoints_error_cases() {
    let server = docker_builder().start().await;
    let alice = server.add_user("alice", &["nx-developer"]).await;
    let http = client();
    let url = format!("{}/v2/token", server.base_url());

    let granted = http
        .post(&url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form(&[
            ("grant_type", "password"),
            ("username", "alice"),
            ("password", alice.password.as_str()),
            ("service", "rustryx"),
            ("scope", "repository:docker-hosted/app:pull"),
            ("client_id", "containerd"),
        ]))
        .send()
        .await
        .unwrap();
    assert_eq!(granted.status(), StatusCode::OK);
    let body: Value = granted.json().await.unwrap();
    assert!(body["access_token"].as_str().unwrap().starts_with("v2t_"));
    assert_eq!(body["scope"], "repository:docker-hosted/app:pull");

    let bad_password = http
        .post(&url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form(&[
            ("grant_type", "password"),
            ("username", "alice"),
            ("password", "wrong"),
        ]))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_password.status(), StatusCode::UNAUTHORIZED);
    let refresh = http
        .post(&url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", "x"),
        ]))
        .send()
        .await
        .unwrap();
    assert_eq!(refresh.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        http.delete(&url).send().await.unwrap().status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn registry_tokens_are_short_lived() {
    let server = docker_builder()
        .oci_token_ttl(Duration::from_millis(150))
        .start()
        .await;
    let alice = server.add_user("alice", &["nx-developer"]).await;
    let token = token_for(&server, Some(&alice.basic_header())).await["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let ping_url = format!("{}/v2/", server.base_url());
    let http = client();
    let bearer = format!("Bearer {token}");
    assert_eq!(
        get(&http, &ping_url, Some(&bearer)).await.status(),
        StatusCode::OK
    );
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        get(&http, &ping_url, Some(&bearer)).await.status(),
        StatusCode::UNAUTHORIZED
    );
    server.shutdown().await;
}

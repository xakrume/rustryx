//! `presign_get`: lifetime caps, signed overrides, and what must never happen.

use std::sync::Mutex;
use std::time::Duration;

use object_store::signer::{Method, SignedUrlOptions, Signer};
use object_store::{ObjectStore, Result as OsResult};
use rustryx_core::ports::{BlobStore, PresignOptions};
use url::Url;

use super::*;
use crate::config::{MAX_PRESIGN_TTL_SECS, PresignSettings};

/// A signer that records what it was asked for and returns a recognisable URL.
#[derive(Debug, Default)]
struct RecordingSigner {
    calls: Mutex<Vec<Call>>,
    fail: bool,
}

#[derive(Debug, Clone)]
struct Call {
    method: Method,
    path: String,
    expires_in: Duration,
    query: Vec<(String, String)>,
}

#[async_trait]
impl Signer for RecordingSigner {
    async fn signed_url(
        &self,
        method: Method,
        path: &ObjectPath,
        expires_in: Duration,
    ) -> OsResult<Url> {
        self.signed_url_opts(method, path, expires_in, &SignedUrlOptions::default())
            .await
    }

    async fn signed_url_opts(
        &self,
        method: Method,
        path: &ObjectPath,
        expires_in: Duration,
        options: &SignedUrlOptions,
    ) -> OsResult<Url> {
        self.calls.lock().unwrap().push(Call {
            method,
            path: path.to_string(),
            expires_in,
            query: options.extra_query.clone(),
        });
        if self.fail {
            return Err(injected("signer failure"));
        }
        Ok(Url::parse(&format!(
            "https://signed.example.test/{path}?X-Amz-Signature=canary-signature"
        ))
        .unwrap())
    }
}

fn enabled(ttl_secs: u64) -> PresignSettings {
    PresignSettings {
        enabled: true,
        ttl: Duration::from_secs(ttl_secs),
    }
}

fn signed_store(
    prefix: &str,
    presign: PresignSettings,
    signer: Arc<RecordingSigner>,
) -> S3BlobStore {
    let target = Arc::new(InMemory::new());
    let fake = Arc::new(FakeMultipart {
        target: Arc::clone(&target),
        state: Mutex::default(),
    });
    S3BlobStore::from_backends_signed(
        target as Arc<dyn ObjectStore>,
        fake as Arc<dyn MultipartStore>,
        Some(signer as Arc<dyn Signer>),
        prefix,
        S3Settings {
            presign,
            ..settings(2)
        },
    )
    .unwrap()
}

async fn lifetime_for(presign: PresignSettings, asked: Duration) -> Duration {
    let signer = Arc::new(RecordingSigner::default());
    let store = signed_store("", presign, Arc::clone(&signer));
    store
        .presign_get(&key("k"), asked, &PresignOptions::default())
        .await
        .unwrap()
        .unwrap();
    let calls = signer.calls.lock().unwrap();
    calls[0].expires_in
}

#[tokio::test]
async fn a_download_url_is_a_signed_get_of_the_sharded_object_name() {
    let signer = Arc::new(RecordingSigner::default());
    let store = signed_store("tenant", enabled(300), Arc::clone(&signer));
    let before = OffsetDateTime::now_utc();
    let presigned = store
        .presign_get(
            &key("artifact-1"),
            Duration::from_secs(120),
            &PresignOptions::default(),
        )
        .await
        .unwrap()
        .unwrap();
    let call = signer.calls.lock().unwrap()[0].clone();
    assert_eq!(call.method, Method::GET);
    assert_eq!(
        call.path,
        format!("tenant/{}", crate::key::relative_location("artifact-1"))
    );
    assert_eq!(call.expires_in, Duration::from_secs(120));
    assert!(call.query.is_empty(), "no override was asked for");
    assert!(presigned.url.expose().contains("signed.example.test"));
    let expiry = presigned.expires_at - before;
    assert!(
        (119..=122).contains(&expiry.whole_seconds()),
        "expires_at follows the lifetime: {expiry}"
    );
}

#[tokio::test]
async fn the_overrides_are_signed_not_appended() {
    let signer = Arc::new(RecordingSigner::default());
    let store = signed_store("", enabled(300), Arc::clone(&signer));
    let options = PresignOptions {
        response_content_type: Some("application/gzip".into()),
        response_content_disposition: Some("attachment; filename=\"a.tgz\"".into()),
    };
    store
        .presign_get(&key("k"), Duration::from_secs(60), &options)
        .await
        .unwrap();
    let call = signer.calls.lock().unwrap()[0].clone();
    assert_eq!(
        call.query,
        vec![
            (
                "response-content-type".to_owned(),
                "application/gzip".to_owned()
            ),
            (
                "response-content-disposition".to_owned(),
                "attachment; filename=\"a.tgz\"".to_owned()
            ),
        ]
    );
}

#[tokio::test]
async fn must_not_sign_beyond_the_max_ttl() {
    let max = Duration::from_secs(u64::from(MAX_PRESIGN_TTL_SECS));
    // The caller asks for a day.
    let day = Duration::from_hours(24);
    assert_eq!(
        lifetime_for(enabled(300), day).await,
        Duration::from_secs(300)
    );
    assert_eq!(lifetime_for(enabled(900), day).await, max);
    // A configuration built in code with a longer TTL than the contract is capped as well.
    assert_eq!(lifetime_for(enabled(86_400), day).await, max);
    assert_eq!(
        lifetime_for(enabled(u64::MAX / 4), Duration::MAX).await,
        max
    );
    // A shorter ask is honoured, and zero is raised to one second, never signed as expired.
    assert_eq!(
        lifetime_for(enabled(300), Duration::from_secs(10)).await,
        Duration::from_secs(10)
    );
    assert_eq!(
        lifetime_for(enabled(300), Duration::ZERO).await,
        Duration::from_secs(1)
    );
    // A configured TTL of zero cannot make the minimum exceed the maximum.
    assert_eq!(
        lifetime_for(enabled(0), Duration::from_secs(50)).await,
        Duration::from_secs(1)
    );
}

#[tokio::test]
async fn a_store_with_presign_off_answers_none_and_never_calls_the_signer() {
    let signer = Arc::new(RecordingSigner::default());
    let store = signed_store("", PresignSettings::default(), Arc::clone(&signer));
    assert!(!store.capabilities().can_presign);
    let outcome = store
        .presign_get(
            &key("k"),
            Duration::from_secs(60),
            &PresignOptions::default(),
        )
        .await
        .unwrap();
    assert!(outcome.is_none());
    assert!(signer.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_backend_without_a_signer_cannot_presign_even_when_enabled() {
    let fx = fixture_with(
        "",
        S3Settings {
            presign: enabled(300),
            ..settings(1)
        },
    );
    assert!(!fx.store.capabilities().can_presign);
    assert!(
        fx.store
            .presign_get(
                &key("k"),
                Duration::from_secs(60),
                &PresignOptions::default()
            )
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn capabilities_say_what_this_store_can_do() {
    let signed = signed_store("", enabled(300), Arc::default());
    let caps = signed.capabilities();
    assert!(caps.can_presign && caps.lists_keys);
    assert!(
        !caps.lists_staging,
        "S3 cannot list stale uploads: an empty answer means cannot tell"
    );
    assert!(!caps.resumable);
    assert!(signed.resumable().is_none());
}

#[tokio::test]
async fn an_invalid_key_is_refused_before_any_signing() {
    let signer = Arc::new(RecordingSigner::default());
    let store = signed_store("", enabled(300), Arc::clone(&signer));
    let outcome = store
        .presign_get(
            &key("../x"),
            Duration::from_secs(60),
            &PresignOptions::default(),
        )
        .await;
    assert!(matches!(outcome, Err(BlobError::InvalidKey(_))));
    assert!(signer.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn must_not_sign_a_response_header_with_control_characters() {
    let signer = Arc::new(RecordingSigner::default());
    let store = signed_store("", enabled(300), Arc::clone(&signer));
    for bad in [
        "text/plain\r\nSet-Cookie: x=1",
        "text/plain\n",
        "a\0b",
        &"x".repeat(2000),
    ] {
        for options in [
            PresignOptions {
                response_content_type: Some(bad.to_owned()),
                response_content_disposition: None,
            },
            PresignOptions {
                response_content_type: None,
                response_content_disposition: Some(bad.to_owned()),
            },
        ] {
            let outcome = store
                .presign_get(&key("k"), Duration::from_secs(60), &options)
                .await;
            assert!(matches!(outcome, Err(BlobError::Unsupported(_))), "{bad:?}");
        }
    }
    assert!(signer.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_failing_signer_is_an_error_not_a_missing_url() {
    let signer = Arc::new(RecordingSigner {
        fail: true,
        ..RecordingSigner::default()
    });
    let store = signed_store("", enabled(300), signer);
    let outcome = store
        .presign_get(
            &key("k"),
            Duration::from_secs(60),
            &PresignOptions::default(),
        )
        .await;
    assert!(
        matches!(outcome, Err(BlobError::Unavailable(_))),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn must_not_leak_a_presigned_url_through_debug_or_the_store() {
    let signer = Arc::new(RecordingSigner::default());
    let store = signed_store("", enabled(300), signer);
    let presigned = store
        .presign_get(
            &key("k"),
            Duration::from_secs(60),
            &PresignOptions::default(),
        )
        .await
        .unwrap()
        .unwrap();
    let shown = format!("{presigned:?} {store:?}");
    assert!(!shown.contains("canary-signature"), "{shown}");
    assert!(!shown.contains("X-Amz"), "{shown}");
    assert!(presigned.url.expose().contains("canary-signature"));
}

#[tokio::test]
async fn a_real_s3_signer_signs_locally_without_network() {
    let config: S3Config = crate::config::parse_config(
        "s3",
        &serde_json::json!({
            "bucket": "artifacts",
            "endpoint": "http://127.0.0.1:1",
            "allow_http": true,
            "prefix": "p",
            "presign": {"enabled": true, "ttl_secs": 900},
        }),
    )
    .unwrap();
    let creds = S3Credentials {
        access_key_id: Secret::new("AKIDEXAMPLE"),
        secret_access_key: Secret::new("secret-example"),
        session_token: None,
    };
    let store = S3BlobStore::new("signing-unit", &config, Some(&creds)).unwrap();
    assert!(store.capabilities().can_presign);
    let options = PresignOptions {
        response_content_type: Some("application/octet-stream".into()),
        response_content_disposition: Some("attachment; filename=\"x.bin\"".into()),
    };
    let presigned = store
        .presign_get(&key("real"), Duration::from_hours(24), &options)
        .await
        .unwrap()
        .unwrap();
    let url = Url::parse(presigned.url.expose()).unwrap();
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["X-Amz-Expires"], "900", "capped at the maximum");
    assert_eq!(query["X-Amz-Algorithm"], "AWS4-HMAC-SHA256");
    assert_eq!(query["response-content-type"], "application/octet-stream");
    assert_eq!(
        query["response-content-disposition"],
        "attachment; filename=\"x.bin\""
    );
    assert!(query["X-Amz-SignedHeaders"].contains("host"));
    assert_ne!(query["X-Amz-Signature"], "");
    assert!(
        url.path().ends_with(&crate::key::relative_location("real")),
        "{}",
        url.path()
    );
    // The secret key itself never appears in the URL.
    assert!(!presigned.url.expose().contains("secret-example"));
}

#[tokio::test]
async fn presign_results_are_counted_under_their_own_op() {
    use crate::metrics::capture::capture;
    let signer = Arc::new(RecordingSigner::default());
    let mut settings = settings(1);
    settings.store_name = "presign-metrics".into();
    settings.presign = enabled(300);
    let fx = fixture_with("", settings.clone());
    let store = {
        let target = Arc::clone(&fx.target);
        S3BlobStore::from_backends_signed(
            target as Arc<dyn ObjectStore>,
            Arc::clone(&fx.fake) as Arc<dyn MultipartStore>,
            Some(signer as Arc<dyn Signer>),
            "",
            settings,
        )
        .unwrap()
    };
    store
        .presign_get(
            &key("k"),
            Duration::from_secs(10),
            &PresignOptions::default(),
        )
        .await
        .unwrap();
    assert!(
        store
            .presign_get(
                &key("../bad"),
                Duration::from_secs(10),
                &PresignOptions::default()
            )
            .await
            .is_err()
    );
    let count = |result: &str| {
        capture().counter(
            "blob_store_ops_total",
            &[
                ("store", "presign-metrics"),
                ("op", "presign"),
                ("result", result),
            ],
        )
    };
    assert_eq!(count("ok"), 1);
    assert_eq!(count("rejected"), 1);
}

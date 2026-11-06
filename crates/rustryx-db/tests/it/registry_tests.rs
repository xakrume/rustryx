//! Registries: sealed credentials, versioned updates and proxy dependents.

use rustryx_core::StoreError;
use rustryx_core::ids::RegistryId;
use rustryx_core::model::{ProxyConfig, RepoType, UpstreamAuth};
use rustryx_core::ports::{RegistryRepo, RepositoryRepo};
use rustryx_core::registry::{Registry, RegistryAuth, RegistryProvider, RegistryTest};
use rustryx_core::secret::Secret;
use time::OffsetDateTime;

use crate::fixtures::{hosted_repository, seed_repository, unique};
use crate::support::{TestDb, test_sealer};

fn registry(name: &str) -> Registry {
    let mut registry = Registry::new(
        RegistryId::generate(),
        name,
        RegistryProvider::Oci,
        "https://harbor.example",
        OffsetDateTime::now_utc(),
    );
    registry.auth = RegistryAuth::Basic {
        username: "pull".to_owned(),
        password: Secret::new("s3cret"),
    };
    registry
}

#[tokio::test(flavor = "multi_thread")]
async fn a_registry_seals_its_password_and_rewrites_bound_proxies() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let repos = rustryx_db::repos::PgRegistryRepo::new(store.pool().clone(), test_sealer());
    let first = registry("harbor");
    repos.create(&first).await.unwrap();
    assert!(matches!(
        repos.create(&registry("harbor")).await,
        Err(StoreError::Conflict(_))
    ));
    let mut bad = registry("broken");
    bad.url = "https://harbor.example/v2".to_owned();
    assert!(matches!(
        repos.create(&bad).await,
        Err(StoreError::Conflict(_))
    ));
    let stored = repos.get(first.id).await.unwrap().unwrap();
    match stored.auth {
        RegistryAuth::Basic {
            ref username,
            ref password,
        } => {
            assert_eq!(username, "pull");
            assert_eq!(password.expose(), "s3cret");
        }
        other => panic!("unexpected auth {other:?}"),
    }
    let sealed: String =
        sqlx::query_scalar("SELECT auth->>'password_sealed' FROM registries WHERE id = $1")
            .bind(first.id.as_uuid())
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_ne!(sealed, "s3cret");

    let (blob_store, _) = seed_repository(&store).await;
    let mut repo = hosted_repository(&unique("proxy"), blob_store.id);
    repo.repo_type = RepoType::Proxy;
    let mut proxy = ProxyConfig::new("https://old.example");
    proxy.auth = Some(UpstreamAuth {
        username: "svc".to_owned(),
        password: Secret::new("old-password"),
    });
    proxy.registry_id = Some(first.id);
    repo.proxy = Some(proxy);
    store.repositories.create(&repo).await.unwrap();
    assert_eq!(
        repos.dependents(first.id).await.unwrap().proxy_repositories,
        vec![repo.id]
    );
    assert!(matches!(
        repos.delete(first.id).await,
        Err(StoreError::Conflict(_))
    ));

    let mut edited = stored;
    edited.url = "https://new.example".to_owned();
    repos.update(&edited, 1).await.unwrap();
    let remote: String =
        sqlx::query_scalar("SELECT proxy_config->>'remote_url' FROM repositories WHERE id = $1")
            .bind(repo.id.as_uuid())
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(remote, "https://new.example");
    assert!(matches!(
        repos.update(&edited, 1).await,
        Err(StoreError::Conflict(_))
    ));

    repos
        .record_test(
            first.id,
            &RegistryTest {
                at: OffsetDateTime::now_utc(),
                ok: true,
                protocol_version: None,
                message: "ok".to_owned(),
            },
        )
        .await
        .unwrap();
    assert!(
        repos
            .get(first.id)
            .await
            .unwrap()
            .unwrap()
            .last_test
            .unwrap()
            .ok
    );

    sqlx::query(
        "UPDATE repositories SET proxy_config = proxy_config - 'registry_id' WHERE id = $1",
    )
    .bind(repo.id.as_uuid())
    .execute(store.pool())
    .await
    .unwrap();
    repos.delete(first.id).await.unwrap();
    assert!(repos.get(first.id).await.unwrap().is_none());
}

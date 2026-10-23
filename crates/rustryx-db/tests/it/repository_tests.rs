use rustryx_core::Secret;
use rustryx_core::StoreError;
use rustryx_core::ids::{BlobStoreId, RepositoryId};
use rustryx_core::model::{GroupConfig, ProxyConfig, RepoFormat, RepoType, UpstreamAuth};
use rustryx_core::names::RepoName;
use rustryx_core::ports::{BlobStoreRepo, RepositoryRepo};

use crate::fixtures::{blob_store, hosted_repository, now, seed_repository, unique};
use crate::support::TestDb;

fn proxy(password: &str) -> ProxyConfig {
    ProxyConfig {
        remote_url: "https://repo.example.org/maven2".to_owned(),
        auth: Some(UpstreamAuth {
            username: "svc".to_owned(),
            password: Secret::new(password),
        }),
        content_max_age_secs: 86_400,
        metadata_max_age_secs: 600,
        negative_cache_ttl_secs: 60,
        max_concurrent_upstream: 8,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn blob_store_crud_and_conflicts() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let def = blob_store("primary");
    store.blob_stores.create(&def).await.unwrap();

    assert_eq!(
        store.blob_stores.get(def.id).await.unwrap().unwrap().name,
        "primary"
    );
    assert_eq!(
        store
            .blob_stores
            .get_by_name("primary")
            .await
            .unwrap()
            .unwrap()
            .id,
        def.id
    );
    assert!(
        store
            .blob_stores
            .get_by_name("nope")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(store.blob_stores.list().await.unwrap().len(), 1);

    let duplicate = blob_store("primary");
    let err = store.blob_stores.create(&duplicate).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(ref m) if m == "blob store name already exists"),
        "{err:?}"
    );

    let mut with_quota = blob_store("quota");
    with_quota.quota_bytes = Some(1 << 40);
    store.blob_stores.create(&with_quota).await.unwrap();
    assert_eq!(
        store
            .blob_stores
            .get(with_quota.id)
            .await
            .unwrap()
            .unwrap()
            .quota_bytes,
        Some(1 << 40)
    );

    store.blob_stores.delete(def.id).await.unwrap();
    assert!(matches!(
        store.blob_stores.delete(def.id).await.unwrap_err(),
        StoreError::NotFound(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn blob_store_in_use_cannot_be_deleted() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (blob_store, _repo) = seed_repository(&store).await;
    let err = store.blob_stores.delete(blob_store.id).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn repository_round_trip_and_listing_order() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (blob_store, mut repo) = seed_repository(&store).await;
    repo.quota_bytes = Some(5_000_000);
    repo.allow_anonymous = true;
    repo.online = false;
    repo.format_config = serde_json::json!({ "layout": "strict", "versionPolicy": "release" });
    repo.cleanup_policy_ids = vec![rustryx_core::ids::CleanupPolicyId::generate()];
    repo.routing_rule_id = Some(rustryx_core::ids::RoutingRuleId::generate());
    store.repositories.update(&repo).await.unwrap();

    let back = store.repositories.get(repo.id).await.unwrap().unwrap();
    assert_eq!(back.name, repo.name);
    assert_eq!(back.quota_bytes, Some(5_000_000));
    assert!(back.allow_anonymous);
    assert!(!back.online);
    assert_eq!(back.format_config, repo.format_config);
    assert_eq!(back.cleanup_policy_ids, repo.cleanup_policy_ids);
    assert_eq!(back.routing_rule_id, repo.routing_rule_id);
    assert_eq!(back.blob_store_id, blob_store.id);
    assert!(
        store
            .repositories
            .get_by_name(&repo.name)
            .await
            .unwrap()
            .is_some()
    );

    for name in ["zeta", "alpha", "mid"] {
        store
            .repositories
            .create(&hosted_repository(name, blob_store.id))
            .await
            .unwrap();
    }
    let names: Vec<String> = store
        .repositories
        .list()
        .await
        .unwrap()
        .iter()
        .map(|r| r.name.to_string())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert_eq!(names.len(), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn repository_name_conflict_and_missing_blob_store() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (blob_store, repo) = seed_repository(&store).await;

    let clash = hosted_repository(repo.name.as_str(), blob_store.id);
    let err = store.repositories.create(&clash).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(ref m) if m == "repository name already exists"));

    let orphan = hosted_repository(&unique("orphan"), BlobStoreId::generate());
    let err = store.repositories.create(&orphan).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn repository_type_must_match_its_config() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (blob_store, _) = seed_repository(&store).await;

    let mut proxy_without_config = hosted_repository(&unique("p"), blob_store.id);
    proxy_without_config.repo_type = RepoType::Proxy;
    let err = store
        .repositories
        .create(&proxy_without_config)
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(ref m) if m == "repository type does not match its configuration"),
        "{err:?}"
    );

    let mut hosted_with_group = hosted_repository(&unique("g"), blob_store.id);
    hosted_with_group.group = Some(GroupConfig::default());
    assert!(matches!(
        store
            .repositories
            .create(&hosted_with_group)
            .await
            .unwrap_err(),
        StoreError::Conflict(_)
    ));

    let mut group = hosted_repository(&unique("grp"), blob_store.id);
    group.repo_type = RepoType::Group;
    group.group = Some(GroupConfig {
        members: vec![RepoName::parse("a").unwrap(), RepoName::parse("b").unwrap()],
    });
    store.repositories.create(&group).await.unwrap();
    let back = store.repositories.get(group.id).await.unwrap().unwrap();
    assert_eq!(back.group.unwrap().members.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn update_refuses_identity_changes_and_reports_missing() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (blob_store, repo) = seed_repository(&store).await;

    let mut renamed = repo.clone();
    renamed.name = RepoName::parse(&unique("renamed")).unwrap();
    assert!(matches!(
        store.repositories.update(&renamed).await.unwrap_err(),
        StoreError::Conflict(_)
    ));

    let mut reformatted = repo.clone();
    reformatted.format = RepoFormat::Npm;
    assert!(matches!(
        store.repositories.update(&reformatted).await.unwrap_err(),
        StoreError::Conflict(_)
    ));

    let mut moved = repo.clone();
    let other = blob_store_with_name(&store, "other").await;
    moved.blob_store_id = other;
    assert!(matches!(
        store.repositories.update(&moved).await.unwrap_err(),
        StoreError::Conflict(_)
    ));
    let _ = blob_store;

    let ghost = hosted_repository(&unique("ghost"), repo.blob_store_id);
    assert!(matches!(
        store.repositories.update(&ghost).await.unwrap_err(),
        StoreError::NotFound(_)
    ));
}

async fn blob_store_with_name(store: &rustryx_db::PgStore, name: &str) -> BlobStoreId {
    let def = blob_store(name);
    store.blob_stores.create(&def).await.unwrap();
    def.id
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_repository_and_missing_delete() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (_, repo) = seed_repository(&store).await;
    store.repositories.delete(repo.id).await.unwrap();
    assert!(store.repositories.get(repo.id).await.unwrap().is_none());
    assert!(matches!(
        store
            .repositories
            .delete(RepositoryId::generate())
            .await
            .unwrap_err(),
        StoreError::NotFound(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn proxy_password_round_trips_and_is_sealed_at_rest() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (blob_store, _) = seed_repository(&store).await;
    let mut repo = hosted_repository(&unique("proxy"), blob_store.id);
    repo.repo_type = RepoType::Proxy;
    repo.proxy = Some(proxy("upstream-canary-pw"));
    store.repositories.create(&repo).await.unwrap();

    let back = store.repositories.get(repo.id).await.unwrap().unwrap();
    let auth = back.proxy.unwrap().auth.unwrap();
    assert_eq!(auth.username, "svc");
    assert_eq!(auth.password.expose(), "upstream-canary-pw");

    let raw: String =
        sqlx::query_scalar("SELECT proxy_config::text FROM repositories WHERE id = $1")
            .bind(repo.id.as_uuid())
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert!(!raw.contains("upstream-canary-pw"), "{raw}");
    assert!(raw.contains("rx1.test."));
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_open_a_sealed_password_copied_to_another_repository() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (blob_store, _) = seed_repository(&store).await;
    let mut victim = hosted_repository(&unique("victim"), blob_store.id);
    victim.repo_type = RepoType::Proxy;
    victim.proxy = Some(proxy("victim-secret"));
    store.repositories.create(&victim).await.unwrap();
    let mut attacker = hosted_repository(&unique("attacker"), blob_store.id);
    attacker.repo_type = RepoType::Proxy;
    attacker.proxy = Some(proxy("attacker-secret"));
    store.repositories.create(&attacker).await.unwrap();

    // Someone with write access to the table copies the victim's envelope to another row.
    sqlx::query("UPDATE repositories SET proxy_config = (SELECT proxy_config FROM repositories WHERE id = $1) WHERE id = $2")
        .bind(victim.id.as_uuid())
        .bind(attacker.id.as_uuid())
        .execute(store.pool())
        .await
        .unwrap();
    let err = store.repositories.get(attacker.id).await.unwrap_err();
    assert!(matches!(err, StoreError::Backend { .. }), "{err:?}");
    assert!(!format!("{err} {err:?}").contains("victim-secret"));
}

#[tokio::test(flavor = "multi_thread")]
async fn reseal_moves_secrets_to_the_current_key() {
    use rustryx_db::repos::PgRepositoryRepo;
    use rustryx_db::{AeadSealer, KeyEntry};
    use std::sync::Arc;

    let db = TestDb::new().await;
    let pool = db.pool().await;
    let old =
        Arc::new(AeadSealer::new(KeyEntry::from_bytes("k1", [1; 32]).unwrap(), vec![]).unwrap());
    let repos_old = PgRepositoryRepo::new(pool.clone(), old);
    let blob_stores = rustryx_db::repos::PgBlobStoreRepo::new(pool.clone());
    let bs = blob_store("s");
    blob_stores.create(&bs).await.unwrap();
    let mut repo = hosted_repository(&unique("rot"), bs.id);
    repo.repo_type = RepoType::Proxy;
    repo.proxy = Some(proxy("rotate-me"));
    repos_old.create(&repo).await.unwrap();

    let rotated = Arc::new(
        AeadSealer::new(
            KeyEntry::from_bytes("k2", [2; 32]).unwrap(),
            vec![KeyEntry::from_bytes("k1", [1; 32]).unwrap()],
        )
        .unwrap(),
    );
    let repos_new = PgRepositoryRepo::new(pool.clone(), rotated);
    assert_eq!(repos_new.reseal_upstream_secrets().await.unwrap(), 1);
    let raw: String =
        sqlx::query_scalar("SELECT proxy_config::text FROM repositories WHERE id = $1")
            .bind(repo.id.as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(raw.contains("rx1.k2."), "{raw}");
    assert_eq!(
        repos_new
            .get(repo.id)
            .await
            .unwrap()
            .unwrap()
            .proxy
            .unwrap()
            .auth
            .unwrap()
            .password
            .expose(),
        "rotate-me"
    );
    // The old key alone can no longer read the row, which is what retiring it means.
    assert!(repos_old.get(repo.id).await.is_err());
    let _ = now();
}

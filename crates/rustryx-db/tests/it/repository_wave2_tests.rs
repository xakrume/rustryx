//! What Core revision 2 added to repositories and blob stores: the project link, the four new
//! proxy fields, `BlobStoreRepo::update`, and the grants of application privileges.

use rustryx_core::StoreError;
use rustryx_core::identity::{Action, Privilege, PrivilegeGrant};
use rustryx_core::ids::{
    BlobStoreId, PrivilegeId, ProjectId, RegistryId, RepositoryId, TrustedCertificateId,
};
use rustryx_core::model::{BlobStoreKind, ProjectRef, ProxyConfig, RepoType};
use rustryx_core::ports::{BlobStoreRepo, PrivilegeRepo, RepositoryRepo};
use rustryx_core::project::{ProjectName, ProjectVisibility};
use uuid::Uuid;

use crate::fixtures::{blob_store, hosted_repository, seed_repository, unique};
use crate::support::{TestDb, listen_rbac, next_payload};

const WAIT: std::time::Duration = std::time::Duration::from_secs(5);

async fn insert_project(store: &rustryx_db::PgStore, name: &str, visibility: &str) -> ProjectRef {
    let id = ProjectId::generate();
    sqlx::query("INSERT INTO projects (id, name, visibility) VALUES ($1, $2, $3)")
        .bind(id.as_uuid())
        .bind(name)
        .bind(visibility)
        .execute(store.pool())
        .await
        .unwrap();
    ProjectRef {
        id,
        name: ProjectName::parse(name).unwrap(),
        visibility: if visibility == "public" {
            ProjectVisibility::Public
        } else {
            ProjectVisibility::Private
        },
    }
}

async fn anonymous_flag(store: &rustryx_db::PgStore, id: RepositoryId) -> bool {
    sqlx::query_scalar("SELECT allow_anonymous FROM repositories WHERE id = $1")
        .bind(id.as_uuid())
        .fetch_one(store.pool())
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repository_reads_back_its_project_with_name_and_visibility() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, _) = seed_repository(&store).await;
    let project = insert_project(&store, "payments", "private").await;

    let mut repo = hosted_repository(&unique("proj-repo"), bs.id);
    repo.project = Some(project.clone());
    store.repositories.create(&repo).await.unwrap();

    let back = store.repositories.get(repo.id).await.unwrap().unwrap();
    assert_eq!(back.project, Some(project.clone()));
    let by_name = store
        .repositories
        .get_by_name(&repo.name)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_name.project, Some(project));
    let listed = store.repositories.list().await.unwrap();
    assert!(
        listed
            .iter()
            .any(|r| r.id == repo.id && r.project.is_some())
    );
    // A repository outside any project has none.
    let (_, plain) = seed_repository(&store).await;
    assert!(
        store
            .repositories
            .get(plain.id)
            .await
            .unwrap()
            .unwrap()
            .project
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn anonymous_access_of_a_project_repository_follows_the_project_visibility() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, _) = seed_repository(&store).await;
    let public = insert_project(&store, "open-source", "public").await;
    let private = insert_project(&store, "internal", "private").await;

    // The caller asks for the opposite on insert: the trigger decides.
    let mut in_public = hosted_repository(&unique("pub"), bs.id);
    in_public.project = Some(public.clone());
    in_public.allow_anonymous = false;
    store.repositories.create(&in_public).await.unwrap();
    let mut in_private = hosted_repository(&unique("priv"), bs.id);
    in_private.project = Some(private.clone());
    in_private.allow_anonymous = true;
    store.repositories.create(&in_private).await.unwrap();
    assert!(anonymous_flag(&store, in_public.id).await);
    assert!(!anonymous_flag(&store, in_private.id).await);

    // An update that writes the old value, as an N-1 binary does, is overruled.
    let mut stale = store.repositories.get(in_public.id).await.unwrap().unwrap();
    stale.allow_anonymous = false;
    store.repositories.update(&stale).await.unwrap();
    assert!(
        anonymous_flag(&store, in_public.id).await,
        "an N-1 writer cannot hide a public project"
    );
    let mut stale = store
        .repositories
        .get(in_private.id)
        .await
        .unwrap()
        .unwrap();
    stale.allow_anonymous = true;
    store.repositories.update(&stale).await.unwrap();
    assert!(
        !anonymous_flag(&store, in_private.id).await,
        "nor expose a private one"
    );

    // A repository outside a project keeps what it was given.
    let mut free = hosted_repository(&unique("free"), bs.id);
    free.allow_anonymous = true;
    store.repositories.create(&free).await.unwrap();
    assert!(anonymous_flag(&store, free.id).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn changing_the_visibility_rewrites_the_repositories_and_tells_the_other_nodes() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, _) = seed_repository(&store).await;
    let project = insert_project(&store, "handbook", "private").await;
    let mut repo = hosted_repository(&unique("docs-repo"), bs.id);
    repo.project = Some(project.clone());
    store.repositories.create(&repo).await.unwrap();
    assert!(!anonymous_flag(&store, repo.id).await);

    let mut listener = listen_rbac(&db).await;
    sqlx::query("UPDATE projects SET visibility = 'public' WHERE id = $1")
        .bind(project.id.as_uuid())
        .execute(store.pool())
        .await
        .unwrap();
    assert!(anonymous_flag(&store, repo.id).await);
    assert_eq!(
        next_payload(&mut listener, WAIT).await.as_deref(),
        Some("projects")
    );
    let back = store.repositories.get(repo.id).await.unwrap().unwrap();
    assert_eq!(back.project.unwrap().visibility, ProjectVisibility::Public);

    sqlx::query("UPDATE projects SET visibility = 'private' WHERE id = $1")
        .bind(project.id.as_uuid())
        .execute(store.pool())
        .await
        .unwrap();
    assert!(!anonymous_flag(&store, repo.id).await, "and back again");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_with_repositories_cannot_be_deleted_and_names_are_validated() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, _) = seed_repository(&store).await;
    let project = insert_project(&store, "keep-me", "private").await;
    let mut repo = hosted_repository(&unique("held"), bs.id);
    repo.project = Some(project.clone());
    store.repositories.create(&repo).await.unwrap();

    let err = sqlx::query("DELETE FROM projects WHERE id = $1")
        .bind(project.id.as_uuid())
        .execute(store.pool())
        .await
        .map_err(|e| rustryx_db::error::map_err("delete project", e))
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");

    for bad in ["A", "Upper", "-lead", "trail-", "a b", "x", &"n".repeat(65)] {
        let err = sqlx::query("INSERT INTO projects (id, name) VALUES ($1, $2)")
            .bind(Uuid::now_v7())
            .bind(bad)
            .execute(store.pool())
            .await;
        assert!(err.is_err(), "{bad} must be refused by the name check");
    }
    let dup = sqlx::query("INSERT INTO projects (id, name) VALUES ($1, 'keep-me')")
        .bind(Uuid::now_v7())
        .execute(store.pool())
        .await
        .map_err(|e| rustryx_db::error::map_err("insert project", e))
        .unwrap_err();
    assert!(
        matches!(dup, StoreError::Conflict(ref m) if m == "project name already exists"),
        "{dup:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_new_proxy_fields_round_trip_and_survive_a_reseal() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, _) = seed_repository(&store).await;
    let mut repo = hosted_repository(&unique("px"), bs.id);
    repo.repo_type = RepoType::Proxy;
    let mut proxy = ProxyConfig::new("https://registry-1.docker.io");
    let registry = RegistryId::generate();
    let ca = TrustedCertificateId::generate();
    proxy.registry_id = Some(registry);
    proxy.ca_certificate_ids = vec![ca];
    proxy.namespace_allowlist = vec!["library/*".to_owned(), "acme/**".to_owned()];
    proxy.bandwidth_limit_kbps = Some(2048);
    repo.proxy = Some(proxy);
    store.repositories.create(&repo).await.unwrap();

    let back = store
        .repositories
        .get(repo.id)
        .await
        .unwrap()
        .unwrap()
        .proxy
        .unwrap();
    assert_eq!(back.registry_id, Some(registry));
    assert_eq!(back.ca_certificate_ids, vec![ca]);
    assert_eq!(back.namespace_allowlist, ["library/*", "acme/**"]);
    assert_eq!(back.bandwidth_limit_kbps, Some(2048));
    assert_eq!(back.content_max_age_secs, 3600);

    // A resealing pass rewrites rows with credentials only, and keeps every field.
    store.repositories.reseal_upstream_secrets().await.unwrap();
    let again = store
        .repositories
        .get(repo.id)
        .await
        .unwrap()
        .unwrap()
        .proxy
        .unwrap();
    assert_eq!(again.namespace_allowlist, back.namespace_allowlist);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_row_in_the_phase_1_shape_reads_with_defaults_and_a_plain_proxy_keeps_that_shape() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, _) = seed_repository(&store).await;
    let mut repo = hosted_repository(&unique("old"), bs.id);
    repo.repo_type = RepoType::Proxy;
    repo.proxy = Some(ProxyConfig::new("https://repo.example.org"));
    store.repositories.create(&repo).await.unwrap();

    // What a phase 1 binary stored: the six fields and nothing else.
    let legacy = serde_json::json!({
        "remote_url": "https://legacy.example.org",
        "auth": null,
        "content_max_age_secs": 10,
        "metadata_max_age_secs": 20,
        "negative_cache_ttl_secs": 30,
        "max_concurrent_upstream": 4
    });
    sqlx::query("UPDATE repositories SET proxy_config = $2 WHERE id = $1")
        .bind(repo.id.as_uuid())
        .bind(&legacy)
        .execute(store.pool())
        .await
        .unwrap();
    let back = store
        .repositories
        .get(repo.id)
        .await
        .unwrap()
        .unwrap()
        .proxy
        .unwrap();
    assert_eq!(back.remote_url, "https://legacy.example.org");
    assert!(back.registry_id.is_none() && back.ca_certificate_ids.is_empty());
    assert!(back.namespace_allowlist.is_empty() && back.bandwidth_limit_kbps.is_none());

    // Written back unchanged in shape: an N-1 binary reads it and nothing new leaks in.
    store
        .repositories
        .update(&store.repositories.get(repo.id).await.unwrap().unwrap())
        .await
        .unwrap();
    let raw: serde_json::Value =
        sqlx::query_scalar("SELECT proxy_config FROM repositories WHERE id = $1")
            .bind(repo.id.as_uuid())
            .fetch_one(store.pool())
            .await
            .unwrap();
    let keys: std::collections::BTreeSet<&str> = raw
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "auth",
            "content_max_age_secs",
            "max_concurrent_upstream",
            "metadata_max_age_secs",
            "negative_cache_ttl_secs",
            "remote_url"
        ]
        .into_iter()
        .collect()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_blob_store_update_replaces_config_and_quota_and_announces_a_reset() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let mut def = blob_store(&unique("editable"));
    store.blob_stores.create(&def).await.unwrap();

    let mut listener = listen_rbac(&db).await;
    def.config = serde_json::json!({ "path": "/mnt/blobs-2" });
    def.quota_bytes = Some(10_000);
    store.blob_stores.update(&def).await.unwrap();
    let back = store.blob_stores.get(def.id).await.unwrap().unwrap();
    assert_eq!(back.config, def.config);
    assert_eq!(back.quota_bytes, Some(10_000));
    assert_eq!(
        next_payload(&mut listener, WAIT).await.as_deref(),
        Some("reset")
    );

    def.quota_bytes = None;
    store.blob_stores.update(&def).await.unwrap();
    assert_eq!(
        store
            .blob_stores
            .get(def.id)
            .await
            .unwrap()
            .unwrap()
            .quota_bytes,
        None
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_blob_store_update_refuses_identity_changes_and_unstored_settings() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let def = blob_store(&unique("fixed"));
    store.blob_stores.create(&def).await.unwrap();

    let mut renamed = def.clone();
    renamed.name = unique("renamed");
    let err = store.blob_stores.update(&renamed).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
    let mut reclassified = def.clone();
    reclassified.kind = BlobStoreKind::S3;
    let err = store.blob_stores.update(&reclassified).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");

    let mut ghost = def.clone();
    ghost.id = BlobStoreId::generate();
    let err = store.blob_stores.update(&ghost).await.unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)), "{err:?}");

    // Settings that have no column yet are refused loudly instead of being dropped.
    let mut soft = def.clone();
    soft.soft_quota = Some(rustryx_core::lifecycle::SoftQuota {
        kind: rustryx_core::lifecycle::SoftQuotaKind::MaxUsed,
        bytes: 100,
    });
    let err = store.blob_stores.update(&soft).await.unwrap_err();
    assert!(matches!(err, StoreError::Invalid(_)), "{err:?}");
    let err = store.blob_stores.create(&soft).await.unwrap_err();
    assert!(matches!(err, StoreError::Invalid(_)), "{err:?}");

    // Kinds of the storage stream are mapped, and refused by the CHECK until its migration.
    for kind in [BlobStoreKind::Azure, BlobStoreKind::Group] {
        let mut def = blob_store(&unique("future"));
        def.kind = kind;
        let err = store.blob_stores.create(&def).await.unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)), "{kind:?}: {err:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_application_grant_reads_back_and_is_not_writable_before_its_migration() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let grant = PrivilegeGrant::Application {
        area: rustryx_core::identity::AppArea::Blobstores,
    };
    let privilege = Privilege {
        id: PrivilegeId::generate(),
        name: unique("app.blobstores.read"),
        description: String::new(),
        builtin: false,
        grant: grant.clone(),
        actions: vec![Action::Read],
    };
    // The CHECK of the privileges table does not list the kind yet: refused with a fixed text.
    let err = store.privileges.create(&privilege).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(ref m) if m == "privilege grant is not valid for this schema version"),
        "{err:?}"
    );

    // A newer node that widened the CHECK wrote one: this build reads it.
    for constraint in ["ck_privileges_grant_shape", "privileges_grant_kind_check"] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE privileges DROP CONSTRAINT {constraint}"
        )))
        .execute(store.pool())
        .await
        .unwrap();
    }
    store.privileges.create(&privilege).await.unwrap();
    let back = store.privileges.get(privilege.id).await.unwrap().unwrap();
    assert_eq!(back.grant, grant);
    assert_eq!(back.actions, vec![Action::Read]);

    // An area this build does not know is a read error, never a default grant.
    sqlx::query("UPDATE privileges SET grant_repository = 'from_the_future' WHERE id = $1")
        .bind(privilege.id.as_uuid())
        .execute(store.pool())
        .await
        .unwrap();
    let err = store.privileges.get(privilege.id).await.unwrap_err();
    assert!(matches!(err, StoreError::Backend { .. }), "{err:?}");
}

//! Scan policies: one row per scope, field-wise inheritance, a deleted scanner clears the pointer.

use rustryx_core::StoreError;
use rustryx_core::ids::{ProjectId, ScannerId, SecurityPolicyId};
use rustryx_core::lifecycle::PolicyScope;
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::{BlobStoreRepo, RepositoryRepo, ScannerRepo, SecurityPolicyRepo};
use rustryx_core::scan::{AllowlistMode, ScanPolicy, ScannerKind, ScannerRecord};
use time::OffsetDateTime;

use crate::fixtures::{blob_store, hosted_repository, unique};
use crate::support::{TestDb, test_sealer};

#[tokio::test(flavor = "multi_thread")]
async fn a_scan_policy_inherits_and_drops_a_deleted_scanner() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let pool = store.pool().clone();
    let policies = rustryx_db::repos::PgScanPolicyRepo::new(pool.clone());

    let mut system = ScanPolicy::system_default(SecurityPolicyId::generate());
    policies.upsert_scan_policy(&system, None).await.unwrap();
    let mut partial = ScanPolicy::new(SecurityPolicyId::generate(), PolicyScope::System);
    partial.auto_scan = Some(false);
    assert!(matches!(
        policies.upsert_scan_policy(&partial, Some(1)).await,
        Err(StoreError::Conflict(_))
    ));

    let project = ProjectId::generate();
    sqlx::query("INSERT INTO projects (id, name) VALUES ($1, $2)")
        .bind(project.as_uuid())
        .bind(unique("scan"))
        .execute(&pool)
        .await
        .unwrap();
    let def = blob_store(&unique("blobs"));
    store.blob_stores.create(&def).await.unwrap();
    let mut repo = hosted_repository(&unique("repo"), def.id);
    repo.format = RepoFormat::Docker;
    store.repositories.create(&repo).await.unwrap();
    sqlx::query("UPDATE repositories SET project_id = $2 WHERE id = $1")
        .bind(repo.id.as_uuid())
        .bind(project.as_uuid())
        .execute(&pool)
        .await
        .unwrap();

    let scanners = rustryx_db::repos::PgScannerRepo::new(pool.clone(), test_sealer());
    let scanner = ScannerRecord::new(
        ScannerId::generate(),
        "trivy",
        ScannerKind::Trivy,
        OffsetDateTime::now_utc(),
    );
    scanners.create(&scanner).await.unwrap();

    let mut project_policy =
        ScanPolicy::new(SecurityPolicyId::generate(), PolicyScope::Project(project));
    project_policy.auto_scan = Some(false);
    project_policy.scanner_id = Some(scanner.id);
    policies
        .upsert_scan_policy(&project_policy, None)
        .await
        .unwrap();
    assert!(matches!(
        policies.upsert_scan_policy(&project_policy, None).await,
        Err(StoreError::Conflict(_))
    ));
    project_policy.allowlist_mode = Some(AllowlistMode::Project);
    assert!(matches!(
        policies.upsert_scan_policy(&project_policy, Some(2)).await,
        Err(StoreError::Conflict(_))
    ));
    policies
        .upsert_scan_policy(&project_policy, Some(1))
        .await
        .unwrap();

    let mut repo_policy = ScanPolicy::new(
        SecurityPolicyId::generate(),
        PolicyScope::Repository(repo.id),
    );
    repo_policy.quarantine = Some(true);
    policies
        .upsert_scan_policy(&repo_policy, None)
        .await
        .unwrap();

    let effective = policies.effective_for(repo.id).await.unwrap();
    assert_eq!(effective.scope, PolicyScope::Repository(repo.id));
    assert_eq!(effective.auto_scan, Some(false));
    assert_eq!(effective.quarantine, Some(true));
    assert_eq!(effective.scanner_id, Some(scanner.id));
    assert_eq!(effective.allowlist_mode, Some(AllowlistMode::Project));
    assert!(effective.gate.is_some());

    scanners.delete(scanner.id).await.unwrap();
    let after = policies
        .get_scan_policy(PolicyScope::Project(project))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.scanner_id, None);
    assert_eq!(after.auto_scan, Some(false));

    system.auto_scan = Some(false);
    policies.upsert_scan_policy(&system, Some(1)).await.unwrap();
    assert_eq!(
        policies
            .get_scan_policy(PolicyScope::System)
            .await
            .unwrap()
            .unwrap()
            .version,
        2
    );
}

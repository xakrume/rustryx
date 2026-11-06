//! Trust material and the policy that names it. The narrower scope wins whole.

use rustryx_core::ids::{ProjectId, TrustMaterialId, TrustPolicyId};
use rustryx_core::lifecycle::PolicyScope;
use rustryx_core::ports::{BlobStoreRepo, RepositoryRepo, SecurityPolicyRepo, TrustRepo};
use rustryx_core::scan::{
    GateMode, RequiredSignatures, TrustMaterial, TrustMaterialKind, TrustPolicy,
};
use time::OffsetDateTime;

use crate::fixtures::{blob_store, hosted_repository, unique};
use crate::support::TestDb;

fn key(name: &str, fingerprint: &str) -> TrustMaterial {
    TrustMaterial::new(
        TrustMaterialId::generate(),
        TrustMaterialKind::CosignPublicKey,
        name,
        "-----BEGIN PUBLIC KEY-----\nMIIB\n-----END PUBLIC KEY-----\n",
        fingerprint,
        OffsetDateTime::now_utc(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn the_repository_policy_wins_and_listed_material_stays() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let pool = store.pool().clone();
    let material = rustryx_db::repos::PgTrustRepo::new(pool.clone());
    let policies = rustryx_db::repos::PgScanPolicyRepo::new(pool.clone());
    let project = ProjectId::generate();
    sqlx::query("INSERT INTO projects (id, name) VALUES ($1, $2)")
        .bind(project.as_uuid())
        .bind(unique("trust"))
        .execute(&pool)
        .await
        .unwrap();
    let def = blob_store(&unique("blobs"));
    store.blob_stores.create(&def).await.unwrap();
    let repo = hosted_repository(&unique("lib"), def.id);
    store.repositories.create(&repo).await.unwrap();
    sqlx::query("UPDATE repositories SET project_id = $2 WHERE id = $1")
        .bind(repo.id.as_uuid())
        .bind(project.as_uuid())
        .execute(&pool)
        .await
        .unwrap();

    let fingerprint = "ab".repeat(32);
    let key = key("release", &fingerprint);
    material.create(&key).await.unwrap();
    assert!(material.create(&key).await.is_err());

    let mut system = TrustPolicy::new(TrustPolicyId::generate(), PolicyScope::System);
    system.required = RequiredSignatures::Cosign;
    system.mode = GateMode::AuditOnly;
    system.cosign_keys = vec![key.id];
    policies.upsert_trust_policy(&system, None).await.unwrap();

    let mut project_policy =
        TrustPolicy::new(TrustPolicyId::generate(), PolicyScope::Project(project));
    project_policy.required = RequiredSignatures::Cosign;
    project_policy.mode = GateMode::Enforce;
    project_policy.cosign_keys = vec![key.id];
    policies
        .upsert_trust_policy(&project_policy, None)
        .await
        .unwrap();
    let effective = policies
        .effective_trust_for(repo.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(effective.mode, GateMode::Enforce);
    assert_eq!(effective.scope, PolicyScope::Project(project));

    let mut own = TrustPolicy::new(TrustPolicyId::generate(), PolicyScope::Repository(repo.id));
    own.required = RequiredSignatures::Cosign;
    own.mode = GateMode::AuditOnly;
    own.cosign_keys = vec![key.id];
    own.cosign_require_transparency_log = true;
    policies.upsert_trust_policy(&own, None).await.unwrap();
    let effective = policies
        .effective_trust_for(repo.id)
        .await
        .unwrap()
        .unwrap();
    assert!(effective.cosign_require_transparency_log);
    assert_eq!(effective.scope, PolicyScope::Repository(repo.id));

    assert!(material.delete(key.id).await.is_err());
    own.mode = GateMode::Enforce;
    assert!(policies.upsert_trust_policy(&own, Some(9)).await.is_err());
    policies.upsert_trust_policy(&own, Some(1)).await.unwrap();
    let stored = policies
        .get_trust_policy(PolicyScope::Repository(repo.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.version, 2);
    assert_eq!(stored.mode, GateMode::Enforce);
    assert_eq!(
        material
            .list(Some(TrustMaterialKind::CosignPublicKey))
            .await
            .unwrap()
            .len(),
        1
    );
}

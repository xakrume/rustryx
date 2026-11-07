//! A promotion target, one counted approval, and the executor job it queues.

use rustryx_core::ids::{LabelId, PromotionId, UserId};
use rustryx_core::ports::{BlobStoreRepo, PromotionRepo, RepositoryRepo};
use rustryx_core::promotion::{
    PromotionDecision, PromotionMode, PromotionRequest, PromotionSelector, PromotionStatus,
    PromotionTarget,
};
use time::OffsetDateTime;

use crate::fixtures::{blob_store, hosted_repository, unique};
use crate::support::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn one_counted_approval_releases_a_promotion() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let promotions = rustryx_db::repos::PgPromotionRepo::new(store.pool().clone());
    let def = blob_store(&unique("blobs"));
    store.blob_stores.create(&def).await.unwrap();
    let source = hosted_repository(&unique("src"), def.id);
    let target_repo = hosted_repository(&unique("dst"), def.id);
    store.repositories.create(&source).await.unwrap();
    store.repositories.create(&target_repo).await.unwrap();

    let mut target = PromotionTarget::new(target_repo.id);
    target.require_approval = true;
    target.required_approvals = 1;
    promotions.set_target(&target, None).await.unwrap();
    assert!(promotions.set_target(&target, None).await.is_err());

    let requester = UserId::generate();
    let mut request = PromotionRequest::new(
        PromotionId::generate(),
        PromotionMode::Copy,
        source.id,
        target_repo.id,
        PromotionSelector::Label {
            label_id: LabelId::generate(),
        },
        requester,
        OffsetDateTime::now_utc(),
    );
    request.required_approvals = 1;
    promotions.create(&request, &[], &[]).await.unwrap();
    let decision = PromotionDecision {
        promotion_id: request.id,
        approver: UserId::generate(),
        approve: true,
        comment: "ship".to_owned(),
        counted: true,
        at: OffsetDateTime::now_utc(),
    };
    let status = promotions.decide(&decision, &[]).await.unwrap();
    assert_eq!(status, PromotionStatus::Approved);
    assert!(promotions.decide(&decision, &[]).await.is_err());
    assert!(
        promotions
            .mark_running(request.id, OffsetDateTime::now_utc())
            .await
            .unwrap()
    );
    promotions
        .finish(
            request.id,
            PromotionStatus::Succeeded,
            OffsetDateTime::now_utc(),
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        promotions.get(request.id).await.unwrap().unwrap().status,
        PromotionStatus::Succeeded
    );
}

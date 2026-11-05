//! Runs an approved promotion: link each component's blobs into the target, then delete the
//! source when the mode is move.
//!
//! A request whose snapshotted checks are not empty fails each item with `check_failed`.
//! Scan, signature and SBOM evidence is not read here yet, so a required check is not treated
//! as passed.

use time::OffsetDateTime;

use crate::audit::ActorRef;
use crate::error::{MutationError, PolicyViolation, StoreError};
use crate::events::{EventDraft, EventKind};
use crate::ids::{ComponentId, PromotionId};
use crate::model::{NewAssetLink, NewComponent, PublishMode, WriteOptions};
use crate::ports::{AssetRepo, ComponentRepo, PageRequest, PromotionRepo};
use crate::promotion::{PromotionItemStatus, PromotionMode, PromotionStatus};

/// Copies or moves every pending item of an approved promotion and finishes the request.
///
/// `false` from [`PromotionRepo::mark_running`] on a request that is not approved leaves it
/// unchanged. A request that is already `running` is resumed: only pending items are handled.
pub async fn execute(
    promotions: &dyn PromotionRepo,
    components: &dyn ComponentRepo,
    assets: &dyn AssetRepo,
    id: PromotionId,
    now: OffsetDateTime,
) -> Result<PromotionStatus, StoreError> {
    let Some(request) = promotions.get(id).await? else {
        return Err(StoreError::NotFound(format!("promotion {id}")));
    };
    if request.status == PromotionStatus::Approved {
        if !promotions.mark_running(id, now).await? {
            return promotions
                .get(id)
                .await?
                .map(|request| request.status)
                .ok_or_else(|| StoreError::NotFound(format!("promotion {id}")));
        }
    } else if request.status != PromotionStatus::Running {
        return Ok(request.status);
    }

    let mut done = 0_u64;
    let mut skipped = 0_u64;
    let mut failed = 0_u64;
    let mut after = None;
    loop {
        let page = promotions
            .items(id, &PageRequest { limit: 200, after })
            .await?;
        for item in &page.items {
            if item.status != PromotionItemStatus::Pending {
                match item.status {
                    PromotionItemStatus::Done => done += 1,
                    PromotionItemStatus::Skipped => skipped += 1,
                    PromotionItemStatus::Failed => failed += 1,
                    PromotionItemStatus::Pending => {}
                }
                continue;
            }
            match promote_one(components, assets, &request, item.component_id).await {
                ItemOutcome::Done => {
                    promotions
                        .set_item(id, item.component_id, PromotionItemStatus::Done, None)
                        .await?;
                    done += 1;
                }
                ItemOutcome::Skipped(reason) => {
                    promotions
                        .set_item(
                            id,
                            item.component_id,
                            PromotionItemStatus::Skipped,
                            Some(reason),
                        )
                        .await?;
                    skipped += 1;
                }
                ItemOutcome::Failed(reason) => {
                    promotions
                        .set_item(
                            id,
                            item.component_id,
                            PromotionItemStatus::Failed,
                            Some(reason),
                        )
                        .await?;
                    failed += 1;
                }
            }
        }
        match page.next {
            Some(token) => after = Some(token),
            None => break,
        }
    }

    let status = if failed == 0 {
        PromotionStatus::Succeeded
    } else if done + skipped == 0 {
        PromotionStatus::Failed
    } else {
        PromotionStatus::PartiallyFailed
    };
    let event = EventDraft::new(
        EventKind::PromotionCompleted,
        ActorRef::system("promotion"),
        serde_json::json!({
            "promotion_id": id,
            "status": status,
            "done": done,
            "skipped": skipped,
            "failed": failed,
        }),
    );
    promotions.finish(id, status, now, &[event]).await?;
    Ok(status)
}

enum ItemOutcome {
    Done,
    Skipped(&'static str),
    Failed(&'static str),
}

async fn promote_one(
    components: &dyn ComponentRepo,
    assets: &dyn AssetRepo,
    request: &crate::promotion::PromotionRequest,
    component_id: ComponentId,
) -> ItemOutcome {
    if !request.checks.is_empty() {
        return ItemOutcome::Failed("check_failed");
    }
    let Some(source) = components.get(component_id).await.ok().flatten() else {
        return ItemOutcome::Failed("missing");
    };
    if source.repository_id != request.source_repository_id {
        return ItemOutcome::Failed("missing");
    }
    let files = match assets.list_by_component(component_id).await {
        Ok(files) => files,
        Err(_) => return ItemOutcome::Failed("store"),
    };
    if files.is_empty() {
        return ItemOutcome::Skipped("empty");
    }
    let mut coordinate = NewComponent::new(
        request.target_repository_id,
        source.format,
        source.name.clone(),
    );
    coordinate.group_id = source.group_id.clone();
    coordinate.version = source.version.clone();
    coordinate.created_by = source.created_by;
    let target = match components.find_or_create(&coordinate).await {
        Ok(target) => target,
        Err(_) => return ItemOutcome::Failed("store"),
    };
    for file in &files {
        if let Ok(Some(existing)) = assets
            .get_by_path(request.target_repository_id, &file.path)
            .await
        {
            if existing.blob_id == file.blob_id {
                continue;
            }
            return ItemOutcome::Failed("target_exists");
        }
        let mut link = NewAssetLink::new(
            request.target_repository_id,
            file.path.clone(),
            file.content_type.clone(),
            file.blob_id,
        );
        link.component_id = Some(target.id);
        link.created_by = file.created_by;
        let options = WriteOptions::new().with_mode(PublishMode::CreateOnly);
        if let Err(error) = assets.link(&link, &options).await {
            return ItemOutcome::Failed(reason_of(&error));
        }
    }
    if request.mode == PromotionMode::Move {
        if let Err(error) = components.delete(component_id).await {
            return ItemOutcome::Failed(store_reason(&error));
        }
    }
    ItemOutcome::Done
}

fn reason_of(error: &MutationError) -> &'static str {
    match error {
        MutationError::Violation(violation) => match violation {
            PolicyViolation::Immutable { .. } => "immutable",
            PolicyViolation::Held { .. } => "held",
            PolicyViolation::QuotaExceeded(_) => "quota",
            PolicyViolation::AlreadyExists => "target_exists",
            PolicyViolation::ReadOnly => "read_only",
            PolicyViolation::ApprovalRequired => "approval_required",
            PolicyViolation::WritePolicy(_) => "write_policy",
        },
        MutationError::Store(error) => store_reason(error),
    }
}

fn store_reason(error: &StoreError) -> &'static str {
    match error {
        StoreError::Conflict(text) if text.contains("immutable") => "immutable",
        StoreError::Conflict(text) if text.contains("hold") => "held",
        StoreError::Conflict(text) if text.contains("quota") => "quota",
        StoreError::Conflict(_) => "conflict",
        _ => "store",
    }
}

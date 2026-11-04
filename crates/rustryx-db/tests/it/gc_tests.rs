//! Garbage collection history: progress, a capped dry-run sample, stop, and purge.

use rustryx_core::ids::{GcRunId, TaskRunId, UserId};
use rustryx_core::lifecycle::{GcCounts, GcOptions, GcRun, GcSample};
use rustryx_core::ports::{BlobStoreRepo, GcRunRepo, PageRequest};
use rustryx_core::tasks::{TaskRunStatus, TaskTrigger};
use time::OffsetDateTime;

use crate::fixtures::{blob_store, unique};
use crate::support::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_gc_run_keeps_its_sample_and_purges() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let pool = store.pool().clone();
    let gc = rustryx_db::repos::PgGcRunRepo::new(pool);
    let def = blob_store(&unique("blobs"));
    store.blob_stores.create(&def).await.unwrap();

    let started = OffsetDateTime::now_utc();
    let mut options = GcOptions::new(3600);
    options.dry_run = true;
    options.blob_store = Some(def.id);
    let mut run = GcRun::new(GcRunId::generate(), TaskTrigger::Manual, options);
    run.requested_by = Some(UserId::generate());
    run.task_run_id = Some(TaskRunId::generate());
    run.started_at = Some(started);
    gc.create(&run).await.unwrap();
    assert!(gc.create(&run).await.is_err());

    let counts = GcCounts {
        examined: 4,
        candidates: 2,
        deleted: 0,
        failed: 0,
        bytes_candidates: 90,
        bytes_deleted: 0,
        untagged_deleted: 0,
    };
    let per_store = serde_json::json!({ "files": { "candidates": 2 } });
    gc.update_progress(run.id, &counts, &per_store).await.unwrap();
    assert!(gc.request_stop(run.id).await.unwrap());
    assert!(gc.stop_requested(run.id).await.unwrap());

    let orphaned = started - time::Duration::hours(2);
    let mut samples: Vec<GcSample> = (0..1000)
        .map(|index| GcSample {
            run_id: run.id,
            blob_store: "files".to_owned(),
            key: format!("blob-{index}"),
            size_bytes: 10,
            orphaned_at: orphaned,
        })
        .collect();
    samples.push(GcSample {
        run_id: run.id,
        blob_store: "files".to_owned(),
        key: "past-the-cap".to_owned(),
        size_bytes: 10,
        orphaned_at: orphaned,
    });
    gc.save_samples(&samples).await.unwrap();
    let page = gc
        .list_samples(run.id, &PageRequest { limit: 1000, after: None })
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1000);
    assert!(page.next.is_none());
    assert_eq!(page.items[0].key, "blob-0");
    assert!(page.items.iter().all(|sample| sample.key != "past-the-cap"));

    let finished = started + time::Duration::minutes(1);
    gc.finish(run.id, TaskRunStatus::Cancelled, finished).await.unwrap();
    assert!(!gc.request_stop(run.id).await.unwrap());
    let stored = gc.get(run.id).await.unwrap().unwrap();
    assert_eq!(stored.status, TaskRunStatus::Cancelled);
    assert_eq!(stored.counts.candidates, 2);
    assert!(stored.stop_requested);
    assert_eq!(stored.options.blob_store, Some(def.id));
    let manual = gc.last_manual_start().await.unwrap().unwrap();
    assert_eq!(manual, stored.started_at.unwrap());

    let mut later = GcRun::new(GcRunId::generate(), TaskTrigger::Schedule, GcOptions::new(3600));
    later.status = TaskRunStatus::Running;
    gc.create(&later).await.unwrap();
    let first = gc
        .list(&PageRequest { limit: 1, after: None })
        .await
        .unwrap();
    assert_eq!(first.items.len(), 1);
    let second = gc
        .list(&PageRequest { limit: 1, after: first.next })
        .await
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert_ne!(first.items[0].id, second.items[0].id);

    let removed = gc
        .purge(finished + time::Duration::seconds(1))
        .await
        .unwrap();
    assert_eq!(removed, 1);
    assert!(gc.get(run.id).await.unwrap().is_none());
    assert!(gc.get(later.id).await.unwrap().is_some());
    assert!(gc.list_samples(run.id, &PageRequest::default()).await.is_err());
}

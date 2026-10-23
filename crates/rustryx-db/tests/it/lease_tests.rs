use std::collections::HashSet;
use std::time::Duration;

use rustryx_core::StoreError;
use rustryx_core::events::{CursorAdvance, OutboxCursor};
use rustryx_core::ports::{JobLeaseRepo, NewJob};
use time::Duration as TimeDuration;
use tokio::task::JoinSet;

use crate::fixtures::now;
use crate::support::TestDb;

const LEASE: Duration = Duration::from_secs(30);

#[tokio::test(flavor = "multi_thread")]
async fn enqueue_deduplicates_queued_and_running_jobs() {
    let db = TestDb::new().await;
    let leases = db.store().await.job_leases;
    let payload = serde_json::json!({ "repo": "maven-central" });
    assert!(
        leases
            .enqueue("cleanup", Some("repo:maven-central"), &payload)
            .await
            .unwrap()
    );
    assert!(
        !leases
            .enqueue("cleanup", Some("repo:maven-central"), &payload)
            .await
            .unwrap(),
        "queued twin"
    );
    assert!(
        leases
            .enqueue("cleanup", Some("repo:npm"), &payload)
            .await
            .unwrap()
    );
    assert!(
        leases
            .enqueue("gc", Some("repo:maven-central"), &payload)
            .await
            .unwrap(),
        "other queue"
    );
    assert!(leases.enqueue("cleanup", None, &payload).await.unwrap());
    assert!(
        leases.enqueue("cleanup", None, &payload).await.unwrap(),
        "no key, no dedupe"
    );

    let job = leases
        .claim("cleanup", "node-a", LEASE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.payload, payload);
    assert_eq!(job.attempts, 1);
    // Still deduplicated while the job runs; free again once it is done.
    assert!(
        !leases
            .enqueue("cleanup", Some("repo:maven-central"), &payload)
            .await
            .unwrap()
    );
    assert!(leases.complete(job.id, "node-a").await.unwrap());
    assert!(
        leases
            .enqueue("cleanup", Some("repo:maven-central"), &payload)
            .await
            .unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn claim_hands_every_job_to_exactly_one_worker() {
    let db = TestDb::new().await;
    let leases = db.store().await.job_leases;
    for i in 0..30 {
        leases
            .enqueue("work", None, &serde_json::json!({ "n": i }))
            .await
            .unwrap();
    }
    let mut workers = JoinSet::new();
    for w in 0..8 {
        let leases = leases.clone();
        workers.spawn(async move {
            let mut mine = Vec::new();
            while let Some(job) = leases
                .claim("work", &format!("node-{w}"), LEASE)
                .await
                .unwrap()
            {
                mine.push(job.id);
            }
            mine
        });
    }
    let mut all = Vec::new();
    while let Some(joined) = workers.join_next().await {
        all.extend(joined.unwrap());
    }
    assert_eq!(all.len(), 30);
    assert_eq!(
        all.iter().collect::<HashSet<_>>().len(),
        30,
        "no job claimed twice"
    );
    assert!(leases.claim("work", "late", LEASE).await.unwrap().is_none());
    assert!(
        leases
            .claim("empty-queue", "late", LEASE)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_expired_lease_is_taken_over_and_the_old_owner_is_told() {
    let db = TestDb::new().await;
    let leases = db.store().await.job_leases;
    leases
        .enqueue("q", None, &serde_json::json!({}))
        .await
        .unwrap();
    let short = Duration::from_millis(1000);
    let first = leases.claim("q", "node-a", short).await.unwrap().unwrap();
    assert!(
        leases.claim("q", "node-b", short).await.unwrap().is_none(),
        "lease still valid"
    );
    assert!(
        leases.heartbeat(first.id, "node-a", short).await.unwrap(),
        "owner renews in time"
    );

    tokio::time::sleep(Duration::from_millis(1300)).await;
    let second = leases
        .claim("q", "node-b", LEASE)
        .await
        .unwrap()
        .expect("expired lease is claimable");
    assert_eq!(second.id, first.id);
    assert_eq!(second.attempts, 2);
    assert!(
        !leases.heartbeat(first.id, "node-a", LEASE).await.unwrap(),
        "the old owner lost it"
    );
    assert!(!leases.complete(first.id, "node-a").await.unwrap());
    assert!(leases.complete(second.id, "node-b").await.unwrap());
    assert!(
        leases.claim("q", "node-c", LEASE).await.unwrap().is_none(),
        "done jobs are not claimed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn release_for_retry_delays_the_next_claim_and_keeps_the_error() {
    let db = TestDb::new().await;
    let store = db.store().await;
    store
        .job_leases
        .enqueue("q", Some("k"), &serde_json::json!({}))
        .await
        .unwrap();
    let job = store
        .job_leases
        .claim("q", "node-a", LEASE)
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .job_leases
            .release_for_retry(
                job.id,
                "node-a",
                Duration::from_millis(1500),
                &"x".repeat(900)
            )
            .await
            .unwrap()
    );
    assert!(
        !store
            .job_leases
            .release_for_retry(job.id, "node-b", Duration::ZERO, "no")
            .await
            .unwrap()
    );
    assert!(
        store
            .job_leases
            .claim("q", "node-a", LEASE)
            .await
            .unwrap()
            .is_none(),
        "not before the delay"
    );
    let stored: String = sqlx::query_scalar("SELECT last_error FROM job_leases WHERE id = $1")
        .bind(job.id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(stored.len(), 500, "the error text is bounded");

    tokio::time::sleep(Duration::from_millis(1700)).await;
    let again = store
        .job_leases
        .claim("q", "node-a", LEASE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.attempts, 2);
}

fn job(queue: &str, key: Option<&str>, n: i32) -> NewJob {
    NewJob {
        queue: queue.to_owned(),
        dedupe_key: key.map(ToOwned::to_owned),
        payload: serde_json::json!({ "n": n }),
        delay: Duration::ZERO,
    }
}

async fn job_count(store: &rustryx_db::PgStore) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM job_leases")
        .fetch_one(store.pool())
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn enqueue_after_delays_the_first_claim() {
    let db = TestDb::new().await;
    let leases = db.store().await.job_leases;
    assert!(
        leases
            .enqueue_after(
                "q",
                Some("later"),
                &serde_json::json!({}),
                Duration::from_millis(1200)
            )
            .await
            .unwrap()
    );
    assert!(
        leases.claim("q", "n", LEASE).await.unwrap().is_none(),
        "not before the delay"
    );
    assert_eq!(
        leases.depth("q").await.unwrap(),
        1,
        "a delayed job still counts as queued"
    );
    tokio::time::sleep(Duration::from_millis(1400)).await;
    assert!(leases.claim("q", "n", LEASE).await.unwrap().is_some());
    // The dedupe key still applies to a delayed enqueue.
    assert!(
        !leases
            .enqueue_after(
                "q",
                Some("later"),
                &serde_json::json!({}),
                Duration::from_secs(60)
            )
            .await
            .unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn fail_is_terminal_keeps_a_bounded_error_and_frees_the_dedupe_key() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let leases = &store.job_leases;
    leases
        .enqueue("q", Some("k"), &serde_json::json!({}))
        .await
        .unwrap();
    let claimed = leases.claim("q", "node-a", LEASE).await.unwrap().unwrap();

    assert!(
        !leases.fail(claimed.id, "node-b", "not mine").await.unwrap(),
        "lost lease"
    );
    assert!(
        leases
            .fail(claimed.id, "node-a", &"e".repeat(900))
            .await
            .unwrap()
    );
    assert!(
        !leases.fail(claimed.id, "node-a", "twice").await.unwrap(),
        "already terminal"
    );
    let (state, error): (String, String) =
        sqlx::query_as("SELECT state, last_error FROM job_leases WHERE id = $1")
            .bind(claimed.id)
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(state, "failed");
    assert_eq!(error.len(), 500);
    assert!(
        leases.claim("q", "node-a", LEASE).await.unwrap().is_none(),
        "never claimed again"
    );
    assert_eq!(leases.depth("q").await.unwrap(), 0);
    assert!(
        leases
            .enqueue("q", Some("k"), &serde_json::json!({}))
            .await
            .unwrap(),
        "the same logical job can be queued again after a terminal failure"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn depth_counts_queued_and_running_rows_of_one_queue() {
    let db = TestDb::new().await;
    let leases = db.store().await.job_leases;
    for n in 0..3 {
        leases
            .enqueue("a", None, &serde_json::json!({ "n": n }))
            .await
            .unwrap();
    }
    leases
        .enqueue("b", None, &serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(leases.depth("a").await.unwrap(), 3);
    let job = leases.claim("a", "n", LEASE).await.unwrap().unwrap();
    assert_eq!(leases.depth("a").await.unwrap(), 3, "running counts");
    leases.complete(job.id, "n").await.unwrap();
    assert_eq!(leases.depth("a").await.unwrap(), 2, "done does not");
    assert_eq!(leases.depth("b").await.unwrap(), 1);
    assert_eq!(leases.depth("none").await.unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn purge_finished_removes_only_old_done_and_failed_rows() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let leases = &store.job_leases;
    for key in ["done", "failed", "queued", "running"] {
        leases
            .enqueue("q", Some(key), &serde_json::json!({ "k": key }))
            .await
            .unwrap();
    }
    // Claim order is by availability: take all four and finish two of them.
    let mut claimed = Vec::new();
    while let Some(job) = leases.claim("q", "n", LEASE).await.unwrap() {
        claimed.push(job);
    }
    for job in &claimed {
        match job.payload["k"].as_str().unwrap() {
            "done" => assert!(leases.complete(job.id, "n").await.unwrap()),
            "failed" => assert!(leases.fail(job.id, "n", "boom").await.unwrap()),
            "running" => {}
            _ => assert!(
                leases
                    .release_for_retry(job.id, "n", Duration::from_secs(3600), "later")
                    .await
                    .unwrap()
            ),
        }
    }
    // Nothing is old enough yet.
    assert_eq!(
        leases
            .purge_finished(now() - TimeDuration::hours(1))
            .await
            .unwrap(),
        0
    );
    assert_eq!(job_count(&store).await, 4);
    // An hour from now both finished rows are old; the other two never are.
    assert_eq!(
        leases
            .purge_finished(now() + TimeDuration::hours(1))
            .await
            .unwrap(),
        2
    );
    assert_eq!(job_count(&store).await, 2);
    assert_eq!(
        leases
            .purge_finished(now() + TimeDuration::days(1))
            .await
            .unwrap(),
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_batch_inserts_every_row_once_and_dedupes_inside_and_across_batches() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let leases = &store.job_leases;
    leases
        .enqueue("scan", Some("existing"), &serde_json::json!({}))
        .await
        .unwrap();

    let inserted = leases
        .enqueue_batch(
            &[
                job("scan", Some("existing"), 0),
                job("scan", Some("new"), 1),
                job("scan", Some("new"), 2),
                job("scan", None, 3),
                job("scan", None, 4),
                job("other", Some("new"), 5),
            ],
            None,
        )
        .await
        .unwrap();
    // `existing` and the second `new` are skipped; a null key never dedupes; another queue
    // has its own key space.
    assert_eq!(inserted, 4);
    assert_eq!(leases.depth("scan").await.unwrap(), 4);
    assert_eq!(leases.depth("other").await.unwrap(), 1);
    assert_eq!(leases.enqueue_batch(&[], None).await.unwrap(), 0);

    // A delay in a batch applies to its own row only.
    let mut delayed = job("timed", None, 1);
    delayed.delay = Duration::from_secs(3600);
    leases
        .enqueue_batch(&[delayed, job("timed", None, 2)], None)
        .await
        .unwrap();
    let claimed = leases.claim("timed", "n", LEASE).await.unwrap().unwrap();
    assert_eq!(claimed.payload["n"], 2);
    assert!(leases.claim("timed", "n", LEASE).await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn queue_names_are_checked_as_invalid_input() {
    let db = TestDb::new().await;
    let leases = db.store().await.job_leases;
    for bad in [String::new(), "q".repeat(129)] {
        let err = leases
            .enqueue(&bad, None, &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Invalid(_)), "{err:?}");
        let err = leases
            .enqueue_batch(&[job(&bad, None, 1)], None)
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Invalid(_)), "{err:?}");
    }
    assert!(
        leases
            .enqueue(&"q".repeat(128), None, &serde_json::json!({}))
            .await
            .unwrap()
    );
}

fn advance(consumer: &'static str, from: (u64, i64), to: (u64, i64)) -> CursorAdvance {
    CursorAdvance {
        consumer,
        from: OutboxCursor {
            tx_id: from.0,
            seq: from.1,
        },
        to: OutboxCursor {
            tx_id: to.0,
            seq: to.1,
        },
    }
}

async fn cursor_of(store: &rustryx_db::PgStore, consumer: &str) -> Option<(String, i64)> {
    sqlx::query_as("SELECT last_tx_id::text, last_seq FROM outbox_cursors WHERE consumer = $1")
        .bind(consumer)
        .fetch_optional(store.pool())
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_batch_and_the_cursor_move_commit_together() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let leases = &store.job_leases;

    // The first move of a consumer starts at (0, 0) and creates its row.
    let n = leases
        .enqueue_batch(
            &[job("scan", Some("a"), 1)],
            Some(&advance("scan", (0, 0), (500, 7))),
        )
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(cursor_of(&store, "scan").await, Some(("500".to_owned(), 7)));

    // The next batch continues from where the cursor is.
    let n = leases
        .enqueue_batch(
            &[job("scan", Some("b"), 2)],
            Some(&advance("scan", (500, 7), (900, 12))),
        )
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(
        cursor_of(&store, "scan").await,
        Some(("900".to_owned(), 12))
    );

    // A move with nothing to write is just the cursor.
    let n = leases
        .enqueue_batch(&[], Some(&advance("scan", (900, 12), (901, 13))))
        .await
        .unwrap();
    assert_eq!(n, 0);
    assert_eq!(
        cursor_of(&store, "scan").await,
        Some(("901".to_owned(), 13))
    );
    assert_eq!(leases.depth("scan").await.unwrap(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_write_a_batch_after_losing_the_cursor() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let leases = &store.job_leases;
    leases
        .enqueue_batch(&[], Some(&advance("scan", (0, 0), (100, 5))))
        .await
        .unwrap();

    // A consumer that read the old position (lost its lock, or a duplicate instance): nothing
    // of its batch is written, and the cursor stays where the winner put it.
    let stale = advance("scan", (0, 0), (150, 9));
    let inserted = leases
        .enqueue_batch(&[job("scan", None, 1), job("scan", None, 2)], Some(&stale))
        .await
        .unwrap();
    assert_eq!(inserted, 0);
    assert_eq!(job_count(&store).await, 0, "no row of the dropped batch");
    assert_eq!(cursor_of(&store, "scan").await, Some(("100".to_owned(), 5)));

    // A consumer that never ran cannot claim a position other than the start either.
    let skipped = advance("fresh", (7, 7), (8, 8));
    assert_eq!(
        leases
            .enqueue_batch(&[job("scan", None, 3)], Some(&skipped))
            .await
            .unwrap(),
        0
    );
    assert!(cursor_of(&store, "fresh").await.is_none());
    assert_eq!(job_count(&store).await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn of_many_consumers_with_one_batch_exactly_one_writes_it() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let mut tasks = JoinSet::new();
    for n in 0..10 {
        let leases = store.job_leases.clone();
        tasks.spawn(async move {
            leases
                .enqueue_batch(
                    &[job("scan", None, n)],
                    Some(&advance("scan", (0, 0), (1000, 10))),
                )
                .await
                .unwrap()
        });
    }
    let mut written = 0;
    while let Some(done) = tasks.join_next().await {
        written += done.unwrap();
    }
    assert_eq!(written, 1);
    assert_eq!(job_count(&store).await, 1);
    assert_eq!(
        cursor_of(&store, "scan").await,
        Some(("1000".to_owned(), 10))
    );
}

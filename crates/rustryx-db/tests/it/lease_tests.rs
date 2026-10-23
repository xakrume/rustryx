use std::collections::HashSet;
use std::time::Duration;

use tokio::task::JoinSet;

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

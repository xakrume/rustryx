//! The read side of the audit log: filters, keyset paging and the NDJSON export stream.

use std::collections::HashSet;

use futures::StreamExt;
use rustryx_core::StoreError;
use rustryx_core::audit::{ActorKind, AuditEvent, AuditKind};
use rustryx_core::ids::{ProjectId, UserId};
use rustryx_core::ports::{AuditQuery, AuditReadRepo, AuditSink, PageRequest};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::fixtures::now;
use crate::support::TestDb;

fn event(kind: AuditKind, action: &str, at: OffsetDateTime) -> AuditEvent {
    let mut event = AuditEvent::new(kind, "alice", action, "success");
    event.at = at;
    event
}

fn page(limit: u32, after: Option<String>) -> PageRequest {
    PageRequest { limit, after }
}

/// Reads every page of `query` with `limit` rows per page.
async fn all_pages(
    store: &rustryx_db::PgStore,
    query: &AuditQuery,
    limit: u32,
) -> Vec<rustryx_core::ports::StoredAuditEvent> {
    let mut items = Vec::new();
    let mut after = None;
    for _ in 0..1000 {
        let result = store
            .audit_read
            .query(query, &page(limit, after.take()))
            .await
            .unwrap();
        items.extend(result.items);
        match result.next {
            Some(next) => after = Some(next),
            None => return items,
        }
    }
    panic!("paging did not end");
}

async fn bulk_insert(pool: &PgPool, count: i32, project: Option<Uuid>, base: OffsetDateTime) {
    sqlx::query(
        "INSERT INTO audit_events (id, event_time, kind, actor_name, action, outcome, project_id) \
         SELECT gen_random_uuid(), $1::timestamptz + make_interval(secs => g), 'content_change', \
                'bulk', 'asset.upload', 'success', $2 \
         FROM generate_series(1, $3) AS g",
    )
    .bind(base)
    .bind(project)
    .bind(count)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn rows_round_trip_with_every_field() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let project = ProjectId::generate();
    let actor = UserId::generate();
    let mut written = event(AuditKind::ProjectChange, "project.update", now());
    written.actor_id = Some(actor);
    written.actor_kind = ActorKind::Robot;
    written.client_ip = Some("203.0.113.9".to_owned());
    written.request_id = Some("req-42".to_owned());
    written.target = Some("payments".to_owned());
    written.outcome = "failure".to_owned();
    written.project_id = Some(project);
    written.detail = serde_json::json!({ "field": "visibility" });
    store.audit.record(written.clone()).await.unwrap();

    let result = store
        .audit_read
        .query(&AuditQuery::new(), &PageRequest::default())
        .await
        .unwrap();
    assert_eq!(result.items.len(), 1);
    let read = &result.items[0].event;
    assert_eq!(read.at, written.at);
    assert_eq!(read.kind, AuditKind::ProjectChange);
    assert_eq!(read.actor_id, Some(actor));
    assert_eq!(read.actor_kind, ActorKind::Robot);
    assert_eq!(read.actor_name, "alice");
    assert_eq!(read.client_ip.as_deref(), Some("203.0.113.9"));
    assert_eq!(read.request_id.as_deref(), Some("req-42"));
    assert_eq!(read.action, "project.update");
    assert_eq!(read.target.as_deref(), Some("payments"));
    assert_eq!(read.outcome, "failure");
    assert_eq!(read.project_id, Some(project));
    assert_eq!(read.detail, written.detail);
    assert!(result.next.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn every_kind_the_sink_can_write_can_be_read_back() {
    let db = TestDb::new().await;
    let store = db.store().await;
    for kind in AuditKind::ALL {
        store.audit.record(event(*kind, "x", now())).await.unwrap();
    }
    let read = all_pages(&store, &AuditQuery::new(), 100).await;
    let kinds: HashSet<_> = read.iter().map(|s| s.event.kind).collect();
    assert_eq!(kinds, AuditKind::ALL.iter().copied().collect());
}

#[tokio::test(flavor = "multi_thread")]
async fn paging_is_newest_first_and_complete_even_when_events_share_one_instant() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let same_instant = now() + Duration::hours(1);
    // Ten events in one microsecond: only the id tells them apart.
    for n in 0..10 {
        store
            .audit
            .record(event(
                AuditKind::ContentChange,
                &format!("same.{n}"),
                same_instant,
            ))
            .await
            .unwrap();
    }
    for n in 0..5 {
        store
            .audit
            .record(event(
                AuditKind::ContentChange,
                &format!("older.{n}"),
                same_instant - Duration::seconds(10 + n),
            ))
            .await
            .unwrap();
    }
    let read = all_pages(&store, &AuditQuery::new(), 4).await;
    assert_eq!(
        read.len(),
        15,
        "no row skipped or repeated across page borders"
    );
    let ids: HashSet<_> = read.iter().map(|s| s.id).collect();
    assert_eq!(ids.len(), 15);
    let keys: Vec<(OffsetDateTime, Uuid)> = read.iter().map(|s| (s.event.at, s.id)).collect();
    let mut sorted = keys.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(keys, sorted, "newest first by (time, id)");
}

#[tokio::test(flavor = "multi_thread")]
async fn each_filter_narrows_the_result() {
    let db = TestDb::new().await;
    let store = db.store().await;
    // Events lie in the future on purpose: a test that ran just after midnight on the first of
    // a month would otherwise write into a month that has no partition.
    let t0 = now() + Duration::hours(1);
    let user = UserId::generate();
    let project = ProjectId::generate();

    let mut updated = event(AuditKind::RbacChange, "role.update", t0);
    updated.actor_id = Some(user);
    updated.target = Some("nx-developer".to_owned());
    let mut created = event(
        AuditKind::RbacChange,
        "role.create",
        t0 + Duration::minutes(10),
    );
    created.actor_name = "bob".to_owned();
    created.outcome = "failure".to_owned();
    let mut login = event(
        AuditKind::LoginFailure,
        "login.password",
        t0 + Duration::minutes(20),
    );
    login.project_id = Some(project);
    let mut token = event(
        AuditKind::TokenChange,
        "token.create",
        t0 + Duration::minutes(30),
    );
    token.target = Some("ci".to_owned());
    for e in [&updated, &created, &login, &token] {
        store.audit.record((*e).clone()).await.unwrap();
    }

    let count = |query: AuditQuery| {
        let store = store.clone();
        async move { all_pages(&store, &query, 100).await.len() }
    };
    let mut q = AuditQuery::new();
    q.kinds = vec![AuditKind::RbacChange, AuditKind::TokenChange];
    assert_eq!(count(q).await, 3);
    let mut q = AuditQuery::new();
    q.actor_id = Some(user);
    assert_eq!(count(q).await, 1);
    let mut q = AuditQuery::new();
    q.actor_name = Some("bob".to_owned());
    assert_eq!(count(q).await, 1);
    let mut q = AuditQuery::new();
    q.action_prefix = Some("role.".to_owned());
    assert_eq!(count(q).await, 2);
    let mut q = AuditQuery::new();
    q.target = Some("ci".to_owned());
    assert_eq!(count(q).await, 1);
    let mut q = AuditQuery::new();
    q.outcome = Some("failure".to_owned());
    assert_eq!(count(q).await, 1);
    let mut q = AuditQuery::new();
    q.project_id = Some(project);
    assert_eq!(count(q).await, 1);

    // The window is [from, to): from is inclusive, to is exclusive.
    let mut q = AuditQuery::new();
    q.from = Some(t0 + Duration::minutes(10));
    q.to = Some(t0 + Duration::minutes(30));
    let in_window = all_pages(&store, &q, 100).await;
    assert_eq!(in_window.len(), 2);
    assert_eq!(in_window[0].event.action, "login.password");
    assert_eq!(in_window[1].event.action, "role.create");

    // Filters combine with AND.
    let mut q = AuditQuery::new();
    q.kinds = vec![AuditKind::RbacChange];
    q.outcome = Some("failure".to_owned());
    q.actor_name = Some("bob".to_owned());
    assert_eq!(count(q).await, 1);
    let mut q = AuditQuery::new();
    q.kinds = vec![AuditKind::RbacChange];
    q.outcome = Some("failure".to_owned());
    q.actor_name = Some("alice".to_owned());
    assert_eq!(count(q).await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_action_prefix_is_matched_literally_not_as_a_pattern() {
    let db = TestDb::new().await;
    let store = db.store().await;
    for action in ["role.update", "role_update", "role%update", "roleXupdate"] {
        store
            .audit
            .record(event(AuditKind::RbacChange, action, now()))
            .await
            .unwrap();
    }
    let by_prefix = |prefix: &'static str| {
        let store = store.clone();
        async move {
            let mut q = AuditQuery::new();
            q.action_prefix = Some(prefix.to_owned());
            let mut actions: Vec<String> = all_pages(&store, &q, 100)
                .await
                .into_iter()
                .map(|s| s.event.action)
                .collect();
            actions.sort();
            actions
        }
    };
    assert_eq!(
        by_prefix("role_").await,
        ["role_update"],
        "'_' is not a wildcard"
    );
    assert_eq!(
        by_prefix("role%").await,
        ["role%update"],
        "'%' is not a wildcard"
    );
    assert_eq!(by_prefix("role.").await, ["role.update"]);
    assert_eq!(by_prefix("role").await.len(), 4);
    assert!(by_prefix("%").await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_return_another_projects_rows_when_a_project_filter_is_set() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let pool = store.pool().clone();
    let mine = ProjectId::generate();
    let theirs = ProjectId::generate();
    let base = now() + Duration::days(1);
    bulk_insert(&pool, 700, Some(mine.as_uuid()), base).await;
    bulk_insert(&pool, 300, Some(theirs.as_uuid()), base).await;
    bulk_insert(&pool, 200, None, base).await;

    let mut query = AuditQuery::new();
    query.project_id = Some(mine);

    // Through every page of the viewer...
    let paged = all_pages(&store, &query, 250).await;
    assert_eq!(paged.len(), 700);
    assert!(paged.iter().all(|s| s.event.project_id == Some(mine)));

    // ...through the export stream, across its batch borders...
    let exported: Vec<_> = store
        .audit_read
        .export(&query)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
    assert_eq!(exported.len(), 700);
    assert!(exported.iter().all(|s| s.event.project_id == Some(mine)));

    // ...with other filters added, and with a token taken from a page of ANOTHER query: a
    // token only moves the window, it never widens the filter.
    query.kinds = vec![AuditKind::ContentChange];
    let combined = all_pages(&store, &query, 1000).await;
    assert_eq!(combined.len(), 700);
    let foreign_page = store
        .audit_read
        .query(&AuditQuery::new(), &page(10, None))
        .await
        .unwrap();
    let token = foreign_page.next.unwrap();
    let moved = store
        .audit_read
        .query(&query, &page(1000, Some(token)))
        .await
        .unwrap();
    assert!(moved.items.iter().all(|s| s.event.project_id == Some(mine)));

    // A filter on a project without events is empty, not "everything".
    let mut nobody = AuditQuery::new();
    nobody.project_id = Some(ProjectId::generate());
    assert!(all_pages(&store, &nobody, 100).await.is_empty());
    assert!(
        store
            .audit_read
            .export(&nobody)
            .collect::<Vec<_>>()
            .await
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_tokens_are_invalid_input() {
    use base64::Engine;
    let db = TestDb::new().await;
    let store = db.store().await;
    store
        .audit
        .record(event(AuditKind::LoginSuccess, "login", now()))
        .await
        .unwrap();
    let encode = |text: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text);
    let tokens = [
        "not base64!!".to_owned(),
        String::new(),
        encode("audit:"),
        encode("audit:12345"),
        encode("audit:abc:not-a-uuid"),
        encode("audit:1:2:3"),
        encode(&format!("user:1:{}", Uuid::now_v7())),
        encode(&format!(
            "audit:99999999999999999999999999999999999999:{}",
            Uuid::now_v7()
        )),
    ];
    for token in tokens {
        let err = store
            .audit_read
            .query(&AuditQuery::new(), &page(10, Some(token.clone())))
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Invalid(_)), "{token}: {err:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_export_streams_oldest_first_in_batches_and_stops_when_dropped() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let pool = store.pool().clone();
    let base = now() + Duration::days(1);
    bulk_insert(&pool, 1203, None, base).await;

    let mut stream = store.audit_read.export(&AuditQuery::new());
    let mut seen = Vec::new();
    while let Some(item) = stream.next().await {
        seen.push(item.unwrap());
    }
    assert_eq!(
        seen.len(),
        1203,
        "more than two batches, none lost, none repeated"
    );
    let unique: HashSet<_> = seen.iter().map(|s| s.id).collect();
    assert_eq!(unique.len(), 1203);
    let keys: Vec<(OffsetDateTime, Uuid)> = seen.iter().map(|s| (s.event.at, s.id)).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "oldest first");

    // Dropping the stream after a few rows ends the work: nothing is left running.
    let mut partial = store.audit_read.export(&AuditQuery::new());
    for _ in 0..3 {
        partial.next().await.unwrap().unwrap();
    }
    drop(partial);

    // An empty export ends at once.
    let mut q = AuditQuery::new();
    q.actor_name = Some("nobody".to_owned());
    assert!(store.audit_read.export(&q).next().await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_kind_is_an_error_item_and_ends_the_export_instead_of_skipping_rows() {
    let db = TestDb::new().await;
    let store = db.store().await;
    store
        .audit
        .record(event(
            AuditKind::LoginSuccess,
            "first",
            now() + Duration::minutes(1),
        ))
        .await
        .unwrap();
    // A newer node wrote a kind this build does not know.
    sqlx::query(
        "INSERT INTO audit_events (id, event_time, kind, actor_name, action, outcome) \
         VALUES ($1, now() + interval '2 minutes', 'kind_from_the_future', 'x', 'a', 'success')",
    )
    .bind(Uuid::now_v7())
    .execute(store.pool())
    .await
    .unwrap();

    let results = store
        .audit_read
        .export(&AuditQuery::new())
        .collect::<Vec<_>>()
        .await;
    assert_eq!(
        results.len(),
        1,
        "the failing batch yields its error and the stream ends"
    );
    let err = results.into_iter().next().unwrap().unwrap_err();
    assert!(matches!(err, StoreError::Backend { .. }), "{err:?}");
    let err = store
        .audit_read
        .query(&AuditQuery::new(), &PageRequest::default())
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Backend { .. }), "{err:?}");
    assert!(!format!("{err} {err:?}").contains("kind_from_the_future"));
}

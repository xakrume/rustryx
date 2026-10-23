use std::collections::HashSet;

use rustryx_core::StoreError;
use rustryx_core::ids::ComponentId;
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::{AssetRepo, ComponentFilter, ComponentRepo, PageRequest};
use tokio::task::JoinSet;

use crate::fixtures::{blob, new_asset, new_component, seed_repository};
use crate::support::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn find_or_create_returns_the_same_row_for_one_coordinate() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (_, repo) = seed_repository(&store).await;
    let new = new_component(repo.id, Some("com.acme"), "app", Some("1.0"));
    let first = store.components.find_or_create(&new).await.unwrap();
    let second = store.components.find_or_create(&new).await.unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(first.name, "app");
    assert_eq!(first.group_id.as_deref(), Some("com.acme"));
    assert_eq!(first.version.as_deref(), Some("1.0"));
    assert_eq!(first.format, RepoFormat::Maven2);

    assert_eq!(
        store.components.get(first.id).await.unwrap().unwrap().id,
        first.id
    );
    assert_eq!(
        store.components.find(&new).await.unwrap().unwrap().id,
        first.id
    );
    assert!(
        store
            .components
            .get(ComponentId::generate())
            .await
            .unwrap()
            .is_none()
    );
    let other = new_component(repo.id, Some("com.acme"), "app", Some("2.0"));
    assert!(store.components.find(&other).await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn uniqueness_is_null_safe() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (_, repo) = seed_repository(&store).await;
    // No namespace and no version: a plain UNIQUE would let this repeat forever.
    let bare = new_component(repo.id, None, "left-pad", None);
    let a = store.components.find_or_create(&bare).await.unwrap();
    let b = store.components.find_or_create(&bare).await.unwrap();
    assert_eq!(a.id, b.id);

    let scoped = store
        .components
        .find_or_create(&new_component(repo.id, Some("@scope"), "left-pad", None))
        .await
        .unwrap();
    assert_ne!(scoped.id, a.id);
    let versioned = store
        .components
        .find_or_create(&new_component(repo.id, None, "left-pad", Some("1.0.0")))
        .await
        .unwrap();
    assert_ne!(versioned.id, a.id);

    let mut npm = bare.clone();
    npm.format = RepoFormat::Npm;
    assert_ne!(
        store.components.find_or_create(&npm).await.unwrap().id,
        a.id
    );

    // The constraint itself holds against a direct duplicate insert.
    let err = sqlx::query(
        "INSERT INTO components (id, repository_id, format, name) VALUES ($1, $2, 'maven2', 'left-pad')",
    )
    .bind(ComponentId::generate().as_uuid())
    .bind(repo.id.as_uuid())
    .execute(store.pool())
    .await
    .unwrap_err();
    assert!(
        err.as_database_error()
            .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_find_or_create_converges_on_one_row() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (_, repo) = seed_repository(&store).await;
    let new = new_component(repo.id, Some("org.race"), "lib", Some("9.9"));
    let mut tasks = JoinSet::new();
    for _ in 0..32 {
        let components = store.components.clone();
        let new = new.clone();
        tasks.spawn(async move { components.find_or_create(&new).await.unwrap().id });
    }
    let mut ids = HashSet::new();
    while let Some(id) = tasks.join_next().await {
        ids.insert(id.unwrap());
    }
    assert_eq!(ids.len(), 1);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM components")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(rows, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn list_filters_and_paginates_by_keyset() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (_, repo) = seed_repository(&store).await;
    let (_, other_repo) = seed_repository(&store).await;
    for i in 0..25 {
        store
            .components
            .find_or_create(&new_component(
                repo.id,
                Some("g"),
                &format!("n{i:02}"),
                Some("1"),
            ))
            .await
            .unwrap();
    }
    store
        .components
        .find_or_create(&new_component(
            other_repo.id,
            Some("g"),
            "elsewhere",
            Some("1"),
        ))
        .await
        .unwrap();

    let filter = ComponentFilter {
        repository_id: Some(repo.id),
        ..ComponentFilter::default()
    };
    let mut seen = Vec::new();
    let mut after = None;
    let mut pages = 0;
    loop {
        let page = store
            .components
            .list(
                &filter,
                &PageRequest {
                    limit: 10,
                    after: after.clone(),
                },
            )
            .await
            .unwrap();
        pages += 1;
        seen.extend(page.items.iter().map(|c| c.id));
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(seen.len(), 25);
    assert_eq!(seen.iter().collect::<HashSet<_>>().len(), 25);
    let mut sorted = seen.clone();
    sorted.sort();
    assert_eq!(seen, sorted);

    let exact = ComponentFilter {
        repository_id: Some(repo.id),
        group_id: Some("g".to_owned()),
        name: Some("n03".to_owned()),
        version: Some("1".to_owned()),
        query: None,
    };
    let page = store
        .components
        .list(&exact, &PageRequest::default())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(page.next.is_none());

    let everywhere = store
        .components
        .list(
            &ComponentFilter::default(),
            &PageRequest {
                limit: 100,
                after: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(everywhere.items.len(), 26);
}

#[tokio::test(flavor = "multi_thread")]
async fn query_matches_words_prefixes_and_substrings_but_not_noise() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (_, repo) = seed_repository(&store).await;
    for (group, name, version) in [
        (Some("com.google.guava"), "guava", Some("32.1.2-jre")),
        (Some("org.apache.commons"), "commons-lang3", Some("3.14.0")),
        (None, "left-pad", Some("1.3.0")),
    ] {
        store
            .components
            .find_or_create(&new_component(repo.id, group, name, version))
            .await
            .unwrap();
    }
    let search = |q: &str| {
        let filter = ComponentFilter {
            query: Some(q.to_owned()),
            ..ComponentFilter::default()
        };
        let components = store.components.clone();
        async move {
            components
                .list(&filter, &PageRequest::default())
                .await
                .unwrap()
                .items
                .into_iter()
                .map(|c| c.name)
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(search("guava").await, vec!["guava"]);
    assert_eq!(search("com.google").await, vec!["guava"], "group words");
    assert_eq!(search("32.1.2").await, vec!["guava"], "version words");
    assert_eq!(
        search("guav").await,
        vec!["guava"],
        "substring through the trigram index"
    );
    assert_eq!(
        search("COMMONS").await,
        vec!["commons-lang3"],
        "case insensitive"
    );
    assert_eq!(search("lang").await, vec!["commons-lang3"]);
    assert_eq!(search("pad").await, vec!["left-pad"]);
    assert!(search("nothing-like-this").await.is_empty());
    assert_eq!(search("   ").await.len(), 3, "a blank query is no filter");
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_treat_query_text_as_sql_or_as_a_like_pattern() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (_, repo) = seed_repository(&store).await;
    store
        .components
        .find_or_create(&new_component(repo.id, None, "plain", Some("7")))
        .await
        .unwrap();
    for hostile in [
        "'; DROP TABLE components; --",
        "%",
        "_",
        "\\",
        "zzz' OR '1'='1",
        "a%b_c\\d",
    ] {
        let filter = ComponentFilter {
            query: Some(hostile.to_owned()),
            ..ComponentFilter::default()
        };
        let page = store
            .components
            .list(&filter, &PageRequest::default())
            .await
            .unwrap();
        assert!(page.items.is_empty(), "{hostile} must match nothing");
    }
    let still_there: i64 = sqlx::query_scalar("SELECT count(*) FROM components")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(still_there, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_cascades_to_assets_and_reports_missing() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let component = store
        .components
        .find_or_create(&new_component(repo.id, None, "c", Some("1")))
        .await
        .unwrap();
    let mut new = new_asset(repo.id, "c-1.jar", blob(bs.id, 5));
    new.component_id = Some(component.id);
    store.assets.publish(&new).await.unwrap();

    store.components.delete(component.id).await.unwrap();
    assert!(
        store
            .assets
            .get_by_path(repo.id, "c-1.jar")
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        store.components.delete(component.id).await.unwrap_err(),
        StoreError::NotFound(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn component_in_a_missing_repository_is_refused() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let ghost = new_component(rustryx_core::ids::RepositoryId::generate(), None, "x", None);
    let err = store.components.find_or_create(&ghost).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(ref m) if m == "repository does not exist"),
        "{err:?}"
    );
}

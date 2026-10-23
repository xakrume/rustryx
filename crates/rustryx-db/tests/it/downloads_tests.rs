//! `touch_fresh` and `record_downloads`: the cheap statistics writes.

use std::time::Duration;

use rustryx_core::StoreError;
use rustryx_core::ids::AssetId;
use rustryx_core::model::DownloadTick;
use rustryx_core::ports::{AssetRepo, ComponentRepo};
use time::Duration as Span;

use crate::fixtures::{blob, new_asset, new_component, now, seed_repository};
use crate::support::TestDb;

fn tick(asset: AssetId, at: time::OffsetDateTime) -> DownloadTick {
    DownloadTick {
        asset_id: asset,
        at,
        count: 1,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn touch_fresh_moves_updated_at_forward_only_and_leaves_the_blob_alone() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let record = blob(bs.id, 3);
    let asset = store
        .assets
        .publish(&new_asset(repo.id, "cached", record.clone()))
        .await
        .unwrap()
        .asset;

    let later = now() + Span::minutes(10);
    store.assets.touch_fresh(asset.id, later).await.unwrap();
    let after = store.assets.get(asset.id).await.unwrap().unwrap();
    assert_eq!(after.updated_at, later);
    assert_eq!(
        after.blob_id, record.id,
        "a revalidation does not touch the content"
    );

    // A stale 304 (an older instant) must not rewind the freshness another node set.
    store
        .assets
        .touch_fresh(asset.id, now() - Span::minutes(10))
        .await
        .unwrap();
    assert_eq!(
        store
            .assets
            .get(asset.id)
            .await
            .unwrap()
            .unwrap()
            .updated_at,
        later
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn touch_fresh_reports_a_missing_asset_as_not_found() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let err = store
        .assets
        .touch_fresh(AssetId::generate(), now())
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn record_downloads_sets_asset_and_component_times_and_ignores_unknown_assets() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let component = store
        .components
        .find_or_create(&new_component(repo.id, None, "lib", Some("1")))
        .await
        .unwrap();
    let mut with_component = new_asset(repo.id, "lib-1.jar", blob(bs.id, 1));
    with_component.component_id = Some(component.id);
    let jar = store.assets.publish(&with_component).await.unwrap().asset;
    let pom = {
        let mut other = new_asset(repo.id, "lib-1.pom", blob(bs.id, 1));
        other.component_id = Some(component.id);
        store.assets.publish(&other).await.unwrap().asset
    };
    let loose = store
        .assets
        .publish(&new_asset(repo.id, "loose", blob(bs.id, 1)))
        .await
        .unwrap()
        .asset;

    let t1 = now() + Span::hours(1);
    let t2 = t1 + Span::minutes(5);
    store
        .assets
        .record_downloads(&[
            tick(jar.id, t1),
            // Two ticks of one asset: the newer wins whatever the order.
            tick(pom.id, t2),
            tick(pom.id, t1),
            tick(loose.id, t1),
            tick(AssetId::generate(), t1),
        ])
        .await
        .unwrap();

    let read = |id: AssetId| {
        let assets = store.assets.clone();
        async move { assets.get(id).await.unwrap().unwrap().last_downloaded_at }
    };
    assert_eq!(read(jar.id).await, Some(t1));
    assert_eq!(read(pom.id).await, Some(t2));
    assert_eq!(read(loose.id).await, Some(t1));
    let c = store.components.get(component.id).await.unwrap().unwrap();
    assert_eq!(
        c.last_downloaded_at,
        Some(t2),
        "the component takes the newest of its assets"
    );

    store.assets.record_downloads(&[]).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn record_downloads_rewrites_a_time_only_when_it_lags_by_more_than_the_interval() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let store = rustryx_db::PgStore::new(pool.clone(), crate::support::test_sealer());
    let assets = rustryx_db::repos::PgAssetRepo::new(pool)
        .with_download_write_interval(Duration::from_secs(60));
    let (bs, repo) = seed_repository(&store).await;
    let asset = assets
        .publish(&new_asset(repo.id, "hot", blob(bs.id, 1)))
        .await
        .unwrap()
        .asset;

    let base = now() + Span::hours(1);
    assets
        .record_downloads(&[tick(asset.id, base)])
        .await
        .unwrap();
    // 30 seconds later: inside the interval, not rewritten.
    assets
        .record_downloads(&[tick(asset.id, base + Span::seconds(30))])
        .await
        .unwrap();
    assert_eq!(
        assets
            .get(asset.id)
            .await
            .unwrap()
            .unwrap()
            .last_downloaded_at,
        Some(base)
    );
    // 90 seconds later: rewritten.
    let far = base + Span::seconds(90);
    assets
        .record_downloads(&[tick(asset.id, far)])
        .await
        .unwrap();
    assert_eq!(
        assets
            .get(asset.id)
            .await
            .unwrap()
            .unwrap()
            .last_downloaded_at,
        Some(far)
    );
    // An older time never moves it back.
    assets
        .record_downloads(&[tick(asset.id, base)])
        .await
        .unwrap();
    assert_eq!(
        assets
            .get(asset.id)
            .await
            .unwrap()
            .unwrap()
            .last_downloaded_at,
        Some(far)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn overlapping_flushes_from_two_nodes_do_not_deadlock() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let mut ids = Vec::new();
    for n in 0..40 {
        let asset = store
            .assets
            .publish(&new_asset(repo.id, &format!("p{n:02}"), blob(bs.id, 1)))
            .await
            .unwrap()
            .asset;
        ids.push(asset.id);
    }
    let mut tasks = tokio::task::JoinSet::new();
    for node in 0..6_i64 {
        let assets = store.assets.clone();
        let mut batch: Vec<DownloadTick> = ids
            .iter()
            .map(|id| tick(*id, now() + Span::hours(2) + Span::seconds(node * 100)))
            .collect();
        // Each node sends its batch in a different order.
        if node % 2 == 0 {
            batch.reverse();
        }
        tasks.spawn(async move {
            for _ in 0..5 {
                assets.record_downloads(&batch).await.unwrap();
            }
        });
    }
    while let Some(done) = tasks.join_next().await {
        done.unwrap();
    }
    let newest = store
        .assets
        .get(ids[0])
        .await
        .unwrap()
        .unwrap()
        .last_downloaded_at;
    assert!(newest.is_some());
}

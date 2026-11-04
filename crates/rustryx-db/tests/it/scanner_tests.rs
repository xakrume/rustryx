//! Scanner registration: unique names, one vulnerability default, probes that survive edits.

use rustryx_core::StoreError;
use rustryx_core::ids::ScannerId;
use rustryx_core::ports::ScannerRepo;
use rustryx_core::scan::{ProbeResult, ScannerKind, ScannerRecord};
use time::OffsetDateTime;

use crate::support::{TestDb, test_sealer};

fn scanner(name: &str) -> ScannerRecord {
    ScannerRecord::new(
        ScannerId::generate(),
        name,
        ScannerKind::Trivy,
        OffsetDateTime::now_utc(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn scanners_are_unique_and_only_one_vulnerability_default_is_kept() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let scanners = rustryx_db::repos::PgScannerRepo::new(store.pool().clone(), test_sealer());
    let first = scanner("trivy-a");
    scanners.create(&first).await.unwrap();
    assert!(matches!(
        scanners.create(&scanner("trivy-a")).await,
        Err(StoreError::Conflict(_))
    ));
    let mut bad = scanner("bad");
    bad.endpoint = Some("https://x.example.test".to_owned());
    assert!(matches!(
        scanners.create(&bad).await,
        Err(StoreError::Conflict(_))
    ));

    let mut default_one = scanner("default-one");
    default_one.is_default = true;
    scanners.create(&default_one).await.unwrap();
    let mut default_two = scanner("default-two");
    default_two.is_default = true;
    assert!(matches!(
        scanners.create(&default_two).await,
        Err(StoreError::Conflict(_))
    ));
    scanners.create(&scanner("default-two")).await.unwrap();
    let two = scanners
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.name == "default-two")
        .unwrap();
    scanners.set_default(two.id).await.unwrap();
    let defaults: Vec<String> = scanners
        .list()
        .await
        .unwrap()
        .into_iter()
        .filter(|row| row.is_default)
        .map(|row| row.name)
        .collect();
    assert_eq!(defaults, ["default-two"]);

    let mut edited = first.clone();
    edited.enabled = false;
    edited.is_default = true;
    assert!(matches!(
        scanners.update(&edited, 1).await,
        Err(StoreError::Conflict(_))
    ));
    edited.is_default = false;
    scanners.update(&edited, 1).await.unwrap();
    assert_eq!(
        scanners.get(first.id).await.unwrap().unwrap().enabled,
        false
    );

    let probe = ProbeResult {
        at: OffsetDateTime::now_utc(),
        ok: true,
        version: Some("0.50".to_owned()),
        message: "ready".to_owned(),
    };
    scanners.record_probe(first.id, &probe).await.unwrap();
    let mut again = scanners.get(first.id).await.unwrap().unwrap();
    again.enabled = true;
    scanners.update(&again, again.version).await.unwrap();
    let kept = scanners.get(first.id).await.unwrap().unwrap();
    assert_eq!(kept.last_probe.unwrap().message, "ready");
    scanners.delete(first.id).await.unwrap();
    assert!(matches!(
        scanners.delete(first.id).await,
        Err(StoreError::NotFound(_))
    ));
}

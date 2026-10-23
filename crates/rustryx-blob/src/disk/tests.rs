use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::testing::FakeDisk;
use super::*;

fn usage(total: u64, free: u64) -> DiskUsage {
    DiskUsage {
        total_bytes: total,
        free_bytes: free,
    }
}

/// Waits until `condition` holds, for at most a few seconds.
async fn eventually(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[test]
fn the_floor_is_inclusive_and_never_trips_without_a_size() {
    let disk = usage(1000, 0);
    assert!(
        !disk.below_floor(100, 10),
        "exactly at the floor is allowed"
    );
    assert!(disk.below_floor(99, 10));
    assert!(!disk.below_floor(0, 0), "a floor of zero never trips");
    assert!(!usage(0, 0).below_floor(0, 90), "no size, nothing to judge");
    // Products of two u64 values do not wrap.
    assert!(!usage(u64::MAX, 0).below_floor(u64::MAX, 90));
    assert!(usage(u64::MAX, 0).below_floor(u64::MAX / 100, 90));
}

#[test]
fn the_statvfs_probe_reads_a_real_file_system() {
    let dir = tempfile::tempdir().unwrap();
    let usage = StatvfsProbe.usage(dir.path()).unwrap();
    assert!(usage.total_bytes > 0);
    assert!(usage.free_bytes <= usage.total_bytes);
}

#[test]
fn the_statvfs_probe_reports_a_missing_path_as_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("not-there");
    assert!(StatvfsProbe.usage(&missing).is_err());
}

#[test]
fn a_guard_without_a_sample_or_without_a_floor_allows_writes() {
    let blind = DiskGuard::new(10);
    assert!(blind.check(u64::MAX).is_ok(), "no sample, no judgement");
    let off = DiskGuard::new(0);
    off.record(usage(1000, 0));
    assert!(off.check(1).is_ok(), "a floor of zero is off");
}

#[test]
fn must_not_accept_a_write_that_would_cross_the_floor() {
    let guard = DiskGuard::new(10);
    guard.record(usage(1000, 300));
    assert!(guard.check(200).is_ok(), "leaves exactly 100, the floor");
    assert!(matches!(
        guard.check(201),
        Err(BlobError::InsufficientStorage)
    ));
}

#[test]
fn bytes_accepted_since_the_sample_count_against_the_free_space() {
    let guard = DiskGuard::new(10);
    guard.record(usage(1000, 300));
    guard.note_written(150);
    guard.note_written(40);
    assert!(guard.check(10).is_ok());
    assert!(matches!(
        guard.check(11),
        Err(BlobError::InsufficientStorage)
    ));
    // A new sample is the truth again.
    guard.record(usage(1000, 600));
    assert!(guard.check(400).is_ok());
}

#[tokio::test]
async fn the_monitor_samples_before_the_store_opens_and_feeds_the_guard() {
    let disk = Arc::new(FakeDisk::new(1000, 500));
    let guard = Arc::new(DiskGuard::new(10));
    let monitor = DiskMonitor::start(
        PathBuf::from("/unused"),
        Arc::clone(&disk) as Arc<dyn DiskProbe>,
        Arc::clone(&guard),
        StoreMetrics::new("disk-monitor-test"),
        Duration::from_secs(3600),
    )
    .unwrap();
    monitor.first_sample().await;
    assert!(guard.check(400).is_ok());
    assert!(guard.check(401).is_err());
    assert_eq!(disk.calls(), 1);
}

#[tokio::test]
async fn refresh_returns_only_after_a_probe_that_started_after_the_request() {
    let disk = Arc::new(FakeDisk::new(1000, 500));
    let guard = Arc::new(DiskGuard::new(10));
    let monitor = DiskMonitor::start(
        PathBuf::from("/unused"),
        Arc::clone(&disk) as Arc<dyn DiskProbe>,
        Arc::clone(&guard),
        StoreMetrics::new("disk-refresh-test"),
        Duration::from_secs(3600),
    )
    .unwrap();
    monitor.first_sample().await;
    for free in [100, 900, 100, 900] {
        disk.set(1000, free);
        monitor.refresh().await;
        assert_eq!(guard.check(1).is_err(), free == 100, "free {free}");
    }
}

#[tokio::test]
async fn a_failing_probe_keeps_the_last_sample_and_recovers() {
    let disk = Arc::new(FakeDisk::new(1000, 500));
    let guard = Arc::new(DiskGuard::new(10));
    let monitor = DiskMonitor::start(
        PathBuf::from("/unused"),
        Arc::clone(&disk) as Arc<dyn DiskProbe>,
        Arc::clone(&guard),
        StoreMetrics::new("disk-failing-test"),
        Duration::from_secs(3600),
    )
    .unwrap();
    monitor.first_sample().await;

    disk.fail(true);
    disk.set(1000, 0);
    monitor.refresh().await;
    assert!(
        guard.check(400).is_ok(),
        "a failed probe must not replace the last good sample"
    );

    disk.fail(false);
    monitor.refresh().await;
    assert!(guard.check(1).is_err(), "the recovered probe is believed");
}

#[tokio::test]
async fn a_probe_that_fails_from_the_start_leaves_the_floor_open() {
    let disk = Arc::new(FakeDisk::new(1000, 0));
    disk.fail(true);
    let guard = Arc::new(DiskGuard::new(10));
    let monitor = DiskMonitor::start(
        PathBuf::from("/unused"),
        Arc::clone(&disk) as Arc<dyn DiskProbe>,
        Arc::clone(&guard),
        StoreMetrics::new("disk-blind-test"),
        Duration::from_secs(3600),
    )
    .unwrap();
    monitor.first_sample().await;
    assert!(guard.check(u64::MAX / 2).is_ok());
}

#[tokio::test]
async fn the_monitor_samples_on_its_own_interval() {
    let disk = Arc::new(FakeDisk::new(1000, 500));
    let guard = Arc::new(DiskGuard::new(10));
    let _monitor = DiskMonitor::start(
        PathBuf::from("/unused"),
        Arc::clone(&disk) as Arc<dyn DiskProbe>,
        guard,
        StoreMetrics::new("disk-interval-test"),
        Duration::from_millis(10),
    )
    .unwrap();
    let watched = Arc::clone(&disk);
    eventually("three probes", move || watched.calls() >= 3).await;
}

#[tokio::test]
async fn must_not_leave_the_thread_running_after_the_monitor_is_dropped() {
    let disk = Arc::new(FakeDisk::new(1000, 500));
    let guard = Arc::new(DiskGuard::new(10));
    let monitor = DiskMonitor::start(
        PathBuf::from("/unused"),
        Arc::clone(&disk) as Arc<dyn DiskProbe>,
        guard,
        StoreMetrics::new("disk-drop-test"),
        Duration::from_millis(10),
    )
    .unwrap();
    monitor.first_sample().await;
    assert!(Arc::strong_count(&disk) > 1, "the thread holds the probe");
    drop(monitor);
    // The thread releases its clone of the probe when it exits.
    let watched = Arc::clone(&disk);
    eventually("the thread to exit", move || {
        Arc::strong_count(&watched) == 2
    })
    .await;
    let calls = disk.calls();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(disk.calls(), calls, "no probe after the thread ended");
}

#[tokio::test]
async fn stop_is_idempotent_and_refresh_after_stop_returns_at_once() {
    let disk = Arc::new(FakeDisk::new(1000, 500));
    let guard = Arc::new(DiskGuard::new(10));
    let monitor = DiskMonitor::start(
        PathBuf::from("/unused"),
        Arc::clone(&disk) as Arc<dyn DiskProbe>,
        guard,
        StoreMetrics::new("disk-stop-test"),
        Duration::from_secs(3600),
    )
    .unwrap();
    monitor.first_sample().await;
    monitor.stop();
    monitor.stop();
    let started = Instant::now();
    monitor.refresh().await;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "refresh on a stopped monitor must not wait"
    );
}

//! Samples of the tokio runtime and the process itself, published as gauges.
//!
//! Both are pull-style values (the runtime and the kernel keep the counters), so a task
//! reads them every few seconds and sets gauges. `tokio_blocking_queue_depth` is not among
//! them on purpose: the runtime hides it unless the whole build is compiled with
//! `--cfg tokio_unstable`, and a build-wide flag is not worth one panel.
//!
//! The process series come from `/proc` and exist on Linux only (the image's platform);
//! elsewhere the sampler publishes the runtime gauges and no process series, which is the
//! honest answer for a developer laptop. The files are read through `tokio::fs`, never
//! with `std::fs` on the runtime.

use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::names;
use crate::prometheus::MetricsHandle;

/// Kernel clock ticks per second in `/proc/<pid>/stat`. `USER_HZ` is part of the Linux
/// ABI and 100 on every architecture; `sysconf(_SC_CLK_TCK)` would need `unsafe`.
const USER_HZ: f64 = 100.0;

/// One reading of the process, parsed from `/proc/self`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessSnapshot {
    /// User plus system CPU time in seconds.
    pub cpu_seconds: f64,
    /// Resident set size in bytes.
    pub resident_bytes: u64,
    /// Virtual memory size in bytes.
    pub virtual_bytes: u64,
    /// Soft limit of open files, `None` for `unlimited`.
    pub max_fds: Option<u64>,
}

/// CPU seconds from the text of `/proc/self/stat`: utime and stime, fields 14 and 15.
///
/// The second field is the command name in parentheses and may itself hold spaces and
/// parentheses, so the fields are counted from the last closing parenthesis.
#[must_use]
pub fn parse_cpu_seconds(stat: &str) -> Option<f64> {
    let rest = stat.get(stat.rfind(')')? + 1..)?;
    // `rest` starts with field 3 (state), so utime (field 14) is at index 11.
    let mut fields = rest.split_whitespace().skip(11);
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    // Whole ticks, far below 2^53: the conversion is exact.
    #[allow(clippy::cast_precision_loss)]
    Some((utime + stime) as f64 / USER_HZ)
}

/// A `kB` value of `/proc/self/status`, for example `VmRSS`, in bytes.
#[must_use]
pub fn parse_status_bytes(status: &str, key: &str) -> Option<u64> {
    let line = status.lines().find(|l| l.starts_with(key))?;
    let kb: u64 = line[key.len()..].split_whitespace().next()?.parse().ok()?;
    kb.checked_mul(1024)
}

/// The soft `Max open files` limit of `/proc/self/limits`. `Some(None)` is `unlimited`.
#[must_use]
pub fn parse_max_fds(limits: &str) -> Option<Option<u64>> {
    let line = limits.lines().find(|l| l.starts_with("Max open files"))?;
    let soft = line["Max open files".len()..].split_whitespace().next()?;
    if soft == "unlimited" {
        Some(None)
    } else {
        soft.parse().ok().map(Some)
    }
}

impl ProcessSnapshot {
    /// Builds a snapshot from the three `/proc/self` files.
    #[must_use]
    pub fn from_proc(stat: &str, status: &str, limits: &str) -> Option<Self> {
        Some(Self {
            cpu_seconds: parse_cpu_seconds(stat)?,
            resident_bytes: parse_status_bytes(status, "VmRSS:")?,
            virtual_bytes: parse_status_bytes(status, "VmSize:")?,
            max_fds: parse_max_fds(limits)?,
        })
    }
}

/// Reads `/proc/self`. `None` where there is no such directory (not Linux).
async fn read_process() -> Option<(ProcessSnapshot, u64)> {
    let stat = tokio::fs::read_to_string("/proc/self/stat").await.ok()?;
    let status = tokio::fs::read_to_string("/proc/self/status").await.ok()?;
    let limits = tokio::fs::read_to_string("/proc/self/limits").await.ok()?;
    let snapshot = ProcessSnapshot::from_proc(&stat, &status, &limits)?;
    let mut entries = tokio::fs::read_dir("/proc/self/fd").await.ok()?;
    let mut open = 0_u64;
    while entries.next_entry().await.ok()?.is_some() {
        open += 1;
    }
    // The directory handle used for the listing is itself one of the entries.
    Some((snapshot, open.saturating_sub(1)))
}

/// Publishes one process reading.
pub fn publish_process(metrics: &MetricsHandle, snapshot: &ProcessSnapshot, open_fds: u64) {
    metrics
        .gauge(names::PROCESS_CPU_SECONDS_TOTAL, &[])
        .set(snapshot.cpu_seconds);
    // Memory sizes and descriptor counts are far below 2^53 (the exact range of an f64).
    #[allow(clippy::cast_precision_loss)]
    {
        metrics
            .gauge(names::PROCESS_RESIDENT_MEMORY_BYTES, &[])
            .set(snapshot.resident_bytes as f64);
        metrics
            .gauge(names::PROCESS_VIRTUAL_MEMORY_BYTES, &[])
            .set(snapshot.virtual_bytes as f64);
        metrics
            .gauge(names::PROCESS_OPEN_FDS, &[])
            .set(open_fds as f64);
        if let Some(max) = snapshot.max_fds {
            metrics.gauge(names::PROCESS_MAX_FDS, &[]).set(max as f64);
        }
    }
    metrics
        .gauge(names::PROCESS_START_TIME_SECONDS, &[])
        .set(metrics.process_start_unix());
}

/// What the runtime reported at one instant, kept to compute a busy ratio between samples.
struct RuntimeReading {
    at: Instant,
    busy: Duration,
}

fn read_runtime(handle: &tokio::runtime::Handle) -> (RuntimeReading, usize, usize, usize) {
    let runtime = handle.metrics();
    let workers = runtime.num_workers();
    #[cfg(target_has_atomic = "64")]
    let busy = (0..workers)
        .map(|worker| runtime.worker_total_busy_duration(worker))
        .sum();
    #[cfg(not(target_has_atomic = "64"))]
    let busy = Duration::ZERO;
    (
        RuntimeReading {
            at: Instant::now(),
            busy,
        },
        workers,
        runtime.global_queue_depth(),
        runtime.num_alive_tasks(),
    )
}

/// Share of worker time that was busy between two readings, clamped to 0..=1.
#[must_use]
pub fn busy_ratio(busy: Duration, elapsed: Duration, workers: usize) -> f64 {
    let capacity = elapsed.as_secs_f64() * f64::from(u32::try_from(workers).unwrap_or(u32::MAX));
    if capacity <= 0.0 {
        return 0.0;
    }
    (busy.as_secs_f64() / capacity).clamp(0.0, 1.0)
}

/// Samples the runtime of the calling task and the process every `interval` until `cancel`
/// fires. Spawn it inside the runtime it should describe.
pub async fn run_runtime_sampler(
    metrics: MetricsHandle,
    interval: Duration,
    cancel: CancellationToken,
) {
    let handle = tokio::runtime::Handle::current();
    let (mut last, ..) = read_runtime(&handle);
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = ticker.tick() => {}
        }
        let (now, workers, queue, alive) = read_runtime(&handle);
        let ratio = busy_ratio(
            now.busy.saturating_sub(last.busy),
            now.at.saturating_duration_since(last.at),
            workers,
        );
        last = now;
        // Counts of workers and tasks are far below 2^53.
        #[allow(clippy::cast_precision_loss)]
        {
            metrics.gauge(names::TOKIO_WORKERS, &[]).set(workers as f64);
            metrics
                .gauge(names::TOKIO_GLOBAL_QUEUE_DEPTH, &[])
                .set(queue as f64);
            metrics
                .gauge(names::TOKIO_ALIVE_TASKS, &[])
                .set(alive as f64);
        }
        metrics.gauge(names::TOKIO_BUSY_RATIO, &[]).set(ratio);
        if let Some((snapshot, open)) = read_process().await {
            publish_process(&metrics, &snapshot, open);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "4242 (rust (ry)x worker) S 1 4242 4242 0 -1 4194560 1000 0 0 0 \
                        1234 566 0 0 20 0 9 0 100 123456789 5000 18446744073709551615";

    #[test]
    fn cpu_time_is_counted_from_the_last_parenthesis_so_odd_names_do_not_shift_fields() {
        let seconds = parse_cpu_seconds(STAT).expect("parses");
        assert!((seconds - 18.0).abs() < 1e-9, "{seconds}");
        assert!(parse_cpu_seconds("no parenthesis at all").is_none());
        assert!(parse_cpu_seconds("1 (x) S 1 2").is_none(), "too few fields");
        assert!(parse_cpu_seconds("1 (x) S a b c d e f g h i j k l").is_none());
    }

    #[test]
    fn status_values_are_kibibytes_converted_to_bytes() {
        let status = "Name:\trustryx\nVmSize:\t  204800 kB\nVmRSS:\t   51200 kB\nThreads:\t9\n";
        assert_eq!(parse_status_bytes(status, "VmRSS:"), Some(51_200 * 1024));
        assert_eq!(parse_status_bytes(status, "VmSize:"), Some(204_800 * 1024));
        assert_eq!(parse_status_bytes(status, "VmSwap:"), None);
        assert_eq!(parse_status_bytes("VmRSS:\tabc kB", "VmRSS:"), None);
    }

    #[test]
    fn the_soft_descriptor_limit_is_the_first_number_and_unlimited_is_a_state() {
        let limits = "Limit                     Soft Limit           Hard Limit           Units\n\
                      Max open files            1024                 1048576              files\n";
        assert_eq!(parse_max_fds(limits), Some(Some(1024)));
        let unlimited =
            "Max open files            unlimited            unlimited            files\n";
        assert_eq!(parse_max_fds(unlimited), Some(None));
        assert_eq!(
            parse_max_fds("Max cpu time  unlimited unlimited seconds\n"),
            None
        );
    }

    #[test]
    fn a_snapshot_needs_all_three_files() {
        let status = "VmSize:\t10 kB\nVmRSS:\t5 kB\n";
        let limits = "Max open files            8                    16                   files\n";
        let snapshot = ProcessSnapshot::from_proc(STAT, status, limits).expect("complete");
        assert_eq!(snapshot.resident_bytes, 5 * 1024);
        assert_eq!(snapshot.max_fds, Some(8));
        assert!(ProcessSnapshot::from_proc(STAT, "VmSize:\t10 kB\n", limits).is_none());
        assert!(ProcessSnapshot::from_proc("garbage", status, limits).is_none());
    }

    #[test]
    fn publishing_sets_every_process_series() {
        let metrics = MetricsHandle::new().expect("recorder");
        let snapshot = ProcessSnapshot {
            cpu_seconds: 12.5,
            resident_bytes: 1000,
            virtual_bytes: 2000,
            max_fds: Some(1024),
        };
        publish_process(&metrics, &snapshot, 17);
        let text = metrics.render();
        for expected in [
            "process_cpu_seconds_total 12.5",
            "process_resident_memory_bytes 1000",
            "process_virtual_memory_bytes 2000",
            "process_open_fds 17",
            "process_max_fds 1024",
            "process_start_time_seconds ",
        ] {
            assert!(text.contains(expected), "missing {expected}\n{text}");
        }
        let unlimited = MetricsHandle::new().expect("recorder");
        publish_process(
            &unlimited,
            &ProcessSnapshot {
                max_fds: None,
                ..snapshot
            },
            1,
        );
        assert!(
            !unlimited.render().contains("process_max_fds"),
            "no limit, no series"
        );
    }

    #[test]
    fn the_busy_ratio_is_a_share_of_worker_time_and_never_leaves_zero_to_one() {
        let second = Duration::from_secs(1);
        assert!((busy_ratio(Duration::from_millis(500), second, 1) - 0.5).abs() < 1e-9);
        assert!((busy_ratio(Duration::from_secs(2), second, 4) - 0.5).abs() < 1e-9);
        assert!((busy_ratio(Duration::from_secs(9), second, 1) - 1.0).abs() < f64::EPSILON);
        assert!(busy_ratio(second, Duration::ZERO, 4).abs() < f64::EPSILON);
        assert!(busy_ratio(second, second, 0).abs() < f64::EPSILON);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_sampler_publishes_runtime_gauges_and_stops_on_cancel() {
        let metrics = MetricsHandle::new().expect("recorder");
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run_runtime_sampler(
            metrics.clone(),
            Duration::from_millis(10),
            cancel.clone(),
        ));
        let mut text = String::new();
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            text = metrics.render();
            if text.contains("tokio_busy_ratio") {
                break;
            }
        }
        for expected in [
            "tokio_workers 2",
            "tokio_global_queue_depth",
            "tokio_alive_tasks",
            "tokio_busy_ratio",
        ] {
            assert!(text.contains(expected), "missing {expected}\n{text}");
        }
        assert!(
            !text.contains("tokio_blocking_queue_depth"),
            "not exported, see the module docs"
        );
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("stops in time")
            .expect("no panic");
    }
}

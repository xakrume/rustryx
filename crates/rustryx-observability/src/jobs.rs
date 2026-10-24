//! Metrics of background tasks: runs, running state and last success.
//!
//! The label is `task`, not `job`: a series label named `job` collides with the scrape
//! label and Prometheus would rename it to `exported_job`.

use crate::names;
use crate::prometheus::MetricsHandle;

/// Result label of a finished run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobResult {
    /// The run finished and did its work.
    Ok,
    /// The run failed.
    Error,
    /// The run had nothing to do or another node held the lock.
    Skipped,
}

impl JobResult {
    /// The label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Skipped => "skipped",
        }
    }
}

/// The series of one background task.
#[derive(Debug, Clone)]
pub struct JobMetrics {
    metrics: MetricsHandle,
    task: String,
}

impl JobMetrics {
    pub(crate) fn new(metrics: MetricsHandle, task: &str) -> Self {
        // Initialised to the process start, not to "now" and not left absent: a task that
        // never ran must go stale, which needs a series to compare against.
        metrics
            .gauge(
                names::BACKGROUND_JOB_LAST_SUCCESS_TIMESTAMP_SECONDS,
                &[("task", task)],
            )
            .set(metrics.process_start_unix());
        metrics
            .gauge(names::BACKGROUND_JOB_RUNNING, &[("task", task)])
            .set(0.0);
        Self {
            metrics,
            task: task.to_owned(),
        }
    }

    /// A run began.
    pub fn started(&self) {
        self.metrics
            .gauge(names::BACKGROUND_JOB_RUNNING, &[("task", &self.task)])
            .set(1.0);
    }

    /// A run ended. A successful one also moves the last-success time.
    pub fn finished(&self, result: JobResult) {
        self.metrics
            .gauge(names::BACKGROUND_JOB_RUNNING, &[("task", &self.task)])
            .set(0.0);
        self.metrics
            .counter(
                names::BACKGROUND_JOB_RUNS_TOTAL,
                &[("task", &self.task), ("result", result.as_str())],
            )
            .increment(1);
        if result == JobResult::Ok {
            self.metrics
                .gauge(
                    names::BACKGROUND_JOB_LAST_SUCCESS_TIMESTAMP_SECONDS,
                    &[("task", &self.task)],
                )
                .set(MetricsHandle::unix_now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_starts_the_last_success_at_the_process_start_and_not_running() {
        let metrics = MetricsHandle::new().expect("recorder");
        let _job = metrics.job("blob_gc");
        let text = metrics.render();
        assert!(
            text.contains(r#"background_job_running{task="blob_gc"} 0"#),
            "{text}"
        );
        let line = text
            .lines()
            .find(|l| {
                l.starts_with(r#"background_job_last_success_timestamp_seconds{task="blob_gc"}"#)
            })
            .expect("series");
        let value: f64 = line
            .rsplit(' ')
            .next()
            .expect("value")
            .parse()
            .expect("number");
        assert!((value - metrics.process_start_unix()).abs() < 1.0, "{line}");
        assert!(
            !text.contains("exported_job") && !text.contains(r#"job=""#),
            "{text}"
        );
    }

    #[test]
    fn a_run_moves_running_runs_and_only_a_success_moves_the_last_success() {
        let metrics = MetricsHandle::new().expect("recorder");
        let job = metrics.job("cleanup");
        job.started();
        assert!(
            metrics
                .render()
                .contains(r#"background_job_running{task="cleanup"} 1"#)
        );
        job.finished(JobResult::Error);
        let after_error = metrics.render();
        assert!(after_error.contains(r#"background_job_running{task="cleanup"} 0"#));
        assert!(
            after_error.contains(r#"background_job_runs_total{task="cleanup",result="error"} 1"#)
        );
        let stamp = |text: &str| {
            text.lines()
                .find(|l| {
                    l.starts_with(
                        r#"background_job_last_success_timestamp_seconds{task="cleanup"}"#,
                    )
                })
                .and_then(|l| l.rsplit(' ').next().map(str::to_owned))
                .expect("series")
        };
        let before = stamp(&after_error);
        job.finished(JobResult::Skipped);
        assert_eq!(
            stamp(&metrics.render()),
            before,
            "a skipped run is not a success"
        );
        std::thread::sleep(std::time::Duration::from_millis(1100));
        job.finished(JobResult::Ok);
        let text = metrics.render();
        assert_ne!(stamp(&text), before);
        assert!(text.contains(r#"background_job_runs_total{task="cleanup",result="ok"} 1"#));
        assert!(text.contains(r#"background_job_runs_total{task="cleanup",result="skipped"} 1"#));
    }

    #[test]
    fn result_labels_are_a_fixed_vocabulary() {
        assert_eq!(JobResult::Ok.as_str(), "ok");
        assert_eq!(JobResult::Error.as_str(), "error");
        assert_eq!(JobResult::Skipped.as_str(), "skipped");
    }
}

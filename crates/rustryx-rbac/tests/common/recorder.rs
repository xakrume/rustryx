//! A `metrics` recorder that keeps counters in memory, for tests that assert what the
//! authorizer emitted.
//!
//! Install it with `metrics::set_default_local_recorder`. That recorder is per thread, so a
//! test that uses it must run on a current-thread runtime (the `#[tokio::test]` default),
//! where every task, including the invalidation listener, runs on the test thread.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use metrics::{Counter, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit};

/// Label set of one series, sorted by label name.
pub type Labels = BTreeMap<String, String>;

#[derive(Default)]
pub struct Capture {
    counters: Mutex<HashMap<(String, Labels), Arc<AtomicU64>>>,
}

fn labels_of(key: &Key) -> Labels {
    key.labels()
        .map(|label| (label.key().to_owned(), label.value().to_owned()))
        .collect()
}

fn labels(pairs: &[(&str, &str)]) -> Labels {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

impl Capture {
    /// The value of the counter `name` with exactly these labels, 0 when it was never touched.
    pub fn get(&self, name: &str, pairs: &[(&str, &str)]) -> u64 {
        self.counters
            .lock()
            .unwrap()
            .get(&(name.to_owned(), labels(pairs)))
            .map_or(0, |value| value.load(Ordering::SeqCst))
    }

    /// The sum of every series of `name`, whatever its labels.
    pub fn total(&self, name: &str) -> u64 {
        self.counters
            .lock()
            .unwrap()
            .iter()
            .filter(|((series, _), _)| series == name)
            .map(|(_, value)| value.load(Ordering::SeqCst))
            .sum()
    }

    /// Every series that was incremented at least once, as `(name, labels)`.
    pub fn series(&self) -> Vec<(String, Labels)> {
        self.counters
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, value)| value.load(Ordering::SeqCst) > 0)
            .map(|(key, _)| key.clone())
            .collect()
    }
}

impl Recorder for Capture {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
        let handle = self
            .counters
            .lock()
            .unwrap()
            .entry((key.name().to_owned(), labels_of(key)))
            .or_default()
            .clone();
        Counter::from_arc(handle)
    }

    fn register_gauge(&self, _: &Key, _: &Metadata<'_>) -> Gauge {
        Gauge::noop()
    }

    fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
        Histogram::noop()
    }
}

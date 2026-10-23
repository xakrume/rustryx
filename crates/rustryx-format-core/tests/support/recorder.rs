//! A recorder for the `metrics` facade, scoped to the current thread.
//!
//! `#[tokio::test]` runs on one thread and spawned tasks of its runtime stay on it, so a
//! thread local recorder sees everything the engine emits and nothing of the tests running
//! beside it. Install it first, keep the guard for the whole test.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use metrics::{
    Counter, CounterFn, Gauge, GaugeFn, Histogram, HistogramFn, Key, KeyName, LocalRecorderGuard,
    Metadata, Recorder, SharedString, Unit,
};

#[derive(Default)]
struct Series {
    counters: Mutex<HashMap<String, Arc<CounterCell>>>,
    gauges: Mutex<HashMap<String, Arc<GaugeCell>>>,
    histograms: Mutex<HashMap<String, Arc<HistogramCell>>>,
}

#[derive(Default)]
struct CounterCell(AtomicU64);

impl CounterFn for CounterCell {
    fn increment(&self, value: u64) {
        self.0.fetch_add(value, Ordering::SeqCst);
    }

    fn absolute(&self, value: u64) {
        self.0.fetch_max(value, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct GaugeCell(Mutex<f64>);

impl GaugeFn for GaugeCell {
    fn increment(&self, value: f64) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) += value;
    }

    fn decrement(&self, value: f64) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) -= value;
    }

    fn set(&self, value: f64) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = value;
    }
}

#[derive(Default)]
struct HistogramCell(Mutex<Vec<f64>>);

impl HistogramFn for HistogramCell {
    fn record(&self, value: f64) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(value);
    }
}

/// `name{a="1",b="2"}` with the labels sorted by key, so a test names a series without caring
/// about the order the code attached the labels in.
pub fn series_id(name: &str, labels: &[(&str, &str)]) -> String {
    if labels.is_empty() {
        return name.to_owned();
    }
    let sorted: BTreeMap<&str, &str> = labels.iter().copied().collect();
    let rendered: Vec<String> = sorted.iter().map(|(k, v)| format!("{k}=\"{v}\"")).collect();
    format!("{name}{{{}}}", rendered.join(","))
}

fn id_of(key: &Key) -> String {
    let labels: Vec<(&str, &str)> = key
        .labels()
        .map(|label| (label.key(), label.value()))
        .collect();
    series_id(key.name(), &labels)
}

/// Captures counters, gauges and histograms.
#[derive(Default)]
pub struct TestRecorder {
    series: Series,
}

impl TestRecorder {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Makes this the recorder of the current thread until the guard is dropped.
    pub fn install(&self) -> LocalRecorderGuard<'_> {
        metrics::set_default_local_recorder(self)
    }

    /// Value of a counter series, 0 when it was never emitted.
    pub fn counter(&self, name: &str, labels: &[(&str, &str)]) -> u64 {
        self.series
            .counters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&series_id(name, labels))
            .map_or(0, |cell| cell.0.load(Ordering::SeqCst))
    }

    /// Sum of a counter over every label set it was emitted with.
    pub fn counter_total(&self, name: &str) -> u64 {
        let prefix = format!("{name}{{");
        self.series
            .counters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(id, _)| id.as_str() == name || id.starts_with(&prefix))
            .map(|(_, cell)| cell.0.load(Ordering::SeqCst))
            .sum()
    }

    /// Last value of a gauge series, `None` when it was never emitted.
    pub fn gauge(&self, name: &str, labels: &[(&str, &str)]) -> Option<f64> {
        self.series
            .gauges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&series_id(name, labels))
            .map(|cell| *cell.0.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Number of samples of a histogram series.
    pub fn histogram_count(&self, name: &str, labels: &[(&str, &str)]) -> usize {
        self.series
            .histograms
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&series_id(name, labels))
            .map_or(0, |cell| {
                cell.0.lock().unwrap_or_else(PoisonError::into_inner).len()
            })
    }

    /// Every counter series that was emitted, with its value.
    pub fn counters(&self) -> BTreeMap<String, u64> {
        self.series
            .counters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(id, cell)| (id.clone(), cell.0.load(Ordering::SeqCst)))
            .collect()
    }
}

impl Recorder for TestRecorder {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
        let mut map = self
            .series
            .counters
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Counter::from_arc(Arc::clone(map.entry(id_of(key)).or_default()))
    }

    fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
        let mut map = self
            .series
            .gauges
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Gauge::from_arc(Arc::clone(map.entry(id_of(key)).or_default()))
    }

    fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
        let mut map = self
            .series
            .histograms
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Histogram::from_arc(Arc::clone(map.entry(id_of(key)).or_default()))
    }
}

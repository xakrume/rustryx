//! A metrics recorder for tests.
//!
//! It is installed as the PROCESS-WIDE recorder, once, because one series is emitted from
//! a thread the test does not own (the disk monitor): a thread-local recorder would never
//! see it. Tests therefore give every store a name of their own and read only their own
//! series, which keeps parallel tests apart.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use metrics::{
    Counter, CounterFn, Gauge, GaugeFn, Histogram, Key, KeyName, Metadata, Recorder, SharedString,
    Unit,
};

#[derive(Default)]
struct Cell(AtomicU64);

impl CounterFn for Cell {
    fn increment(&self, value: u64) {
        self.0.fetch_add(value, Ordering::SeqCst);
    }

    fn absolute(&self, value: u64) {
        self.0.fetch_max(value, Ordering::SeqCst);
    }
}

impl GaugeFn for Cell {
    fn increment(&self, value: f64) {
        self.set(f64::from_bits(self.0.load(Ordering::SeqCst)) + value);
    }

    fn decrement(&self, value: f64) {
        self.set(f64::from_bits(self.0.load(Ordering::SeqCst)) - value);
    }

    fn set(&self, value: f64) {
        self.0.store(value.to_bits(), Ordering::SeqCst);
    }
}

type Labels = BTreeMap<String, String>;

/// Everything emitted so far, by series.
#[derive(Default)]
pub(crate) struct Capture {
    counters: Mutex<Vec<(String, Labels, Arc<Cell>)>>,
    gauges: Mutex<Vec<(String, Labels, Arc<Cell>)>>,
}

fn labels_of(key: &Key) -> Labels {
    key.labels()
        .map(|label| (label.key().to_owned(), label.value().to_owned()))
        .collect()
}

fn wanted(labels: &[(&str, &str)]) -> Labels {
    labels
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn lookup(table: &Mutex<Vec<(String, Labels, Arc<Cell>)>>, key: &Key) -> Arc<Cell> {
    let mut table = table.lock().unwrap_or_else(PoisonError::into_inner);
    let (name, labels) = (key.name().to_owned(), labels_of(key));
    if let Some((_, _, cell)) = table.iter().find(|(n, l, _)| *n == name && *l == labels) {
        return Arc::clone(cell);
    }
    let cell = Arc::new(Cell::default());
    table.push((name, labels, Arc::clone(&cell)));
    cell
}

impl Capture {
    /// Value of the counter with exactly these labels; 0 when it was never emitted.
    pub(crate) fn counter(&self, name: &str, labels: &[(&str, &str)]) -> u64 {
        let want = wanted(labels);
        let table = self.counters.lock().unwrap_or_else(PoisonError::into_inner);
        table
            .iter()
            .find(|(n, l, _)| n == name && *l == want)
            .map_or(0, |(_, _, cell)| cell.0.load(Ordering::SeqCst))
    }

    /// Last value of the gauge with exactly these labels.
    pub(crate) fn gauge(&self, name: &str, labels: &[(&str, &str)]) -> Option<f64> {
        let want = wanted(labels);
        let table = self.gauges.lock().unwrap_or_else(PoisonError::into_inner);
        table
            .iter()
            .find(|(n, l, _)| n == name && *l == want)
            .map(|(_, _, cell)| f64::from_bits(cell.0.load(Ordering::SeqCst)))
    }

    /// Label sets of every series (counter or gauge) of this name.
    pub(crate) fn label_sets(&self, name: &str) -> Vec<Labels> {
        let counters = self.counters.lock().unwrap_or_else(PoisonError::into_inner);
        let gauges = self.gauges.lock().unwrap_or_else(PoisonError::into_inner);
        counters
            .iter()
            .chain(gauges.iter())
            .filter(|(n, _, _)| n == name)
            .map(|(_, labels, _)| labels.clone())
            .collect()
    }
}

struct Handle(Arc<Capture>);

impl Recorder for Handle {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
        Counter::from_arc(lookup(&self.0.counters, key))
    }

    fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
        Gauge::from_arc(lookup(&self.0.gauges, key))
    }

    fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
        Histogram::noop()
    }
}

/// The process-wide capture, installed on first use.
pub(crate) fn capture() -> &'static Capture {
    static CAPTURE: OnceLock<Arc<Capture>> = OnceLock::new();
    CAPTURE.get_or_init(|| {
        let shared = Arc::new(Capture::default());
        metrics::set_global_recorder(Handle(Arc::clone(&shared)))
            .expect("no other recorder is installed in the unit tests of this crate");
        shared
    })
}

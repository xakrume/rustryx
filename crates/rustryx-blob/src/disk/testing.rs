//! A disk probe that reports whatever a test tells it to.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{DiskProbe, DiskUsage};

pub(crate) struct FakeDisk {
    total: AtomicU64,
    free: AtomicU64,
    failing: AtomicBool,
    calls: AtomicU64,
}

impl FakeDisk {
    pub(crate) fn new(total: u64, free: u64) -> Self {
        Self {
            total: AtomicU64::new(total),
            free: AtomicU64::new(free),
            failing: AtomicBool::new(false),
            calls: AtomicU64::new(0),
        }
    }

    pub(crate) fn set(&self, total: u64, free: u64) {
        self.total.store(total, Ordering::SeqCst);
        self.free.store(free, Ordering::SeqCst);
    }

    pub(crate) fn fail(&self, failing: bool) {
        self.failing.store(failing, Ordering::SeqCst);
    }

    pub(crate) fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

impl DiskProbe for FakeDisk {
    fn usage(&self, _path: &Path) -> std::io::Result<DiskUsage> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.failing.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("probe failure injected"));
        }
        Ok(DiskUsage {
            total_bytes: self.total.load(Ordering::SeqCst),
            free_bytes: self.free.load(Ordering::SeqCst),
        })
    }
}

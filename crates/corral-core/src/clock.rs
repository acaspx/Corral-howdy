//! Time, injectable so tests never sleep and event timestamps are deterministic.

use std::sync::atomic::{AtomicU64, Ordering};

/// A source of wall-clock time in Unix milliseconds (UTC).
pub trait Clock: Send + Sync {
    /// The current time in Unix milliseconds.
    fn now_ms(&self) -> u64;
}

/// A clock that only moves when told to. For tests and fixtures.
#[derive(Debug, Default)]
pub struct ManualClock(AtomicU64);

impl ManualClock {
    /// A clock stopped at `ms`.
    pub fn at(ms: u64) -> Self {
        Self(AtomicU64::new(ms))
    }

    /// Moves the clock forward by `ms`.
    pub fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }

    /// Sets the clock to `ms`.
    pub fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

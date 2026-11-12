//! Time, injectable so tests never sleep and event timestamps are deterministic.

use std::sync::atomic::{AtomicU64, Ordering};

/// A source of wall-clock time in Unix milliseconds (UTC).
pub trait Clock: Send + Sync {
    /// The current time in Unix milliseconds.
    fn now_ms(&self) -> u64;
}

/// The system's wall clock, for the app.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        // A clock set before 1970 reads as 0 rather than failing every event.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }
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

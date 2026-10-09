//! Injectable monotonic time source (spec `03-resources.md` §2: samples and
//! ordering use monotonic milliseconds, never wall clock).
//!
//! `term-core` stays OS-free, so the clock is a trait: the daemon injects
//! [`MonotonicClock`] (`std::time::Instant`), tests inject [`FakeClock`] to
//! drive priority aging and pressure-recovery hysteresis deterministically.

use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Monotonic milliseconds since an arbitrary fixed point.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// Production clock: milliseconds since construction via `Instant`
/// (monotonic, immune to wall-clock adjustments).
#[derive(Debug)]
pub struct MonotonicClock {
    start: Instant,
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl MonotonicClock {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Clock for MonotonicClock {
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

/// Deterministic clock for tests: shared handle, `advance` moves time forward.
/// `Clone` hands the same virtual timeline to the component under test.
#[derive(Debug, Clone, Default)]
pub struct FakeClock {
    now_ms: Arc<Mutex<u64>>,
}

impl FakeClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance the virtual clock by `ms` (saturating at `u64::MAX`).
    pub fn advance(&self, ms: u64) {
        let mut now = self.now_ms.lock().expect("fake clock poisoned");
        *now = now.saturating_add(ms);
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        *self.now_ms.lock().expect("fake clock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_clock_advances_and_shares_one_timeline() {
        let clock = FakeClock::new();
        let twin = clock.clone();
        assert_eq!(clock.now_ms(), 0);
        clock.advance(1_500);
        clock.advance(500);
        assert_eq!(twin.now_ms(), 2_000);
    }

    #[test]
    fn monotonic_clock_never_goes_backwards() {
        let clock = MonotonicClock::new();
        let a = clock.now_ms();
        let b = clock.now_ms();
        assert!(b >= a);
    }
}

//! Cadence gating for selective refreshes.

/// Fires on the first call, then at most once per `interval_ms`.
///
/// Time is caller-supplied monotonic milliseconds so tests can drive the
/// gate deterministically with a fake clock.
#[derive(Debug, Clone)]
pub struct CadenceGate {
    interval_ms: u64,
    last_fired_ms: Option<u64>,
}

impl CadenceGate {
    pub fn new(interval_ms: u64) -> Self {
        Self {
            interval_ms: interval_ms.max(1),
            last_fired_ms: None,
        }
    }

    /// Returns `true` when at least `interval_ms` elapsed since the last
    /// firing (or on the very first call), recording `now_ms` as the new
    /// firing time.
    pub fn due(&mut self, now_ms: u64) -> bool {
        let due = match self.last_fired_ms {
            None => true,
            Some(last) => now_ms.saturating_sub(last) >= self.interval_ms,
        };
        if due {
            self.last_fired_ms = Some(now_ms);
        }
        due
    }

    /// Monotonic time of the last firing, if the gate ever fired.
    pub fn last_fired_ms(&self) -> Option<u64> {
        self.last_fired_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_immediately_then_waits_for_interval() {
        let mut gate = CadenceGate::new(1_000);
        assert!(gate.due(0));
        assert!(!gate.due(500));
        assert!(!gate.due(999));
        assert!(gate.due(1_000));
        assert_eq!(gate.last_fired_ms(), Some(1_000));
        assert!(!gate.due(1_999));
        assert!(gate.due(2_000));
    }

    #[test]
    fn backwards_clock_never_fires_twice_at_same_instant() {
        let mut gate = CadenceGate::new(1_000);
        assert!(gate.due(5_000));
        // Clock jumping backwards saturates to 0 elapsed: no re-fire.
        assert!(!gate.due(1_000));
        assert!(gate.due(6_000));
    }
}

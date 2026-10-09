//! Pure differential math shared by host and workload sampling.

/// Outcome of differencing a cumulative counter between two polls.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Delta {
    /// `(cur - prev) * 1000 / dt_ms` — units per second.
    Rate(f64),
    /// `cur < prev`: the counter was reset (reboot, interface re-enumeration,
    /// process restart). The rate is unknowable, not zero.
    Reset,
    /// Zero elapsed time between the two polls.
    NoDelta,
}

/// Rate per second from cumulative counter deltas.
///
/// `dt_ms == 0` → [`Delta::NoDelta`]; `cur < prev` → [`Delta::Reset`]
/// (counter reset detection); otherwise the byte/cpu-millisecond/... rate.
pub fn counter_delta_rate(prev_total: u64, cur_total: u64, dt_ms: u64) -> Delta {
    if dt_ms == 0 {
        return Delta::NoDelta;
    }
    match cur_total.checked_sub(prev_total) {
        Some(delta) => Delta::Rate(delta as f64 * 1_000.0 / dt_ms as f64),
        None => Delta::Reset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(prev: u64, cur: u64, dt_ms: u64) -> f64 {
        match counter_delta_rate(prev, cur, dt_ms) {
            Delta::Rate(v) => v,
            other => panic!("expected a rate, got {other:?}"),
        }
    }

    #[test]
    fn rate_math_on_deltas() {
        // 1500 bytes over 500 ms → 3000 B/s.
        assert!((rate(1_000, 2_500, 500) - 3_000.0).abs() < 1e-9);
        // 2000 counter units over 1000 ms → 2000 units/s. (For CPU-ms
        // counters the caller divides by dt directly to get cores; this
        // function is the per-second form used for byte counters.)
        assert!((rate(3_000, 5_000, 1_000) - 2_000.0).abs() < 1e-9);
        // Idle counter: a true zero rate, not null.
        assert!((rate(500, 500, 1_000) - 0.0).abs() < 1e-9);
        // Sub-second polls scale correctly.
        assert!((rate(0, 10, 100) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn counter_decrease_is_reset_not_negative() {
        assert_eq!(counter_delta_rate(10_000, 4_000, 1_000), Delta::Reset);
        // Reset wins even at absurd rates; zero dt is reported as no-delta.
        assert_eq!(counter_delta_rate(10, 0, 0), Delta::NoDelta);
    }

    #[test]
    fn zero_elapsed_time_has_no_rate() {
        assert_eq!(counter_delta_rate(100, 200, 0), Delta::NoDelta);
    }

    #[test]
    fn u64_boundaries_do_not_overflow() {
        // Max deltas stay finite and ordered.
        let r = rate(u64::MAX / 2, u64::MAX, 1_000);
        assert!(r.is_finite() && r > 0.0);
        assert_eq!(counter_delta_rate(u64::MAX, 0, 1_000), Delta::Reset);
    }
}

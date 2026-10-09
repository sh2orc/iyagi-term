//! Instantaneous host memory pressure classification (spec
//! `03-resources.md` §2/§3). Hysteresis (2 consecutive worsening samples,
//! 10 s sustained recovery, native critical signals) lives daemon-side in
//! `term-core`; this function is the pure instantaneous classifier.

use term_contracts::metrics::PressureLevel;

/// 1 GiB absolute floor for the CRITICAL level.
pub const CRITICAL_ABSOLUTE_BYTES: u64 = 1 << 30;

/// Classifies memory pressure from physical totals.
///
/// Boundary semantics (integer cross-multiplication, no float error), per
/// spec `03-resources.md` §3 prose and identical to `term-core`'s tracker:
/// * CRITICAL when `available < 1 GiB`, or `available/total` is strictly
///   below 10% (`available * 10 < total`). Exactly 10.0% is WARNING.
/// * WARNING when the ratio is strictly below 20% (`available * 5 < total`).
///   Exactly 20.0% is NORMAL per the spec's `<20%` wording.
/// * A total of 0 is broken telemetry and classifies conservatively as
///   CRITICAL — identical to `term-core`'s tracker, so the instantaneous
///   `HostSample.pressure` and the hysteresis level never disagree on it.
pub fn classify_pressure(total_bytes: u64, available_bytes: u64) -> PressureLevel {
    if total_bytes == 0 || available_bytes < CRITICAL_ABSOLUTE_BYTES {
        return PressureLevel::Critical;
    }
    let total = total_bytes as u128;
    let avail = available_bytes as u128;
    // available/total < 10%  <=>  available * 10 < total
    if avail * 10 < total {
        return PressureLevel::Critical;
    }
    // available/total < 20%  <=>  available * 5 < total
    if avail * 5 < total {
        return PressureLevel::Warning;
    }
    PressureLevel::Normal
}

#[cfg(test)]
mod tests {
    use super::*;
    use term_contracts::metrics::PressureLevel::*;

    const GIB: u64 = 1 << 30;
    // Percent-boundary tests need GiB-scale values so the absolute
    // `available < 1 GiB` floor does not dominate the ratio rules.
    const T: u64 = 1_000 * GIB;

    #[test]
    fn comfortable_headroom_is_normal() {
        assert_eq!(classify_pressure(16 * GIB, 12 * GIB), Normal);
        // Exactly 20.0% available: <20% is WARNING, so 20.0% itself is NORMAL.
        assert_eq!(classify_pressure(T, 200 * GIB), Normal);
        assert_eq!(classify_pressure(10 * GIB, 2 * GIB), Normal);
    }

    #[test]
    fn below_twenty_percent_is_warning() {
        assert_eq!(classify_pressure(T, 199 * GIB), Warning); // 19.9%
        assert_eq!(classify_pressure(16 * GIB, 3 * GIB), Warning); // 18.75%
                                                                   // Just above the critical line: 10.1% is WARNING territory.
        assert_eq!(classify_pressure(T, 101 * GIB), Warning);
    }

    #[test]
    fn ten_percent_boundary_is_warning_strict() {
        // Exactly 10.0% is WARNING (spec: `<10%` is critical); one below is CRITICAL.
        assert_eq!(classify_pressure(T, 100 * GIB), Warning);
        assert_eq!(classify_pressure(T, 100 * GIB - 1), Critical);
        assert_eq!(classify_pressure(10 * GIB, GIB), Warning);
        // Comfortably below 10%.
        assert_eq!(classify_pressure(16 * GIB, GIB), Critical); // 6.25%
    }

    #[test]
    fn one_gib_absolute_floor() {
        // avail = 1 GiB exactly is not "< 1 GiB"; with a large total it falls
        // to the ratio rule (12.5% here → WARNING).
        assert_eq!(classify_pressure(8 * GIB, GIB), Warning);
        // One byte under the floor is CRITICAL regardless of ratio.
        assert_eq!(classify_pressure(64 * GIB, GIB - 1), Critical);
        assert_eq!(classify_pressure(10_000, 0), Critical);
    }

    #[test]
    fn degenerate_inputs_do_not_panic_or_misclassify() {
        assert_eq!(classify_pressure(0, 0), Critical);
        // No total is broken telemetry: CRITICAL, same as term-core's tracker.
        assert_eq!(classify_pressure(0, 2 * GIB), Critical);
        // available > total is nonsensical but must not wrap around.
        assert_eq!(classify_pressure(GIB, 8 * GIB), Normal);
    }

    #[test]
    fn boundary_math_survives_large_byte_values() {
        // Large totals divisible by 10 so `total/10` is an exact 10.0%.
        let total: u64 = 10 * (2 << 40) / 10 * 10; // 10 TiB-scale, divisible by 10
        assert_eq!(classify_pressure(total, total / 10), Warning); // exactly 10%
        assert_eq!(classify_pressure(total, total / 10 - 1), Critical);
        assert_eq!(classify_pressure(total, total / 10 + 1), Warning);
        assert_eq!(classify_pressure(u64::MAX, u64::MAX / 2), Normal);
    }
}

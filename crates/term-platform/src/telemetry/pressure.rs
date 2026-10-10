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
/// the resource governance plan: CRITICAL is the 1 GiB absolute floor (or a
/// zero total = broken telemetry) — the old `<10%` ratio rule is gone, it
/// raised false CRITICALs on large-RAM hosts where macOS keeps compressing
/// and swapping well below it. Real crises surface through the kernel signal
/// (`kernel_pressure::kernel_memory_critical`) instead, which the daemon
/// feeds to the tracker's `force_critical`.
///
/// * WARNING when the ratio is strictly below 12% (`available * 25 <
///   total * 3`). Exactly 12.0% is NORMAL.
/// * A total of 0 is broken telemetry and classifies conservatively as
///   CRITICAL — identical to `term-core`'s tracker, so the instantaneous
///   `HostSample.pressure` and the hysteresis level never disagree on it.
pub fn classify_pressure(total_bytes: u64, available_bytes: u64) -> PressureLevel {
    if total_bytes == 0 || available_bytes < CRITICAL_ABSOLUTE_BYTES {
        return PressureLevel::Critical;
    }
    let total = total_bytes as u128;
    let avail = available_bytes as u128;
    // available/total < 12%  <=>  available * 25 < total * 3
    if avail * 25 < total * 3 {
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
        // Exactly 12.0% available: <12% is WARNING, so 12.0% itself is NORMAL.
        assert_eq!(classify_pressure(T, 120 * GIB), Normal);
        assert_eq!(classify_pressure(10 * GIB, 2 * GIB), Normal);
    }

    #[test]
    fn below_twelve_percent_is_warning() {
        assert_eq!(classify_pressure(T, 119 * GIB), Warning); // 11.9%
        assert_eq!(classify_pressure(16 * GIB, GIB + 1), Warning); // ~6%
                                                                       // Just above the critical floor: WARNING territory, never CRITICAL
                                                                       // by ratio.
        assert_eq!(classify_pressure(T, 101 * GIB), Warning);
    }

    #[test]
    fn one_gib_absolute_floor_is_the_only_critical_rule() {
        // avail = 1 GiB exactly is not "< 1 GiB"; with a large total it falls
        // to the ratio rule (6.25% of 16 GiB here → WARNING).
        assert_eq!(classify_pressure(16 * GIB, GIB), Warning);
        // One byte under the floor is CRITICAL regardless of ratio — even on
        // a huge host where that is a tiny fraction (the old <10% rule).
        assert_eq!(classify_pressure(64 * GIB, GIB - 1), Critical);
        assert_eq!(classify_pressure(10_000, 0), Critical);
        // A large host deep under the old ratio line stays WARNING, not
        // CRITICAL: the kernel signal owns real crises now.
        assert_eq!(classify_pressure(24 * GIB, 2 * GIB), Warning); // 8.3%
        assert_eq!(classify_pressure(32 * GIB, 2 * GIB), Warning); // 6.25%
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
        // Large total divisible by 100 so `total*3/25` is an exact 12.0%.
        let total: u64 = 25 * (2 << 40) / 100 * 100;
        let twelve_percent = (total as u128 * 3).div_ceil(25) as u64;
        assert_eq!(classify_pressure(total, twelve_percent), Normal); // exactly 12%
        assert_eq!(classify_pressure(total, twelve_percent - 1), Warning);
        assert_eq!(classify_pressure(total, twelve_percent + 1), Normal);
        assert_eq!(classify_pressure(u64::MAX, u64::MAX / 2), Normal);
    }
}

//! Host memory pressure classification with hysteresis
//! (spec `03-resources.md` §3).
//!
//! Raw thresholds: `available/T < 10%` **or** `available < 1 GiB` → CRITICAL;
//! `< 20%` → WARNING; else NORMAL. Hysteresis: worsening requires 2
//! consecutive worse samples; recovery to NORMAL requires
//! `available/T >= 25%` **and** `available >= S` sustained for
//! `timing_ms.pressure_recovery` (default 10 s). A native memory-pressure
//! critical signal raises CRITICAL immediately ([`PressureTracker::force_critical`]).
//!
//! Staleness is admission's business, not pressure's: a stale sample simply
//! is not fed here, while admission returns `WAIT_TELEMETRY` for it. All math
//! is integer (`u128` intermediates for the percent ratios).

use term_contracts::defaults::Defaults;
use term_contracts::metrics::PressureLevel;

use crate::admission::host_reserve_bytes;
use crate::clock::Clock;

/// Thresholds from `defaults.json` (`admission.*`, `timing_ms.pressure_recovery`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PressureConfig {
    /// `critical_available_percent` (default 10).
    pub critical_available_percent: u64,
    /// `critical_available_bytes` (default 1 GiB).
    pub critical_available_bytes: u64,
    /// `warning_available_percent` (default 20).
    pub warning_available_percent: u64,
    /// `recovery_available_percent` (default 25).
    pub recovery_available_percent: u64,
    /// `timing_ms.pressure_recovery` (default 10_000): sustained-good window.
    pub recovery_sustain_ms: u64,
    /// `admission.host_reserve_min_bytes` (default 2 GiB): floor of `S`.
    pub host_reserve_min_bytes: u64,
    /// `admission.host_reserve_percent` (default 15): `S = ceil(T*15%)`.
    pub host_reserve_percent: u64,
}

impl PressureConfig {
    pub fn from_defaults(defaults: &Defaults) -> Self {
        Self {
            critical_available_percent: defaults.admission.critical_available_percent,
            critical_available_bytes: defaults.admission.critical_available_bytes,
            warning_available_percent: defaults.admission.warning_available_percent,
            recovery_available_percent: defaults.admission.recovery_available_percent,
            recovery_sustain_ms: defaults.timing_ms.pressure_recovery,
            host_reserve_min_bytes: defaults.admission.host_reserve_min_bytes,
            host_reserve_percent: defaults.admission.host_reserve_percent,
        }
    }

    /// Host safety reserve `S` used by the recovery gate.
    pub fn host_reserve_bytes(&self, total_bytes: u64) -> u64 {
        host_reserve_bytes(
            total_bytes,
            self.host_reserve_min_bytes,
            self.host_reserve_percent,
        )
    }

    /// Raw per-sample classification, no hysteresis. `available/T < pct` is
    /// strict (exactly 10% is not critical); a zero total is broken telemetry
    /// and classifies conservatively as CRITICAL.
    pub fn classify(&self, total_bytes: u64, available_bytes: u64) -> PressureLevel {
        if total_bytes == 0 {
            return PressureLevel::Critical;
        }
        let percent_of =
            |available: u128, percent: u64| available * 100 < total_bytes as u128 * percent as u128;
        if available_bytes < self.critical_available_bytes
            || percent_of(available_bytes as u128, self.critical_available_percent)
        {
            return PressureLevel::Critical;
        }
        if percent_of(available_bytes as u128, self.warning_available_percent) {
            return PressureLevel::Warning;
        }
        PressureLevel::Normal
    }

    /// Recovery gate: `available/T >= recovery_available_percent` AND
    /// `available >= S` (spec `03-resources.md` §3).
    pub fn recovery_satisfied(&self, total_bytes: u64, available_bytes: u64) -> bool {
        available_bytes as u128 * 100
            >= total_bytes as u128 * self.recovery_available_percent as u128
            && available_bytes >= self.host_reserve_bytes(total_bytes)
    }
}

fn rank(level: PressureLevel) -> u8 {
    match level {
        PressureLevel::Normal => 0,
        PressureLevel::Warning => 1,
        PressureLevel::Critical => 2,
    }
}

/// Hysteresis state machine over host samples. Daemon-owned; feed it every
/// host telemetry tick (1 s) and read [`PressureTracker::level`] into the
/// next [`term_contracts::metrics::HostSample`].
#[derive(Debug)]
pub struct PressureTracker<C: Clock> {
    config: PressureConfig,
    clock: C,
    level: PressureLevel,
    /// Worst raw level seen in the current worsening streak.
    pending_worse: Option<PressureLevel>,
    worse_streak: u32,
    /// When the current stretch of sustained-good samples started.
    recovery_since_ms: Option<u64>,
}

impl<C: Clock> PressureTracker<C> {
    pub fn new(config: PressureConfig, clock: C) -> Self {
        Self {
            config,
            clock,
            level: PressureLevel::Normal,
            pending_worse: None,
            worse_streak: 0,
            recovery_since_ms: None,
        }
    }

    pub fn config(&self) -> &PressureConfig {
        &self.config
    }

    pub fn level(&self) -> PressureLevel {
        self.level
    }

    /// Feed one host memory sample; returns the effective level.
    ///
    /// * Worsening flips only after 2 consecutive worse-than-current samples
    ///   (the pending level is the worst seen in the streak).
    /// * Any non-worse sample clears the worsening streak.
    /// * Recovery to NORMAL needs the sustained-good condition held for
    ///   `recovery_sustain_ms` of monotonic time. The spec defines no
    ///   intermediate CRITICAL→WARNING recovery path, so none exists here.
    /// * `None` (unknown/unavailable measurement) holds the current level but
    ///   breaks the recovery stretch: sustained goodness must be observed.
    pub fn update(&mut self, total_bytes: u64, available_bytes: Option<u64>) -> PressureLevel {
        let Some(available) = available_bytes else {
            self.recovery_since_ms = None;
            return self.level;
        };
        let raw = self.config.classify(total_bytes, available);
        if rank(raw) > rank(self.level) {
            self.recovery_since_ms = None;
            let pending = match self.pending_worse {
                Some(p) if rank(p) >= rank(raw) => p,
                _ => raw,
            };
            self.pending_worse = Some(pending);
            self.worse_streak += 1;
            if self.worse_streak >= 2 {
                self.level = pending;
                self.worse_streak = 0;
                self.pending_worse = None;
            }
        } else {
            self.worse_streak = 0;
            self.pending_worse = None;
            if self.level != PressureLevel::Normal
                && self.config.recovery_satisfied(total_bytes, available)
            {
                let now = self.clock.now_ms();
                match self.recovery_since_ms {
                    None => self.recovery_since_ms = Some(now),
                    Some(start) if now.saturating_sub(start) >= self.config.recovery_sustain_ms => {
                        self.level = PressureLevel::Normal;
                        self.recovery_since_ms = None;
                    }
                    Some(_) => {}
                }
            } else {
                self.recovery_since_ms = None;
            }
        }
        self.level
    }

    /// Native memory-pressure critical signal: raise to CRITICAL immediately
    /// (spec `03-resources.md` §3). Level-triggered contract: a persistent
    /// native signal must re-invoke this on every event; because recovery
    /// needs 10 s of sustained-good samples afterwards, one call effectively
    /// pins CRITICAL for at least the recovery window.
    pub fn force_critical(&mut self) {
        self.level = PressureLevel::Critical;
        self.worse_streak = 0;
        self.pending_worse = None;
        self.recovery_since_ms = None;
    }
}

// ---------------------------------------------------------------------------
// CPU saturation (spec `08-pressure-relief.md` §1)

/// CPU 포화도 임계값(`defaults.json`의 `cpu_pressure.*`,
/// `timing_ms.pressure_recovery`). 메모리 압력이 "남은 여유"를 보는 것과
/// 달리 여기서는 "쓰고 있는 비율"(used_cores / logical_cpus)을 본다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuPressureConfig {
    /// `cpu_pressure.warning_used_percent` (default 85).
    pub warning_used_percent: u64,
    /// `cpu_pressure.critical_used_percent` (default 95).
    pub critical_used_percent: u64,
    /// `cpu_pressure.recovery_used_percent` (default 70).
    pub recovery_used_percent: u64,
    /// `timing_ms.pressure_recovery` (default 10_000): sustained-good window,
    /// shared with the memory tracker.
    pub recovery_sustain_ms: u64,
}

impl CpuPressureConfig {
    pub fn from_defaults(defaults: &Defaults) -> Self {
        Self {
            warning_used_percent: defaults.cpu_pressure.warning_used_percent,
            critical_used_percent: defaults.cpu_pressure.critical_used_percent,
            recovery_used_percent: defaults.cpu_pressure.recovery_used_percent,
            recovery_sustain_ms: defaults.timing_ms.pressure_recovery,
        }
    }

    /// Raw per-sample classification, no hysteresis. `None` means *unknown*
    /// (no logical CPU count, or a NaN/negative measurement) — never 0 and
    /// never CRITICAL: CPU pressure only ever triggers relief, so an
    /// unmeasurable host must do nothing at all (03 §2: unavailable is not
    /// a number).
    ///
    /// The comparison is cross-multiplied (`used * 100 >= cpus * pct`) so no
    /// division or float-equality trap decides a threshold.
    pub fn classify(&self, used_cores: f64, logical_cpus: u32) -> Option<PressureLevel> {
        if logical_cpus == 0 || !used_cores.is_finite() || used_cores < 0.0 {
            return None;
        }
        let at_or_above = |percent: u64| used_cores * 100.0 >= logical_cpus as f64 * percent as f64;
        if at_or_above(self.critical_used_percent) {
            return Some(PressureLevel::Critical);
        }
        if at_or_above(self.warning_used_percent) {
            return Some(PressureLevel::Warning);
        }
        Some(PressureLevel::Normal)
    }

    /// Recovery gate: `used/cpus <= recovery_used_percent`. Unknown samples
    /// never satisfy it.
    pub fn recovery_satisfied(&self, used_cores: f64, logical_cpus: u32) -> bool {
        logical_cpus != 0
            && used_cores.is_finite()
            && used_cores >= 0.0
            && used_cores * 100.0 <= logical_cpus as f64 * self.recovery_used_percent as f64
    }
}

/// Hysteresis state machine over host CPU samples, with exactly the
/// semantics of [`PressureTracker`]: 2 consecutive worse samples to worsen,
/// sustained-good for `recovery_sustain_ms` to return to NORMAL, unknown
/// holds the level and breaks the recovery stretch. There is no native
/// "CPU critical" signal, so no force path exists.
#[derive(Debug)]
pub struct CpuPressureTracker<C: Clock> {
    config: CpuPressureConfig,
    clock: C,
    level: PressureLevel,
    /// Worst raw level seen in the current worsening streak.
    pending_worse: Option<PressureLevel>,
    worse_streak: u32,
    /// When the current stretch of sustained-good samples started.
    recovery_since_ms: Option<u64>,
}

impl<C: Clock> CpuPressureTracker<C> {
    pub fn new(config: CpuPressureConfig, clock: C) -> Self {
        Self {
            config,
            clock,
            level: PressureLevel::Normal,
            pending_worse: None,
            worse_streak: 0,
            recovery_since_ms: None,
        }
    }

    pub fn config(&self) -> &CpuPressureConfig {
        &self.config
    }

    pub fn level(&self) -> PressureLevel {
        self.level
    }

    /// Feed one host CPU sample (`used_cores` = busy logical cores, e.g.
    /// 3.2 of 8); returns the effective level.
    ///
    /// * Worsening flips only after 2 consecutive worse-than-current samples
    ///   (the pending level is the worst seen in the streak).
    /// * Any non-worse sample clears the worsening streak.
    /// * Recovery to NORMAL needs `used/cpus <= recovery_used_percent` held
    ///   for `recovery_sustain_ms` of monotonic time. Like the memory
    ///   tracker there is no intermediate CRITICAL→WARNING path.
    /// * `None`/unmeasurable holds the current level and breaks the
    ///   recovery stretch — relief must never be decided on a guess.
    pub fn update(&mut self, used_cores: Option<f64>, logical_cpus: u32) -> PressureLevel {
        let Some(used) = used_cores else {
            self.recovery_since_ms = None;
            return self.level;
        };
        let Some(raw) = self.config.classify(used, logical_cpus) else {
            self.recovery_since_ms = None;
            return self.level;
        };
        if rank(raw) > rank(self.level) {
            self.recovery_since_ms = None;
            let pending = match self.pending_worse {
                Some(p) if rank(p) >= rank(raw) => p,
                _ => raw,
            };
            self.pending_worse = Some(pending);
            self.worse_streak += 1;
            if self.worse_streak >= 2 {
                self.level = pending;
                self.worse_streak = 0;
                self.pending_worse = None;
            }
        } else {
            self.worse_streak = 0;
            self.pending_worse = None;
            if self.level != PressureLevel::Normal
                && self.config.recovery_satisfied(used, logical_cpus)
            {
                let now = self.clock.now_ms();
                match self.recovery_since_ms {
                    None => self.recovery_since_ms = Some(now),
                    Some(start) if now.saturating_sub(start) >= self.config.recovery_sustain_ms => {
                        self.level = PressureLevel::Normal;
                        self.recovery_since_ms = None;
                    }
                    Some(_) => {}
                }
            } else {
                self.recovery_since_ms = None;
            }
        }
        self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use term_contracts::defaults::load_spec_defaults;

    const GIB: u64 = 1 << 30;
    const T16: u64 = 16 * GIB;

    fn config() -> PressureConfig {
        PressureConfig::from_defaults(&load_spec_defaults().expect("spec defaults must parse"))
    }

    fn tracker() -> (PressureTracker<FakeClock>, FakeClock) {
        let clock = FakeClock::new();
        (PressureTracker::new(config(), clock.clone()), clock)
    }

    #[test]
    fn from_defaults_pins_spec_thresholds() {
        let cfg = config();
        assert_eq!(cfg.critical_available_percent, 10);
        assert_eq!(cfg.critical_available_bytes, GIB);
        assert_eq!(cfg.warning_available_percent, 20);
        assert_eq!(cfg.recovery_available_percent, 25);
        assert_eq!(cfg.recovery_sustain_ms, 10_000);
        assert_eq!(cfg.host_reserve_bytes(T16), 2_576_980_378);
    }

    #[test]
    fn raw_classification_thresholds_are_strict() {
        let cfg = config();
        // Ratio rule: exactly 10% of 16 GiB is 1717986918.4 -> 1717986918 is
        // under (CRITICAL), 1717986919 is at-or-above (WARNING until 20%).
        assert_eq!(cfg.classify(T16, 1_717_986_918), PressureLevel::Critical);
        assert_eq!(cfg.classify(T16, 1_717_986_919), PressureLevel::Warning);
        // Byte rule binds when 1 GiB > 10% of T (8 GiB host, 11.25% free).
        assert_eq!(cfg.classify(8 * GIB, GIB - 1), PressureLevel::Critical);
        assert_eq!(cfg.classify(8 * GIB, GIB), PressureLevel::Warning);
        // Exactly 20% (3435973836.8) is the WARNING/NORMAL line: 836 is
        // under (WARNING), 837 is at-or-above (NORMAL).
        assert_eq!(cfg.classify(T16, 3_435_973_836), PressureLevel::Warning);
        assert_eq!(cfg.classify(T16, 3_435_973_837), PressureLevel::Normal);
        assert_eq!(cfg.classify(0, 4 * GIB), PressureLevel::Critical);
    }

    #[test]
    fn single_worse_sample_does_not_flip_but_two_consecutive_do() {
        let (mut t, clock) = tracker();
        // CRITICAL raw sample 1: hold NORMAL.
        assert_eq!(
            t.update(T16, Some(500 * 1024 * 1024)),
            PressureLevel::Normal
        );
        // Interleaved good sample resets the streak.
        assert_eq!(t.update(T16, Some(8 * GIB)), PressureLevel::Normal);
        assert_eq!(
            t.update(T16, Some(500 * 1024 * 1024)),
            PressureLevel::Normal
        );
        assert_eq!(t.update(T16, Some(8 * GIB)), PressureLevel::Normal);
        // Two consecutive CRITICAL samples flip.
        clock.advance(1_000);
        assert_eq!(
            t.update(T16, Some(500 * 1024 * 1024)),
            PressureLevel::Normal
        );
        clock.advance(1_000);
        assert_eq!(
            t.update(T16, Some(500 * 1024 * 1024)),
            PressureLevel::Critical
        );
    }

    #[test]
    fn worsening_to_warning_also_needs_two_samples() {
        let (mut t, _clock) = tracker();
        // 15.6% free: WARNING raw.
        assert_eq!(t.update(T16, Some(2_500_000_000)), PressureLevel::Normal);
        assert_eq!(t.update(T16, Some(2_500_000_000)), PressureLevel::Warning);
        // Worsening further from WARNING to CRITICAL needs 2 fresh samples.
        assert_eq!(
            t.update(T16, Some(500 * 1024 * 1024)),
            PressureLevel::Warning
        );
        assert_eq!(
            t.update(T16, Some(500 * 1024 * 1024)),
            PressureLevel::Critical
        );
    }

    #[test]
    fn mixed_worse_streak_uses_the_worst_pending_level() {
        let (mut t, _clock) = tracker();
        assert_eq!(t.update(T16, Some(2_500_000_000)), PressureLevel::Normal); // WARNING raw
        assert_eq!(
            t.update(T16, Some(500 * 1024 * 1024)),
            PressureLevel::Critical
        ); // CRITICAL raw: flip to worst
    }

    #[test]
    fn recovery_needs_ten_seconds_sustained() {
        let (mut t, clock) = tracker();
        // Drive to CRITICAL (2 samples).
        t.update(T16, Some(500 * 1024 * 1024));
        clock.advance(1_000);
        assert_eq!(
            t.update(T16, Some(500 * 1024 * 1024)),
            PressureLevel::Critical
        );
        // Good samples (25% of 16 GiB = 4 GiB; S = ~2.4 GiB -> satisfied).
        for _ in 0..10 {
            clock.advance(1_000);
            assert_eq!(t.update(T16, Some(4 * GIB)), PressureLevel::Critical);
        }
        // First good sample was at +1s; at +11s the stretch is 10s: recover.
        clock.advance(1_000);
        assert_eq!(t.update(T16, Some(4 * GIB)), PressureLevel::Normal);
    }

    #[test]
    fn interrupted_recovery_resets_the_window() {
        let (mut t, clock) = tracker();
        t.update(T16, Some(500 * 1024 * 1024));
        clock.advance(1_000);
        assert_eq!(
            t.update(T16, Some(500 * 1024 * 1024)),
            PressureLevel::Critical
        );
        for _ in 0..5 {
            clock.advance(1_000);
            t.update(T16, Some(4 * GIB));
        }
        // A bad-but-not-worse sample (WARNING raw) breaks the good stretch.
        clock.advance(1_000);
        assert_eq!(t.update(T16, Some(2_500_000_000)), PressureLevel::Critical);
        // Window restarts at the next good sample: 10 samples inside the
        // window, the 11th (>= 10 s later) recovers.
        for _ in 0..10 {
            clock.advance(1_000);
            assert_eq!(t.update(T16, Some(4 * GIB)), PressureLevel::Critical);
        }
        clock.advance(1_000);
        assert_eq!(t.update(T16, Some(4 * GIB)), PressureLevel::Normal);
    }

    #[test]
    fn recovery_also_requires_available_ge_reserve() {
        let (mut t, clock) = tracker();
        // 4 GiB host: S = 2 GiB floor; 25% = 1 GiB. 1.5 GiB free is 37.5%
        // (percent gate passes) but below S -> recovery NOT satisfied.
        let small = 4 * GIB;
        t.update(small, Some(300 * 1024 * 1024));
        clock.advance(1_000);
        assert_eq!(
            t.update(small, Some(300 * 1024 * 1024)),
            PressureLevel::Critical
        );
        for _ in 0..20 {
            clock.advance(1_000);
            assert_eq!(t.update(small, Some(3 * GIB / 2)), PressureLevel::Critical);
        }
        // Above S the window starts and completes after 10 s.
        for _ in 0..10 {
            clock.advance(1_000);
            assert_eq!(t.update(small, Some(2 * GIB)), PressureLevel::Critical);
        }
        clock.advance(1_000);
        assert_eq!(t.update(small, Some(2 * GIB)), PressureLevel::Normal);
    }

    #[test]
    fn unknown_sample_holds_level_and_breaks_recovery() {
        let (mut t, clock) = tracker();
        t.update(T16, Some(500 * 1024 * 1024));
        clock.advance(1_000);
        assert_eq!(
            t.update(T16, Some(500 * 1024 * 1024)),
            PressureLevel::Critical
        );
        for _ in 0..8 {
            clock.advance(1_000);
            t.update(T16, Some(4 * GIB));
        }
        // Unknown availability: level holds, recovery stretch resets.
        clock.advance(1_000);
        assert_eq!(t.update(T16, None), PressureLevel::Critical);
        // Window restarts at the next observed good sample.
        for _ in 0..10 {
            clock.advance(1_000);
            assert_eq!(t.update(T16, Some(4 * GIB)), PressureLevel::Critical);
        }
        clock.advance(1_000);
        assert_eq!(t.update(T16, Some(4 * GIB)), PressureLevel::Normal);
    }

    #[test]
    fn force_critical_is_immediate_and_holds_until_recovery() {
        let (mut t, clock) = tracker();
        t.force_critical();
        assert_eq!(t.level(), PressureLevel::Critical);
        // Good samples alone do not clear it instantly; 10 s are required.
        for _ in 0..10 {
            clock.advance(1_000);
            assert_eq!(t.update(T16, Some(8 * GIB)), PressureLevel::Critical);
        }
        clock.advance(1_000);
        assert_eq!(t.update(T16, Some(8 * GIB)), PressureLevel::Normal);
        // A second force after recovery re-pins immediately.
        t.force_critical();
        assert_eq!(t.level(), PressureLevel::Critical);
    }
}

#[cfg(test)]
mod cpu_tests {
    use super::*;
    use crate::clock::FakeClock;
    use term_contracts::defaults::load_spec_defaults;

    /// 8 logical cores: WARNING at 6.8, CRITICAL at 7.6, recovery at 5.6.
    const CPUS: u32 = 8;

    fn config() -> CpuPressureConfig {
        CpuPressureConfig::from_defaults(&load_spec_defaults().expect("spec defaults must parse"))
    }

    fn tracker() -> (CpuPressureTracker<FakeClock>, FakeClock) {
        let clock = FakeClock::new();
        (CpuPressureTracker::new(config(), clock.clone()), clock)
    }

    #[test]
    fn from_defaults_pins_spec_thresholds() {
        let cfg = config();
        assert_eq!(cfg.warning_used_percent, 85);
        assert_eq!(cfg.critical_used_percent, 95);
        assert_eq!(cfg.recovery_used_percent, 70);
        assert_eq!(cfg.recovery_sustain_ms, 10_000);
    }

    #[test]
    fn raw_classification_is_at_or_above_the_thresholds() {
        let cfg = config();
        // 85% of 8 cores = 6.8; 95% = 7.6.
        assert_eq!(cfg.classify(6.79, CPUS), Some(PressureLevel::Normal));
        assert_eq!(cfg.classify(6.8, CPUS), Some(PressureLevel::Warning));
        assert_eq!(cfg.classify(7.59, CPUS), Some(PressureLevel::Warning));
        assert_eq!(cfg.classify(7.6, CPUS), Some(PressureLevel::Critical));
        // A single core host: 0.85 / 0.95.
        assert_eq!(cfg.classify(0.84, 1), Some(PressureLevel::Normal));
        assert_eq!(cfg.classify(0.95, 1), Some(PressureLevel::Critical));
        // Over-full (rounding in the sampler can exceed the core count).
        assert_eq!(cfg.classify(9.0, CPUS), Some(PressureLevel::Critical));
        assert_eq!(cfg.classify(0.0, CPUS), Some(PressureLevel::Normal));
    }

    /// Unknown must be unknown — never 0, never CRITICAL (03 §2).
    #[test]
    fn unmeasurable_samples_classify_as_unknown_not_critical() {
        let cfg = config();
        assert_eq!(cfg.classify(1.0, 0), None);
        assert_eq!(cfg.classify(f64::NAN, CPUS), None);
        assert_eq!(cfg.classify(f64::INFINITY, CPUS), None);
        assert_eq!(cfg.classify(-0.5, CPUS), None);
        assert!(!cfg.recovery_satisfied(1.0, 0));
        assert!(!cfg.recovery_satisfied(f64::NAN, CPUS));
        // 70% of 8 = 5.6 exactly satisfies the gate.
        assert!(cfg.recovery_satisfied(5.6, CPUS));
        assert!(!cfg.recovery_satisfied(5.61, CPUS));
    }

    #[test]
    fn single_worse_sample_does_not_flip_but_two_consecutive_do() {
        let (mut t, clock) = tracker();
        assert_eq!(t.update(Some(7.9), CPUS), PressureLevel::Normal);
        // An idle sample in between resets the streak.
        assert_eq!(t.update(Some(1.0), CPUS), PressureLevel::Normal);
        assert_eq!(t.update(Some(7.9), CPUS), PressureLevel::Normal);
        assert_eq!(t.update(Some(1.0), CPUS), PressureLevel::Normal);
        clock.advance(1_000);
        assert_eq!(t.update(Some(7.9), CPUS), PressureLevel::Normal);
        clock.advance(1_000);
        assert_eq!(t.update(Some(7.9), CPUS), PressureLevel::Critical);
    }

    #[test]
    fn worsening_to_warning_also_needs_two_samples() {
        let (mut t, _clock) = tracker();
        assert_eq!(t.update(Some(7.0), CPUS), PressureLevel::Normal);
        assert_eq!(t.update(Some(7.0), CPUS), PressureLevel::Warning);
        assert_eq!(t.update(Some(7.9), CPUS), PressureLevel::Warning);
        assert_eq!(t.update(Some(7.9), CPUS), PressureLevel::Critical);
    }

    #[test]
    fn mixed_worse_streak_uses_the_worst_pending_level() {
        let (mut t, _clock) = tracker();
        assert_eq!(t.update(Some(7.0), CPUS), PressureLevel::Normal); // WARNING raw
        assert_eq!(t.update(Some(7.9), CPUS), PressureLevel::Critical); // CRITICAL raw
    }

    #[test]
    fn recovery_needs_ten_seconds_sustained() {
        let (mut t, clock) = tracker();
        t.update(Some(7.9), CPUS);
        clock.advance(1_000);
        assert_eq!(t.update(Some(7.9), CPUS), PressureLevel::Critical);
        // 5.6 cores of 8 is exactly 70%: the recovery gate is satisfied.
        for _ in 0..10 {
            clock.advance(1_000);
            assert_eq!(t.update(Some(5.6), CPUS), PressureLevel::Critical);
        }
        clock.advance(1_000);
        assert_eq!(t.update(Some(5.6), CPUS), PressureLevel::Normal);
    }

    #[test]
    fn interrupted_recovery_resets_the_window() {
        let (mut t, clock) = tracker();
        t.update(Some(7.9), CPUS);
        clock.advance(1_000);
        assert_eq!(t.update(Some(7.9), CPUS), PressureLevel::Critical);
        for _ in 0..5 {
            clock.advance(1_000);
            t.update(Some(2.0), CPUS);
        }
        // Busy but not worse (6.0 = 75%: above the recovery line, below
        // WARNING): the good stretch breaks without changing the level.
        clock.advance(1_000);
        assert_eq!(t.update(Some(6.0), CPUS), PressureLevel::Critical);
        for _ in 0..10 {
            clock.advance(1_000);
            assert_eq!(t.update(Some(2.0), CPUS), PressureLevel::Critical);
        }
        clock.advance(1_000);
        assert_eq!(t.update(Some(2.0), CPUS), PressureLevel::Normal);
    }

    #[test]
    fn unknown_sample_holds_level_and_breaks_recovery() {
        let (mut t, clock) = tracker();
        t.update(Some(7.9), CPUS);
        clock.advance(1_000);
        assert_eq!(t.update(Some(7.9), CPUS), PressureLevel::Critical);
        for _ in 0..8 {
            clock.advance(1_000);
            t.update(Some(1.0), CPUS);
        }
        // Unmeasurable CPU: hold, and restart the window at the next sample.
        clock.advance(1_000);
        assert_eq!(t.update(None, CPUS), PressureLevel::Critical);
        clock.advance(1_000);
        // A known value with an unknown core count is unknown all the same.
        assert_eq!(t.update(Some(1.0), 0), PressureLevel::Critical);
        for _ in 0..10 {
            clock.advance(1_000);
            assert_eq!(t.update(Some(1.0), CPUS), PressureLevel::Critical);
        }
        clock.advance(1_000);
        assert_eq!(t.update(Some(1.0), CPUS), PressureLevel::Normal);
    }

    /// Unknown never *raises* anything either: a fresh tracker fed nothing
    /// but unknowns stays NORMAL (relief must not fire on a guess).
    #[test]
    fn unknown_never_raises_pressure() {
        let (mut t, clock) = tracker();
        for _ in 0..20 {
            clock.advance(1_000);
            assert_eq!(t.update(None, CPUS), PressureLevel::Normal);
            assert_eq!(t.update(Some(f64::NAN), CPUS), PressureLevel::Normal);
            assert_eq!(t.update(Some(99.0), 0), PressureLevel::Normal);
        }
        assert_eq!(t.level(), PressureLevel::Normal);
    }
}

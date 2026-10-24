//! Host memory pressure classification with hysteresis
//! (spec `03-resources.md` §3, 자원 거버넌스 계획의 임계 완화).
//!
//! Raw thresholds: `available < 1 GiB` → CRITICAL — the old `<10%` ratio
//! rule is off by default (`critical_available_percent: 0`): 큰 RAM 호스트에서
//! 압축·스왑이 잘 감당하는 데도 거짓 CRITICAL을 냈고, 실제 위기는 커널
//! 신호(`force_critical`)가 잡는다. `< 12%` → WARNING; else NORMAL.
//! Hysteresis: worsening requires 2 consecutive worse samples; recovery to
//! NORMAL requires `available/T >= 10%` **and** `available >= S` sustained
//! for `timing_ms.pressure_recovery` (default 10 s). CRITICAL steps down to
//! WARNING once the raw sample has left CRITICAL for the same window — 진입선
//! (1 GiB)과 NORMAL 회복선(S)이 멀어 그 사이에서 CRITICAL이 풀리지 않고 가드가
//! 계속 얼리던 래치를 없앤다.
//!
//! 커널 신호([`KernelMemorySignal`]): critical이면 즉시 CRITICAL이고 신호가
//! 남아 있는 동안 어떤 회복도 없다(macOS는 가용 정의가 넉넉해 페이지 수만
//! 보면 커널 위기 중에도 10초 만에 "회복"했다). warn이 지속되면
//! ([`KERNEL_WARN_SUSTAIN_SAMPLES`]) WARNING 수준으로 보고, warn인 동안은
//! NORMAL로 돌아가지 않는다.
//!
//! Staleness is admission's business, not pressure's: a stale sample simply
//! is not fed here, while admission returns `WAIT_TELEMETRY` for it. All math
//! is integer (`u128` intermediates for the percent ratios).

use term_contracts::defaults::Defaults;
use term_contracts::metrics::PressureLevel;

use crate::admission::host_reserve_bytes;
use crate::clock::Clock;

/// 커널의 메모리 압력 판정(플랫폼이 읽어 넘긴다 — 이 크레이트는 OS를 모른다).
/// macOS `kern.memorystatus_vm_pressure_level`(1/2/4), Linux PSI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KernelMemorySignal {
    /// 읽을 수 없다(미지원 플랫폼·권한·파싱 실패). 행동하지 않는다.
    #[default]
    Unknown,
    Normal,
    /// 압축·스왑이 일하기 시작했다. 흔하므로 지속될 때만 WARNING으로 본다.
    Warn,
    /// jetsam/OOM 기준의 위기.
    Critical,
}

/// 커널 warn이 이만큼 연속(1초 틱 기준 10초)이어야 WARNING 수준으로 본다.
/// macOS에서 warn은 흔하고 잠깐씩 오간다 — 한두 틱으로 반응하면 완화가
/// 비포커스 전부를 양보시켰다 풀기를 반복한다.
pub const KERNEL_WARN_SUSTAIN_SAMPLES: u32 = 10;

/// Thresholds from `defaults.json` (`admission.*`, `timing_ms.pressure_recovery`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PressureConfig {
    /// `critical_available_percent` (default 0 = 비율 규칙 끔. 실제 위기는
    /// 커널 신호가 잡고 CRITICAL의 절대선은 바이트 floor가 담당한다).
    pub critical_available_percent: u64,
    /// `critical_available_bytes` (default 1 GiB).
    pub critical_available_bytes: u64,
    /// `warning_available_percent` (default 12).
    pub warning_available_percent: u64,
    /// `recovery_available_percent` (default 10).
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

    /// Raw per-sample classification, no hysteresis. The byte floor binds
    /// first; the ratio rule (`available/T < pct`) is strict and inert at the
    /// default `pct = 0`; a zero total is broken telemetry and classifies
    /// conservatively as CRITICAL.
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
    /// CRITICAL에서 원시 분류가 CRITICAL을 벗어난(커널 critical도 아닌)
    /// 구간이 시작된 시각 — 같은 창이 지나면 WARNING으로 내려간다.
    critical_exit_since_ms: Option<u64>,
    /// 연속된 커널 warn 샘플 수.
    kernel_warn_streak: u32,
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
            critical_exit_since_ms: None,
            kernel_warn_streak: 0,
        }
    }

    pub fn config(&self) -> &PressureConfig {
        &self.config
    }

    pub fn level(&self) -> PressureLevel {
        self.level
    }

    /// Feed one host memory sample without a kernel signal; returns the
    /// effective level. [`Self::update_with_kernel`] with `Unknown`.
    pub fn update(&mut self, total_bytes: u64, available_bytes: Option<u64>) -> PressureLevel {
        self.update_with_kernel(total_bytes, available_bytes, KernelMemorySignal::Unknown)
    }

    /// Feed one host memory sample and the kernel's own verdict; returns the
    /// effective level.
    ///
    /// * Kernel critical raises CRITICAL immediately and blocks every recovery
    ///   path while it lasts.
    /// * Worsening flips only after 2 consecutive worse-than-current samples
    ///   (the pending level is the worst seen in the streak). A kernel warn
    ///   held for [`KERNEL_WARN_SUSTAIN_SAMPLES`] counts as a WARNING sample.
    /// * Any non-worse sample clears the worsening streak.
    /// * Recovery to NORMAL needs the sustained-good condition (and no kernel
    ///   warn) held for `recovery_sustain_ms` of monotonic time.
    /// * CRITICAL steps down to WARNING when the raw sample has stayed out of
    ///   CRITICAL for the same window (NORMAL wins when both complete).
    /// * `None` (unknown/unavailable measurement) holds the current level but
    ///   breaks the recovery stretches: sustained goodness must be observed.
    pub fn update_with_kernel(
        &mut self,
        total_bytes: u64,
        available_bytes: Option<u64>,
        kernel: KernelMemorySignal,
    ) -> PressureLevel {
        if kernel == KernelMemorySignal::Warn {
            self.kernel_warn_streak = self.kernel_warn_streak.saturating_add(1);
        } else {
            self.kernel_warn_streak = 0;
        }
        if kernel == KernelMemorySignal::Critical {
            self.force_critical();
            return self.level;
        }
        let Some(available) = available_bytes else {
            self.recovery_since_ms = None;
            self.critical_exit_since_ms = None;
            return self.level;
        };
        let mut raw = self.config.classify(total_bytes, available);
        if self.kernel_warn_streak >= KERNEL_WARN_SUSTAIN_SAMPLES
            && rank(raw) < rank(PressureLevel::Warning)
        {
            raw = PressureLevel::Warning;
        }
        if rank(raw) > rank(self.level) {
            self.recovery_since_ms = None;
            self.critical_exit_since_ms = None;
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
            return self.level;
        }
        self.worse_streak = 0;
        self.pending_worse = None;
        let now = self.clock.now_ms();
        let window = self.config.recovery_sustain_ms;
        // NORMAL 회복: 비율·예비량 게이트 + 커널 warn 아님.
        let normal_ok = self.level != PressureLevel::Normal
            && kernel != KernelMemorySignal::Warn
            && self.config.recovery_satisfied(total_bytes, available);
        if normal_ok {
            let start = *self.recovery_since_ms.get_or_insert(now);
            if now.saturating_sub(start) >= window {
                self.level = PressureLevel::Normal;
                self.recovery_since_ms = None;
                self.critical_exit_since_ms = None;
                return self.level;
            }
        } else {
            self.recovery_since_ms = None;
        }
        // CRITICAL → WARNING: 원시 분류가 CRITICAL을 벗어난 채 같은 창이 지났다.
        if self.level == PressureLevel::Critical && raw != PressureLevel::Critical {
            let start = *self.critical_exit_since_ms.get_or_insert(now);
            if now.saturating_sub(start) >= window {
                self.level = PressureLevel::Warning;
                self.critical_exit_since_ms = None;
            }
        } else {
            self.critical_exit_since_ms = None;
        }
        self.level
    }

    /// Native memory-pressure critical signal: raise to CRITICAL immediately
    /// (spec `03-resources.md` §3). Level-triggered contract: a persistent
    /// native signal must re-invoke this on every event — every call resets
    /// the recovery windows, so recovery starts only after the signal stops
    /// ([`Self::update_with_kernel`] does this from the kernel verdict).
    pub fn force_critical(&mut self) {
        self.level = PressureLevel::Critical;
        self.worse_streak = 0;
        self.pending_worse = None;
        self.recovery_since_ms = None;
        self.critical_exit_since_ms = None;
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
        assert_eq!(cfg.critical_available_percent, 0);
        assert_eq!(cfg.critical_available_bytes, GIB);
        assert_eq!(cfg.warning_available_percent, 12);
        assert_eq!(cfg.recovery_available_percent, 10);
        assert_eq!(cfg.recovery_sustain_ms, 10_000);
        assert_eq!(cfg.host_reserve_bytes(T16), 2_576_980_378);
    }

    #[test]
    fn raw_classification_thresholds_are_strict() {
        let cfg = config();
        // The old 10% ratio line is gone: a deep-but-absolute-fine ratio on a
        // big host is WARNING territory, never CRITICAL.
        assert_eq!(cfg.classify(T16, 1_717_986_918), PressureLevel::Warning);
        // Byte floor: one byte under 1 GiB is CRITICAL regardless of ratio.
        assert_eq!(cfg.classify(T16, GIB - 1), PressureLevel::Critical);
        assert_eq!(cfg.classify(T16, GIB), PressureLevel::Warning); // 6.25%
        assert_eq!(cfg.classify(8 * GIB, GIB - 1), PressureLevel::Critical);
        // Exactly 12% of 16 GiB is 2061584302.08 -> ...302 is under (WARNING),
        // ...303 is at-or-above (NORMAL).
        assert_eq!(cfg.classify(T16, 2_061_584_302), PressureLevel::Warning);
        assert_eq!(cfg.classify(T16, 2_061_584_303), PressureLevel::Normal);
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
        // 10.9% free(1.75 GiB of 16 GiB): WARNING raw.
        let warn = 1_879_048_192u64;
        assert_eq!(t.update(T16, Some(warn)), PressureLevel::Normal);
        assert_eq!(t.update(T16, Some(warn)), PressureLevel::Warning);
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
        let warn = 1_879_048_192u64;
        assert_eq!(t.update(T16, Some(warn)), PressureLevel::Normal); // WARNING raw
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
        // Good samples (4 GiB of 16 GiB: ≥10% and above S ≈ 2.4 GiB).
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
        // A bad-but-not-worse sample (NORMAL raw yet under the reserve S, so
        // the recovery gate fails) breaks the NORMAL stretch — but the raw
        // sample is still out of CRITICAL, so the step-down window (started
        // at the first good sample, +2 s) keeps running.
        clock.advance(1_000);
        assert_eq!(t.update(T16, Some(2_500_000_000)), PressureLevel::Critical);
        // NORMAL window restarts at the next good sample (+8 s). The
        // step-down completes first (+12 s): WARNING, not NORMAL yet.
        for _ in 0..4 {
            clock.advance(1_000);
            assert_eq!(t.update(T16, Some(4 * GIB)), PressureLevel::Critical);
        }
        for _ in 0..6 {
            clock.advance(1_000);
            assert_eq!(t.update(T16, Some(4 * GIB)), PressureLevel::Warning);
        }
        // +18 s: the restarted NORMAL window (from +8 s) completes.
        clock.advance(1_000);
        assert_eq!(t.update(T16, Some(4 * GIB)), PressureLevel::Normal);
    }

    #[test]
    fn recovery_also_requires_available_ge_reserve() {
        let (mut t, clock) = tracker();
        // 4 GiB host: S = 2 GiB floor; 25% = 1 GiB. 1.5 GiB free is 37.5%
        // (percent gate passes) but below S -> NORMAL recovery NOT satisfied.
        let small = 4 * GIB;
        t.update(small, Some(300 * 1024 * 1024));
        clock.advance(1_000);
        assert_eq!(
            t.update(small, Some(300 * 1024 * 1024)),
            PressureLevel::Critical
        );
        // The raw sample left CRITICAL: after the 10 s window the level steps
        // down to WARNING (it used to stay latched at CRITICAL while the
        // guard kept freezing), and holds WARNING while below S.
        for _ in 0..10 {
            clock.advance(1_000);
            assert_eq!(t.update(small, Some(3 * GIB / 2)), PressureLevel::Critical);
        }
        for _ in 0..10 {
            clock.advance(1_000);
            assert_eq!(t.update(small, Some(3 * GIB / 2)), PressureLevel::Warning);
        }
        // Above S the NORMAL window starts and completes after 10 s.
        for _ in 0..10 {
            clock.advance(1_000);
            assert_eq!(t.update(small, Some(2 * GIB)), PressureLevel::Warning);
        }
        clock.advance(1_000);
        assert_eq!(t.update(small, Some(2 * GIB)), PressureLevel::Normal);
    }

    #[test]
    fn critical_holds_while_the_raw_sample_stays_critical() {
        let (mut t, clock) = tracker();
        t.update(T16, Some(500 * 1024 * 1024));
        clock.advance(1_000);
        assert_eq!(
            t.update(T16, Some(500 * 1024 * 1024)),
            PressureLevel::Critical
        );
        // Still under the 1 GiB floor: no step-down, however long.
        for _ in 0..30 {
            clock.advance(1_000);
            assert_eq!(
                t.update(T16, Some(900 * 1024 * 1024)),
                PressureLevel::Critical
            );
        }
    }

    #[test]
    fn kernel_critical_raises_at_once_and_blocks_every_recovery() {
        let (mut t, clock) = tracker();
        // Plenty of available pages (macOS counts active+inactive+free), yet
        // the kernel says critical: CRITICAL immediately, no 2-sample wait.
        assert_eq!(
            t.update_with_kernel(T16, Some(12 * GIB), KernelMemorySignal::Critical),
            PressureLevel::Critical
        );
        // While the kernel keeps saying critical, neither NORMAL nor the
        // step-down can complete — it used to "recover" after 10 s and flap.
        for _ in 0..30 {
            clock.advance(1_000);
            assert_eq!(
                t.update_with_kernel(T16, Some(12 * GIB), KernelMemorySignal::Critical),
                PressureLevel::Critical
            );
        }
        // Kernel back to normal: the ordinary 10 s window applies.
        for _ in 0..10 {
            clock.advance(1_000);
            assert_eq!(
                t.update_with_kernel(T16, Some(12 * GIB), KernelMemorySignal::Normal),
                PressureLevel::Critical
            );
        }
        clock.advance(1_000);
        assert_eq!(
            t.update_with_kernel(T16, Some(12 * GIB), KernelMemorySignal::Normal),
            PressureLevel::Normal
        );
    }

    #[test]
    fn kernel_warn_counts_only_when_sustained_and_blocks_normal() {
        let (mut t, clock) = tracker();
        let warn = KernelMemorySignal::Warn;
        // Brief warns (common on macOS) do nothing.
        for _ in 0..(KERNEL_WARN_SUSTAIN_SAMPLES - 1) {
            clock.advance(1_000);
            assert_eq!(
                t.update_with_kernel(T16, Some(12 * GIB), warn),
                PressureLevel::Normal
            );
        }
        // One normal sample resets the streak.
        clock.advance(1_000);
        assert_eq!(
            t.update_with_kernel(T16, Some(12 * GIB), KernelMemorySignal::Normal),
            PressureLevel::Normal
        );
        // A sustained warn becomes a WARNING sample; the usual 2-sample rule
        // then flips the level.
        for _ in 0..KERNEL_WARN_SUSTAIN_SAMPLES {
            clock.advance(1_000);
            assert_eq!(
                t.update_with_kernel(T16, Some(12 * GIB), warn),
                PressureLevel::Normal
            );
        }
        clock.advance(1_000);
        assert_eq!(
            t.update_with_kernel(T16, Some(12 * GIB), warn),
            PressureLevel::Warning
        );
        // Good pages but the kernel still warns: no NORMAL recovery.
        for _ in 0..30 {
            clock.advance(1_000);
            assert_eq!(
                t.update_with_kernel(T16, Some(12 * GIB), warn),
                PressureLevel::Warning
            );
        }
        // Warn clears: 10 s later NORMAL.
        for _ in 0..10 {
            clock.advance(1_000);
            assert_eq!(
                t.update_with_kernel(T16, Some(12 * GIB), KernelMemorySignal::Normal),
                PressureLevel::Warning
            );
        }
        clock.advance(1_000);
        assert_eq!(
            t.update_with_kernel(T16, Some(12 * GIB), KernelMemorySignal::Normal),
            PressureLevel::Normal
        );
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

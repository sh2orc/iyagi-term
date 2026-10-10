//! Daemon configuration: spec defaults + optional test overrides.
//!
//! Production values come from `term_contracts::defaults` (the typed mirror
//! of `docs/implementation/defaults.json`). Integration tests override a
//! small set (session limit, managed concurrency, idle-exit window,
//! admission thresholds) by pointing the `IYAGI_TEST_CONFIG` environment
//! variable at a JSON file:
//!
//! ```json
//! {"limits": {"sessions": 3, "managed_concurrency": 1},
//!  "timing_ms": {"idle_daemon_exit": 30000},
//!  "admission": {"critical_available_percent": 0}}
//! ```
//!
//! Overrides are folded into the effective `Defaults` copy so downstream
//! builders (`AdmissionConfig::from_defaults`, `QueueConfig::from_defaults`)
//! all see the same numbers.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use term_contracts::defaults::{self, Defaults};

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Effective spec defaults (overrides applied).
    pub defaults: Defaults,
    /// Scheduler pump cadence (implementation constant).
    pub scheduler_interval: Duration,
    /// Where the override file came from, if any (diagnostics only).
    pub override_source: Option<PathBuf>,
    /// O1 mission orchestration feature gate. Declaration (`Capabilities.
    /// mission_protocol`) follows this flag; development builds default to
    /// on, release builds stay off until the O18 release gate. The env
    /// override only ever turns it OFF in dev, or fails to turn it on in
    /// release (O1 ticket O01).
    pub missions_enabled: bool,
    /// 연결당 미완료 `session.input` 완료 상한(ipc.rs). 종류별 독립 예산의
    /// 하나로, resize 예산과 함께 `limits.max_pending_*` 오버라이드로 낮춰
    /// 시험한다(기본 64).
    pub max_pending_inputs: usize,
    /// 연결당 미완료 `session.resize` 완료 상한(ipc.rs, 기본 64).
    pub max_pending_resizes: usize,
}

impl DaemonConfig {
    pub fn session_limit(&self) -> u32 {
        self.defaults.limits.sessions
    }
    pub fn managed_concurrency(&self) -> u32 {
        self.defaults.limits.managed_concurrency
    }
    pub fn journal_session_bytes(&self) -> u64 {
        self.defaults.limits.journal_session_bytes
    }
    pub fn journal_global_bytes(&self) -> u64 {
        self.defaults.limits.journal_global_bytes
    }
    /// defaults.json `journal_segment_bytes`(16 MiB): 롤링 저널 세그먼트
    /// 파일 크기의 상한(목표는 세션 상한/8, 이 값 이하).
    pub fn journal_segment_bytes(&self) -> u64 {
        self.defaults.limits.journal_segment_bytes.max(1)
    }
    /// defaults.json `journal_retention_days`(기본 7일) — 0도 최소 1일로
    /// 해석해 즉시 삭제 사이클을 만들지 않는다.
    pub fn journal_retention_days(&self) -> u32 {
        self.defaults.limits.journal_retention_days.max(1)
    }
    pub fn hello_timeout(&self) -> Duration {
        Duration::from_millis(self.defaults.timing_ms.hello_timeout.max(1))
    }
    pub fn rpc_timeout(&self) -> Duration {
        Duration::from_millis(self.defaults.timing_ms.rpc_timeout.max(1))
    }
    pub fn data_token_ttl(&self) -> Duration {
        Duration::from_millis(self.defaults.timing_ms.data_token_ttl.max(1))
    }
    pub fn gate_timeout(&self) -> Duration {
        Duration::from_millis(self.defaults.timing_ms.gate_timeout.max(1))
    }
    pub fn telemetry_interval(&self) -> Duration {
        Duration::from_millis(self.defaults.timing_ms.telemetry.max(1))
    }
    /// defaults.json `process_inventory`(2 s): 프로세스 트리 재열거 주기.
    /// 셸 세션 관측(08 §1)의 pid 공급자가 이 간격으로만 다시 훑는다.
    pub fn process_inventory_interval_ms(&self) -> u64 {
        self.defaults.timing_ms.process_inventory.max(1)
    }
    pub fn stop_grace(&self) -> Duration {
        Duration::from_millis(self.defaults.timing_ms.stop_grace.max(1))
    }
    pub fn idle_exit(&self) -> Duration {
        Duration::from_millis(self.defaults.timing_ms.idle_daemon_exit.max(1))
    }

    pub fn load() -> DaemonConfig {
        let defaults = defaults::load_spec_defaults().expect("spec defaults must be loadable");
        let mut config = DaemonConfig {
            defaults,
            scheduler_interval: Duration::from_millis(250),
            override_source: None,
            missions_enabled: missions_gate_default(),
            max_pending_inputs: 64,
            max_pending_resizes: 64,
        };
        if let Some(path) = std::env::var_os("IYAGI_TEST_CONFIG").map(PathBuf::from) {
            if let Ok(text) = std::fs::read_to_string(&path) {
                match serde_json::from_str::<OverrideFile>(&text) {
                    Ok(over) => {
                        over.apply_to(&mut config);
                        config.override_source = Some(path);
                    }
                    Err(e) => {
                        tracing::warn!(?path, error = %e, "ignored malformed IYAGI_TEST_CONFIG");
                    }
                }
            }
        }
        config
    }

    /// Admission config for this host's CPU count.
    pub fn admission_config(&self, logical_cpus: u32) -> term_core::AdmissionConfig {
        term_core::AdmissionConfig::from_defaults(&self.defaults, logical_cpus)
    }

    /// Queue config (capacity + aging).
    pub fn queue_config(&self) -> term_core::QueueConfig {
        term_core::QueueConfig::from_defaults(&self.defaults)
    }

    /// Pressure classifier config.
    pub fn pressure_config(&self) -> term_core::PressureConfig {
        term_core::PressureConfig::from_defaults(&self.defaults)
    }

    /// CPU saturation classifier config (spec `08-pressure-relief.md` §1).
    pub fn cpu_pressure_config(&self) -> term_core::CpuPressureConfig {
        term_core::CpuPressureConfig::from_defaults(&self.defaults)
    }

    /// Pressure-relief policy seed + release cadence (spec
    /// `08-pressure-relief.md` §2). The policy is only the *initial* value:
    /// `relief.set_policy` owns it from the first call on.
    pub fn relief_config(&self) -> ReliefConfig {
        ReliefConfig {
            policy: term_contracts::snapshot::ReliefPolicy {
                auto_yield: self.defaults.relief.auto_yield,
            },
            release_interval_ms: self.defaults.timing_ms.relief_release_interval.max(1),
        }
    }

    /// 08 §5: 가드 정책 초기값.
    pub fn guard_policy(&self) -> term_contracts::snapshot::GuardPolicy {
        self.defaults.resource_guard.clone()
    }
}

/// Startup values of the relief controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReliefConfig {
    pub policy: term_contracts::snapshot::ReliefPolicy,
    /// `timing_ms.relief_release_interval` — NORMAL 회복 뒤 한 번에 하나씩
    /// 복원하는 간격.
    pub release_interval_ms: u64,
}

#[derive(Debug, Default, Deserialize)]
struct OverrideFile {
    #[serde(default)]
    limits: LimitsOverride,
    #[serde(default)]
    timing_ms: TimingOverride,
    /// Admission-threshold overrides (test-only lever: acceptance suites
    /// must not depend on the host's transient memory pressure — the loaded
    /// developer machine may legitimately sit at CRITICAL pressure).
    #[serde(default)]
    admission: AdmissionOverride,
    /// CPU saturation thresholds (spec `08-pressure-relief.md` §1). Relief
    /// acceptance suites force a deterministic level instead of waiting for
    /// the host to really saturate.
    #[serde(default)]
    cpu_pressure: CpuPressureOverride,
    /// `relief.auto_yield` initial value (08 §2).
    #[serde(default)]
    relief: ReliefOverride,
}

#[derive(Debug, Default, Deserialize)]
struct CpuPressureOverride {
    warning_used_percent: Option<u64>,
    critical_used_percent: Option<u64>,
    recovery_used_percent: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct ReliefOverride {
    auto_yield: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct AdmissionOverride {
    host_reserve_min_bytes: Option<u64>,
    host_reserve_percent: Option<u64>,
    managed_budget_percent: Option<u64>,
    critical_available_percent: Option<u64>,
    critical_available_bytes: Option<u64>,
    warning_available_percent: Option<u64>,
    recovery_available_percent: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct LimitsOverride {
    sessions: Option<u32>,
    managed_concurrency: Option<u32>,
    queued_workloads: Option<u32>,
    journal_session_bytes: Option<u64>,
    journal_global_bytes: Option<u64>,
    journal_segment_bytes: Option<u64>,
    /// 연결당 pending 완료 예산(ipc.rs) — 제품 기본(64)에서 낮추는 시험 전용
    /// 레버. `DaemonConfig`에만 산다(spec defaults 미러는 그대로).
    max_pending_inputs: Option<u32>,
    max_pending_resizes: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
struct TimingOverride {
    hello_timeout: Option<u64>,
    rpc_timeout: Option<u64>,
    data_token_ttl: Option<u64>,
    gate_timeout: Option<u64>,
    telemetry: Option<u64>,
    idle_daemon_exit: Option<u64>,
    stop_grace: Option<u64>,
    relief_release_interval: Option<u64>,
}

impl OverrideFile {
    fn apply_to(&self, d: &mut DaemonConfig) {
        let defaults = &mut d.defaults;
        if let Some(v) = self.limits.sessions {
            defaults.limits.sessions = v.max(1);
        }
        if let Some(v) = self.limits.managed_concurrency {
            defaults.limits.managed_concurrency = v.max(1);
        }
        if let Some(v) = self.limits.queued_workloads {
            defaults.limits.queued_workloads = v.max(1);
        }
        if let Some(v) = self.limits.journal_session_bytes {
            defaults.limits.journal_session_bytes = v.max(1);
        }
        if let Some(v) = self.limits.journal_global_bytes {
            defaults.limits.journal_global_bytes = v.max(1);
        }
        if let Some(v) = self.limits.journal_segment_bytes {
            defaults.limits.journal_segment_bytes = v.max(1);
        }
        if let Some(v) = self.timing_ms.hello_timeout {
            defaults.timing_ms.hello_timeout = v.max(1);
        }
        if let Some(v) = self.timing_ms.rpc_timeout {
            defaults.timing_ms.rpc_timeout = v.max(1);
        }
        if let Some(v) = self.timing_ms.data_token_ttl {
            defaults.timing_ms.data_token_ttl = v.max(1);
        }
        if let Some(v) = self.timing_ms.gate_timeout {
            defaults.timing_ms.gate_timeout = v.max(1);
        }
        if let Some(v) = self.timing_ms.telemetry {
            defaults.timing_ms.telemetry = v.max(1);
        }
        if let Some(v) = self.timing_ms.idle_daemon_exit {
            defaults.timing_ms.idle_daemon_exit = v.max(1);
        }
        if let Some(v) = self.timing_ms.stop_grace {
            defaults.timing_ms.stop_grace = v.max(1);
        }
        if let Some(v) = self.admission.host_reserve_min_bytes {
            defaults.admission.host_reserve_min_bytes = v;
        }
        if let Some(v) = self.admission.host_reserve_percent {
            defaults.admission.host_reserve_percent = v;
        }
        if let Some(v) = self.admission.managed_budget_percent {
            defaults.admission.managed_budget_percent = v.min(100);
        }
        if let Some(v) = self.admission.critical_available_percent {
            defaults.admission.critical_available_percent = v;
        }
        if let Some(v) = self.admission.critical_available_bytes {
            defaults.admission.critical_available_bytes = v;
        }
        if let Some(v) = self.admission.warning_available_percent {
            defaults.admission.warning_available_percent = v;
        }
        if let Some(v) = self.admission.recovery_available_percent {
            defaults.admission.recovery_available_percent = v;
        }
        if let Some(v) = self.timing_ms.relief_release_interval {
            defaults.timing_ms.relief_release_interval = v.max(1);
        }
        if let Some(v) = self.cpu_pressure.warning_used_percent {
            defaults.cpu_pressure.warning_used_percent = v;
        }
        if let Some(v) = self.cpu_pressure.critical_used_percent {
            defaults.cpu_pressure.critical_used_percent = v;
        }
        if let Some(v) = self.cpu_pressure.recovery_used_percent {
            defaults.cpu_pressure.recovery_used_percent = v;
        }
        if let Some(v) = self.relief.auto_yield {
            defaults.relief.auto_yield = v;
        }
        if let Some(v) = self.limits.max_pending_inputs {
            d.max_pending_inputs = v.max(1) as usize;
        }
        if let Some(v) = self.limits.max_pending_resizes {
            d.max_pending_resizes = v.max(1) as usize;
        }
    }
}

/// O1 feature gate default (ticket O01): development builds may run the
/// mission engine; release builds advertise nothing until O18. `IYAGI_MISSION_
/// PROTOCOL=0` forces the gate off (used to prove the off-state leaves R1
/// untouched); `=1` cannot force it on in a release build.
fn missions_gate_default() -> bool {
    if cfg!(debug_assertions) {
        std::env::var_os("IYAGI_MISSION_PROTOCOL")
            .map(|v| v != "0")
            .unwrap_or(true)
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// IYAGI_TEST_CONFIG is process-global; tests touching it must not race.
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 완화 수용 시험은 호스트가 실제로 포화하기를 기다릴 수 없다:
    /// `cpu_pressure`·`relief` 덮어쓰기로 level과 정책을 결정적으로 만든다.
    #[test]
    fn relief_and_cpu_pressure_overrides_apply() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("relief.json");
        std::fs::write(
            &path,
            r#"{"cpu_pressure":{"warning_used_percent":0,"critical_used_percent":200},
                "relief":{"auto_yield":false},
                "timing_ms":{"relief_release_interval":250}}"#,
        )
        .expect("write");
        std::env::set_var("IYAGI_TEST_CONFIG", &path);
        let cfg = DaemonConfig::load();
        std::env::remove_var("IYAGI_TEST_CONFIG");

        let relief = cfg.relief_config();
        assert!(!relief.policy.auto_yield);
        assert_eq!(relief.release_interval_ms, 250);
        // 0% WARNING 선이면 어떤 사용률이든 WARNING 후보다(2-sample 악화는
        // tracker가 따로 요구한다).
        let cpu = cfg.cpu_pressure_config();
        assert_eq!(
            cpu.classify(0.0, 8),
            Some(term_contracts::metrics::PressureLevel::Warning)
        );
    }

    #[test]
    fn overrides_apply_when_env_points_at_file() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cfg.json");
        std::fs::write(
            &path,
            r#"{"limits":{"sessions":3,"managed_concurrency":1},"timing_ms":{"idle_daemon_exit":50}}"#,
        )
        .expect("write");
        // Scoped env var (single-writer for IYAGI_TEST_CONFIG in this process).
        std::env::set_var("IYAGI_TEST_CONFIG", &path);
        let cfg = DaemonConfig::load();
        std::env::remove_var("IYAGI_TEST_CONFIG");
        assert_eq!(cfg.session_limit(), 3);
        assert_eq!(cfg.managed_concurrency(), 1);
        assert_eq!(cfg.idle_exit(), Duration::from_millis(50));
        assert!(cfg.override_source.is_some());
        // The effective defaults feed admission too.
        let admission = cfg.admission_config(8);
        assert_eq!(admission.managed_concurrency, 1);
    }

    #[test]
    fn admission_overrides_relax_pressure_and_reserve_gates() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("relaxed.json");
        std::fs::write(
            &path,
            r#"{"admission":{"critical_available_percent":0,"critical_available_bytes":0,
                "warning_available_percent":0,"recovery_available_percent":0,
                "host_reserve_min_bytes":1,"host_reserve_percent":0,
                "managed_budget_percent":100}}"#,
        )
        .expect("write");
        std::env::set_var("IYAGI_TEST_CONFIG", &path);
        let cfg = DaemonConfig::load();
        std::env::remove_var("IYAGI_TEST_CONFIG");
        let pressure = cfg.pressure_config();
        // Even a starved host (available < 1 GiB, < 10%) classifies Normal.
        assert_eq!(
            pressure.classify(64 << 30, 512 << 20),
            term_contracts::metrics::PressureLevel::Normal
        );
        assert!(pressure.recovery_satisfied(64 << 30, 1));
        // Budget: 100% of total; reserve floor 1 byte.
        let admission = cfg.admission_config(8);
        assert_eq!(admission.managed_budget_bytes(10 << 30), 10 << 30);
        assert_eq!(admission.host_reserve_bytes(10 << 30), 1);
    }
}

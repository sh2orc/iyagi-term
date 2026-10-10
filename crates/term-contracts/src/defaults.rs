//! Typed mirror of `docs/implementation/defaults.json`.
//!
//! The JSON file under `docs/implementation/` is the specification asset; this
//! module is the product's runtime source of truth. A unit test asserts both
//! stay identical so a spec edit can never silently drift from the daemon.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Defaults {
    pub spec_version: u32,
    pub limits: Limits,
    pub timing_ms: TimingMs,
    pub admission: AdmissionDefaults,
    /// CPU saturation thresholds (spec `08-pressure-relief.md` §1). Older
    /// spec assets omit the block: the defaults then apply unchanged.
    #[serde(default)]
    pub cpu_pressure: CpuPressureDefaults,
    /// `relief.*` — 압력 완화 정책의 초기값(spec `08-pressure-relief.md`
    /// §2). 구 spec asset은 블록을 생략하며 그때는 기본값이 그대로 쓰인다.
    #[serde(default)]
    pub relief: ReliefDefaults,
    /// `resource_guard.*` — 자원 가드 정책의 초기값(spec `08-pressure-relief.md`
    /// §5). 구 spec asset은 블록을 생략하며 그때는 기본값이 그대로 쓰인다.
    #[serde(default)]
    pub resource_guard: crate::snapshot::GuardPolicy,
    pub ui: UiDefaults,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Limits {
    pub sessions: u32,
    pub panes_per_tab: u32,
    pub views_per_session: u32,
    pub managed_concurrency: u32,
    pub queued_workloads: u32,
    pub rpc_frame_bytes: u64,
    pub launch_argument_bytes: u64,
    pub launch_argv_count: u32,
    pub output_chunk_bytes: u64,
    pub output_high_bytes: u64,
    pub output_low_bytes: u64,
    pub output_raw_global_bytes: u64,
    pub transport_global_bytes: u64,
    pub input_chunk_bytes: u64,
    pub input_queue_bytes: u64,
    pub paste_bytes: u64,
    pub input_dedup_entries: u32,
    pub journal_session_bytes: u64,
    pub journal_global_bytes: u64,
    /// Rolling journal: cap on one segment file (the target is an eighth
    /// of `journal_session_bytes`, clamped to this).
    pub journal_segment_bytes: u64,
    pub journal_retention_days: u32,
    pub scrollback_lines: u32,
    pub graph_samples: u32,
    pub control_queue_entries: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TimingMs {
    pub rpc_timeout: u64,
    pub hello_timeout: u64,
    pub data_token_ttl: u64,
    pub daemon_start_timeout: u64,
    pub gate_timeout: u64,
    pub telemetry: u64,
    pub process_inventory: u64,
    pub background_process_detail: u64,
    pub disk_capacity: u64,
    pub telemetry_stale: u64,
    pub resize_coalesce: u64,
    pub ack_coalesce: u64,
    pub journal_flush: u64,
    pub stop_grace: u64,
    pub exit_drain: u64,
    pub idle_daemon_exit: u64,
    pub priority_aging: u64,
    pub pressure_recovery: u64,
    /// 완화 해제 간격(08 §2): NORMAL 회복 뒤 한 번에 하나씩 이 간격으로
    /// 복원한다. 구 spec asset은 이 키를 생략한다(기본 3 s).
    #[serde(default = "default_relief_release_interval")]
    pub relief_release_interval: u64,
}

fn default_relief_release_interval() -> u64 {
    3_000
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AdmissionDefaults {
    pub reservation_bytes: u64,
    pub cpu_slots: u32,
    pub enforcement: String,
    pub memory_max_bytes: Option<u64>,
    pub cpu_max_cores: Option<f64>,
    pub pids_max: Option<u32>,
    pub host_reserve_min_bytes: u64,
    pub host_reserve_percent: u64,
    pub managed_budget_percent: u64,
    pub critical_available_percent: u64,
    pub critical_available_bytes: u64,
    pub warning_available_percent: u64,
    pub recovery_available_percent: u64,
}

/// `cpu_pressure.*` — CPU 포화도 분류 임계값(사용 코어/논리 코어 비율, %).
/// 메모리 압력과 달리 "여유"가 아니라 "사용률" 기준이다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CpuPressureDefaults {
    /// WARNING line (default 85% of the logical cores busy).
    pub warning_used_percent: u64,
    /// CRITICAL line (default 95%).
    pub critical_used_percent: u64,
    /// Sustained-good line for the recovery window (default 70%); the
    /// sustain duration reuses `timing_ms.pressure_recovery`.
    pub recovery_used_percent: u64,
}

impl Default for CpuPressureDefaults {
    fn default() -> Self {
        Self {
            warning_used_percent: 85,
            critical_used_percent: 95,
            recovery_used_percent: 70,
        }
    }
}

/// `relief.*` — 완화 수단의 자동 적용 스위치. P2는 양보 하나다.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ReliefDefaults {
    /// CPU 압력 ≥ WARNING에서 비포커스·비보호 세션을 자동 양보시킨다.
    pub auto_yield: bool,
}

impl Default for ReliefDefaults {
    fn default() -> Self {
        Self { auto_yield: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct UiDefaults {
    pub leaf_min_width_px: u32,
    pub leaf_min_height_px: u32,
    pub pane_header_px: u32,
    pub divider_px: u32,
    pub resource_strip_px: u32,
    pub font_size_px: u32,
    pub line_height: f64,
}

/// `docs/implementation/defaults.json` 원문. 빌드에 내장해 설치한 PC에서도
/// 저장소와 같은 값으로 돈다(예전에는 빌드 시점 경로를 런타임에 읽어
/// 저장소 밖에서는 fallback 값이 쓰였다).
pub const SPEC_DEFAULTS_JSON: &str = include_str!("../../../docs/implementation/defaults.json");

pub fn load_spec_defaults() -> Option<Defaults> {
    serde_json::from_str(SPEC_DEFAULTS_JSON).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Product constants and the spec asset must not drift apart.
    #[test]
    fn spec_defaults_parse_and_match_hardcoded_limits() {
        let defaults = load_spec_defaults().expect("docs/implementation/defaults.json must parse");
        assert_eq!(defaults.spec_version, 1);
        assert_eq!(defaults.limits.managed_concurrency, 2);
        assert_eq!(defaults.limits.sessions, 32);
        assert_eq!(defaults.limits.output_chunk_bytes, 16_384);
        assert_eq!(defaults.limits.journal_session_bytes, 128 << 20);
        assert_eq!(defaults.limits.journal_segment_bytes, 16 << 20);
        // Enforced as the per-connection outbound IPC queue bound (ipc.rs).
        assert_eq!(defaults.limits.control_queue_entries, 128);
        assert_eq!(defaults.timing_ms.telemetry_stale, 3_000);
        assert_eq!(defaults.admission.reservation_bytes, 2 << 30);
        assert_eq!(defaults.admission.host_reserve_min_bytes, 2 << 30);
        assert_eq!(defaults.admission.managed_budget_percent, 50);
        assert_eq!(defaults.cpu_pressure.warning_used_percent, 85);
        assert_eq!(defaults.cpu_pressure.critical_used_percent, 95);
        assert_eq!(defaults.cpu_pressure.recovery_used_percent, 70);
        assert!(defaults.relief.auto_yield);
        assert_eq!(defaults.timing_ms.relief_release_interval, 3_000);
        assert_eq!(defaults.ui.leaf_min_width_px, 240);
        assert_eq!(defaults.ui.divider_px, 4);
        // Invariants the spec verifier also asserts.
        let l = &defaults.limits;
        assert!(
            l.output_chunk_bytes < l.output_low_bytes && l.output_low_bytes < l.output_high_bytes
        );
        assert!(l.output_high_bytes <= l.output_raw_global_bytes);
        assert!(l.input_chunk_bytes <= l.input_queue_bytes && l.input_queue_bytes < l.paste_bytes);
        assert!(l.journal_session_bytes <= l.journal_global_bytes);
        assert!(l.journal_segment_bytes <= l.journal_session_bytes);
        // One max-size base64 chunk + envelope must fit an RPC frame.
        assert!(4 * l.output_chunk_bytes.div_ceil(3) + 1024 < l.rpc_frame_bytes);
    }
}

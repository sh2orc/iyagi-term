// 관리 exec 슈퍼바이저 픽스처: 결정적인 어드미션 호스트(16GiB/12GiB)와 게이트
// helper로 감싼 ExecSupervisor를 만든다. macOS 전용 검증 격리(sandbox-exec)를
// 돌리는 시험 묶음(mission_verification_exec·mission_integration_exec)과 그
// 픽스처만 빌리는 mission_usability가 함께 쓴다 — 모듈 자체는 어느 OS에서나
// 컴파일되어야 해서 여기 따로 둔다.
use super::*;
use iyagi_termd_lib::exec::{gated::GateConfig, ExecSupervisor};

pub(super) fn supervisor(rig: &Rig, service: &Arc<MissionService>) -> Arc<ExecSupervisor> {
    Arc::new(ExecSupervisor::persistent(
        term_core::AdmissionConfig {
            logical_cpus: 8,
            managed_concurrency: 2,
            telemetry_stale_ms: 3000,
            host_reserve_min_bytes: 2 << 30,
            host_reserve_percent: 15,
            managed_budget_percent: 50,
        },
        service.exec_persistence(),
        term_core::AdmissionHost {
            total_bytes: 16 << 30,
            available_bytes: Some(12 << 30),
            sample_age_ms: 0,
            reconciliation_required: false,
            pressure: term_contracts::metrics::PressureLevel::Normal,
        },
        GateConfig {
            helper_program: env!("CARGO_BIN_EXE_iyagi-termd").into(),
            directory: rig.dir.path().join("gates"),
            platform: Arc::from(term_platform::group::select_backend()),
            // 게이트 핸드셰이크 기한: 병렬 시험 폭풍에서 helper 기동이 밀려도
            // 실패가 아니라 느림으로만 보이게 넉넉히 잡는다.
            timeout: Duration::from_secs(15),
        },
    ))
}

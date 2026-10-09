/**
 * 실행 종료 증거(reconciliation)와 멈춘 실행 사용자 확인 정리(계약 C) 판정.
 *
 * - 데몬 `Run::holds_execution_slot`(term-contracts states.rs)의 사본: live 상태이거나,
 *   불확실(unknown/interrupted) 상태인데 종료 증거(reconciliation_ref)가 없다.
 * - 사용자 확인 정리(`mission.run.attest_exited`)의 대상은 데몬 `attestation_target`과 같이
 *   종료 증거 없는 unknown/interrupted 실행뿐이다(멈추는 중 실행은 데몬이 아직 감독한다).
 *   그 밖의 거절(이 데몬이 아직 감독 중인 프로세스 등)은 `attestation_not_applicable`로 온다.
 * - 사용자 확인으로 남은 증거는 `Run.reconciliation_kind = "user_attested"`로 구분한다
 *   (관측한 종료는 `exec_exited` 또는 필드 없음). 제공자 결과·외부 영향은 여전히 미확인이다.
 */

import type { ExecRecord } from "../../generated/ExecRecord";
import type { Run } from "../../generated/Run";

/** 사용자가 확인했다고 체크한 문장 키(감사 기록용, `[a-z0-9_]{1,64}`). */
export const PROCESS_ABSENT_ATTESTATION = "process_absent_confirmed";
/** 사용자 확인으로 기록된 reconciliation 종류. */
export const USER_ATTESTED_RECONCILIATION = "user_attested";

const LIVE_STATES: readonly string[] = ["prepared", "starting", "running", "awaiting_input", "stopping"];
const UNCERTAIN_STATES: readonly string[] = ["unknown", "interrupted"];

type RunFacts = Pick<Run, "state" | "reconciliation_ref">;

/** 데몬 `Run::holds_execution_slot`. */
export function runHoldsExecutionSlot(run: RunFacts): boolean {
  return LIVE_STATES.includes(run.state) || (UNCERTAIN_STATES.includes(run.state) && run.reconciliation_ref === null);
}

/**
 * 종료 증거 없이 실행 자리를 잡은 불확실 실행(unknown/interrupted + 증거 없음) — 사용자 확인
 * 정리 동선을 보이고, 전송 직전 최신 store에서 다시 검사하는 조건이다.
 */
export function runAwaitsExitAttestation(run: RunFacts | null | undefined): boolean {
  return run != null && UNCERTAIN_STATES.includes(run.state) && run.reconciliation_ref === null;
}

/**
 * 데몬 `exec_store::has_recoverable_group`의 사본: 프로세스 신원·시작 시각·그룹 참조와 함께
 * 되찾을 수 있는 그룹 신원(Linux cgroup v2 / macOS guardian)이 있으면 데몬 native recovery가
 * 그 그룹을 다시 감시하므로, 사용자 확인 정리는 `attestation_not_applicable`로 거절된다.
 */
export function execHasRecoverableGroup(exec: ExecRecord | null | undefined): boolean {
  if (!exec || exec.identity === null || exec.started_at === null || exec.group_reference === null) return false;
  const kind = exec.group_identity?.kind ?? null;
  return (kind === "cgroup_v2" && exec.group_kind === "cgroup")
    || (kind === "macos_guardian" && exec.group_kind === "observed_tree");
}

/** 사용자 확인으로 정리된 실행인가(종료 증거가 있고 그 종류가 user_attested). */
export function runUserAttested(run: (RunFacts & Pick<Run, "reconciliation_kind">) | null | undefined): boolean {
  return run != null && run.reconciliation_ref !== null && run.reconciliation_kind === USER_ATTESTED_RECONCILIATION;
}

/** 이 작업에서 실행 자리를 잡고 있는 실행 수(종료 확인 대기). */
export function selectSlotHoldingRunCount(state: { runs: Record<string, Run> }, missionId: string): number {
  let count = 0;
  for (const run of Object.values(state.runs)) {
    if (run.mission_id === missionId && runHoldsExecutionSlot(run)) count += 1;
  }
  return count;
}

/**
 * 종료 확인을 기다리는 첫 실행: 증거 없는 불확실 실행(직접 확인 동선이 있는 실행) → 멈추는 중
 * → 그 밖의 live 실행. 같은 묶음 안에서는 시작 순(시작 전 실행은 뒤로), 같으면 id 순.
 */
export function selectFirstRunAwaitingExit(state: { runs: Record<string, Run> }, missionId: string): string | null {
  const rank = (run: Run) => (runAwaitsExitAttestation(run) ? 0 : run.state === "stopping" ? 1 : 2);
  const startKey = (run: Run) => run.started_at ?? "￿";
  let first: Run | null = null;
  for (const run of Object.values(state.runs)) {
    if (run.mission_id !== missionId || !runHoldsExecutionSlot(run)) continue;
    if (
      first === null
      || rank(run) < rank(first)
      || (rank(run) === rank(first) && (startKey(run) < startKey(first) || (startKey(run) === startKey(first) && run.id < first.id)))
    ) {
      first = run;
    }
  }
  return first?.id ?? null;
}

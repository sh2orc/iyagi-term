/**
 * mission 요약 → 앱 셸 표시(작업 목록 그룹·탭 배지).
 *
 * 순수 함수만 둔다 — `mission.list` 요약(Mission)만으로도 판정되어야 한다.
 * 백그라운드 탭은 snapshot을 뽑지 않으므로 live Run 수는 모를 수 있다(null).
 */

import type { Mission } from "../../generated/Mission";
import type { MissionState } from "../../generated/MissionState";
import { selectLiveRunCount, selectOpenDecisionCount } from "./selectors";
import type { MissionStoreState } from "./store";

const FINISHED_STATES: ReadonlySet<MissionState> = new Set<MissionState>(["completed", "failed", "cancelled"]);
const RUNNING_STATES: ReadonlySet<MissionState> = new Set<MissionState>(["running", "pausing", "stopping"]);

export function isFinishedMissionState(state: MissionState): boolean {
  return FINISHED_STATES.has(state);
}

/** 작업 목록의 그룹(표시 순서 = 이 배열 순서). 보관됨은 별도 목록이다. */
export const MISSION_LIST_GROUPS = ["decision", "acceptance", "active", "finished", "archived"] as const;
export type MissionListGroup = (typeof MISSION_LIST_GROUPS)[number];

/**
 * 한 mission이 속하는 그룹. 끝난 작업은 남은 결정 수와 무관하게 "끝남"이다 —
 * 더 답할 수 없는 질문을 "결정 필요"로 부르지 않는다.
 */
export function missionListGroup(mission: Pick<Mission, "state" | "phase" | "open_decision_count" | "archived_at">): MissionListGroup {
  if (mission.archived_at !== null) return "archived";
  if (FINISHED_STATES.has(mission.state)) return "finished";
  if (mission.open_decision_count > 0) return "decision";
  if (mission.phase === "awaiting_acceptance") return "acceptance";
  return "active";
}

/** 탭 배지(상태 우선): 결정 필요 N > 확정 대기 > 실패 > 실행 중 N > 완료. */
export type MissionTabBadge =
  | { kind: "decision"; count: number }
  | { kind: "acceptance" }
  | { kind: "failed" }
  | { kind: "running"; count: number | null }
  | { kind: "done" };

export interface MissionTabBadgeInput {
  state: MissionState | null;
  phase: Mission["phase"] | null;
  /** 열린 결정 수(snapshot이 있으면 그 개수, 없으면 요약의 open_decision_count). */
  decisions: number;
  /** live Run 수 — snapshot을 뽑지 않은 탭은 모른다(null). */
  liveRuns: number | null;
}

export function missionTabBadge(input: MissionTabBadgeInput): MissionTabBadge | null {
  const finished = input.state !== null && FINISHED_STATES.has(input.state);
  if (!finished && input.decisions > 0) return { kind: "decision", count: input.decisions };
  if (!finished && input.phase === "awaiting_acceptance") return { kind: "acceptance" };
  if (input.state === "failed") return { kind: "failed" };
  const liveRuns = input.liveRuns !== null && input.liveRuns > 0 ? input.liveRuns : null;
  // 요약이 아직 없어도(state null) 살아 있는 실행이 보이면 실행 중이다.
  if ((input.state !== null && RUNNING_STATES.has(input.state)) || (input.state === null && liveRuns !== null)) {
    return { kind: "running", count: liveRuns };
  }
  if (input.state === "completed") return { kind: "done" };
  return null;
}

/**
 * store에서 탭 배지를 고른다. snapshot을 뽑은 작업은 snapshot의 결정·실행 수를,
 * 아직 안 뽑은 백그라운드 탭은 `mission.list` 요약(open_decision_count·state·phase)을 쓴다.
 */
export function selectMissionTabBadge(
  state: Pick<MissionStoreState, "missions" | "sync" | "decisions" | "runs">,
  missionId: string,
): MissionTabBadge | null {
  const mission = state.missions[missionId];
  const status = state.sync[missionId];
  const snapshotBacked = status !== undefined && status.atSeq !== "0";
  const full = state as MissionStoreState;
  return missionTabBadge({
    state: mission?.state ?? null,
    phase: mission?.phase ?? null,
    decisions: snapshotBacked ? selectOpenDecisionCount(full, missionId) : (mission?.open_decision_count ?? 0),
    liveRuns: snapshotBacked ? selectLiveRunCount(full, missionId) : null,
  });
}

/**
 * 배지를 원시 문자열 하나로 — zustand selector가 객체를 새로 만들면 매 갱신마다
 * 모든 탭이 다시 그려진다(selectors.ts와 같은 이유).
 */
export function encodeMissionTabBadge(badge: MissionTabBadge | null): string {
  if (!badge) return "";
  switch (badge.kind) {
    case "decision":
      return `decision:${badge.count}`;
    case "running":
      return badge.count === null ? "running" : `running:${badge.count}`;
    default:
      return badge.kind;
  }
}

export function decodeMissionTabBadge(encoded: string): MissionTabBadge | null {
  if (encoded === "") return null;
  const [kind, rawCount] = encoded.split(":");
  const count = rawCount === undefined ? null : Number(rawCount);
  switch (kind) {
    case "decision":
      return { kind: "decision", count: count ?? 0 };
    case "running":
      return { kind: "running", count };
    case "acceptance":
      return { kind: "acceptance" };
    case "failed":
      return { kind: "failed" };
    case "done":
      return { kind: "done" };
    default:
      return null;
  }
}

/** 저장소 경로의 마지막 조각(목록 행의 저장소 이름). 윈도 구분자도 받는다. */
export function repositoryName(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, "");
  const parts = trimmed.split(/[\\/]/);
  return parts[parts.length - 1] || trimmed || path;
}

/**
 * 두 요약 중 더 새로운 쪽(revision 비교). 목록 요약이 늦게 도착해 이미 적용된
 * snapshot의 mission을 되돌리지 않게 한다. revision이 같으면 들어온 쪽을 쓴다.
 */
export function newerMission(current: Mission | undefined, incoming: Mission): Mission {
  if (!current) return incoming;
  try {
    return BigInt(incoming.revision) >= BigInt(current.revision) ? incoming : current;
  } catch {
    return incoming;
  }
}

/**
 * missionUiStore(05-ui §2): mission별 화면 상태.
 *
 * selectedTaskId/selectedRunId/detailTab/필터/검색/분할 비율을 mission
 * 키로 저장한다. 실행 상태·대화 원문은 절대 넣지 않고(05 §2) localStorage
 * 에도 persist하지 않는다 — 탭을 다시 열면 선택 없음 상태에서 시작해도
 * 계약상 문제없다(빈 상태 구분만 정확하면 된다).
 */

import { create } from "zustand";
import type { Phase } from "../../generated/Phase";

export type TeamFilter = "all" | "running" | "awaiting" | "waiting" | "done";
export type DetailTab = "activity" | "exec" | "changes" | "verification";
/** <760px 단일 영역 탐색(05 §7): 현재 보여 주는 영역. */
export type NarrowView = "lead" | "team" | "result";
export type RightPane = "lead" | "detail" | "result";

export interface MissionUiState {
  /** Consumed once by the mounted mission header after creation. */
  startRequested: boolean;
  selectedTaskId: string | null;
  selectedRunId: string | null;
  /**
   * 선택을 사용자가 직접 했는가. 자동 선택(실행 중인 Lead·첫 실행 중 할 일)은
   * 사용자 선택을 덮지 않고, 사용자가 고른 뒤에는 자동 선택이 끼어들지 않는다.
   */
  selectedByUser: boolean;
  detailTab: DetailTab;
  filter: TeamFilter;
  search: string;
  /** Lead pane 비율(0.45..0.7, 기본 0.6 — 05 §1/§7). */
  leadWidthRatio: number;
  /** 오른쪽 목록:상세 높이 비율(기본 0.4 — 05 §7). */
  teamListHeightRatio: number;
  /** <760px에서 현재 영역. */
  narrowView: NarrowView;
  /** 760..1099 side sheet가 열려 있는가(선택 시에만 연다 — 05 §7). */
  sheetOpen: boolean;
  /**
   * 사용자가 직접 고른 오른쪽 아래 영역(실행 상세 / 결과 검토). null이면 고른 적이
   * 없다 — 화면은 단계 기본값(인수 대기·완료면 결과, 아니면 상세)을 쓴다.
   * 인수 대기로 바뀌는 순간 한 번 null로 되돌려 결과 화면을 보이고, 그 뒤 사용자가
   * 다른 곳을 고르면 그 선택을 존중한다(05 §9).
   */
  rightPane: RightPane | null;
  /** 이 화면이 마지막으로 관찰한 phase — 인수 대기로 "바뀌는 순간"을 한 번만 잡는다. */
  observedPhase: Phase | null;
}

export const LEAD_RATIO_MIN = 0.45;
export const LEAD_RATIO_MAX = 0.7;
export const LEAD_RATIO_DEFAULT = 0.6;
export const LIST_RATIO_MIN = 160;
export const DETAIL_RATIO_MIN = 200;

export interface MissionUiStoreState {
  perMission: Record<string, MissionUiState>;

  getUi(missionId: string): MissionUiState;
  patchUi(missionId: string, patch: Partial<MissionUiState>): void;
  /** 사용자의 task 선택(같은 행 재클릭은 유지 — 목록 흔들기 없음). */
  selectTask(missionId: string, taskId: string | null): void;
  /**
   * 자동 선택 — 선택이 비어 있을 때만 채운다. side sheet는 열지 않는다(중간 폭에서
   * 사용자가 고르기 전에는 상세가 목록을 가리지 않는다 — 05 §7).
   */
  autoSelectTask(missionId: string, taskId: string): void;
  /** 할 일 하나를 골라 상세를 연다(결정·링크에서 이동). */
  openTaskDetail(missionId: string, taskId: string): void;
  /** 사용자가 오른쪽 아래 영역을 직접 고른다. */
  chooseRightPane(missionId: string, pane: RightPane): void;
  /** phase 관찰 — 인수 대기로 바뀌는 순간 결과 화면(좁은 폭은 결과 탭)으로 한 번 전환한다. */
  observePhase(missionId: string, phase: Phase): void;
  closeSheet(missionId: string): void;
}

export function defaultMissionUi(): MissionUiState {
  return {
    startRequested: false,
    selectedTaskId: null,
    selectedRunId: null,
    selectedByUser: false,
    detailTab: "activity",
    filter: "all",
    search: "",
    leadWidthRatio: LEAD_RATIO_DEFAULT,
    teamListHeightRatio: 0.4,
    narrowView: "lead",
    sheetOpen: false,
    rightPane: null,
    observedPhase: null,
  };
}

export function clampLeadRatio(ratio: number): number {
  if (!Number.isFinite(ratio)) return LEAD_RATIO_DEFAULT;
  return Math.min(LEAD_RATIO_MAX, Math.max(LEAD_RATIO_MIN, ratio));
}

/** 사용자 선택이 없을 때 오른쪽 아래 영역의 단계 기본값. */
export function defaultRightPane(phase: Phase, missionState: string): RightPane {
  return phase === "awaiting_acceptance" || missionState === "completed" ? "result" : "detail";
}

export const useMissionUiStore = create<MissionUiStoreState>((set, get) => ({
  perMission: {},

  getUi: (missionId) => get().perMission[missionId] ?? defaultMissionUi(),

  patchUi: (missionId, patch) =>
    set((s) => {
      const current = s.perMission[missionId] ?? defaultMissionUi();
      return { perMission: { ...s.perMission, [missionId]: { ...current, ...patch } } };
    }),

  selectTask: (missionId, taskId) => {
    const current = get().perMission[missionId] ?? defaultMissionUi();
    // 이미 같은 task가 선택돼 있으면 상태를 그대로 둔다(불필요한 리렌더·
    // 스크럽 방지). 다른 작업을 고르면 실행 기록부터 보여 준다.
    if (current.selectedTaskId === taskId && current.sheetOpen && current.selectedByUser && current.rightPane === "detail") return;
    set({
      perMission: {
        ...get().perMission,
        [missionId]: {
          ...current,
          selectedTaskId: taskId,
          selectedRunId: null,
          rightPane: "detail",
          detailTab: "activity",
          narrowView: "team",
          selectedByUser: taskId !== null,
          sheetOpen: taskId !== null,
        },
      },
    });
  },

  autoSelectTask: (missionId, taskId) => {
    const current = get().perMission[missionId] ?? defaultMissionUi();
    if (current.selectedTaskId !== null) return;
    set({
      perMission: {
        ...get().perMission,
        [missionId]: { ...current, selectedTaskId: taskId, selectedRunId: null, selectedByUser: false },
      },
    });
  },

  openTaskDetail: (missionId, taskId) => {
    const current = get().perMission[missionId] ?? defaultMissionUi();
    set({
      perMission: {
        ...get().perMission,
        [missionId]: {
          ...current,
          selectedTaskId: taskId,
          selectedRunId: current.selectedTaskId === taskId ? current.selectedRunId : null,
          selectedByUser: true,
          sheetOpen: true,
          rightPane: "detail",
          detailTab: "activity",
          narrowView: "team",
        },
      },
    });
  },

  chooseRightPane: (missionId, pane) => {
    const current = get().perMission[missionId] ?? defaultMissionUi();
    if (current.rightPane === pane) return;
    set({ perMission: { ...get().perMission, [missionId]: { ...current, rightPane: pane } } });
  },

  observePhase: (missionId, phase) => {
    const current = get().perMission[missionId] ?? defaultMissionUi();
    if (current.observedPhase === phase) return;
    const next: MissionUiState = { ...current, observedPhase: phase };
    if (phase === "awaiting_acceptance") {
      // 바뀌는 순간 한 번만: 사용자 선택을 비워 결과를 보이고, 좁은 폭은 결과 탭으로.
      next.rightPane = null;
      next.narrowView = "result";
    }
    set({ perMission: { ...get().perMission, [missionId]: next } });
  },

  closeSheet: (missionId) => {
    const current = get().perMission[missionId];
    if (!current || !current.sheetOpen) return;
    set({ perMission: { ...get().perMission, [missionId]: { ...current, sheetOpen: false } } });
  },
}));

/** 시험 전용 초기화. */
export function resetMissionUiStoreForTests(): void {
  useMissionUiStore.setState({ perMission: {} });
}

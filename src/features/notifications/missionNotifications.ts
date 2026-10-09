/**
 * AI 작업(mission) → 알림 센터 연동.
 *
 * missionStore의 변화를 보고 사용자의 손이 필요한 "전이"만 알림 센터에 한 줄 남긴다:
 * 새 결정 필요 · 확정 대기 진입 · 실패. 조용한 신호 우선(W1-5 §2.1)이라
 *
 * - 앱을 켜기 전부터 있던 상태(시작 시 처음 본 작업)는 기준선일 뿐 알리지 않는다.
 * - 같은 사건은 한 번만: id = mission id + 사건 종류 + 결정 id(결정 id를 모르는 요약
 *   경로는 mission revision). 알림 센터 store의 id 멱등성이 중복을 막는다.
 * - 사용자가 지금 그 작업 탭을 보고 있으면 남기지 않는다(화면의 결정 배너가 이미 알린다).
 * - OS 알림은 새 경로를 만들지 않는다: 터미널 개입 요청이 쓰는 기존 게이트
 *   (maybeDesktopNotify — 창이 숨겨져 있고 권한이 있을 때만)를 결정·확정 대기에만 쓴다.
 */

import type { Decision } from "../../generated/Decision";
import type { Mission } from "../../generated/Mission";
import { t } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import type { MissionStoreState } from "../missions/store";
import { maybeDesktopNotify, useNotificationStore } from "./notificationStore";

export type MissionNotificationEvent = "decision" | "acceptance" | "failed";

/** 알림 id(멱등 키): mission id + 사건 종류 + 결정 id(없으면 대체 키). */
export function missionNotificationId(missionId: string, event: MissionNotificationEvent, key: string | null): string {
  return key ? `mission:${missionId}:${event}:${key}` : `mission:${missionId}:${event}`;
}

/** 사건 종류별 알림 센터 라벨 키. */
export function missionNotificationKindKey(event: MissionNotificationEvent): string {
  switch (event) {
    case "decision":
      return "notifications.kind.missionDecision";
    case "acceptance":
      return "notifications.kind.missionAcceptance";
    case "failed":
      return "notifications.kind.missionFailed";
  }
}

/** 관찰에 필요한 store 표면(zustand store 그대로 넘긴다 — 시험 seam). */
export interface MissionNotificationSource {
  getState(): Pick<MissionStoreState, "missions" | "decisions" | "sync">;
  subscribe(
    listener: (
      state: Pick<MissionStoreState, "missions" | "decisions" | "sync">,
      prev: Pick<MissionStoreState, "missions" | "decisions" | "sync">,
    ) => void,
  ): () => void;
}

export interface MissionNotificationOptions {
  /** 관찰을 시작한 시각(ms) — 이보다 먼저 만들어진 작업은 처음 볼 때 기준선만 잡는다. */
  now?: () => number;
}

interface SeenMission {
  state: Mission["state"];
  phase: Mission["phase"];
  openDecisions: number;
}

const FINISHED: ReadonlySet<Mission["state"]> = new Set(["completed", "failed", "cancelled"]);

export function startMissionNotifications(
  source: MissionNotificationSource,
  options: MissionNotificationOptions = {},
): () => void {
  const now = options.now ?? (() => Date.now());
  const startedAt = now();
  const seenMissions = new Map<string, SeenMission>();
  /** snapshot으로 결정 id를 아는 작업의 본 적 있는 결정 id. */
  const seenDecisions = new Map<string, Set<string>>();

  const observe = (state: Pick<MissionStoreState, "missions" | "decisions" | "sync">): void => {
    let openByMission: Map<string, Decision[]> | null = null;
    const openDecisionsOf = (missionId: string): Decision[] => {
      if (openByMission === null) {
        openByMission = new Map();
        for (const decision of Object.values(state.decisions)) {
          if (decision.state !== "open") continue;
          const list = openByMission.get(decision.mission_id);
          if (list) list.push(decision);
          else openByMission.set(decision.mission_id, [decision]);
        }
      }
      return openByMission.get(missionId) ?? [];
    };

    for (const mission of Object.values(state.missions)) {
      const previous = seenMissions.get(mission.id);
      const createdAt = Date.parse(mission.created_at);
      // 앱을 켜기 전에 만들어진 작업을 처음 보면 기준선만 잡는다. 켠 뒤에 만들어진 작업은
      // 빈 상태에서 시작한 것으로 보아, 처음 보는 순간의 결정·확정 대기도 알린다.
      const baselineOnly = previous === undefined && !(Number.isFinite(createdAt) && createdAt >= startedAt);
      const before: SeenMission = previous ?? { state: "draft", phase: "planning", openDecisions: 0 };
      const finished = FINISHED.has(mission.state);
      const snapshotBacked = hasSnapshot(state, mission.id);

      if (snapshotBacked) {
        const open = openDecisionsOf(mission.id);
        const known = seenDecisions.get(mission.id);
        if (!known) {
          // 결정 id를 처음 알게 된 순간은 기준선이다 — 요약 경로가 이미 알렸을 수 있다.
          seenDecisions.set(mission.id, new Set(open.map((decision) => decision.id)));
        } else {
          for (const decision of open) {
            if (known.has(decision.id)) continue;
            known.add(decision.id);
            if (!baselineOnly && !finished) notify(mission, "decision", decision.id);
          }
        }
      } else if (!baselineOnly && !finished && mission.open_decision_count > before.openDecisions) {
        notify(mission, "decision", `r${mission.revision}`);
      }

      if (!baselineOnly && !finished && mission.phase === "awaiting_acceptance" && before.phase !== "awaiting_acceptance") {
        notify(mission, "acceptance", mission.candidate_id ?? `r${mission.revision}`);
      }
      if (!baselineOnly && mission.state === "failed" && before.state !== "failed") {
        notify(mission, "failed", null);
      }

      seenMissions.set(mission.id, {
        state: mission.state,
        phase: mission.phase,
        openDecisions: mission.open_decision_count,
      });
    }
  };

  observe(source.getState());
  const unsubscribe = source.subscribe((state, prev) => {
    if (state.missions === prev.missions && state.decisions === prev.decisions && state.sync === prev.sync) return;
    observe(state);
  });
  return unsubscribe;
}

function hasSnapshot(state: Pick<MissionStoreState, "sync">, missionId: string): boolean {
  const status = state.sync[missionId];
  return status !== undefined && status.atSeq !== "0";
}

/** 사용자가 지금 그 작업 탭을 보고 있는가(창이 보이고, 터미널 화면에서 그 탭이 활성). */
function isViewingMission(missionId: string): boolean {
  if (typeof document !== "undefined" && document.hidden) return false;
  const workbench = useWorkbenchStore.getState();
  if (workbench.page !== "terminal") return false;
  const tab = workbench.tabs.find((candidate) => candidate.id === workbench.activeTabId);
  return tab?.kind === "mission" && tab.missionId === missionId;
}

function notify(mission: Mission, event: MissionNotificationEvent, key: string | null): void {
  if (isViewingMission(mission.id)) return;
  const title = mission.title || t("missions.tabTitle");
  const added = useNotificationStore.getState().push({
    id: missionNotificationId(mission.id, event, key),
    kind: "mission",
    title,
    detail: event === "decision" ? t("missions.decision.live") : null,
    at: new Date().toISOString(),
    sessionId: null,
    missionId: mission.id,
    missionEvent: event,
    acknowledged: false,
  });
  // 사용자의 손이 필요한 사건만 기존 데스크톱 알림 게이트로 보낸다(실패는 알림 센터만).
  if (added && event !== "failed") {
    maybeDesktopNotify(t(missionNotificationKindKey(event)), title);
  }
}

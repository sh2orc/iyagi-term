/**
 * 탭 닫기 요청 — 탭 ×·탭 메뉴·메뉴 막대가 같은 규칙을 쓴다.
 *
 * terminal 탭은 모든 pane의 닫기 확인(창만 닫기 / 터미널도 종료)을 거친 뒤 탭을 닫는다. mission 계열
 * 탭 닫기는 로컬 숨김이다(05 §8) — 종료 확인·취소 요청이 없고 실행은 데몬에 그대로 있다. mission 탭을
 * 닫으면 "계속 실행되며 AI 작업 목록에서 다시 열 수 있다"를 잠깐 알린다(토스트는 행동 버튼이 없어 문구만).
 */

import type { MissionState } from "../generated/MissionState";
import { t } from "../i18n";
import { useMissionStore } from "../features/missions/store";
import { isFinishedMissionState } from "../features/missions/missionStatus";
import type { SessionController } from "../features/terminal/sessionController";
import { listLeaves } from "../features/terminal/splitTree";
import { flashToast, useWorkbenchStore } from "../store/workbenchStore";

/** mission 탭을 닫을 때의 안내 문구 키 — 끝난 작업에는 "계속 실행됩니다"라고 하지 않는다. */
export function missionTabClosedNoticeKey(state: MissionState | null): string {
  return state !== null && isFinishedMissionState(state) ? "missions.tabClosed.finished" : "missions.tabClosed.running";
}

export function requestCloseTab(controller: Pick<SessionController, "requestClosePanes">, tabId: string): void {
  const tab = useWorkbenchStore.getState().tabs.find((candidate) => candidate.id === tabId);
  if (!tab) return;
  if (tab.kind !== "terminal") {
    useWorkbenchStore.getState().closeTab(tabId);
    if (tab.kind === "mission") {
      const state = useMissionStore.getState().missions[tab.missionId]?.state ?? null;
      flashToast(t(missionTabClosedNoticeKey(state)));
    }
    return;
  }
  controller.requestClosePanes(
    listLeaves(tab.root).map((leaf) => leaf.id),
    tabId,
  );
}

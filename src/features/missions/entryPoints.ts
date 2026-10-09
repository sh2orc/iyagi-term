/**
 * 앱 셸의 AI 작업 진입점 공용 규칙: 상단 막대 버튼·명령 팔레트·메뉴 막대·단축키·
 * pane ⋯ 메뉴가 같은 가용성 판정(capability.ts)과 같은 동작을 쓴다.
 *
 * - 데몬이 프로토콜을 선언하지 않았으면: 개발 빌드는 비활성 + 사유, 프로덕션 빌드는 숨김.
 * - 데몬이 앱보다 새로우면: 비활성 + "앱을 업데이트하세요".
 */

import { t } from "../../i18n";
import { flashToast, useWorkbenchStore } from "../../store/workbenchStore";
import { missionEntryState, PRODUCTION_BUILD, type MissionEntryState } from "./capability";
import { focusedRepositoryHint, openMissionCreate } from "./entry";

export { missionEntryState, type MissionEntryState } from "./capability";

/** React 진입점용: 원시값(protocol)만 구독하고 판정은 렌더에서 한다. */
export function useMissionEntryState(): MissionEntryState {
  const protocol = useWorkbenchStore((s) => s.missionProtocol);
  return missionEntryState(protocol);
}

/** AI 작업 목록 대화상자를 연다. */
export function openMissionList(): void {
  useWorkbenchStore.getState().openModal({ kind: "mission-list" });
}

/**
 * 단축키·메뉴의 "새 AI 작업": 쓸 수 있으면 보고 있는 터미널의 저장소로 대화상자를 열고,
 * 비활성이면 사유를 잠깐 알린다. 숨김 상태(프로덕션 + 프로토콜 없음)에서는 아무것도 하지 않는다.
 * 대화상자를 열었으면 true.
 */
export function requestNewMission(production: boolean = PRODUCTION_BUILD): boolean {
  const store = useWorkbenchStore.getState();
  const entry = missionEntryState(store.missionProtocol, production);
  if (entry.hidden) return false;
  if (!entry.enabled) {
    if (entry.reasonKey) flashToast(t(entry.reasonKey));
    return false;
  }
  openMissionCreate(focusedRepositoryHint(store));
  return true;
}

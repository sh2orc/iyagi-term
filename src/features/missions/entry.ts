/**
 * 새 AI 작업 작성 패널 진입점(05 §3).
 *
 * `+` 메뉴·팔레트·메뉴 막대·mission 탭·pane 오른쪽 클릭 메뉴가 모두 여기로 연다.
 * 저장소 경로를 손으로 적지 않게, 보고 있는 터미널의 저장소(git 최상위 → cwd)를
 * 채워 연다 — 작성 패널이 열리자마자 그 경로를 확인한다.
 */

import { findLeaf } from "../terminal/splitTree";
import { useWorkbenchStore, type PaneMeta, type WorkbenchState } from "../../store/workbenchStore";

/** pane이 가리키는 저장소 후보: git 최상위 경로, 없으면 cwd(빈 문자열은 없는 값). */
export function paneRepositoryHint(pane: Pick<PaneMeta, "cwd" | "project"> | null | undefined): string | null {
  if (!pane) return null;
  return pane.project || pane.cwd || null;
}

/**
 * 보이는 terminal 탭의 초점 pane이 가리키는 저장소 후보. mission·agent-view 탭을
 * 보고 있거나, 초점이 다른 탭에 남아 있거나, pane 정보가 없으면 null.
 */
export function focusedRepositoryHint(state: WorkbenchState): string | null {
  const tab = state.tabs.find((candidate) => candidate.id === state.activeTabId);
  const leafId = state.focusedLeafId;
  if (!tab || tab.kind !== "terminal" || leafId === null || findLeaf(tab.root, leafId) === null) return null;
  return paneRepositoryHint(state.panes[leafId]);
}

/**
 * 새 AI 작업 작성 패널 옵션 — goal은 목표 입력의 초기값(후속 작업·같은 목표로 다시),
 * followUpOf는 이전 결과 위에서 시작할 확정된 작업 id(계약 E).
 */
export interface OpenMissionCreateOptions {
  goal?: string | null;
  followUpOf?: string | null;
}

/** 새 AI 작업 작성 패널을 연다. 경로가 없으면 빈 입력으로 연다. 비어 있는 옵션은 상태에 싣지 않는다. */
export function openMissionCreate(repositoryPath?: string | null, options?: OpenMissionCreateOptions): void {
  const path = repositoryPath?.trim() ? repositoryPath : null;
  const goal = options?.goal?.trim() ? options.goal : null;
  const followUpOf = options?.followUpOf?.trim() ? options.followUpOf : null;
  useWorkbenchStore.getState().openModal({
    kind: "mission-create",
    repositoryPath: path,
    ...(goal === null ? {} : { goal }),
    ...(followUpOf === null ? {} : { followUpOf }),
  });
}

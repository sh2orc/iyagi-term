/**
 * 워크벤치 위에 뜨는 화면(배치 편집 04-ui §2-6·설정)에서의 키·명령 규칙 — Workbench의 window
 * keydown과 명령 팔레트가 쓴다.
 *
 * 그 화면이 떠 있는 동안 터미널은 숨어 있다: 앱 단축키 가운데 화면 전환(Cmd+Shift+G)과 팔레트만
 * 받고, 터미널에서만 뜻이 있는 팔레트 명령은 먼저 터미널로 돌아간 뒤 실행한다. Esc는 터미널로
 * 돌아가기지만, 대화상자를 닫는 Esc와 조합 중인 Esc는 화면까지 닫지 않는다.
 */

import { useWorkbenchStore, type WorkbenchPage } from "../store/workbenchStore";
import type { ShortcutAction } from "../features/terminal/shortcuts";

/**
 * 이 keydown이 배치 편집 화면을 닫고 터미널로 돌아가는가. modalOpen은 "누른 순간 모달이 떠 있었는가"다
 * — 대화상자는 자기 Esc 처리에서 모달을 먼저 닫아 버리므로 호출자가 capture 단계의 값을 넘긴다.
 */
export function leavesLayoutPage(
  event: { key: string; isComposing: boolean },
  state: { page: WorkbenchPage; modalOpen: boolean },
): boolean {
  return state.page === "layout" && event.key === "Escape" && !event.isComposing && !state.modalOpen;
}

/** 배치 편집 화면에서 발동할 앱 단축키 — 숨은 터미널을 겨냥하는 나머지는 무시한다. */
export function layoutPageAllowsAction(action: ShortcutAction): boolean {
  return action === "layout-editor" || action === "palette";
}

/**
 * 터미널 화면에서만 뜻이 있는 명령(붙여넣기·찾기·탭 이름 바꾸기·패널 열기)을 다른 화면에서 고르면
 * 먼저 터미널로 돌아간 뒤 실행한다 — 숨은 터미널에 붙여 넣거나, 보이지 않는 이름 칸에 단축키가
 * 갇히지 않게.
 */
export function runOnTerminalPage(run: () => void): () => void {
  return () => {
    const store = useWorkbenchStore.getState();
    if (store.page !== "terminal") store.setPage("terminal");
    run();
  };
}

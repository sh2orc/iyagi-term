/**
 * mission 화면의 mutation client 접근 seam(05-ui O16).
 *
 * missionStore의 동기화 client(syncClient)는 읽기 최소 표면만 필요하지만,
 * 화면의 제어·생성·메시지 흐름은 Rpc 전체 표면(artifact/template/control)
 * 을 쓴다. Workbench가 마운트에서 한 번 연결한 client를 여기에 두고
 * 컴포넌트는 getter로만 꺼낸다 — 컴포넌트가 client를 props로 들고 다니면
 * 시험이 매 화면에 fake를 물려줘야 한다.
 */

import type { DaemonClient } from "../daemon/client";

let actionClient: DaemonClient | null = null;

/** Workbench 마운트에서 호출(동기화 구독과 같은 시점). 해제는 null 전달. */
export function setMissionClient(client: DaemonClient | null): void {
  actionClient = client;
}

/** mutation용 client — 없으면 null(호출부는 오류를 영역에 표시한다). */
export function getMissionClient(): DaemonClient | null {
  return actionClient;
}

/** 시험 정리용. */
export function resetMissionClientForTests(): void {
  actionClient = null;
}

/**
 * 관리 실행 UI deps seam (I11).
 *
 * ManagedRunDialog는 DaemonClient와 SystemProbe가 필요하지만 그 소유권은
 * Workbench/App wiring에 있다(src/app는 별도 에이전트 관리). 이 모듈은
 * wiring이 한 줄로 주입하는 모듈 스코프 seam이다:
 *
 *   setManagedRunDeps({ client, probe: tauriSystemProbe });
 *
 * 테스트와 커스텀 마운트는 props로 직접 주입할 수 있다(props가 우선).
 */

import type { DaemonClient } from "../daemon/client";
import type { SystemProbe } from "../profiles/probeTypes";

export interface ManagedRunDeps {
  client: DaemonClient;
  /** null이면 탐지 제안·버전 조회 없이 직접 입력만 동작한다. */
  probe: SystemProbe | null;
}

let current: ManagedRunDeps | null = null;

export function setManagedRunDeps(deps: ManagedRunDeps | null): void {
  current = deps;
}

export function getManagedRunDeps(): ManagedRunDeps | null {
  return current;
}

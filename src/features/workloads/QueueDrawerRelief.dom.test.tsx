/**
 * 자동 양보 토글(08-pressure-relief §2)의 상호작용 — happy-dom(*.dom.test.tsx).
 *
 * 체크박스를 누르면 `relief.set_policy`가 나가고, 스토어에는 요청값이 아니라
 * 데몬이 돌려준 값이 남는다. 되돌릴 수 있는 양보를 지원하지 않는 플랫폼에서는
 * 꺼진 채 잠기고 사유가 보인다.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ControllerContext } from "../../app/controllerContext";
import { t, useI18nStore } from "../../i18n";
import { MockDaemonClient } from "../daemon/mockClient";
import type { SessionController } from "../terminal/sessionController";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { QueueDrawer } from "./QueueDrawer";

let root: Root | null = null;
let container: HTMLElement | null = null;
let client: MockDaemonClient | null = null;

/** 컨트롤러의 정책 경로만 흉내 낸다(데몬 호출 → 응답값을 스토어로). */
function controllerFor(daemon: MockDaemonClient): SessionController {
  return {
    setAutoYield: async (autoYield: boolean) => {
      const policy = await daemon.reliefSetPolicy({ auto_yield: autoYield });
      useWorkbenchStore.getState().setReliefPolicy(policy);
    },
  } as unknown as SessionController;
}

function render(): HTMLInputElement {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root?.render(
      <ControllerContext.Provider value={controllerFor(client as MockDaemonClient)}>
        <QueueDrawer />
      </ControllerContext.Provider>,
    );
  });
  const input = container.querySelector<HTMLInputElement>(".relief-policy input");
  if (!input) throw new Error("relief toggle not rendered");
  return input;
}

beforeEach(() => {
  useI18nStore.setState({ language: "ko" });
  client = new MockDaemonClient({ resourceIntervalMs: 0 });
  useWorkbenchStore.setState({
    queueDrawerOpen: true,
    queue: [],
    workloads: [],
    panes: {},
    reliefPolicy: { auto_yield: true },
    schedulingYield: { support: "supported", reason: null },
  });
});

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
  root = null;
  container = null;
  client?.dispose();
  client = null;
  useWorkbenchStore.setState({
    queueDrawerOpen: false,
    reliefPolicy: { auto_yield: true },
    schedulingYield: null,
  });
});

describe("QueueDrawer auto-yield toggle", () => {
  it("끄면 relief.set_policy를 보내고 데몬이 돌려준 값을 반영한다", async () => {
    const setPolicy = vi.spyOn(client as MockDaemonClient, "reliefSetPolicy");
    const input = render();
    expect(input.checked).toBe(true);
    expect(input.disabled).toBe(false);

    await act(async () => {
      input.click();
    });

    expect(setPolicy).toHaveBeenCalledWith({ auto_yield: false });
    expect(useWorkbenchStore.getState().reliefPolicy).toEqual({ auto_yield: false });
    expect(input.checked).toBe(false);

    // 다시 켜면 반대 방향으로도 같은 경로를 탄다.
    await act(async () => {
      input.click();
    });
    expect(setPolicy).toHaveBeenLastCalledWith({ auto_yield: true });
    expect(useWorkbenchStore.getState().reliefPolicy).toEqual({ auto_yield: true });
  });

  it("양보를 지원하지 않는 플랫폼에서는 꺼진 채 잠그고 사유를 보여 준다", () => {
    const reason = "위임 cgroup이 없어 되돌릴 수 없습니다";
    useWorkbenchStore.setState({ schedulingYield: { support: "unsupported", reason } });
    const setPolicy = vi.spyOn(client as MockDaemonClient, "reliefSetPolicy");
    const input = render();

    expect(input.disabled).toBe(true);
    // 데몬 정책이 켜져 있어도 적용할 수 없으면 켜진 것처럼 보이지 않는다.
    expect(input.checked).toBe(false);
    expect(container?.textContent).toContain(reason);
    expect(setPolicy).not.toHaveBeenCalled();
  });

  it("사유가 없는 미지원 데몬에도 설명을 남긴다", () => {
    useWorkbenchStore.setState({ schedulingYield: { support: "permission_required", reason: null } });
    render();
    expect(container?.textContent).toContain(t("queue.relief.unsupported"));
  });
});

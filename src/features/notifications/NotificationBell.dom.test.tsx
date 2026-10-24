/**
 * 알림 패널의 자원 알림(happy-dom): 자원 가드가 스스로 일시정지한 작업은 그 자리에서
 * "재개"·"보호"를 고르고(고르면 읽음), 이미 풀린 작업에는 버튼을 보이지 않는다.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { t } from "../../i18n";
import { ControllerContext } from "../../app/controllerContext";
import type { SessionController } from "../terminal/sessionController";
import type { WorkloadSummary } from "../../generated/WorkloadSummary";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { NotificationBell, notificationKindKey } from "./NotificationBell";
import { useNotificationStore, type NotificationItem } from "./notificationStore";

let root: Root | null = null;
let container: HTMLElement | null = null;

afterEach(() => {
  act(() => root?.unmount());
  root = null;
  container?.remove();
  container = null;
  useNotificationStore.getState().clear();
  useNotificationStore.setState({ panelOpen: false });
  useWorkbenchStore.setState({ workloads: [], workloadById: new Map(), workloadBySession: new Map() });
});

function workload(guard: WorkloadSummary["guard"]): WorkloadSummary {
  return {
    workload_id: "w1", session_id: "s1", mode: "shell", state: "RUNNING", priority: 1, title: "zsh", cwd: "/w",
    program: "/bin/zsh", reservation_bytes: "0", cpu_slots: 1, enforcement: "observe", root_exited: false,
    cancel_requested: false, connection: "attached", relief: { kind: "NONE" }, guard, guard_warning: null, protected: false,
  };
}

const notice: NotificationItem = {
  id: "guard:w1:10",
  kind: "resource",
  title: "build 일시정지됨",
  detail: "시스템 메모리 부족",
  at: "2026-10-14T00:00:00.000Z",
  sessionId: "s1",
  workloadId: "w1",
  acknowledged: false,
};

function render(controller: Partial<SessionController>): HTMLElement {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root?.render(
      <ControllerContext.Provider value={controller as SessionController}>
        <NotificationBell />
      </ControllerContext.Provider>,
    );
  });
  return container;
}

const buttonByText = (scope: HTMLElement, text: string): HTMLButtonElement | undefined =>
  [...scope.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent === text);

describe("NotificationBell 자원 알림", () => {
  it("정지된 작업이면 재개·보호를 그 자리에서 고르고, 고르면 읽음으로 한다", () => {
    useWorkbenchStore.getState().upsertWorkload(
      workload({ kind: "SUSPENDED", since_ms: "10", reason: "host_memory_pressure", manual: false, partial: false }),
    );
    useNotificationStore.getState().push(notice);
    useNotificationStore.setState({ panelOpen: true });
    const controller = { resumeWorkload: vi.fn(async () => undefined), protectWorkload: vi.fn(async () => undefined) };
    const el = render(controller);

    expect(el.textContent).toContain(t("notifications.kind.autoSuspended"));
    const resume = buttonByText(el, t("notifications.resource.resume"));
    const protect = buttonByText(el, t("notifications.resource.protect"));
    expect(resume).toBeDefined();
    expect(protect?.title).toBe(t("notifications.resource.protectTitle"));

    act(() => protect!.click());
    expect(controller.protectWorkload).toHaveBeenCalledWith("w1");
    act(() => resume!.click());
    expect(controller.resumeWorkload).toHaveBeenCalledWith("w1");
    expect(useNotificationStore.getState().items[0].acknowledged).toBe(true);
  });

  it("이미 풀린 작업이면 버튼을 보이지 않는다", () => {
    useWorkbenchStore.getState().upsertWorkload(workload({ kind: "NONE" }));
    useNotificationStore.getState().push(notice);
    useNotificationStore.setState({ panelOpen: true });
    const el = render({ resumeWorkload: vi.fn(), protectWorkload: vi.fn() });
    expect(el.textContent).toContain(notice.title);
    expect(buttonByText(el, t("notifications.resource.resume"))).toBeUndefined();
  });

  it("종류별 라벨 키", () => {
    expect(notificationKindKey(notice)).toBe("notifications.kind.autoSuspended");
    expect(notificationKindKey({ ...notice, kind: "workload-finished" })).toBe("notifications.kind.finished");
    expect(notificationKindKey({ ...notice, kind: "intervention", interventionKind: "permission" })).toBe(
      "notifications.kind.permission",
    );
  });
});

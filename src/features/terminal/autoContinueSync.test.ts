/**
 * 자동 continue 동기화: 이벤트 → 토스트/알림 매핑과 사유 문구.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DaemonEvent } from "../daemon/client";
import { autoContinueReasonText, startAutoContinueSync } from "./autoContinueSync";
import { useNotificationStore } from "../notifications/notificationStore";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { t } from "../../i18n";

function event(state: "armed" | "fired" | "disarmed", reason?: string): DaemonEvent {
  return {
    kind: "auto_continue.changed",
    payload: {
      session_id: "sess-1",
      workload_id: "wl-1",
      state,
      reason: reason ?? null,
      at_ms: 1_800_000_000_000,
    },
  } as DaemonEvent;
}

function harness(): { client: Parameters<typeof startAutoContinueSync>[0]; emit(e: DaemonEvent): void } {
  let listener: ((e: DaemonEvent) => void) | null = null;
  const client = {
    events: {
      subscribe(l: (e: DaemonEvent) => void) {
        listener = l;
        return { dispose: () => (listener = null) };
      },
    },
  } as unknown as Parameters<typeof startAutoContinueSync>[0];
  return { client, emit: (e) => listener?.(e) };
}

afterEach(() => {
  useNotificationStore.setState({ items: [], panelOpen: false });
  useWorkbenchStore.setState({ toast: null });
  vi.restoreAllMocks();
});

describe("autoContinueReasonText", () => {
  it("아는 사유는 문구로, 모르는 사유는 원문으로", () => {
    expect(autoContinueReasonText("user-input")).toBe(t("autoContinue.reason.user-input"));
    expect(autoContinueReasonText("brand-new")).toBe("brand-new");
    expect(autoContinueReasonText(null)).toBe(t("autoContinue.reason.unknown"));
  });
});

describe("startAutoContinueSync", () => {
  it("armed는 토스트만, fired는 토스트+알림, disarmed는 사유 토스트", () => {
    const h = harness();
    const stop = startAutoContinueSync(h.client);

    h.emit(event("armed"));
    expect(useWorkbenchStore.getState().toast).toBe(t("autoContinue.armed"));
    expect(useNotificationStore.getState().items).toHaveLength(0);

    h.emit(event("fired"));
    expect(useWorkbenchStore.getState().toast).toBe(t("autoContinue.fired"));
    const item = useNotificationStore.getState().items[0];
    expect(item).toMatchObject({
      id: "auto-continue:sess-1:1800000000000",
      kind: "auto-continue",
      sessionId: "sess-1",
    });

    h.emit(event("disarmed", "user-input"));
    expect(useWorkbenchStore.getState().toast).toBe(
      t("autoContinue.disarmed", { reason: t("autoContinue.reason.user-input") }),
    );

    stop();
  });

  it("세션 종료 해제는 조용히 넘어간다", () => {
    const h = harness();
    const stop = startAutoContinueSync(h.client);
    try {
      useWorkbenchStore.setState({ toast: null });
      h.emit(event("disarmed", "session-gone"));
      expect(useWorkbenchStore.getState().toast).toBeNull();
    } finally {
      stop();
    }
  });
});

/**
 * 알림센터 스토어 시험(W1-5): 멱등 dedup·유계·읽음 처리·게이트 규칙.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  interventionKindKey,
  maybeDesktopNotify,
  selectUnreadCount,
  useNotificationStore,
} from "./notificationStore";

import type { NotificationItem } from "./notificationStore";

function item(id: string, patch: Partial<NotificationItem> = {}): NotificationItem {
  return {
    id,
    kind: "intervention" as const,
    title: `알림 ${id}`,
    detail: null,
    at: "2026-09-08T00:00:00.000Z",
    sessionId: null,
    acknowledged: false,
    ...patch,
  };
}

describe("notificationStore — 유계·멱등·읽음", () => {
  beforeEach(() => {
    useNotificationStore.getState().clear();
  });

  it("같은 id는 두 번 쌓이지 않는다(hook 재시도 멱등)", () => {
    expect(useNotificationStore.getState().push(item("r-1"))).toBe(true);
    expect(useNotificationStore.getState().push(item("r-1"))).toBe(false);
    expect(useNotificationStore.getState().items).toHaveLength(1);
  });

  it("최신이 앞이고 100개로 유계된다", () => {
    for (let i = 0; i < 120; i++) useNotificationStore.getState().push(item(`r-${i}`));
    const items = useNotificationStore.getState().items;
    expect(items).toHaveLength(100);
    expect(items[0].id).toBe("r-119");
  });

  it("읽음 처리와 미읽음 카운트", () => {
    useNotificationStore.getState().push(item("a"));
    useNotificationStore.getState().push(item("b"));
    expect(selectUnreadCount(useNotificationStore.getState().items)).toBe(2);
    useNotificationStore.getState().acknowledge("a");
    expect(selectUnreadCount(useNotificationStore.getState().items)).toBe(1);
    useNotificationStore.getState().acknowledgeAll();
    expect(selectUnreadCount(useNotificationStore.getState().items)).toBe(0);
  });
});

describe("개입 종류 라벨 키", () => {
  it("enum 값이 그대로 키로 매핑된다", () => {
    expect(interventionKindKey("permission")).toBe("notifications.kind.permission");
    expect(interventionKindKey("question")).toBe("notifications.kind.question");
    expect(interventionKindKey("stop")).toBe("notifications.kind.stop");
    expect(interventionKindKey("whatever")).toBe("notifications.kind.notification");
  });
});

describe("maybeDesktopNotify — 비포커스+권한 게이트(§2.1)", () => {
  it("권한이 없으면 조용히 건너뛴다", () => {
    const ctor = vi.fn();
    (globalThis as { Notification?: unknown }).Notification = Object.assign(ctor, {
      permission: "denied",
    }) as never;
    expect(() => maybeDesktopNotify("t", "b")).not.toThrow();
    expect(ctor).not.toHaveBeenCalled();
  });

  it("창이 보이면(visible) 데스크톱 알림을 치지 않는다", () => {
    const ctor = vi.fn();
    (globalThis as { Notification?: unknown }).Notification = Object.assign(ctor, {
      permission: "granted",
    }) as never;
    vi.stubGlobal("document", { hidden: false });
    maybeDesktopNotify("t", "b");
    expect(ctor).not.toHaveBeenCalled();
    vi.unstubAllGlobals();
  });
});

describe("알림 패널 열림 — 벨과 메뉴 막대가 함께 쓴다(04-ui §3-2)", () => {
  beforeEach(() => {
    useNotificationStore.getState().clear();
    useNotificationStore.setState({ panelOpen: false });
  });

  it("열면 모두 읽음 처리하고, 닫아도 항목은 그대로다", () => {
    useNotificationStore.getState().push(item("a"));
    useNotificationStore.getState().setPanelOpen(true);
    expect(useNotificationStore.getState().panelOpen).toBe(true);
    expect(selectUnreadCount(useNotificationStore.getState().items)).toBe(0);
    useNotificationStore.getState().setPanelOpen(false);
    expect(useNotificationStore.getState().panelOpen).toBe(false);
    expect(useNotificationStore.getState().items).toHaveLength(1);
  });

  it("열 때만 데스크톱 알림 권한을 묻는다(이미 열려 있으면 다시 묻지 않는다)", () => {
    const requestPermission = vi.fn(async () => "granted");
    (globalThis as { Notification?: unknown }).Notification = Object.assign(vi.fn(), {
      permission: "default",
      requestPermission,
    }) as never;
    useNotificationStore.getState().setPanelOpen(true);
    useNotificationStore.getState().setPanelOpen(true);
    expect(requestPermission).toHaveBeenCalledOnce();
    useNotificationStore.getState().setPanelOpen(false);
    expect(requestPermission).toHaveBeenCalledOnce();
    delete (globalThis as { Notification?: unknown }).Notification;
  });
});

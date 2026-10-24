/**
 * 예약 명령 스토어·동기화: refresh 흡수(구 데몬), schedule.fired 이벤트가
 * 목록 갱신 + 토스트 + 알림 센터 항목으로 이어지는지.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DaemonEvent } from "../daemon/client";
import type { ScheduledCommand } from "../../generated/ScheduledCommand";
import { startScheduleSync, useScheduleStore } from "./scheduleStore";
import { useNotificationStore } from "../notifications/notificationStore";
import { useWorkbenchStore } from "../../store/workbenchStore";

function command(overrides: Partial<ScheduledCommand> = {}): ScheduledCommand {
  return {
    schedule_id: "sch-1",
    session_id: "sess-1",
    command: "cargo test",
    fire_at_ms: 1_800_000_000_000,
    status: "pending",
    attempts: 0,
    created_at_ms: 1_700_000_000_000,
    fired_at_ms: null,
    reason_code: null,
    ...overrides,
  };
}

/** events.subscribe(+선택 scheduleList)만 있는 최소 클라이언트(시험 주입). */
function stubClient(events: {
  subscribe(listener: (event: DaemonEvent) => void): { dispose(): void };
  scheduleList?: () => Promise<ScheduledCommand[]>;
}) {
  return {
    events: { subscribe: events.subscribe },
    scheduleList: events.scheduleList ?? (() => Promise.resolve([])),
  } as unknown as Parameters<typeof startScheduleSync>[0];
}

function listenerCollector(): {
  client: Parameters<typeof startScheduleSync>[0];
  emit(event: DaemonEvent): void;
  disposed: () => boolean;
} {
  let listener: ((event: DaemonEvent) => void) | null = null;
  let disposed = false;
  const client = stubClient({
    subscribe(l) {
      listener = l;
      return { dispose: () => (disposed = true) };
    },
    scheduleList: () => Promise.resolve([]),
  });
  return {
    client,
    emit: (event) => listener?.(event),
    disposed: () => disposed,
  };
}

function firedEvent(outcome: "fired" | "failed" | "skipped", reason?: string): DaemonEvent {
  return {
    kind: "schedule.fired",
    payload: {
      schedule_id: "sch-1",
      session_id: "sess-1",
      outcome,
      reason_code: reason ?? null,
      command: "cargo test",
      fired_at_ms: 1_800_000_000_000,
    },
  } as DaemonEvent;
}

afterEach(() => {
  useScheduleStore.setState({ items: [], loaded: false });
  useNotificationStore.setState({ items: [], panelOpen: false });
  useWorkbenchStore.setState({ toast: null });
  vi.restoreAllMocks();
});

describe("useScheduleStore.refresh", () => {
  it("성공하면 목록을 채운다", async () => {
    const items = [command({ schedule_id: "sch-2" })];
    await useScheduleStore.getState().refresh(stubClient({
      subscribe: () => ({ dispose: () => undefined }),
      scheduleList: () => Promise.resolve(items),
    }));
    expect(useScheduleStore.getState().items).toEqual(items);
    expect(useScheduleStore.getState().loaded).toBe(true);
  });

  it("구 데몬(schedule.* 없음) 실패는 빈 목록으로 조용히 흡수한다", async () => {
    await useScheduleStore.getState().refresh(stubClient({
      subscribe: () => ({ dispose: () => undefined }),
      scheduleList: () => Promise.reject(new Error("INVALID_ARGUMENT")),
    }));
    expect(useScheduleStore.getState().items).toEqual([]);
    expect(useScheduleStore.getState().loaded).toBe(true);
  });
});

describe("startScheduleSync", () => {
  it("schedule.fired마다 목록을 다시 가져오고 토스트·알림을 흘린다", async () => {
    const harness = listenerCollector();
    const stop = startScheduleSync(harness.client);
    try {
      await Promise.resolve();
      harness.emit(firedEvent("fired"));
      // refresh + flashToast + notification push 모두 이벤트 직후 동기·마이크로태스크로 끝난다.
      await Promise.resolve();
      expect(useWorkbenchStore.getState().toast).toContain("cargo test");
      const item = useNotificationStore.getState().items[0];
      expect(item).toMatchObject({
        id: "schedule:sch-1:fired",
        kind: "schedule",
        title: "cargo test",
        sessionId: "sess-1",
      });
    } finally {
      stop();
    }
    expect(harness.disposed()).toBe(true);
  });

  it("같은 예약·같은 결과는 알림에 두 번 쌓이지 않는다(멱등 키)", async () => {
    const harness = listenerCollector();
    const stop = startScheduleSync(harness.client);
    try {
      await Promise.resolve();
      harness.emit(firedEvent("failed", "write-failed"));
      harness.emit(firedEvent("failed", "write-failed"));
      await Promise.resolve();
      expect(useNotificationStore.getState().items).toHaveLength(1);
    } finally {
      stop();
    }
  });

  it("다른 종착점은 각각 알린다", async () => {
    const harness = listenerCollector();
    const stop = startScheduleSync(harness.client);
    try {
      await Promise.resolve();
      harness.emit(firedEvent("skipped", "session-gone"));
      harness.emit(firedEvent("fired"));
      await Promise.resolve();
      const ids = useNotificationStore.getState().items.map((item) => item.id).sort();
      expect(ids).toEqual(["schedule:sch-1:fired", "schedule:sch-1:skipped"]);
    } finally {
      stop();
    }
  });
});

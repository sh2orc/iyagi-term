import { describe, expect, it } from "vitest";
import type { QueueReason } from "../src/generated/QueueReason";
import {
  detachedRunningText,
  interruptedText,
  journalLimitPausedText,
  observeOnlyText,
  priorityText,
  queueWaitText,
  workloadStateText,
} from "../src/features/monitor/statusStrings";

const GiB = 1024 ** 3;

describe("queue wait texts — 04-ui.md §7 exact strings", () => {
  it("WAIT_CONCURRENCY produces the exact spec sentence", () => {
    expect(queueWaitText("WAIT_CONCURRENCY", { runningManaged: 2, needBytes: null, safeAvailableBytes: null })).toBe(
      `대기: 관리 작업 2개가 실행 중입니다.`,
    );
  });

  it("WAIT_MEMORY_HEADROOM produces the exact spec sentence with measurements", () => {
    expect(
      queueWaitText("WAIT_MEMORY_HEADROOM", {
        runningManaged: 1,
        needBytes: 2 * GiB,
        safeAvailableBytes: Math.round(1.3 * GiB),
      }),
    ).toBe(`대기: 새 작업에 2 GiB가 필요하지만 안전 여유를 제외하면 1.3 GiB입니다.`);
  });

  it("every QueueReason renders a non-empty sentence; ADMIT is not a wait", () => {
    const reasons: QueueReason[] = [
      "WAIT_TELEMETRY",
      "RESOURCE_UNSCHEDULABLE",
      "WAIT_HOST_PRESSURE",
      "WAIT_CONCURRENCY",
      "WAIT_CPU_SLOTS",
      "WAIT_RESERVATION_BUDGET",
      "WAIT_MEMORY_HEADROOM",
      "ADMIT",
    ];
    for (const reason of reasons) {
      const text = queueWaitText(reason, { runningManaged: 2, needBytes: 2 * GiB, safeAvailableBytes: 1 * GiB });
      if (reason === "ADMIT") {
        expect(text).toBeNull();
      } else {
        expect(text && text.length).toBeGreaterThan(5);
      }
    }
    expect(queueWaitText(null, { runningManaged: 0, needBytes: null, safeAvailableBytes: null })).toBeNull();
  });

  it("keeps wait wording for waiting reasons and action wording for unschedulable", () => {
    const wait = queueWaitText("WAIT_CPU_SLOTS", { runningManaged: 0, needBytes: null, safeAvailableBytes: null });
    expect(wait && wait.startsWith("대기:")).toBe(true);
    const unschedulable = queueWaitText("RESOURCE_UNSCHEDULABLE", { runningManaged: 0, needBytes: null, safeAvailableBytes: null });
    expect(unschedulable && !unschedulable.startsWith("대기:")).toBe(true);
  });
});

describe("state texts — 04-ui.md §7", () => {
  it("observe-only notice matches the exact macOS sentence and is generated from enums", () => {
    expect(observeOnlyText("darwin", "observe")).toBe(
      `관측만 가능: 이 macOS 실행에서는 메모리 강제 상한을 적용할 수 없습니다.`,
    );
    expect(observeOnlyText("macos", "prefer")).toBe(
      `관측만 가능: 이 macOS 실행에서는 메모리 강제 상한을 적용할 수 없습니다.`,
    );
    expect(observeOnlyText("windows", "observe")).toBeNull();
    expect(observeOnlyText("darwin", "require")).toBeNull();
  });

  it("detached + running produces the exact sentence", () => {
    expect(detachedRunningText("detached", "RUNNING")).toBe(`화면 연결 해제됨 · 프로세스 실행 중`);
    expect(detachedRunningText("attached", "RUNNING")).toBeNull();
    expect(detachedRunningText("detached", "SUCCEEDED")).toBeNull();
  });

  it("journal limit pause text embeds the measured cap (128 MiB default)", () => {
    expect(journalLimitPausedText(134217728)).toBe(
      `출력 읽기 일시 중지: 로그 상한 128 MiB. 상한 늘리기 / 종료된 기록 관리`,
    );
  });

  it("interrupted workload asks for manual review with the exact sentence", () => {
    expect(interruptedText("INTERRUPTED")).toBe(
      `실행 결과 확인 필요: 실행 관리자가 재시작되었습니다. 자동 재실행하지 않았습니다.`,
    );
    expect(interruptedText("RUNNING")).toBeNull();
  });

  it("maps workload states and priorities to Korean labels", () => {
    expect(workloadStateText("RUNNING")).toBe("실행 중");
    expect(workloadStateText("DRAINING")).toBe("정리 중");
    expect(priorityText(0)).toBe("높음");
    expect(priorityText(1)).toBe("보통");
    expect(priorityText(2)).toBe("낮음");
  });
});

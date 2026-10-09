import { afterEach, describe, expect, it, vi } from "vitest";
import type { WorkloadSummary } from "../../generated/WorkloadSummary";
import { useWorkbenchStore } from "../../store/workbenchStore";
import {
  INPUT_STALL_BADGE_MS,
  classifyInputRejection,
  inputIssueOf,
  setInputIssue,
  useInputHealth,
} from "./inputHealth";

function rpcError(code: string, message: string, details?: Record<string, unknown>): Error {
  return Object.assign(new Error(message), { code, details });
}

describe("classifyInputRejection", () => {
  it("detached views reattach", () => {
    expect(classifyInputRejection(rpcError("STALE_EPOCH", "x"))).toBe("detached");
    expect(classifyInputRejection(rpcError("NOT_INPUT_OWNER", "x"))).toBe("detached");
    expect(classifyInputRejection(rpcError("DAEMON_UNAVAILABLE", "x"))).toBe("detached");
    expect(classifyInputRejection(new Error("not attached"))).toBe("detached");
  });

  it("reason_code separates a stalled program from a guard suspension", () => {
    expect(
      classifyInputRejection(rpcError("BUSY", "not reading", { reason_code: "INPUT_STALLED" })),
    ).toBe("stalled");
    expect(
      classifyInputRejection(rpcError("INVALID_STATE", "suspended", { reason_code: "GUARD_SUSPENDED" })),
    ).toBe("suspended");
  });

  it("recognizes the same cases from daemons that send no reason_code", () => {
    expect(classifyInputRejection(rpcError("BUSY", "input queue is full"))).toBe("stalled");
    expect(
      classifyInputRejection(
        rpcError("INVALID_STATE", "session is suspended by the resource guard; resume it first"),
      ),
    ).toBe("suspended");
  });

  it("other rejections are only logged", () => {
    expect(classifyInputRejection(rpcError("BUSY", "one outstanding input per writer"))).toBe("other");
    expect(classifyInputRejection(rpcError("INVALID_STATE", "session has no live actor"))).toBe("other");
  });

  it("a full actor queue with a healthy program is not a stall (no badge)", () => {
    // 같은 BUSY 'input queue is full' 메시지라도 새 데몬은 상태를 가린다 —
    // 프로그램이 건강한 클라이언트 폭주를 '입력 막힘' 배지로 오판하지 않는다.
    expect(
      classifyInputRejection(rpcError("BUSY", "input queue is full", { reason_code: "INPUT_QUEUE_FULL" })),
    ).toBe("other");
    // 반면 막힌 쪽은 같은 메시지에 INPUT_STALLED를 실어 배지가 켜진다.
    expect(
      classifyInputRejection(
        rpcError("BUSY", "input queue is full", { reason_code: "INPUT_STALLED" }),
      ),
    ).toBe("stalled");
  });
});

describe("setInputIssue", () => {
  afterEach(() => {
    vi.useRealTimers();
    useInputHealth.setState({ issues: new Map() });
  });

  it("records, replaces and clears a session's issue", () => {
    setInputIssue("s1", "suspended");
    expect(inputIssueOf("s1")).toBe("suspended");
    const before = useInputHealth.getState().issues;
    setInputIssue("s1", "suspended");
    expect(useInputHealth.getState().issues).toBe(before); // no-op keeps identity
    setInputIssue("s1", null);
    expect(inputIssueOf("s1")).toBeNull();
  });

  it("a stall badge expires when no further rejection arrives", () => {
    vi.useFakeTimers();
    setInputIssue("s1", "stalled");
    vi.advanceTimersByTime(INPUT_STALL_BADGE_MS - 1);
    setInputIssue("s1", "stalled"); // a fresh rejection restarts the window
    vi.advanceTimersByTime(INPUT_STALL_BADGE_MS - 1);
    expect(inputIssueOf("s1")).toBe("stalled");
    vi.advanceTimersByTime(1);
    expect(inputIssueOf("s1")).toBeNull();
  });

  it("a suspension does not expire on its own", () => {
    vi.useFakeTimers();
    setInputIssue("s1", "suspended");
    vi.advanceTimersByTime(INPUT_STALL_BADGE_MS * 4);
    expect(inputIssueOf("s1")).toBe("suspended");
  });
});

describe("guard mirror watch — suspended 전이 클리어", () => {
  const workload = (sessionId: string, guard: { kind: string }): WorkloadSummary =>
    ({ workload_id: `w-${sessionId}`, session_id: sessionId, mode: "shell", state: "RUNNING", guard }) as unknown as WorkloadSummary;

  afterEach(() => {
    useWorkbenchStore.setState({ workloads: [] });
    useInputHealth.setState({ issues: new Map() });
  });

  it("미러가 일시정지를 본 뒤 실행으로 풀리면 suspended 문제를 걷는다", () => {
    setInputIssue("s1", "suspended");
    // 첫 스냅샷: 일시정지 관측(이전 상태 없음 — 아직 아무것도 안 함).
    useWorkbenchStore.setState({ workloads: [workload("s1", { kind: "SUSPENDED" })] });
    expect(inputIssueOf("s1")).toBe("suspended");
    // 다음 스냅샷: 풀림 — 전이 관측 → 문제 제거.
    useWorkbenchStore.setState({ workloads: [workload("s1", { kind: "NONE" })] });
    expect(inputIssueOf("s1")).toBeNull();
  });

  it("일시정지를 본 적 없는 세션은 오래된 Running 스냅샷으로 걷지 않는다", () => {
    setInputIssue("s2", "suspended");
    useWorkbenchStore.setState({ workloads: [workload("s2", { kind: "NONE" })] });
    useWorkbenchStore.setState({ workloads: [workload("s2", { kind: "NONE" })] });
    expect(inputIssueOf("s2")).toBe("suspended"); // 전이가 없으면 유지
  });

  it("다른 세션의 전이는 이 세션의 문제를 건드리지 않는다", () => {
    setInputIssue("s3", "suspended");
    useWorkbenchStore.setState({ workloads: [workload("s4", { kind: "SUSPENDED" })] });
    useWorkbenchStore.setState({ workloads: [workload("s4", { kind: "NONE" })] });
    expect(inputIssueOf("s3")).toBe("suspended");
  });
});

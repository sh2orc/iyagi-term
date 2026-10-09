import { describe, expect, it } from "vitest";
import { MockDaemonClient } from "../src/features/daemon/mockClient";
import type { DaemonEvent } from "../src/features/daemon/client";
import { base64ToBytes } from "../src/features/daemon/base64";
import type { LaunchRequest } from "../src/generated/LaunchRequest";
import type { SessionOutput } from "../src/generated/SessionOutput";

function makeMock(extra: ConstructorParameters<typeof MockDaemonClient>[0] = {}) {
  let n = 0;
  const mock = new MockDaemonClient({
    schedule: (fn) => fn(), // 동기 flush — 결정적 테스트
    resourceIntervalMs: 0,
    uuid: () => `uuid-${++n}`,
    ...extra,
  });
  const events: DaemonEvent[] = [];
  const sub = mock.events.subscribe((ev) => events.push(ev));
  return { mock, events, dispose: () => sub.dispose() };
}

function shellRequest(cwd = "D:\\project"): LaunchRequest {
  return {
    request_id: `req-${Math.random().toString(36).slice(2)}`,
    profile_id: "default-shell",
    cwd,
    program: "C:\\Windows\\System32\\cmd.exe",
    argv: [],
    env_overrides: {},
    mode: "shell",
    cols: 80,
    rows: 24,
    priority: 1,
    policy: {
      reservation_bytes: "2147483648",
      cpu_slots: 1,
      enforcement: "observe",
      memory_max_bytes: null,
      cpu_max_cores: null,
      pids_max: null,
    },
  };
}

function managedRequest(name: string): LaunchRequest {
  return { ...shellRequest(), mode: "managed", program: `C:\\tools\\${name}.exe`, argv: ["run"] };
}

describe("MockDaemonClient — echo round-trip", () => {
  it("delivers journal records in order: initial resize, prompt, then echoes", async () => {
    const { mock, events } = makeMock();
    const outcome = await mock.workloadLaunch(shellRequest());
    expect(outcome.session_id).not.toBeNull();
    const attach = await mock.sessionAttach({ session_id: outcome.session_id as string, view_id: "v1", access: "writer" });
    expect(attach.epoch).toBeTruthy();
    expect(attach.replay_from_seq).toBe("1");
    expect(attach.last_seq).toBe("2");

    await mock.sessionInput({
      session_id: outcome.session_id as string,
      epoch: attach.epoch,
      input_id: "in-1",
      data_b64: btoa("ls\r"),
    });

    const outputs = events.filter((e) => e.kind === "session.output").map((e) => e.payload as SessionOutput);
    expect(outputs.map((o) => Number(o.seq))).toEqual([1, 2, 3]);
    expect(outputs[0].kind).toBe("resize");
    expect(outputs[0].cols).toBe(80);
    expect(outputs[1].kind).toBe("output");
    // 배너 문구 없이 프롬프트만 — 셸 종류(cmd)에 맞는 형태다.
    expect(new TextDecoder().decode(base64ToBytes(outputs[1].data_b64))).toBe("D:\\project> ");
    // echo: 마지막 record는 입력 그 자체
    expect(new TextDecoder().decode(base64ToBytes(outputs[2].data_b64))).toBe("ls\r");
    expect(outputs.every((o) => o.epoch === attach.epoch)).toBe(true);
  });

  it("rejects input with a stale epoch", async () => {
    const { mock } = makeMock();
    const outcome = await mock.workloadLaunch(shellRequest());
    const attach = await mock.sessionAttach({ session_id: outcome.session_id as string, view_id: "v1", access: "writer" });
    await expect(
      mock.sessionInput({ session_id: outcome.session_id as string, epoch: "old-epoch", input_id: "in", data_b64: btoa("x") }),
    ).rejects.toMatchObject({ code: "STALE_EPOCH" });
  });

  it("appends resize records into the same journal order", async () => {
    const { mock, events } = makeMock();
    const outcome = await mock.workloadLaunch(shellRequest());
    const sessionId = outcome.session_id as string;
    const attach = await mock.sessionAttach({ session_id: sessionId, view_id: "v1", access: "writer" });
    const result = await mock.sessionResize({
      session_id: outcome.session_id as string,
      epoch: attach.epoch,
      resize_id: "r1",
      cols: 120,
      rows: 40,
    });
    expect(result.applied_seq).toBe("3");
    const outputs = events.filter((e) => e.kind === "session.output").map((e) => e.payload as SessionOutput);
    expect(outputs[2].kind).toBe("resize");
    expect(outputs[2].cols).toBe(120);
    await expect(
      mock.sessionResize({ session_id: sessionId, epoch: attach.epoch, resize_id: "r2", cols: 0, rows: 0 }),
    ).rejects.toMatchObject({ code: "INVALID_ARGUMENT" });
  });
});

describe("MockDaemonClient — ACK accounting (02 §4)", () => {
  it("advances ackedSeq and drains unacked bytes", async () => {
    const { mock } = makeMock();
    const outcome = await mock.workloadLaunch(shellRequest());
    const sessionId = outcome.session_id as string;
    const attach = await mock.sessionAttach({ session_id: sessionId, view_id: "v1", access: "writer" });
    const before = mock.inspect(sessionId);
    expect(before.lastDeliveredSeq).toBe(2);
    expect(before.unackedBytes).toBeGreaterThan(0);
    mock.sessionAck({ session_id: sessionId, epoch: attach.epoch, through_seq: "2" });
    const after = mock.inspect(sessionId);
    expect(after.ackedSeq).toBe(2);
    expect(after.unackedBytes).toBe(0);
  });

  it("ignores duplicate ACKs", async () => {
    const { mock } = makeMock();
    const outcome = await mock.workloadLaunch(shellRequest());
    const sessionId = outcome.session_id as string;
    const attach = await mock.sessionAttach({ session_id: sessionId, view_id: "v1", access: "writer" });
    mock.sessionAck({ session_id: sessionId, epoch: attach.epoch, through_seq: "2" });
    expect(() => mock.sessionAck({ session_id: sessionId, epoch: attach.epoch, through_seq: "1" })).not.toThrow();
    expect(mock.protocolErrors).toHaveLength(0);
    expect(mock.inspect(sessionId).ackedSeq).toBe(2);
  });

  it("surfaces a protocol error for future-seq ACKs", async () => {
    const { mock } = makeMock();
    const outcome = await mock.workloadLaunch(shellRequest());
    const sessionId = outcome.session_id as string;
    const attach = await mock.sessionAttach({ session_id: sessionId, view_id: "v1", access: "writer" });
    expect(() => mock.sessionAck({ session_id: sessionId, epoch: attach.epoch, through_seq: "99" })).toThrow();
    expect(mock.protocolErrors.length).toBeGreaterThan(0);
    expect(mock.protocolErrors[0].detail).toContain("99");
  });

  it("ignores ACKs from a previous epoch", async () => {
    const { mock } = makeMock();
    const outcome = await mock.workloadLaunch(shellRequest());
    const sessionId = outcome.session_id as string;
    await mock.sessionAttach({ session_id: sessionId, view_id: "v1", access: "writer" });
    // 두 번째 attach가 epoch를 회전시킨 뒤 이전 epoch ACK는 무시된다.
    const attach2 = await mock.sessionAttach({ session_id: sessionId, view_id: "v2", access: "reader" });
    expect(() =>
      mock.sessionAck({ session_id: sessionId, epoch: "not-current", through_seq: "99" }),
    ).not.toThrow();
    expect(mock.protocolErrors).toHaveLength(0);
    expect(mock.inspect(sessionId).epoch).toBe(attach2.epoch);
  });
});

describe("MockDaemonClient — slow consumer flow control", () => {
  it("blocks at the 256 KiB high watermark and resumes at <= 64 KiB", async () => {
    const { mock, events } = makeMock();
    const outcome = await mock.workloadLaunch(shellRequest());
    const sessionId = outcome.session_id as string;
    const attach = await mock.sessionAttach({ session_id: sessionId, view_id: "v1", access: "writer" });
    // banner(2 records) already delivered.

    const total = 300 * 1024;
    const payload = new Uint8Array(total).fill(65);
    mock.emitProgramOutput(sessionId, payload);

    const state = mock.inspect(sessionId);
    expect(state.blocked).toBe(true);
    const blockedEvents = events.filter((e) => e.kind === "session.flow_blocked");
    expect(blockedEvents.some((e) => e.kind === "session.flow_blocked" && e.payload.blocked)).toBe(true);
    // 300 KiB 중 256 KiB까지만 전송되었다.
    expect(state.lastDeliveredSeq).toBeLessThan(state.recordCount);

    // unacked가 64 KiB 이하가 되도록 ACK한다 (16 KiB record × 4).
    const resumeThrough = state.lastDeliveredSeq - 4;
    mock.sessionAck({ session_id: sessionId, epoch: attach.epoch, through_seq: String(resumeThrough) });
    const resumed = mock.inspect(sessionId);
    expect(resumed.blocked).toBe(false);
    expect(
      events.filter((e) => e.kind === "session.flow_blocked").some((e) => e.kind === "session.flow_blocked" && !e.payload.blocked),
    ).toBe(true);
    // 재개 후 남은 record가 전송되었다.
    expect(resumed.lastDeliveredSeq).toBe(resumed.recordCount);
  });
});

describe("MockDaemonClient — queue behavior (U14)", () => {
  it("queues managed launches beyond concurrency=2 and creates no session while queued", async () => {
    const { mock } = makeMock();
    const first = await mock.workloadLaunch(managedRequest("codex"));
    const second = await mock.workloadLaunch(managedRequest("claude"));
    const third = await mock.workloadLaunch(managedRequest("opencode"));

    expect(first.state).toBe("RUNNING");
    expect(second.state).toBe("RUNNING");
    expect(third.state).toBe("QUEUED");
    expect(third.session_id).toBeNull(); // 대기 중 placeholder는 session/PTY 없음

    const snapshot = await mock.systemSnapshot();
    expect(snapshot.queue.length).toBe(1);
    expect(snapshot.queue[0].workload_id).toBe(third.workload_id);

    mock.refreshQueue("WAIT_CONCURRENCY");
    const snapshot2 = await mock.systemSnapshot();
    expect(snapshot2.queue[0].wait_reason).toBe("WAIT_CONCURRENCY");
    expect(snapshot2.revision).toBeGreaterThan(snapshot.revision);
  });

  it("cancels a queued workload and empties the queue", async () => {
    const { mock } = makeMock();
    await mock.workloadLaunch(managedRequest("codex"));
    await mock.workloadLaunch(managedRequest("claude"));
    const third = await mock.workloadLaunch(managedRequest("opencode"));
    const result = await mock.workloadCancel({ request_id: "r", workload_id: third.workload_id });
    expect(result.state).toBe("CANCELLED");
    const snapshot = await mock.systemSnapshot();
    expect(snapshot.queue).toHaveLength(0);
  });

  it("is idempotent per request_id", async () => {
    const { mock } = makeMock();
    const request = shellRequest();
    const a = await mock.workloadLaunch(request);
    const b = await mock.workloadLaunch(request);
    expect(a.workload_id).toBe(b.workload_id);
  });
});

describe("MockDaemonClient — exit and snapshot", () => {
  it("emits session.exited and moves the workload to SUCCEEDED", async () => {
    const { mock, events } = makeMock();
    const outcome = await mock.workloadLaunch(shellRequest());
    mock.killSession(outcome.session_id as string, 0);
    expect(events.some((e) => e.kind === "session.exited")).toBe(true);
    const snapshot = await mock.systemSnapshot();
    expect(snapshot.workloads[0].state).toBe("SUCCEEDED");
  });

  it("serves resource snapshots on demand", async () => {
    const { mock } = makeMock();
    mock.emitResourceSnapshot();
    const snapshot = await mock.systemSnapshot();
    expect(snapshot.host.logical_cpu_count).toBeGreaterThan(0);
    expect(snapshot.capabilities.platform).toBe("mock");
    expect(snapshot.reconciliation_required).toBe(false);
  });
});

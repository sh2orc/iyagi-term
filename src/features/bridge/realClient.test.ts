/**
 * RealDaemonClient unit tests against a fake IpcAdapter harness: scripted
 * hello/connect, RPC mapping with out-of-order resolutions, error mapping,
 * the single DAEMON_UNAVAILABLE retry, per-session output channels, and the
 * isTauri fallback. No Tauri runtime is involved.
 */

import { describe, expect, it, vi } from "vitest";
import type { Policy } from "../../generated/Policy";
import type { CliCandidate } from "./systemProbe";
import { mockSystemProbe, tauriSystemProbe } from "./systemProbe";
import { isTauri } from "./ipc";
import type { IpcAdapter } from "./ipc";
import { createDaemonClient, RealDaemonClient } from "./realClient";
import { MockDaemonClient } from "../daemon/mockClient";
import { RpcClientError } from "../daemon/client";
import { GraphCollector } from "../monitor/graphRing";

/** Fake channel: records the sink so tests can push messages into it. */
interface FakeChannel {
  __fakeChannel: true;
  onmessage: (message: unknown) => void;
}

class FakeIpc implements IpcAdapter {
  readonly calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
  readonly channels: FakeChannel[] = [];
  private readonly handlers = new Map<string, (args: Record<string, unknown>) => Promise<unknown>>();

  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    this.calls.push({ command, args });
    const handler = this.handlers.get(command);
    if (!handler) return Promise.reject(new Error(`no handler for ${command}`));
    return handler(args ?? {}) as Promise<T>;
  }

  channel<T>(onMessage: (message: T) => void): unknown {
    const channel: FakeChannel = {
      __fakeChannel: true,
      onmessage: (message) => onMessage(message as T),
    };
    this.channels.push(channel);
    return channel;
  }

  handle(command: string, handler: (args: Record<string, unknown>) => Promise<unknown>): void {
    this.handlers.set(command, handler);
  }

  commandNames(): string[] {
    return this.calls.map((call) => call.command);
  }

  /** Deliver a message to the nth created channel (0 = events). */
  emit(index: number, message: unknown): void {
    this.channels[index].onmessage(message);
  }

  bridgeRpcArgs(): Array<{ id: string; method: string; params: unknown }> {
    return this.calls
      .filter((call) => call.command === "bridge_rpc")
      .map((call) => call.args as { id: string; method: string; params: unknown });
  }
}

function helloResult() {
  return {
    daemon_id: "d-1",
    protocol: 1,
    connection_id: "00000000-0000-4000-8000-000000000001",
    data_token: "secret-data-token",
    capabilities: { platform: "test" },
  };
}

/** Standard lifecycle handlers; tests override bridge_rpc as needed. */
function stubLifecycle(ipc: FakeIpc): void {
  ipc.handle("bridge_connect", async () => helloResult());
  ipc.handle("bridge_open_data", async () => null);
  ipc.handle("bridge_disconnect", async () => null);
  ipc.handle("bridge_subscribe_events", async () => null);
  ipc.handle("bridge_subscribe_session", async () => null);
  ipc.handle("bridge_unsubscribe_session", async () => null);
  ipc.handle("bridge_ack", async () => null);
  ipc.handle("bridge_connection_status", async () => ({
    control_alive: true,
    data_alive: true,
  }));
}

/** The connected daemon predates this app build (what the restart strip offers to fix). */
function stubOutdated(ipc: FakeIpc): void {
  ipc.handle("bridge_connection_status", async () => ({
    control_alive: true,
    data_alive: true,
    daemon_outdated: true,
  }));
}

async function flush(times = 8): Promise<void> {
  for (let i = 0; i < times; i++) {
    await Promise.resolve();
  }
}

describe("RealDaemonClient lifecycle", () => {
  it.each(["bridge_connect", "bridge_open_data", "bridge_subscribe_events"])(
    "preserves typed errors from %s so launch failures show their cause",
    async (command) => {
      const ipc = new FakeIpc();
      stubLifecycle(ipc);
      ipc.handle(command, async () => {
        throw { code: "PROTOCOL_MISMATCH", message: "hello result malformed", retryable: false };
      });
      const client = new RealDaemonClient(ipc);
      const error = await client.systemSnapshot().catch((err: unknown) => err);
      expect(error).toBeInstanceOf(RpcClientError);
      expect(error).toMatchObject({ code: "PROTOCOL_MISMATCH", message: "hello result malformed" });
      expect(ipc.bridgeRpcArgs()).toHaveLength(0);
    },
  );

  it("connects, opens data, then subscribes events on first subscribe", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    const client = new RealDaemonClient(ipc);

    const seen: string[] = [];
    client.events.subscribe((event) => seen.push(event.kind));
    await flush();

    expect(ipc.commandNames().slice(0, 3)).toEqual([
      "bridge_connect",
      "bridge_open_data",
      "bridge_subscribe_events",
    ]);
    expect(ipc.channels.length).toBe(1);
    expect(seen).toEqual([]);
  });

  it("connect is single-flight across concurrent calls", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    const client = new RealDaemonClient(ipc);
    const sub1 = client.events.subscribe(() => undefined);
    void client.systemSnapshot().catch(() => undefined);
    await flush();
    const connects = ipc.calls.filter((call) => call.command === "bridge_connect").length;
    expect(connects).toBe(1);
    sub1.dispose();
  });

  it("reports split-transport health and advances generation after reconnect", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    ipc.handle("bridge_connection_status", async () => ({
      control_alive: true,
      data_alive: false,
    }));
    const client = new RealDaemonClient(ipc);

    expect(await client.transportStatus()).toEqual({
      controlAlive: true,
      dataAlive: false,
      generation: 0,
      daemonOutdated: false,
    });
    await client.reconnectTransport();
    expect((await client.transportStatus()).generation).toBe(1);
    expect(ipc.commandNames()).toContain("bridge_disconnect");
  });

  it("maps the daemon-outdated flag from bridge_connection_status", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    ipc.handle("bridge_connection_status", async () => ({
      control_alive: true,
      data_alive: true,
      daemon_outdated: true,
    }));
    const client = new RealDaemonClient(ipc);
    expect((await client.transportStatus()).daemonOutdated).toBe(true);
  });

  it("restartDaemon retires the daemon without killing workloads, then reconnects", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    stubOutdated(ipc);
    ipc.handle("bridge_rpc", async () => ({ shutting_down: true }));
    const client = new RealDaemonClient(ipc);
    // Establish the control connection first (restartDaemon ensures it).
    client.events.subscribe(() => undefined);
    await flush();

    await client.restartDaemon();

    const shutdown = ipc
      .bridgeRpcArgs()
      .find((call) => call.method === "daemon.shutdown");
    expect(shutdown?.params).toEqual({ stop_workloads: false });
    // Reconnect path: disconnect then a fresh connect spawns a new daemon.
    const names = ipc.commandNames();
    expect(names).toContain("bridge_disconnect");
    expect(names.indexOf("bridge_connect")).toBeGreaterThanOrEqual(0);
  });

  it("restartDaemon reconnects even when the shutdown request is dropped", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    stubOutdated(ipc);
    // The daemon drops the control connection as it retires — the request
    // rejects, but the reconnect must still bring a fresh daemon up.
    ipc.handle("bridge_rpc", async () => {
      throw { code: "DAEMON_UNAVAILABLE", message: "daemon connection closed", retryable: true };
    });
    const client = new RealDaemonClient(ipc);
    client.events.subscribe(() => undefined);
    await flush();

    await expect(client.restartDaemon()).resolves.toBeUndefined();
    expect(ipc.commandNames()).toContain("bridge_disconnect");
  });

  it("restartDaemon leaves a daemon that is already current running", async () => {
    // An earlier restart timed out but the fresh daemon came up meanwhile: a
    // second click must not retire it (and every terminal with it).
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    ipc.handle("bridge_rpc", async () => ({ shutting_down: true }));
    const client = new RealDaemonClient(ipc);
    client.events.subscribe(() => undefined);
    await flush();
    const before = ipc.commandNames().length;

    await client.restartDaemon();

    expect(ipc.bridgeRpcArgs().some((call) => call.method === "daemon.shutdown")).toBe(false);
    expect(ipc.commandNames().slice(before)).not.toContain("bridge_disconnect");
  });

  it("a throwing listener does not starve the others or later events", async () => {
    // Tauri's Channel only advances its message index after onmessage
    // returns: an exception escaping dispatch froze every later event.
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    const client = new RealDaemonClient(ipc);
    const seen: string[] = [];
    const errors = vi.spyOn(console, "error").mockImplementation(() => undefined);
    client.events.subscribe(() => {
      throw new Error("listener bug");
    });
    client.events.subscribe((event) => seen.push(event.kind));
    await flush();

    expect(() => ipc.emit(0, { event: "queue.changed", payload: { queue: [], revision: 1 } })).not.toThrow();
    ipc.emit(0, { event: "queue.changed", payload: { queue: [], revision: 2 } });
    expect(seen).toEqual(["queue.changed", "queue.changed"]);
    expect(errors).toHaveBeenCalled();
    errors.mockRestore();
  });

  it("drops listeners when an HMR-owned client is disposed", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    const client = new RealDaemonClient(ipc);
    const seen: string[] = [];
    client.events.subscribe((event) => seen.push(event.kind));
    await flush();
    client.dispose();

    ipc.emit(0, { event: "queue.changed", payload: { queue: [], revision: 1 } });
    expect(seen).toEqual([]);
  });
});

describe("RealDaemonClient rpc mapping", () => {
  it("maps methods to bridge_rpc with fresh printable ids", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    ipc.handle("bridge_rpc", async (args) => {
      const { method } = args as { method: string };
      return { method };
    });
    const client = new RealDaemonClient(ipc);

    const snapshot = await client.systemSnapshot();
    expect((snapshot as unknown as { method: string }).method).toBe("system.snapshot");

    await client.workloadCancel({
      request_id: "00000000-0000-4000-8000-0000000000aa",
      workload_id: "00000000-0000-4000-8000-0000000000bb",
    });
    const rpcArgs = ipc.bridgeRpcArgs();
    expect(rpcArgs.map((call) => call.method)).toEqual(["system.snapshot", "workload.cancel"]);
    for (const call of rpcArgs) {
      expect(call.id).toMatch(/^[0-9a-f-]{36}$/);
    }
    expect(rpcArgs[1].params).toEqual({
      request_id: "00000000-0000-4000-8000-0000000000aa",
      workload_id: "00000000-0000-4000-8000-0000000000bb",
    });
  });

  it("matches responses to callers even when resolutions are out of order", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    ipc.handle("bridge_rpc", async (args) => {
      const { method } = args as { method: string };
      if (method === "system.snapshot") {
        // Slow call resolves LAST; the fast one must not steal its result.
        await new Promise((resolve) => setTimeout(resolve, 20));
        return { revision: 1, slow: true };
      }
      return { fast: true };
    });
    const client = new RealDaemonClient(ipc);

    const slow = client.systemSnapshot() as Promise<unknown> as Promise<{ slow: boolean }>;
    const fast = (await client.workloadCancel({
      request_id: "r",
      workload_id: "w",
    })) as unknown as { fast: boolean };
    expect(fast.fast).toBe(true);
    expect((await slow).slow).toBe(true);
  });

  it("maps serialized RpcError rejections to RpcClientError", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    ipc.handle("bridge_rpc", async () => {
      throw { code: "STALE_EPOCH", message: "epoch rotated", retryable: false };
    });
    const client = new RealDaemonClient(ipc);

    const error = await client
      .sessionInput({
        session_id: "s",
        epoch: "e",
        input_id: "i",
        data_b64: "aGk=",
      })
      .catch((err: unknown) => err);
    expect(error).toBeInstanceOf(RpcClientError);
    const rpcError = error as RpcClientError;
    expect(rpcError.code).toBe("STALE_EPOCH");
    expect(rpcError.retryable).toBe(false);
    expect(rpcError.message).toBe("epoch rotated");
  });

  it("wraps non-RPC rejections as plain errors (never crashes on odd shapes)", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    ipc.handle("bridge_rpc", async () => {
      throw "wire cut";
    });
    const client = new RealDaemonClient(ipc);
    await expect(client.systemSnapshot()).rejects.toThrow("wire cut");
  });
});

describe("RealDaemonClient retry semantics", () => {
  it("retries exactly once on DAEMON_UNAVAILABLE after a full reconnect", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    let rpcAttempts = 0;
    ipc.handle("bridge_rpc", async () => {
      rpcAttempts += 1;
      if (rpcAttempts === 1) {
        throw { code: "DAEMON_UNAVAILABLE", message: "gone", retryable: true };
      }
      return { revision: 9 };
    });
    const client = new RealDaemonClient(ipc);

    const snapshot = (await client.systemSnapshot()) as unknown as { revision: number };
    expect(snapshot.revision).toBe(9);
    expect(rpcAttempts).toBe(2);
    const names = ipc.commandNames();
    // Reconnect ran between the two attempts.
    expect(names.indexOf("bridge_disconnect")).toBeGreaterThan(names.indexOf("bridge_rpc"));
    expect(names.indexOf("bridge_rpc", names.indexOf("bridge_disconnect"))).toBeGreaterThan(0);
  });

  it("does NOT retry non-retryable rejections", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    let attempts = 0;
    ipc.handle("bridge_rpc", async () => {
      attempts += 1;
      throw { code: "INVALID_ARGUMENT", message: "bad", retryable: false };
    });
    const client = new RealDaemonClient(ipc);
    await expect(client.systemSnapshot()).rejects.toBeInstanceOf(RpcClientError);
    expect(attempts).toBe(1);
  });

  it("fails after one retry instead of looping forever", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    let attempts = 0;
    ipc.handle("bridge_rpc", async () => {
      attempts += 1;
      throw { code: "DAEMON_UNAVAILABLE", message: "still gone", retryable: true };
    });
    const client = new RealDaemonClient(ipc);
    await expect(client.systemSnapshot()).rejects.toBeInstanceOf(RpcClientError);
    expect(attempts).toBe(2);
  });
});

describe("RealDaemonClient session streams", () => {
  it("expands a native output batch without dropping resize or epoch boundaries or acknowledging early", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    ipc.handle("bridge_rpc", async () => ({ epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 }));
    const client = new RealDaemonClient(ipc);
    const seen: unknown[] = [];
    client.events.subscribe((event) => seen.push(event.payload));
    await client.sessionAttach({ session_id: "s1", view_id: "v1", access: "writer" });
    const records = [
      { session_id: "s1", epoch: "e1", seq: "1", kind: "output", data_b64: "YQ==", raw_len: 1 },
      { session_id: "s1", epoch: "e1", seq: "2", kind: "resize", data_b64: "", raw_len: 0, cols: 90, rows: 30 },
      { session_id: "s1", epoch: "e1", seq: "3", kind: "output", data_b64: "Yg==", raw_len: 1 },
      { session_id: "s1", epoch: "e2", seq: "4", kind: "output", data_b64: "Yw==", raw_len: 1 },
    ];
    ipc.emit(1, { event: "session.output.batch", payload: records });
    expect(seen).toEqual(records);
    expect(ipc.commandNames()).not.toContain("bridge_ack");
    client.dispose();
  });

  it("unwraps daemon resource snapshots before handing them to UI consumers", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    const client = new RealDaemonClient(ipc);
    const mock = new MockDaemonClient({ resourceIntervalMs: 0 });
    const { host } = await mock.systemSnapshot();
    const graph = new GraphCollector();
    let received: unknown;
    client.events.subscribe((event) => {
      if (event.kind !== "resource.snapshot") return;
      received = event.payload;
      graph.push(event.payload);
    });
    await flush();

    expect(() => ipc.emit(0, {
      event: "resource.snapshot",
      payload: { revision: 7, host, pressure: "WARNING" },
    })).not.toThrow();
    expect(received).toEqual({ ...host, pressure: "WARNING" });
    expect(graph.rings.cpuCores.length).toBe(1);
  });

  it("subscribes the session output channel BEFORE attach, then routes outputs", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    ipc.handle("bridge_rpc", async (args) => {
      const { method } = args as { method: string };
      if (method === "session.attach") {
        return { epoch: "e1", replay_from_seq: "1", last_seq: "2", cols: 80, rows: 24 };
      }
      return null;
    });
    const client = new RealDaemonClient(ipc);

    const seen: Array<{ kind: string; payload: unknown }> = [];
    client.events.subscribe((event) => seen.push({ kind: event.kind, payload: event.payload }));
    await flush();

    const attach = client.sessionAttach({
      session_id: "00000000-0000-4000-8000-0000000000cc",
      view_id: "00000000-0000-4000-8000-0000000000dd",
      access: "writer",
    });
    await flush();
    // Order check: subscribe_session precedes the attach rpc on the wire.
    const names = ipc.commandNames();
    expect(names.indexOf("bridge_subscribe_session")).toBeGreaterThan(
      names.indexOf("bridge_open_data"),
    );
    expect(names.indexOf("bridge_rpc")).toBeGreaterThan(names.indexOf("bridge_subscribe_session"));

    const result = await attach;
    expect(result.epoch).toBe("e1");
    // The bridge keys output channels by view so re-attaches replace instead
    // of accumulating — both ids travel with the subscription.
    const subscribe = ipc.calls.find((call) => call.command === "bridge_subscribe_session");
    expect(subscribe?.args).toMatchObject({
      sessionId: "00000000-0000-4000-8000-0000000000cc",
      viewId: "00000000-0000-4000-8000-0000000000dd",
    });

    // Push a session.output record through the session channel (index 1:
    // 0 = events, 1 = session) and see it fanned into the event stream.
    ipc.emit(1, {
      event: "session.output",
      payload: {
        session_id: "00000000-0000-4000-8000-0000000000cc",
        epoch: "e1",
        seq: "1",
        kind: "output",
        data_b64: "aGk=",
        raw_len: 2,
      },
    });
    expect(seen).toHaveLength(1);
    expect(seen[0].kind).toBe("session.output");
    expect((seen[0].payload as { data_b64: string }).data_b64).toBe("aGk=");
  });

  it("re-subscribes the view after the reconnect that precedes an attach retry", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    let attachAttempts = 0;
    ipc.handle("bridge_rpc", async (args) => {
      const { method } = args as { method: string };
      if (method !== "session.attach") return null;
      attachAttempts += 1;
      if (attachAttempts === 1) {
        throw { code: "DAEMON_UNAVAILABLE", message: "gone", retryable: true };
      }
      return { epoch: "e2", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 };
    });
    const client = new RealDaemonClient(ipc);
    const result = await client.sessionAttach({
      session_id: "00000000-0000-4000-8000-0000000000cc",
      view_id: "00000000-0000-4000-8000-0000000000dd",
      access: "writer",
    });
    expect(result.epoch).toBe("e2");
    expect(attachAttempts).toBe(2);
    const names = ipc.commandNames();
    const disconnectAt = names.indexOf("bridge_disconnect");
    expect(disconnectAt).toBeGreaterThan(0);
    // bridge_disconnect wiped the bridge's channel table: a fresh
    // subscribe must land between the reconnect and the retried attach.
    const resubscribeAt = names.indexOf("bridge_subscribe_session", disconnectAt);
    const retryAt = names.lastIndexOf("bridge_rpc");
    expect(resubscribeAt).toBeGreaterThan(disconnectAt);
    expect(retryAt).toBeGreaterThan(resubscribeAt);
    expect(names.filter((name) => name === "bridge_subscribe_session")).toHaveLength(2);
  });

  it("unsubscribes a view through bridge_unsubscribe_session (fire-and-forget)", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    const client = new RealDaemonClient(ipc);
    client.sessionUnsubscribe({
      session_id: "00000000-0000-4000-8000-0000000000cc",
      view_id: "00000000-0000-4000-8000-0000000000dd",
    });
    await flush();
    const call = ipc.calls.find((entry) => entry.command === "bridge_unsubscribe_session");
    expect(call?.args).toEqual({
      sessionId: "00000000-0000-4000-8000-0000000000cc",
      viewId: "00000000-0000-4000-8000-0000000000dd",
    });
    // A failing unsubscribe never surfaces (the bridge treats unknown ids as no-ops).
    ipc.handle("bridge_unsubscribe_session", async () => {
      throw new Error("boom");
    });
    expect(() => client.sessionUnsubscribe({ session_id: "s", view_id: "v" })).not.toThrow();
    await flush();
  });

  it("routes control events from the event channel", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    ipc.handle("bridge_rpc", async () => null);
    const client = new RealDaemonClient(ipc);
    const seen: string[] = [];
    client.events.subscribe((event) => seen.push(event.kind));
    await flush();

    ipc.emit(0, { event: "workload.changed", payload: { workload_id: "w", state: "RUNNING" } });
    ipc.emit(0, { event: "queue.changed", payload: { queue: [], revision: 4 } });
    ipc.emit(0, { event: "not.a-real-event", payload: {} });
    expect(seen).toEqual(["workload.changed", "queue.changed"]);
  });

  it("sends ACKs through bridge_ack on the data connection (camelCase args)", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    const client = new RealDaemonClient(ipc);
    client.events.subscribe(() => undefined);
    await flush();

    client.sessionAck({
      session_id: "00000000-0000-4000-8000-0000000000cc",
      epoch: "e1",
      through_seq: "3",
    });
    await flush();
    const ackCall = ipc.calls.find((call) => call.command === "bridge_ack");
    expect(ackCall?.args).toEqual({
      sessionId: "00000000-0000-4000-8000-0000000000cc",
      epoch: "e1",
      throughSeq: "3",
    });
  });
});

describe("RealDaemonClient session focus", () => {
  it("maps session.focus onto bridge_rpc and returns the daemon aggregate", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    ipc.handle("bridge_rpc", async (args) => {
      const { params } = args as { params: { session_id: string | null } };
      return { focused_session_ids: params.session_id ? [params.session_id] : [] };
    });
    const client = new RealDaemonClient(ipc);

    expect(await client.sessionFocus({ session_id: "session-1" })).toEqual({
      focused_session_ids: ["session-1"],
    });
    expect(await client.sessionFocus({ session_id: null })).toEqual({ focused_session_ids: [] });

    expect(ipc.bridgeRpcArgs()).toEqual([
      { id: expect.any(String), method: "session.focus", params: { session_id: "session-1" } },
      { id: expect.any(String), method: "session.focus", params: { session_id: null } },
    ]);
  });
});

// 08-pressure-relief §2: 수동 완화·정책 토글은 계약의 method 이름 그대로
// bridge_rpc에 실린다(전송만 — 의미는 데몬이 지킨다).
describe("RealDaemonClient relief methods", () => {
  it("maps session.relief / relief.set_policy onto bridge_rpc", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    ipc.handle("bridge_rpc", async (args) => {
      const { method, params } = args as { method: string; params: Record<string, unknown> };
      return method === "session.relief"
        ? { relief: { kind: "YIELDED", since_ms: "7", manual: true, partial: false }, protected: false }
        : { auto_yield: params.auto_yield };
    });
    const client = new RealDaemonClient(ipc);

    expect(await client.sessionRelief({ session_id: "session-1", action: "yield" })).toEqual({
      relief: { kind: "YIELDED", since_ms: "7", manual: true, partial: false },
      protected: false,
    });
    expect(await client.reliefSetPolicy({ auto_yield: false })).toEqual({ auto_yield: false });

    expect(ipc.bridgeRpcArgs()).toEqual([
      {
        id: expect.any(String),
        method: "session.relief",
        params: { session_id: "session-1", action: "yield" },
      },
      { id: expect.any(String), method: "relief.set_policy", params: { auto_yield: false } },
    ]);
  });
});

describe("RealDaemonClient agent session methods", () => {
  it("maps agent_session.list / agent_session.forget onto bridge_rpc", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    ipc.handle("bridge_rpc", async (args) => {
      const { method } = args as { method: string };
      return method === "agent_session.list" ? [{ id: "rec-1" }] : { forgotten: true };
    });
    const client = new RealDaemonClient(ipc);

    const records = await client.agentSessionList({ limit: 100 });
    expect(records).toEqual([{ id: "rec-1" }]);
    const forget = await client.agentSessionForget({ id: "rec-1" });
    expect(forget).toEqual({ forgotten: true });

    expect(ipc.bridgeRpcArgs()).toEqual([
      { id: expect.any(String), method: "agent_session.list", params: { limit: 100 } },
      { id: expect.any(String), method: "agent_session.forget", params: { id: "rec-1" } },
    ]);
  });

  it("sends an empty params object when the caller passes none", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    ipc.handle("bridge_rpc", async () => []);
    await new RealDaemonClient(ipc).agentSessionList();
    expect(ipc.bridgeRpcArgs()[0].params).toEqual({});
  });

  it("surfaces METHOD_NOT_FOUND from an older daemon as a typed error", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    ipc.handle("bridge_rpc", async () => {
      throw { code: "METHOD_NOT_FOUND", message: "agent_session.list", retryable: false };
    });
    const error = await new RealDaemonClient(ipc).agentSessionList().catch((err: unknown) => err);
    expect(error).toBeInstanceOf(RpcClientError);
    expect((error as RpcClientError).code).toBe("METHOD_NOT_FOUND");
  });
});

describe("RealDaemonClient mission RPCs", () => {
  function fakePolicy(): Policy {
    return {
      max_parallel_runs: 2,
      max_attempts_per_task: 3,
      max_repair_cycles: 2,
      max_automatic_starts: 1,
      active_time_limit_ms: "3600000",
      run_time_limit_ms: "1800000",
      max_cost_usd_micros: null,
      unknown_cost: "allow_with_notice",
      allow_network: false,
      allow_automatic_plan_apply: false,
      allow_recovery_of_unsent: false,
      allowed_binding_ids: [],
      allowed_roles: ["lead"],
      allowed_verification_ids: [],
      require_independent_review: true,
      require_enforced_verification: true,
    };
  }

  it("maps mission/artifact/config methods onto bridge_rpc verbatim", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    ipc.handle("bridge_rpc", async (args) => {
      const { method } = args as { method: string };
      return { method };
    });
    const client = new RealDaemonClient(ipc);
    const goalRef = {
      id: "00000000-0000-4000-8000-0000000000g1",
      sha256: "a".repeat(64),
      bytes: "128",
      media_type: "text/markdown",
    };

    await client.missionCreate({
      request_id: "req-1",
      title: "ship it",
      repository_path: "D:/repo",
      expected_base_oid: "b".repeat(40),
      goal_ref: goalRef,
      requirements: [],
      policy: fakePolicy(),
      role_bindings: [],
      follow_up_of: null,
    });
    await client.missionList({ cursor: null, limit: 50, archived: false });
    await client.missionSnapshot({ mission_id: "m-1", snapshot_id: null, cursor: null });
    await client.missionEvents({ mission_id: "m-1", after_seq: "0", limit: 100 });
    await client.missionControl({ request_id: "req-2", mission_id: "m-1", expected_revision: "3", action: "pause" });
    await client.missionRequestGet({ request_id: "req-2" });
    await client.missionActivity({ mission_id: "m-1", run_id: "r-1", after_offset: "0", max_bytes: 4096 });
    await client.missionRunAttestExited({
      request_id: "req-4",
      mission_id: "m-1",
      expected_revision: "5",
      run_id: "r-2",
      attestation: "process_absent_confirmed",
    });
    await client.workspaceUsage({ mission_id: null });
    await client.workspaceCleanup({ request_id: "req-5", mission_id: "m-1" });
    await client.bindingList();
    await client.bindingProbe({ binding_id: "b-1" });
    await client.runtimeDetect();
    await client.templateList({ repository_id: null });
    await client.verificationList({ repository_id: "repo-1" });
    await client.artifactBegin({
      request_id: "req-3",
      mission_id: null,
      media_type: "text/plain",
      bytes: "9450",
      sha256: "c".repeat(64),
    });
    await client.artifactWrite({ upload_id: "u-1", offset: "0", data_b64: "aGk=" });
    await client.artifactCommit({ upload_id: "u-1" });
    await client.artifactRead({ artifact_id: "a-1", offset: "0", max_bytes: 4096 });

    expect(ipc.bridgeRpcArgs()).toEqual([
      {
        id: expect.any(String),
        method: "mission.create",
        params: {
          request_id: "req-1",
          title: "ship it",
          repository_path: "D:/repo",
          expected_base_oid: "b".repeat(40),
          goal_ref: goalRef,
          requirements: [],
          policy: fakePolicy(),
          role_bindings: [],
          follow_up_of: null,
        },
      },
      { id: expect.any(String), method: "mission.list", params: { cursor: null, limit: 50, archived: false } },
      { id: expect.any(String), method: "mission.snapshot", params: { mission_id: "m-1", snapshot_id: null, cursor: null } },
      { id: expect.any(String), method: "mission.events", params: { mission_id: "m-1", after_seq: "0", limit: 100 } },
      {
        id: expect.any(String),
        method: "mission.control",
        params: { request_id: "req-2", mission_id: "m-1", expected_revision: "3", action: "pause" },
      },
      { id: expect.any(String), method: "mission.request.get", params: { request_id: "req-2" } },
      {
        id: expect.any(String),
        method: "mission.activity",
        params: { mission_id: "m-1", run_id: "r-1", after_offset: "0", max_bytes: 4096 },
      },
      {
        id: expect.any(String),
        method: "mission.run.attest_exited",
        params: {
          request_id: "req-4",
          mission_id: "m-1",
          expected_revision: "5",
          run_id: "r-2",
          attestation: "process_absent_confirmed",
        },
      },
      { id: expect.any(String), method: "workspace.usage", params: { mission_id: null } },
      { id: expect.any(String), method: "workspace.cleanup", params: { request_id: "req-5", mission_id: "m-1" } },
      { id: expect.any(String), method: "binding.list", params: {} },
      { id: expect.any(String), method: "binding.probe", params: { binding_id: "b-1" } },
      { id: expect.any(String), method: "runtime.detect", params: {} },
      { id: expect.any(String), method: "template.list", params: { repository_id: null } },
      { id: expect.any(String), method: "verification.list", params: { repository_id: "repo-1" } },
      {
        id: expect.any(String),
        method: "artifact.begin",
        params: { request_id: "req-3", mission_id: null, media_type: "text/plain", bytes: "9450", sha256: "c".repeat(64) },
      },
      { id: expect.any(String), method: "artifact.write", params: { upload_id: "u-1", offset: "0", data_b64: "aGk=" } },
      { id: expect.any(String), method: "artifact.commit", params: { upload_id: "u-1" } },
      { id: expect.any(String), method: "artifact.read", params: { artifact_id: "a-1", offset: "0", max_bytes: 4096 } },
    ]);
  });

  it("maps mission.changed notifications from the event channel", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    const client = new RealDaemonClient(ipc);
    const seen: Array<{ kind: string; payload: unknown }> = [];
    client.events.subscribe((event) => seen.push({ kind: event.kind, payload: event.payload }));
    await flush();

    ipc.emit(0, { event: "mission.changed", payload: { mission_id: "m-1", latest_seq: "42" } });
    expect(seen).toEqual([{ kind: "mission.changed", payload: { mission_id: "m-1", latest_seq: "42" } }]);
  });

  it("preserves structured error details on RpcClientError (01 §7)", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    ipc.handle("bridge_rpc", async () => {
      throw {
        code: "REVISION_CONFLICT",
        message: "stale expected_revision",
        retryable: false,
        details: { current_revision: "7", retry_after_ms: null },
      };
    });
    const client = new RealDaemonClient(ipc);

    const error = await client
      .missionControl({ request_id: "r", mission_id: "m-1", expected_revision: "6", action: "start" })
      .catch((err: unknown) => err);
    expect(error).toBeInstanceOf(RpcClientError);
    const rpcError = error as RpcClientError;
    expect(rpcError.code).toBe("REVISION_CONFLICT");
    expect(rpcError.retryable).toBe(false);
    // null은 미관측이 아니라 '없음'으로 정규화 — 0으로 바꾸지 않는다.
    expect(rpcError.details).toEqual({ current_revision: "7" });
  });

  it("keeps retry_after_ms and decision_id details and omits absent ones", async () => {
    const ipc = new FakeIpc();
    stubLifecycle(ipc);
    let thrown = 0;
    ipc.handle("bridge_rpc", async () => {
      thrown += 1;
      throw {
        code: thrown === 1 ? "PROVIDER_RATE_LIMITED" : "STALE_DECISION",
        message: "limited",
        retryable: false,
        details:
          thrown === 1 ? { retry_after_ms: 30_000 } : { decision_id: "d-1", reason_code: null },
      };
    });
    const client = new RealDaemonClient(ipc);

    const rateLimited = (await client
      .missionMessage({
        request_id: "r1",
        mission_id: "m-1",
        expected_revision: "1",
        target_task_id: null,
        body_ref: { id: "g", sha256: "a".repeat(64), bytes: "1", media_type: "text/plain" },
      })
      .catch((err: unknown) => err)) as RpcClientError;
    expect(rateLimited.details).toEqual({ retry_after_ms: 30_000 });

    const stale = (await client
      .missionDecisionAnswer({
        request_id: "r2",
        mission_id: "m-1",
        expected_revision: "1",
        decision_id: "d-0",
        option_id: null,
        answer_ref: null,
      })
      .catch((err: unknown) => err)) as RpcClientError;
    expect(stale.details).toEqual({ decision_id: "d-1" });

    // details 없는 오류는 details 필드 자체가 없다.
    ipc.handle("bridge_rpc", async () => {
      throw { code: "INVALID_ARGUMENT", message: "no details", retryable: false };
    });
    const bare = (await client
      .missionList({ cursor: null, limit: 50, archived: false })
      .catch((err: unknown) => err)) as RpcClientError;
    expect(bare.details).toBeUndefined();
  });
});

describe("transport fallback", () => {
  it("isTauri is false in node (no window) and true with tauri internals", () => {
    expect(isTauri()).toBe(false);
    vi.stubGlobal("window", { __TAURI_INTERNALS__: {} });
    try {
      expect(isTauri()).toBe(true);
    } finally {
      vi.unstubAllGlobals();
    }
    vi.stubGlobal("window", { location: {} });
    try {
      expect(isTauri()).toBe(false);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("createDaemonClient picks mock outside tauri and real inside", () => {
    expect(createDaemonClient(new FakeIpc(), () => false)).toBeInstanceOf(MockDaemonClient);
    expect(createDaemonClient(new FakeIpc(), () => true)).toBeInstanceOf(RealDaemonClient);
  });
});

describe("system probe", () => {
  it("mock is deterministic and defensive-copied", async () => {
    const probe = mockSystemProbe();
    const a = await probe.listClis();
    const b = await probe.listClis();
    expect(a).toEqual(b);
    expect(a).not.toBe(b);
    a.pop();
    expect((await probe.listClis()).length).toBe(3);

    expect(await probe.queryVersion("C:/x/codex.exe")).toBe("0.20.0");
    expect(await probe.queryVersion("/usr/bin/claude")).toBe("1.0.32");
    expect(await probe.queryVersion("opencode")).toBe("0.9.6");
    expect(await probe.queryVersion("C:/x/some-custom-tool.exe")).toBeNull();
  });

  it("tauri probe invokes the bridge commands with a fixed version arg", async () => {
    const ipc = new FakeIpc();
    ipc.handle("system_list_clis", async () => [
      { program: "C:/x/codex.exe", kind: "codex", resolvedTarget: null, installForm: "native" },
    ]);
    ipc.handle("system_query_version", async (args) => {
      const { program, arg } = args as { program: string; arg: string };
      return program.includes("codex") && arg === "--version" ? "0.20.0" : null;
    });
    const probe = tauriSystemProbe(ipc);

    const candidates = await probe.listClis();
    expect((candidates[0] as CliCandidate).kind).toBe("codex");

    expect(await probe.queryVersion("C:/x/codex.exe")).toBe("0.20.0");
    // Unknown binaries never trigger a version query at all.
    expect(await probe.queryVersion("C:/x/custom.exe")).toBeNull();
    const versionCalls = ipc.calls.filter((call) => call.command === "system_query_version");
    expect(versionCalls).toHaveLength(1);
    expect(versionCalls[0].args).toEqual({ program: "C:/x/codex.exe", arg: "--version" });
  });
});

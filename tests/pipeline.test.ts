import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AttachParams } from "../src/generated/AttachParams";
import type { AttachResult } from "../src/generated/AttachResult";
import type { InputParams } from "../src/generated/InputParams";
import type { ResizeParams } from "../src/generated/ResizeParams";
import type { SessionAck } from "../src/generated/SessionAck";
import { base64ToBytes, bytesToBase64 } from "../src/features/daemon/base64";
import { InputQueue, SessionPipeline, type PipelineTerminal } from "../src/features/terminal/pipeline";

class FakeTerm implements PipelineTerminal {
  order: string[] = [];
  pendingWrites: Array<{ data: Uint8Array; cb?: () => void }> = [];
  resizes: Array<{ cols: number; rows: number }> = [];
  dataCb: ((data: string) => void) | null = null;

  write(data: Uint8Array, callback?: () => void): void {
    this.pendingWrites.push({ data, cb: callback });
    this.order.push(`write:${new TextDecoder().decode(data)}`);
  }
  resize(cols: number, rows: number): void {
    this.resizes.push({ cols, rows });
    this.order.push(`resize:${cols}x${rows}`);
  }
  onData(callback: (data: string) => void): { dispose(): void } {
    this.dataCb = callback;
    return { dispose: () => (this.dataCb = null) };
  }
  emit(data: string): void {
    this.dataCb?.(data);
  }
  /** Complete all pending write callbacks (simulates xterm consumption). */
  consume(): void {
    const writes = this.pendingWrites;
    this.pendingWrites = [];
    for (const w of writes) w.cb?.();
  }
  /** Consume until the serialized pump chain quiesces. */
  drain(): void {
    let guard = 0;
    while (this.pendingWrites.length > 0 && guard++ < 1000) this.consume();
  }
}

class FakeClient {
  attachResults: AttachResult[] = [];
  inputs: InputParams[] = [];
  resizes: ResizeParams[] = [];
  acks: SessionAck[] = [];
  attachCalls = 0;

  async sessionAttach(_params: AttachParams): Promise<AttachResult> {
    this.attachCalls++;
    const result = this.attachResults.shift();
    if (!result) throw new Error("no scripted attach result");
    return result;
  }
  async sessionInput(params: InputParams): Promise<{ input_id: string; accepted_bytes: number }> {
    this.inputs.push(params);
    return { input_id: params.input_id, accepted_bytes: base64ToBytes(params.data_b64).length };
  }
  async sessionResize(params: ResizeParams): Promise<{ resize_id: string; applied_seq: string }> {
    this.resizes.push(params);
    return { resize_id: params.resize_id, applied_seq: "1" };
  }
  sessionAck(ack: SessionAck): void {
    this.acks.push(ack);
  }
}

function outputEvent(sessionId: string, epoch: string, seq: number, text: string) {
  const bytes = new TextEncoder().encode(text);
  return {
    session_id: sessionId,
    epoch,
    seq: String(seq),
    kind: "output" as const,
    data_b64: bytesToBase64(bytes),
    raw_len: bytes.length,
  };
}

function resizeEvent(sessionId: string, epoch: string, seq: number, cols: number, rows: number) {
  return {
    session_id: sessionId,
    epoch,
    seq: String(seq),
    kind: "resize" as const,
    data_b64: "",
    raw_len: 0,
    cols,
    rows,
  };
}

let counter = 0;
const uuid = () => `id-${++counter}`;

describe("SessionPipeline", () => {
  it("shares overlapping attaches without losing output received before the reply", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    let reply!: (value: AttachResult) => void;
    client.sessionAttach = () => {
      client.attachCalls++;
      return new Promise(resolve => { reply = resolve; });
    };
    const pipeline = new SessionPipeline({ client, sessionId: "s1", viewId: "v1", terminal: term, uuid });
    const first = pipeline.attach();
    const second = pipeline.attach(true);
    expect(client.attachCalls).toBe(1);
    pipeline.handleOutput(outputEvent("s1", "e1", 1, "restored"));
    reply({ epoch: "e1", replay_from_seq: "1", last_seq: "1", cols: 80, rows: 24 });
    await Promise.all([first, second]);
    term.drain();
    expect(term.order).toEqual(["write:restored"]);
    expect(pipeline.currentMode).toBe("live");
    pipeline.dispose();
  });

  it("does not revive a disposed pane when its attach reply arrives", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    let reply!: (value: AttachResult) => void;
    client.sessionAttach = () => new Promise(resolve => { reply = resolve; });
    const onModeChange = vi.fn();
    const pipeline = new SessionPipeline(
      { client, sessionId: "s1", viewId: "v1", terminal: term, uuid },
      { onModeChange },
    );
    const attaching = pipeline.attach();
    pipeline.dispose();
    reply({ epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 });
    await attaching;
    expect(onModeChange).not.toHaveBeenCalled();
    expect(pipeline.sendInput("ignored")).toBe(false);
    expect(term.order).toEqual([]);
  });

  it("sends fits during replay right away (5e37e02) while the grid follows journal size records", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "2", cols: 80, rows: 24 });
    const pipeline = new SessionPipeline({ client, sessionId: "s1", viewId: "v1", terminal: term, uuid });
    pipeline.requestResize(90, 30);
    await pipeline.attach();
    pipeline.handleOutput(resizeEvent("s1", "e1", 1, 80, 24));
    pipeline.requestResize(100, 40);
    pipeline.handleOutput(outputEvent("s1", "e1", 2, "restored"));
    // 새 계약: 재생 중에도 최신 fit은 곧바로 데몬에 간다 — attach 직후 첫
    // fit(90x30), 재생 중 다음 fit(100x40). 격자는 저널 기록(80x24)을 따른다.
    expect(client.resizes.map(({ cols, rows }) => ({ cols, rows }))).toEqual([
      { cols: 90, rows: 30 },
      { cols: 100, rows: 40 },
    ]);
    term.drain();
    expect(term.order).toEqual(["resize:80x24", "write:restored"]);
    expect(pipeline.currentMode).toBe("live");
    pipeline.dispose();
  });

  it("keeps an exit received during attach when an older reply says the process was live", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    let reply!: (value: AttachResult) => void;
    client.sessionAttach = () => new Promise(resolve => { reply = resolve; });
    const pipeline = new SessionPipeline({ client, sessionId: "s1", viewId: "v1", terminal: term, uuid });
    const attaching = pipeline.attach();
    pipeline.handleExit({ session_id: "s1", exit_code: 1, reason: "process_exit", descendants_remaining: false });
    reply({ epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24, exited: false });
    await attaching;
    expect(pipeline.currentMode).toBe("exited");
    expect(pipeline.sendInput("ignored")).toBe(false);
    pipeline.dispose();
  });

  it("replays a finished journal without sending input or resize to its dead process", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "2", cols: 80, rows: 24, exited: true });
    const pipeline = new SessionPipeline({ client, sessionId: "s1", viewId: "v1", terminal: term, uuid });
    await pipeline.attach();
    pipeline.requestResize(100, 30);
    pipeline.handleOutput(resizeEvent("s1", "e1", 1, 80, 24));
    pipeline.handleOutput(outputEvent("s1", "e1", 2, "previous terminal output"));
    expect(pipeline.currentMode).toBe("replay");
    term.drain();
    expect(pipeline.currentMode).toBe("exited");
    expect(term.order).toContain("write:previous terminal output");
    expect(term.resizes.at(-1)).toEqual({ cols: 100, rows: 30 });
    term.emit("ignored");
    expect(pipeline.sendInput("ignored")).toBe(false);
    expect(await pipeline.sendLargeInput("ignored")).toBe(false);
    expect(client.inputs).toEqual([]);
    expect(client.resizes).toEqual([]);
    pipeline.dispose();
  });

  it("ends a replay the caller knows is finished in exited when the attach reply omits `exited`", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    // A daemon built before the field: a finished session attaches without `exited`.
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "1", cols: 80, rows: 24 });
    const modes: string[] = [];
    const pipeline = new SessionPipeline(
      { client, sessionId: "s1", viewId: "v1", terminal: term, uuid },
      { onModeChange: (mode) => modes.push(mode) },
    );
    pipeline.markExited();
    await pipeline.attach();
    pipeline.handleOutput(outputEvent("s1", "e1", 1, "previous terminal output"));
    term.drain();
    expect(pipeline.currentMode).toBe("exited");
    expect(modes).toEqual(["replay", "exited"]);
    term.emit("ignored");
    expect(pipeline.sendInput("ignored")).toBe(false);
    expect(client.inputs).toEqual([]);
    pipeline.dispose();
  });

  it("stops treating a live session as writable once told its process finished", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 });
    const modes: string[] = [];
    const pipeline = new SessionPipeline(
      { client, sessionId: "s1", viewId: "v1", terminal: term, uuid },
      { onModeChange: (mode) => modes.push(mode) },
    );
    await pipeline.attach();
    expect(pipeline.currentMode).toBe("live");
    pipeline.markExited();
    pipeline.markExited();
    expect(pipeline.currentMode).toBe("exited");
    expect(modes).toEqual(["replay", "live", "exited"]);
    expect(pipeline.sendInput("ignored")).toBe(false);
    expect(client.inputs).toEqual([]);
    pipeline.dispose();
  });

  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("preserves the screen on failed attach and resets before successful replay", async () => {
    const term = new FakeTerm();
    const reset = vi.fn(() => term.order.push("reset"));
    const client = new FakeClient();
    const pipeline = new SessionPipeline({
      client, sessionId: "s1", viewId: "v1", terminal: Object.assign(term, { reset }), uuid,
    });
    await expect(pipeline.attach(true)).rejects.toThrow("no scripted attach result");
    expect(reset).not.toHaveBeenCalled();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "1", cols: 80, rows: 24 });
    await pipeline.attach(true);
    pipeline.handleOutput(outputEvent("s1", "e1", 1, "saved output"));
    expect(term.order).toEqual(["reset", "write:saved output"]);
    term.drain();
    pipeline.dispose();
  });

  it("waits for in-flight writes to finish parsing before reset on reattach (no stale-epoch repaint)", async () => {
    const term = new FakeTerm();
    const reset = vi.fn(() => term.order.push("reset"));
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 });
    const pipeline = new SessionPipeline({
      client: client as never, sessionId: "s1", viewId: "v1",
      terminal: Object.assign(term, { reset }), uuid,
    });
    await pipeline.attach();
    expect(pipeline.currentMode).toBe("live");

    // Live write sits in xterm//s queue; its parse callback has not run yet.
    pipeline.handleOutput(outputEvent("s1", "e1", 1, "frame"));
    expect(term.order).toEqual(["write:frame"]);

    // Re-attach while that write is still parsing: reset must wait for it.
    client.attachResults.push({ epoch: "e2", replay_from_seq: "1", last_seq: "1", cols: 80, rows: 24 });
    const reattach = pipeline.attach(true);
    for (let i = 0; i < 10; i++) await Promise.resolve();
    // Still parsing → reset has not run. Resetting now would let the stale
    // bytes repaint into the fresh screen (verbatim duplication on screen).
    expect(term.order).toEqual(["write:frame"]);
    term.consume(); // parse completes → drain resolves → attach proceeds
    await reattach;
    pipeline.handleOutput(outputEvent("s1", "e2", 1, "fresh"));
    term.drain();
    expect(term.order).toEqual(["write:frame", "reset", "write:fresh"]);
    pipeline.dispose();
  });

  it("applies output and resize records in strict journal order", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "3", cols: 80, rows: 24 });
    const pipeline = new SessionPipeline(
      { client: client as never, sessionId: "s1", viewId: "v1", terminal: term, uuid },
    );
    await pipeline.attach();
    expect(pipeline.currentMode).toBe("replay");

    // Deliver out of order — seq 3 before 2 must be buffered.
    pipeline.handleOutput(outputEvent("s1", "e1", 1, "A"));
    pipeline.handleOutput(outputEvent("s1", "e1", 3, "C"));
    pipeline.handleOutput(resizeEvent("s1", "e1", 2, 100, 30));

    // Nothing consumed yet: write is async (callback pending), resize waits.
    expect(term.order).toEqual(["write:A"]);
    term.drain();
    // resize applied strictly after write A completed; write C issued after that.
    expect(term.order).toEqual(["write:A", "resize:100x30", "write:C"]);
    expect(pipeline.ackedThrough).toBe(3);
    expect(pipeline.currentMode).toBe("live");
  });

  it("coalesces a ConPTY frame end and its delayed cursor restore before rendering or ACK", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 40 });
    const pipeline = new SessionPipeline({
      client, sessionId: "s1", viewId: "v1", terminal: term, uuid, outputCoalesceMs: 32,
    });
    await pipeline.attach();
    // Control sequence ordering observed in Windows Codex output: the content
    // cursor is still active at DECRST 2026, then ConPTY restores the input row.
    const frame = "\x1b[?2026h\x1b[?25l\x1b[31;1HWorking\x1b[?25h\x1b[?2026l";
    const restore = "\x1b[?25l\x1b[35;3H\x1b[?25h";
    pipeline.handleOutput(outputEvent("s1", "e1", 1, frame));
    vi.advanceTimersByTime(16);
    pipeline.handleOutput(outputEvent("s1", "e1", 2, restore));
    expect(term.order).toEqual([]);
    expect(pipeline.ackedThrough).toBe(0);
    vi.advanceTimersByTime(16);
    expect(term.order).toEqual([`write:${frame}${restore}`]);
    expect(client.acks).toEqual([]);
    term.consume();
    vi.advanceTimersByTime(16);
    expect(client.acks.at(-1)?.through_seq).toBe("2");
    pipeline.dispose();
  });

  it("drains a live backlog in bounded writes and ACKs only consumed bytes", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 });
    const pipeline = new SessionPipeline({ client, sessionId: "s1", viewId: "v1", terminal: term, uuid });
    await pipeline.attach();
    for (let seq = 1; seq <= 130; seq++) {
      pipeline.handleOutput(outputEvent("s1", "e1", seq, String(seq % 10).repeat(1024)));
    }
    expect(term.pendingWrites.map((write) => write.data.length)).toEqual([1024]);
    expect(pipeline.ackedThrough).toBe(0);
    term.consume();
    expect(pipeline.ackedThrough).toBe(1);
    expect(term.pendingWrites.map((write) => write.data.length)).toEqual([65536]);
    term.consume();
    expect(pipeline.ackedThrough).toBe(65);
    expect(term.pendingWrites.map((write) => write.data.length)).toEqual([65536]);
    term.consume();
    expect(pipeline.ackedThrough).toBe(129);
    expect(term.pendingWrites.map((write) => write.data.length)).toEqual([1024]);
    term.consume();
    vi.advanceTimersByTime(16);
    expect(client.acks.at(-1)?.through_seq).toBe("130");
    expect(term.order.map((write) => write.slice(6)).join("")).toBe(
      Array.from({ length: 130 }, (_, index) => String((index + 1) % 10).repeat(1024)).join(""),
    );
    pipeline.dispose();
  });

  it("stops a live backlog at each resize barrier", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 });
    const pipeline = new SessionPipeline({ client, sessionId: "s1", viewId: "v1", terminal: term, uuid });
    await pipeline.attach();
    for (const [index, text] of ["A", "B", "C"].entries()) {
      pipeline.handleOutput(outputEvent("s1", "e1", index + 1, text));
    }
    pipeline.handleOutput(resizeEvent("s1", "e1", 4, 90, 30));
    pipeline.handleOutput(outputEvent("s1", "e1", 5, "D"));
    pipeline.handleOutput(outputEvent("s1", "e1", 6, "E"));
    term.consume();
    expect(term.order).toEqual(["write:A", "write:BC"]);
    expect(pipeline.ackedThrough).toBe(1);
    term.consume();
    expect(term.order).toEqual(["write:A", "write:BC", "resize:90x30"]);
    expect(pipeline.ackedThrough).toBe(4);
    vi.advanceTimersByTime(16);
    expect(term.order.at(-1)).toBe("write:DE");
    term.consume();
    expect(pipeline.ackedThrough).toBe(6);
    pipeline.dispose();
  });

  it("presents a resize redraw together after its chunks settle, then resumes immediate output", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 });
    const pipeline = new SessionPipeline({ client, sessionId: "s1", viewId: "v1", terminal: term, uuid });
    await pipeline.attach();
    pipeline.handleOutput(resizeEvent("s1", "e1", 1, 90, 30));
    const chunks = ["\x1b[2J\x1b[H", "history\r\n", "prompt"];
    for (const [index, chunk] of chunks.entries()) {
      pipeline.handleOutput(outputEvent("s1", "e1", index + 2, chunk));
      vi.advanceTimersByTime(10);
    }
    expect(term.order).toEqual(["resize:90x30"]);
    expect(pipeline.ackedThrough).toBe(1);
    vi.advanceTimersByTime(6);
    expect(term.order).toEqual(["resize:90x30", `write:${chunks.join("")}`]);
    term.consume();
    expect(pipeline.ackedThrough).toBe(4);
    pipeline.handleOutput(outputEvent("s1", "e1", 5, "typed"));
    expect(term.order.at(-1)).toBe("write:typed");
    pipeline.dispose();
  });

  it("bounds a continuously arriving resize redraw to 100ms", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 });
    const pipeline = new SessionPipeline({ client, sessionId: "s1", viewId: "v1", terminal: term, uuid });
    await pipeline.attach();
    pipeline.handleOutput(resizeEvent("s1", "e1", 1, 90, 30));
    for (let seq = 2; seq <= 11; seq++) {
      pipeline.handleOutput(outputEvent("s1", "e1", seq, "x"));
      vi.advanceTimersByTime(10);
    }
    expect(term.order).toEqual(["resize:90x30", `write:${"x".repeat(10)}`]);
    term.consume();
    expect(pipeline.ackedThrough).toBe(11);
    pipeline.dispose();
  });

  it("flushes the redraw before another resize and cancels stale redraw timers on reattach", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push(
      { epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 },
      { epoch: "e2", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 },
    );
    const pipeline = new SessionPipeline({ client, sessionId: "s1", viewId: "v1", terminal: term, uuid });
    await pipeline.attach();
    pipeline.handleOutput(resizeEvent("s1", "e1", 1, 90, 30));
    pipeline.handleOutput(outputEvent("s1", "e1", 2, "A"));
    pipeline.handleOutput(resizeEvent("s1", "e1", 3, 100, 40));
    expect(term.order).toEqual(["resize:90x30", "write:A"]);
    term.consume();
    // 버스트 창(60ms)이 닫혀야 마지막 resize가 적용된다(간격 재판정 참조).
    vi.advanceTimersByTime(61);
    expect(term.order.at(-1)).toBe("resize:100x40");
    pipeline.handleOutput(outputEvent("s1", "e1", 4, "stale"));
    await pipeline.attach();
    pipeline.handleOutput(outputEvent("s1", "e2", 1, "fresh"));
    expect(term.order.at(-1)).toBe("write:fresh");
    vi.advanceTimersByTime(150);
    expect(term.order).not.toContain("write:stale");
    pipeline.dispose();
  });

  it("keeps replay immediate and preserves resize barriers in coalesced live output", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "1", cols: 80, rows: 24 });
    const pipeline = new SessionPipeline({
      client, sessionId: "s1", viewId: "v1", terminal: term, uuid, outputCoalesceMs: 32,
    });
    await pipeline.attach();
    pipeline.handleOutput(outputEvent("s1", "e1", 1, "replay"));
    expect(term.order).toEqual(["write:replay"]);
    term.consume();
    pipeline.handleOutput(outputEvent("s1", "e1", 2, "A"));
    pipeline.handleOutput(resizeEvent("s1", "e1", 3, 100, 30));
    pipeline.handleOutput(outputEvent("s1", "e1", 4, "B"));
    vi.advanceTimersByTime(32);
    expect(term.order).toEqual(["write:replay", "write:A"]);
    term.consume();
    expect(term.order.at(-1)).toBe("resize:100x30");
    vi.advanceTimersByTime(32);
    expect(term.order.at(-1)).toBe("write:B");
    term.consume();
    expect(pipeline.ackedThrough).toBe(4);
    pipeline.dispose();
  });

  it("cancels buffered output on reattach and disposal", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push(
      { epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 },
      { epoch: "e2", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 },
    );
    const pipeline = new SessionPipeline({
      client, sessionId: "s1", viewId: "v1", terminal: term, uuid, outputCoalesceMs: 32,
    });
    await pipeline.attach();
    pipeline.handleOutput(outputEvent("s1", "e1", 1, "stale"));
    await pipeline.attach();
    vi.advanceTimersByTime(32);
    expect(term.order).toEqual([]);
    pipeline.handleOutput(outputEvent("s1", "e2", 1, "disposed"));
    pipeline.dispose();
    vi.advanceTimersByTime(32);
    expect(term.order).toEqual([]);
    expect(client.acks).toEqual([]);
  });

  it("counts only fresh live output as activity, excluding replay, resize and duplicates", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "1", cols: 80, rows: 24 });
    const onLiveOutput = vi.fn();
    const pipeline = new SessionPipeline({ client: client as never, sessionId: "s1", viewId: "v1", terminal: term, uuid }, { onLiveOutput });
    await pipeline.attach();
    pipeline.handleOutput(outputEvent("s1", "e1", 1, "old screen"));
    term.drain();
    pipeline.handleOutput(resizeEvent("s1", "e1", 2, 90, 30));
    expect(onLiveOutput).not.toHaveBeenCalled();
    pipeline.handleOutput(outputEvent("s1", "e1", 3, "new output"));
    pipeline.handleOutput(outputEvent("s1", "e1", 3, "new output"));
    expect(onLiveOutput).toHaveBeenCalledOnce();
    pipeline.dispose();
  });

  it("blocks terminal data during replay; live unlocks input (02 §5)", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "1", cols: 80, rows: 24 });
    const blocked: number[] = [];
    const pipeline = new SessionPipeline(
      { client: client as never, sessionId: "s1", viewId: "v1", terminal: term, uuid },
      { onInputBlocked: () => blocked.push(1) },
    );
    await pipeline.attach();
    pipeline.handleOutput(outputEvent("s1", "e1", 1, "boot"));

    // User keystroke AND terminal-generated query response during replay:
    term.emit("x");
    term.emit("\x1b[?1;2c"); // DA response xterm would emit mid-replay
    expect(client.inputs.length).toBe(0);
    expect(blocked.length).toBe(2);

    term.drain(); // replay done → live
    expect(pipeline.currentMode).toBe("live");
    term.emit("ls\r");
    expect(client.inputs.length).toBe(1);
    expect(new TextDecoder().decode(base64ToBytes(client.inputs[0].data_b64))).toBe("ls\r");
  });

  it("ACKs batch by 16ms window with contiguous through_seq", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "5", cols: 80, rows: 24 });
    const pipeline = new SessionPipeline(
      { client: client as never, sessionId: "s1", viewId: "v1", terminal: term, uuid },
    );
    await pipeline.attach();
    for (let i = 1; i <= 5; i++) pipeline.handleOutput(outputEvent("s1", "e1", i, "x"));
    term.drain(); // all consumed, 5 bytes pending
    expect(client.acks.length).toBe(0); // 아직 배치 창 안에서 대기
    vi.advanceTimersByTime(16);
    expect(client.acks.length).toBe(1);
    expect(client.acks[0].through_seq).toBe("5");
    expect(client.acks[0].epoch).toBe("e1");
  });

  it("flushes ACK immediately once 64 KiB is pending", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "5", cols: 80, rows: 24 });
    const pipeline = new SessionPipeline(
      { client: client as never, sessionId: "s1", viewId: "v1", terminal: term, uuid },
    );
    await pipeline.attach();
    const chunk = "a".repeat(16384);
    for (let i = 1; i <= 4; i++) pipeline.handleOutput(outputEvent("s1", "e1", i, chunk));
    term.drain();
    expect(client.acks.length).toBe(1); // 64 KiB 도달 → 즉시 flush
    expect(client.acks[0].through_seq).toBe("4");
  });

  it("rotates epochs: stale-epoch records are dropped, replay restarts", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "1", cols: 80, rows: 24 });
    const pipeline = new SessionPipeline(
      { client: client as never, sessionId: "s1", viewId: "v1", terminal: term, uuid },
    );
    await pipeline.attach();
    pipeline.handleOutput(outputEvent("s1", "e1", 1, "old"));
    term.consume();
    expect(pipeline.currentEpoch).toBe("e1");

    // Re-attach: fresh epoch, replay from 1 again.
    client.attachResults.push({ epoch: "e2", replay_from_seq: "1", last_seq: "2", cols: 80, rows: 24 });
    await pipeline.attach();
    expect(pipeline.currentEpoch).toBe("e2");
    expect(pipeline.currentMode).toBe("replay");
    // Old-epoch event must be ignored.
    pipeline.handleOutput(outputEvent("s1", "e1", 2, "stale"));
    expect(term.order.filter((o) => o === "write:stale")).toHaveLength(0);
    pipeline.handleOutput(outputEvent("s1", "e2", 1, "fresh"));
    pipeline.handleOutput(outputEvent("s1", "e2", 2, "again"));
    term.consume();
    expect(term.order).toEqual(["write:old", "write:fresh", "write:again"]);
  });

  it("sends the first resize immediately and retains only the latest size while the daemon is busy", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 });
    let complete!: () => void;
    vi.spyOn(client, "sessionResize").mockImplementation((params) => {
      client.resizes.push(params);
      return new Promise((resolve) => { complete = () => resolve({ resize_id: params.resize_id, applied_seq: "1" }); });
    });
    const pipeline = new SessionPipeline({ client: client as never, sessionId: "s1", viewId: "v1", terminal: term, uuid });
    await pipeline.attach();
    pipeline.requestResize(100, 30);
    expect(client.resizes).toHaveLength(1); // No additional frontend timer.
    for (let cols = 101; cols <= 140; cols++) {
      pipeline.requestResize(cols, 40);
      await vi.advanceTimersByTimeAsync(16);
    }
    pipeline.requestResize(0, 0);
    expect(client.resizes).toHaveLength(1);
    complete();
    await vi.advanceTimersByTimeAsync(0);
    expect(client.resizes).toHaveLength(2);
    expect(client.resizes[1]).toMatchObject({ cols: 140, rows: 40 });
    pipeline.requestResize(140, 40);
    complete();
    await vi.advanceTimersByTimeAsync(100);
    expect(client.resizes).toHaveLength(2);
    expect(term.resizes).toEqual([]); // Ordered records still control the grid.
    pipeline.dispose();
  });

  it("drops an obsolete pending size when the drag returns to the in-flight size", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 });
    const pipeline = new SessionPipeline({ client: client as never, sessionId: "s1", viewId: "v1", terminal: term, uuid });
    await pipeline.attach();
    pipeline.requestResize(100, 30);
    pipeline.requestResize(120, 40);
    pipeline.requestResize(100, 30);
    await vi.advanceTimersByTimeAsync(100);
    expect(client.resizes).toHaveLength(1);
    expect(client.resizes[0]).toMatchObject({ cols: 100, rows: 30 });
    pipeline.dispose();
  });

  it.each([false, true])("settles a failed resize without a retry loop (disposed: %s)", async (dispose) => {
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 });
    let fail!: () => void;
    const spy = vi.spyOn(client, "sessionResize").mockImplementationOnce(() => new Promise((_resolve, reject) => {
      fail = () => reject(new Error("resize failed"));
    }));
    const pipeline = new SessionPipeline({ client: client as never, sessionId: "s1", viewId: "v1", terminal: new FakeTerm(), uuid });
    await pipeline.attach();
    pipeline.requestResize(100, 30);
    if (dispose) {
      pipeline.requestResize(120, 40);
      pipeline.dispose();
    }
    fail();
    await vi.advanceTimersByTimeAsync(100);
    expect(spy).toHaveBeenCalledTimes(1);
    pipeline.requestResize(100, 30);
    await vi.advanceTimersByTimeAsync(0);
    expect(spy).toHaveBeenCalledTimes(dispose ? 1 : 2);
    pipeline.dispose();
  });

  it("applies the actual resize only when the ordered record arrives", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "2", cols: 80, rows: 24 });
    const pipeline = new SessionPipeline(
      { client: client as never, sessionId: "s1", viewId: "v1", terminal: term, uuid },
    );
    await pipeline.attach();
    pipeline.requestResize(120, 40);
    vi.advanceTimersByTime(16);
    expect(term.resizes).toEqual([]); // 아직 journal record가 오지 않았다
    pipeline.handleOutput(outputEvent("s1", "e1", 1, "x"));
    pipeline.handleOutput(resizeEvent("s1", "e1", 2, 120, 40));
    term.consume();
    expect(term.resizes).toEqual([{ cols: 120, rows: 40 }]);
  });
});

describe("InputQueue — 4 KiB chunks, one in-flight", () => {
  it("chunks large input into ≤4 KiB writes", async () => {
    const sent: Uint8Array[] = [];
    const queue = new InputQueue(async (bytes) => {
      sent.push(bytes);
    });
    queue.enqueue("x".repeat(9000));
    await vi.waitFor(() => expect(sent.length).toBe(3));
    expect(sent.map((b) => b.length)).toEqual([4096, 4096, 808]);
  });

  it("drops the queue on send error (no auto-retransmit)", async () => {
    let calls = 0;
    const queue = new InputQueue(async () => {
      calls++;
      if (calls === 1) throw new Error("INPUT_OUTCOME_UNKNOWN");
    });
    queue.enqueue("a");
    queue.enqueue("b");
    await vi.waitFor(() => expect(calls).toBe(1));
    expect(queue.pendingBytes).toBe(0);
  });

  it("rejects beyond the 64 KiB queue cap", () => {
    const queue = new InputQueue(async () => undefined);
    expect(() => queue.enqueue("y".repeat(70000))).toThrow();
    queue.dispose();
  });

  it("passes a 1 MiB paste through enqueueLarge in 4 KiB chunks (W1-8)", async () => {
    const sent: Uint8Array[] = [];
    const queue = new InputQueue(async (bytes) => {
      sent.push(bytes);
      // 드레인은 세미그룹처럼 순차 진행 — 각 청크 전송은 다음 마이크로태스크.
    });
    const payload = "p".repeat(1048576);
    const done = queue.enqueueLarge(payload);
    await done;
    await vi.waitFor(() => expect(queue.pendingBytes).toBe(0));
    // 1 MiB = 4 KiB × 256 — 전체가 순서대로, 상한 위반 없이 흘러간다.
    expect(sent.length).toBe(256);
    expect(sent.every((b) => b.length === 4096)).toBe(true);
    expect(sent.reduce((sum, b) => sum + b.length, 0)).toBe(1048576);
    expect(queue.didOverflow).toBe(false);
    queue.dispose();
  });

  it("enqueueLarge waits for queue capacity instead of overflowing", async () => {
    const gates: Array<() => void> = [];
    const queue = new InputQueue(
      () =>
        new Promise<void>((resolve) => {
          // 첫 전송만 수동으로 막고, 이후 전송은 즉시 완료된다.
          if (gates.length === 0) gates.push(resolve);
          else resolve();
        }),
    );
    // 64 KiB를 채운다. 청크 1개는 즉시 전송(gate[0]에서 대기)으로 빠지고
    // 큐에는 15개(61440 B)가 남는다.
    const first = queue.enqueueLarge("z".repeat(65536));
    await vi.waitFor(() => expect(queue.pendingBytes).toBe(61440));
    expect(gates.length).toBe(1); // 전송 1개만 진행 중(동시 미완료 write 1개)

    // 두 번째 대량 입력: 청크 1개는 여유(61440+4096=65536)에 들어가고,
    // 그다음 청크는 큐가 가득 찬 상태라 수용량을 기다린다.
    const second = queue.enqueueLarge("z".repeat(8192));
    await vi.waitFor(() => expect(queue.pendingBytes).toBe(65536));
    expect(gates.length).toBe(1); // capacity 대기 중 — 새 전송이 시작되지 않는다

    gates[0](); // 첫 전송 완료 → 큐에서 다음 청크 빠짐 → 대기자가 채워넣는다
    await Promise.all([first, second]);
    await vi.waitFor(() => expect(queue.pendingBytes).toBe(0));
    expect(queue.didOverflow).toBe(false);
    queue.dispose();
  });
});

describe("SessionPipeline — attach edge cases", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("goes live right after attaching to an empty journal (nothing to replay, input must not stay blocked)", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 });
    const pipeline = new SessionPipeline(
      { client: client as never, sessionId: "s1", viewId: "v1", terminal: term, uuid },
    );
    await pipeline.attach();
    expect(pipeline.currentMode).toBe("live");
    term.emit("ls\r");
    expect(client.inputs.length).toBe(1);
    pipeline.dispose();
  });

  it("treats records before replay_from_seq as consumed so ACK and live follow a trimmed journal", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "5", last_seq: "6", cols: 80, rows: 24 });
    const pipeline = new SessionPipeline(
      { client: client as never, sessionId: "s1", viewId: "v1", terminal: term, uuid },
    );
    await pipeline.attach();
    expect(pipeline.currentMode).toBe("replay");
    pipeline.handleOutput(outputEvent("s1", "e1", 5, "E"));
    pipeline.handleOutput(outputEvent("s1", "e1", 6, "F"));
    term.drain();
    expect(term.order).toEqual(["write:E", "write:F"]);
    expect(pipeline.ackedThrough).toBe(6);
    expect(pipeline.currentMode).toBe("live");
    pipeline.dispose();
  });

  it("consumes an in-flight write before a re-attach rotates the epoch (no stale repaint)", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "1", cols: 80, rows: 24 });
    const pipeline = new SessionPipeline(
      { client: client as never, sessionId: "s1", viewId: "v1", terminal: term, uuid },
    );
    await pipeline.attach();
    pipeline.handleOutput(outputEvent("s1", "e1", 1, "old"));
    const stale = term.pendingWrites.shift(); // xterm still holds the old write

    client.attachResults.push({ epoch: "e2", replay_from_seq: "1", last_seq: "2", cols: 80, rows: 24 });
    const reattach = pipeline.attach();
    for (let i = 0; i < 25; i++) await Promise.resolve();
    // RPC는 답했지만 옛 write를 파싱 중엔 epoch를 돌리지 않는다 — 먼저 reset하면
    // 낡은 바이트가 새 화면에 다시 찍힌다(화면 중복).
    expect(pipeline.currentEpoch).toBe("e1");
    stale?.cb?.(); // 파싱 완료가 attach 대기 중 도착 — 옛 epoch가 소비한다
    await reattach;
    expect(pipeline.currentEpoch).toBe("e2");
    expect(pipeline.currentMode).toBe("replay");

    pipeline.handleOutput(outputEvent("s1", "e2", 1, "fresh"));
    // seq 2는 "fresh"가 소비된 뒤에만 나간다.
    pipeline.handleOutput(outputEvent("s1", "e2", 2, "again"));
    expect(term.order).toEqual(["write:old", "write:fresh"]);
    term.drain();
    expect(term.order).toEqual(["write:old", "write:fresh", "write:again"]);
    expect(pipeline.ackedThrough).toBe(2);
    expect(pipeline.currentMode).toBe("live");
    pipeline.dispose();
  });

  it("drops a non-finite resize proposal (a detached host makes FitAddon report NaN)", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 });
    const pipeline = new SessionPipeline(
      { client: client as never, sessionId: "s1", viewId: "v1", terminal: term, uuid },
    );
    await pipeline.attach();
    pipeline.requestResize(Number.NaN, Number.NaN);
    expect(client.resizes).toHaveLength(0);
    pipeline.requestResize(100, 30);
    expect(client.resizes).toHaveLength(1);
    pipeline.dispose();
  });
});

describe("InputQueue.fitsNow", () => {
  it("judges the whole input, not just its first chunk, so a mid-size paste never loses its tail", () => {
    const queue = new InputQueue(() => new Promise<void>(() => undefined)); // 전송이 끝나지 않는다
    queue.enqueue("a".repeat(60000)); // 청크 1개(4096)는 전송 중, 55904 B가 큐에 남는다
    expect(queue.pendingBytes).toBe(55904);
    expect(queue.fitsNow("b".repeat(4096))).toBe(true);
    expect(queue.fitsNow("b".repeat(20000))).toBe(false); // 55904 + 20000 > 64 KiB → 대용량 경로
    queue.dispose();
  });
});

describe("InputQueue — 연결 예산 재시도(PENDING_BUDGET)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  const budgetError = (retryAfterMs = 10) => ({
    code: "BUSY",
    message: "too many pending session requests",
    details: { reason_code: "PENDING_BUDGET", retry_after_ms: retryAfterMs },
  });

  it("예산 거절은 청크를 버리지 않고 간격 뒤 같은 내용으로 다시 보낸다", async () => {
    const sent: string[] = [];
    let fail = true;
    const onError = vi.fn();
    const onDelivered = vi.fn();
    const queue = new InputQueue(
      (bytes) => {
        sent.push(new TextDecoder().decode(bytes));
        if (fail) {
          fail = false;
          return Promise.reject(budgetError());
        }
        return Promise.resolve();
      },
      onError,
      onDelivered,
    );
    queue.enqueue("ab");
    expect(sent).toEqual(["ab"]);
    await vi.advanceTimersByTimeAsync(0); // 거절 continuation이 큐에 청크를 돌려놓는다.
    expect(queue.pendingBytes).toBe(2); // 재시도 대기 중 — 버리지 않았다.
    expect(onError).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(10);
    expect(sent).toEqual(["ab", "ab"]);
    expect(onDelivered).toHaveBeenCalledTimes(1);
    expect(queue.pendingBytes).toBe(0);
    queue.dispose();
  });

  it("재시도 상한을 넘기면 기존대로 남은 입력을 버리고 알린다", async () => {
    const sent: string[] = [];
    const error = budgetError();
    const onError = vi.fn();
    const queue = new InputQueue(
      (bytes) => {
        sent.push(new TextDecoder().decode(bytes));
        return Promise.reject(error);
      },
      onError,
    );
    queue.enqueue("first");
    queue.enqueue("second"); // 실패 시 함께 버려지는 뒤의 입력.
    await vi.advanceTimersByTimeAsync(10_000);
    // 첫 시도 1회 + 재시도 5회 = 같은 청크 6번, 그리고 포기.
    expect(sent.filter((s) => s === "first")).toHaveLength(6);
    expect(sent).not.toContain("second");
    expect(onError).toHaveBeenCalledTimes(1);
    expect(onError.mock.calls[0][0]).toBe(error);
    expect(queue.pendingBytes).toBe(0);
    queue.dispose();
  });

  it("PENDING_BUDGET이 아닌 BUSY(INPUT_STALLED)는 재시도하지 않는다", async () => {
    const sent: string[] = [];
    const onError = vi.fn();
    const queue = new InputQueue(
      (bytes) => {
        sent.push(new TextDecoder().decode(bytes));
        return Promise.reject({
          code: "BUSY",
          message: "not reading",
          details: { reason_code: "INPUT_STALLED" },
        });
      },
      onError,
    );
    queue.enqueue("x");
    await vi.advanceTimersByTimeAsync(10_000);
    expect(sent).toEqual(["x"]);
    expect(onError).toHaveBeenCalledTimes(1);
    queue.dispose();
  });

  it("재시도 대기 중 새 입력은 타이머를 앞당기지 않는다", async () => {
    const sent: string[] = [];
    let fail = true;
    const queue = new InputQueue((bytes) => {
      sent.push(new TextDecoder().decode(bytes));
      if (fail) {
        fail = false;
        return Promise.reject(budgetError(50));
      }
      return Promise.resolve();
    });
    queue.enqueue("first");
    await vi.advanceTimersByTimeAsync(0);
    queue.enqueue("later"); // 예산 재시도가 예약된 사이에 온 새 입력.
    await vi.advanceTimersByTimeAsync(49);
    expect(sent).toEqual(["first"]); // 간격(50 ms)을 채우기 전에는 재전송하지 않는다.
    await vi.advanceTimersByTimeAsync(1);
    expect(sent).toEqual(["first", "first", "later"]);
    expect(queue.pendingBytes).toBe(0);
    queue.dispose();
  });

  it("되돌아온 재시도 청크 때문에 수용량 대기자가 상한을 넘겨 채우지 않는다", async () => {
    const sent: string[] = [];
    let fail = true;
    const queue = new InputQueue((bytes) => {
      sent.push(new TextDecoder().decode(bytes));
      if (fail) {
        fail = false;
        return Promise.reject(budgetError(20));
      }
      return Promise.resolve();
    });
    // 큐를 상한 근처까지 채운다(15청크 = 61440 B). 첫 청크는 예산 거절 → 재시도 예약.
    queue.enqueue("a".repeat(61440));
    await vi.advanceTimersByTimeAsync(0);
    expect(queue.pendingBytes).toBe(61440);
    // 대용량 경로의 대기자 입장: 재시도로 되돌아올 바이트가 자리를 지키므로
    // 상한(65536)을 넘겨 밀어 넣지 못한다.
    expect(queue.fitsNow("b".repeat(8192))).toBe(false);
    expect(queue.fitsNow("b".repeat(4096))).toBe(true);
    await vi.advanceTimersByTimeAsync(20);
    // 재시도가 성공하면 나머지가 순서대로 흘러나간다.
    expect(sent.at(-1)).toBe("a".repeat(4096));
    expect(queue.pendingBytes).toBe(0);
    queue.dispose();
  });
});

describe("SessionPipeline — 롤링 저널의 잘린 헤드(02-runner §5)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("잘린 헤드부터 재생하면 알리고, live 전환 뒤 크기를 한 번 흔든다", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({
      epoch: "e1",
      replay_from_seq: "500",
      last_seq: "501",
      cols: 80,
      rows: 24,
      replay_dropped_bytes: "1048576",
    });
    const trimmed: Array<[number, number]> = [];
    const pipeline = new SessionPipeline(
      { client: client as never, sessionId: "s1", viewId: "v1", terminal: term, uuid },
      { onReplayTrimmed: (bytes, from) => trimmed.push([bytes, from]) },
    );
    pipeline.requestResize(80, 24); // fit before attach, as the registry does
    await pipeline.attach();
    expect(trimmed).toEqual([[1048576, 500]]);
    expect(pipeline.currentMode).toBe("replay");
    // 재생 중에도 attach 직전의 fit은 곧바로 나간다(즉시 fit 계약).
    await vi.advanceTimersByTimeAsync(0);
    expect(client.resizes.map((r) => `${r.cols}x${r.rows}`)).toEqual(["80x24"]);

    // The replay opens with the segment's size record, then the tail.
    pipeline.handleOutput(resizeEvent("s1", "e1", 500, 80, 24));
    pipeline.handleOutput(outputEvent("s1", "e1", 501, "tail"));
    term.drain();
    expect(pipeline.currentMode).toBe("live");
    expect(pipeline.ackedThrough).toBe(501);

    // Nudge: rows-1 right away, rows again after the interval — both as real
    // resize requests so the TUI repaints the whole screen.
    await vi.advanceTimersByTimeAsync(0);
    expect(client.resizes.map((r) => `${r.cols}x${r.rows}`)).toEqual(["80x24", "80x23"]);
    await vi.advanceTimersByTimeAsync(200);
    expect(client.resizes.map((r) => `${r.cols}x${r.rows}`)).toEqual(["80x24", "80x23", "80x24"]);
    pipeline.dispose();
  });

  it("에이전트 pane은 잘린 재생 뒤 화면을 지우고 폭을 흔든다", async () => {
    // Claude Code 2.1의 인라인 렌더러는 높이를 되돌리는 SIGWINCH에 아무것도
    // 내보내지 않고 화면 전체도 지우지 않는다 — 지운 화면에 폭 변경으로 다시 그린다.
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({
      epoch: "e1",
      replay_from_seq: "500",
      last_seq: "501",
      cols: 80,
      rows: 24,
      replay_dropped_bytes: "1048576",
    });
    const clearTerminal = vi.fn();
    const cleared: number[] = [];
    const pipeline = new SessionPipeline(
      {
        client: client as never,
        sessionId: "s1",
        viewId: "v1",
        terminal: term,
        uuid,
        clearTerminal,
        repaintTrimmedReplay: () => true,
      },
      { onCleared: (seq) => cleared.push(seq) },
    );
    pipeline.requestResize(80, 24);
    await pipeline.attach();
    await vi.advanceTimersByTimeAsync(0);
    pipeline.handleOutput(resizeEvent("s1", "e1", 500, 80, 24));
    pipeline.handleOutput(outputEvent("s1", "e1", 501, "tail"));
    term.drain();
    expect(pipeline.currentMode).toBe("live");
    // 재생 꼬리를 다 쓴 뒤에 지우고, 그 지점을 기억해 다음 재생도 같은 자리에서 지운다.
    expect(clearTerminal).toHaveBeenCalledTimes(1);
    expect(cleared).toEqual([501]);

    await vi.advanceTimersByTimeAsync(0);
    expect(client.resizes.map((r) => `${r.cols}x${r.rows}`)).toEqual(["80x24", "79x24"]);
    await vi.advanceTimersByTimeAsync(200);
    expect(client.resizes.map((r) => `${r.cols}x${r.rows}`)).toEqual(["80x24", "79x24", "80x24"]);
    pipeline.dispose();
  });

  it("잘리지 않은 attach는 알리지도 흔들지도 않는다", async () => {
    const term = new FakeTerm();
    const client = new FakeClient();
    client.attachResults.push({ epoch: "e1", replay_from_seq: "1", last_seq: "1", cols: 80, rows: 24 });
    const onReplayTrimmed = vi.fn();
    const pipeline = new SessionPipeline(
      { client: client as never, sessionId: "s1", viewId: "v1", terminal: term, uuid },
      { onReplayTrimmed },
    );
    pipeline.requestResize(80, 24);
    await pipeline.attach();
    pipeline.handleOutput(resizeEvent("s1", "e1", 1, 80, 24));
    term.drain();
    expect(pipeline.currentMode).toBe("live");
    await vi.advanceTimersByTimeAsync(300);
    expect(onReplayTrimmed).not.toHaveBeenCalled();
    expect(client.resizes.map((r) => `${r.cols}x${r.rows}`)).toEqual(["80x24"]);
    pipeline.dispose();
  });
});

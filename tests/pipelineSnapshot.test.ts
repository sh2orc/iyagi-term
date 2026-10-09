/**
 * 화면 스냅샷 복원(pipeline): 데몬이 `resume_from_seq`를 받아들였을 때만 xterm을
 * 비우고 스냅샷을 먼저 쓴 뒤 그 뒤 레코드를 잇는다. 받아들이지 않으면 전체 재생이다.
 * 재생 레코드는 묶어 쓰되 크기 레코드·지우기 지점에서 끊는다.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AttachParams } from "../src/generated/AttachParams";
import type { AttachResult } from "../src/generated/AttachResult";
import type { InputParams } from "../src/generated/InputParams";
import type { ResizeParams } from "../src/generated/ResizeParams";
import type { SessionAck } from "../src/generated/SessionAck";
import { bytesToBase64 } from "../src/features/daemon/base64";
import { SessionPipeline, type PipelineSnapshot, type PipelineTerminal } from "../src/features/terminal/pipeline";

class FakeTerm implements PipelineTerminal {
  order: string[] = [];
  pendingWrites: Array<() => void> = [];
  dataCb: ((data: string) => void) | null = null;

  reset(): void {
    this.order.push("reset");
  }
  write(data: Uint8Array, callback?: () => void): void {
    this.order.push(`write:${new TextDecoder().decode(data)}`);
    this.pendingWrites.push(() => callback?.());
  }
  resize(cols: number, rows: number): void {
    this.order.push(`resize:${cols}x${rows}`);
  }
  onData(callback: (data: string) => void): { dispose(): void } {
    this.dataCb = callback;
    return { dispose: () => (this.dataCb = null) };
  }
  clear(): void {
    this.order.push("clear");
  }
  consumeOne(): void {
    this.pendingWrites.shift()?.();
  }
  drain(): void {
    let guard = 0;
    while (this.pendingWrites.length > 0 && guard++ < 1000) this.consumeOne();
  }
}

class FakeClient {
  attachResults: AttachResult[] = [];
  attachParams: AttachParams[] = [];
  inputs: InputParams[] = [];
  acks: SessionAck[] = [];

  async sessionAttach(params: AttachParams): Promise<AttachResult> {
    this.attachParams.push(params);
    const result = this.attachResults.shift();
    if (!result) throw new Error("no scripted attach result");
    return result;
  }
  async sessionInput(params: InputParams): Promise<{ input_id: string; accepted_bytes: number }> {
    this.inputs.push(params);
    return { input_id: params.input_id, accepted_bytes: 0 };
  }
  async sessionResize(params: ResizeParams): Promise<{ resize_id: string; applied_seq: string }> {
    return { resize_id: params.resize_id, applied_seq: "1" };
  }
  sessionAck(ack: SessionAck): void {
    this.acks.push(ack);
  }
}

function output(epoch: string, seq: number, text: string) {
  const bytes = new TextEncoder().encode(text);
  return {
    session_id: "s1",
    epoch,
    seq: String(seq),
    kind: "output" as const,
    data_b64: bytesToBase64(bytes),
    raw_len: bytes.length,
  };
}

function resize(epoch: string, seq: number, cols: number, rows: number) {
  return { session_id: "s1", epoch, seq: String(seq), kind: "resize" as const, data_b64: "", raw_len: 0, cols, rows };
}

function attachResult(fromSeq: number, lastSeq: number, extra: Partial<AttachResult> = {}): AttachResult {
  return { epoch: "e1", replay_from_seq: String(fromSeq), last_seq: String(lastSeq), cols: 80, rows: 24, ...extra };
}

const SNAPSHOT: PipelineSnapshot = { seq: 3, cols: 100, rows: 30, data: "[screen]" };

function makePipeline(options: { clearAfterSeq?: number; onReplayTrimmed?: () => void } = {}) {
  const term = new FakeTerm();
  const client = new FakeClient();
  const pipeline = new SessionPipeline(
    {
      client: client as never,
      sessionId: "s1",
      viewId: "v1",
      terminal: term,
      uuid: () => "id",
      clearTerminal: () => term.clear(),
      clearAfterSeq: options.clearAfterSeq,
    },
    { onReplayTrimmed: options.onReplayTrimmed },
  );
  return { term, client, pipeline };
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("SessionPipeline screen snapshot", () => {
  it("asks for the records after the snapshot and writes the snapshot first when the daemon honors it", async () => {
    const { term, client, pipeline } = makePipeline();
    client.attachResults.push(attachResult(4, 5));
    await pipeline.attach(false, SNAPSHOT);
    expect(client.attachParams[0].resume_from_seq).toBe("4");
    // reset은 스냅샷 격자로 맞춘 뒤 첫 조각 쓰기 직전에 내린다(지연 초기화).
    expect(term.order).toEqual(["resize:100x30", "reset", "write:[screen]"]);

    pipeline.handleOutput(output("e1", 4, "D"));
    pipeline.handleOutput(output("e1", 5, "E"));
    // Records wait behind the snapshot write; input stays closed until it lands.
    expect(term.order).toHaveLength(3);
    expect(pipeline.currentMode).toBe("replay");
    term.dataCb?.("typed");
    expect(client.inputs).toHaveLength(0);

    term.drain();
    expect(term.order).toEqual(["resize:100x30", "reset", "write:[screen]", "write:DE"]);
    expect(pipeline.currentMode).toBe("live");
    vi.advanceTimersByTime(16);
    expect(client.acks.at(-1)?.through_seq).toBe("5");
    pipeline.dispose();
  });

  it("goes live only after the snapshot lands when nothing followed it", async () => {
    const { term, client, pipeline } = makePipeline();
    client.attachResults.push(attachResult(4, 3));
    await pipeline.attach(false, SNAPSHOT);
    expect(pipeline.currentMode).toBe("replay");
    expect(pipeline.snapshotSeq).toBeNull();
    term.drain();
    expect(pipeline.currentMode).toBe("live");
    expect(pipeline.snapshotSeq).toBe(3);
    pipeline.dispose();
  });

  it("discards the snapshot and replays everything when the daemon starts elsewhere", async () => {
    const { term, client, pipeline } = makePipeline();
    // An old daemon ignores resume_from_seq; replay starts at the head.
    client.attachResults.push(attachResult(1, 2));
    await pipeline.attach(false, SNAPSHOT);
    expect(term.order).toEqual([]);
    pipeline.handleOutput(output("e1", 1, "A"));
    pipeline.handleOutput(output("e1", 2, "B"));
    term.drain();
    expect(term.order).toEqual(["write:A", "write:B"]);
    expect(pipeline.currentMode).toBe("live");
    pipeline.dispose();
  });

  it("does not report a trimmed head when the snapshot covers it", async () => {
    const onReplayTrimmed = vi.fn();
    const { term, client, pipeline } = makePipeline({ onReplayTrimmed });
    client.attachResults.push(attachResult(4, 4, { replay_dropped_bytes: "1048576" }));
    await pipeline.attach(false, SNAPSHOT);
    pipeline.handleOutput(output("e1", 4, "D"));
    term.drain();
    expect(onReplayTrimmed).not.toHaveBeenCalled();
    expect(pipeline.currentMode).toBe("live");
    pipeline.dispose();
  });

  it("clears after the snapshot when the screen was cleared at its seq after it was taken", async () => {
    const { term, client, pipeline } = makePipeline({ clearAfterSeq: 3 });
    client.attachResults.push(attachResult(4, 4));
    await pipeline.attach(false, { ...SNAPSHOT, clearMark: 0 });
    pipeline.handleOutput(output("e1", 4, "D"));
    term.drain();
    expect(term.order).toEqual(["resize:100x30", "reset", "write:[screen]", "clear", "write:D"]);
    pipeline.dispose();
  });

  it("does not clear again when the snapshot already shows the cleared screen", async () => {
    const { term, client, pipeline } = makePipeline({ clearAfterSeq: 3 });
    client.attachResults.push(attachResult(4, 4));
    await pipeline.attach(false, { ...SNAPSHOT, clearMark: 3 });
    pipeline.handleOutput(output("e1", 4, "D"));
    term.drain();
    expect(term.order).toEqual(["resize:100x30", "reset", "write:[screen]", "write:D"]);
    pipeline.dispose();
  });

  it("offers a snapshot point only at an idle record boundary", async () => {
    const { term, client, pipeline } = makePipeline();
    client.attachResults.push(attachResult(1, 1));
    await pipeline.attach();
    pipeline.handleOutput(output("e1", 1, "A"));
    expect(pipeline.snapshotSeq).toBeNull(); // replaying, write in flight
    term.drain();
    expect(pipeline.snapshotSeq).toBe(1);
    pipeline.handleOutput(output("e1", 2, "B"));
    expect(pipeline.snapshotSeq).toBeNull(); // live write in flight
    pipeline.handleOutput(output("e1", 3, "C"));
    term.consumeOne();
    expect(pipeline.snapshotSeq).toBeNull(); // C still pending
    term.drain();
    expect(pipeline.snapshotSeq).toBe(3);
    pipeline.dispose();
  });

  it("batches replay records but stops at a resize record and a remembered clear point", async () => {
    const { term, client, pipeline } = makePipeline({ clearAfterSeq: 3 });
    client.attachResults.push(attachResult(1, 6));
    await pipeline.attach();
    pipeline.handleOutput(output("e1", 1, "a"));
    for (const event of [output("e1", 2, "b"), output("e1", 3, "c"), output("e1", 4, "d"), resize("e1", 5, 90, 20), output("e1", 6, "f")]) {
      pipeline.handleOutput(event);
    }
    term.drain();
    expect(term.order).toEqual(["write:a", "write:bc", "clear", "write:d", "resize:90x20", "write:f"]);
    expect(pipeline.replayedBytes).toBe(5);
    expect(pipeline.currentMode).toBe("live");
    pipeline.dispose();
  });
});

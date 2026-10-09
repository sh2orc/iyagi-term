/**
 * 화면 지우기(pipeline): 적용한 레코드 뒤에서 지우고 그 지점을 기억하며, 재접속
 * 재생이 같은 레코드를 지나는 순간 다시 지워 지운 내용이 스크롤로 돌아오지 않는다.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AttachParams } from "../src/generated/AttachParams";
import type { AttachResult } from "../src/generated/AttachResult";
import type { InputParams } from "../src/generated/InputParams";
import type { ResizeParams } from "../src/generated/ResizeParams";
import type { SessionAck } from "../src/generated/SessionAck";
import { bytesToBase64 } from "../src/features/daemon/base64";
import { SessionPipeline, type PipelineTerminal } from "../src/features/terminal/pipeline";

class FakeTerm implements PipelineTerminal {
  order: string[] = [];
  pendingWrites: Array<() => void> = [];

  write(data: Uint8Array, callback?: () => void): void {
    this.order.push(`write:${new TextDecoder().decode(data)}`);
    this.pendingWrites.push(() => callback?.());
  }
  resize(cols: number, rows: number): void {
    this.order.push(`resize:${cols}x${rows}`);
  }
  onData(): { dispose(): void } {
    return { dispose: () => undefined };
  }
  clear(): void {
    this.order.push("clear");
  }
  /** Complete the oldest pending write callback. */
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
  async sessionAttach(_params: AttachParams): Promise<AttachResult> {
    const result = this.attachResults.shift();
    if (!result) throw new Error("no scripted attach result");
    return result;
  }
  async sessionInput(params: InputParams): Promise<{ input_id: string; accepted_bytes: number }> {
    return { input_id: params.input_id, accepted_bytes: 0 };
  }
  async sessionResize(params: ResizeParams): Promise<{ resize_id: string; applied_seq: string }> {
    return { resize_id: params.resize_id, applied_seq: "1" };
  }
  sessionAck(_ack: SessionAck): void {}
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

function attachResult(epoch: string, fromSeq: number, lastSeq: number): AttachResult {
  return { epoch, replay_from_seq: String(fromSeq), last_seq: String(lastSeq), cols: 80, rows: 24 };
}

function makePipeline(options: { clearAfterSeq?: number; withClear?: boolean } = {}) {
  const term = new FakeTerm();
  const client = new FakeClient();
  const onCleared = vi.fn();
  const pipeline = new SessionPipeline(
    {
      client: client as never,
      sessionId: "s1",
      viewId: "v1",
      terminal: term,
      uuid: () => "id",
      clearTerminal: options.withClear === false ? undefined : () => term.clear(),
      clearAfterSeq: options.clearAfterSeq,
    },
    { onCleared },
  );
  return { term, client, pipeline, onCleared };
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("SessionPipeline clear screen", () => {
  it("clears right away when idle and remembers the last applied record", async () => {
    const { term, client, pipeline, onCleared } = makePipeline();
    client.attachResults.push(attachResult("e1", 1, 2));
    await pipeline.attach();
    pipeline.handleOutput(output("e1", 1, "a"));
    pipeline.handleOutput(output("e1", 2, "b"));
    term.drain();
    pipeline.clearScreen();
    expect(term.order).toEqual(["write:a", "write:b", "clear"]);
    expect(onCleared).toHaveBeenCalledWith(2);
    expect(pipeline.clearMark).toBe(2);
  });

  it("waits for the in-flight write, then clears before writing the next record", async () => {
    const { term, client, pipeline, onCleared } = makePipeline();
    client.attachResults.push(attachResult("e1", 1, 0));
    await pipeline.attach();
    pipeline.handleOutput(output("e1", 1, "a"));
    pipeline.clearScreen();
    pipeline.handleOutput(output("e1", 2, "b"));
    expect(term.order).toEqual(["write:a"]);
    term.consumeOne();
    expect(term.order).toEqual(["write:a", "clear", "write:b"]);
    expect(onCleared).toHaveBeenCalledWith(1);
  });

  it("clears again at the same record when reattaching replays the journal", async () => {
    const { term, client, pipeline, onCleared } = makePipeline();
    client.attachResults.push(attachResult("e1", 1, 2));
    await pipeline.attach();
    pipeline.handleOutput(output("e1", 1, "a"));
    pipeline.handleOutput(output("e1", 2, "b"));
    term.drain();
    pipeline.clearScreen();
    pipeline.handleOutput(output("e1", 3, "c"));
    term.drain();

    term.order = [];
    client.attachResults.push(attachResult("e2", 1, 3));
    await pipeline.attach();
    for (const [seq, text] of [[1, "a"], [2, "b"], [3, "c"]] as const) pipeline.handleOutput(output("e2", seq, text));
    term.drain();
    expect(term.order).toEqual(["write:a", "write:b", "clear", "write:c"]);
    expect(onCleared).toHaveBeenCalledTimes(1);
    expect(pipeline.currentMode).toBe("live");
  });

  it("uses a remembered clear point on the first attach (after an app restart)", async () => {
    const { term, client, pipeline, onCleared } = makePipeline({ clearAfterSeq: 2 });
    client.attachResults.push(attachResult("e1", 1, 3));
    await pipeline.attach();
    for (const [seq, text] of [[1, "a"], [2, "b"], [3, "c"]] as const) pipeline.handleOutput(output("e1", seq, text));
    term.drain();
    expect(term.order).toEqual(["write:a", "write:b", "clear", "write:c"]);
    expect(onCleared).not.toHaveBeenCalled();
  });

  it("skips the replayed clear when the journal no longer has that record", async () => {
    const { term, client, pipeline } = makePipeline({ clearAfterSeq: 2 });
    client.attachResults.push(attachResult("e1", 5, 6));
    await pipeline.attach();
    pipeline.handleOutput(output("e1", 5, "e"));
    pipeline.handleOutput(output("e1", 6, "f"));
    term.drain();
    expect(term.order).toEqual(["write:e", "write:f"]);
  });

  it("keeps an earlier clear point when a later clear has nothing new to remember", async () => {
    const { term, client, pipeline, onCleared } = makePipeline({ clearAfterSeq: 4 });
    client.attachResults.push(attachResult("e1", 1, 0));
    await pipeline.attach();
    pipeline.clearScreen();
    expect(term.order).toEqual(["clear"]);
    expect(onCleared).not.toHaveBeenCalled();
    expect(pipeline.clearMark).toBe(4);
  });

  it("does nothing without a terminal clear function", async () => {
    const { term, client, pipeline, onCleared } = makePipeline({ withClear: false });
    client.attachResults.push(attachResult("e1", 1, 0));
    await pipeline.attach();
    expect(() => pipeline.clearScreen()).not.toThrow();
    expect(term.order).toEqual([]);
    expect(onCleared).not.toHaveBeenCalled();
  });
});

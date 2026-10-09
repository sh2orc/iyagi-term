/**
 * 덮개(zoom preview)가 올라와 있는 동안의 출력 묶음 우회.
 *
 * 16ms 묶음 창·프레임 보류는 반쯤 그려진 화면이 비치지 않게 하는 장치다.
 * 덮개가 이미 화면을 가리고 있으면 그 지연은 드러내기까지의 시간만 더할
 * 뿐이라, pipeline은 덮개가 있는 동안 묶음을 건너뛰고 곧바로 쓴다.
 * (node 환경에선 DOM이 없어 진짜 프리뷰를 못 만들므로 모듈을 가짜로 갈아끼운다.)
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SessionPipeline, type PipelineTerminal } from "./pipeline";
import type { PipelineClient } from "../daemon/client";
import type { SessionOutput } from "../../generated/SessionOutput";
import { bytesToBase64 } from "../daemon/base64";

const cover = vi.hoisted(() => ({ active: false }));
vi.mock("./zoomPreview", () => ({
  zoomPreviewFor: () =>
    cover.active
      ? {
          outputPending: () => undefined,
          outputIdle: () => undefined,
          resizeApplied: () => undefined,
          resizeSignaled: () => undefined,
          holdForResize: () => undefined,
        }
      : undefined,
  cancelZoomPreview: () => undefined,
}));

interface RecordingTerminal extends PipelineTerminal {
  writes: Uint8Array[];
}

function fakeTerminal(): RecordingTerminal {
  const writes: Uint8Array[] = [];
  return {
    writes,
    reset: () => undefined,
    write: (data: Uint8Array, callback?: () => void) => {
      writes.push(data);
      callback?.();
    },
    resize: () => undefined,
    onData: () => ({ dispose: () => undefined }),
  };
}

function fakeClient(epoch: string): PipelineClient {
  return {
    sessionAttach: async () => ({
      epoch,
      replay_from_seq: "1",
      last_seq: "0",
      cols: 80,
      rows: 24,
    }),
    sessionInput: async () => ({ input_id: "i", accepted_bytes: 0 }),
    sessionResize: async () => ({ resize_id: "r", applied_seq: "0" }),
    sessionAck: () => undefined,
  };
}

function outputEvent(epoch: string, seq: number, text: string): SessionOutput {
  const bytes = new TextEncoder().encode(text);
  return {
    session_id: "s1",
    epoch,
    seq: String(seq),
    kind: "output",
    data_b64: bytesToBase64(bytes),
    raw_len: bytes.length,
  };
}

async function makeLivePipeline(outputCoalesceMs: number) {
  const terminal = fakeTerminal();
  const epoch = "e1";
  const pipeline = new SessionPipeline({
    client: fakeClient(epoch),
    sessionId: "s1",
    viewId: "v1",
    terminal,
    uuid: () => "u",
    outputCoalesceMs,
  });
  await pipeline.attach();
  return { pipeline, terminal, epoch };
}

describe("SessionPipeline 덮개 중 묶음 우회", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    cover.active = false;
  });
  afterEach(() => vi.useRealTimers());

  it("덮개가 있으면 묶음 창을 기다리지 않고 곧바로 쓴다", async () => {
    const { pipeline, terminal, epoch } = await makeLivePipeline(16);
    cover.active = true;
    pipeline.handleOutput(outputEvent(epoch, 1, "a"));
    pipeline.handleOutput(outputEvent(epoch, 2, "b"));
    // 창(16ms)이 지나지 않았는데도 두 기록이 이미 흘렀다.
    expect(terminal.writes.length).toBe(2);
  });

  it("덮개가 없으면 여전히 묶음 창을 지킨다(반쯤 그린 프레임 방지)", async () => {
    const { pipeline, terminal, epoch } = await makeLivePipeline(16);
    pipeline.handleOutput(outputEvent(epoch, 1, "a"));
    pipeline.handleOutput(outputEvent(epoch, 2, "b"));
    expect(terminal.writes.length).toBe(0);
    vi.advanceTimersByTime(16);
    expect(terminal.writes.length).toBe(1); // 묶여서 한 번
  });
});

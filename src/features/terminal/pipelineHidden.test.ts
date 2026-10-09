/**
 * 숨은 view(다른 탭) 출력 표시 빈도 낮추기(paint throttle, 04-ui §4).
 *
 * 활성 탭만 그려지고 숨은 pane의 xterm·WebGL 렌더러는 유지되지만(registry LRU)
 * 아무도 그 캔버스를 보지 않는다. 숨은 동안 live 출력을 넓은 창으로 묶어
 * write/paint를 줄이되, 버퍼에는 그대로 써서 출력을 하나도 잃지 않고, 다시
 * 보이면 밀린 출력을 즉시 흘려 곧바로 최신 화면이 되게 한다.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SessionPipeline, type PipelineTerminal } from "./pipeline";
import type { PipelineClient } from "../daemon/client";
import type { SessionOutput } from "../../generated/SessionOutput";
import { bytesToBase64 } from "../daemon/base64";

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
      callback?.(); // xterm 계열 fake: 파싱 완료 콜백을 동기로 부른다.
    },
    resize: () => undefined,
    onData: () => ({ dispose: () => undefined }),
  };
}

/** 빈 저널로 attach하면 재생 없이 곧바로 live가 된다(replay_from=1, last=0). */
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

async function makeLivePipeline(outputCoalesceMs = 0) {
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

/** 지금까지 터미널 버퍼에 넘어간 전체 바이트(순서대로 이어붙여 디코드). */
function writtenText(terminal: RecordingTerminal): string {
  const total = terminal.writes.reduce((n, w) => n + w.length, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const w of terminal.writes) {
    out.set(w, offset);
    offset += w.length;
  }
  return new TextDecoder().decode(out);
}

describe("SessionPipeline 숨은 view paint throttle", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("숨은 view는 여러 출력을 보이는 view보다 적은 write로 묶는다", async () => {
    // 보이는 view: coalesce 0 → 시간차로 온 출력마다 곧바로 그린다.
    const visible = await makeLivePipeline();
    expect(visible.pipeline.currentMode).toBe("live");
    for (let i = 1; i <= 4; i += 1) {
      visible.pipeline.handleOutput(outputEvent(visible.epoch, i, `v${i}`));
    }
    const visibleWrites = visible.terminal.writes.length;
    expect(visibleWrites).toBe(4);

    // 숨은 view: 같은 개수의 출력을 넓은 창으로 묶는다.
    const hidden = await makeLivePipeline();
    hidden.pipeline.setHidden(true);
    for (let i = 1; i <= 4; i += 1) {
      hidden.pipeline.handleOutput(outputEvent(hidden.epoch, i, `h${i}`));
    }
    // 창이 아직 안 지났다 — 하나도 그리지 않았다(버퍼에는 쌓였다).
    expect(hidden.terminal.writes.length).toBe(0);

    vi.advanceTimersByTime(350); // HIDDEN_OUTPUT_COALESCE_MS(300) 경과
    const hiddenWrites = hidden.terminal.writes.length;
    expect(hiddenWrites).toBe(1);
    expect(hiddenWrites).toBeLessThan(visibleWrites);
    // 묶었을 뿐 잃지 않았다 — 4개 출력이 모두 순서대로 버퍼에 들어갔다.
    expect(writtenText(hidden.terminal)).toBe("h1h2h3h4");
  });

  it("다시 보이면 밀린 출력을 즉시 흘리고 빠른 창을 되돌린다(유실 없음)", async () => {
    const { pipeline, terminal, epoch } = await makeLivePipeline();
    pipeline.setHidden(true);
    const fed = ["chunk-1", "chunk-2", "chunk-3"];
    fed.forEach((text, idx) => pipeline.handleOutput(outputEvent(epoch, idx + 1, text)));
    expect(terminal.writes.length).toBe(0); // 숨은 동안 대기(버퍼에는 있다)

    // 다시 보임: 타이머를 돌리지 않았는데도 즉시 한 번에 흘린다.
    pipeline.setHidden(false);
    expect(terminal.writes.length).toBe(1);
    expect(writtenText(terminal)).toBe(fed.join(""));

    // 빠른 창 복귀: 다음 출력은 타이머 없이 곧바로 그린다.
    pipeline.handleOutput(outputEvent(epoch, 4, "chunk-4"));
    expect(terminal.writes.length).toBe(2);
    expect(writtenText(terminal)).toBe(`${fed.join("")}chunk-4`);
  });

  it("숨김→표시 왕복 전체에서 먹인 바이트와 그린 바이트가 같다(유실 없음)", async () => {
    const { pipeline, terminal, epoch } = await makeLivePipeline();
    let seq = 0;
    let expected = "";
    const feed = (text: string) => {
      seq += 1;
      expected += text;
      pipeline.handleOutput(outputEvent(epoch, seq, text));
    };

    feed("a1"); // 보임: 즉시
    pipeline.setHidden(true);
    feed("b1");
    feed("b2");
    vi.advanceTimersByTime(350); // 숨김 창 경과 → 묶어서 한 번
    feed("b3"); // 여전히 숨김: 다음 창
    pipeline.setHidden(false); // 표시: 밀린 것 즉시 flush
    feed("c1"); // 보임: 즉시

    expect(writtenText(terminal)).toBe(expected);
  });
});

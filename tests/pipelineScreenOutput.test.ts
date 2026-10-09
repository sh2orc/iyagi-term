/**
 * 마지막 출력 배지 신호(pipeline `onScreenOutput`): 화면 글자를 바꾼 live 출력만 센다.
 * 활동 점 신호(`onLiveOutput`)는 예전처럼 live 바이트마다 켜진다.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AttachParams } from "../src/generated/AttachParams";
import type { AttachResult } from "../src/generated/AttachResult";
import type { InputParams } from "../src/generated/InputParams";
import type { ResizeParams } from "../src/generated/ResizeParams";
import type { SessionAck } from "../src/generated/SessionAck";
import type { SessionOutput } from "../src/generated/SessionOutput";
import { bytesToBase64 } from "../src/features/daemon/base64";
import { SessionPipeline, type PipelineTerminal } from "../src/features/terminal/pipeline";
import { SCREEN_CHECK_MS, SETTLE_MS } from "../src/features/terminal/screenOutput";

/** 실제 Claude Code 저널에서 되풀이되던 레코드: 문자셋 지정 + SI + 마우스 추적 모드 켜기. */
const MOUSE_MODES = "\x1b(B\x0f\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h";
/** xterm이 마우스 추적 중인 앱에 보내는 휠 올림 보고(SGR). */
const WHEEL_UP = "\x1b[<64;10;5M";

/** 쓰기가 적용되면 글자를 화면에 이어 붙이는 가짜 — ESC로 시작하는 제어 바이트는 글자를 바꾸지 않는다. */
class ScreenTerm implements PipelineTerminal {
  screen = "";
  pendingWrites: Array<() => void> = [];
  private dataListener: ((data: string) => void) | null = null;

  write(data: Uint8Array, callback?: () => void): void {
    const text = new TextDecoder().decode(data);
    this.pendingWrites.push(() => {
      if (!text.startsWith("\x1b")) this.screen += text;
      callback?.();
    });
  }
  resize(): void {}
  onData(callback: (data: string) => void): { dispose(): void } {
    this.dataListener = callback;
    return {
      dispose: () => {
        this.dataListener = null;
      },
    };
  }
  /** xterm이 보내는 입력(키·마우스·포커스 보고)을 흉내 낸다. */
  type(data: string): void {
    this.dataListener?.(data);
  }
  drain(): void {
    let guard = 0;
    while (this.pendingWrites.length > 0 && guard++ < 1000) this.pendingWrites.shift()?.();
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

function output(epoch: string, seq: number, text: string): SessionOutput {
  const bytes = new TextEncoder().encode(text);
  return { session_id: "s1", epoch, seq: String(seq), kind: "output", data_b64: bytesToBase64(bytes), raw_len: bytes.length };
}

function resize(epoch: string, seq: number, cols: number, rows: number): SessionOutput {
  return { session_id: "s1", epoch, seq: String(seq), kind: "resize", data_b64: "", raw_len: 0, cols, rows };
}

function attachResult(epoch: string, fromSeq: number, lastSeq: number): AttachResult {
  return { epoch, replay_from_seq: String(fromSeq), last_seq: String(lastSeq), cols: 80, rows: 24 };
}

function makePipeline(options: { withScreen?: boolean } = {}) {
  const term = new ScreenTerm();
  const client = new FakeClient();
  const onLiveOutput = vi.fn();
  const onScreenOutput = vi.fn();
  const pipeline = new SessionPipeline(
    {
      client: client as never,
      sessionId: "s1",
      viewId: "v1",
      terminal: term,
      uuid: () => "id",
      clearTerminal: () => {
        term.screen = "";
      },
      screenSignature: options.withScreen === false ? undefined : () => term.screen,
    },
    { onLiveOutput, onScreenOutput },
  );
  return { term, client, pipeline, onLiveOutput, onScreenOutput };
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("SessionPipeline last-output signal", () => {
  it("ignores bytes that leave the screen text alone, while the activity dot still lights up", async () => {
    const { term, client, pipeline, onLiveOutput, onScreenOutput } = makePipeline();
    client.attachResults.push(attachResult("e1", 1, 0));
    await pipeline.attach();
    for (let seq = 1; seq <= 5; seq++) pipeline.handleOutput(output("e1", seq, MOUSE_MODES));
    term.drain();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onLiveOutput).toHaveBeenCalledTimes(5);
    expect(onScreenOutput).not.toHaveBeenCalled();

    pipeline.handleOutput(output("e1", 6, "answer"));
    term.drain();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onScreenOutput).toHaveBeenCalledOnce();
  });

  it("does not count the redraw that follows a live resize (SIGWINCH), then counts real output", async () => {
    const { term, client, pipeline, onScreenOutput } = makePipeline();
    client.attachResults.push(attachResult("e1", 1, 1));
    await pipeline.attach();
    pipeline.handleOutput(output("e1", 1, "prompt"));
    term.drain();
    expect(pipeline.currentMode).toBe("live");

    pipeline.handleOutput(resize("e1", 2, 81, 32));
    pipeline.handleOutput(output("e1", 3, "\r\nprompt"));
    vi.advanceTimersByTime(16); // The resize redraw is collected before writing.
    term.drain();
    vi.advanceTimersByTime(SETTLE_MS);
    expect(onScreenOutput).not.toHaveBeenCalled();

    pipeline.handleOutput(output("e1", 4, "\r\nanswer"));
    term.drain();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onScreenOutput).toHaveBeenCalledOnce();
  });

  it("does not count the app's reaction to local input, such as a wheel scroll in a mouse-tracking TUI", async () => {
    const { term, client, pipeline, onScreenOutput } = makePipeline();
    client.attachResults.push(attachResult("e1", 1, 1));
    await pipeline.attach();
    pipeline.handleOutput(output("e1", 1, "transcript bottom"));
    term.drain();

    term.type(WHEEL_UP);
    pipeline.handleOutput(output("e1", 2, "\r\ntranscript top"));
    term.drain();
    vi.advanceTimersByTime(SETTLE_MS);
    expect(onScreenOutput).not.toHaveBeenCalled();

    pipeline.handleOutput(output("e1", 3, "\r\nnew answer"));
    term.drain();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onScreenOutput).toHaveBeenCalledOnce();
  });

  it("treats pasted and broadcast input like keystrokes", async () => {
    const { term, client, pipeline, onScreenOutput } = makePipeline();
    client.attachResults.push(attachResult("e1", 1, 0));
    await pipeline.attach();
    expect(pipeline.sendInput("echo hi")).toBe(true);
    pipeline.handleOutput(output("e1", 1, "echo hi"));
    term.drain();
    vi.advanceTimersByTime(SETTLE_MS);
    expect(onScreenOutput).not.toHaveBeenCalled();
  });

  it("does not count replayed records and takes the replayed screen as the baseline", async () => {
    const { term, client, pipeline, onScreenOutput } = makePipeline();
    client.attachResults.push(attachResult("e1", 1, 2));
    await pipeline.attach();
    pipeline.handleOutput(output("e1", 1, "hello"));
    pipeline.handleOutput(output("e1", 2, " world"));
    term.drain();
    vi.advanceTimersByTime(SETTLE_MS);
    expect(pipeline.currentMode).toBe("live");
    expect(onScreenOutput).not.toHaveBeenCalled();

    pipeline.handleOutput(output("e1", 3, MOUSE_MODES));
    term.drain();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onScreenOutput).not.toHaveBeenCalled();
  });

  it("re-attaching suspends counting until the new replay has been applied", async () => {
    const { term, client, pipeline, onScreenOutput } = makePipeline();
    client.attachResults.push(attachResult("e1", 1, 0));
    await pipeline.attach();
    pipeline.handleOutput(output("e1", 1, "live text"));
    term.drain();

    client.attachResults.push(attachResult("e2", 1, 1));
    await pipeline.attach();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    pipeline.handleOutput(output("e2", 1, "live text"));
    term.drain();
    expect(pipeline.currentMode).toBe("live");
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onScreenOutput).not.toHaveBeenCalled();
  });

  it("takes a cleared screen as the new baseline", async () => {
    const { term, client, pipeline, onScreenOutput } = makePipeline();
    client.attachResults.push(attachResult("e1", 1, 1));
    await pipeline.attach();
    pipeline.handleOutput(output("e1", 1, "old text"));
    term.drain();
    pipeline.clearScreen();

    pipeline.handleOutput(output("e1", 2, MOUSE_MODES));
    term.drain();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onScreenOutput).not.toHaveBeenCalled();
  });

  it("counts every live output when there is no screen signature, as before", async () => {
    const { term, client, pipeline, onScreenOutput } = makePipeline({ withScreen: false });
    client.attachResults.push(attachResult("e1", 1, 0));
    await pipeline.attach();
    pipeline.handleOutput(output("e1", 1, MOUSE_MODES));
    term.drain();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onScreenOutput).toHaveBeenCalledOnce();
  });

  it("stops checking once disposed", async () => {
    const { term, client, pipeline, onScreenOutput } = makePipeline();
    client.attachResults.push(attachResult("e1", 1, 0));
    await pipeline.attach();
    pipeline.handleOutput(output("e1", 1, "text"));
    term.drain();
    pipeline.dispose();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onScreenOutput).not.toHaveBeenCalled();
  });
});

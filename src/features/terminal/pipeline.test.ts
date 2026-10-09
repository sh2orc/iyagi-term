/**
 * 재생 막힘 회복 결함 수정의 회귀 검증:
 *  1) attach RPC 상한 — 응답을 잃은 attach가 "기록 재생 중…"에 영원히 묶여
 *     있지 않게 한다(ATTACH_TIMEOUT_MS 뒤 실패, 재시도 가능, 늦은 응답 무시).
 *  2) 막힘 와치독 반복 — 알린 뒤에도 점검을 잇는다. 회복이 조용히 실패해도
 *     (재시도가 이미 걸린 attach에 묶이면) 계속 알린다.
 *  3) 터미널 초기화 지연 — reset을 attach 응답 직후가 아니라 첫 재생 기록·
 *     스냅샷 조각 쓰기 직전에 내려 attach 응답과 첫 바이트 사이의 빈 화면
 *     틈을 없앤다.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SessionPipeline, type PipelineTerminal } from "./pipeline";
import type { PipelineClient } from "../daemon/client";
import type { AttachResult } from "../../generated/AttachResult";
import type { SessionOutput } from "../../generated/SessionOutput";
import { bytesToBase64 } from "../daemon/base64";

/** pipeline.ts의 상수와 같은 값 — 경계(±1ms) 판정에 쓴다. */
const ATTACH_TIMEOUT_MS = 15000;
const REPLAY_STALL_MS = 15000;
const REPLAY_STALL_CHECK_MS = 5000;

interface RecordingTerminal extends PipelineTerminal {
  writes: Uint8Array[];
  /** reset·write·resize 호출 순서(초기화 시점 검증용). */
  calls: string[];
}

function fakeTerminal(): RecordingTerminal {
  const writes: Uint8Array[] = [];
  const calls: string[] = [];
  return {
    writes,
    calls,
    reset: () => calls.push("reset"),
    write: (data: Uint8Array, callback?: () => void) => {
      calls.push(`write:${new TextDecoder().decode(data)}`);
      writes.push(data);
      callback?.(); // xterm 계열 fake: 파싱 완료 콜백을 동기로 부른다.
    },
    resize: (cols: number, rows: number) => calls.push(`resize:${cols}x${rows}`),
    onData: () => ({ dispose: () => undefined }),
  };
}

function attachResult(epoch: string, fromSeq: number, lastSeq: number): AttachResult {
  return { epoch, replay_from_seq: String(fromSeq), last_seq: String(lastSeq), cols: 80, rows: 24 };
}

/** lastSeq를 주지 않으면 빈 저널(from 앞에 적용할 기록이 없다). */
function fakeClient(epoch: string, fromSeq = 1, lastSeq = fromSeq - 1): PipelineClient {
  return {
    sessionAttach: async () => attachResult(epoch, fromSeq, lastSeq),
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

describe("SessionPipeline attach RPC 상한", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("응답을 잃은 attach는 상한 뒤 실패하고, 이어서 다시 attach할 수 있다", async () => {
    // 첫 attach는 응답이 사라진다(재시작 직후 브리지 등) — resolve를 쥐고 놓지 않는다.
    const held: { resolve: ((result: AttachResult) => void) | null } = { resolve: null };
    let nextAttach: () => Promise<AttachResult> = () =>
      new Promise<AttachResult>((resolve) => {
        held.resolve = resolve;
      });
    const client: PipelineClient = {
      ...fakeClient("e1"),
      sessionAttach: () => nextAttach(),
    };
    const pipeline = new SessionPipeline({
      client,
      sessionId: "s1",
      viewId: "v1",
      terminal: fakeTerminal(),
      uuid: () => "u",
    });

    const first = pipeline.attach();
    expect(pipeline.isAttachPending).toBe(true);
    const outcome = expect(first).rejects.toThrow("session attach timed out");
    vi.advanceTimersByTime(ATTACH_TIMEOUT_MS);
    await outcome;
    // 상한 걸림 → finally가 attachRun을 비워 다음 재시도가 가능하다.
    expect(pipeline.isAttachPending).toBe(false);

    // 타임아웃이 이미 이긴 뒤 늦게 도착한 응답은 무시된다 — 끝난 attach가 되살아나지 않는다.
    held.resolve?.(attachResult("e-late", 1, 0));
    await vi.advanceTimersByTimeAsync(0);
    expect(pipeline.currentMode).toBe("detached");
    expect(pipeline.currentEpoch).toBeNull();

    // attachRun이 비웠으므로 다시 붙을 수 있다 — 정상 응답이면 live가 된다.
    nextAttach = async () => attachResult("e2", 1, 0);
    await pipeline.attach();
    expect(pipeline.currentEpoch).toBe("e2");
    expect(pipeline.currentMode).toBe("live");
    expect(pipeline.isAttachPending).toBe(false);
  });

  it("정상 응답은 상한보다 빨라 항상 이긴다 — 상한 뒤에도 아무 일도 일어나지 않는다", async () => {
    const pipeline = new SessionPipeline({
      client: fakeClient("e1"),
      sessionId: "s1",
      viewId: "v1",
      terminal: fakeTerminal(),
      uuid: () => "u",
    });
    await pipeline.attach();
    expect(pipeline.currentMode).toBe("live");
    await vi.advanceTimersByTimeAsync(ATTACH_TIMEOUT_MS + REPLAY_STALL_CHECK_MS);
    expect(pipeline.currentMode).toBe("live");
  });
});

describe("SessionPipeline 재생 막힘 와치독", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("알림 뒤에도 점검을 잇는다 — 회복이 조용히 실패해도 다시 알리고, 진행이 오면 멈춘다", async () => {
    const stalls = vi.fn();
    const epoch = "e1";
    // 레코드 1..3을 재생해야 하는 저널 — 기록을 주지 않으면 재생이 막혀 있다.
    const pipeline = new SessionPipeline(
      {
        client: fakeClient(epoch, 1, 3),
        sessionId: "s1",
        viewId: "v1",
        terminal: fakeTerminal(),
        uuid: () => "u",
      },
      { onReplayStalled: stalls },
    );
    await pipeline.attach();
    expect(pipeline.currentMode).toBe("replay");

    // REPLAY_STALL_MS(15s) 경과 — 첫 알림.
    vi.advanceTimersByTime(REPLAY_STALL_MS);
    expect(stalls).toHaveBeenCalledTimes(1);

    // 여기서 회복(재attach)이 일어나지 않거나 이미 걸린 attach에 묶여 아무
    // 일도 하지 않아도, 점검 간격(5s)마다 다시 알린다.
    vi.advanceTimersByTime(REPLAY_STALL_CHECK_MS);
    expect(stalls).toHaveBeenCalledTimes(2);

    // 레코드를 끝까지 먹인다 — 재생이 끝나 live로 나가면 점검도 멈춘다.
    for (let seq = 1; seq <= 3; seq += 1) {
      pipeline.handleOutput(outputEvent(epoch, seq, `r${seq}`));
    }
    expect(pipeline.currentMode).toBe("live");
    vi.advanceTimersByTime(3 * REPLAY_STALL_MS);
    expect(stalls).toHaveBeenCalledTimes(2);
  });
});

describe("SessionPipeline 터미널 초기화 지연(reset)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("resetTerminal 초기화는 attach 응답이 아니라 첫 기록 쓰기 직전에 내린다", async () => {
    const terminal = fakeTerminal();
    const epoch = "e1";
    const pipeline = new SessionPipeline({
      client: fakeClient(epoch, 1, 2),
      sessionId: "s1",
      viewId: "v1",
      terminal,
      uuid: () => "u",
    });
    await pipeline.attach(true);
    // attach 응답 직후에는 아직 초기화하지 않는다 — 첫 바이트가 올 때까지
    // 빈 화면이 드러나지 않게.
    expect(terminal.calls).toEqual([]);

    pipeline.handleOutput(outputEvent(epoch, 1, "hello"));
    // reset이 첫 write보다 먼저, 정확히 한 번.
    expect(terminal.calls).toEqual(["reset", "write:hello"]);

    // 이어지는 기록에는 다시 초기화하지 않는다.
    pipeline.handleOutput(outputEvent(epoch, 2, "!"));
    expect(terminal.calls).toEqual(["reset", "write:hello", "write:!"]);
    expect(pipeline.currentMode).toBe("live");
  });

  it("스냅샷 복원에서는 격자 맞춤 뒤, 첫 조각 쓰기 직전에 초기화한다(resize → reset → write)", async () => {
    const terminal = fakeTerminal();
    const snapshot = { seq: 3, cols: 100, rows: 30, data: "SNAP" };
    const epoch = "e1";
    // 데몬이 스냅샷 바로 다음(seq 4)부터 재생한다고 받아들였다.
    const pipeline = new SessionPipeline({
      client: fakeClient(epoch, 4, 4),
      sessionId: "s1",
      viewId: "v1",
      terminal,
      uuid: () => "u",
    });
    await pipeline.attach(false, snapshot);
    expect(pipeline.currentMode).toBe("replay"); // 스냅샷 뒤 남은 기록(seq 4) 대기
    expect(terminal.calls).toEqual(["resize:100x30", "reset", "write:SNAP"]);

    pipeline.handleOutput(outputEvent(epoch, 4, "tail"));
    expect(terminal.calls).toEqual(["resize:100x30", "reset", "write:SNAP", "write:tail"]);
    expect(pipeline.currentMode).toBe("live");
  });

  it("빈 저널 재생에서는 초기화를 소비하지 않는다 — 지난 화면은 같은 세션의 마지막 상태다", async () => {
    const terminal = fakeTerminal();
    const pipeline = new SessionPipeline({
      client: fakeClient("e1", 1, 0),
      sessionId: "s1",
      viewId: "v1",
      terminal,
      uuid: () => "u",
    });
    await pipeline.attach(true);
    // 적용할 기록이 없으면 reset을 내리지 않는다(의도된 동작) — 이 화면은
    // 이미 같은 세션이 그려 놓은 것이다.
    expect(pipeline.currentMode).toBe("live");
    expect(terminal.calls).toEqual([]);
  });
});

describe("SessionPipeline 재생 중 크기 맞춤", () => {
  it("재생이 끝나기 전에도 최신 fit을 곧바로 데몬에 보낸다 — 크기 기록이 저널 꼬리에 실린다", async () => {
    const resizes: Array<{ cols: number; rows: number }> = [];
    const client: PipelineClient = {
      ...fakeClient("e1", 1, 2), // 기록 두 개가 아직 오지 않은 재생
      sessionResize: async (params) => {
        resizes.push({ cols: params.cols, rows: params.rows });
        // applied_seq가 재생 꼬리(2)를 가리킨다 — 데몬이 크기 기록을 저널에
        // 실었다는 뜻이므로 파이프라인은 로컬 격자 정렬을 하지 않는다.
        return { resize_id: "r", applied_seq: "2" };
      },
    };
    const terminal = fakeTerminal();
    const pipeline = new SessionPipeline({ client, sessionId: "s1", viewId: "v1", terminal, uuid: () => "u" });
    await pipeline.attach();
    expect(pipeline.currentMode).toBe("replay");
    pipeline.requestResize(132, 40);
    await Promise.resolve();
    expect(resizes).toEqual([{ cols: 132, rows: 40 }]);
    // xterm 격자는 저널의 크기 기록을 따른다 — RPC만으로는 바꾸지 않는다.
    expect(terminal.calls).not.toContain("resize:132x40");
  });

  it("끝난 세션은 재생이 끝난 뒤 로컬로만 맞춘다(PTY가 없다)", async () => {
    const resizes: number[] = [];
    const client: PipelineClient = {
      ...fakeClient("e1", 1, 1),
      sessionAttach: async () => ({ ...attachResult("e1", 1, 1), exited: true }) as AttachResult,
      sessionResize: async () => {
        resizes.push(1);
        return { resize_id: "r", applied_seq: "0" };
      },
    };
    const terminal = fakeTerminal();
    const pipeline = new SessionPipeline({ client, sessionId: "s1", viewId: "v1", terminal, uuid: () => "u" });
    await pipeline.attach();
    pipeline.requestResize(132, 40);
    await Promise.resolve();
    expect(resizes).toEqual([]);
    expect(terminal.calls).not.toContain("resize:132x40");
    pipeline.handleOutput(outputEvent("e1", 1, "bye"));
    expect(pipeline.currentMode).toBe("exited");
    expect(terminal.calls).toContain("resize:132x40");
    expect(resizes).toEqual([]);
  });
});

describe("SessionPipeline 크기 기록이 생략된 resize", () => {
  it("데몬이 이미 그 크기라 기록을 쓰지 않으면 xterm 격자를 직접 맞춘다", async () => {
    const client: PipelineClient = {
      ...fakeClient("e1"),
      // applied_seq가 이미 소비한 기록(빈 저널의 0)을 가리킨다 = 새 기록 없음.
      sessionResize: async () => ({ resize_id: "r", applied_seq: "0" }),
    };
    const terminal = fakeTerminal();
    const pipeline = new SessionPipeline({ client, sessionId: "s1", viewId: "v1", terminal, uuid: () => "u" });
    await pipeline.attach();
    expect(pipeline.currentMode).toBe("live");
    pipeline.requestResize(100, 40);
    for (let i = 0; i < 8; i += 1) await Promise.resolve();
    expect(terminal.calls).toContain("resize:100x40");
  });
});

describe("SessionPipeline 프레임 붙잡기(커서 숨김으로 끝난 묶음)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  function livePipeline(terminal: RecordingTerminal): SessionPipeline {
    return new SessionPipeline({
      client: fakeClient("e1"),
      sessionId: "s1",
      viewId: "v1",
      terminal,
      uuid: () => "u",
      outputCoalesceMs: 16,
    });
  }

  it("열린 프레임의 조각은 다음 조각을 기다렸다가 한 번에 쓴다", async () => {
    const terminal = fakeTerminal();
    const pipeline = livePipeline(terminal);
    await pipeline.attach();
    expect(pipeline.currentMode).toBe("live");
    pipeline.handleOutput(outputEvent("e1", 1, "\x1b[?25l\x1b[Hfirst"));
    await vi.advanceTimersByTimeAsync(16);
    expect(terminal.writes).toHaveLength(0); // 커서가 숨겨진 채 끝났다 — 붙잡는다
    pipeline.handleOutput(outputEvent("e1", 2, "second\x1b[?25h"));
    await vi.advanceTimersByTimeAsync(16);
    expect(terminal.writes).toHaveLength(1);
    expect(new TextDecoder().decode(terminal.writes[0])).toBe("\x1b[?25l\x1b[Hfirstsecond\x1b[?25h");
  });

  it("상한(80ms)이 지나면 열린 채라도 쓴다", async () => {
    const terminal = fakeTerminal();
    const pipeline = livePipeline(terminal);
    await pipeline.attach();
    pipeline.handleOutput(outputEvent("e1", 1, "\x1b[?25lonly"));
    await vi.advanceTimersByTimeAsync(16 * 6);
    expect(terminal.writes).toHaveLength(1);
  });

  it("커서를 보인 채 끝난 묶음은 창이 끝나는 대로 쓴다", async () => {
    const terminal = fakeTerminal();
    const pipeline = livePipeline(terminal);
    await pipeline.attach();
    pipeline.handleOutput(outputEvent("e1", 1, "\x1b[?25lframe\x1b[?25h"));
    await vi.advanceTimersByTimeAsync(16);
    expect(terminal.writes).toHaveLength(1);
  });
});

describe("SessionPipeline 크기 레코드는 출력 묶음 창을 기다리지 않는다", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  function livePipeline(terminal: RecordingTerminal, outputCoalesceMs = 16): SessionPipeline {
    return new SessionPipeline({
      client: fakeClient("e1"),
      sessionId: "s1",
      viewId: "v1",
      terminal,
      uuid: () => "u",
      outputCoalesceMs,
    });
  }

  function resizeEvent(epoch: string, seq: number, cols: number, rows: number): SessionOutput {
    return {
      session_id: "s1",
      epoch,
      seq: String(seq),
      kind: "resize",
      data_b64: "",
      raw_len: 0,
      cols,
      rows,
    };
  }

  it("출력이 열어 둔 묶음 창 뒤에 크기 레코드가 오면 타이머를 기다리지 않고 곧바로 처리된다", async () => {
    const terminal = fakeTerminal();
    const pipeline = livePipeline(terminal);
    await pipeline.attach();
    expect(pipeline.currentMode).toBe("live");

    pipeline.handleOutput(outputEvent("e1", 1, "out"));
    // 출력만 왔다 — 묶음 창이 열렸을 뿐, 아직 아무것도 쓰지 않았다.
    expect(terminal.calls).toEqual([]);

    pipeline.handleOutput(resizeEvent("e1", 2, 100, 40));
    // 타이머를 한 틱도 돌리지 않았는데 장벽(크기 레코드)이 열린 창을 곧바로
    // 닫는다 — 앞서 밀린 출력과 크기 변경이 같은 틱에서 순서대로 적용된다.
    expect(terminal.calls).toEqual(["write:out", "resize:100x40"]);
  });

  it("크기 레코드가 없으면 기존 묶음 동작 그대로다(회귀 방지) — 출력만 있으면 여전히 창만큼 기다린다", async () => {
    const terminal = fakeTerminal();
    const pipeline = livePipeline(terminal);
    await pipeline.attach();
    pipeline.handleOutput(outputEvent("e1", 1, "a"));
    expect(terminal.calls).toEqual([]); // 창이 열렸을 뿐 아직 쓰지 않았다
    await vi.advanceTimersByTimeAsync(16);
    expect(terminal.calls).toEqual(["write:a"]);
  });

  it("크기 레코드를 소비한 뒤에는 pendingResizes가 되돌아가 다음 출력이 다시 창만큼 기다린다(카운터 누수 없음)", async () => {
    const terminal = fakeTerminal();
    const pipeline = livePipeline(terminal);
    await pipeline.attach();

    pipeline.handleOutput(outputEvent("e1", 1, "out"));
    pipeline.handleOutput(resizeEvent("e1", 2, 100, 40));
    expect(terminal.calls).toEqual(["write:out", "resize:100x40"]);

    // 장벽을 지난 뒤 다음 출력 — 카운터가 남아 있다면(누수) 창을 열지 않고
    // 곧바로 쓴다. 새 창을 여는지로 카운터가 0으로 돌아갔는지 관찰한다.
    pipeline.handleOutput(outputEvent("e1", 3, "next"));
    expect(terminal.calls).toEqual(["write:out", "resize:100x40"]); // 아직 안 썼다 — 새 창이 열렸다
    await vi.advanceTimersByTimeAsync(16);
    expect(terminal.calls).toEqual(["write:out", "resize:100x40", "write:next"]);
  });
});

describe("SessionPipeline 입력 전송 실패 알림", () => {
  it("데몬이 입력을 거절하면 onInputError로 code째 알린다 — 조용히 삼키지 않는다", async () => {
    const stale = Object.assign(new Error("epoch does not match any attached view"), { code: "STALE_EPOCH" });
    const onInputError = vi.fn();
    const pipeline = new SessionPipeline(
      {
        client: { ...fakeClient("e1"), sessionInput: async () => { throw stale; } },
        sessionId: "s1",
        viewId: "v1",
        terminal: fakeTerminal(),
        uuid: () => "u",
      },
      { onInputError },
    );
    await pipeline.attach();
    expect(pipeline.sendInput("ls\n")).toBe(true);
    await vi.waitFor(() => expect(onInputError).toHaveBeenCalledTimes(1));
    expect((onInputError.mock.calls[0][0] as { code?: string }).code).toBe("STALE_EPOCH");
  });

  it("브리지가 던진 {code,message} 객체도 code를 잃지 않는다", async () => {
    const onInputError = vi.fn();
    const pipeline = new SessionPipeline(
      {
        client: {
          ...fakeClient("e1"),
          sessionInput: async () => { throw { code: "DAEMON_UNAVAILABLE", message: "timeout" }; },
        },
        sessionId: "s1",
        viewId: "v1",
        terminal: fakeTerminal(),
        uuid: () => "u",
      },
      { onInputError },
    );
    await pipeline.attach();
    pipeline.sendInput("x");
    await vi.waitFor(() => expect(onInputError).toHaveBeenCalled());
    const err = onInputError.mock.calls[0][0] as Error & { code?: string };
    expect(err).toBeInstanceOf(Error);
    expect(err.code).toBe("DAEMON_UNAVAILABLE");
  });
  it("브리지 객체의 details(reason_code)도 보존한다 — 입력 막힘·가드 정지를 가르는 근거", async () => {
    const onInputError = vi.fn();
    const pipeline = new SessionPipeline(
      {
        client: {
          ...fakeClient("e1"),
          sessionInput: async () => {
            throw { code: "BUSY", message: "not reading", details: { reason_code: "INPUT_STALLED" } };
          },
        },
        sessionId: "s1",
        viewId: "v1",
        terminal: fakeTerminal(),
        uuid: () => "u",
      },
      { onInputError },
    );
    await pipeline.attach();
    pipeline.sendInput("x");
    await vi.waitFor(() => expect(onInputError).toHaveBeenCalled());
    const err = onInputError.mock.calls[0][0] as Error & { details?: { reason_code?: string } };
    expect(err.details?.reason_code).toBe("INPUT_STALLED");
  });

  it("받아들여진 청크마다 onInputDelivered를 알린다", async () => {
    const onInputDelivered = vi.fn();
    const pipeline = new SessionPipeline(
      {
        client: fakeClient("e1"),
        sessionId: "s1",
        viewId: "v1",
        terminal: fakeTerminal(),
        uuid: () => "u",
      },
      { onInputDelivered },
    );
    await pipeline.attach();
    pipeline.sendInput("ls\n");
    await vi.waitFor(() => expect(onInputDelivered).toHaveBeenCalledTimes(1));
  });
});

describe("SessionPipeline 겹쳐진 resize 기록 접기(창 끌기)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  function livePipeline(terminal: RecordingTerminal): SessionPipeline {
    return new SessionPipeline({
      client: fakeClient("e1"),
      sessionId: "s1",
      viewId: "v1",
      terminal,
      uuid: () => "u",
      outputCoalesceMs: 16,
    });
  }

  function resizeEvent(epoch: string, seq: number, cols: number, rows: number): SessionOutput {
    return {
      session_id: "s1",
      epoch,
      seq: String(seq),
      kind: "resize",
      data_b64: "",
      raw_len: 0,
      cols,
      rows,
    };
  }

  it("연쇄 resize는 첫 단계만 즉시 적용하고 나머지는 마지막 것 하나로 접는다", async () => {
    const terminal = fakeTerminal();
    const pipeline = livePipeline(terminal);
    await pipeline.attach();
    expect(pipeline.currentMode).toBe("live");

    // 창 끌기 한 번: 8ms 간격의 드래그 단계가 차례로 도착한 모습.
    pipeline.handleOutput(resizeEvent("e1", 1, 100, 30));
    expect(terminal.calls).toEqual(["resize:100x30"]); // 첫 단계는 홀드 없이 즉시
    await vi.advanceTimersByTimeAsync(8);
    pipeline.handleOutput(resizeEvent("e1", 2, 104, 31)); // 직전과 8ms — 버스트
    await vi.advanceTimersByTimeAsync(8);
    pipeline.handleOutput(resizeEvent("e1", 3, 108, 32)); // 앞 것을 소비로 접는다
    await vi.advanceTimersByTimeAsync(8);
    pipeline.handleOutput(resizeEvent("e1", 4, 112, 33));
    // 이어지는 연쇄는 홀드를 늘리고, 간격 창(60ms)이 닫히기 전까지 계속
    // 잡고 있다 — 마지막 도착(t=24) 뒤 59ms에도 아직.
    expect(terminal.calls).toEqual(["resize:100x30"]);
    await vi.advanceTimersByTimeAsync(59);
    expect(terminal.calls).toEqual(["resize:100x30"]);
    await vi.advanceTimersByTimeAsync(1);
    // 연쇄 전체의 재접기는 마지막 격자 한 번뿐이다.
    expect(terminal.calls).toEqual(["resize:100x30", "resize:112x33"]);

    // 연쇄가 끊긴 뒤의 resize는 다시 홀드 없이 즉시 적용한다(단계 리셋).
    await vi.advanceTimersByTimeAsync(30);
    pipeline.handleOutput(resizeEvent("e1", 5, 120, 36));
    expect(terminal.calls).toEqual(["resize:100x30", "resize:112x33", "resize:120x36"]);

    // 접힌 기록도 소비는 됐다 — 뒤이은 출력이 막히지 않고 곧바로 흐른다.
    pipeline.handleOutput(outputEvent("e1", 6, "after"));
    await vi.advanceTimersByTimeAsync(16);
    expect(terminal.calls).toEqual([
      "resize:100x30", "resize:112x33", "resize:120x36", "write:after",
    ]);
  });

  it("홀수 resize(줌 한 번)는 홀드하지 않고 즉시 적용한다 — 지연 0", async () => {
    const terminal = fakeTerminal();
    const pipeline = livePipeline(terminal);
    await pipeline.attach();

    pipeline.handleOutput(resizeEvent("e1", 1, 100, 30));
    expect(terminal.calls).toEqual(["resize:100x30"]);
    // GAP(60ms)보다 한참 뒤의 resize도 마찬가지로 즉시다.
    await vi.advanceTimersByTimeAsync(200);
    pipeline.handleOutput(resizeEvent("e1", 2, 120, 36));
    expect(terminal.calls).toEqual(["resize:100x30", "resize:120x36"]);
  });

  it("버스트 간격(60ms) 안의 느린 드래그(40ms 간격)도 마지막 것만 남긴다", async () => {
    const terminal = fakeTerminal();
    const pipeline = livePipeline(terminal);
    await pipeline.attach();
    pipeline.handleOutput(resizeEvent("e1", 1, 100, 30)); // 연쇄의 첫 단계 — 즉시 적용.
    for (let seq = 2; seq <= 4; seq++) {
      await vi.advanceTimersByTimeAsync(40); // 40ms 간격 — 60ms 창 안이지만 옛 홀드 상한(32ms)보다 느리다.
      pipeline.handleOutput(resizeEvent("e1", seq, 100 + seq * 4, 30 + seq));
    }
    await vi.advanceTimersByTimeAsync(200); // 드래그 종료 — 간격 창이 닫히면 마지막 크기 적용.
    expect(terminal.calls).toEqual(["resize:100x30", "resize:116x34"]);
  });

  it("홀드가 돌던 중 epoch가 바뀌면 낡은 홀드가 새 재생을 미루지 않는다", async () => {
    const terminal = fakeTerminal();
    // 첫 attach는 e1, 재접속 attach는 e2로 응답한다.
    const replies = [attachResult("e1", 1, 1), attachResult("e2", 1, 1)];
    const client: PipelineClient = {
      ...fakeClient("e1"),
      sessionAttach: async () => replies.shift() ?? attachResult("e2", 1, 1),
    };
    const pipeline = new SessionPipeline({
      client,
      sessionId: "s1",
      viewId: "v1",
      terminal,
      uuid: () => "u",
      outputCoalesceMs: 16,
    });
    await pipeline.attach();
    pipeline.handleOutput(resizeEvent("e1", 1, 100, 30));
    await vi.advanceTimersByTimeAsync(40);
    pipeline.handleOutput(resizeEvent("e1", 2, 108, 32)); // 홀드 시작.
    // 재접속(막힘 회복 등)으로 epoch 회전 — 재생 첫 기록이 낡은 홀드에 묶이면 안 된다.
    await pipeline.attach(true);
    pipeline.handleOutput(outputEvent("e2", 1, "fresh"));
    await vi.advanceTimersByTimeAsync(0);
    expect(terminal.calls).toContain("write:fresh");
    pipeline.dispose();
  });

  it("쓰기가 진행 중인 사이 도착한 연속 resize도 마지막 것만 적용한다", async () => {
    // 파싱 완료 콜백을 쥐고 있는 터미널 — write가 끝나지 않아 pump가 막힌 상태.
    const writes: Uint8Array[] = [];
    const calls: string[] = [];
    // 클로저 안에서만 대입되는 let은 TS 흐름 분석이 null로 좁힌 채 놔두므로
    // 홀더 객체로 읽는다(속성 접근은 선언 타입을 그대로 쓴다).
    const pending: { write: (() => void) | null } = { write: null };
    const terminal: RecordingTerminal = {
      writes,
      calls,
      reset: () => calls.push("reset"),
      write: (data: Uint8Array, callback?: () => void) => {
        calls.push(`write:${new TextDecoder().decode(data)}`);
        writes.push(data);
        pending.write = callback ?? null;
      },
      resize: (cols: number, rows: number) => calls.push(`resize:${cols}x${rows}`),
      onData: () => ({ dispose: () => undefined }),
    };
    const pipeline = livePipeline(terminal);
    await pipeline.attach();

    pipeline.handleOutput(outputEvent("e1", 1, "a")); // write 진행 중
    pipeline.handleOutput(resizeEvent("e1", 2, 100, 30));
    pipeline.handleOutput(resizeEvent("e1", 3, 108, 32)); // 앞 것을 접는다
    expect(terminal.calls).toEqual(["write:a"]);

    pending.write?.(); // 파싱 완료 → 큐에 남은 것들 적용
    await vi.advanceTimersByTimeAsync(200); // 버스트 홀드가 마저 끝나면(간격 창 닫힘)
    expect(terminal.calls).toEqual(["write:a", "resize:108x32"]);
  });

  it("재생이 끝나면 버스트 표식을 비운다 — live 첫 resize는 낡은 표식에 홀드되지 않는다", async () => {
    const terminal = fakeTerminal();
    // 저널 3개(출력 - resize - 출력): 재생 중 도착한 resize가 버스트 표식을 남긴다.
    const pipeline = new SessionPipeline({
      client: fakeClient("e1", 1, 3),
      sessionId: "s1",
      viewId: "v1",
      terminal,
      uuid: () => "u",
    });
    await pipeline.attach();
    expect(pipeline.currentMode).toBe("replay");
    pipeline.handleOutput(outputEvent("e1", 1, "out"));
    pipeline.handleOutput(resizeEvent("e1", 2, 100, 30));
    pipeline.handleOutput(outputEvent("e1", 3, "end"));
    expect(pipeline.currentMode).toBe("live");

    // 같은 (가짜) 시각에 도착한 live resize - 재생 때의 표식이 살아 있다면
    // 버스트로 오해해 16ms 홀드한다. live 진입에서 비웠으므로 즉시 적용한다.
    pipeline.handleOutput(resizeEvent("e1", 4, 120, 36));
    expect(terminal.calls).toContain("resize:120x36");
  });

  it("resize 사이에 출력이 있으면 접지 않는다(출력은 그 격자에서 파싱되어야 한다)", async () => {
    const terminal = fakeTerminal();
    const pipeline = livePipeline(terminal);
    await pipeline.attach();

    pipeline.handleOutput(outputEvent("e1", 1, "a"));
    await vi.advanceTimersByTimeAsync(16); // 출력이 먼저 흐른다
    pipeline.handleOutput(resizeEvent("e1", 2, 100, 30));
    pipeline.handleOutput(outputEvent("e1", 3, "b"));
    // 직전 resize(2)와 GAP(60ms) 이상 벌어진 홀수 resize — 홀드 없이 즉시 적용.
    await vi.advanceTimersByTimeAsync(70);
    pipeline.handleOutput(resizeEvent("e1", 4, 120, 36));
    expect(terminal.calls).toEqual(["write:a", "resize:100x30", "write:b", "resize:120x36"]);
  });
});


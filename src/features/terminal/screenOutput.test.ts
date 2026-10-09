import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  SCREEN_CHECK_MS,
  SETTLE_MS,
  ScreenOutputDetector,
  screenSignature,
  type ScreenBufferSource,
} from "./screenOutput";

function fakeTerm(lines: string[], options: { type?: string; baseY?: number; rows?: number } = {}): ScreenBufferSource {
  const baseY = options.baseY ?? 0;
  return {
    rows: options.rows ?? lines.length - baseY,
    buffer: {
      active: {
        type: options.type ?? "normal",
        baseY,
        length: lines.length,
        getLine: (y) =>
          y < lines.length
            ? { translateToString: (trimRight?: boolean) => (trimRight ? lines[y].replace(/\s+$/, "") : lines[y]) }
            : undefined,
      },
    },
  };
}

describe("screenSignature — 보이는 화면의 글자", () => {
  it("사용자 스크롤 위치와 무관하게 버퍼 맨 아래 rows줄을 읽는다", () => {
    const term = fakeTerm(["old 1", "old 2", "row a", "row b"], { baseY: 2, rows: 2 });
    expect(screenSignature(term)).toBe(["normal", "4", "row a", "row b"].join("\n"));
  });

  it("줄 끝 공백은 무시하고, 대체 화면 전환은 변화로 본다", () => {
    expect(screenSignature(fakeTerm(["a   "]))).toBe(screenSignature(fakeTerm(["a"])));
    expect(screenSignature(fakeTerm(["a"], { type: "alternate" }))).not.toBe(screenSignature(fakeTerm(["a"])));
  });

  it("버퍼가 없는 터미널(node 테스트 가짜)은 null", () => {
    expect(screenSignature({})).toBeNull();
    expect(screenSignature(null)).toBeNull();
  });
});

describe("ScreenOutputDetector — 화면을 바꾼 출력만 센다", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  const timers = {
    setTimeout: (fn: () => void, ms: number) => setTimeout(fn, ms),
    clearTimeout: (handle: unknown) => clearTimeout(handle as ReturnType<typeof setTimeout>),
  };

  function setup(initial: string | null = "prompt") {
    let screen = initial;
    const onOutput = vi.fn();
    const detector = new ScreenOutputDetector(() => screen, onOutput, timers);
    return {
      detector,
      onOutput,
      setScreen: (next: string | null) => {
        screen = next;
      },
    };
  }

  it("live 전에는 세지 않고, live에서는 화면 글자가 바뀐 출력만 센다", () => {
    const { detector, onOutput, setScreen } = setup();
    setScreen("replayed");
    detector.outputApplied();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onOutput).not.toHaveBeenCalled();

    detector.rebaseline();
    // 마우스 추적 모드 재설정 같은 제어 바이트: 바이트는 왔지만 글자는 그대로다.
    detector.outputApplied();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onOutput).not.toHaveBeenCalled();

    setScreen("replayed\nanswer");
    detector.outputApplied();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onOutput).toHaveBeenCalledOnce();
  });

  it("몰려온 출력은 간격마다 한 번 비교해 스트리밍 중에도 계속 갱신한다", () => {
    const { detector, onOutput, setScreen } = setup();
    detector.rebaseline();
    for (let i = 0; i < 5; i++) {
      setScreen(`chunk ${i}`);
      detector.outputApplied();
    }
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onOutput).toHaveBeenCalledOnce();

    setScreen("chunk 5");
    detector.outputApplied();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onOutput).toHaveBeenCalledTimes(2);
  });

  it("크기 변경 뒤 다시 그리기는 세지 않고, 가라앉은 화면을 새 기준으로 삼는다", () => {
    const { detector, onOutput, setScreen } = setup();
    detector.rebaseline();
    detector.resizeApplied();
    setScreen("reflowed prompt");
    detector.outputApplied();
    vi.advanceTimersByTime(SETTLE_MS);
    expect(onOutput).not.toHaveBeenCalled();

    // 가라앉은 뒤에도 글자를 그대로 두는 바이트는 출력이 아니다…
    detector.outputApplied();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onOutput).not.toHaveBeenCalled();
    // …실제로 글자가 바뀌면 출력이다.
    setScreen("reflowed prompt\nanswer");
    detector.outputApplied();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onOutput).toHaveBeenCalledOnce();
  });

  it("입력 직후의 화면 변화(에코·마우스 휠에 따른 앱 안 스크롤)는 세지 않는다", () => {
    const { detector, onOutput, setScreen } = setup();
    detector.rebaseline();
    detector.inputSent();
    setScreen("scrolled transcript");
    detector.outputApplied();
    vi.advanceTimersByTime(SETTLE_MS);
    expect(onOutput).not.toHaveBeenCalled();

    setScreen("scrolled transcript\nnew answer");
    detector.outputApplied();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onOutput).toHaveBeenCalledOnce();
  });

  it("크기 변경·입력이 이어지면 가라앉기 창을 늘린다(레이아웃 드래그·연속 스크롤)", () => {
    const { detector, onOutput, setScreen } = setup();
    detector.rebaseline();
    detector.resizeApplied();
    vi.advanceTimersByTime(SETTLE_MS - 100);
    detector.inputSent();
    setScreen("second reaction");
    vi.advanceTimersByTime(200); // 첫 창은 지났지만 둘째 창 안
    detector.outputApplied();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onOutput).not.toHaveBeenCalled();
  });

  it("suspend(재접속 재생) 동안은 다음 rebaseline까지 세지 않는다", () => {
    const { detector, onOutput, setScreen } = setup();
    detector.rebaseline();
    setScreen("changed");
    detector.outputApplied();
    detector.suspend();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    detector.outputApplied();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onOutput).not.toHaveBeenCalled();
  });

  it("화면을 읽을 수 없는 터미널은 예전처럼 live 출력마다 센다", () => {
    const { detector, onOutput } = setup(null);
    detector.rebaseline();
    detector.outputApplied();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    detector.outputApplied();
    vi.advanceTimersByTime(SCREEN_CHECK_MS);
    expect(onOutput).toHaveBeenCalledTimes(2);
  });
});

/**
 * 마지막 출력 경과 배지의 근거: 화면 글자를 바꾼 live 출력만 "출력"으로 센다.
 *
 * PTY 바이트가 왔다고 모두 세면 배지가 늘 0초에 머문다 — 화면은 그대로인데
 * 이벤트마다 바이트가 오기 때문이다. 실제 저널에서 본 예: Claude Code는 마우스
 * 추적 모드를 다시 켜는 36바이트 제어 시퀀스(ESC ( B, SI, ESC[?1000h…1006h)를
 * 되풀이해 보냈고, 레이아웃이 바뀌면 모든 pane이 SIGWINCH를 받아 화면을 다시
 * 그렸다.
 *
 * 그래서 live 출력이 터미널에 적용된 뒤 보이는 글자(화면 서명)를 기준과 비교해
 * 달라졌을 때만 센다. 이 pane의 크기 변경이나 입력(키·마우스 휠·포커스 보고)
 * 직후에는 앱의 반응(다시 그리기·에코·앱 안 스크롤)으로 글자가 달라지므로
 * 가라앉을 때까지 세지 않고, 끝난 화면을 새 기준으로 삼는다. 그 창 안에 도착한
 * 진짜 출력은 놓칠 수 있다 — 배지는 방치된 터미널을 고르는 단서라 그 정도 오차는
 * 받아들인다.
 */

/** live 출력을 모아 화면을 한 번 비교하는 간격. 스트리밍 중에도 이 주기로 갱신된다. */
export const SCREEN_CHECK_MS = 250;
/** 크기 변경·입력 뒤 앱의 반응(다시 그리기·에코·앱 안 스크롤)이 가라앉기를 기다리는 시간. */
export const SETTLE_MS = 1500;

/** 화면 서명을 읽을 수 있는 터미널 표면 — xterm `Terminal`이 만족한다. */
export interface ScreenBufferSource {
  readonly rows?: number;
  readonly buffer?: {
    readonly active: {
      readonly type: string;
      readonly baseY: number;
      readonly length: number;
      getLine(y: number): { translateToString(trimRight?: boolean): string } | undefined;
    };
  };
}

/**
 * 보이는 화면의 글자 서명: 활성 버퍼 종류와 줄 수, 그리고 화면 줄(사용자 스크롤
 * 위치와 무관하게 버퍼 맨 아래 rows줄)의 글자. 색·커서·터미널 모드는 넣지 않는다
 * — 그것만 바꾸는 바이트는 출력이 아니다. 버퍼가 없는 터미널(node 테스트 가짜)은
 * null.
 */
export function screenSignature(term: unknown): string | null {
  const source = term as ScreenBufferSource | null | undefined;
  const active = source?.buffer?.active;
  const rows = source?.rows;
  if (!active || typeof rows !== "number" || rows <= 0) return null;
  const parts = [active.type, String(active.length)];
  for (let y = active.baseY; y < active.baseY + rows; y++) {
    parts.push(active.getLine(y)?.translateToString(true) ?? "");
  }
  return parts.join("\n");
}

export interface ScreenOutputTimers {
  setTimeout(fn: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
}

/**
 * pipeline(view) 하나의 판정기. live가 되면 `rebaseline`, live 출력이 적용될 때마다
 * `outputApplied`, live 크기 레코드가 적용되면 `resizeApplied`, 이 pane에서 입력을
 * 보내면 `inputSent`, 재생으로 돌아가면 `suspend`.
 */
export class ScreenOutputDetector {
  private baseline: string | null = null;
  private active = false;
  private checkTimer: unknown = null;
  private settleTimer: unknown = null;

  constructor(
    private readonly signature: () => string | null,
    private readonly onOutput: () => void,
    private readonly timers: ScreenOutputTimers,
  ) {}

  /** 지금 화면을 기준으로 삼고 판정을 켠다(재생이 끝나 live가 됐을 때, 화면을 지웠을 때). */
  rebaseline(): void {
    this.active = true;
    this.baseline = this.read();
  }

  /** 재생·재접속 중에는 세지 않는다 — 다음 `rebaseline`까지 판정을 끈다. */
  suspend(): void {
    this.active = false;
    this.cancelTimers();
  }

  /** live 출력 레코드가 터미널에 적용됐다. 간격마다 한 번만 비교한다. */
  outputApplied(): void {
    if (!this.active || this.checkTimer !== null) return;
    this.checkTimer = this.timers.setTimeout(() => {
      this.checkTimer = null;
      this.check();
    }, SCREEN_CHECK_MS);
  }

  /** live 크기 변경이 적용됐다 — 줄바꿈·다시 그리기가 가라앉을 때까지 세지 않는다. */
  resizeApplied(): void {
    this.settle();
  }

  /** 이 pane에서 입력(키·마우스·포커스 보고)을 보냈다 — 에코·앱 안 스크롤을 세지 않는다. */
  inputSent(): void {
    this.settle();
  }

  dispose(): void {
    this.suspend();
  }

  /** 가라앉기 창을 (다시) 연다. 창이 끝나면 그때 화면을 기준으로 삼는다. */
  private settle(): void {
    if (!this.active) return;
    if (this.settleTimer !== null) this.timers.clearTimeout(this.settleTimer);
    this.settleTimer = this.timers.setTimeout(() => {
      this.settleTimer = null;
      if (this.active) this.baseline = this.read();
    }, SETTLE_MS);
  }

  private check(): void {
    if (!this.active || this.settleTimer !== null) return;
    const current = this.read();
    // 화면을 읽을 수 없는 터미널은 예전처럼 live 출력마다 센다.
    if (current !== null && current === this.baseline) return;
    this.baseline = current;
    this.onOutput();
  }

  private read(): string | null {
    try {
      return this.signature();
    } catch {
      return null;
    }
  }

  private cancelTimers(): void {
    if (this.checkTimer !== null) this.timers.clearTimeout(this.checkTimer);
    if (this.settleTimer !== null) this.timers.clearTimeout(this.settleTimer);
    this.checkTimer = null;
    this.settleTimer = null;
  }
}

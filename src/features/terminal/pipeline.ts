/**
 * Per-view session pipeline: ordered output application, end-to-end ACK flow
 * control, replay-vs-live input gating, and resize coalescing
 * (02-runner.md §4–§6).
 *
 * Ordering discipline: output records and resize records share ONE FIFO.
 * An output record is applied via `term.write(bytes, cb)` and only when its
 * callback fires does the next record apply — a resize record therefore runs
 * strictly after prior writes and before later ones, matching the journal
 * order. ACK `through_seq` is the last *contiguously* processed record.
 *
 * Replay: after attach, records up to `last_seq` replay sequentially.
 * Terminal-generated queries and user input during replay are NOT sent to
 * the daemon (guard flag); inputs unlock only in live mode.
 *
 * Clear: 화면 지우기는 마지막으로 적용한 레코드 뒤에서 화면·스크롤백을 지우고
 * 그 seq를 기억한다. 재생(재접속)은 저널 첫 레코드부터 다시 그리므로 같은
 * 레코드를 지나는 순간 다시 지워, 지운 내용이 스크롤로 돌아오지 않는다.
 *
 * Snapshot: attach에 화면 스냅샷(seq까지 반영)을 주면 `resume_from_seq`로
 * 그 뒤부터만 재생을 요청한다. 데몬이 받아들였을 때(`replay_from_seq`가 seq+1)만
 * xterm을 비우고 스냅샷을 먼저 쓴 뒤 레코드를 잇는다. 스냅샷 쓰기가 끝나기 전에는
 * 레코드를 적용하지 않고 live로 바꾸지 않는다.
 */

import type { AttachResult } from "../../generated/AttachResult";
import type { SessionExit } from "../../generated/SessionExit";
import type { SessionOutput } from "../../generated/SessionOutput";
import type { ResizeAppliedPayload } from "../daemon/client";
import type { PipelineClient } from "../daemon/client";
import { base64ToBytes, bytesToBase64 } from "../daemon/base64";
import { INPUT_REJECTION_REASONS } from "./inputHealth";
import { ScreenOutputDetector } from "./screenOutput";
import { preserveViewport, type ViewportTerminal } from "./viewport";
import { cancelZoomPreview, zoomPreviewFor } from "./zoomPreview";

/** defaults.json limits/timings */
const ACK_COALESCE_MS = 16;
/** 잘린 헤드 재생 뒤 크기 흔들기(rows-1 → rows)의 간격. */
const RESIZE_NUDGE_MS = 120;
const ACK_BATCH_BYTES = 65536;
const INPUT_CHUNK_BYTES = 4096;
const INPUT_QUEUE_BYTES = 65536;
/**
 * 스냅샷을 xterm에 나눠 쓰는 조각 크기. xterm(6.0)은 WriteBuffer 항목 하나를
 * 한 번의 작업(task) 안에서 통째로 파싱한다 — 16 MiB 스냅샷을 한 번의 write로
 * 넘기면 메인 스레드가 수 초 묶여 그 pane뿐 아니라 같은 창의 모든 UI가
 * 얼고, "기록 재생 중…" 오버레이도 끝까지 떠 있는다. 조각 단위로 넘기면
 * 파싱이 이벤트 루프와 섞여 돈다(대기 중인 write 1개 규칙도 그대로).
 * 50 MiB 넘는 write의 동기 throw(xterm WriteBuffer 상한)도 원천적으로
 * 불가능해진다.
 */
const SNAPSHOT_WRITE_CHUNK_BYTES = 256 * 1024;
/** 재생이 이만큼 한 칸도 못 나아가면 막힌 것으로 본다 — controller가 재접속한다. */
const REPLAY_STALL_MS = 15000;
/** 재생 막힘 점검 간격. */
const REPLAY_STALL_CHECK_MS = 5000;
/**
 * attach RPC의 클라이언트 쪽 상한. 데몬 RPC 상한(5s)보다 길게 잡는다 — 정상
 * 응답은 언제나 상한 안에 도착해 이기고, 응답을 잃은 attach만 이 상한에
 * 걸려 재시도/실패 경로로 빠져나가게. 없으면 `attaching`이 영원히 풀리지
 * 않아 pane이 "기록 재생 중…"에 그대로 멈춘다.
 */
const ATTACH_TIMEOUT_MS = 15000;
/** attach 경계에서 진행 중 write의 파싱 완료를 기다리는 상한 — 콜백이 영원히
 * 안 돌아도 attach가 막히지 않게 한다(중복보다 실패가 낫다). */
const WRITE_DRAIN_TIMEOUT_MS = 250;
/** Collect the redraw following SIGWINCH into one write/paint where possible. */
const RESIZE_REDRAW_QUIET_MS = 16;
const RESIZE_REDRAW_MAX_MS = 100;
/**
 * 숨은 view(다른 탭)의 출력 표시 간격(paint throttle). 활성 탭만 그려지고
 * 숨은 pane의 xterm·WebGL 렌더러는 유지되지만(registry LRU), 그 캔버스는
 * 아무도 보지 않는다 — live 출력을 이 간격으로 묶어 초당 몇 프레임으로 낮춰
 * 보이지 않는 그리기에 드는 CPU/GPU를 아낀다. 버퍼에는 그대로 쓰므로 출력은
 * 하나도 잃지 않고, 다시 보일 때 밀린 출력을 즉시 흘려 최신 화면이 된다
 * (04-ui §4 "탭을 바꾸면 이미 열린 xterm을 유지하고 표시 빈도만 줄인다").
 */
const HIDDEN_OUTPUT_COALESCE_MS = 300;
/**
 * 프레임 붙잡기 상한. Ink 계열 TUI(Claude Code 등)는 프레임을 `ESC[?25l`(커서
 * 숨김)로 열고 `ESC[?25h`로 닫는데, PTY 읽기·저널·브리지가 그 프레임을 여러
 * 레코드로 쪼갠다. 묶음 창이 끝났을 때 마지막 커서 제어가 "숨김"이면 프레임이
 * 아직 열린 것이다 — 이 상한까지 다음 조각을 기다렸다가 한 번에 쓴다. 조각을
 * 따로 그리면 반쯤 지운 화면이 보여 새 출력마다 위아래로 떨린다.
 */
const FRAME_HOLD_MAX_MS = 80;
/**
 * 창 끌기 중 resize 기록 접기. 드래그는 16ms 간격으로 resize 기록을 저널에
 * 쌓는데 xterm resize는 스크롤백 전체를 다시 접는 비싼 작업이라 하나씩 적용하면
 * 드래그 내내 수십 번의 재접기가 메인 스레드를 누른다. GAP 안에 이어지는
 * resize 연쇄는 홀드했다가(도착마다 시한 갱신) 마지막 것만 적용한다. 사이에
 * 출력이 없는 한 격자만 바뀌므로 마지막 격자 하나로 결과가 같다. 채팅형 TUI는
 * RPC 왕복(5–15ms)마다 출력을 뿜어 고정 홀드 바로 밖에 기록이 도착하므로,
 * 연쇄가 이어지는 동안 홀드를 단계적으로 늘린다(16→24→32ms) — 드래그가 길어질
 * 수록 reflow·파싱 빈도가 낮아진다. 연쇄가 끊기면(시한 만료) 단계는 되돌아간다.
 * 오래떨어진 홀수 resize(줌 한 번)는 홀드하지 않아 지연이 0이고, 드래그 끝
 * 최종 적용도 마지막 홀드(≤32ms) 안에 끝난다.
 */
const RESIZE_BURST_GAP_MS = 60;
/** 드래그 연쇄가 이어질 때의 홀드 길이 단계(도착마다 한 단계씩, 마지막에서 고정). */
const RESIZE_HOLD_STEPS_MS: readonly number[] = [16, 24, 32, 60];
/** `ESC[?25l` — 커서 숨김(프레임 시작). */
const CURSOR_HIDE = [0x1b, 0x5b, 0x3f, 0x32, 0x35, 0x6c];
/** `ESC[?25h` — 커서 표시(프레임 끝). */
const CURSOR_SHOW = [0x1b, 0x5b, 0x3f, 0x32, 0x35, 0x68];

/** Terminal surface the pipeline needs (xterm Terminal satisfies this). */
export interface PipelineTerminal extends ViewportTerminal {
  reset?(): void;
  write(data: Uint8Array, callback?: () => void): void;
  resize(cols: number, rows: number): void;
  onData(callback: (data: string) => void): { dispose(): void };
}

interface AppliedRecord {
  seq: number;
  kind: "output" | "resize";
  bytes?: Uint8Array;
  rawLen: number;
  cols?: number;
  rows?: number;
  consumed: boolean;
}

export type PipelineMode = "detached" | "replay" | "live" | "exited";

/** attach 선택지. */
export interface AttachOptions {
  /**
   * 재생 바이트 예산(`AttachParams.max_replay_bytes`). 보존된 저널이 이보다 크면
   * 데몬이 뒤쪽 세그먼트만 재생한다 — 앞부분은 잘린 헤드처럼 다뤄 live 전환 때
   * 크기를 흔든다(TUI가 다시 그린다). 없으면 보존된 기록 전부를 재생한다.
   */
  maxReplayBytes?: number | null;
}

/** 화면 스냅샷(replaySnapshot.ts): 저널 `seq`까지 반영된 화면을 되살리는 직렬화 결과. */
export interface PipelineSnapshot {
  seq: number;
  cols: number;
  rows: number;
  data: string;
  /** 스냅샷을 뜰 때 이미 반영된 화면 지우기 지점(0이면 없음). */
  clearMark?: number;
}

export interface PipelineCallbacks {
  onLiveOutput?(): void;
  onModeChange?(mode: PipelineMode, previous: PipelineMode): void;
  onExit?(exit: SessionExit): void;
  /**
   * 롤링 저널이 앞부분을 지운 뒤의 attach(02-runner §5): 재생은 `fromSeq`
   * (세그먼트 머리의 크기 레코드)부터이고 `droppedBytes`만큼은 디스크에
   * 없다. 재생이 끝나면 pipeline이 크기를 한 번 흔들어 TUI가 전체 화면을
   * 다시 그리게 한다 — 잘린 지점 이전 화면 상태는 복원하지 않는다.
   */
  onReplayTrimmed?(droppedBytes: number, fromSeq: number): void;
  /**
   * 재생이 막혔다(레코드가 오지 않아 throughSeq가 한 칸도 못 나아갔다).
   * 어느 쪽 결함인지와 무관하게 유일한 회복은 다시 붙는 것이므로 controller가
   * 이 알림을 받아 이 pane을 다시 attach한다(스냅샷 없이 전체 재생).
   */
  onReplayStalled?(): void;
  onInputBlocked?(): void;
  onInputError?(error: Error): void;
  /** 입력 청크 하나가 데몬에 받아들여졌다 — 입력 막힘 표시를 걷는 근거. */
  onInputDelivered?(): void;
  /**
   * 이 pane이 방금 보낸 로컬 입력(키보드·IME). 동기 입력이 켜져 있으면
   * controller가 같은 입력을 형제 pane에 배분한다. sendInput()에서는
   * 부르지 않는다 — 배분받은 입력이 다시 배분되면 무한 반사가 된다.
   */
  onLocalInput?(data: string): void;
  /**
   * live 출력 텍스트(W3-5 패턴 감지용). 재생 레코드에는 부르지 않는다
   * — 재접속마다 과거 배지가 재발화되지 않게.
   */
  onOutputText?(text: string): void;
  /**
   * 화면 지우기가 레코드 `afterSeq`를 적용한 뒤에 일어났다. controller가
   * 세션별로 기억해 두면 앱을 다시 켠 뒤의 재생에서도 같은 자리에서 지운다.
   * 재생이 기억된 지점을 다시 지울 때는 부르지 않는다.
   */
  onCleared?(afterSeq: number): void;
  /**
   * 화면 글자를 바꾼 live 출력(마지막 출력 경과 배지). `onLiveOutput`과 달리
   * 마우스·포커스 모드 재설정이나 크기 변경 뒤 다시 그리기처럼 화면이 그대로인
   * 바이트는 세지 않는다 — 판정은 screenOutput.ts. `screenSignature`가 없으면
   * live 출력마다 부른다.
   */
  onScreenOutput?(): void;
}

export interface PipelineDeps {
  client: PipelineClient;
  sessionId: string;
  viewId: string;
  terminal: PipelineTerminal;
  uuid?: () => string;
  timers?: TimerHost;
  /** ConPTY can restore the input cursor just after a synchronized frame ends. */
  outputCoalesceMs?: number;
  /**
   * 화면·스크롤백 지우기(terminalClear.clearTerminalHistory). 없으면 화면
   * 지우기는 아무것도 하지 않는다.
   */
  clearTerminal?: () => void;
  /** 이전에 지운 지점(레코드 seq) — 재생이 그 레코드를 지나면 다시 지운다. */
  clearAfterSeq?: number | null;
  /** 보이는 화면의 글자 서명(screenOutput.screenSignature). 없으면 화면 비교 없이 센다. */
  screenSignature?: () => string | null;
  /**
   * 잘린 헤드부터 재생한 화면을 live 전환 때 지우고 폭을 흔들어 다시 그리게
   * 할지(에이전트 CLI가 붙은 pane). 없거나 false면 높이만 흔든다.
   */
  repaintTrimmedReplay?: () => boolean;
}

/** Test seam for timer control. */
export interface TimerHost {
  setTimeout(fn: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
}

const defaultTimers: TimerHost = {
  // Resolve globals lazily so test doubles (vi.useFakeTimers) take effect.
  setTimeout: (fn, ms) => globalThis.setTimeout(fn, ms),
  clearTimeout: (handle) => globalThis.clearTimeout(handle as ReturnType<typeof setTimeout>),
};

export class SessionPipeline {
  readonly sessionId: string;
  readonly viewId: string;

  private mode: PipelineMode = "detached";
  private exited = false;
  private attaching = false;
  private attachRun: Promise<AttachResult> | null = null;
  private earlyEvents: SessionOutput[] = [];
  private epoch: string | null = null;
  /** attach마다 증가 — 이전 attach의 xterm write 콜백을 무시하는 기준. */
  private generation = 0;
  private replayTargetSeq = 0;
  /** xterm에 넘겼으나 파싱 콜백이 아직 돌지 않은 write 수. attach 경계에서 이것이
   * 0이 되기를 기다린다 — terminal.reset()은 대기 중인 write 입력을 취소하지
   * 못하므로(xterm 계약), 다 파싱되기 전에 초기화하면 이전 epoch의 낡은 바이트가
   * 스냅샷·재생이 그린 화면 위에 다시 찍혀 같은 내용이 두 번 보인다. */
  private inflightWrites = 0;
  private writeDrainWaiters: Array<() => void> = [];
  /** 이번 attach가 잘린 헤드부터 재생 중이다 — live 전환 때 크기를 흔든다. */
  private replayTrimmed = false;
  private nextSeq = 1; // next journal seq we accept on the stream
  private received = new Map<number, AppliedRecord>();
  private pending: AppliedRecord[] = [];
  /**
   * `pending`에 남은 크기 레코드 수. 크기 레코드는 FIFO 장벽이라 앞선 출력이
   * 열어 둔 묶음 창 뒤에서 기다리면 격자 변경이 그만큼 늦는다 — 매번 큐를
   * 훑지 않고 O(1)로 판정하려고 센다(push/shift/비우기에서만 움직인다).
   */
  private pendingResizes = 0;
  /** 드래그 홀드 중인 resize 적용의 시한(도착마다 갱신). */
  private resizeHoldTimer: unknown = null;
  /** 마지막 resize 기록 도착 시각(버스트 판정). 0이면 버스트 아님. */
  private lastResizeArrivalAt = 0;
  /** 이번 resize가 짧은 간격의 연쇄(드래그) 안에 있는가(직전 도착 기준). */
  private resizeBurstActive = false;
  /** 현재 연쇄의 홀드 단계(RESIZE_HOLD_STEPS_MS 인덱스). 연쇄가 끊기면 0으로. */
  private resizeHoldStep = 0;
  private applying = false;
  private outputTimer: unknown = null;
  /** 지금 묶음 창이 열린 시각 — 프레임 붙잡기 상한(FRAME_HOLD_MAX_MS)의 기준. */
  private batchStartedAt = 0;
  private resizeRedrawPending = false;
  private resizeRedrawTimer: unknown = null;
  private readonly outputCoalesceMs: number;
  /**
   * 이 view가 숨겨졌는가(다른 탭). 숨은 동안 출력 coalesce 창을 넓혀
   * write/paint 빈도를 낮춘다 — 버퍼 적용은 그대로라 출력은 잃지 않는다.
   */
  private hidden = false;
  private throughSeq = 0;
  private ackBytes = 0;
  private ackTimer: unknown = null;
  private resizeInFlight = false;
  private lastSentResize: { cols: number; rows: number } | null = null;
  private proposedResize: { cols: number; rows: number } | null = null;
  private readonly input: InputQueue;
  private readonly decoder = new TextDecoder("utf-8", { fatal: false });
  private dataSub: { dispose(): void } | null = null;
  private disposed = false;
  /** 마지막 화면 지우기 지점(이 레코드까지 적용한 뒤 지웠다). 0이면 없음. */
  private clearAfterSeq = 0;
  /** write가 진행 중일 때 누른 화면 지우기 — 그 write가 끝나면 지운다. */
  private clearQueued = false;
  /** 스냅샷을 xterm에 쓰는 중이다 — 끝나기 전에는 live로 바꾸지 않는다. */
  private restoring = false;
  /** 이번 epoch에 적용한 재생 레코드 바이트(스냅샷을 새로 뜰지 판단용). */
  private replayBytes = 0;
  /**
   * 다음 재생 레코드/스냅샷 조각을 쓰기 직전에 내릴 터미널 초기화. attach 응답
   * 바로 뒤에 reset하면 첫 바이트가 도착할 때까지 빈 화면이 남는다 — 초기화
   * 시점을 첫 쓰기 직전으로 미룬다. 이 epoch에 적용할 레코드가 없으면(빈 저널)
   * 소비되지 않고 지난 화면이 그대로 남는데, 의도된 동작이다 — 지난 화면은
   * 같은 세션의 마지막 상태라 지울 것이 없다.
   */
  private pendingReset = false;
  /** 재생 막힘 점검 타이머 — replay 모드에서만 돈다. */
  private stallTimer: unknown = null;
  /** 재생이 마지막으로 앞으로 나아간 시각(레코드 소비·스냅샷 조각 완료). */
  private lastProgressAt = 0;

  constructor(
    deps: PipelineDeps,
    private readonly callbacks: PipelineCallbacks = {},
  ) {
    this.sessionId = deps.sessionId;
    this.outputCoalesceMs = deps.outputCoalesceMs ?? 0;
    this.viewId = deps.viewId;
    const uuid = deps.uuid ?? (() => crypto.randomUUID());
    const timers = deps.timers ?? defaultTimers;
    this.input = new InputQueue(
      async (bytes) => {
        if (this.epoch === null) throw new Error("not attached");
        // 응답(InputResult)을 그대로 돌려준다 — InputQueue가 Queued(아직 안
        // 쓰였음)를 가려 '전달' 신호를 조건화한다.
        return deps.client.sessionInput({
          session_id: this.sessionId,
          epoch: this.epoch,
          input_id: uuid(),
          data_b64: bytesToBase64(bytes),
        });
      },
      (error) => this.callbacks.onInputError?.(toError(error)),
      () => this.callbacks.onInputDelivered?.(),
    );
    this.timers = timers;
    this.uuidFn = uuid;
    this.client = deps.client;
    this.terminal = deps.terminal;
    this.clearTerminal = deps.clearTerminal ?? null;
    this.repaintTrimmedReplay = deps.repaintTrimmedReplay ?? null;
    if (deps.clearAfterSeq && Number.isSafeInteger(deps.clearAfterSeq) && deps.clearAfterSeq > 0) {
      this.clearAfterSeq = deps.clearAfterSeq;
    }
    this.screen = new ScreenOutputDetector(
      deps.screenSignature ?? (() => null),
      () => this.callbacks.onScreenOutput?.(),
      timers,
    );
    this.dataSub = deps.terminal.onData((data) => this.handleTerminalData(data));
  }

  private readonly timers: TimerHost;
  /** 마지막 출력 배지용 "화면을 바꾼 출력" 판정. */
  private readonly screen: ScreenOutputDetector;
  private readonly uuidFn: () => string;
  private readonly client: PipelineClient;
  private readonly terminal: PipelineTerminal;
  private readonly clearTerminal: (() => void) | null;
  private readonly repaintTrimmedReplay: (() => boolean) | null;

  get currentMode(): PipelineMode {
    return this.mode;
  }

  get currentEpoch(): string | null {
    return this.epoch;
  }

  get ackedThrough(): number {
    return this.throughSeq;
  }

  /** 마지막 화면 지우기 지점(레코드 seq). 없으면 0. */
  get clearMark(): number {
    return this.clearAfterSeq;
  }

  /** 이번 attach가 재생으로 xterm에 쓴 저널 바이트(스냅샷 제외). */
  get replayedBytes(): number {
    return this.replayBytes;
  }

  /**
   * 지금 화면을 스냅샷으로 떠도 되면 그 화면에 반영된 마지막 seq, 아니면 null.
   * 레코드 경계에서만 뜬다: 쓰는 중·스냅샷 복원 중·대기 레코드가 있으면 화면이
   * 어느 seq와도 정확히 맞지 않는다. 재생 중에도 뜨지 않는다(곧 끝난다).
   */
  get snapshotSeq(): number | null {
    if (this.disposed || this.attaching || this.applying || this.restoring || this.clearQueued) return null;
    if (this.mode !== "live" && this.mode !== "exited") return null;
    if (this.pending.length > 0 || this.received.size > 0 || this.outputTimer !== null) return null;
    return this.throughSeq > 0 ? this.throughSeq : null;
  }

  /**
   * 이 view가 숨겨졌는지(다른 탭) 알린다(paint throttle, 04-ui §4). 숨은 동안
   * live 출력 coalesce 창을 HIDDEN_OUTPUT_COALESCE_MS로 넓혀 write/paint를
   * 초당 몇 프레임으로 묶는다 — 버퍼에는 그대로 쓰므로 출력은 하나도 잃지
   * 않는다. 다시 보이면 넓은 창의 대기 타이머를 접고 밀린 출력을 즉시 흘려
   * 방금 열린 탭이 곧바로 최신 화면이 되게 한다(재부착과 짝을 이룬다).
   * 출력 라우팅은 바꾸지 않는다 — 숨은 view도 계속 받아 버퍼에 쌓는다.
   */
  setHidden(hidden: boolean): void {
    if (this.disposed || this.hidden === hidden) return;
    this.hidden = hidden;
    if (!hidden) {
      // 다시 보임: 넓은 창의 대기 타이머를 접고 밀린 출력을 즉시 적용한다.
      // 마침 쓰기가 진행 중이면 그 완료 콜백이 이제 좁은 창으로 이어 받는다.
      this.clearOutputTimer();
      this.pump(true);
    }
  }

  /**
   * 화면 지우기: 지금까지 터미널에 넘긴 출력을 화면·스크롤백에서 지운다.
   * write가 진행 중이면 묶음(coalescing) 전체가 적용된 직후에 지워 이미 넘긴
   * 출력까지 함께 지운다 — 다음 기록은 아직 넘기지 않았다.
   */
  clearScreen(): void {
    if (this.disposed || !this.clearTerminal) return;
    if (this.applying) {
      this.clearQueued = true;
      return;
    }
    this.applyClear(this.throughSeq);
  }

  private applyClear(afterSeq: number): void {
    this.clearTerminal?.();
    // 지운 빈 화면이 새 기준 — 뒤따르는 제어 바이트가 "화면이 바뀌었다"로 세지지 않게.
    if (this.mode === "live") this.screen.rebaseline();
    this.rememberClear(afterSeq);
  }

  private rememberClear(afterSeq: number): void {
    if (afterSeq <= this.clearAfterSeq) return;
    this.clearAfterSeq = afterSeq;
    this.callbacks.onCleared?.(afterSeq);
  }

  /**
   * Attach (or re-attach) with a fresh epoch. Journal replay follows as
   * ordered session.output events carrying the new epoch. Events that arrive
   * while the attach RPC is in flight (eager transports deliver replay
   * records before the reply lands) are buffered and replayed after the
   * epoch rotates.
   */
  attach(
    resetTerminal = false,
    snapshot: PipelineSnapshot | null = null,
    options: AttachOptions = {},
  ): Promise<AttachResult> {
    // Transport recovery and the replay watchdog can request the same attach.
    // Two RPCs would rotate the daemon epoch twice and discard each other's
    // early output, leaving a permanent gap in the replay stream.
    if (this.attachRun) return this.attachRun;
    if (this.disposed) return Promise.reject(new Error("pipeline disposed"));
    this.attachRun = this.performAttach(resetTerminal, snapshot, options).finally(() => {
      this.attachRun = null;
    });
    return this.attachRun;
  }

  /** attach 요청이 진행 중인가 — controller가 막힘 재시도 중복을 피하려고 쓴다. */
  get isAttachPending(): boolean {
    return this.attachRun !== null;
  }

  private async performAttach(
    resetTerminal: boolean,
    snapshot: PipelineSnapshot | null,
    options: AttachOptions,
  ): Promise<AttachResult> {
    this.attaching = true;
    this.stopStallWatch();
    const resume = snapshot && Number.isSafeInteger(snapshot.seq) && snapshot.seq > 0 ? snapshot : null;
    const budget = options.maxReplayBytes;
    const maxReplay = typeof budget === "number" && Number.isSafeInteger(budget) && budget > 0 ? budget : null;
    let result: AttachResult;
    try {
      // 응답을 잃은 RPC(재시작 직후 브리지 등)는 이 프라미스를 영원히 묶어
      // 둔다 — 상한을 걸어 재시도/실패 경로로 빠져나가게 한다. settle 가드로
      // 타임아웃 승리 뒤 늦게 도착한 응답은 무시한다(이미 끝난 attach를
      // 되살리지 않는다).
      result = await new Promise<AttachResult>((resolve, reject) => {
        let settled = false;
        const timeout = this.timers.setTimeout(() => {
          if (settled) return;
          settled = true;
          reject(new Error("session attach timed out"));
        }, ATTACH_TIMEOUT_MS);
        this.client
          .sessionAttach({
            session_id: this.sessionId,
            view_id: this.viewId,
            access: "writer",
            ...(resume ? { resume_from_seq: String(resume.seq + 1) } : {}),
            ...(maxReplay ? { max_replay_bytes: String(maxReplay) } : {}),
          })
          .then(
            (reply) => {
              if (settled) return;
              settled = true;
              this.timers.clearTimeout(timeout);
              resolve(reply);
            },
            (error: unknown) => {
              if (settled) return;
              settled = true;
              this.timers.clearTimeout(timeout);
              reject(error);
            },
          );
      });
      // Closing a pane while its RPC is pending must not reset a disposed
      // terminal or revive the pane through onModeChange.
      if (this.disposed) return result;
      // 진행 중인 write의 파싱이 끝나기를 기다린다 — reset은 대기 중인 write
      // 입력을 취소하지 못하므로(xterm 계약), 초기화가 먼저 나면 이전 epoch의
      // 낡은 바이트가 새 화면에 다시 찍혀 같은 내용이 두 번 보인다.
      await this.writeDrained();
      if (this.disposed) return result;
      // An exit event can arrive while the attach reply is in flight.
      this.exited ||= result.exited === true;
      const fromSeq = Number(result.replay_from_seq);
      // 데몬이 스냅샷 바로 다음부터 재생할 때만 스냅샷을 쓴다. 다르면(저널 앞부분이
      // 잘렸거나 필드를 모르는 구 데몬) 스냅샷을 버리고 평소처럼 전체 재생한다.
      const restore = resume && fromSeq === resume.seq + 1 ? resume : null;
      this.rotateEpoch(result.epoch, fromSeq, Number(result.last_seq));
      // 첫 재생 기록/스냅샷 조각이 실제로 쓰이기 직전에 초기화한다 — attach
      // 응답과 첫 바이트 사이의 빈 화면을 없앤다. attach마다 무조건 덮어
      // 쓰므로 이전 epoch의 흔적(true)이 남지 않는다.
      this.pendingReset = resetTerminal || restore !== null;
      const dropped = result.replay_dropped_bytes ? Number(result.replay_dropped_bytes) : 0;
      // 스냅샷은 잘린 지점 이전 화면까지 담고 있다 — 잘림을 알리거나 크기를 흔들지 않는다.
      this.replayTrimmed = !restore && (dropped > 0 || fromSeq > 1);
      if (this.replayTrimmed) {
        this.callbacks.onReplayTrimmed?.(dropped, fromSeq);
      }
      if (restore) this.applySnapshot(restore);
    } catch (error) {
      this.earlyEvents = [];
      throw error;
    } finally {
      this.attaching = false;
    }
    const early = this.earlyEvents;
    this.earlyEvents = [];
    for (const event of early) this.handleOutput(event);
    // 빈 저널(last_seq == replay_from_seq - 1)은 재생할 레코드가 없어 pump가
    // 돌지 않는다 — 여기서 판정하지 않으면 첫 출력이 올 때까지 입력이 막힌다.
    this.checkLive();
    this.flushResize();
    return result;
  }

  /**
   * 스냅샷 화면을 쓴다: 직렬화할 때의 격자로 맞춘 뒤 조각(256 KiB)마다 한 번씩
   * write한다. xterm은 WriteBuffer 항목 하나를 한 작업 안에서 통째로 파싱하므로
   * 한 번에 쓰면 큰 스냅샷이 메인 스레드를 수 초 묶는다 — 조각 사이에 이벤트
   * 루프가 끼어 UI가 살아 있는다. 뒤따르는 레코드는 `applying`에 막혀 대기열에
   * 쌓이고, 마지막 조각이 끝나면 순서대로 이어진다. 스냅샷은 저널 레코드가
   * 아니므로 ACK·throughSeq에 세지 않는다.
   */
  private applySnapshot(snapshot: PipelineSnapshot): void {
    const generation = this.generation;
    this.restoring = true;
    this.applying = true;
    if (snapshot.cols > 0 && snapshot.rows > 0) this.terminal.resize(snapshot.cols, snapshot.rows);
    // 미뤄 둔 초기화는 스냅샷 격자로 맞춘 뒤, 첫 조각을 쓰기 직전에 내린다 —
    // 격자와 조각 사이에 빈 화면이 끼지 않게.
    this.consumePendingReset();
    const bytes = new TextEncoder().encode(snapshot.data);
    const writeChunk = (offset: number): void => {
      if (this.disposed || generation !== this.generation) return;
      if (offset >= bytes.length) {
        this.applying = false;
        this.restoring = false;
        this.lastProgressAt = Date.now();
        // 스냅샷을 뜬 뒤 같은 seq에서 화면을 지웠다면 스냅샷에는 지우기 전 화면이
        // 있다. 그 뒤 지점은 재생이 지나며 지우지만, 이 지점은 재생되지 않으니
        // 여기서 지운다.
        if (this.clearAfterSeq === snapshot.seq && (snapshot.clearMark ?? 0) < this.clearAfterSeq) {
          this.clearTerminal?.();
        }
        this.pump();
        return;
      }
      const end = Math.min(offset + SNAPSHOT_WRITE_CHUNK_BYTES, bytes.length);
      // 대기 중인 write는 항상 한 조각뿐이라 xterm의 50 MiB 동기 throw에 닿지
      // 않는다. 그래도 write가 동기적으로 던지면(구형 빌드 등) 상태를 풀고
      // 예외를 되살려 attach가 실패하게 한다 — 반쯤 복원된 화면에 레코드를
      // 잇는 것보다 실패 오버레이(재시도 있음)가 낫다.
      try {
        this.terminal.write(bytes.subarray(offset, end), () => {
          this.lastProgressAt = Date.now();
          writeChunk(end);
        });
      } catch (error) {
        this.applying = false;
        this.restoring = false;
        throw error;
      }
    };
    writeChunk(0);
  }

  /** 진행 중인 write가 모두 파싱될 때까지 기다린다. 없으면 즉시 끝난다. */
  private writeDrained(): Promise<void> {
    if (this.inflightWrites === 0 || this.disposed) return Promise.resolve();
    return new Promise<void>((resolve) => {
      let settle!: () => void;
      const timer = this.timers.setTimeout(() => {
        const at = this.writeDrainWaiters.indexOf(settle);
        if (at >= 0) this.writeDrainWaiters.splice(at, 1);
        resolve();
      }, WRITE_DRAIN_TIMEOUT_MS);
      settle = () => {
        this.timers.clearTimeout(timer);
        resolve();
      };
      this.writeDrainWaiters.push(settle);
    });
  }

  private notifyWriteDrained(): void {
    if (this.inflightWrites > 0) return;
    const waiters = this.writeDrainWaiters;
    this.writeDrainWaiters = [];
    for (const resolve of waiters) resolve();
  }

  /** 미뤄 둔 터미널 초기화를 지금 내린다(요청이 없으면 아무것도 하지 않는다). */
  private consumePendingReset(): void {
    if (!this.pendingReset) return;
    this.pendingReset = false;
    this.terminal.reset?.();
  }

  private rotateEpoch(epoch: string, fromSeq: number, lastSeq: number): void {
    cancelZoomPreview(this.terminal);
    // 이전 epoch에서 write 도중 누른 화면 지우기는 그 epoch가 적용한 곳까지로
    // 확정한다 — 화면은 곧 재생이 다시 그리고, 재생이 그 지점에서 지운다.
    if (this.clearQueued) {
      this.clearQueued = false;
      this.rememberClear(this.throughSeq);
    }
    this.epoch = epoch;
    this.generation += 1;
    this.lastSentResize = null;
    this.replayTargetSeq = lastSeq;
    this.nextSeq = fromSeq;
    this.received.clear();
    this.pending = [];
    this.pendingResizes = 0;
    this.clearOutputTimer();
    this.clearResizeRedraw();
    // 이전 epoch의 드래그 홀드도 버린다 — 남아 있으면 새 epoch의 재생 기록을
    // 홀드 시한(최대 60 ms)만큼 밀고, 재생된 저널의 resize 기록이 낡은 홀드를
    // 계속 갱신하며 재생을 지연시킨다(dispose의 clearResizeHold와 같은 정리).
    this.clearResizeHold();
    this.lastResizeArrivalAt = 0;
    this.resizeBurstActive = false;
    this.resizeHoldStep = 0;
    this.applying = false;
    this.restoring = false;
    this.replayBytes = 0;
    // 다시 붙어 재생이 이어지는 경우 setMode가 같은 모드라 일찍 돌아오므로
    // 여기서 진행 시각을 갱신해 막힘 판정의 기준을 새 epoch으로 맞춘다.
    this.lastProgressAt = Date.now();
    // 막힘 점검도 새 epoch에서 다시 돈다 — 재생 중 재접속(막힘 회복 포함)은
    // 모드가 그대로 "replay"라 setMode의 시작 지점을 지나치지 않는다.
    this.startStallWatch();
    // 계약(session.rs): replay_from_seq는 새 세션에서 1이지만 더 클 수 있다.
    // 그 앞은 이미 소비된 것으로 두어야 ACK와 live 전환이 이어진다.
    this.throughSeq = fromSeq - 1;
    this.ackBytes = 0;
    this.clearAckTimer();
    this.setMode("replay");
  }

  private setMode(mode: PipelineMode): void {
    if (this.mode === mode) return;
    const previous = this.mode;
    this.mode = mode;
    // 재생 중 도착한 resize 기록이 남긴 버스트 표식은 live의 홀드 판정에
    // 무의미하다 — 그대로 두면 재생 직후 첫 live resize가 낡은 표식에 이끌려
    // 홀드 홀드 없이도 될 것을 한 번 16ms 기다린다. live 진입에서 비운다.
    if (mode === "live") {
      this.lastResizeArrivalAt = 0;
      this.resizeBurstActive = false;
    }
    // live: 재생이 그린 화면을 기준으로 삼고 판정을 켠다. 재생 중에는 세지 않는다.
    if (mode === "live") this.screen.rebaseline();
    else this.screen.suspend();
    this.callbacks.onModeChange?.(mode, previous);
    // 재생 동안에만 막힘을 지켜본다 — 레코드가 끊기면 아무도 이 pipeline을
    // 살리지 못한다(연결은 살아 있을 수 있다). live·exited로 나가면 접는다.
    if (mode === "replay") this.startStallWatch();
    else this.stopStallWatch();
    if (mode === "live" && this.replayTrimmed) {
      this.replayTrimmed = false;
      this.repaintAfterTrimmedReplay();
    }
  }

  /** 재생 막힘 점검을 시작한다(이미 돌고 있으면 다시 스케줄하지 않는다). */
  private startStallWatch(): void {
    if (this.stallTimer !== null) return;
    const tick = (): void => {
      this.stallTimer = null;
      if (this.disposed || this.mode !== "replay") return;
      if (Date.now() - this.lastProgressAt >= REPLAY_STALL_MS) {
        // 알린 뒤에도 점검을 잇는다 — controller가 재시도 횟수(strikes)를 세어
        // 실패 오버레이로 끝내므로, 회복이 조용히 실패해도(재시도가 이미 걸린
        // attach에 묶여 아무 일도 하지 않으면) 와치독이 계속 알린다.
        this.callbacks.onReplayStalled?.();
        // 콜백 안에서 곧바로 다시 붙어(rotateEpoch → startStallWatch) 새 점검이
        // 이미 시작됐으면 이 틱은 물러난다 — 스케줄을 덮어 쓰면 새 타이머가
        // 유실된다.
        if (this.stallTimer !== null) return;
      }
      this.stallTimer = this.timers.setTimeout(tick, REPLAY_STALL_CHECK_MS);
    };
    this.stallTimer = this.timers.setTimeout(tick, REPLAY_STALL_CHECK_MS);
  }

  private stopStallWatch(): void {
    if (this.stallTimer !== null) {
      this.timers.clearTimeout(this.stallTimer);
      this.stallTimer = null;
    }
  }

  /**
   * 잘린 헤드부터 재생한 화면은 잘린 지점 이전 상태를 모른다 — 크기를 흔들어
   * (SIGWINCH 두 번) 프로그램이 다시 그리게 한다. 실제 xterm 크기 변경은 여느
   * resize처럼 순서 있는 저널 레코드를 따른다. 아직 fit이 없었으면 건너뛴다:
   * 곧 올 첫 fit이 같은 일을 한다.
   *
   * 에이전트 CLI(Claude Code 2.1 등 일반 화면의 인라인 렌더러)는 SIGWINCH에도
   * 화면 전체를 지우지 않고 커서 기준으로 자기 영역만 지워 다시 그리며, 높이를
   * 되돌리는 쪽에는 아무것도 내보내지 않는다(실측). 그래서 높이 흔들기로는
   * 잘린 꼬리가 남긴 어긋난 프레임이 그대로 남는다 — 이 창의 화면·스크롤백을
   * 지운 뒤(셸에는 아무것도 보내지 않는다) 두 번 모두 다시 그리는 폭을 흔든다.
   * 셸은 재생된 출력 자체가 쓸모 있으니 지우지 않고 높이만 흔든다.
   */
  private repaintAfterTrimmedReplay(): void {
    const size = this.proposedResize ?? this.lastSentResize;
    if (!size) return;
    const repaint = this.repaintTrimmedReplay?.() === true;
    if (repaint ? size.cols <= 1 : size.rows <= 1) return;
    if (repaint) {
      this.applyClear(this.throughSeq);
      this.requestResize(size.cols - 1, size.rows);
    } else {
      this.requestResize(size.cols, size.rows - 1);
    }
    this.timers.setTimeout(() => {
      if (!this.disposed) this.requestResize(size.cols, size.rows);
    }, RESIZE_NUDGE_MS);
  }

  /** Route a `session.output` event (output or resize record). */
  handleOutput(event: SessionOutput): void {
    if (this.disposed) return;
    if (this.attaching) {
      this.earlyEvents.push(event);
      return;
    }
    if (event.epoch !== this.epoch) return; // 이전 epoch 무시
    const seq = Number(event.seq);
    if (seq < this.nextSeq) return; // duplicate retransmit
    if (!this.received.has(seq) && seq > this.replayTargetSeq && event.kind === "output" && event.raw_len > 0) {
      this.callbacks.onLiveOutput?.();
    }
    const record: AppliedRecord = {
      seq,
      kind: event.kind,
      bytes: event.kind === "output" ? base64ToBytes(event.data_b64) : undefined,
      rawLen: event.raw_len,
      cols: event.cols ?? undefined,
      rows: event.rows ?? undefined,
      consumed: false,
    };
    this.received.set(seq, record);
    zoomPreviewFor(this.terminal)?.outputPending();
    this.drainReceived();
    // 크기 레코드는 FIFO 장벽이다. 앞선 조각이 열어 둔 묶음 창(16ms, Windows
    // 32ms) 뒤에서 기다리면 격자 변경이 딱 그만큼 늦는다 — 출력 중인 pane을
    // 줌하거나 창을 끌 때의 체감 지연이라 장벽이 들어오면 창을 곧바로 닫는다.
    if (this.outputTimer !== null && this.pendingResizes > 0) {
      this.clearOutputTimer();
      this.pump(true);
      return;
    }
    if (this.resizeRedrawPending && !zoomPreviewFor(this.terminal) && this.outputTimer !== null) {
      // Wait for the burst to go quiet, with a fixed maximum below.
      this.clearOutputTimer();
    }
    this.pump();
  }

  /** Control-side notification that a resize record landed (same FIFO). */
  handleResizeApplied(event: ResizeAppliedPayload): void {
    // The resize record itself arrives as a kind:'resize' session.output
    // frame; this notification only confirms the RPC. Nothing to apply here.
    void event;
  }

  handleExit(event: SessionExit): void {
    if (this.disposed) return;
    this.markExited();
    this.callbacks.onExit?.(event);
  }

  /**
   * The caller already knows the process finished (its workload ended). Replay
   * then ends in `exited` instead of `live` even when the attach reply carries
   * no `exited` (daemons built before that field), so a dead PTY never looks
   * writable and whatever waits for the replay to end still runs.
   */
  markExited(): void {
    if (this.disposed) return;
    this.exited = true;
    if (this.mode === "live") this.setMode("exited");
  }

  private drainReceived(): void {
    while (this.received.has(this.nextSeq)) {
      const rec = this.received.get(this.nextSeq) as AppliedRecord;
      this.received.delete(this.nextSeq);
      this.nextSeq++;
      if (rec.kind === "resize") {
        this.pendingResizes++;
        if ((rec.cols ?? 0) > 0 && (rec.rows ?? 0) > 0) {
          // 바로 앞에 아직 적용하지 않은 resize가 있으면 겹쳐진 것이다 — 이번
          // 것이 같은 효과를 낸다(사이에 출력이 없다). 소비만 선포하고 적용은
          // 이번 것에 맡긴다(throughSeq 전진·ACK는 onConsumed가 맡는다).
          const tail = this.pending[this.pending.length - 1];
          if (
            tail?.kind === "resize" && !tail.consumed &&
            (tail.cols ?? 0) > 0 && (tail.rows ?? 0) > 0
          ) {
            this.onConsumed(tail);
          }
          // 버스트는 **직전** resize 도착과의 간격으로 판정한다 — 이번 기록의
          // 도착을 포함하면 첫 resize도 항상 홀드되어 홀수 resize(줌 한 번)까지
          // 늦어진다. 첫 단계는 즉시 적용하고 뒤따르는 연쇄만 접는다.
          const arrivedAt = Date.now();
          this.resizeBurstActive =
            this.lastResizeArrivalAt > 0 &&
            arrivedAt - this.lastResizeArrivalAt < RESIZE_BURST_GAP_MS;
          this.lastResizeArrivalAt = arrivedAt;
          this.renewResizeHold();
        }
      }
      this.pending.push(rec);
    }
  }

  /**
   * 홀드가 이미 돌고 있으면 시한을 다시 잰다 — 연쇄가 이어지는 한 적용을 미룬다.
   * 이어질 때마다 홀드를 한 단계 늘려 마지막 단계는 버스트 간격(60 ms)과 같게
   * 둔다 — 연쇄 판정 간격보다 홀드가 짧으면(예: 32 ms) 33–59 ms 간격의 드래그
   * 단계가 매번 "새 버스트"로 분류돼 접히지 않고 중간 격자가 그려진다. 단,
   * 연쇄 시작의 첫 홀드는 짧게 둔다(짧은 드래그가 불필요하게 늘어지지 않게).
   */
  private renewResizeHold(): void {
    if (this.resizeHoldTimer === null) return;
    this.resizeHoldStep = Math.min(this.resizeHoldStep + 1, RESIZE_HOLD_STEPS_MS.length - 1);
    this.timers.clearTimeout(this.resizeHoldTimer);
    this.resizeHoldTimer = this.timers.setTimeout(
      () => this.flushResizeHold(),
      RESIZE_HOLD_STEPS_MS[this.resizeHoldStep],
    );
  }

  private flushResizeHold(): void {
    this.resizeHoldTimer = null;
    // 간격 창(60 ms)이 아직 열려 있으면 연쇄가 끝났다는 증거가 없다 — 창이
    // 닫히는 순간까지만 홀드를 이어간다. 이어감이 없으면 33–59 ms 간격의 드래그
    // 단계가 매번 "새 버스트"로 풀려 중간 격자가 그려진다.
    const quietFor = Date.now() - this.lastResizeArrivalAt;
    if (this.lastResizeArrivalAt > 0 && quietFor < RESIZE_BURST_GAP_MS) {
      this.resizeHoldTimer = this.timers.setTimeout(
        () => this.flushResizeHold(),
        RESIZE_BURST_GAP_MS - quietFor,
      );
      return;
    }
    // 버스트를 끝낸다(다음 resize 도착이 새 버스트를 연다).
    this.lastResizeArrivalAt = 0;
    this.resizeBurstActive = false;
    this.resizeHoldStep = 0;
    this.pump();
  }

  private clearResizeHold(): void {
    if (this.resizeHoldTimer !== null) this.timers.clearTimeout(this.resizeHoldTimer);
    this.resizeHoldTimer = null;
  }

  /** `pending.shift()` + 장벽 카운터 유지. 큐에서 빼는 곳은 전부 이걸 쓴다. */
  private shiftPending(): AppliedRecord | undefined {
    const record = this.pending.shift();
    if (record?.kind === "resize") this.pendingResizes--;
    return record;
  }

  private pump(coalesced = false): void {
    if (this.attaching || this.applying || this.disposed || this.outputTimer !== null) return;
    // 드래그 홀드 중에는 선두 resize(과 그 뒤 전부)를 붙잡는다 — 시한이 지나면
    // flushResizeHold가 이 pump를 다시 돌린다.
    if (this.resizeHoldTimer !== null) return;
    // live 버스트의 첫 resize는 잠깐 홀드해 이어지는 drag 단계와 접는다.
    // 재생은 즉시(과거 저널을 되살리는 길이라 지연이 체감된다), 홀수 resize도 즉시.
    if (this.mode === "live" && this.resizeBurstActive) {
      const head = this.pending[0];
      if (
        head?.kind === "resize" && !head.consumed &&
        (head.cols ?? 0) > 0 && (head.rows ?? 0) > 0
      ) {
        this.resizeHoldStep = 0;
        this.resizeHoldTimer = this.timers.setTimeout(
          () => this.flushResizeHold(),
          RESIZE_HOLD_STEPS_MS[0],
        );
        return;
      }
    }
    // Ordinary output uses a fixed window. A resize redraw waits briefly for
    // quiet, bounded by a separate deadline. Replay remains immediate and
    // resize records remain FIFO barriers.
    // The preview already covers incomplete redraws, so while it is up the
    // batching window buys nothing visually — parse/ACK immediately instead of
    // adding its delay to every redraw chunk. Without a cover the window stays
    // (plus the redraw-quiet extension) so partial frames never flash.
    const covered = !!zoomPreviewFor(this.terminal);
    const waitForRedraw = this.resizeRedrawPending && !covered;
    // 숨은 view는 표시 간격을 넓혀 write/paint를 묶는다(버퍼에는 그대로 쓴다).
    const baseCoalesceMs = this.hidden
      ? Math.max(this.outputCoalesceMs, HIDDEN_OUTPUT_COALESCE_MS)
      : this.outputCoalesceMs;
    const coalesceMs = waitForRedraw
      ? Math.max(baseCoalesceMs, RESIZE_REDRAW_QUIET_MS)
      : baseCoalesceMs;
    if (!coalesced && coalesceMs > 0 && !covered && this.mode === "live" &&
      this.pending[0]?.kind === "output" && this.pendingResizes === 0) {
      if (waitForRedraw && this.resizeRedrawTimer === null) {
        this.resizeRedrawTimer = this.timers.setTimeout(() => {
          this.resizeRedrawTimer = null;
          this.clearOutputTimer();
          this.pump(true);
        }, RESIZE_REDRAW_MAX_MS);
      }
      this.batchStartedAt = Date.now();
      this.outputTimer = this.timers.setTimeout(() => {
        this.outputTimer = null;
        this.pump(true);
      }, coalesceMs);
      return;
    }
    // 묶음 창이 끝났는데 프레임이 열린 채(커서 숨김으로 끝남)면 상한 안에서 다음
    // 조각을 한 창 더 기다린다. 커서를 계속 숨겨 두는 프로그램은 조각마다 숨김을
    // 다시 보내지 않으므로 붙잡히지 않는다(이 묶음이 연 프레임만 본다).
    if (
      coalesced && coalesceMs > 0 && !covered && !this.hidden && this.mode === "live" &&
      this.pending[0]?.kind === "output" && this.pendingResizes === 0 &&
      Date.now() - this.batchStartedAt < FRAME_HOLD_MAX_MS &&
      this.pendingFrameOpen()
    ) {
      this.outputTimer = this.timers.setTimeout(() => {
        this.outputTimer = null;
        this.pump(true);
      }, coalesceMs);
      return;
    }
    const record = this.shiftPending();
    if (!record) {
      if (this.received.size === 0) zoomPreviewFor(this.terminal)?.outputIdle();
      this.checkLive();
      return;
    }
    // 미뤄 둔 초기화는 이 epoch의 첫 쓰기(또는 동기 resize 레코드 적용) 직전에
    // 내린다 — reset이 write보다 먼저 오되, 그 사이에 빈 화면이 드러나지 않게.
    this.consumePendingReset();
    // 도착할 때 겹쳐진 것으로 소비 선포가 된 기록(버스트 안의 resize) — 적용
    // 없이 다음으로 넘긴다(onConsumed는 이미 불렸다).
    if (record.consumed) {
      this.pump();
      return;
    }
    if (record.kind === "output" && record.bytes) {
      this.clearResizeRedraw();
      const records = [record];
      let bytes = record.bytes;
      // Drain an existing backlog — live or replay — in bounded writes without
      // introducing another timer. A batch never crosses a resize record, the
      // replay/live boundary (pattern detection and screen-change counting look
      // only at live bytes), or a remembered clear point (the clear must land
      // before the next record is handed to xterm).
      const replayed = record.seq <= this.replayTargetSeq;
      let length = bytes.length;
      while (!this.isClearBarrier(records[records.length - 1])) {
        const next = this.pending[0];
        if (next?.kind !== "output" || !next.bytes) break;
        if ((next.seq <= this.replayTargetSeq) !== replayed) break;
        if (length + next.bytes.length > ACK_BATCH_BYTES) break;
        this.shiftPending();
        records.push(next);
        length += next.bytes.length;
      }
      if (records.length > 1) {
        bytes = new Uint8Array(length);
        let offset = 0;
        for (const next of records) {
          bytes.set(next.bytes!, offset);
          offset += next.bytes!.length;
        }
      }
      this.applying = true;
      if (this.callbacks.onOutputText && this.mode === "live" && record.seq > this.replayTargetSeq) {
        try {
          this.callbacks.onOutputText(this.decoder.decode(bytes, { stream: true }));
        } catch {
          // 디코딩 실패는 패턴 감지 누락으로 충분하다.
        }
      }
      const generation = this.generation;
      zoomPreviewFor(this.terminal)?.outputPending();
      this.inflightWrites += 1;
      this.terminal.write(bytes, () => {
        this.inflightWrites -= 1;
        this.notifyWriteDrained();
        // xterm.reset()은 대기 중인 write 콜백을 취소하지 않는다 — 재접속 뒤
        // 도착한 이전 epoch의 완료가 새 epoch의 순서·ACK를 깨지 않게 한다.
        if (this.disposed || generation !== this.generation) return;
        this.applying = false;
        for (const next of records) this.onConsumed(next);
        // 묶어 쓴 기록까지 모두 소진된 뒤가 화면 지우기의 기준점이다(throughSeq).
        if (this.clearQueued) {
          this.clearQueued = false;
          this.applyClear(this.throughSeq);
        }
        if (record.seq > this.replayTargetSeq) this.screen.outputApplied();
        // 쓰는 동안 밀린 기록은 이미 시간상 묶인 것이다 — 묶음 창을 다시 기다리지
        // 않고 곧바로 잇는다(창마다 64 KiB로 처리량이 묶이지 않게).
        this.pump(this.pending.length > 0);
      });
      return;
    }
    // resize record: applied synchronously — prior writes already completed
    // because `applying` serialized them; later writes have not been issued.
    if (record.kind === "resize" && record.cols !== undefined && record.rows !== undefined) {
      const { cols, rows } = record;
      if (cols > 0 && rows > 0) {
        const grid = this.terminal as { cols?: number; rows?: number };
        const changed = grid.cols !== cols || grid.rows !== rows;
        preserveViewport(this.terminal, () => this.terminal.resize(cols, rows));
        const preview = zoomPreviewFor(this.terminal);
        preview?.resizeApplied(cols, rows);
        // live 크기 변경: 줄바꿈·다시 그리기가 가라앉을 때까지 화면 변화를 세지 않는다.
        if (record.seq > this.replayTargetSeq) {
          this.screen.resizeApplied();
          this.resizeRedrawPending = true;
          // 격자가 실제로 바뀌었으면 PTY가 SIGWINCH를 보냈다 — 프로그램이 다시
          // 그리기 전 화면은 xterm이 옛 내용을 임시로 다시 접은 것이라, 그 답을
          // 잠깐 기다린 뒤에 덮개를 걷는다(끝난 세션은 답할 프로그램이 없다).
          if (changed && !this.exited) preview?.resizeSignaled();
        }
      }
      this.onConsumed(record);
      this.pump();
      return;
    }
    // Unknown shape (should not happen) — count and continue.
    this.onConsumed(record);
    this.pump();
  }

  /**
   * 다음에 쓸 출력 묶음(선두의 연속 출력 레코드, 최대 ACK_BATCH_BYTES)이 커서를
   * 숨긴 채 끝나는가 — 마지막 커서 제어가 `ESC[?25l`이면 TUI 프레임이 아직
   * 열린 것이다. 뒤에서부터 훑어 처음 만나는 제어로 판정한다.
   */
  private pendingFrameOpen(): boolean {
    let length = 0;
    let end = 0;
    while (end < this.pending.length) {
      const next = this.pending[end];
      if (next.kind !== "output" || !next.bytes || length + next.bytes.length > ACK_BATCH_BYTES) break;
      length += next.bytes.length;
      end += 1;
    }
    for (let i = end - 1; i >= 0; i -= 1) {
      const bytes = this.pending[i].bytes!;
      const verdict = lastCursorVisibility(bytes);
      if (verdict !== null) return verdict === "hidden";
    }
    return false;
  }

  /** 재생이 기억된 화면 지우기 지점을 지나는 레코드 — 묶어 쓰기가 여기서 끊긴다. */
  private isClearBarrier(record: AppliedRecord): boolean {
    return this.clearAfterSeq > 0 && record.seq === this.clearAfterSeq && record.seq <= this.replayTargetSeq;
  }

  private onConsumed(record: AppliedRecord): void {
    record.consumed = true;
    this.lastProgressAt = Date.now();
    if (record.seq <= this.replayTargetSeq) this.replayBytes += record.rawLen;
    // 재생이 기억된 지우기 지점을 지나면 그때처럼 지운다 — 뒤따르는 레코드는
    // 아직 터미널에 넘기지 않았다.
    if (record.seq === this.clearAfterSeq && record.seq <= this.replayTargetSeq) {
      this.clearTerminal?.();
    }
    if (record.seq === this.throughSeq + 1) {
      this.throughSeq = record.seq;
      // Advance over records already consumed out of order upstream.
      while (this.pending.length > 0 && this.pending[0].consumed && this.pending[0].seq === this.throughSeq + 1) {
        this.throughSeq = (this.shiftPending() as AppliedRecord).seq;
      }
    }
    this.ackBytes += record.rawLen;
    if (this.ackBytes >= ACK_BATCH_BYTES) {
      this.flushAck();
    } else if (this.ackTimer === null) {
      this.ackTimer = this.timers.setTimeout(() => {
        this.ackTimer = null;
        this.flushAck();
      }, ACK_COALESCE_MS);
    }
    this.checkLive();
  }

  private checkLive(): void {
    // 스냅샷을 쓰는 동안에는 화면이 아직 복원되지 않았다 — 입력을 열지 않는다.
    if (this.attaching || this.restoring) return;
    if (this.mode === "replay" && this.throughSeq >= this.replayTargetSeq) {
      this.setMode(this.exited ? "exited" : "live");
      this.flushResize();
    }
  }

  private flushAck(): void {
    this.clearAckTimer();
    if (this.epoch === null || this.throughSeq === 0 || this.disposed) return;
    this.client.sessionAck({
      session_id: this.sessionId,
      epoch: this.epoch,
      through_seq: String(this.throughSeq),
    });
    this.ackBytes = 0;
  }

  private clearAckTimer(): void {
    if (this.ackTimer !== null) {
      this.timers.clearTimeout(this.ackTimer);
      this.ackTimer = null;
    }
  }

  private clearOutputTimer(): void {
    if (this.outputTimer !== null) {
      this.timers.clearTimeout(this.outputTimer);
      this.outputTimer = null;
    }
  }

  private clearResizeRedraw(): void {
    this.resizeRedrawPending = false;
    if (this.resizeRedrawTimer !== null) {
      this.timers.clearTimeout(this.resizeRedrawTimer);
      this.resizeRedrawTimer = null;
    }
  }

  /**
   * Terminal data (keystrokes and terminal-generated query responses).
   * During replay nothing is sent to the daemon (02 §5-2).
   */
  private handleTerminalData(data: string): void {
    if (this.disposed) return;
    if (this.attaching || this.mode !== "live" || this.epoch === null) {
      this.callbacks.onInputBlocked?.();
      return;
    }
    // 입력(키·마우스·포커스 보고)에 대한 앱의 반응 — 에코·다시 그리기·앱 안
    // 스크롤 — 은 마지막 출력으로 세지 않는다(screenOutput.ts).
    this.screen.inputSent();
    // 브라우저가 xterm textarea로 직접 흘린 대량 붙여넣기(onData)도
    // 있으므로, 큐에 통째로 안 들어가는 입력은 대용량 경로로 보낸다.
    if (!this.input.fitsNow(data)) {
      void this.input
        .enqueueLarge(data)
        .then(() => this.callbacks.onLocalInput?.(data))
        .catch((error) => this.callbacks.onInputError?.(error as Error));
      return;
    }
    try {
      this.input.enqueue(data);
      this.callbacks.onLocalInput?.(data);
    } catch (error) {
      this.callbacks.onInputError?.(error as Error);
    }
  }

  /** Direct input path (paste). Same live-mode gate as keystrokes. */
  sendInput(data: string): boolean {
    if (this.disposed || this.attaching || this.mode !== "live" || this.epoch === null) {
      this.callbacks.onInputBlocked?.();
      return false;
    }
    this.screen.inputSent();
    try {
      this.input.enqueue(data);
      return true;
    } catch (error) {
      this.callbacks.onInputError?.(error as Error);
      return false;
    }
  }

  /**
   * 대용량 입력 경로(붙여넣기): 4 KiB 청크로 나눠 큐 여유를 기다리며
   * 보낸다(1 MiB paste 허용 상한과 64 KiB 큐 상한의 정합성 — W1-8).
   */
  async sendLargeInput(data: string): Promise<boolean> {
    if (this.disposed || this.attaching || this.mode !== "live" || this.epoch === null) {
      this.callbacks.onInputBlocked?.();
      return false;
    }
    this.screen.inputSent();
    try {
      await this.input.enqueueLarge(data);
      return true;
    } catch (error) {
      this.callbacks.onInputError?.(error as Error);
      return false;
    }
  }

  /**
   * Send the first fit immediately. While its RPC is pending, retain only the
   * latest dimensions: queuing every drag frame makes the UI replay obsolete
   * sizes long after the pointer stops. The actor owns the 16ms coalescing;
   * actual xterm resizing still follows the ordered journal records.
   */
  requestResize(cols: number, rows: number): void {
    // NaN(떼어진 host에서 fit)은 `<= 0`을 통과하므로 유한한 양수만 받는다.
    if (this.disposed || !(Number.isFinite(cols) && cols > 0 && Number.isFinite(rows) && rows > 0)) return;
    // Always replace the pending target, even if it matches the in-flight
    // size (A → B → A must discard B).
    this.proposedResize = { cols, rows };
    // 재생·재접속 중에는 화면을 덮지 않는다: 이 fit이 방금 시작한 줌 프리뷰는
    // 막 초기화된(빈) 캔버스를 복사해 재생이 끝나고 크기 기록이 적용될 때까지
    // 그 빈 화면을 보여 준다 — "기록 재생 중…" 배지 아래로 재생이 비쳐야 한다.
    // 프리뷰는 live에서의 대화식 크기 변경(창 끌기·줌)에만 뜻이 있다.
    if (this.mode !== "live") cancelZoomPreview(this.terminal);
    if (this.lastSentResize && (this.lastSentResize.cols !== cols || this.lastSentResize.rows !== rows)) {
      // Zooming straight back can match the current xterm grid while the
      // previous resize is still in transit. Wait for the final journal size,
      // otherwise that older record would reflow an already uncovered screen.
      zoomPreviewFor(this.terminal)?.holdForResize(cols, rows);
    }
    this.flushResize();
  }

  private flushResize(): void {
    if (this.resizeInFlight || this.attaching || this.epoch === null || this.disposed) return;
    // 재생 중에도 최신 fit을 곧바로 보낸다: 크기 기록은 저널 꼬리에 붙어 재생이
    // 끝나는 그 자리에서 순서대로 적용되고, 그에 답한 TUI의 다시 그리기도 바로
    // 뒤에 실린다. live가 될 때까지 미루면 재생 내내 스냅샷/과거 격자로 그려졌다가
    // 재생 뒤 왕복을 한 번 더 기다려서야 창 크기로 펴진다(처음 뜰 때 축소됐다가
    // 뒤늦게 늘어나는 느낌). 끝난 세션은 PTY가 없으니 재생 뒤 로컬로만 맞춘다.
    if (this.exited && this.mode === "replay") return;
    const proposed = this.proposedResize;
    this.proposedResize = null;
    if (!proposed) return;
    if (this.exited) {
      preserveViewport(this.terminal, () => this.terminal.resize(proposed.cols, proposed.rows));
      cancelZoomPreview(this.terminal);
      return;
    }
    if (this.lastSentResize?.cols === proposed.cols && this.lastSentResize.rows === proposed.rows) return;
    const epoch = this.epoch;
    this.lastSentResize = proposed;
    this.resizeInFlight = true;
    // 데몬은 크기 기록을 못 찾으면 600ms까지 기다린다(dispatch.rs `applied_seq`).
    // 그동안 resizeInFlight가 다음 줌·창 끌기를 통째로 막으므로, 실제로 걸리는지
    // 알 수 있게 걸린 시간을 남긴다(devtools 전용 — 사용자에게는 보이지 않는다).
    const startedAt = Date.now();
    void this.client
      .sessionResize({
        session_id: this.sessionId,
        epoch,
        resize_id: this.uuidFn(),
        cols: proposed.cols,
        rows: proposed.rows,
      })
      .then((result) => {
        // 데몬이 이미 그 크기라 새 크기 기록을 쓰지 않았다(applied_seq가 이미
        // 소비한 기록을 가리킨다) — 저널로는 격자가 맞춰지지 않으니 여기서 직접
        // 맞춘다. 스냅샷 격자로 남은 xterm이 PTY와 어긋나면 마지막 줄이 낡은 채
        // 남고 커서 자리가 틀린다. PTY는 이미 그 크기이므로 이후 출력과 맞는다.
        if (this.disposed || this.epoch !== epoch) return;
        zoomPreviewFor(this.terminal)?.resizeAcknowledged();
        const applied = Number(result?.applied_seq);
        if (!Number.isFinite(applied) || applied > this.throughSeq) return;
        if (this.pendingResizes > 0) return;
        const grid = this.terminal as { cols?: number; rows?: number };
        if (grid.cols === proposed.cols && grid.rows === proposed.rows) return;
        preserveViewport(this.terminal, () => this.terminal.resize(proposed.cols, proposed.rows));
        zoomPreviewFor(this.terminal)?.resizeApplied(proposed.cols, proposed.rows);
      })
      .catch((error: unknown) => {
        console.debug("session.resize failed", { ms: Date.now() - startedAt, ...proposed, error });
        // A later fit may retry a failed size; an old epoch's rejection
        // must not alter the current epoch's deduplication state.
        if (this.epoch === epoch) {
          this.lastSentResize = null;
          cancelZoomPreview(this.terminal);
        }
      })
      .finally(() => {
        this.resizeInFlight = false;
        this.flushResize();
      });
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    cancelZoomPreview(this.terminal);
    this.clearOutputTimer();
    this.clearResizeRedraw();
    this.stopStallWatch();
    this.flushAck();
    this.screen.dispose();
    this.dataSub?.dispose();
    this.dataSub = null;
    this.input.dispose();
    this.earlyEvents = [];
    this.pending = [];
    this.pendingResizes = 0;
    this.clearResizeHold();
    this.received.clear();
    this.proposedResize = null;
  }
}

/**
 * `bytes` 안의 마지막 커서 표시/숨김 제어(`ESC[?25h` / `ESC[?25l`)를 뒤에서부터
 * 찾는다. 없으면 null. 레코드 경계에 걸친 제어는 보지 못한다(그러면 붙잡지
 * 않을 뿐이다).
 */
function lastCursorVisibility(bytes: Uint8Array): "hidden" | "shown" | null {
  for (let i = bytes.length - CURSOR_HIDE.length; i >= 0; i -= 1) {
    if (bytes[i] !== 0x1b || bytes[i + 1] !== 0x5b || bytes[i + 2] !== 0x3f ||
      bytes[i + 3] !== 0x32 || bytes[i + 4] !== 0x35) continue;
    const last = bytes[i + 5];
    if (last === CURSOR_HIDE[5]) return "hidden";
    if (last === CURSOR_SHOW[5]) return "shown";
  }
  return null;
}

/**
 * UI input FIFO: 한 write 4 KiB, 동시 미완료 write 1개, queue 상한 64 KiB
 * (02 §6). INPUT_OUTCOME_UNKNOWN이면 자동 재전송하지 않는다. 단, 연결 단위
 * 입력 예산 초과(`BUSY` + `PENDING_BUDGET`, 02 §6)는 이 큐의 잘못이 아니라
 * 같은 연결의 다른 pane들이 채운 일시적 뒤압력이므로 청크를 버리지 않고
 * 정해진 간격으로 유한하게 다시 보낸다 — 데몬은 그 거절을 dispatch 전에
 * 내므로 세션에 닿지 않았다(재전송 금지 계약과 충돌하지 않는다).
 *
 * 상한 검사는 "큐에 **남아 있는** 바이트"에만 적용한다 — 붙여넣기 허용
 * 상한(1 MiB)과 큐 상한(64 KiB)이 서로 어긋나 64 KiB를 넘는 paste가
 * 즉시 거부되던 결함의 수정(SOTA_GAP_REVIEW W1-8). 입력은 enqueue 시점에
 * 4 KiB 청크로 쪼개고, 대용량은 `enqueueLarge`가 큐 여유를 기다리며
 * 나눠 넣는다.
 */
export class InputQueue {
  private chunks: Array<{ data: Uint8Array; retries: number }> = [];
  private queuedBytes = 0;
  /**
   * 예산(PENDING_BUDGET) 거절로 되돌아와 재시도 간격을 기다리는 청크. 큐 밖
   * 슬롯에 두고 그 바이트를 `reservedBytes`로 계상한다 — 대기자가 송신 중에
   * 비워진 자리에 밀어 넣었다가 되돌아온 청크와 함께 상한을 넘기는 일을 막는다.
   * 정상 송신은 예약하지 않는다(이른 깨움 입장 유지 — 파이프라인 처리).
   */
  private retryHead: { data: Uint8Array; retries: number } | null = null;
  private reservedBytes = 0;
  private inFlight = false;
  private disposed = false;
  private overflowed = false;
  private capacityWaiters: Array<() => void> = [];
  /** 예산 재시도 예약 — 해제(dispose)하거나 다음 drain이 대신 흘리면 끊는다. */
  private retryTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    /**
     * 청크를 보내 응답을 돌려준다. `InputResult`(또는 그 일부) — `queued`
     * true는 750 ms 상한의 정직한 Queued(아직 tty에 안 쓰였음)라 전달이
     * 아니다. void를 돌려주는 호출자는 항상 전달된 것으로 본다(구데몬 호환).
     */
    private readonly send: (bytes: Uint8Array) => Promise<unknown>,
    /** 전송 실패를 알린다 — 삼키면 pane은 live인데 키가 조용히 사라진다. */
    private readonly onError?: (error: unknown) => void,
    /** 청크 하나가 받아들여졌다(입력 막힘 회복 신호 — Queued는 제외). */
    private readonly onDelivered?: () => void,
  ) {}

  get pendingBytes(): number {
    // 되돌아와 대기 중인 재시도 청크도 아직 안 온 입력이다.
    return this.queuedBytes + this.reservedBytes;
  }

  /** 이 입력 전체가 지금 큐에 들어가는가(동기 경로 유지 판정). */
  fitsNow(text: string): boolean {
    if (this.disposed) return false;
    // 전체 길이로 판정한다 — 첫 청크만 보면 중간 크기 입력이 동기 경로에서
    // 도중에 넘쳐 꼬리를 잃는다(대용량 경로는 용량을 기다린다).
    return this.canFit(new TextEncoder().encode(text).length);
  }

  enqueue(text: string): void {
    if (this.disposed) return;
    const bytes = new TextEncoder().encode(text);
    try {
      let offset = 0;
      while (offset < bytes.length) {
        const slice = bytes.subarray(offset, Math.min(offset + INPUT_CHUNK_BYTES, bytes.length));
        this.pushChunk(slice);
        offset += slice.length;
      }
    } finally {
      // 도중 상한 초과로 throw해도 이미 들어간 청크는 흘려보낸다.
      void this.drain();
    }
  }

  /**
   * 대용량 입력(붙여넣기). 큐가 가득 차면 드레인이 진행될 때까지 기다렸다
   * 다음 청크를 넣는다 — 상한 위반 없이 1 MiB까지 순서대로 흘려보낸다.
   */
  async enqueueLarge(text: string): Promise<void> {
    if (this.disposed) return;
    const bytes = new TextEncoder().encode(text);
    let offset = 0;
    while (offset < bytes.length && !this.disposed) {
      const slice = bytes.subarray(offset, Math.min(offset + INPUT_CHUNK_BYTES, bytes.length));
      while (!this.canFit(slice.length) && !this.disposed) {
        await this.waitForCapacity();
      }
      if (this.disposed) return;
      this.pushChunk(slice);
      offset += slice.length;
      void this.drain();
    }
  }

  private canFit(len: number): boolean {
    return this.queuedBytes + this.reservedBytes + len <= INPUT_QUEUE_BYTES;
  }

  private pushChunk(chunk: Uint8Array): void {
    if (!this.canFit(chunk.length)) {
      this.overflowed = true;
      throw new Error("입력 대기열이 가득 찼습니다(64 KiB)");
    }
    this.chunks.push({ data: chunk, retries: 0 });
    this.queuedBytes += chunk.length;
  }

  private waitForCapacity(): Promise<void> {
    return new Promise((resolve) => this.capacityWaiters.push(resolve));
  }

  private notifyCapacity(): void {
    const waiters = this.capacityWaiters;
    this.capacityWaiters = [];
    for (const waiter of waiters) waiter();
  }

  private async drain(): Promise<void> {
    // 예산 재시도가 예약되어 있으면 타이머가 간격을 지킨다 — 여기서 다시
    // 보내면 새 입력 한 조각이 250 ms 간격 × 5회를 밀리초 만에 모두 태운다.
    if (this.inFlight || this.retryTimer !== null) return;
    this.inFlight = true;
    try {
      while (this.chunks.length > 0 && !this.disposed) {
        const head = this.chunks[0];
        let chunk = head.data;
        if (chunk.length > INPUT_CHUNK_BYTES) {
          chunk = chunk.slice(0, INPUT_CHUNK_BYTES);
          head.data = head.data.slice(INPUT_CHUNK_BYTES);
        } else {
          this.chunks.shift();
        }
        this.queuedBytes -= chunk.length;
        // 큐에서 빠진 시점에 수용량 대기자를 깨운다(전송 완료를 기다리면
        // 느린 전송 하나가 뒤따르는 대량 입력을 불필요하게 막는다) — 되돌아올
        // 수 있는 청크(예산 재시도)는 reservedBytes로 자리를 지킨다.
        this.notifyCapacity();
        try {
          const reply = await this.send(chunk);
          // 750 ms 상한의 Queued는 받아들여졌을 뿐 아직 쓰이지 않았다 — 막힘
          // 배지를 끄는 '전달' 신호로 세지 않는다(간헐적 진행에서 배지 진동 방지).
          if ((reply as { queued?: unknown } | void)?.queued !== true) {
            this.onDelivered?.();
          }
        } catch (error) {
          const backoff = backpressureRetryOf(error);
          if (backoff !== null && head.retries < INPUT_BACKPRESSURE_RETRIES && !this.disposed) {
            // 연결 단위 입력 예산: 이 청크는 거절됐을 뿐 PTY에는 닿지
            // 않았다 — 순서를 지켜 재시도 슬롯에 두고 간격 뒤 다시 보낸다.
            // 남은 청크도 그대로 둔다(drain이 직렬이라 섞이지 않는다).
            this.retryHead = { data: chunk, retries: head.retries + 1 };
            this.reservedBytes = chunk.length;
            this.scheduleRetry(backoff.retryAfterMs);
            return;
          }
          // 그 외 실패는 자동 재전송 금지: 남은 입력을 버리고 폐기한다.
          this.chunks = [];
          this.queuedBytes = 0;
          this.reservedBytes = 0;
          this.onError?.(error);
          return;
        }
      }
    } finally {
      this.inFlight = false;
      this.notifyCapacity();
    }
  }

  /** 예산 재시도 예약 — 간격이 지나면 슬롯의 청크를 큐 맨 앞에 돌려놓고 이어 보낸다. */
  private scheduleRetry(delayMs: number): void {
    if (this.retryTimer !== null) return;
    this.retryTimer = setTimeout(() => {
      this.retryTimer = null;
      const head = this.retryHead;
      if (head !== null && !this.disposed) {
        this.retryHead = null;
        this.reservedBytes = 0;
        this.chunks.unshift(head);
        this.queuedBytes += head.data.length;
      }
      void this.drain();
    }, delayMs);
  }

  get didOverflow(): boolean {
    return this.overflowed;
  }

  dispose(): void {
    this.disposed = true;
    if (this.retryTimer !== null) clearTimeout(this.retryTimer);
    this.retryTimer = null;
    this.chunks = [];
    this.retryHead = null;
    this.queuedBytes = 0;
    this.reservedBytes = 0;
    this.notifyCapacity();
  }
}

/** 예산 초과 재시도의 상한 — 간격(기본 250 ms)을 곱해 ~1.25 s 안에 포기한다. */
const INPUT_BACKPRESSURE_RETRIES = 5;
/** `retry_after_ms`가 없거나 말도 안 되는 값일 때의 재시도 간격. */
const INPUT_BACKPRESSURE_DEFAULT_MS = 250;

/**
 * 데몬의 연결 단위 입력 예산 초과 거절(02 §6, `BUSY` + `PENDING_BUDGET`)인가?
 * 이 거절은 dispatch 전에 내려지므로 입력은 세션에 닿지 않았다 — 재시도
 * 안전하다. 다른 모든 실패(detached·막힘·unknown)는 기존 재전송 금지 계약.
 */
function backpressureRetryOf(error: unknown): { retryAfterMs: number } | null {
  const source = (error ?? {}) as {
    code?: unknown;
    details?: { reason_code?: unknown; retry_after_ms?: unknown };
  };
  if (source.code !== "BUSY" || source.details?.reason_code !== INPUT_REJECTION_REASONS.pendingBudget) {
    return null;
  }
  const raw = Number(source.details?.retry_after_ms);
  const retryAfterMs =
    Number.isFinite(raw) && raw > 0 && raw <= 5_000 ? raw : INPUT_BACKPRESSURE_DEFAULT_MS;
  return { retryAfterMs };
}

/** 전송 실패를 Error로 맞춘다 — 브리지가 던진 `{code,message}` 객체의 code를 보존한다. */
function toError(error: unknown): Error {
  if (error instanceof Error) return error;
  const source = (error ?? {}) as { code?: unknown; message?: unknown; details?: unknown };
  const wrapped = new Error(typeof source.message === "string" ? source.message : String(error));
  if (source.code !== undefined) (wrapped as Error & { code?: unknown }).code = source.code;
  // details.reason_code가 입력 거절의 원인(INPUT_STALLED·GUARD_SUSPENDED)을 가른다.
  if (source.details !== undefined) (wrapped as Error & { details?: unknown }).details = source.details;
  return wrapped;
}

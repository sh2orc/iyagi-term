/**
 * 에이전트 시작 줄(P2-5) — 이어서 열기(앱 복원·계속 버튼·세션 목록·모두 재개)를
 * 한 줄로 세워 기동 비용이 한꺼번에 몰리지 않게 한다.
 *
 * - 슬롯은 실행·연결 RPC가 돌아와도 바로 풀지 않는다: 에이전트의 진짜 기동 비용
 *   (Node 로딩·대화 기록 읽기)은 그 뒤에 온다. 새 pane의 작업이 에이전트를
 *   보고하거나(workload.agent) 시작이 실패·종료로 끝나면(`settled`) 풀고, 그래도
 *   START_SETTLE_CAP_MS면 푼다. 시작과 시작 사이에는 START_MIN_SPACING_MS를 둔다.
 * - 호스트 메모리가 CRITICAL이면 줄 머리가 풀리길 기다린다(START_MEMORY_WAIT_CAP_MS
 *   뒤에는 그냥 시작 — 영원히 안 띄우지 않는다). 메모리 WARNING 이상이거나 CPU
 *   압력이 WARNING 이상이면 한 번에 하나만 띄운다.
 * - "지금 시작"(startNow)은 줄·압력을 건너뛴다 — 사용자가 직접 고른 것이다.
 *
 * 줄은 시각·타이머·압력을 주입받는 순수 객체다(컨트롤러 밖에서 시험한다).
 */

import type { PressureLevel } from "../../generated/PressureLevel";
import type { PaneStartWait } from "../../store/workbenchStore";

/** 평소 동시에 띄우는 수. 3개면 10코어 PC에서 기동이 체감되지 않는다. */
export const START_CONCURRENCY = 3;
/** 메모리 WARNING+ 또는 CPU 압력 WARNING+일 때의 동시 시작 수. */
export const START_CONCURRENCY_UNDER_PRESSURE = 1;
/** 시작 사이 최소 간격. */
export const START_MIN_SPACING_MS = 1500;
/** 기동 확인을 기다리는 상한 — 넘기면 슬롯을 푼다(에이전트 감지가 없는 플랫폼 등). */
export const START_SETTLE_CAP_MS = 12_000;
/** 메모리 CRITICAL이 풀리길 기다리는 상한(줄 머리 하나당). */
export const START_MEMORY_WAIT_CAP_MS = 30_000;
/** 줄이나 슬롯이 남아 있는 동안의 점검 간격. */
export const START_POLL_MS = 250;

export interface StartPressure {
  memory: PressureLevel;
  cpu: PressureLevel;
}

/** 지금 압력에서 동시에 띄울 수 있는 수(표본 전이면 평소 값). */
export function startConcurrency(pressure: StartPressure | null): number {
  if (!pressure) return START_CONCURRENCY;
  return pressure.memory !== "NORMAL" || pressure.cpu !== "NORMAL"
    ? START_CONCURRENCY_UNDER_PRESSURE
    : START_CONCURRENCY;
}

export interface AgentStartJob {
  /** 대기 표시·지금 시작·취소의 열쇠(pane leaf id). */
  readonly id: string;
  /** 실제 시작. 약속이 끝나도 슬롯은 `settled()`가 참이 될 때까지(상한까지) 쥔다. */
  start(): Promise<void>;
  /** 기동이 끝났는가(에이전트 보고·실패·종료·pane 사라짐). `start`가 끝난 뒤에만 묻는다. */
  settled(): boolean;
  /** 기다리는 동안 무효가 됐는가(pane 닫힘·다른 실행 시작). 참이면 시작하지 않고 버린다. */
  cancelled(): boolean;
  /** 시작하지 않고 버려질 때(취소·폐기) 한 번 불린다. */
  dropped?(): void;
}

export interface AgentStartPoolOptions {
  /** 지금 호스트 압력(표본 전이면 null — 막지 않는다). */
  pressure(): StartPressure | null;
  /** 대기 이유가 바뀔 때(null = 줄에서 빠짐) — pane 표시용. */
  onWait(id: string, wait: PaneStartWait | null): void;
  now?: () => number;
  setTimer?: (callback: () => void, ms: number) => unknown;
  clearTimer?: (handle: unknown) => void;
}

interface QueuedStart {
  job: AgentStartJob;
  /** 사용자가 "지금 시작"을 눌렀다 — 줄·압력을 건너뛴다. */
  bypass: boolean;
  /** 줄 머리에서 메모리 CRITICAL로 막히기 시작한 시각(상한 계산). */
  memorySince: number | null;
  wait: PaneStartWait | null;
}

interface StartSlot {
  job: AgentStartJob;
  startedAt: number;
  /** `start()`가 끝났는가 — 그 뒤에만 기동 확인(`settled`)을 묻는다. */
  launched: boolean;
  promise: Promise<void>;
}

export class AgentStartPool {
  private readonly queue: QueuedStart[] = [];
  private readonly slots = new Set<StartSlot>();
  private lastStartAt = Number.NEGATIVE_INFINITY;
  private timer: unknown = null;
  private disposed = false;
  private readonly now: () => number;
  private readonly setTimer: (callback: () => void, ms: number) => unknown;
  private readonly clearTimer: (handle: unknown) => void;

  constructor(private readonly options: AgentStartPoolOptions) {
    this.now = options.now ?? (() => Date.now());
    this.setTimer = options.setTimer ?? ((callback, ms) => setTimeout(callback, ms));
    this.clearTimer = options.clearTimer ?? ((handle) => clearTimeout(handle as ReturnType<typeof setTimeout>));
  }

  /**
   * 줄 끝에 세운다. 지금 바로 시작할 수 있으면 그 자리에서 시작하고 `start()`의
   * 약속을 돌려준다(호출자는 평소처럼 실행이 끝나길 기다릴 수 있다). 줄에서
   * 기다리게 됐으면 null — 대기 표시는 `onWait`이 맡는다.
   */
  enqueue(job: AgentStartJob): Promise<void> | null {
    if (this.disposed) return null;
    this.queue.push({ job, bypass: false, memorySince: null, wait: null });
    this.pump();
    for (const slot of this.slots) if (slot.job === job) return slot.promise;
    return null;
  }

  /** 이 id가 줄에서 기다리는 중인가(시작한 것은 아니다). */
  isQueued(id: string): boolean {
    return this.queue.some((entry) => entry.job.id === id);
  }

  /** "지금 시작": 줄 맨 앞으로 옮기고 압력·간격·상한을 건너뛴다. 줄에 없으면 false. */
  startNow(id: string): boolean {
    const index = this.queue.findIndex((entry) => entry.job.id === id);
    if (index < 0) return false;
    const [entry] = this.queue.splice(index, 1);
    entry.bypass = true;
    this.queue.unshift(entry);
    this.pump();
    return true;
  }

  /** 기다리던 항목을 빼낸다(시작하지 않는다). */
  cancel(id: string): void {
    const index = this.queue.findIndex((entry) => entry.job.id === id);
    if (index < 0) return;
    const [entry] = this.queue.splice(index, 1);
    this.drop(entry);
    this.schedule();
  }

  /** 줄을 다시 본다 — 슬롯 정산 → 무효 항목 정리 → 시작할 수 있는 만큼 시작. */
  pump(): void {
    if (this.disposed) return;
    const now = this.now();
    for (const slot of [...this.slots]) {
      if (now - slot.startedAt >= START_SETTLE_CAP_MS || (slot.launched && this.safeSettled(slot.job))) {
        this.slots.delete(slot);
      }
    }
    for (let index = this.queue.length - 1; index >= 0; index -= 1) {
      if (!this.safeCancelled(this.queue[index].job)) continue;
      const [entry] = this.queue.splice(index, 1);
      this.drop(entry);
    }
    let blocked: PaneStartWait | null = null;
    while (this.queue.length > 0) {
      const head = this.queue[0];
      if (!head.bypass) {
        const pressure = this.options.pressure();
        if (pressure?.memory === "CRITICAL") {
          head.memorySince ??= now;
          if (now - head.memorySince < START_MEMORY_WAIT_CAP_MS) {
            blocked = "memory";
            break;
          }
        } else {
          head.memorySince = null;
        }
        if (this.slots.size >= startConcurrency(pressure) || now - this.lastStartAt < START_MIN_SPACING_MS) {
          blocked = "turn";
          break;
        }
      }
      this.queue.shift();
      this.launch(head, now);
    }
    if (blocked !== null) {
      for (const entry of this.queue) this.markWait(entry, blocked);
    }
    this.schedule();
  }

  /** 컨트롤러 폐기 — 타이머를 끄고 줄을 비운다(대기 표시는 스토어와 함께 사라진다). */
  dispose(): void {
    this.disposed = true;
    if (this.timer !== null) this.clearTimer(this.timer);
    this.timer = null;
    const pending = this.queue.splice(0);
    for (const entry of pending) {
      try {
        entry.job.dropped?.();
      } catch {
        // 정리 실패가 폐기를 막지 않는다.
      }
    }
    this.slots.clear();
  }

  /** 시험·진단용: 기다리는 수와 쥐고 있는 슬롯 수. */
  get queuedCount(): number {
    return this.queue.length;
  }

  get activeCount(): number {
    return this.slots.size;
  }

  private launch(entry: QueuedStart, now: number): void {
    this.lastStartAt = now;
    if (entry.wait !== null) {
      entry.wait = null;
      this.options.onWait(entry.job.id, null);
    }
    let promise: Promise<void>;
    try {
      promise = Promise.resolve(entry.job.start());
    } catch (error) {
      promise = Promise.reject(error);
    }
    const slot: StartSlot = { job: entry.job, startedAt: now, launched: false, promise };
    this.slots.add(slot);
    void promise
      .catch(() => undefined)
      .finally(() => {
        slot.launched = true;
        this.pump();
      });
  }

  private drop(entry: QueuedStart): void {
    if (entry.wait !== null) this.options.onWait(entry.job.id, null);
    entry.wait = null;
    try {
      entry.job.dropped?.();
    } catch {
      // 정리 실패가 줄을 막지 않는다.
    }
  }

  private markWait(entry: QueuedStart, wait: PaneStartWait): void {
    if (entry.wait === wait) return;
    entry.wait = wait;
    this.options.onWait(entry.job.id, wait);
  }

  private safeSettled(job: AgentStartJob): boolean {
    try {
      return job.settled();
    } catch {
      return true;
    }
  }

  private safeCancelled(job: AgentStartJob): boolean {
    try {
      return job.cancelled();
    } catch {
      return true;
    }
  }

  /** 줄이나 슬롯이 남아 있으면 다음 점검을 하나만 예약한다. */
  private schedule(): void {
    if (this.disposed || this.timer !== null) return;
    if (this.queue.length === 0 && this.slots.size === 0) return;
    this.timer = this.setTimer(() => {
      this.timer = null;
      this.pump();
    }, START_POLL_MS);
  }
}

/**
 * 작업(workload)별로 마지막에 본 터미널 모습 — 터미널이 보고한 마지막 제목
 * (OSC 0/2)과 마지막으로 감지한 에이전트(claude/codex/opencode).
 *
 * 데몬은 끝난 작업을 UI를 다시 켜도 기억하지만(메모리, 최근 64개) 제목은
 * 실행 시점 값("zsh")만 알고, 에이전트 감지값은 에이전트가 끝나면 비운다.
 * pane 메타(terminalTitle·agent)도 창을 닫으면 사라지므로, 닫힌 터미널이
 * "최근 종료"·"배치 안 됨" 목록에서 마지막 제목과 에이전트 표시로 남도록
 * UI가 여기에 보관하고 로컬에 저장한다. 제목 문자열과 에이전트 id만 저장한다
 * — 출력·환경 변수는 넣지 않는다.
 */

import type { WorkloadSummary } from "../generated/WorkloadSummary";
import { isKnownAgent } from "../features/terminal/agentNames";
import { persistedTitle, sanitizeOscTitle } from "../features/terminal/osc";
import type { PaneMeta } from "./workbenchStore";

export interface RememberedWorkload {
  /** 터미널이 마지막으로 보고한 제목(OSC 0/2). 아직 보고 전이면 null. */
  title: string | null;
  /** 이 터미널에서 마지막으로 감지한 에이전트 id. 에이전트가 끝나도 남는다. */
  agent: string | null;
  /**
   * 끝난 이 작업을 이어받은 작업(복구·재실행으로 되살린 새 workload id).
   * 있으면 이 작업은 관리 작업의 새 작업으로 옮겨 간 것이므로 "최근 종료"에 보이지 않는다.
   */
  recoveredBy?: string | null;
}

export type WorkloadMemory = Readonly<Record<string, RememberedWorkload>>;

/**
 * 셸이 끝내는 명령 자체를 보고한 제목인가. oh-my-zsh 같은 설정은 명령을 실행하기
 * 직전(preexec)에 그 명령줄을 제목으로 보내므로, `exit`로 끝낸 터미널의 마지막
 * 제목은 늘 "exit"가 된다. 그런 제목은 터미널을 부를 이름이 못 되므로 기억하지
 * 않고 목록에서도 쓰지 않는다 — 그 직전 제목(프롬프트의 경로 등)이 이름으로 남는다.
 */
const SHELL_EXIT_TITLE = /^(?:exit|logout|bye)(?:\s+-?\d+)?$/;

export function isShellExitTitle(title: string): boolean {
  return SHELL_EXIT_TITLE.test(title.trim());
}

/** 기억한 작업 모습(없으면 null). 자기 속성만 읽는다. */
export function rememberedWorkload(memory: WorkloadMemory, workloadId: string): RememberedWorkload | null {
  return Object.hasOwn(memory, workloadId) ? memory[workloadId] : null;
}

/**
 * 제목을 기억한다. 이미 같은 제목이거나 종료 명령 제목이면 같은 객체를 돌려준다
 * (구독자·저장을 깨우지 않게).
 */
export function rememberWorkloadTitle(memory: WorkloadMemory, workloadId: string, title: string): WorkloadMemory {
  const current = rememberedWorkload(memory, workloadId);
  if (current?.title === title || isShellExitTitle(title)) return memory;
  // 계산된 키는 자기 속성으로 들어간다(`__proto__`도 setter를 타지 않는다).
  return { ...memory, [workloadId]: { agent: null, ...current, title } };
}

/** 끝난 작업을 새 작업이 이어받았다고 기억한다. 같은 연결이면 같은 객체를 돌려준다. */
export function rememberWorkloadRecovery(
  memory: WorkloadMemory,
  fromWorkloadId: string,
  toWorkloadId: string,
): WorkloadMemory {
  const current = rememberedWorkload(memory, fromWorkloadId);
  if (fromWorkloadId === toWorkloadId || current?.recoveredBy === toWorkloadId) return memory;
  return { ...memory, [fromWorkloadId]: { title: null, agent: null, ...current, recoveredBy: toWorkloadId } };
}

/**
 * 작업 요약에 감지된 에이전트를 기억한다. 에이전트가 끝나 요약에서 사라져도
 * 지우지 않는다 — "마지막으로 실행한 에이전트"이기 때문이다. 다른 에이전트가
 * 감지되면 그것으로 바꾼다. 바뀐 것이 없으면 같은 객체를 돌려준다.
 */
export function rememberWorkloadAgents(memory: WorkloadMemory, workloads: readonly WorkloadSummary[]): WorkloadMemory {
  let next = memory;
  for (const workload of workloads) {
    const agent = workload.agent?.agent;
    if (!agent || !isKnownAgent(agent)) continue;
    const current = rememberedWorkload(next, workload.workload_id);
    if (current?.agent === agent) continue;
    next = { ...next, [workload.workload_id]: { title: null, ...current, agent } };
  }
  return next;
}

/**
 * 작업 목록 전체(snapshot) 기준으로 정리한다: 목록에도 없고 어느 pane도
 * 가리키지 않는 작업은 버린다. 버릴 것이 없으면 같은 객체를 돌려준다.
 */
export function pruneWorkloadMemory(
  memory: WorkloadMemory,
  workloads: readonly WorkloadSummary[],
  panes: Record<string, PaneMeta>,
): WorkloadMemory {
  const keep = new Set<string>(workloads.map((workload) => workload.workload_id));
  for (const pane of Object.values(panes)) {
    if (pane.workloadId) keep.add(pane.workloadId);
  }
  const entries = Object.entries(memory);
  const kept = entries.filter(([workloadId]) => keep.has(workloadId));
  return kept.length === entries.length ? memory : Object.fromEntries(kept);
}

/**
 * 한 작업 갱신(upsert)에서 목록 상한이 밀어낸 작업만 버린다. 이벤트 하나는
 * 목록 전체가 아니므로, 아직 목록에 오지 않은 작업(시작 직후·앱 복원 뒤 첫
 * snapshot 전)은 건드리지 않는다.
 */
export function forgetDroppedWorkloads(
  memory: WorkloadMemory,
  before: readonly WorkloadSummary[],
  after: readonly WorkloadSummary[],
): WorkloadMemory {
  if (before.length === after.length) return memory;
  const retained = new Set(after.map((workload) => workload.workload_id));
  const dropped = new Set(
    before
      .map((workload) => workload.workload_id)
      .filter((workloadId) => !retained.has(workloadId) && Object.hasOwn(memory, workloadId)),
  );
  return dropped.size === 0
    ? memory
    : Object.fromEntries(Object.entries(memory).filter(([workloadId]) => !dropped.has(workloadId)));
}

// ---- 로컬 저장

export const WORKLOAD_MEMORY_STORAGE_KEY = "iyagi.workload-memory.v1";
/** 데몬이 보존하는 끝난 작업(64개)과 살아 있는 작업을 함께 담고도 남는 상한. */
export const WORKLOAD_MEMORY_RETAINED = 128;
const MAX_WORKLOAD_ID_LEN = 128;

type StoredEntry =
  | [workloadId: string, title: string | null, agent: string | null]
  | [workloadId: string, title: string | null, agent: string | null, recoveredBy: string];

/**
 * 삽입 순서 그대로 [workload id, 제목, 에이전트(, 이어받은 작업)] 배열로 쓴다 — 넘치면
 * 오래된 것부터 버린다. 이어받은 작업이 없으면 네 번째 칸을 쓰지 않는다.
 */
export function encodeWorkloadMemory(memory: WorkloadMemory): string {
  const entries: StoredEntry[] = Object.entries(memory)
    .slice(-WORKLOAD_MEMORY_RETAINED)
    .map(([workloadId, remembered]): StoredEntry =>
      remembered.recoveredBy
        ? [workloadId, persistedTitle(remembered.title), remembered.agent, remembered.recoveredBy]
        : [workloadId, persistedTitle(remembered.title), remembered.agent],
    );
  return JSON.stringify(entries);
}

/** 망가진 항목은 그것만 버린다. 제목은 OSC 제목과 같은 규칙으로 다시 다듬고, 모르는 에이전트 id는 버린다. */
export function decodeWorkloadMemory(raw: string | null): Record<string, RememberedWorkload> {
  if (!raw) return {};
  try {
    const value: unknown = JSON.parse(raw);
    if (!Array.isArray(value)) return {};
    const entries: [string, RememberedWorkload][] = [];
    for (const entry of value.slice(-WORKLOAD_MEMORY_RETAINED)) {
      if (!Array.isArray(entry)) continue;
      const [workloadId, rawTitle, rawAgent, rawRecoveredBy] = entry as unknown[];
      if (!isWorkloadId(workloadId)) continue;
      const title = typeof rawTitle === "string" ? sanitizeOscTitle(rawTitle) : null;
      const agent = typeof rawAgent === "string" && isKnownAgent(rawAgent) ? rawAgent : null;
      const recoveredBy = isWorkloadId(rawRecoveredBy) && rawRecoveredBy !== workloadId ? rawRecoveredBy : null;
      if (recoveredBy !== null) entries.push([workloadId, { title, agent, recoveredBy }]);
      else if (title !== null || agent !== null) entries.push([workloadId, { title, agent }]);
    }
    return Object.fromEntries(entries);
  } catch {
    return {};
  }
}

function isWorkloadId(value: unknown): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= MAX_WORKLOAD_ID_LEN;
}

export function readWorkloadMemory(): Record<string, RememberedWorkload> {
  try {
    return decodeWorkloadMemory(globalThis.localStorage?.getItem(WORKLOAD_MEMORY_STORAGE_KEY) ?? null);
  } catch {
    return {};
  }
}

let lastMemory: WorkloadMemory | null = null;
let lastSaved: string | null = null;

/** 곧바로 쓴다. 기억 객체가 그대로이거나 직렬화 결과가 같으면 쓰지 않는다. */
export function saveWorkloadMemory(memory: WorkloadMemory): void {
  if (memory === lastMemory) return;
  lastMemory = memory;
  try {
    const encoded = encodeWorkloadMemory(memory);
    if (encoded === lastSaved) return;
    globalThis.localStorage?.setItem(WORKLOAD_MEMORY_STORAGE_KEY, encoded);
    lastSaved = encoded;
  } catch {
    // 저장소가 막히거나 가득 차도 이번 실행 동안은 store가 기억한다.
  }
}

/**
 * 쓰기 간격. 에이전트는 작업 중 제목 앞 스피너를 프레임마다 바꿔 보내므로
 * (초당 약 10번), 제목이 바뀔 때마다 쓰지 않고 이 간격에 한 번으로 모은다.
 */
export const WORKLOAD_MEMORY_SAVE_INTERVAL_MS = 1000;

let pendingMemory: WorkloadMemory | null = null;
let saveTimer: ReturnType<typeof setTimeout> | null = null;
let lastWriteAt = Number.NEGATIVE_INFINITY;

/**
 * store 구독용 저장. 한동안 쓰지 않았으면 곧바로 쓰고(창을 닫은 직후의 마지막
 * 제목이 늦지 않게), 간격 안의 변경은 마지막 것 하나만 간격이 끝날 때 쓴다.
 */
export function scheduleWorkloadMemorySave(memory: WorkloadMemory): void {
  if (memory === (pendingMemory ?? lastMemory)) return;
  pendingMemory = memory;
  if (saveTimer !== null) return;
  const wait = lastWriteAt + WORKLOAD_MEMORY_SAVE_INTERVAL_MS - Date.now();
  if (wait <= 0) {
    flushWorkloadMemorySave();
    return;
  }
  saveTimer = setTimeout(flushWorkloadMemorySave, wait);
}

/** 모아 둔 변경을 지금 쓴다(페이지를 떠날 때도 부른다). */
export function flushWorkloadMemorySave(): void {
  if (saveTimer !== null) clearTimeout(saveTimer);
  saveTimer = null;
  const memory = pendingMemory;
  pendingMemory = null;
  if (memory === null) return;
  lastWriteAt = Date.now();
  saveWorkloadMemory(memory);
}

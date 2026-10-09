import { newBinding } from "./configuration";
/**
 * O16 컴포넌트 시험 지원(happy-dom *.dom.test.tsx 전용).
 *
 * - createRoot + React.act로 감싼 최소 렌더기(testing-library 없이 기존
 *   의존성만으로 상호작용을 시험한다).
 * - missionStore에 직접 entity를 심는 fixture 빌더(artifact 본문은
 *   MockDaemonClient 업로드로 만든다).
 */

import type { ExecRecord } from "../../generated/ExecRecord";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import type { ArtifactRef } from "../../generated/ArtifactRef";
import type { Decision } from "../../generated/Decision";
import type { Message } from "../../generated/Message";
import type { Mission } from "../../generated/Mission";
import type { Run } from "../../generated/Run";
import type { Task } from "../../generated/Task";
import type { TaskState } from "../../generated/TaskState";
import type { Verification } from "../../generated/Verification";
import type { Candidate } from "../../generated/Candidate";
import { MockDaemonClient } from "../daemon/mockClient";
import { useI18nStore } from "../../i18n";
import { resetMissionStoreForTests, useMissionStore } from "./store";
import { resetMissionUiStoreForTests } from "./uiStore";
import { resetMessageDraftsForTests } from "./messageSubmission";
import { decodePendingMessage, messageScope, PendingMessageConflict, setMessageJournalForTests, type MessageJournal } from "./messageJournal";
import type { MissionMessageParams } from "../../generated/MissionMessageParams";
import { resetBodyCacheForTests } from "./bodyCache";
import { resetMissionClientForTests, setMissionClient } from "./clientAccess";

export interface RenderHandle {
  container: HTMLElement;
  rerender(ui: React.ReactElement): void;
  unmount(): void;
}

export function renderUi(ui: React.ReactElement): RenderHandle {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root: Root = createRoot(container);
  act(() => {
    root.render(ui);
  });
  return {
    container,
    rerender: (next) => {
      act(() => {
        root.render(next);
      });
    },
    unmount: () => {
      act(() => {
        root.unmount();
      });
      container.remove();
    },
  };
}

export async function flushAsync(rounds = 8): Promise<void> {
  // 컴포넌트의 await 사슬(sha256 → artifact upload → mutation)에는
  // crypto.subtle처럼 이벤트 루프 턴(libuv thread pool)을 필요로 하는
  // 단계가 있다 — 마이크로태스크만 돌리면 굶어 죽는다. 매 라운드마다
  // 타이머 턴 + 마이크로태스크를 함께 흘려 준다.
  for (let i = 0; i < rounds; i += 1) {
    await act(async () => {
      await new Promise<void>((resolve) => {
        setTimeout(resolve, 0);
      });
      for (let j = 0; j < 8; j += 1) {
        await Promise.resolve();
      }
    });
  }
}

/** input/textarea 값 변경을 React가 알게 하는 이벤트. */
export function setValue(element: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement, value: string): void {
  const proto =
    element instanceof HTMLTextAreaElement
      ? HTMLTextAreaElement.prototype
      : element instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
  const setter = Object.getOwnPropertyDescriptor(proto, "value")?.set;
  act(() => {
    setter?.call(element, value);
    element.dispatchEvent(new Event(element instanceof HTMLSelectElement ? "change" : "input", { bubbles: true }));
  });
}

export function pressKey(
  element: Element,
  key: string,
  options: { isComposing?: boolean; shiftKey?: boolean; ctrlKey?: boolean } = {},
): void {
  const event = new KeyboardEvent("keydown", {
    key,
    bubbles: true,
    cancelable: true,
    shiftKey: options.shiftKey ?? false,
    ctrlKey: options.ctrlKey ?? false,
  });
  if (options.isComposing !== undefined) {
    Object.defineProperty(event, "isComposing", { value: options.isComposing });
  }
  act(() => {
    element.dispatchEvent(event);
  });
}

export function dispatch(element: Element, event: Event): void {
  act(() => {
    element.dispatchEvent(event);
  });
}

export function click(element: Element): void {
  act(() => {
    element.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

// ------------------------------------------------------------------ fixtures

let idSeed = 0;
export function nextId(prefix: string): string {
  idSeed += 1;
  return `${prefix}-${idSeed}`;
}

export function fakeRef(bytes = "1"): ArtifactRef {
  return { id: nextId("artifact"), sha256: "0".repeat(64), bytes, media_type: "text/plain" };
}

export function fakeMission(overrides: Partial<Mission> = {}): Mission {
  return {
    id: nextId("mission"),
    revision: "5",
    state: "running",
    phase: "implementing",
    title: "로그인 기능",
    repository_path: "/repo",
    repository_id: nextId("repo"),
    base_oid: "0".repeat(40),
    goal_ref: fakeRef("12"),
    requirements: [
      { id: "req-1", text: "로그인 폼이 동작한다", verification_ids: ["vcmd-1"], human_check: true },
    ],
    policy: {
      max_parallel_runs: 4,
      max_attempts_per_task: 2,
      max_repair_cycles: 1,
      max_automatic_starts: 1,
      active_time_limit_ms: "86400000",
      run_time_limit_ms: "3600000",
      max_cost_usd_micros: null,
      unknown_cost: "block",
      allow_network: false,
      allow_automatic_plan_apply: true,
      allow_recovery_of_unsent: false,
      allowed_binding_ids: [],
      allowed_roles: [],
      allowed_verification_ids: [],
      require_independent_review: true,
      require_enforced_verification: false,
    },
    role_bindings: [],
    plan_revision: 1,
    candidate_id: null,
    open_decision_count: 0,
    active_time_ms: "0",
    automatic_start_count: 0,
    created_at: "2026-09-13T01:00:00.000Z",
    updated_at: "2026-09-13T01:00:00.000Z",
    archived_at: null,
    accepted_at: null,
    failure_code: null,
    follow_up_of: null,
    ...overrides,
  };
}

export function fakeTask(missionId: string, state: TaskState, overrides: Partial<Task> = {}): Task {
  return {
    id: nextId("task"),
    mission_id: missionId,
    title: `API 구현 ${idSeed}`,
    kind: "implement",
    role: "builder",
    state,
    required: true,
    parent_task_id: null,
    depends_on: [],
    contract: {
      objective_ref: fakeRef(),
      requirement_ids: [],
      input_artifact_ids: [],
      allowed_paths: [],
      expected_outputs: [],
      verification_ids: [],
      specialty: null,
    },
    binding_id: null,
    active_run_id: null,
    ordinal: idSeed,
    attempt_count: 1,
    repair_cycle: 0,
    failure_repair_run_ids: [],
    integration: null,
    replacement_of: null,
    blocked_code: null,
    dispatch_after_unix_ms: null,
    workspace_id: null,
    created_at: "2026-09-13T01:00:00.000Z",
    updated_at: "2026-09-13T01:00:00.000Z",
    ...overrides,
  };
}

export function fakeRun(missionId: string, task: Task, state: Run["state"], overrides: Partial<Run> = {}): Run {
  const id = nextId("run");
  task.active_run_id = id;
  return {
    id,
    mission_id: missionId,
    task_id: task.id,
    attempt: task.attempt_count,
    state,
    binding_snapshot: null,
    requested_model: "GLM-5.3",
    observed_model: null,
    provider_session_id: null,
    provider_turn_id: null,
    exec_id: null,
    pty_session_id: null,
    workspace_id: null,
    fencing_token: "1",
    dispatch_state: "acknowledged",
    context_ref: fakeRef(),
    result_ref: null,
    usage: { input_tokens: "1000", output_tokens: "2000", cost_usd_micros: "1500", cost_source: "estimate" },
    last_activity_at: "2026-09-13T01:00:00.000Z",
    active_time_ms: "60000",
    started_at: "2026-09-13T01:00:00.000Z",
    ended_at: null,
    failure_code: null,
    reconciliation_ref: null,
    rate_limit: null, retry_evidence: null,
    ...overrides,
  };
}

export function fakeMessage(missionId: string, role: Message["role"], overrides: Partial<Message> = {}): Message {
  return {
    id: nextId("msg"),
    mission_id: missionId,
    target_task_id: null,
    role,
    run_id: null,
    body_ref: fakeRef(),
    delivery: "delivered",
    supersedes_message_id: null,
    created_at: "2026-09-13T01:00:00.000Z",
    ...overrides,
  };
}

export function fakeDecision(missionId: string, overrides: Partial<Decision> = {}): Decision {
  return {
    id: nextId("decision"),
    mission_id: missionId,
    requesting_run_id: null,
    kind: "product",
    state: "open",
    question_ref: fakeRef(),
    options: [
      { id: "keep", label: "기존 API 유지" },
      { id: "breaking", label: "호환성 깨기" },
    ],
    affected_task_ids: [],
    blocking: false,
    plan_revision: 1,
    candidate_id: null,
    answer_ref: null,
    selected_option_id: null,
    answer_message_id: null,
    created_at: "2026-09-13T01:00:00.000Z",
    answered_at: null,
    ...overrides,
  };
}

export function fakeCandidate(missionId: string, overrides: Partial<Candidate> = {}): Candidate {
  return {
    id: nextId("candidate"),
    mission_id: missionId,
    revision: 1,
    base_oid: "0".repeat(40),
    tree_oid: "1".repeat(40),
    commit_oid: "a".repeat(40),
    source_run_ids: [],
    manifest_ref: fakeRef(),
    created_at: "2026-09-13T01:00:00.000Z",
    supersedes_id: null,
    ...overrides,
  };
}

export function fakeVerification(missionId: string, candidateId: string, overrides: Partial<Verification> = {}): Verification {
  return {
    id: nextId("verification"),
    mission_id: missionId,
    candidate_id: candidateId,
    task_id: nextId("task"),
    run_id: nextId("run"),
    command_snapshot_ref: fakeRef(),
    environment_ref: fakeRef(),
    requirement_ids: ["req-1"],
    status: "passed",
    input_integrity: "observed",
    exit_code: 0,
    log_ref: fakeRef(),
    started_at: "2026-09-13T01:00:00.000Z",
    ended_at: "2026-09-13T01:00:00.000Z",
    ...overrides,
  };
}

/** missionStore에 entity들을 통째로 심는다(각 시험 독립 실행 보장). */
export function seedStore(entities: {
  missions?: Mission[];
  tasks?: Task[];
  runs?: Run[];
  execs?: ExecRecord[];
  messages?: Message[];
  decisions?: Decision[];
  candidates?: Candidate[];
  verifications?: Verification[];
}): void {
  useMissionStore.setState((state) => {
    const apply = <T extends { id: string }>(current: Record<string, T>, list: T[] | undefined) => {
      if (!list) return current;
      const next = { ...current };
      for (const item of list) next[item.id] = item;
      return next;
    };
    return {
      missions: apply(state.missions, entities.missions),
      tasks: apply(state.tasks, entities.tasks),
      runs: apply(state.runs, entities.runs),
      execs: apply(state.execs, entities.execs),
      messages: apply(state.messages, entities.messages),
      decisions: apply(state.decisions, entities.decisions),
      candidates: apply(state.candidates, entities.candidates),
      verifications: apply(state.verifications, entities.verifications),
    };
  });
}

/** 매 시험 전 상태를 되돌린다. happy-dom의 navigator는 en-US라 문구 단언이
 * ko 정본과 일치하도록 언어를 고정한다. */
export function resetAllMissionState(): void {
  useI18nStore.setState({ language: "ko" });
  resetMissionStoreForTests();
  resetMissionUiStoreForTests();
  resetMessageDraftsForTests();
  setMessageJournalForTests(memoryMessageJournalForTests());
  resetBodyCacheForTests();
  resetMissionClientForTests();
}

/** DOM tests inject storage; browser tests exercise the actual IndexedDB path. */
export function memoryMessageJournalForTests(): MessageJournal {
  const rows = new Map<string, MissionMessageParams>();
  const key = (p: MissionMessageParams) => messageScope(p.mission_id, p.target_task_id, p.supersedes_message_id ?? null);
  const copy = (p: MissionMessageParams) => decodePendingMessage({ version: 1, params: p }, key(p));
  const check = (p: MissionMessageParams) => {
    const old = rows.get(key(p));
    if (old && JSON.stringify(copy(old)) !== JSON.stringify(copy(p))) throw new PendingMessageConflict(copy(old));
  };
  return {
    load: async scope => rows.has(scope) ? copy(rows.get(scope)!) : null,
    save: async p => { check(p); rows.set(key(p), copy(p)); },
    forget: async p => { check(p); rows.delete(key(p)); },
  };
}

/** MockDaemonClient를 mutation seam으로 물리고 돌려준다. */
export function installMockClient(options?: ConstructorParameters<typeof MockDaemonClient>[0]): MockDaemonClient {
  const client = new MockDaemonClient(options);
  setMissionClient(client);
  return client;
}

/** mock에 텍스트 artifact를 올려 ref를 얻는다(fixture의 body_ref용). */
export async function uploadMockText(client: MockDaemonClient, text: string): Promise<ArtifactRef> {
  const bytes = new TextEncoder().encode(text);
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  const sha256 = [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
  const begin = await client.artifactBegin({
    request_id: nextId("req"),
    mission_id: null,
    media_type: "text/plain; charset=utf-8",
    bytes: String(bytes.byteLength),
    sha256,
  });
  let offset = 0;
  for (; offset < bytes.byteLength; offset += begin.chunk_bytes) {
    const slice = bytes.subarray(offset, Math.min(offset + begin.chunk_bytes, bytes.byteLength));
    let binary = "";
    for (const byte of slice) binary += String.fromCharCode(byte);
    await client.artifactWrite({ upload_id: begin.upload_id, offset: String(offset), data_b64: btoa(binary) });
  }
  return client.artifactCommit({ upload_id: begin.upload_id });
}

/** Explicit UI protocol fixture; never evidence for a production CLI. */
export function compatibleBinding() {
  const binding = newBinding();
  for (const key of Object.keys(binding.capabilities) as Array<keyof typeof binding.capabilities>) {
    binding.capabilities[key] = {supported:true, reason_code:null};
  }
  binding.runtime_version = "ui-fixture-v1";
  binding.checked_at = "2026-09-16T00:00:00Z";
  return binding;
}

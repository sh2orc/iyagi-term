/**
 * 브라우저 미리보기 전용 시나리오 패드(O16 — dev/test build에서만 설치).
 *
 * plain browser(vite dev)에서는 createDaemonClient가 MockDaemonClient를
 * 돌려준다. Playwright(test:mission-ui)가 같은 경로를 진짜 화면으로
 * 검사할 수 있게 mock의 seeding helper를 window.__mockDaemon으로 노출한다.
 * production build에서는 호출부가 import.meta.env.DEV로 걸러져 사라진다.
 */

import type { DaemonClient } from "../daemon/client";
import { RpcClientError } from "../daemon/client";
import { MockDaemonClient } from "../daemon/mockClient";
import type { Candidate } from "../../generated/Candidate";
import type { Decision } from "../../generated/Decision";
import type { Entity } from "../../generated/Entity";
import type { Message } from "../../generated/Message";
import type { MessageDelivery } from "../../generated/MessageDelivery";
import type { Mission } from "../../generated/Mission";
import type { Run } from "../../generated/Run";
import type { Task } from "../../generated/Task";
import type { TaskState } from "../../generated/TaskState";
import type { Verification } from "../../generated/Verification";
import type { ArtifactRef } from "../../generated/ArtifactRef";
import { resetMissionStoreForTests, useMissionStore } from "./store";
import { resetMissionUiStoreForTests } from "./uiStore";
import { resetBodyCacheForTests } from "./bodyCache";
import { resetMissionClientForTests, setMissionClient } from "./clientAccess";
import { useWorkbenchStore } from "../../store/workbenchStore";

export interface HarnessMissionOptions {
  title?: string;
  goal?: string;
  /** 처음부터 심을 작업(에이전트) 수. */
  agents?: number;
  /** live run 상태(기본 running). */
  runState?: Run["state"];
}

export interface MockDaemonTestApi {
  readonly client: DaemonClient;
  /** mock store + UI store를 초기화한다(각 시험 시작점). */
  reset(): void;
  /** mission을 만들고 시작해 task/run/goal 메시지를 심는다. 반환은 mission id. */
  seedMission(options?: HarnessMissionOptions): Promise<string>;
  /** 진행 중 mission에 task(+run)를 추가(U03: 1→4→12). */
  addAgents(missionId: string, count: number, state?: TaskState): Promise<string[]>;
  /** Lead 대화 메시지 추가(U07: queued/unknown 전달 상태 포함). */
  addMessage(
    missionId: string,
    options: { role?: Message["role"]; text: string; delivery?: MessageDelivery; runId?: string | null },
  ): Promise<string>;
  /** open decision 추가(U11). */
  openDecision(
    missionId: string,
    options?: { question?: string; options?: Array<{ id: string; label: string }>; blocking?: boolean },
  ): Promise<string>;
  /**
   * 확정 대기 상태로 만든다(U16): 데몬 형식 manifest의 candidate, 끝난 작업·실행,
   * 검증 할 일+observed 검증(allPassed=false면 실패), 독립 리뷰 완료 증거.
   * human check와 observed 확인은 화면에서 직접 체크해야 확정할 수 있다.
   */
  stageForAcceptance(missionId: string, options?: { allPassed?: boolean }): Promise<{ candidateId: string }>;
  /** 현재 candidate를 새 candidate로 교체(U16: accept binding 무효화). */
  replaceCandidate(missionId: string): Promise<{ candidateId: string }>;
  /** 다음 mission.control(start) 1회만 실패(U02: start 실패→재시도). */
  failNextStart(): void;
  /** mission 목록. */
  missions(): Mission[];
  /** UI 쪽: 이 mission의 탭을 열고 snapshot을 뽑는다(시험 구동용). */
  openMissionTab(missionId: string): string | null;
  /** 현재 활성 탭 id — focus steal 여부 단언용(U03/U11). */
  getActiveTabId(): string | null;
  /** 열려 있는 탭 수. */
  getTabCount(): number;
}

declare global {
  interface Window {
    __mockDaemon?: MockDaemonTestApi;
  }
}

function uuid(): string {
  return typeof crypto !== "undefined" && typeof crypto.randomUUID === "function"
    ? crypto.randomUUID()
    : `id-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

/** task ordinal 시드 — addAgents가 이어서 증가시킨다(plan 순서 유지). */
let ordinalSeed = 100;

/** MockDaemonClient.repositoryInspect가 돌려주는 HEAD — 확정 대기 시나리오의 base로 쓴다. */
const MOCK_REPOSITORY_HEAD = "b".repeat(40);

/** 브라우저 dev 미리보기에만 설치한다(production/Tauri에서는 호출 자체가 없다). */
export function installMockDaemonTestHarness(client: DaemonClient): void {
  if (typeof window === "undefined") return;
  if (!(client instanceof MockDaemonClient)) return;
  window.__mockDaemon = createHarness(client);
}

function createHarness(mock: MockDaemonClient): MockDaemonTestApi {

  const upload = async (text: string): Promise<ArtifactRef> => {
    const bytes = new TextEncoder().encode(text);
    const digest = await crypto.subtle.digest("SHA-256", bytes);
    const sha256 = [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
    const begin = await mock.artifactBegin({
      request_id: uuid(),
      mission_id: null,
      media_type: "text/plain; charset=utf-8",
      bytes: String(bytes.byteLength),
      sha256,
    });
    let offset = 0;
    for (; offset < bytes.byteLength; offset += begin.chunk_bytes) {
      const slice = bytes.subarray(offset, Math.min(offset + begin.chunk_bytes, bytes.byteLength));
      await mock.artifactWrite({
        upload_id: begin.upload_id,
        offset: String(offset),
        data_b64: bytesToBase64(slice),
      });
    }
    return mock.artifactCommit({ upload_id: begin.upload_id });
  };

  const currentRevision = (missionId: string): string => {
    const mission = mock.missions.find((m) => m.id === missionId);
    if (!mission) throw new Error(`unknown mission ${missionId}`);
    return mission.revision;
  };

  /** 다음 mission.control(start) 1회만 실패시킨다(U02) — 즉시 wrapper를
   * 깐다(대화상자는 harness.control을 거치지 않고 client를 직접 호출한다). */
  const failNextStart = (): void => {
    const original = MockDaemonClient.prototype.missionControl;
    (mock as { missionControl?: unknown }).missionControl = async (
      params: Parameters<typeof original>[0],
    ) => {
      if (params.action === "start") {
        // 소유 property를 지워 원래 메서드로 복귀 — 1회만 실패.
        delete (mock as { missionControl?: unknown }).missionControl;
        throw new RpcClientError("INVALID_STATE", "주입된 시작 실패(harness)");
      }
      return original.call(mock, params);
    };
  };

  const control = async (missionId: string, action: "start" | "pause" | "resume" | "cancel"): Promise<void> => {
    await mock.missionControl({
      request_id: uuid(),
      mission_id: missionId,
      expected_revision: currentRevision(missionId),
      action,
    });
  };

  /** harness가 심은 작업·실행(최신본) — 확정 대기로 넘길 때 끝난 상태로 바꾼다. */
  const seededWork = new Map<string, Map<string, Entity>>();
  /** stageForAcceptance가 만든 검증·리뷰 할 일 — 후보 교체 때 새 증거를 남긴다. */
  const stagedEvidence = new Map<string, { verifyTask: Task; reviewTask: Task; commandRef: ArtifactRef; environmentRef: ArtifactRef }>();

  const seed = (missionId: string, entities: readonly Entity[]): void => {
    const known = seededWork.get(missionId) ?? new Map<string, Entity>();
    for (const entity of entities) {
      if (entity.kind === "task") known.set(entity.value.id, entity);
      else if (entity.kind === "run") known.set(entity.value.id, entity);
    }
    seededWork.set(missionId, known);
    mock.seedMissionEntities(missionId, entities);
  };

  /** 데몬 integration manifest 형식(workflow.rs manifest_document). */
  const manifestDocument = (
    baseOid: string,
    commitOid: string,
    treeOid: string,
    entries: Array<{ path: string; change: "added" | "modified" | "deleted"; bytes: number }>,
  ): string =>
    JSON.stringify({
      base_oid: baseOid,
      commit_oid: commitOid,
      entries: entries.map((entry) => ({
        bytes: entry.change === "deleted" ? 0 : entry.bytes,
        change: entry.change,
        path: entry.path,
        sha256: entry.change === "deleted" ? "" : "0".repeat(64),
      })),
      sources: [],
      tree_oid: treeOid,
    });

  /**
   * 후보 하나에 대한 검증 통과(observed)와 독립 리뷰 완료 증거를 심는다.
   * 데몬처럼 검증 할 일의 계약이 명령(verify-login)을 담고, 리뷰 실행 결과가 후보를 가리킨다.
   */
  const evidenceFor = async (
    missionId: string,
    candidate: Candidate,
    staged: { verifyTask: Task; reviewTask: Task; commandRef: ArtifactRef; environmentRef: ArtifactRef },
    attempt: number,
    passed: boolean,
  ): Promise<Entity[]> => {
    const at = (offsetMs: number) => new Date(Date.parse(candidate.created_at) + offsetMs).toISOString();
    const logRef = await upload(
      `iyagi verification log\ncommand: 로그인 시험 ("npm" ["test"])\ncandidate: ${candidate.id}\nexit_code: ${passed ? 0 : 1}\ntimed_out: false\n--- stdout (retained 12 of 12 bytes) ---\n1 test passed\n--- stderr (retained 0 of 0 bytes) ---\n`,
    );
    const verifyTask: Task = { ...staged.verifyTask, attempt_count: attempt, active_run_id: null, state: "succeeded" };
    const verifyRun: Run = {
      ...makeRun(missionId, verifyTask, "succeeded"),
      attempt,
      requested_model: null,
      usage: { input_tokens: "0", output_tokens: "0", cost_usd_micros: "0", cost_source: "estimate" },
      started_at: at(1_000),
      ended_at: at(5_000),
      last_activity_at: at(5_000),
    };
    verifyTask.active_run_id = null;
    const verification: Verification = {
      id: uuid(),
      mission_id: missionId,
      candidate_id: candidate.id,
      task_id: verifyTask.id,
      run_id: verifyRun.id,
      command_snapshot_ref: staged.commandRef,
      environment_ref: staged.environmentRef,
      requirement_ids: ["req-1"],
      status: passed ? "passed" : "failed",
      input_integrity: "observed",
      exit_code: passed ? 0 : 1,
      log_ref: logRef,
      started_at: at(1_000),
      ended_at: at(5_000),
    };
    const reportRef = await upload("독립 리뷰: 차단·주요 지적 없음");
    const resultRef = await upload(
      JSON.stringify({ kind: "review", candidate_id: candidate.id, findings: [], report_ref: reportRef }),
    );
    const reviewTask: Task = { ...staged.reviewTask, attempt_count: attempt, active_run_id: null, state: "succeeded" };
    const reviewRun: Run = {
      ...makeRun(missionId, reviewTask, "succeeded"),
      attempt,
      result_ref: resultRef,
      started_at: at(6_000),
      ended_at: at(20_000),
      last_activity_at: at(20_000),
    };
    reviewTask.active_run_id = null;
    stagedEvidence.set(missionId, { ...staged, verifyTask, reviewTask });
    return [
      { kind: "task", value: verifyTask },
      { kind: "run", value: verifyRun },
      { kind: "verification", value: verification },
      { kind: "task", value: reviewTask },
      { kind: "run", value: reviewRun },
    ];
  };

  const makeTask = (missionId: string, index: number, state: TaskState): { task: Task; contractRef: ArtifactRef } => {
    const id = uuid();
    const contractRef: ArtifactRef = { id: uuid(), sha256: "0".repeat(64), bytes: "0", media_type: "text/plain" };
    const task: Task = {
      id,
      mission_id: missionId,
      title: `작업 ${index + 1}`,
      kind: "implement",
      role: index % 3 === 0 ? "builder" : index % 3 === 1 ? "reviewer" : "test_author",
      state,
      required: true,
      parent_task_id: null,
      depends_on: [],
      contract: {
        objective_ref: contractRef,
        requirement_ids: [],
        input_artifact_ids: [],
        allowed_paths: [],
        expected_outputs: [],
        verification_ids: [],
        specialty: null,
      },
      binding_id: null,
      active_run_id: null,
      ordinal: index,
      attempt_count: 1,
      repair_cycle: 0,
      failure_repair_run_ids: [],
    integration: null,
      replacement_of: null,
      blocked_code: null,
    dispatch_after_unix_ms: null,
      workspace_id: null,
      created_at: new Date().toISOString(),
      updated_at: new Date().toISOString(),
    };
    return { task, contractRef };
  };

  const makeRun = (missionId: string, task: Task, state: Run["state"]): Run => {
    const id = uuid();
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
      context_ref: { id: uuid(), sha256: "0".repeat(64), bytes: "0", media_type: "text/plain" },
      result_ref: null,
      usage: { input_tokens: "1200", output_tokens: "3400", cost_usd_micros: "2500", cost_source: "estimate" },
      last_activity_at: new Date().toISOString(),
      active_time_ms: "60000",
      started_at: new Date().toISOString(),
      ended_at: null,
      failure_code: null,
      reconciliation_ref: null,
    rate_limit: null, retry_evidence: null,
    };
  };

  return {
    client: mock,
    reset() {
      seededWork.clear();
      stagedEvidence.clear();
      mock.resetMissions();
      resetMissionStoreForTests();
      resetMissionUiStoreForTests();
      resetBodyCacheForTests();
      resetMissionClientForTests();
      setMissionClient(mock);
    },
    async seedMission(options = {}) {
      const goalText = options.goal ?? "로그인 기능을 구현해 주세요.";
      const goalRef = await upload(goalText);
      const created = await mock.missionCreate({
        request_id: uuid(),
        title: options.title ?? "로그인 기능",
        repository_path: "/repo",
        expected_base_oid: "",
        goal_ref: goalRef,
        requirements: [
          { id: "req-1", text: "로그인 폼이 동작한다", verification_ids: ["verify-login"], human_check: true },
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
      });
      const missionId = created.mission_id;
      await control(missionId, "start");
      const agentCount = options.agents ?? 3;
      const entities: Entity[] = [];
      for (let i = 0; i < agentCount; i++) {
        const { task } = makeTask(missionId, i, "running");
        const run = makeRun(missionId, task, options.runState ?? "running");
        entities.push({ kind: "task", value: task }, { kind: "run", value: run });
      }
      const goalMessage: Message = {
        id: uuid(),
        mission_id: missionId,
        target_task_id: null,
        role: "user",
        run_id: null,
        body_ref: goalRef,
        delivery: "delivered",
        supersedes_message_id: null,
        created_at: new Date().toISOString(),
      };
      entities.push({ kind: "message", value: goalMessage });
      if (entities.length > 0) seed(missionId, entities);
      return missionId;
    },
    async addAgents(missionId, count, state = "running") {
      ordinalSeed += count;
      const baseOrdinal = ordinalSeed;
      const ids: string[] = [];
      const entities: Entity[] = [];
      for (let i = 0; i < count; i++) {
        const { task } = makeTask(missionId, baseOrdinal + i, state);
        const run = makeRun(missionId, task, state === "running" ? "running" : "succeeded");
        ids.push(task.id);
        entities.push({ kind: "task", value: task }, { kind: "run", value: run });
      }
      seed(missionId, entities);
      return ids;
    },
    async addMessage(missionId, options) {
      const bodyRef = await upload(options.text);
      const message: Message = {
        id: uuid(),
        mission_id: missionId,
        target_task_id: null,
        role: options.role ?? "agent",
        run_id: options.runId ?? null,
        body_ref: bodyRef,
        delivery: options.delivery ?? "delivered",
        supersedes_message_id: null,
        created_at: new Date().toISOString(),
      };
      seed(missionId, [{ kind: "message", value: message }]);
      return message.id;
    },
    async openDecision(missionId, options = {}) {
      const questionRef = await upload(options.question ?? "기존 API 호환 범위를 선택해 주세요.");
      const decision: Decision = {
        id: uuid(),
        mission_id: missionId,
        requesting_run_id: null,
        kind: "product",
        state: "open",
        question_ref: questionRef,
        options: options.options ?? [
          { id: "keep", label: "기존 API 유지" },
          { id: "breaking", label: "호환성 깨기" },
        ],
        affected_task_ids: [],
        blocking: options.blocking ?? false,
        plan_revision: 1,
        candidate_id: null,
        answer_ref: null,
        selected_option_id: null,
        answer_message_id: null,
        created_at: new Date().toISOString(),
        answered_at: null,
      };
      seed(missionId, [{ kind: "decision", value: decision }]);
      return decision.id;
    },
    async stageForAcceptance(missionId, options = {}) {
      const existing = mock.missions.find((m) => m.id === missionId);
      if (!existing) throw new Error(`unknown mission ${missionId}`);
      // mock repository.inspect의 HEAD를 작업 시작 커밋으로 둔다(가져오기 경고 없이 시작).
      const baseOid = MOCK_REPOSITORY_HEAD;
      const commitOid = "a".repeat(40);
      const treeOid = "1".repeat(40);
      const manifestRef = await upload(
        manifestDocument(baseOid, commitOid, treeOid, [
          { path: "src/login.test.ts", change: "added", bytes: 812 },
          { path: "src/login.ts", change: "added", bytes: 1834 },
        ]),
      );
      const candidate: Candidate = {
        id: uuid(),
        mission_id: missionId,
        revision: 1,
        base_oid: baseOid,
        tree_oid: treeOid,
        commit_oid: commitOid,
        source_run_ids: [],
        manifest_ref: manifestRef,
        created_at: new Date(Date.now() - 60_000).toISOString(),
        supersedes_id: null,
      };
      const commandRef = await upload(
        JSON.stringify({
          id: "verify-login",
          title: "로그인 시험",
          program: "npm",
          argv: ["test"],
          revision: "1",
          repository_id: existing.repository_id,
          cwd_relative: ".",
          timeout_ms: 600_000,
          env_profile_ref: null,
          allowed_network: false,
        }),
      );
      const environmentRef = await upload(JSON.stringify({ cwd: existing.repository_path, input_integrity: "observed" }));
      ordinalSeed += 2;
      const { task: verifyTask } = makeTask(missionId, ordinalSeed, "succeeded");
      verifyTask.kind = "verify";
      verifyTask.role = null;
      verifyTask.title = "verify: 로그인 시험";
      verifyTask.contract = { ...verifyTask.contract, objective_ref: commandRef, requirement_ids: ["req-1"], verification_ids: ["verify-login"] };
      const { task: reviewTask } = makeTask(missionId, ordinalSeed + 1, "succeeded");
      reviewTask.kind = "review";
      reviewTask.role = "reviewer";
      reviewTask.title = "독립 리뷰";
      const entities: Entity[] = [{ kind: "candidate", value: candidate }];
      // 확정 대기에는 live 실행이 없다 — harness가 심은 작업·실행을 끝낸다.
      for (const entity of seededWork.get(missionId)?.values() ?? []) {
        if (entity.kind === "task") {
          entities.push({ kind: "task", value: { ...entity.value, state: "succeeded", active_run_id: null } });
        } else if (entity.kind === "run" && ["prepared", "starting", "running", "awaiting_input", "stopping"].includes(entity.value.state)) {
          entities.push({ kind: "run", value: { ...entity.value, state: "succeeded", ended_at: candidate.created_at } });
        }
      }
      entities.push(
        ...(await evidenceFor(missionId, candidate, { verifyTask, reviewTask, commandRef, environmentRef }, 1, options.allPassed !== false)),
      );
      const mission = mock.missions.find((m) => m.id === missionId) ?? existing;
      entities.push({
        kind: "mission",
        value: { ...mission, phase: "awaiting_acceptance", candidate_id: candidate.id, base_oid: baseOid },
      });
      seed(missionId, entities);
      return { candidateId: candidate.id };
    },
    async replaceCandidate(missionId) {
      const mission = mock.missions.find((m) => m.id === missionId);
      if (!mission) throw new Error(`unknown mission ${missionId}`);
      const baseOid = mission.base_oid || MOCK_REPOSITORY_HEAD;
      const commitOid = "c".repeat(40);
      const treeOid = "2".repeat(40);
      const manifestRef = await upload(
        manifestDocument(baseOid, commitOid, treeOid, [{ path: "src/login.ts", change: "modified", bytes: 1902 }]),
      );
      const candidate: Candidate = {
        id: uuid(),
        mission_id: missionId,
        revision: 2,
        base_oid: baseOid,
        tree_oid: treeOid,
        commit_oid: commitOid,
        source_run_ids: [],
        manifest_ref: manifestRef,
        created_at: new Date(Date.now() - 30_000).toISOString(),
        supersedes_id: mission.candidate_id,
      };
      const entities: Entity[] = [{ kind: "candidate", value: candidate }];
      // 새 후보에도 검증·리뷰 증거를 새로 남긴다(이전 후보의 증거는 재사용되지 않는다).
      const staged = stagedEvidence.get(missionId);
      if (staged) {
        entities.push(...(await evidenceFor(missionId, candidate, staged, staged.verifyTask.attempt_count + 1, true)));
      }
      const latest = mock.missions.find((m) => m.id === missionId) ?? mission;
      entities.push({ kind: "mission", value: { ...latest, candidate_id: candidate.id } });
      seed(missionId, entities);
      return { candidateId: candidate.id };
    },
    failNextStart,
    missions() {
      return mock.missions;
    },
    openMissionTab(missionId: string) {
      const mission = mock.missions.find((m) => m.id === missionId);
      const tabId = useWorkbenchStore
        .getState()
        .openMissionTab(missionId, mission?.title ?? "AI 작업");
      if (tabId !== null) void useMissionStore.getState().syncMission(missionId);
      return tabId;
    },
    getActiveTabId() {
      return useWorkbenchStore.getState().activeTabId;
    },
    getTabCount() {
      return useWorkbenchStore.getState().tabs.length;
    },
  };
}

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

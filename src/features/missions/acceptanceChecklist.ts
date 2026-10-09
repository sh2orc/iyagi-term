/**
 * 확정(mission.accept) 준비 체크리스트 — 데몬 `acceptance_ready`의 화면 사본.
 *
 * 기준: crates/iyagi-termd/src/mission/workflow.rs `acceptance_ready`
 * (04 §6 steps 1–7)와 `apply_accept`의 사전 검사(state == running).
 * 데몬은 첫 거절 사유 하나만 돌려주지만 화면은 모든 미충족 항목을
 * 데몬 순서대로 모아 보여 준다. `issues[0]`이 데몬이 먼저 거절할 사유다.
 *
 * 화면 전용 규칙: 확정 버튼은 phase가 `awaiting_acceptance`일 때만 연다
 * (데몬은 phase를 보지 않는다).
 *
 * 순수 함수 — store·I/O·시계 없음. 사용자가 체크한 확인(관찰 검증,
 * 직접 확인 요구사항, 불확실 실행)은 두 번째 인자로 받는다.
 *
 * `snapshot.verifications`는 데몬 snapshot 순서(store 삽입 순서 — 데몬 accept가 읽는
 * materialize 순서와 같다)로 넘긴다. 무결성 검사(step 4)는 데몬처럼 요구 명령마다 이 순서의
 * "첫 통과 검증" 하나만 본다. 화면 표시용 목록(통과 검증 id 등)은 시작 순으로 정렬한다.
 */

import type { Candidate } from "../../generated/Candidate";
import type { Decision } from "../../generated/Decision";
import type { Finding } from "../../generated/Finding";
import type { FindingSeverity } from "../../generated/FindingSeverity";
import type { InputIntegrity } from "../../generated/InputIntegrity";
import type { Mission } from "../../generated/Mission";
import type { MissionState } from "../../generated/MissionState";
import type { Phase } from "../../generated/Phase";
import type { Run } from "../../generated/Run";
import type { RunState } from "../../generated/RunState";
import type { Task } from "../../generated/Task";
import type { TaskState } from "../../generated/TaskState";
import type { Verification } from "../../generated/Verification";
import type { VerificationStatus } from "../../generated/VerificationStatus";
import { isExperimentalBinding } from "./bindingSupport";

export interface AcceptanceSnapshot {
  mission: Mission;
  tasks: readonly Task[];
  runs: readonly Run[];
  verifications: readonly Verification[];
  findings: readonly Finding[];
  decisions: readonly Decision[];
  /** mission.candidate_id의 Candidate(리뷰 완료 판정에 필요). 없으면 리뷰 미완료. */
  candidate?: Candidate | null;
  /**
   * 리뷰 실행 id → 그 실행의 Review 결과(result_ref)가 가리키는 candidate_id.
   * 읽은 실행만 담는다. 값이 없는 실행은 "후보 생성 이후 시작한 실행"으로 근사한다.
   */
  reviewedCandidateByRunId?: Readonly<Record<string, string | null>>;
}

export interface AcceptanceAcknowledgements {
  /** 사용자가 한계를 확인한 관찰(observed) 검증 id. */
  verificationIds: readonly string[];
  /** 사용자가 직접 확인한 human_check 요구사항 id. */
  humanRequirementIds: readonly string[];
  /** 사용자가 이전 영향을 검토한 불확실(unknown/interrupted) 실행 id. */
  reconciledRunIds: readonly string[];
}

export const NO_ACKNOWLEDGEMENTS: AcceptanceAcknowledgements = {
  verificationIds: [],
  humanRequirementIds: [],
  reconciledRunIds: [],
};

/** 데몬 reason_code와 같은 이름을 쓴다(다른 것은 주석 표기). */
export type AcceptanceIssue =
  /** apply_accept: only a running mission can be accepted(데몬 reason_code `not_awaiting_acceptance`). */
  | { code: "mission_not_running"; state: MissionState }
  /** 화면 전용: 확정 대기 단계가 아니다. */
  | { code: "not_awaiting_acceptance"; phase: Phase }
  /** CandidateMismatch { current: None }(데몬 reason_code `candidate_mismatch`). */
  | { code: "candidate_missing" }
  | { code: "required_task_not_succeeded"; taskId: string; state: TaskState }
  | { code: "verification_missing"; requirementId: string; commandId: string; taskId: string | null }
  | {
      code: "verification_not_passed_on_candidate";
      requirementId: string;
      commandId: string;
      verificationId: string;
      status: VerificationStatus;
      taskId: string;
    }
  | { code: "integrity_policy_unmet"; commandId: string; verificationId: string; integrity: InputIntegrity }
  | { code: "observed_not_acknowledged"; commandId: string; verificationId: string }
  | { code: "review_incomplete"; taskId: string | null }
  | { code: "open_finding"; findingId: string; severity: FindingSeverity }
  | { code: "human_check_missing"; requirementId: string }
  | { code: "live_run"; runId: string; taskId: string; state: RunState }
  | { code: "unknown_run"; runId: string; taskId: string; state: RunState; hasEvidence: boolean }
  | { code: "open_blocking_decision"; decisionId: string };

export type AcceptanceIssueCode = AcceptanceIssue["code"];

export type ChecklistCategory =
  | "stage"
  | "tasks"
  | "verifications"
  | "integrity"
  | "review"
  | "findings"
  | "confirmations"
  | "runs"
  | "decisions";

export interface ChecklistItem {
  category: ChecklistCategory;
  ok: boolean;
  issues: AcceptanceIssue[];
}

export type CommandStatus = "passed" | "failed" | "missing";

export interface CommandEvidence {
  commandId: string;
  status: CommandStatus;
  /** 대표 검증: 통과한 첫 검증, 없으면 처음 본 검증. */
  verificationId: string | null;
  /** 이 명령을 맡은 검증 할 일(링크용) — 검증 기록의 task, 없으면 계약에 이 명령을 가진 할 일. */
  taskId: string | null;
  /** 현재 후보에서 통과한 모든 검증 id(시작 순). */
  passedVerificationIds: string[];
}

export type RequirementStatus = "passed" | "failed" | "missing" | "no_command";

export interface RequirementEvidence {
  requirementId: string;
  status: RequirementStatus;
  commandIds: string[];
}

export interface UncertainRunEvidence {
  runId: string;
  taskId: string;
  attempt: number;
  state: RunState;
  /** 종료 증거(reconciliation_ref)가 있어야 사용자가 검토를 확인할 수 있다. */
  hasEvidence: boolean;
}

export interface AcceptanceChecklist {
  ready: boolean;
  /** 데몬 거절 순서. issues[0]이 데몬이 먼저 돌려줄 사유다. */
  issues: AcceptanceIssue[];
  /** 화면 체크리스트 행(적용되는 범주만, 고정 순서). */
  items: ChecklistItem[];
  commands: Record<string, CommandEvidence>;
  requirements: RequirementEvidence[];
  /**
   * 확인이 필요한 관찰 검증 — 요구 명령마다 데몬이 무결성을 보는 첫 통과 검증이 observed인 것.
   * strict 정책이면 비어 있다.
   */
  observedVerificationIds: string[];
  /**
   * 요구 명령의 통과 검증 중 observed 전부(확인 대상인지와 무관). 확정 요청은 사용자가 확인하지
   * 않은 관찰 검증을 싣지 않는다.
   */
  passedObservedVerificationIds: string[];
  humanRequirementIds: string[];
  uncertainRuns: UncertainRunEvidence[];
  /**
   * 정보 행(확정을 막지 않음): 실험적 연결(계약 A — 검증되지 않은 CLI 버전을 사용자 동의로
   * 실행)로 실행한 적이 있는 할 일 id(plan 순서). 데몬 acceptance_ready에는 없는 화면 전용 정보다.
   */
  experimentalTaskIds: string[];
}

const LIVE_STATES: readonly RunState[] = ["prepared", "starting", "running", "awaiting_input", "stopping"];

const CATEGORY_OF: Record<AcceptanceIssueCode, ChecklistCategory> = {
  mission_not_running: "stage",
  not_awaiting_acceptance: "stage",
  candidate_missing: "stage",
  required_task_not_succeeded: "tasks",
  verification_missing: "verifications",
  verification_not_passed_on_candidate: "verifications",
  integrity_policy_unmet: "integrity",
  observed_not_acknowledged: "confirmations",
  review_incomplete: "review",
  open_finding: "findings",
  human_check_missing: "confirmations",
  live_run: "runs",
  unknown_run: "runs",
  open_blocking_decision: "decisions",
};

/** 불확실 실행: 증거가 있으면 사용자 확인 대상, 없으면 기다려야 하는 실행 문제. */
export function issueCategory(issue: AcceptanceIssue): ChecklistCategory {
  if (issue.code === "unknown_run" && issue.hasEvidence) return "confirmations";
  return CATEGORY_OF[issue.code];
}

function byStart(a: Verification, b: Verification): number {
  if (a.started_at !== b.started_at) return a.started_at < b.started_at ? -1 : 1;
  return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
}

function timeOf(iso: string | null | undefined): number | null {
  if (!iso) return null;
  const value = Date.parse(iso);
  return Number.isNaN(value) ? null : value;
}

/** 요구 명령 id(요구사항 순서, 중복 제거). */
function requiredCommandIds(mission: Mission): string[] {
  const seen = new Set<string>();
  const ids: string[] = [];
  for (const requirement of mission.requirements) {
    for (const command of requirement.verification_ids) {
      if (!seen.has(command)) {
        seen.add(command);
        ids.push(command);
      }
    }
  }
  return ids;
}

/**
 * 데몬 `current_review_complete`의 근사: 성공한 review 할 일에 후보 출처가 아닌
 * 성공 실행이 있고, 그 결과가 현재 후보를 가리킨다. 결과 본문을 읽지 못한
 * 실행은 후보 생성 이후에 시작했는지로 판정한다.
 */
function reviewState(snapshot: AcceptanceSnapshot, tasks: readonly Task[], runs: readonly Run[]): { complete: boolean; taskId: string | null } {
  const candidate = snapshot.candidate ?? null;
  const reviewTasks = tasks.filter((task) => task.kind === "review");
  const pending = [...reviewTasks]
    .filter((task) => task.state !== "succeeded" && task.state !== "superseded")
    .sort((a, b) => b.ordinal - a.ordinal)[0];
  const fallbackTaskId = pending?.id ?? ([...reviewTasks].sort((a, b) => b.ordinal - a.ordinal)[0]?.id ?? null);
  if (!candidate || candidate.id !== snapshot.mission.candidate_id) return { complete: false, taskId: fallbackTaskId };
  const created = timeOf(candidate.created_at);
  for (const task of reviewTasks) {
    if (task.state !== "succeeded") continue;
    for (const run of runs) {
      if (run.task_id !== task.id || run.state !== "succeeded" || run.result_ref === null) continue;
      if (candidate.source_run_ids.includes(run.id)) continue;
      const known = snapshot.reviewedCandidateByRunId?.[run.id];
      if (known !== undefined) {
        if (known === candidate.id) return { complete: true, taskId: task.id };
        continue;
      }
      const started = timeOf(run.started_at);
      if (created !== null && started !== null && started >= created) return { complete: true, taskId: task.id };
    }
  }
  return { complete: false, taskId: fallbackTaskId };
}

export function acceptanceChecklist(
  snapshot: AcceptanceSnapshot,
  acknowledgements: AcceptanceAcknowledgements = NO_ACKNOWLEDGEMENTS,
): AcceptanceChecklist {
  const { mission } = snapshot;
  const candidateId = mission.candidate_id;
  const tasks = snapshot.tasks.filter((task) => task.mission_id === mission.id);
  const runs = snapshot.runs.filter((run) => run.mission_id === mission.id);
  const decisions = snapshot.decisions.filter((decision) => decision.mission_id === mission.id);
  // 데몬 snapshot 순서 그대로(무결성의 "첫 통과 검증" 판정용)와 시작 순(표시용)을 따로 둔다.
  const snapshotOrderVerifications = snapshot.verifications
    .filter((verification) => verification.mission_id === mission.id && candidateId !== null && verification.candidate_id === candidateId);
  const verifications = [...snapshotOrderVerifications].sort(byStart);
  const findings = snapshot.findings.filter((finding) => finding.mission_id === mission.id);
  const issues: AcceptanceIssue[] = [];

  // 0. apply_accept 사전 검사 + 화면 단계 규칙 + 후보 존재(step 1).
  if (mission.state !== "running") issues.push({ code: "mission_not_running", state: mission.state });
  if (mission.phase !== "awaiting_acceptance") issues.push({ code: "not_awaiting_acceptance", phase: mission.phase });
  if (candidateId === null) issues.push({ code: "candidate_missing" });

  // 2. 필수 할 일(대체된 것 제외)은 모두 succeeded.
  for (const task of [...tasks].sort((a, b) => a.ordinal - b.ordinal)) {
    if (!task.required || task.state === "superseded") continue;
    if (task.state !== "succeeded") issues.push({ code: "required_task_not_succeeded", taskId: task.id, state: task.state });
  }

  // 3. 요구 명령마다 현재 후보에서 통과 1건 이상(명령 범위는 검증 할 일의 계약).
  const tasksById = new Map(tasks.map((task) => [task.id, task]));
  const commands: Record<string, CommandEvidence> = {};
  const contractTaskFor = (command: string): string | null => {
    const owners = tasks
      .filter((task) => task.state !== "superseded" && task.contract.verification_ids.includes(command))
      .sort((a, b) => (a.kind === "verify" ? 0 : 1) - (b.kind === "verify" ? 0 : 1) || b.ordinal - a.ordinal);
    return owners[0]?.id ?? null;
  };
  const seenFor = new Map<string, Verification>();
  const passedFor = new Map<string, Verification[]>();
  for (const verification of verifications) {
    const task = tasksById.get(verification.task_id);
    if (!task) continue;
    for (const command of task.contract.verification_ids) {
      if (!seenFor.has(command)) seenFor.set(command, verification);
      if (verification.status === "passed") {
        const list = passedFor.get(command) ?? [];
        list.push(verification);
        passedFor.set(command, list);
      }
    }
  }
  const allCommands = new Set<string>(requiredCommandIds(mission));
  for (const command of seenFor.keys()) allCommands.add(command);
  for (const command of allCommands) {
    const passed = passedFor.get(command) ?? [];
    const seen = seenFor.get(command) ?? null;
    const representative = passed[0] ?? seen;
    commands[command] = {
      commandId: command,
      status: passed.length > 0 ? "passed" : seen ? "failed" : "missing",
      verificationId: representative?.id ?? null,
      taskId: representative?.task_id ?? contractTaskFor(command),
      passedVerificationIds: passed.map((verification) => verification.id),
    };
  }
  const reportedCommands = new Set<string>();
  const requirements: RequirementEvidence[] = [];
  for (const requirement of mission.requirements) {
    const statuses = requirement.verification_ids.map((command) => commands[command]?.status ?? "missing");
    requirements.push({
      requirementId: requirement.id,
      commandIds: [...requirement.verification_ids],
      status: statuses.length === 0
        ? "no_command"
        : statuses.every((status) => status === "passed")
          ? "passed"
          : statuses.includes("failed") ? "failed" : "missing",
    });
    for (const command of requirement.verification_ids) {
      if (reportedCommands.has(command)) continue;
      const evidence = commands[command];
      if (evidence.status === "passed") continue;
      reportedCommands.add(command);
      const seen = seenFor.get(command);
      if (!seen) {
        issues.push({ code: "verification_missing", requirementId: requirement.id, commandId: command, taskId: evidence.taskId });
      } else {
        issues.push({
          code: "verification_not_passed_on_candidate",
          requirementId: requirement.id,
          commandId: command,
          verificationId: seen.id,
          status: seen.status,
          taskId: seen.task_id,
        });
      }
    }
  }

  // 4. 무결성 정책: strict는 enforced만, 아니면 observed는 명시 확인, unknown은 불가.
  //    데몬(acceptance_ready step 4)과 같이 요구 명령마다 snapshot 순서의 "첫 통과 검증"
  //    하나만 본다 — 같은 명령의 다른 통과 검증은 무결성과 무관하다.
  const firstPassedFor = new Map<string, Verification>();
  for (const verification of snapshotOrderVerifications) {
    if (verification.status !== "passed") continue;
    const task = tasksById.get(verification.task_id);
    if (!task) continue;
    for (const command of task.contract.verification_ids) {
      if (!firstPassedFor.has(command)) firstPassedFor.set(command, verification);
    }
  }
  const observedVerificationIds: string[] = [];
  const passedObservedVerificationIds: string[] = [];
  for (const command of requiredCommandIds(mission)) {
    for (const verification of passedFor.get(command) ?? []) {
      if (verification.input_integrity === "observed" && !passedObservedVerificationIds.includes(verification.id)) {
        passedObservedVerificationIds.push(verification.id);
      }
    }
    const verification = firstPassedFor.get(command);
    if (!verification || verification.input_integrity === "enforced") continue; // 통과 없음은 step 3이 이미 거절
    if (verification.input_integrity === "unknown" || mission.policy.require_enforced_verification) {
      issues.push({ code: "integrity_policy_unmet", commandId: command, verificationId: verification.id, integrity: verification.input_integrity });
      continue;
    }
    if (!observedVerificationIds.includes(verification.id)) observedVerificationIds.push(verification.id);
    if (!acknowledgements.verificationIds.includes(verification.id)) {
      issues.push({ code: "observed_not_acknowledged", commandId: command, verificationId: verification.id });
    }
  }

  // 5. 독립 리뷰 완료 + 현재 후보의 열린 blocking/major 지적 없음.
  const review = reviewState(snapshot, tasks, runs);
  if (mission.policy.require_independent_review && !review.complete) {
    issues.push({ code: "review_incomplete", taskId: review.taskId });
  }
  for (const finding of findings) {
    if (
      candidateId !== null &&
      finding.candidate_id === candidateId &&
      finding.resolution === "open" &&
      (finding.severity === "blocking" || finding.severity === "major")
    ) {
      issues.push({ code: "open_finding", findingId: finding.id, severity: finding.severity });
    }
  }

  // 6. human_check 요구사항은 모두 사용자 확인.
  const humanRequirementIds: string[] = [];
  for (const requirement of mission.requirements) {
    if (!requirement.human_check) continue;
    humanRequirementIds.push(requirement.id);
    if (!acknowledgements.humanRequirementIds.includes(requirement.id)) {
      issues.push({ code: "human_check_missing", requirementId: requirement.id });
    }
  }

  // 7. live 실행 없음, 불확실 실행은 종료 증거 + 사용자 검토, 열린 차단 결정 없음.
  const uncertainRuns: UncertainRunEvidence[] = [];
  for (const run of runs) {
    if ((LIVE_STATES as readonly string[]).includes(run.state)) {
      issues.push({ code: "live_run", runId: run.id, taskId: run.task_id, state: run.state });
      continue;
    }
    if (run.state === "unknown" || run.state === "interrupted") {
      const hasEvidence = run.reconciliation_ref !== null;
      uncertainRuns.push({ runId: run.id, taskId: run.task_id, attempt: run.attempt, state: run.state, hasEvidence });
      if (!hasEvidence || !acknowledgements.reconciledRunIds.includes(run.id)) {
        issues.push({ code: "unknown_run", runId: run.id, taskId: run.task_id, state: run.state, hasEvidence });
      }
    }
  }
  for (const decision of decisions) {
    if (decision.state === "open" && decision.blocking) {
      issues.push({ code: "open_blocking_decision", decisionId: decision.id });
    }
  }

  // 화면 행: 적용되는 범주만 고정 순서로.
  const applicable: ChecklistCategory[] = ["stage", "tasks"];
  if (requiredCommandIds(mission).length > 0) applicable.push("verifications");
  if (requiredCommandIds(mission).length > 0 && mission.policy.require_enforced_verification) applicable.push("integrity");
  if (mission.policy.require_independent_review) applicable.push("review");
  applicable.push("findings");
  if (observedVerificationIds.length > 0 || humanRequirementIds.length > 0 || uncertainRuns.some((run) => run.hasEvidence)) {
    applicable.push("confirmations");
  }
  applicable.push("runs", "decisions");
  for (const issue of issues) {
    const category = issueCategory(issue);
    if (!applicable.includes(category)) applicable.push(category);
  }
  const ORDER: ChecklistCategory[] = ["stage", "tasks", "verifications", "integrity", "review", "findings", "confirmations", "runs", "decisions"];
  const items = ORDER.filter((category) => applicable.includes(category)).map((category) => {
    const own = issues.filter((issue) => issueCategory(issue) === category);
    return { category, ok: own.length === 0, issues: own };
  });

  // 정보: 실험적 연결로 실행한 할 일(확정 조건이 아니다 — issues에 넣지 않는다).
  const experimentalTaskIds = [...tasks]
    .sort((a, b) => a.ordinal - b.ordinal)
    .filter((task) => runs.some((run) => run.task_id === task.id && isExperimentalBinding(run.binding_snapshot, task.kind)))
    .map((task) => task.id);

  return {
    ready: issues.length === 0,
    issues,
    items,
    commands,
    requirements,
    observedVerificationIds,
    passedObservedVerificationIds,
    humanRequirementIds,
    uncertainRuns,
    experimentalTaskIds,
  };
}

/**
 * 확정 요청에 실을 확인 목록 — 현재 체크리스트가 요구하는 대상만 남긴다.
 * 검증: 요구 명령의 통과 검증 전부(enforced는 화면 확인으로 충분, observed는
 * 확인 대상이든 아니든 사용자가 체크한 것만).
 */
export function acceptancePayload(
  checklist: AcceptanceChecklist,
  acknowledgements: AcceptanceAcknowledgements,
): { verificationIds: string[]; humanRequirementIds: string[]; reconciledRunIds: string[] } {
  const verificationIds: string[] = [];
  for (const requirement of checklist.requirements) {
    for (const command of requirement.commandIds) {
      for (const id of checklist.commands[command]?.passedVerificationIds ?? []) {
        if (verificationIds.includes(id)) continue;
        if (checklist.passedObservedVerificationIds.includes(id) && !acknowledgements.verificationIds.includes(id)) continue;
        verificationIds.push(id);
      }
    }
  }
  return {
    verificationIds,
    humanRequirementIds: checklist.humanRequirementIds.filter((id) => acknowledgements.humanRequirementIds.includes(id)),
    reconciledRunIds: checklist.uncertainRuns
      .filter((run) => run.hasEvidence && acknowledgements.reconciledRunIds.includes(run.runId))
      .map((run) => run.runId),
  };
}

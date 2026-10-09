/**
 * 확정 준비 체크리스트 — 데몬 `acceptance_ready`(workflow.rs)의 거절 조건마다 하나씩.
 * (순수 함수 시험이지만 fixture가 testSupport에 있어 happy-dom 환경으로 돌린다.)
 */

import { describe, expect, it } from "vitest";
import type { Decision } from "../../generated/Decision";
import type { Finding } from "../../generated/Finding";
import type { Mission } from "../../generated/Mission";
import type { Run } from "../../generated/Run";
import type { Task } from "../../generated/Task";
import type { Verification } from "../../generated/Verification";
import type { Candidate } from "../../generated/Candidate";
import {
  acceptanceChecklist,
  acceptancePayload,
  type AcceptanceAcknowledgements,
  type AcceptanceSnapshot,
} from "./acceptanceChecklist";
import { compatibleBinding, fakeCandidate, fakeDecision, fakeMission, fakeRef, fakeRun, fakeTask, fakeVerification } from "./testSupport";

interface Stage {
  mission: Mission;
  candidate: Candidate;
  verifyTask: Task;
  verification: Verification;
  reviewTask: Task;
  reviewRun: Run;
  snapshot: AcceptanceSnapshot;
}

/** 확정 가능한 기준 상태: 검증 enforced 통과, 리뷰 완료, human check 1건(확인 필요). */
function ready(overrides: Partial<Mission> = {}): Stage {
  const mission = fakeMission({ phase: "awaiting_acceptance", state: "running", ...overrides });
  const candidate = fakeCandidate(mission.id, { created_at: "2026-09-13T02:00:00.000Z" });
  mission.candidate_id = candidate.id;
  const verifyTask = fakeTask(mission.id, "succeeded", { kind: "verify", role: null, title: "verify: 단위 시험" });
  verifyTask.contract.verification_ids = ["vcmd-1"];
  const verifyRun = fakeRun(mission.id, verifyTask, "succeeded");
  const verification = fakeVerification(mission.id, candidate.id, {
    task_id: verifyTask.id,
    run_id: verifyRun.id,
    input_integrity: "enforced",
  });
  const reviewTask = fakeTask(mission.id, "succeeded", { kind: "review", role: "reviewer" });
  const reviewRun = fakeRun(mission.id, reviewTask, "succeeded", {
    started_at: "2026-09-13T02:05:00.000Z",
    result_ref: fakeRef("40"),
  });
  const snapshot: AcceptanceSnapshot = {
    mission,
    candidate,
    tasks: [verifyTask, reviewTask],
    runs: [verifyRun, reviewRun],
    verifications: [verification],
    findings: [],
    decisions: [],
  };
  return { mission, candidate, verifyTask, verification, reviewTask, reviewRun, snapshot };
}

const HUMAN_OK: AcceptanceAcknowledgements = { verificationIds: [], humanRequirementIds: ["req-1"], reconciledRunIds: [] };

function codes(snapshot: AcceptanceSnapshot, acks: AcceptanceAcknowledgements = HUMAN_OK): string[] {
  return acceptanceChecklist(snapshot, acks).issues.map((issue) => issue.code);
}

function finding(stage: Stage, overrides: Partial<Finding> = {}): Finding {
  return {
    id: `finding-${Math.random().toString(16).slice(2)}`,
    mission_id: stage.mission.id,
    candidate_id: stage.candidate.id,
    reviewer_run_id: stage.reviewRun.id,
    severity: "major",
    path: "src/login.ts",
    line: 3,
    evidence_ref: fakeRef(),
    requirement_id: null,
    resolution: "open",
    resolution_ref: null,
    ...overrides,
  };
}

describe("acceptanceChecklist — 기준 상태", () => {
  it("모든 조건을 채우면 ready이고 행은 모두 ✓", () => {
    const stage = ready();
    const checklist = acceptanceChecklist(stage.snapshot, HUMAN_OK);
    expect(checklist.issues).toEqual([]);
    expect(checklist.ready).toBe(true);
    expect(checklist.items.every((item) => item.ok)).toBe(true);
    expect(checklist.items.map((item) => item.category)).toEqual([
      "stage", "tasks", "verifications", "review", "findings", "confirmations", "runs", "decisions",
    ]);
    expect(checklist.requirements).toEqual([{ requirementId: "req-1", status: "passed", commandIds: ["vcmd-1"] }]);
  });

  it("확인 인자가 없으면 human check가 미충족(데몬은 빈 확인 목록을 거절)", () => {
    const stage = ready();
    const checklist = acceptanceChecklist(stage.snapshot);
    expect(checklist.issues).toEqual([{ code: "human_check_missing", requirementId: "req-1" }]);
    expect(checklist.items.find((item) => item.category === "confirmations")?.ok).toBe(false);
  });
});

describe("acceptanceChecklist — 거절 조건", () => {
  it("mission_not_running: 실행 중이 아닌 미션", () => {
    const stage = ready({ state: "paused" });
    expect(codes(stage.snapshot)).toEqual(["mission_not_running"]);
  });

  it("not_awaiting_acceptance: 화면은 확정 대기 단계에서만 연다", () => {
    const stage = ready({ phase: "reviewing" });
    expect(acceptanceChecklist(stage.snapshot, HUMAN_OK).issues).toEqual([{ code: "not_awaiting_acceptance", phase: "reviewing" }]);
  });

  it("candidate_missing: 후보가 없으면 검증·리뷰도 현재 후보 기준으로 미충족", () => {
    const stage = ready();
    stage.mission.candidate_id = null;
    const found = codes(stage.snapshot);
    expect(found[0]).toBe("candidate_missing");
    expect(found).toContain("verification_missing");
    expect(found).toContain("review_incomplete");
  });

  it("required_task_not_succeeded: 대체됨·선택 할 일은 제외", () => {
    const stage = ready();
    const failed = fakeTask(stage.mission.id, "failed");
    const superseded = fakeTask(stage.mission.id, "superseded");
    const optional = fakeTask(stage.mission.id, "cancelled", { required: false });
    const cancelled = fakeTask(stage.mission.id, "cancelled");
    stage.snapshot = { ...stage.snapshot, tasks: [...stage.snapshot.tasks, failed, superseded, optional, cancelled] };
    expect(acceptanceChecklist(stage.snapshot, HUMAN_OK).issues).toEqual([
      { code: "required_task_not_succeeded", taskId: failed.id, state: "failed" },
      { code: "required_task_not_succeeded", taskId: cancelled.id, state: "cancelled" },
    ]);
  });

  it("verification_missing: 현재 후보 검증이 없으면 계약에 명령을 가진 할 일로 연결", () => {
    const stage = ready();
    stage.snapshot = { ...stage.snapshot, verifications: [] };
    expect(acceptanceChecklist(stage.snapshot, HUMAN_OK).issues).toEqual([
      { code: "verification_missing", requirementId: "req-1", commandId: "vcmd-1", taskId: stage.verifyTask.id },
    ]);
    expect(acceptanceChecklist(stage.snapshot, HUMAN_OK).requirements[0].status).toBe("missing");
  });

  it("이전 후보의 통과는 재사용하지 않는다(W05)", () => {
    const stage = ready();
    const old = { ...stage.verification, id: "verification-old", candidate_id: "candidate-old" };
    stage.snapshot = { ...stage.snapshot, verifications: [old] };
    expect(codes(stage.snapshot)).toEqual(["verification_missing"]);
  });

  it("검증 기록의 할 일이 스냅샷에 없으면 그 검증은 세지 않는다", () => {
    const stage = ready();
    stage.snapshot = { ...stage.snapshot, verifications: [{ ...stage.verification, task_id: "task-unknown" }] };
    expect(codes(stage.snapshot)).toEqual(["verification_missing"]);
  });

  it("verification_not_passed_on_candidate: 실패만 있으면 ✗, 통과가 1건이라도 있으면 ✓", () => {
    const stage = ready();
    const failed = { ...stage.verification, id: "verification-failed", status: "failed" as const, started_at: "2026-09-13T01:00:00.000Z" };
    stage.snapshot = { ...stage.snapshot, verifications: [failed] };
    expect(acceptanceChecklist(stage.snapshot, HUMAN_OK).issues).toEqual([{
      code: "verification_not_passed_on_candidate",
      requirementId: "req-1",
      commandId: "vcmd-1",
      verificationId: failed.id,
      status: "failed",
      taskId: stage.verifyTask.id,
    }]);
    expect(acceptanceChecklist(stage.snapshot, HUMAN_OK).requirements[0].status).toBe("failed");
    stage.snapshot = { ...stage.snapshot, verifications: [failed, stage.verification] };
    const checklist = acceptanceChecklist(stage.snapshot, HUMAN_OK);
    expect(checklist.ready).toBe(true);
    expect(checklist.requirements[0].status).toBe("passed");
  });

  it("요구사항 ✓는 verification.requirement_ids가 아니라 명령별 통과로 판정한다", () => {
    const stage = ready();
    stage.mission.requirements = [
      { id: "req-1", text: "A", verification_ids: ["vcmd-1"], human_check: false },
      { id: "req-2", text: "B", verification_ids: ["vcmd-1", "vcmd-2"], human_check: false },
    ];
    // 검증 기록은 req-1만 가리키지만 명령 vcmd-1 통과는 req-2에도 유효하다.
    const checklist = acceptanceChecklist(stage.snapshot, HUMAN_OK);
    expect(checklist.requirements.map((r) => r.status)).toEqual(["passed", "missing"]);
    expect(checklist.issues).toEqual([{ code: "verification_missing", requirementId: "req-2", commandId: "vcmd-2", taskId: null }]);
  });

  it("integrity_policy_unmet: strict 정책의 observed, 또는 unknown 무결성", () => {
    const strict = ready();
    strict.mission.policy = { ...strict.mission.policy, require_enforced_verification: true };
    strict.snapshot = { ...strict.snapshot, verifications: [{ ...strict.verification, input_integrity: "observed" }] };
    expect(acceptanceChecklist(strict.snapshot, HUMAN_OK).issues).toEqual([
      { code: "integrity_policy_unmet", commandId: "vcmd-1", verificationId: strict.verification.id, integrity: "observed" },
    ]);
    expect(acceptanceChecklist(strict.snapshot, HUMAN_OK).observedVerificationIds).toEqual([]);

    const unknown = ready();
    unknown.snapshot = { ...unknown.snapshot, verifications: [{ ...unknown.verification, input_integrity: "unknown" }] };
    expect(codes(unknown.snapshot)).toEqual(["integrity_policy_unmet"]);
    expect(acceptanceChecklist(unknown.snapshot, HUMAN_OK).items.map((item) => item.category)).toContain("integrity");
  });

  it("observed_not_acknowledged: 관찰 검증은 명시 확인이 있어야 한다", () => {
    const stage = ready();
    stage.snapshot = { ...stage.snapshot, verifications: [{ ...stage.verification, input_integrity: "observed" }] };
    const checklist = acceptanceChecklist(stage.snapshot, HUMAN_OK);
    expect(checklist.issues).toEqual([{ code: "observed_not_acknowledged", commandId: "vcmd-1", verificationId: stage.verification.id }]);
    expect(checklist.observedVerificationIds).toEqual([stage.verification.id]);
    expect(checklist.items.find((item) => item.category === "confirmations")?.ok).toBe(false);
    expect(acceptanceChecklist(stage.snapshot, { ...HUMAN_OK, verificationIds: [stage.verification.id] }).ready).toBe(true);
  });

  it("무결성은 데몬처럼 명령별 첫 통과 검증(snapshot 순서) 하나만 본다", () => {
    // enforced 통과가 먼저면 뒤의 observed·unknown 통과는 확인도 거절 사유도 아니다.
    const enforcedFirst = ready();
    const laterObserved = { ...enforcedFirst.verification, id: "verification-later-observed", input_integrity: "observed" as const, started_at: "2026-09-13T00:00:00.000Z" };
    const laterUnknown = { ...enforcedFirst.verification, id: "verification-later-unknown", input_integrity: "unknown" as const };
    enforcedFirst.snapshot = { ...enforcedFirst.snapshot, verifications: [enforcedFirst.verification, laterObserved, laterUnknown] };
    let checklist = acceptanceChecklist(enforcedFirst.snapshot, HUMAN_OK);
    expect(checklist.issues).toEqual([]);
    expect(checklist.observedVerificationIds).toEqual([]);
    expect(checklist.passedObservedVerificationIds).toEqual([laterObserved.id]);
    // 표시용 통과 목록은 시작 순이다(snapshot 순서와 무관).
    expect(checklist.commands["vcmd-1"].passedVerificationIds[0]).toBe(laterObserved.id);
    // strict 정책도 첫 통과가 enforced면 통과한다.
    enforcedFirst.mission.policy = { ...enforcedFirst.mission.policy, require_enforced_verification: true };
    expect(acceptanceChecklist(enforcedFirst.snapshot, HUMAN_OK).ready).toBe(true);
    // 확정 요청은 확인하지 않은 관찰 검증을 싣지 않는다.
    expect(acceptancePayload(checklist, HUMAN_OK).verificationIds).not.toContain(laterObserved.id);

    // 첫 통과가 observed면 그것 하나만 확인 대상이다.
    const observedFirst = ready();
    const firstObserved = { ...observedFirst.verification, id: "verification-first-observed", input_integrity: "observed" as const };
    observedFirst.snapshot = { ...observedFirst.snapshot, verifications: [firstObserved, observedFirst.verification] };
    checklist = acceptanceChecklist(observedFirst.snapshot, HUMAN_OK);
    expect(checklist.issues).toEqual([{ code: "observed_not_acknowledged", commandId: "vcmd-1", verificationId: firstObserved.id }]);
    expect(checklist.observedVerificationIds).toEqual([firstObserved.id]);

    // 실패한 검증은 "첫 통과"가 아니다 — 그 뒤의 enforced 통과가 무결성 대상이다.
    const failedFirst = ready();
    const failedUnknown = { ...failedFirst.verification, id: "verification-failed-unknown", status: "failed" as const, input_integrity: "unknown" as const };
    failedFirst.snapshot = { ...failedFirst.snapshot, verifications: [failedUnknown, failedFirst.verification] };
    expect(acceptanceChecklist(failedFirst.snapshot, HUMAN_OK).ready).toBe(true);
  });

  it("review_incomplete: 리뷰 할 일이 없거나, 현재 후보 이전 실행이거나, 결과가 다른 후보", () => {
    const none = ready();
    none.snapshot = { ...none.snapshot, tasks: [none.verifyTask] };
    expect(codes(none.snapshot)).toEqual(["review_incomplete"]);

    const before = ready();
    before.snapshot = { ...before.snapshot, runs: [{ ...before.reviewRun, started_at: "2026-09-13T01:30:00.000Z" }] };
    expect(acceptanceChecklist(before.snapshot, HUMAN_OK).issues).toEqual([{ code: "review_incomplete", taskId: before.reviewTask.id }]);

    const other = ready();
    other.snapshot = { ...other.snapshot, reviewedCandidateByRunId: { [other.reviewRun.id]: "candidate-old" } };
    expect(codes(other.snapshot)).toEqual(["review_incomplete"]);
    other.snapshot = { ...other.snapshot, reviewedCandidateByRunId: { [other.reviewRun.id]: other.candidate.id } };
    expect(codes(other.snapshot)).toEqual([]);

    const source = ready();
    source.candidate.source_run_ids = [source.reviewRun.id];
    expect(codes(source.snapshot)).toEqual(["review_incomplete"]);

    const noResult = ready();
    noResult.snapshot = { ...noResult.snapshot, runs: [{ ...noResult.reviewRun, result_ref: null }] };
    expect(codes(noResult.snapshot)).toEqual(["review_incomplete"]);

    const optional = ready();
    optional.mission.policy = { ...optional.mission.policy, require_independent_review: false };
    optional.snapshot = { ...optional.snapshot, tasks: [optional.verifyTask] };
    expect(codes(optional.snapshot)).toEqual([]);
    expect(acceptanceChecklist(optional.snapshot, HUMAN_OK).items.map((item) => item.category)).not.toContain("review");
  });

  it("open_finding: 현재 후보의 열린 blocking/major만", () => {
    const stage = ready();
    const blocking = finding(stage, { severity: "blocking" });
    const major = finding(stage, { severity: "major" });
    const minor = finding(stage, { severity: "minor" });
    const dismissed = finding(stage, { severity: "blocking", resolution: "dismissed" });
    const old = finding(stage, { severity: "blocking", candidate_id: "candidate-old" });
    stage.snapshot = { ...stage.snapshot, findings: [blocking, major, minor, dismissed, old] };
    expect(acceptanceChecklist(stage.snapshot, HUMAN_OK).issues).toEqual([
      { code: "open_finding", findingId: blocking.id, severity: "blocking" },
      { code: "open_finding", findingId: major.id, severity: "major" },
    ]);
  });

  it("human_check_missing: 모든 human check 요구사항을 확인해야 한다", () => {
    const stage = ready();
    stage.mission.requirements = [
      ...stage.mission.requirements,
      { id: "req-2", text: "화면이 보기 좋다", verification_ids: [], human_check: true },
    ];
    expect(acceptanceChecklist(stage.snapshot, HUMAN_OK).issues).toEqual([{ code: "human_check_missing", requirementId: "req-2" }]);
    const checklist = acceptanceChecklist(stage.snapshot, { ...HUMAN_OK, humanRequirementIds: ["req-1", "req-2"] });
    expect(checklist.ready).toBe(true);
    expect(checklist.requirements[1]).toEqual({ requirementId: "req-2", status: "no_command", commandIds: [] });
  });

  it("live_run: 살아 있는 실행이 있으면 확정하지 않는다", () => {
    const stage = ready();
    const task = fakeTask(stage.mission.id, "succeeded", { required: false });
    const live = fakeRun(stage.mission.id, task, "awaiting_input");
    stage.snapshot = { ...stage.snapshot, tasks: [...stage.snapshot.tasks, task], runs: [...stage.snapshot.runs, live] };
    expect(acceptanceChecklist(stage.snapshot, HUMAN_OK).issues).toEqual([
      { code: "live_run", runId: live.id, taskId: task.id, state: "awaiting_input" },
    ]);
  });

  it("unknown_run: 종료 증거 없음은 실행 문제, 증거가 있으면 사용자 검토 대상", () => {
    const stage = ready();
    const task = fakeTask(stage.mission.id, "succeeded");
    const noEvidence = fakeRun(stage.mission.id, task, "interrupted", { attempt: 1 });
    const evidence = fakeRun(stage.mission.id, task, "unknown", { attempt: 2, reconciliation_ref: fakeRef("20") });
    stage.snapshot = { ...stage.snapshot, tasks: [...stage.snapshot.tasks, task], runs: [...stage.snapshot.runs, noEvidence, evidence] };
    const checklist = acceptanceChecklist(stage.snapshot, { ...HUMAN_OK, reconciledRunIds: [noEvidence.id] });
    expect(checklist.issues).toEqual([
      { code: "unknown_run", runId: noEvidence.id, taskId: task.id, state: "interrupted", hasEvidence: false },
      { code: "unknown_run", runId: evidence.id, taskId: task.id, state: "unknown", hasEvidence: true },
    ]);
    expect(checklist.items.find((item) => item.category === "runs")?.issues.map((issue) => issue.code)).toEqual(["unknown_run"]);
    expect(checklist.items.find((item) => item.category === "confirmations")?.issues).toHaveLength(1);
    expect(checklist.uncertainRuns).toEqual([
      { runId: noEvidence.id, taskId: task.id, attempt: 1, state: "interrupted", hasEvidence: false },
      { runId: evidence.id, taskId: task.id, attempt: 2, state: "unknown", hasEvidence: true },
    ]);
    const onlyEvidence = { ...stage.snapshot, runs: stage.snapshot.runs.filter((run) => run.id !== noEvidence.id) };
    expect(acceptanceChecklist(onlyEvidence, { ...HUMAN_OK, reconciledRunIds: [evidence.id] }).ready).toBe(true);
  });

  it("open_blocking_decision: 열린 차단 결정만", () => {
    const stage = ready();
    const blocking: Decision = fakeDecision(stage.mission.id, { blocking: true });
    const soft = fakeDecision(stage.mission.id, { blocking: false });
    const answered = fakeDecision(stage.mission.id, { blocking: true, state: "answered" });
    stage.snapshot = { ...stage.snapshot, decisions: [blocking, soft, answered] };
    expect(acceptanceChecklist(stage.snapshot, HUMAN_OK).issues).toEqual([{ code: "open_blocking_decision", decisionId: blocking.id }]);
  });

  it("다른 미션의 엔티티는 섞이지 않는다", () => {
    const stage = ready();
    const otherTask = fakeTask("mission-other", "failed");
    const otherRun = fakeRun("mission-other", otherTask, "running");
    const otherDecision = fakeDecision("mission-other", { blocking: true });
    stage.snapshot = {
      ...stage.snapshot,
      tasks: [...stage.snapshot.tasks, otherTask],
      runs: [...stage.snapshot.runs, otherRun],
      decisions: [otherDecision],
    };
    expect(codes(stage.snapshot)).toEqual([]);
  });

  it("여러 사유는 데몬 거절 순서대로 나열된다(issues[0] = 데몬이 먼저 돌려줄 사유)", () => {
    const stage = ready();
    const failedTask = fakeTask(stage.mission.id, "failed");
    const live = fakeRun(stage.mission.id, failedTask, "running");
    const decision = fakeDecision(stage.mission.id, { blocking: true });
    stage.snapshot = {
      ...stage.snapshot,
      tasks: [...stage.snapshot.tasks, failedTask],
      runs: [...stage.snapshot.runs, live],
      verifications: [{ ...stage.verification, input_integrity: "observed" }],
      findings: [finding(stage, { severity: "blocking" })],
      decisions: [decision],
    };
    expect(codes(stage.snapshot, { verificationIds: [], humanRequirementIds: [], reconciledRunIds: [] })).toEqual([
      "required_task_not_succeeded",
      "observed_not_acknowledged",
      "open_finding",
      "human_check_missing",
      "live_run",
      "open_blocking_decision",
    ]);
  });
});

describe("acceptancePayload", () => {
  it("요구 명령의 통과 검증과 체크한 확인만 싣는다", () => {
    const stage = ready();
    const observed = { ...stage.verification, id: "verification-observed", input_integrity: "observed" as const };
    const task = fakeTask(stage.mission.id, "succeeded");
    const evidence = fakeRun(stage.mission.id, task, "unknown", { reconciliation_ref: fakeRef("20") });
    const unended = fakeRun(stage.mission.id, task, "interrupted");
    stage.snapshot = {
      ...stage.snapshot,
      tasks: [...stage.snapshot.tasks, task],
      runs: [...stage.snapshot.runs, evidence, unended],
      verifications: [stage.verification, observed],
    };
    const acks: AcceptanceAcknowledgements = {
      verificationIds: ["verification-stale"],
      humanRequirementIds: ["req-1", "req-stale"],
      reconciledRunIds: [evidence.id, unended.id, "run-stale"],
    };
    const checklist = acceptanceChecklist(stage.snapshot, acks);
    expect(acceptancePayload(checklist, acks)).toEqual({
      verificationIds: [stage.verification.id],
      humanRequirementIds: ["req-1"],
      reconciledRunIds: [evidence.id],
    });
    const withObserved = { ...acks, verificationIds: [observed.id] };
    expect(acceptancePayload(acceptanceChecklist(stage.snapshot, withObserved), withObserved).verificationIds).toEqual([
      stage.verification.id,
      observed.id,
    ]);
  });
});

describe("acceptanceChecklist — 실험적 연결 정보(계약 A)", () => {
  it("실험적 연결로 실행한 할 일을 plan 순서로 모으지만 확정 조건(issues·행 ok)에는 넣지 않는다", () => {
    const stage = ready();
    const experimental = compatibleBinding();
    experimental.capabilities.read_only = { supported: true, reason_code: "experimental_opt_in" };
    const flaggedReview = { ...stage.reviewRun, binding_snapshot: experimental };
    const snapshot: AcceptanceSnapshot = { ...stage.snapshot, runs: [stage.snapshot.runs[0], flaggedReview] };
    const checklist = acceptanceChecklist(snapshot, HUMAN_OK);
    expect(checklist.experimentalTaskIds).toEqual([stage.reviewTask.id]);
    expect(checklist.ready).toBe(true);
    expect(checklist.issues).toEqual([]);
    expect(checklist.items.every((item) => item.ok)).toBe(true);

    const plain = acceptanceChecklist({ ...stage.snapshot, runs: [stage.snapshot.runs[0], { ...stage.reviewRun, binding_snapshot: compatibleBinding() }] }, HUMAN_OK);
    expect(plain.experimentalTaskIds).toEqual([]);
  });
});

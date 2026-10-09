/**
 * DecisionPanel(05-ui §8): 결정 요청 한 건.
 *
 * 형식 고정: 상황 1문장 → (판단 자료: 계획 할 일·파일 변경·충돌 파일·영향 작업·
 * 자유 입력) → 선택지 버튼(각 버튼 아래 누르면 생기는 일 1줄, 패널 하단 고정)
 * → `자세히`(긴 경고/설명·원문).
 *
 * - 답변은 mission.decision.answer(option_id, answer_ref). 자유 입력은 artifact로
 *   올린 뒤 answer_ref로 보낸다. 전송은 mutateWithResync, 오류는 MissionErrorNotice.
 * - 저장됨/전달됨을 과장하지 않는다 — mutation 성공은 `답변됨`으로만 표시한다.
 * - obsolete 질문은 버튼 disabled + 현재 질문으로 가는 링크.
 * - 낭독(aria-live)은 배너가 맡는다 — 패널은 낭독 영역을 만들지 않는다.
 */

import { useEffect, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import type { ArtifactRef } from "../../generated/ArtifactRef";
import type { Decision } from "../../generated/Decision";
import type { DecisionOption } from "../../generated/DecisionOption";
import type { Mission } from "../../generated/Mission";
import type { Role } from "../../generated/Role";
import type { Run } from "../../generated/Run";
import type { Task } from "../../generated/Task";
import { useI18n } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { uploadTextArtifact } from "./artifactUpload";
import { getMissionClient } from "./clientAccess";
import { PolicyLimitsEditor } from "./CostPolicyEditor";
import { isCostDecision } from "./costs";
import { MissionErrorNotice, missionError, withSupportedActions, type MissionUiError } from "./errors";
import { FileChangeApproval, fileChangeRequest, approvalText } from "./FileChangeApproval";
import { blockedReasonLabel, decisionOptionEffect, decisionOptionLabel, roleLabel } from "./labels";
import { ModelReassignPicker } from "./ModelReassignPicker";
import { mutateWithResync } from "./mutation";
import { RecoveryCoverageNotice } from "./RecoveryNotice";
import { useMissionStore } from "./store";
import { isInternalIntegration, newRequestId, useArtifactText } from "./viewUtils";
import "./decisions.css";

export interface DecisionPanelProps {
  mission: Mission;
  decision: Decision;
  /** obsolete 안내의 이동 대상(현재 open decision). */
  currentDecisionId: string | null;
  onJumpToCurrent: () => void;
  /** 영향 작업 제목을 누르면 해당 할 일을 연다(MissionPage가 연결). */
  onOpenTask?: (taskId: string) => void;
}

export function isTaskFailureDecision(decision: Decision | null): boolean {
  return decision?.kind === "recovery" && decision.options.some((o) => o.id === "stop_failed_mission");
}

export function isReconciledDecision(decision: Decision | null): boolean {
  return decision?.kind === "recovery" && decision.options.some((o) => o.id === "stop_reconciled_mission");
}

export function isReviewRepairDecision(decision: Decision | null): boolean {
  return decision?.kind === "budget" && decision.options.some(o => o.id === "stop_review_repair");
}

export function isIntegrationConflictDecision(decision: Decision | null): boolean {
  return decision?.kind === "conflict" && decision.requesting_run_id != null;
}

const LIMIT_CODES = ["active_time_limit", "automatic_start_limit"] as const;
type LimitCode = (typeof LIMIT_CODES)[number];

/** 실행 예산(활성 시간/자동 시작) 결정이면 그 code. 영향 작업의 blocked_code, 없으면 질문 원문으로 판정한다. */
export function decisionLimitCode(decision: Decision, affected: readonly Task[], questionText: string | null): LimitCode | null {
  if (decision.kind !== "budget") return null;
  for (const task of affected) {
    const code = LIMIT_CODES.find((candidate) => task.blocked_code === candidate);
    if (code) return code;
  }
  if (questionText) {
    const code = LIMIT_CODES.find((candidate) => questionText.includes(`(${candidate})`));
    if (code) return code;
  }
  return null;
}

/** 모델 변경으로 풀 수 있는 실패 code. */
const MODEL_FAILURES = ["MODEL_UNAVAILABLE", "CAPABILITY_UNSUPPORTED", "PROVIDER_RATE_LIMITED"];

function conflictPaths(text: string | null | undefined): string[] {
  if (!text) return [];
  try {
    const paths: unknown = JSON.parse(text)?.conflict?.paths;
    return Array.isArray(paths) && paths.every((path): path is string => typeof path === "string") ? paths : [];
  } catch { return []; }
}

export interface ProposalTaskView {
  id: string;
  title: string;
  role: Role | null;
  required: boolean;
  dependsOn: string[];
}

export interface ProposalView {
  tasks: ProposalTaskView[];
  retire: string[];
  rationaleRef: ArtifactRef | null;
}

/** 계획 결정의 질문 본문(PlanProposal JSON)을 화면용으로 파싱. 형식이 아니면 null. */
export function parsePlanProposal(text: string | null | undefined): ProposalView | null {
  if (!text) return null;
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return null;
  }
  if (!value || typeof value !== "object") return null;
  const record = value as { tasks?: unknown; retire_task_ids?: unknown; rationale_ref?: unknown };
  if (!Array.isArray(record.tasks)) return null;
  const tasks: ProposalTaskView[] = [];
  for (const raw of record.tasks) {
    if (!raw || typeof raw !== "object") return null;
    const task = raw as { id?: unknown; title?: unknown; role?: unknown; required?: unknown; depends_on?: unknown };
    if (typeof task.id !== "string" || typeof task.title !== "string") return null;
    tasks.push({
      id: task.id,
      title: task.title,
      role: typeof task.role === "string" ? (task.role as Role) : null,
      required: task.required !== false,
      dependsOn: Array.isArray(task.depends_on) ? task.depends_on.filter((id): id is string => typeof id === "string") : [],
    });
  }
  const retire = Array.isArray(record.retire_task_ids)
    ? record.retire_task_ids.filter((id): id is string => typeof id === "string")
    : [];
  const ref = record.rationale_ref as Partial<ArtifactRef> | undefined;
  const rationaleRef = ref && typeof ref === "object" && typeof ref.id === "string" && typeof ref.bytes === "string"
    ? (ref as ArtifactRef)
    : null;
  return { tasks, retire, rationaleRef };
}

/** decision.answer의 answer_ref 본문 상한(01-contracts §8: UTF-8 64 KiB). */
export const DECISION_ANSWER_MAX_BYTES = 64 * 1024;

export interface ProviderBlockedView {
  taskTitle: string | null;
  code: string | null;
  reportRef: ArtifactRef | null;
  attempt: number | null;
  limit: number | null;
  message: string | null;
}

/** provider `blocked` 결정의 질문 본문(`{"kind":"provider_blocked",…}`)을 파싱. 형식이 아니면 null. */
export function parseProviderBlocked(text: string | null | undefined): ProviderBlockedView | null {
  if (!text) return null;
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return null;
  }
  if (!value || typeof value !== "object") return null;
  const record = value as Record<string, unknown>;
  if (record.kind !== "provider_blocked") return null;
  const str = (field: unknown) => (typeof field === "string" && field.length > 0 ? field : null);
  const num = (field: unknown) => (typeof field === "number" && Number.isFinite(field) ? field : null);
  const ref = record.report_ref as Partial<ArtifactRef> | null | undefined;
  return {
    taskTitle: str(record.task_title),
    code: str(record.code),
    reportRef: ref && typeof ref === "object" && typeof ref.id === "string" && typeof ref.bytes === "string" ? (ref as ArtifactRef) : null,
    attempt: num(record.attempt_count),
    limit: num(record.max_attempts_per_task),
    message: str(record.message),
  };
}

/** 실패 결과 본문의 첫 줄(비어 있지 않은 줄, 200자 제한). */
function firstLine(text: string | null): string | null {
  if (!text) return null;
  const line = text.split(/\r?\n/).map((part) => part.trim()).find((part) => part.length > 0);
  if (!line) return null;
  return line.length > 200 ? `${line.slice(0, 199)}…` : line;
}

type TextMode = "answer" | "revise" | "instruction" | null;

export function DecisionPanel(props: DecisionPanelProps): JSX.Element {
  const { t, language } = useI18n();
  const decision = props.decision;
  const question = useArtifactText(decision.question_ref);
  const approval = decision.kind === "approval" ? approvalText(question.text) : null;
  const fileChange = fileChangeRequest(approval);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<MissionUiError | null>(null);
  const [text, setText] = useState("");
  /** option = change_model 선택지(재배정 후 답변), remedy = 실패 결정의 모델 변경(재배정+재시도). */
  const [pickerMode, setPickerMode] = useState<"option" | "remedy" | null>(null);
  const [limitsOpen, setLimitsOpen] = useState(false);
  const [loginOpen, setLoginOpen] = useState(false);
  const composingRef = useRef(false);
  const limitsRef = useRef<HTMLDivElement | null>(null);
  const lastAnswer = useRef<{ optionId: string | null; withText: boolean } | null>(null);

  const obsolete = decision.state === "obsolete";
  const closed = obsolete || decision.state === "answered";
  const failure = isTaskFailureDecision(decision);
  const cost = isCostDecision(decision);
  const reconciled = isReconciledDecision(decision);
  const reviewRepair = isReviewRepairDecision(decision);
  const integrationConflict = isIntegrationConflictDecision(decision);
  const plan = decision.kind === "plan";
  const paths = integrationConflict ? conflictPaths(question.text) : [];
  const affected = useMissionStore(useShallow((s) =>
    decision.affected_task_ids.map((id) => s.tasks[id]).filter((task): task is Task => task !== undefined)));
  const storeTasks = useMissionStore(useShallow((s) => (plan ? Object.values(s.tasks).filter((task) => task.mission_id === decision.mission_id) : [])));
  const task = decision.affected_task_ids.length === 1 ? affected[0] ?? null : null;
  const run: Run | null = useMissionStore((s) => (decision.requesting_run_id ? s.runs[decision.requesting_run_id] ?? null : null));
  const integrationRecovery = reconciled && task != null && isInternalIntegration(task);
  const limitCode = decisionLimitCode(decision, affected, question.text);
  const proposal = plan ? parsePlanProposal(question.text) : null;
  const rationale = useArtifactText(proposal?.rationaleRef ?? null);
  const failureResult = useArtifactText(failure && run ? run.result_ref : null);
  const failureLine = failure ? firstLine(failureResult.text) : null;
  const hasOption = (id: string) => decision.options.some((option) => option.id === id);
  const hasAdjustOption = hasOption("adjust_limits");
  const limitDecision = limitCode !== null || cost || reviewRepair || hasAdjustOption;
  const blockedInfo = decision.kind === "recovery" ? parseProviderBlocked(question.text) : null;
  const providerBlocked = decision.kind === "recovery"
    && (blockedInfo !== null || hasOption("retry_with_instruction") || hasOption("change_model") || hasOption("replan"));
  const blockedReport = useArtifactText(blockedInfo?.reportRef ?? null);
  const unsent = decision.kind === "recovery" && hasOption("resume_unsent");
  const mayHaveSent = decision.kind === "recovery" && !unsent && !providerBlocked && hasOption("stop_mission") && !failure && !reconciled;
  const noOptions = decision.options.length === 0;
  const textMode: TextMode = closed ? null
    : noOptions ? "answer"
    : hasOption("retry_with_instruction") || hasOption("change_model") || hasOption("replan") ? "instruction"
    : hasOption("revise") ? "revise"
    : null;
  const trimmed = text.trim();
  const tooLarge = new TextEncoder().encode(trimmed).byteLength > DECISION_ANSWER_MAX_BYTES;
  const failureCode = failure ? run?.failure_code ?? null : null;
  const canChangeModel = !closed && task !== null && task.kind !== "verify"
    && (hasOption("change_model") || (failureCode !== null && MODEL_FAILURES.includes(failureCode)));
  const pickerAction = task?.state === "failed" || task?.state === "cancelled" ? "retry" : "reassign";

  useEffect(() => {
    if (limitsOpen) limitsRef.current?.scrollIntoView?.({ block: "nearest" });
  }, [limitsOpen]);

  // ---------------------------------------------------------------- 상황 1문장
  const attemptParams = task ? { task: task.title, attempt: task.attempt_count, limit: props.mission.policy.max_attempts_per_task } : null;
  let situation: string;
  if (fileChange) situation = t("missions.decision.fileChange");
  else if (integrationConflict) situation = t("missions.decision.situation.conflict");
  else if (reviewRepair) situation = t("missions.decision.situation.reviewRepair");
  else if (cost) situation = task ? t("missions.decision.situation.cost", { task: task.title }) : t("missions.cost.summary");
  else if (limitCode === "active_time_limit") situation = t("missions.decision.situation.activeTimeLimit");
  else if (limitCode === "automatic_start_limit") situation = t("missions.decision.situation.automaticStartLimit");
  else if (reconciled && attemptParams && run) {
    situation = t(integrationRecovery ? "missions.decision.situation.integrationRecovery" : "missions.decision.situation.reconciled", attemptParams);
  } else if (failure && attemptParams && run) {
    const reason = run.failure_code ? blockedReasonLabel(t, run.failure_code) : null;
    situation = reason && reason !== blockedReasonLabel(t, null)
      ? t("missions.decision.situation.failure", { ...attemptParams, reason })
      : t("missions.decision.situation.failurePlain", attemptParams);
  } else if (providerBlocked) {
    situation = t("missions.decision.situation.providerBlocked", {
      task: blockedInfo?.taskTitle ?? task?.title ?? "",
      attempt: blockedInfo?.attempt ?? task?.attempt_count ?? 0,
      limit: blockedInfo?.limit ?? props.mission.policy.max_attempts_per_task,
    });
  } else if (plan && proposal) situation = t("missions.decision.situation.plan", { count: proposal.tasks.length });
  else if (plan && question.text !== null) situation = t("missions.decision.situation.planUnparsed");
  else if (unsent) situation = t("missions.decision.situation.unsent");
  else if (mayHaveSent) situation = t("missions.decision.situation.mayHaveSent");
  else if (question.error) situation = t("missions.decision.questionError");
  else situation = (decision.kind === "approval" ? approval : question.text) ?? "…";

  // ---------------------------------------------------------------- 자세히(긴 설명 · 원문)
  const details: JSX.Element[] = [];
  if (integrationConflict) {
    details.push(<p key="conflict">{t("missions.integration.question")}</p>);
    if (hasOption("exclude_candidate")) {
      details.push(<p key="exclude" className="muted" data-testid="integration-exclusion-note">{t("missions.integration.excludeNote")}</p>);
    }
  } else if (reviewRepair) {
    details.push(<p key="review">{t("missions.requiredRepair.reviewExhausted")}</p>);
  } else if (cost && task) {
    details.push(<p key="cost">{t("missions.cost.blocked", { task: task.title })}</p>);
  } else if (reconciled && attemptParams && run) {
    details.push(<p key="recovery">{t(integrationRecovery ? "missions.integration.recoveryQuestion" : "missions.recovery.question", attemptParams)}</p>);
  } else if (failure && attemptParams && run) {
    details.push(<p key="failure">{t("missions.decision.taskFailure", { ...attemptParams, code: run.failure_code ?? "UNKNOWN" })}</p>);
  }
  if (blockedInfo) {
    if (blockedInfo.code) details.push(<p key="blocked-code">{t("missions.decision.blocked.code", { code: blockedInfo.code })}</p>);
    if (blockedInfo.message) details.push(<pre key="blocked-message">{blockedInfo.message}</pre>);
  }
  if (cost || limitCode !== null) details.push(<p key="admission">{t("missions.cost.admissionNote")}</p>);
  if (plan && proposal?.rationaleRef && rationale.text) {
    details.push(<div key="rationale"><strong>{t("missions.decision.plan.rationale")}</strong><pre>{rationale.text}</pre></div>);
  }
  const showOriginal = question.text && !fileChange && !integrationConflict && decision.kind !== "approval" && decision.kind !== "product"
    && !(plan && proposal) && blockedInfo === null && situation !== question.text;
  if (showOriginal) {
    details.push(<div key="original" data-testid="decision-original"><strong>{t("missions.decision.original")}</strong><pre>{question.text}</pre></div>);
  }

  // ---------------------------------------------------------------- 전송
  const answer = async (optionId: string | null, withText: boolean) => {
    lastAnswer.current = { optionId, withText };
    const client = getMissionClient();
    if (!client) {
      setError(missionError(t, new Error(t("missions.sync.noClient"))));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      // 본문은 한 번만 올린다 — CAS 재시도는 같은 불변 artifact를 다시 가리킨다.
      const answerRef = withText && trimmed ? await uploadTextArtifact(client, trimmed, { missionId: props.mission.id }) : null;
      await mutateWithResync(props.mission.id, (latest) => {
        const current = useMissionStore.getState().decisions[decision.id];
        if (current && current.state !== "open") return null;
        return client.missionDecisionAnswer({
          request_id: newRequestId(),
          mission_id: latest.id,
          expected_revision: latest.revision,
          decision_id: decision.id,
          option_id: optionId,
          answer_ref: answerRef,
        });
      });
      if (withText) setText("");
    } catch (cause) {
      setError(missionError(t, cause));
    } finally {
      setBusy(false);
    }
  };

  const chooseOption = (option: DecisionOption) => {
    if (option.id === "change_model" && task) {
      // 계약: 먼저 task.control reassign으로 다른 연결을 지정한 뒤 답한다(그대로면 model_not_changed).
      setPickerMode("option");
      return;
    }
    if (option.id === "adjust_limits") {
      // UI 동선 전용 — 답하면 policy_update_required. policy.update로 한도를 올리면 결정이 obsolete된다.
      setLimitsOpen(true);
      return;
    }
    const withText = ["revise", "retry_with_instruction", "replan"].includes(option.id);
    void answer(option.id, withText);
  };

  const optionDisabled = (option: DecisionOption) =>
    busy || closed
    || (option.id === "accept" && fileChange != null && !fileChange.details_available)
    || (["revise", "retry_with_instruction", "replan", "change_model"].includes(option.id) && tooLarge)
    || (option.id === "change_model" && !task);

  const optionLabel = (option: DecisionOption) =>
    integrationConflict && option.id === "stop_mission" ? t("missions.integration.stop")
      : integrationRecovery && option.id === "retry_reconciled_task" ? t("missions.integration.rebuild")
      : decisionOptionLabel(t, option);

  const submitText = () => {
    if (busy || tooLarge) return;
    if (textMode === "answer" && trimmed) void answer(null, true);
    else if (textMode === "instruction" && hasOption("retry_with_instruction")) void answer("retry_with_instruction", true);
    else if (textMode === "revise") void answer("revise", true);
  };

  const openSettings = () => useWorkbenchStore.getState().openSettings("missions");
  const titleOf = (id: string) => proposal?.tasks.find((candidate) => candidate.id === id)?.title
    ?? storeTasks.find((candidate) => candidate.id === id)?.title ?? null;
  const resetAt = run?.rate_limit ? new Date(Number(run.rate_limit.resets_at_unix_ms)) : null;
  const resetText = resetAt && Number.isFinite(resetAt.getTime()) ? resetAt.toLocaleString(language) : t("missions.quota.unknownReset");

  return (
    <section
      className={`mission-decision mission-decision-bounded${obsolete ? " obsolete" : ""}${decision.blocking ? " blocking" : ""}`}
      aria-labelledby={`decision-question-${decision.id}`}
      data-testid="decision-panel"
      data-decision-kind={decision.kind}
    >
      <header className="mission-decision-header">
        <h3>{t("missions.decision.title")}</h3>
        {decision.blocking ? <span className="mission-decision-blocking">{t("missions.decision.blocking")}</span> : null}
        {decision.state === "answered" ? <span className="mission-decision-answered">{t("missions.decision.answered")}</span> : null}
      </header>
      <p id={`decision-question-${decision.id}`} className="mission-decision-situation" data-testid="decision-question">
        {situation}
      </p>

      {failure && failureLine ? (
        <p className="mission-decision-failure-line" data-testid="decision-failure-message">
          {t("missions.decision.failure.message", { line: failureLine })}
        </p>
      ) : null}
      {failure && failureCode === "PROVIDER_RATE_LIMITED" ? (
        <p className="muted" data-testid="decision-rate-limit-reset">{t("missions.decision.failure.reset", { time: resetText })}</p>
      ) : null}
      {failure && !closed && (failureCode === "AUTH_REQUIRED" || canChangeModel) ? (
        <div className="mission-error-actions" data-testid="decision-remedies">
          {failureCode === "AUTH_REQUIRED" ? (
            <>
              <button type="button" aria-expanded={loginOpen} onClick={() => setLoginOpen(!loginOpen)} data-testid="decision-login-help">
                {t("missions.error.action.login")}
              </button>
              <button type="button" onClick={openSettings} data-testid="decision-open-settings">
                {t("missions.error.action.openSettings")}
              </button>
            </>
          ) : null}
          {canChangeModel && !hasOption("change_model") ? (
            <button type="button" disabled={busy} onClick={() => setPickerMode("remedy")} data-testid="decision-change-model">
              {t("missions.cost.changeModel")}
            </button>
          ) : null}
        </div>
      ) : null}
      {loginOpen ? <p className="muted" data-testid="decision-login-text">{t("missions.error.loginHelp")}</p> : null}

      {providerBlocked && blockedInfo ? (
        <div data-testid="decision-blocked">
          <strong>{t("missions.detail.blockedReport")}</strong>
          {blockedInfo.reportRef === null || blockedReport.error ? (
            <p className="muted">{t("missions.decision.blocked.reportMissing")}</p>
          ) : blockedReport.text ? (
            <pre className="mission-blocked-report" data-testid="decision-blocked-report">{blockedReport.text}</pre>
          ) : (
            <p className="muted">…</p>
          )}
        </div>
      ) : null}
      {reconciled ? <RecoveryCoverageNotice run={run} /> : null}
      {fileChange ? <FileChangeApproval request={fileChange} /> : null}
      {paths.length > 0 ? (
        <div className="mission-conflict-paths" data-testid="integration-conflict-paths">
          <strong>{t("missions.integration.paths")}</strong>
          <ul>{paths.map((path, index) => <li key={index}><code>{path}</code></li>)}</ul>
        </div>
      ) : null}
      {integrationConflict && question.error ? <p className="mission-area-error">{t("missions.decision.questionError")}</p> : null}

      {proposal && proposal.tasks.length > 0 ? (
        <div data-testid="decision-plan">
          <strong>{t("missions.decision.plan.tasks")}</strong>
          <ol className="mission-decision-plan">
            {proposal.tasks.map((item) => {
              const deps = item.dependsOn.map(titleOf).filter((title): title is string => title !== null);
              return (
                <li key={item.id} data-testid="decision-plan-task">
                  <span>{item.title}</span>{" "}
                  <span className="mission-plan-meta">
                    · {roleLabel(t, item.role)}
                    {item.required ? null : ` · ${t("missions.decision.plan.optional")}`}
                    {deps.length > 0 ? ` · ${t("missions.decision.plan.dependsOn", { titles: deps.join(", ") })}` : null}
                  </span>
                </li>
              );
            })}
          </ol>
          {proposal.retire.length > 0 ? (
            <p className="muted">
              {t("missions.decision.plan.retire", { titles: proposal.retire.map(titleOf).filter((title): title is string => title !== null).join(", ") || String(proposal.retire.length) })}
            </p>
          ) : null}
        </div>
      ) : null}

      {decision.affected_task_ids.length > 0 ? (
        <div className="mission-decision-affected-wrap" data-testid="decision-affected">
          <span className="muted">{t("missions.decision.affected", { count: decision.affected_task_ids.length })}</span>
          {affected.length > 0 ? (
            <ul className="mission-decision-affected">
              {affected.map((item) => (
                <li key={item.id}>
                  {props.onOpenTask ? (
                    <button
                      type="button"
                      className="link"
                      aria-label={t("missions.decision.openTask", { task: item.title })}
                      data-testid="decision-affected-task"
                      onClick={() => props.onOpenTask?.(item.id)}
                    >
                      {item.title}
                    </button>
                  ) : (
                    <span data-testid="decision-affected-task">{item.title}</span>
                  )}
                </li>
              ))}
            </ul>
          ) : null}
        </div>
      ) : null}

      {textMode ? (
        <label className="mission-decision-text">
          <span>{t(textMode === "answer" ? "missions.decision.text.answer" : textMode === "instruction" ? "missions.decision.text.instruction" : "missions.decision.text.revise")}</span>
          <textarea
            value={text}
            rows={3}
            disabled={busy}
            placeholder={t("missions.decision.text.placeholder")}
            data-testid="decision-text"
            onChange={(event) => setText(event.target.value)}
            onCompositionStart={() => { composingRef.current = true; }}
            onCompositionEnd={() => { composingRef.current = false; }}
            onKeyDown={(event) => {
              // 조합 중 Enter는 IME 확정이다 — 전송하지 않는다. 일반 Enter는 줄바꿈.
              if (event.key !== "Enter" || composingRef.current || event.nativeEvent.isComposing) return;
              if (!(event.ctrlKey || event.metaKey)) return;
              event.preventDefault();
              submitText();
            }}
          />
          {tooLarge ? <span className="mission-area-error">{t("missions.error.reason.answerTooLarge")}</span> : null}
          {textMode === "answer" ? (
            <span>
              <button type="button" className="primary" disabled={busy || !trimmed || tooLarge} onClick={() => void answer(null, true)} data-testid="decision-text-send">
                {t(busy ? "missions.decision.pending" : "missions.decision.text.send")}
              </button>
              {!trimmed ? <span className="muted"> {t("missions.decision.text.required")}</span> : null}
            </span>
          ) : null}
        </label>
      ) : null}

      {pickerMode !== null && task ? (
        <ModelReassignPicker
          mission={props.mission}
          task={task}
          action={pickerMode === "option" ? "reassign" : pickerAction}
          onClose={() => setPickerMode(null)}
          onDone={pickerMode === "option" ? () => void answer("change_model", true) : undefined}
        />
      ) : null}
      {limitsOpen && !closed ? (
        <div ref={limitsRef}>
          <PolicyLimitsEditor key={decision.id} mission={props.mission} onClose={() => setLimitsOpen(false)} />
        </div>
      ) : null}

      <div className="mission-decision-footer" data-testid="decision-footer">
        {decision.options.length > 0 || (limitDecision && !hasAdjustOption && !closed) ? (
          <div className="mission-decision-options">
            {decision.options.map((option) => {
              const effectId = `decision-effect-${decision.id}-${option.id}`;
              return (
                <div className="mission-decision-option" key={option.id}>
                  <button
                    type="button"
                    disabled={optionDisabled(option)}
                    aria-describedby={effectId}
                    data-testid="decision-option"
                    data-option-id={option.id}
                    onClick={() => chooseOption(option)}
                  >
                    {optionLabel(option)}
                  </button>
                  <p className="mission-decision-effect" id={effectId}>{decisionOptionEffect(t, option)}</p>
                </div>
              );
            })}
            {limitDecision && !hasAdjustOption && !closed ? (
              <div className="mission-decision-option">
                <button
                  type="button"
                  aria-expanded={limitsOpen}
                  aria-describedby={`decision-effect-${decision.id}-limits`}
                  data-testid="decision-adjust-limits"
                  onClick={() => setLimitsOpen(!limitsOpen)}
                >
                  {t("missions.decision.limits.open")}
                </button>
                <p className="mission-decision-effect" id={`decision-effect-${decision.id}-limits`}>
                  {decisionOptionEffect(t, { id: "adjust_limits", label: "" })}
                </p>
              </div>
            ) : null}
          </div>
        ) : null}
        {obsolete ? (
          <p className="mission-decision-obsolete">
            {t("missions.decision.obsolete")}{" "}
            {props.currentDecisionId ? (
              <button type="button" className="link" onClick={props.onJumpToCurrent}>
                {t("missions.decision.goToCurrent")}
              </button>
            ) : null}
          </p>
        ) : null}
        {error ? (
          <MissionErrorNotice
            error={withSupportedActions(error, task ? ["resync", "open_settings", "retry", "change_model"] : ["resync", "open_settings", "retry"])}
            onRetry={() => {
              const last = lastAnswer.current;
              if (last) void answer(last.optionId, last.withText);
            }}
            onAction={(action) => {
              if (action === "resync") void useMissionStore.getState().syncMission(props.mission.id);
              else if (action === "open_settings") openSettings();
              else if (action === "change_model" && task) setPickerMode(hasOption("change_model") ? "option" : "remedy");
            }}
          />
        ) : null}
      </div>

      {details.length > 0 ? (
        <details className="mission-decision-details" data-testid="decision-details">
          <summary>{t("missions.common.details")}</summary>
          <div>{details}</div>
        </details>
      ) : null}
    </section>
  );
}

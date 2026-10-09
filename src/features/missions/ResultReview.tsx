/**
 * ResultReview(05-ui §9): 결과 확인·확정 화면.
 *
 * 상단 요약(고정): 상태 → "확정하려면" 체크리스트 → 확정 버튼(+첫 미충족 이유)
 * → 수정 요청/버리기 → 가져오기 요약.
 * 상세(스크롤): 직접 확인할 항목 → 요구사항 충족표 → 변경 → 검증 → 리뷰 지적
 * → 남은 한계 → 결과 가져오기 → 실행 이력/사용량. 확정 후에는 가져오기를 맨 위로.
 *
 * - 체크리스트는 데몬 `acceptance_ready`의 사본(acceptanceChecklist.ts)이다.
 *   확정 버튼은 phase가 awaiting_acceptance이고 모든 조건을 채웠을 때만 열린다.
 * - 확정은 화면을 열 때 본 candidate에 binding된다. 후보가 바뀌면 disable +
 *   갱신 요구, 확인 체크는 모두 초기화한다.
 * - 원래 브랜치에는 자동 반영이 없다 — 가져오기 명령만 제공한다.
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import type { ArtifactRef } from "../../generated/ArtifactRef";
import type { Candidate } from "../../generated/Candidate";
import type { Finding } from "../../generated/Finding";
import type { Mission } from "../../generated/Mission";
import type { MissionState } from "../../generated/MissionState";
import type { Run } from "../../generated/Run";
import type { Task } from "../../generated/Task";
import type { Verification } from "../../generated/Verification";
import { useI18n } from "../../i18n";
import { useMissionStore, type MissionStoreState } from "./store";
import {
  selectCandidateFindings,
  selectCandidateVerifications,
  selectCandidateVerificationsInSnapshotOrder,
  selectMissionDecisions,
  selectMissionRuns,
  selectTaskList,
} from "./selectors";
import { getMissionClient } from "./clientAccess";
import {
  inputIntegrityNoteKey,
  missionStateLabelKey,
  phaseLabelKey,
  runStateLabel,
  severityLabel,
  taskStatusDisplay,
  verificationStatusLabelKey,
} from "./labels";
import { newRequestId, useArtifactText } from "./viewUtils";
import { readArtifactText } from "./bodyCache";
import { UsageSummary } from "./UsageSummary";
import { FindingReview } from "./FindingReview";
import {
  MANIFEST_VISIBLE_LIMIT,
  formatManifestBytes,
  manifestChangeLabelKey,
  parseCandidateManifest,
} from "./manifest";
import {
  acceptanceChecklist,
  acceptancePayload,
  type AcceptanceAcknowledgements,
  type AcceptanceIssue,
  type AcceptanceSnapshot,
  type CommandStatus,
} from "./acceptanceChecklist";
import { MissionErrorNotice, StaleActionError, missionError, type MissionErrorAction, type MissionUiError } from "./errors";
import { mutateWithResync } from "./mutation";
import { ResultImport, ResultImportSummary, useRepositoryHead } from "./ResultImport";
import { useArtifactExcerpt } from "./artifactExcerpt";
import { RunAttestedLabel } from "./RunExitAttestation";
import { WorkspaceCleanup, useWorkspaceUsage } from "./WorkspaceCleanup";
import { workspaceUsageEntry } from "./workspaceUsage";
import "./resultReview.css";

export interface ResultReviewProps {
  mission: Mission;
  onRefresh: () => void;
  /** 체크리스트·확인 항목의 할 일 링크(할 일 상세 열기). 없으면 링크를 그리지 않는다. */
  onOpenTask?: (taskId: string) => void;
  /** 체크리스트의 차단 결정 링크(결정 패널 열기). */
  onOpenDecision?: (decisionId: string) => void;
  /** "수정 요청": Lead 대화 입력창으로 이동·포커스. 없으면 버튼을 그리지 않는다. */
  onRequestChanges?: () => void;
  /** "이 결과 버리기"가 보관까지 끝났을 때(되돌리기 토스트·탭 닫기 등). */
  onArchived?: () => void;
}

interface AckState {
  verificationIds: string[];
  humanRequirementIds: string[];
  /** `${run.id}:${reconciliation_ref.sha256}` — 증거가 바뀌면 확인이 풀린다. */
  runKeys: string[];
}

const NO_ACKS: AckState = { verificationIds: [], humanRequirementIds: [], runKeys: [] };
const EMPTY_VERIFICATIONS: Verification[] = [];
const EMPTY_FINDINGS: Finding[] = [];
const TERMINAL_STATES: ReadonlySet<MissionState> = new Set(["completed", "failed", "cancelled"]);
const CANCELLABLE_STATES: ReadonlySet<MissionState> = new Set(["draft", "running", "pausing", "paused"]);
/** 리뷰 결과(AgentResult::Review) 본문 읽기 상한. */
const REVIEW_RESULT_MAX_BYTES = 256 * 1024;
const ISSUES_PER_ITEM = 5;

type DiscardPhase = "idle" | "confirm" | "cancelling" | "waiting" | "archiving" | "done" | "failed";

function runKey(run: Run): string {
  return `${run.id}:${run.reconciliation_ref?.sha256 ?? ""}`;
}

/** 검증 할 일 제목(`verify: <명령 제목>`)에서 명령 이름만. */
export function verifyCommandTitle(title: string): string {
  return title.startsWith("verify: ") ? title.slice("verify: ".length) : title;
}

function commandTitleFromSnapshot(text: string | null): string | null {
  if (!text) return null;
  try {
    const parsed = JSON.parse(text) as { title?: unknown };
    return typeof parsed.title === "string" && parsed.title.length > 0 ? parsed.title : null;
  } catch {
    return null;
  }
}

function reviewCandidateOf(text: string): string | null {
  try {
    const parsed = JSON.parse(text) as { kind?: unknown; candidate_id?: unknown };
    return parsed.kind === "review" && typeof parsed.candidate_id === "string" ? parsed.candidate_id : null;
  } catch {
    return null;
  }
}

function snapshotFor(
  state: MissionStoreState,
  mission: Mission,
  reviewed: Readonly<Record<string, string | null>>,
): AcceptanceSnapshot {
  const candidateId = mission.candidate_id;
  return {
    mission,
    candidate: candidateId ? state.candidates[candidateId] ?? null : null,
    tasks: selectTaskList(state, mission.id),
    runs: selectMissionRuns(state, mission.id),
    // 무결성 검사는 데몬처럼 snapshot 순서의 첫 통과 검증을 본다(acceptanceChecklist 참고).
    verifications: candidateId ? selectCandidateVerificationsInSnapshotOrder(state, candidateId) : [],
    findings: candidateId ? selectCandidateFindings(state, candidateId) : [],
    decisions: selectMissionDecisions(state, mission.id),
    reviewedCandidateByRunId: reviewed,
  };
}

function acknowledgementsFor(acks: AckState, runs: readonly Run[]): AcceptanceAcknowledgements {
  return {
    verificationIds: acks.verificationIds,
    humanRequirementIds: acks.humanRequirementIds,
    reconciledRunIds: runs
      .filter((run) => run.reconciliation_ref !== null && acks.runKeys.includes(runKey(run)))
      .map((run) => run.id),
  };
}

function sameIds(a: readonly string[], b: readonly string[]): boolean {
  if (a.length !== b.length) return false;
  const sorted = [...b].sort();
  return [...a].sort().every((id, index) => id === sorted[index]);
}

/**
 * 성공한 리뷰 실행의 결과 본문에서 리뷰한 candidate_id를 읽는다(데몬
 * `review_task_complete`와 같은 근거). 읽지 못한 실행은 비워 둔다.
 */
function useReviewedCandidates(tasks: readonly Task[], runs: readonly Run[]): Record<string, string | null> {
  const reviewTaskIds = new Set(tasks.filter((task) => task.kind === "review" && task.state === "succeeded").map((task) => task.id));
  const targets = runs.filter((run) => reviewTaskIds.has(run.task_id) && run.state === "succeeded" && run.result_ref !== null);
  const key = targets.map((run) => `${run.id}:${run.result_ref?.id ?? ""}`).join("|");
  const [known, setKnown] = useState<Record<string, string | null>>({});
  useEffect(() => {
    const client = getMissionClient();
    if (!client || targets.length === 0) return;
    let alive = true;
    for (const run of targets) {
      const ref = run.result_ref;
      if (!ref) continue;
      const bytes = Number(ref.bytes);
      if (!Number.isSafeInteger(bytes) || bytes > REVIEW_RESULT_MAX_BYTES) continue;
      readArtifactText(client, ref.id, bytes)
        .then((text) => {
          if (!alive) return;
          const reviewed = reviewCandidateOf(text);
          setKnown((previous) => (run.id in previous && previous[run.id] === reviewed ? previous : { ...previous, [run.id]: reviewed }));
        })
        .catch(() => undefined);
    }
    return () => {
      alive = false;
    };
    // key가 대상 실행과 결과 artifact를 모두 담는다.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  return known;
}

export function ResultReview(props: ResultReviewProps): JSX.Element {
  const { t } = useI18n();
  const { mission } = props;
  const candidateId = mission.candidate_id;
  const candidate = useMissionStore((s) => (candidateId ? s.candidates[candidateId] ?? null : null));
  const tasks = useMissionStore(useShallow((s) => selectTaskList(s, mission.id)));
  const runs = useMissionStore(useShallow((s) => selectMissionRuns(s, mission.id)));
  const decisions = useMissionStore(useShallow((s) => selectMissionDecisions(s, mission.id)));
  const verifications = useMissionStore(
    useShallow((s) => (candidateId ? selectCandidateVerifications(s, candidateId) : EMPTY_VERIFICATIONS)),
  );
  // 확정 체크리스트 입력 — 데몬 snapshot 순서(무결성의 "명령별 첫 통과 검증"이 이 순서를 따른다).
  const checklistVerifications = useMissionStore(
    useShallow((s) => (candidateId ? selectCandidateVerificationsInSnapshotOrder(s, candidateId) : EMPTY_VERIFICATIONS)),
  );
  const findings = useMissionStore(useShallow((s) => (candidateId ? selectCandidateFindings(s, candidateId) : EMPTY_FINDINGS)));
  const reviewed = useReviewedCandidates(tasks, runs);
  const tasksById = useMemo(() => new Map(tasks.map((task) => [task.id, task])), [tasks]);

  // accept binding: 이 화면이 확인하기 시작한 candidate(05 §9).
  const boundCandidateIdRef = useRef<string | null>(candidateId);
  const [, setBindingTick] = useState(0);
  useEffect(() => {
    if (boundCandidateIdRef.current === null && candidateId !== null) {
      boundCandidateIdRef.current = candidateId;
      setBindingTick((tick) => tick + 1);
    }
  }, [candidateId]);
  const boundCandidateId = boundCandidateIdRef.current;
  const candidateChanged = boundCandidateId !== null && candidateId !== null && boundCandidateId !== candidateId;

  // 사용자 확인 체크 — 후보가 바뀌면 전부 풀린다.
  const [acks, setAcks] = useState<AckState>(NO_ACKS);
  useEffect(() => {
    setAcks(NO_ACKS);
  }, [candidateId]);
  const acknowledgements = useMemo(() => acknowledgementsFor(acks, runs), [acks, runs]);
  const checklist = useMemo(
    () =>
      acceptanceChecklist(
        { mission, candidate, tasks, runs, verifications: checklistVerifications, findings, decisions, reviewedCandidateByRunId: reviewed },
        acknowledgements,
      ),
    [mission, candidate, tasks, runs, checklistVerifications, findings, decisions, reviewed, acknowledgements],
  );
  const payload = acceptancePayload(checklist, acknowledgements);
  const accepted = mission.state === "completed" && mission.accepted_at !== null;
  const head = useRepositoryHead(candidate ? mission.repository_path : "", accepted ? 1 : 0);
  // 확정 뒤에만 작업 공간 정리를 제안한다(계약 D — 자동 삭제 없음, 조회 실패는 숨김).
  const workspace = useWorkspaceUsage(mission.id, accepted);

  const [busy, setBusy] = useState(false);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [acceptError, setAcceptError] = useState<MissionUiError | null>(null);
  const confirmRef = useRef<HTMLDivElement | null>(null);
  const detailsRef = useRef<HTMLDivElement | null>(null);
  const importRef = useRef<HTMLElement | null>(null);

  const acceptBlocked = accepted || candidateChanged || candidate === null || !checklist.ready;
  useEffect(() => {
    if (confirmOpen) confirmRef.current?.scrollIntoView?.({ block: "nearest" });
  }, [confirmOpen]);

  // ------------------------------------------------------------ 문구 도우미

  const taskTitle = (taskId: string) => tasksById.get(taskId)?.title ?? t("missions.result.unknownTask");
  const commandName = (commandId: string): string => {
    const owner = tasks.find((task) => task.kind === "verify" && task.contract.verification_ids.includes(commandId));
    return owner ? verifyCommandTitle(owner.title) : commandId;
  };
  const runAttempt = (runId: string) => runs.find((run) => run.id === runId)?.attempt ?? 0;

  const issueText = (issue: AcceptanceIssue): string => {
    switch (issue.code) {
      case "mission_not_running":
        return t("missions.result.issue.missionNotRunning", { state: t(missionStateLabelKey(issue.state)) });
      case "not_awaiting_acceptance":
        return t("missions.result.issue.notAwaitingAcceptance", { phase: t(phaseLabelKey(issue.phase)) });
      case "candidate_missing":
        return t("missions.result.issue.candidateMissing");
      case "required_task_not_succeeded":
        return t("missions.result.issue.requiredTask", {
          task: taskTitle(issue.taskId),
          state: t(taskStatusDisplay(issue.state, null).labelKey),
        });
      case "verification_missing":
        return t("missions.result.issue.verificationMissing", { command: commandName(issue.commandId) });
      case "verification_not_passed_on_candidate":
        return t("missions.result.issue.verificationNotPassed", {
          command: commandName(issue.commandId),
          status: t(verificationStatusLabelKey(issue.status)),
        });
      case "integrity_policy_unmet":
        return t("missions.result.issue.integrityUnmet", { command: commandName(issue.commandId) });
      case "observed_not_acknowledged":
        return t("missions.result.issue.observedUnacknowledged", { command: commandName(issue.commandId) });
      case "review_incomplete":
        return t("missions.result.issue.reviewIncomplete");
      case "open_finding": {
        const found = findings.find((item) => item.id === issue.findingId);
        return t("missions.result.issue.openFinding", {
          severity: severityLabel(t, issue.severity),
          location: found?.path ? ` (${found.path}${found.line !== null ? `:${found.line}` : ""})` : "",
        });
      }
      case "human_check_missing": {
        const requirement = mission.requirements.find((item) => item.id === issue.requirementId);
        return t("missions.result.issue.humanCheckMissing", { requirement: requirement?.text ?? issue.requirementId });
      }
      case "live_run":
        return t("missions.result.issue.liveRun", {
          task: taskTitle(issue.taskId),
          attempt: runAttempt(issue.runId),
          state: runStateLabel(t, issue.state),
        });
      case "unknown_run":
        return t(issue.hasEvidence ? "missions.result.issue.unknownRunReview" : "missions.result.issue.unknownRunNoEvidence", {
          task: taskTitle(issue.taskId),
          attempt: runAttempt(issue.runId),
          state: runStateLabel(t, issue.state),
        });
      case "open_blocking_decision":
        return t("missions.result.issue.openDecision");
    }
  };

  /** 상세 영역의 data-anchor 대상으로 스크롤하고 포커스한다. */
  const reveal = (anchor: string) => {
    const root = detailsRef.current;
    if (!root) return;
    const target = [...root.querySelectorAll<HTMLElement>("[data-anchor]")].find((element) => element.dataset.anchor === anchor);
    if (!target) return;
    target.scrollIntoView?.({ block: "nearest" });
    const focusable = target.querySelector<HTMLElement>("input:not(:disabled), textarea, button") ?? target;
    focusable.focus?.();
  };

  const openTaskLink = (taskId: string | null) =>
    taskId && props.onOpenTask ? { label: t("missions.result.openTask"), run: () => props.onOpenTask?.(taskId) } : null;

  const issueLink = (issue: AcceptanceIssue): { label: string; run: () => void } | null => {
    switch (issue.code) {
      case "required_task_not_succeeded":
      case "live_run":
        return openTaskLink(issue.taskId);
      case "verification_missing":
        return openTaskLink(issue.taskId) ?? { label: t("missions.result.showRequirements"), run: () => reveal("requirements") };
      case "verification_not_passed_on_candidate":
      case "integrity_policy_unmet":
        return { label: t("missions.result.showVerification"), run: () => reveal(`verification:${issue.verificationId}`) };
      case "observed_not_acknowledged":
        return { label: t("missions.result.showConfirmations"), run: () => reveal(`ack-verification:${issue.verificationId}`) };
      case "review_incomplete":
        return openTaskLink(issue.taskId);
      case "open_finding":
        return { label: t("missions.result.showFinding"), run: () => reveal(`finding:${issue.findingId}`) };
      case "human_check_missing":
        return { label: t("missions.result.showConfirmations"), run: () => reveal(`ack-requirement:${issue.requirementId}`) };
      case "unknown_run":
        return issue.hasEvidence
          ? { label: t("missions.result.showConfirmations"), run: () => reveal(`ack-run:${issue.runId}`) }
          : openTaskLink(issue.taskId);
      case "open_blocking_decision":
        return props.onOpenDecision
          ? { label: t("missions.result.openDecision"), run: () => props.onOpenDecision?.(issue.decisionId) }
          : null;
      default:
        return null;
    }
  };

  const onErrorAction = (action: MissionErrorAction) => {
    if (action === "resync") props.onRefresh();
  };

  // ------------------------------------------------------------ 확정

  const accept = async () => {
    if (busy) return;
    const client = getMissionClient();
    if (!client) {
      setAcceptError(missionError(t, new Error(t("missions.sync.noClient"))));
      return;
    }
    const bound = boundCandidateIdRef.current;
    if (bound === null || candidateChanged || acceptBlocked) return;
    const expected = payload;
    const ackState = acks;
    const reviewedNow = reviewed;
    setBusy(true);
    setAcceptError(null);
    try {
      await mutateWithResync(mission.id, (latest) => {
        // 같은 후보이고, 최신 snapshot에서도 체크한 대상이 그대로 유효할 때만 보낸다.
        if (latest.candidate_id === null || latest.candidate_id !== bound) return null;
        const snapshot = snapshotFor(useMissionStore.getState(), latest, reviewedNow);
        const latestAcks = acknowledgementsFor(ackState, snapshot.runs);
        const current = acceptanceChecklist(snapshot, latestAcks);
        if (!current.ready) return null;
        const next = acceptancePayload(current, latestAcks);
        if (
          !sameIds(next.verificationIds, expected.verificationIds) ||
          !sameIds(next.humanRequirementIds, expected.humanRequirementIds) ||
          !sameIds(next.reconciledRunIds, expected.reconciledRunIds)
        ) {
          return null;
        }
        return client.missionAccept({
          request_id: newRequestId(),
          mission_id: latest.id,
          expected_revision: latest.revision,
          candidate_id: latest.candidate_id,
          acknowledged_verification_ids: next.verificationIds,
          human_requirement_ids: next.humanRequirementIds,
          ...(next.reconciledRunIds.length > 0 ? { acknowledged_reconciled_run_ids: next.reconciledRunIds } : {}),
        });
      });
      setConfirmOpen(false);
      props.onRefresh();
    } catch (cause) {
      setAcceptError(missionError(t, cause));
    } finally {
      setBusy(false);
    }
  };

  // ------------------------------------------------------------ 버리기

  const [discardPhase, setDiscardPhase] = useState<DiscardPhase>("idle");
  const [discardError, setDiscardError] = useState<MissionUiError | null>(null);
  const discardBusy = discardPhase === "cancelling" || discardPhase === "archiving";

  const archive = async () => {
    const client = getMissionClient();
    if (!client) {
      setDiscardError(missionError(t, new Error(t("missions.sync.noClient"))));
      setDiscardPhase("failed");
      return;
    }
    setDiscardPhase("archiving");
    try {
      await mutateWithResync(mission.id, (latest) =>
        TERMINAL_STATES.has(latest.state) && latest.archived_at === null
          ? client.missionControl({
              request_id: newRequestId(),
              mission_id: latest.id,
              expected_revision: latest.revision,
              action: "archive",
            })
          : null,
      );
      setDiscardPhase("done");
      props.onRefresh();
      props.onArchived?.();
    } catch (cause) {
      if (useMissionStore.getState().missions[mission.id]?.archived_at) {
        setDiscardPhase("done");
        props.onArchived?.();
        return;
      }
      setDiscardError(missionError(t, cause));
      setDiscardPhase("failed");
    }
  };

  const discard = async () => {
    const client = getMissionClient();
    setDiscardError(null);
    if (!client) {
      setDiscardError(missionError(t, new Error(t("missions.sync.noClient"))));
      setDiscardPhase("failed");
      return;
    }
    const current = useMissionStore.getState().missions[mission.id] ?? mission;
    if (CANCELLABLE_STATES.has(current.state)) {
      setDiscardPhase("cancelling");
      try {
        await mutateWithResync(mission.id, (latest) =>
          CANCELLABLE_STATES.has(latest.state)
            ? client.missionControl({
                request_id: newRequestId(),
                mission_id: latest.id,
                expected_revision: latest.revision,
                action: "cancel",
              })
            : null,
        );
      } catch (cause) {
        const now = useMissionStore.getState().missions[mission.id];
        // 이미 누가 중단했으면 이어서 종료를 기다린다.
        if (!(cause instanceof StaleActionError && now && !CANCELLABLE_STATES.has(now.state))) {
          setDiscardError(missionError(t, cause));
          setDiscardPhase("failed");
          return;
        }
      }
      props.onRefresh();
    }
    setDiscardPhase("waiting");
  };

  // 중단 요청 후 종료(cancelled 등)가 확인되면 보관한다.
  useEffect(() => {
    if (discardPhase !== "waiting") return;
    if (!TERMINAL_STATES.has(mission.state)) return;
    if (mission.archived_at !== null) {
      setDiscardPhase("done");
      props.onArchived?.();
      return;
    }
    void archive();
    // archive/props는 매 렌더 새로 만들어진다 — 상태 전이만 감시한다.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [discardPhase, mission.state, mission.archived_at]);

  // ------------------------------------------------------------ 렌더

  if (candidateId === null) {
    return (
      <section className="mission-result result-review" data-testid="result-review">
        <div className="result-review-summary">
          <header className="mission-result-header">
            <h3>{t("missions.result.title")}</h3>
          </header>
          <p className="muted">{t("missions.result.noCandidate")}</p>
        </div>
      </section>
    );
  }

  const status = (() => {
    if (accepted) return { text: t("missions.result.statusAccepted"), tone: "ok" };
    if (mission.state !== "running") return { text: t(missionStateLabelKey(mission.state)), tone: "muted" };
    if (mission.phase !== "awaiting_acceptance") {
      return { text: t("missions.result.statusPhase", { phase: t(phaseLabelKey(mission.phase)) }), tone: "muted" };
    }
    return checklist.ready && !candidateChanged
      ? { text: t("missions.result.statusReady"), tone: "ok" }
      : { text: t("missions.result.statusBlocked"), tone: "review" };
  })();

  const firstIssue = checklist.issues[0] ?? null;
  const blockReason = accepted || candidateChanged
    ? null
    : candidate === null
      ? t("missions.result.candidateLoading")
      : firstIssue
        ? issueText(firstIssue)
        : null;

  const humanChecks = mission.requirements.filter((requirement) => requirement.human_check);
  const observed = verifications.filter((verification) => checklist.observedVerificationIds.includes(verification.id));
  const uncertain = checklist.uncertainRuns;
  const hasConfirmations = humanChecks.length > 0 || observed.length > 0 || uncertain.length > 0;
  const openFindings = findings.filter((finding) => finding.resolution === "open");
  const confirmDisabled = accepted || busy;
  const toggle = (field: "verificationIds" | "humanRequirementIds" | "runKeys", id: string, checked: boolean) =>
    setAcks((current) => ({
      ...current,
      [field]: checked ? [...current[field].filter((item) => item !== id), id] : current[field].filter((item) => item !== id),
    }));
  const showImport = () => {
    importRef.current?.scrollIntoView?.({ block: "start" });
  };
  const canDiscard = !accepted && mission.archived_at === null;

  const importSection = candidate ? (
    <section
      className={`mission-result-section result-review-import-section${accepted ? " emphasized" : ""}`}
      ref={importRef}
      data-anchor="import"
    >
      <h4>{t("missions.result.importTitle")}</h4>
      <ResultImport mission={mission} candidate={candidate} head={head} accepted={accepted} />
    </section>
  ) : null;

  return (
    <section className="mission-result result-review" data-testid="result-review">
      <div className="result-review-summary" data-testid="result-summary">
        <header className="mission-result-header">
          <h3>{t("missions.result.title")}</h3>
          <span className={`mission-result-status result-review-status tone-${status.tone}`} data-testid="result-status">
            {status.text}
          </span>
        </header>

        {candidateChanged ? (
          <p className="mission-area-error" role="alert" data-testid="candidate-changed">
            {t("missions.result.candidateChanged")}{" "}
            <button
              type="button"
              className="link"
              onClick={() => {
                boundCandidateIdRef.current = candidateId;
                setBindingTick((tick) => tick + 1);
                props.onRefresh();
              }}
            >
              {t("missions.result.refresh")}
            </button>
          </p>
        ) : null}

        {checklist.experimentalTaskIds.length > 0 ? (
          <p className="mission-experimental-warning" data-testid="result-experimental-warning">
            <span aria-hidden="true">⚠ </span>
            {t("missions.experimentalRun.resultWarning")}
          </p>
        ) : null}

        {accepted ? (
          <>
            <p className="result-review-accepted" role="status" data-testid="accepted-notice">
              {t("missions.result.acceptedNotice")}
            </p>
            <WorkspaceCleanup
              missionId={mission.id}
              entry={workspaceUsageEntry(workspace.usage, mission.id)}
              variant="suggest"
              onRefresh={workspace.refresh}
            />
          </>
        ) : (
          <div className="result-review-checklist" data-testid="acceptance-checklist">
            <h4>{t("missions.result.checklistTitle")}</h4>
            <ul>
              {checklist.items.map((item) => (
                <li
                  key={item.category}
                  className={item.ok ? "ok" : "unmet"}
                  data-testid={`checklist-${item.category}`}
                  data-ok={item.ok ? "true" : "false"}
                >
                  <span className="result-review-glyph" aria-hidden="true">{item.ok ? "✓" : "✗"}</span>
                  <span className="result-review-sr-only">
                    {item.ok ? t("missions.result.checkMet") : t("missions.result.checkUnmet")}
                  </span>{" "}
                  <span>{t(`missions.result.check.${item.category}`)}</span>
                  {item.issues.length > 0 ? (
                    <ul className="result-review-issues">
                      {item.issues.slice(0, ISSUES_PER_ITEM).map((issue, index) => {
                        const link = issueLink(issue);
                        return (
                          <li key={`${issue.code}-${index}`} data-testid="checklist-issue" data-code={issue.code}>
                            {issueText(issue)}
                            {link ? (
                              <>
                                {" "}
                                <button type="button" className="link" onClick={link.run} data-testid="checklist-link">
                                  {link.label}
                                </button>
                              </>
                            ) : null}
                          </li>
                        );
                      })}
                      {item.issues.length > ISSUES_PER_ITEM ? (
                        <li className="muted">{t("missions.result.issuesMore", { count: item.issues.length - ISSUES_PER_ITEM })}</li>
                      ) : null}
                    </ul>
                  ) : null}
                </li>
              ))}
              {checklist.experimentalTaskIds.length > 0 ? (
                // 정보 행: 확정을 막지 않는다(ok 판정·issues와 무관).
                <li className="info" data-testid="checklist-experimental" data-ok="true">
                  <span className="result-review-glyph" aria-hidden="true">ⓘ</span>{" "}
                  <span>{t("missions.experimentalRun.checklistInfo", { count: checklist.experimentalTaskIds.length })}</span>
                </li>
              ) : null}
            </ul>
          </div>
        )}

        {accepted ? null : (
          <div className="result-review-accept-row">
            <button
              type="button"
              className="primary mission-result-accept"
              disabled={busy || acceptBlocked}
              onClick={() => {
                setAcceptError(null);
                setConfirmOpen(true);
              }}
              data-testid="accept-button"
            >
              {t("missions.result.acceptAction")}
            </button>
            {blockReason ? (
              <span className="result-review-block-reason" data-testid="accept-blocked-reason">
                {blockReason}
              </span>
            ) : null}
          </div>
        )}

        {confirmOpen && !accepted ? (
          <div
            className="mission-inline-confirm result-review-confirm"
            role="alertdialog"
            aria-labelledby={`accept-title-${mission.id}`}
            data-testid="accept-confirm"
            ref={confirmRef}
          >
            <h4 id={`accept-title-${mission.id}`}>{t("missions.result.acceptConfirmTitle")}</h4>
            <p>
              {t("missions.result.acceptConfirmBody", {
                n: payload.verificationIds.length,
                m: payload.humanRequirementIds.length + payload.reconciledRunIds.length,
              })}
            </p>
            <div className="result-review-confirm-actions">
              <button
                type="button"
                className="primary"
                disabled={busy || acceptBlocked}
                onClick={() => void accept()}
                data-testid="accept-confirm-ok"
              >
                {busy ? t("missions.result.accepting") : t("missions.result.acceptAction")}
              </button>
              <button type="button" onClick={() => setConfirmOpen(false)} data-testid="accept-confirm-cancel">
                {t("missions.common.cancel")}
              </button>
            </div>
          </div>
        ) : null}
        {acceptError ? <MissionErrorNotice error={acceptError} onAction={onErrorAction} onRetry={() => void accept()} /> : null}

        {accepted ? null : (
          <div className="result-review-actions">
            {props.onRequestChanges && CANCELLABLE_STATES.has(mission.state) ? (
              <button type="button" onClick={props.onRequestChanges} data-testid="request-changes">
                {t("missions.result.requestChanges")}
              </button>
            ) : null}
            {canDiscard ? (
              <button
                type="button"
                disabled={discardBusy || discardPhase === "waiting" || discardPhase === "done"}
                onClick={() => {
                  setDiscardError(null);
                  setDiscardPhase("confirm");
                }}
                data-testid="discard-button"
              >
                {t("missions.result.discard")}
              </button>
            ) : null}
          </div>
        )}
        {discardPhase === "confirm" ? (
          <div
            className="mission-inline-confirm result-review-confirm"
            role="alertdialog"
            aria-labelledby={`discard-title-${mission.id}`}
            data-testid="discard-confirm"
          >
            <h4 id={`discard-title-${mission.id}`}>{t("missions.result.discardTitle")}</h4>
            <p>{t("missions.result.discardBody")}</p>
            <div className="result-review-confirm-actions">
              <button type="button" className="danger" onClick={() => void discard()} data-testid="discard-confirm-ok">
                {t("missions.result.discardAction")}
              </button>
              <button type="button" onClick={() => setDiscardPhase("idle")} data-testid="discard-confirm-cancel">
                {t("missions.common.cancel")}
              </button>
            </div>
          </div>
        ) : null}
        {discardPhase === "cancelling" || discardPhase === "waiting" || discardPhase === "archiving" ? (
          <p className="muted" role="status" data-testid="discard-status">
            {t("missions.result.discardStopping")}
          </p>
        ) : null}
        {discardPhase === "done" ? (
          <p role="status" data-testid="discard-status">
            {t("missions.result.discardDone")}
          </p>
        ) : null}
        {discardError ? <MissionErrorNotice error={discardError} onAction={onErrorAction} onRetry={() => void discard()} /> : null}

        {candidate ? (
          <ResultImportSummary mission={mission} candidate={candidate} head={head} accepted={accepted} onShowDetails={showImport} />
        ) : null}
      </div>

      <div className="result-review-details" ref={detailsRef} data-testid="result-details">
        {accepted ? importSection : null}

        {hasConfirmations && !accepted ? (
          <section className="mission-result-section result-review-confirmations" data-anchor="confirmations" data-testid="human-confirmations">
            <h4>{t("missions.result.confirmTitle")}</h4>
            <p className="muted">{t("missions.result.confirmHelp")}</p>
            {humanChecks.length > 0 ? (
              <ul className="result-review-checks">
                {humanChecks.map((requirement) => (
                  <li key={requirement.id} data-anchor={`ack-requirement:${requirement.id}`}>
                    <label>
                      <input
                        type="checkbox"
                        checked={acks.humanRequirementIds.includes(requirement.id)}
                        disabled={confirmDisabled}
                        onChange={(event) => toggle("humanRequirementIds", requirement.id, event.target.checked)}
                        data-testid="human-check"
                      />{" "}
                      {t("missions.result.confirmHuman", { requirement: requirement.text })}
                    </label>
                  </li>
                ))}
              </ul>
            ) : null}
            {observed.length > 0 ? (
              <ul className="result-review-checks" data-testid="observed-acknowledgements">
                {observed.map((verification) => (
                  <li key={verification.id} data-anchor={`ack-verification:${verification.id}`}>
                    <label>
                      <input
                        type="checkbox"
                        checked={acks.verificationIds.includes(verification.id)}
                        disabled={confirmDisabled}
                        onChange={(event) => toggle("verificationIds", verification.id, event.target.checked)}
                        data-testid="observed-check"
                      />{" "}
                      {t("missions.result.confirmObserved", {
                        command: tasksById.has(verification.task_id)
                          ? verifyCommandTitle(taskTitle(verification.task_id))
                          : t("missions.result.verificationUnnamed"),
                      })}
                    </label>
                  </li>
                ))}
              </ul>
            ) : null}
            {uncertain.length > 0 ? (
              <div className="mission-reconciled-accept" data-testid="reconciled-accept-review">
                <p>{t("missions.result.uncertainReview")}</p>
                <ul className="result-review-checks">
                  {uncertain.map((item) => {
                    const run = runs.find((candidateRun) => candidateRun.id === item.runId);
                    const key = run ? runKey(run) : item.runId;
                    const params = { task: taskTitle(item.taskId), attempt: item.attempt, state: runStateLabel(t, item.state) };
                    return (
                      <li key={item.runId} data-anchor={`ack-run:${item.runId}`} data-testid="reconciled-run">
                        <label>
                          <input
                            type="checkbox"
                            disabled={!item.hasEvidence || confirmDisabled}
                            checked={item.hasEvidence && acks.runKeys.includes(key)}
                            onChange={(event) => toggle("runKeys", key, event.target.checked)}
                            data-testid="reconciled-check"
                          />{" "}
                          {t("missions.result.confirmRun", params)}
                        </label>
                        {run ? <RunAttestedLabel run={run} /> : null}
                        {item.hasEvidence ? null : (
                          <p className="muted result-review-disabled-reason" data-testid="reconciled-unended">
                            {t("missions.result.confirmRunUnended", params)}
                          </p>
                        )}
                        {props.onOpenTask ? (
                          <button type="button" className="link" onClick={() => props.onOpenTask?.(item.taskId)} data-testid="reconciled-open-task">
                            {t("missions.result.openDetail")}
                          </button>
                        ) : null}
                      </li>
                    );
                  })}
                </ul>
              </div>
            ) : null}
          </section>
        ) : null}

        {/* 요구사항 충족표 — 명령별로 현재 후보에서 통과 1건 이상이면 ✓ */}
        <section className="mission-result-section" data-anchor="requirements" tabIndex={-1}>
          <h4>{t("missions.result.requirements")}</h4>
          <table className="mission-req-table" data-testid="req-table">
            <tbody>
              {mission.requirements.map((requirement) => {
                const evidence = checklist.requirements.find((item) => item.requirementId === requirement.id);
                const rowStatus = evidence?.status ?? "missing";
                return (
                  <tr key={requirement.id} data-testid="req-row" data-status={rowStatus}>
                    <td className="result-review-req-glyph" data-testid="req-status">
                      {rowStatus === "no_command" ? null : (
                        <span aria-label={t(`missions.result.commandStatus.${rowStatus}`)}>{glyphFor(rowStatus)}</span>
                      )}
                    </td>
                    <td className="mission-req-text">{requirement.text}</td>
                    <td>
                      {requirement.verification_ids.length === 0 ? (
                        <span className="muted">{t("missions.result.requirementNoVerification")}</span>
                      ) : (
                        <ul className="result-review-req-commands">
                          {requirement.verification_ids.map((command) => {
                            const commandStatus = checklist.commands[command]?.status ?? "missing";
                            return (
                              <li key={command} className={`req-command-${commandStatus}`}>
                                <span aria-hidden="true">{glyphFor(commandStatus)}</span> {commandName(command)} ·{" "}
                                {t(`missions.result.commandStatus.${commandStatus}`)}
                              </li>
                            );
                          })}
                        </ul>
                      )}
                      {requirement.human_check ? (
                        <span className="mission-req-human">{t("missions.result.requirementHuman")}</span>
                      ) : null}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </section>

        <section className="mission-result-section">
          <h4>{t("missions.result.changes")}</h4>
          <CandidateChanges candidate={candidate} />
        </section>

        <section className="mission-result-section">
          <h4>{t("missions.result.verification")}</h4>
          {verifications.length === 0 ? (
            <p className="muted">{t("missions.detail.verificationEmpty")}</p>
          ) : (
            <ul className="mission-verification-list result-review-verifications">
              {verifications.map((verification) => (
                <VerificationItem key={verification.id} verification={verification} task={tasksById.get(verification.task_id) ?? null} />
              ))}
            </ul>
          )}
        </section>

        <section className="mission-result-section">
          <h4>{t("missions.result.findingsTitle")}</h4>
          {findings.length === 0 ? (
            <p className="muted">{t("missions.result.noFindingsText")}</p>
          ) : (
            <ul className="mission-findings-list">
              {findings.map((finding) => (
                <FindingReview key={finding.id} mission={mission} finding={finding} disabled={candidateChanged} onRefresh={props.onRefresh} />
              ))}
            </ul>
          )}
        </section>

        <section className="mission-result-section">
          <h4>{t("missions.result.limitations")}</h4>
          {openFindings.length === 0 ? (
            <p className="muted">{t("missions.result.noLimitations")}</p>
          ) : (
            <ul className="mission-limitations" data-testid="result-limitations">
              {openFindings.map((finding) => (
                <li key={finding.id}>
                  {severityLabel(t, finding.severity)}
                  {finding.path ? ` · ${finding.path}` : ""}
                </li>
              ))}
            </ul>
          )}
        </section>

        {accepted ? null : importSection}

        <section className="mission-result-section">
          <h4>{t("missions.result.history")}</h4>
          <UsageSummary runs={runs} />
        </section>
      </div>
    </section>
  );
}

function glyphFor(status: CommandStatus | "no_command"): string {
  switch (status) {
    case "passed":
      return "✓";
    case "failed":
      return "✗";
    case "missing":
      return "–";
    case "no_command":
      return "";
  }
}

/** 후보 변경 목록 — 데몬 manifest(entries[])를 읽는다. */
function CandidateChanges(props: { candidate: Candidate | null }): JSX.Element {
  const { t } = useI18n();
  const manifest = useArtifactText(props.candidate ? props.candidate.manifest_ref : null);
  if (!props.candidate) return <p className="muted">{t("missions.detail.noChanges")}</p>;
  const parsed = parseCandidateManifest(manifest.text);
  const loading = manifest.text === null && !manifest.error;
  const visible = parsed.entries.slice(0, MANIFEST_VISIBLE_LIMIT);
  const hidden = parsed.entries.length - visible.length;
  const commit = props.candidate.commit_oid.slice(0, 10);
  return (
    <div>
      <p data-testid="result-diff-summary">
        {loading
          ? t("missions.result.changesLoading", { commit })
          : manifest.error
            ? t("missions.result.changesUnreadable", { commit })
            : t("missions.result.changesSummary", { count: parsed.entries.length, commit })}
      </p>
      {visible.length > 0 ? (
        <ul className="mission-changes-list result-review-files">
          {visible.map((entry) => {
            const labelKey = manifestChangeLabelKey(entry.change);
            return (
              <li key={entry.path} className={`change-${entry.change}`} data-testid="result-file">
                <span className="result-review-change">{labelKey ? t(labelKey) : entry.change}</span>{" "}
                <span className="result-review-path">{entry.path}</span>
                {entry.bytes !== null ? <span className="muted"> · {formatManifestBytes(entry.bytes)}</span> : null}
              </li>
            );
          })}
        </ul>
      ) : null}
      {hidden > 0 ? (
        <p className="muted" data-testid="result-files-more">
          {t("missions.result.changesMore", { count: hidden })}
        </p>
      ) : null}
    </div>
  );
}

function VerificationItem(props: { verification: Verification; task: Task | null }): JSX.Element {
  const { t } = useI18n();
  const { verification, task } = props;
  const snapshot = useArtifactText(task ? null : verification.command_snapshot_ref);
  const name = task
    ? verifyCommandTitle(task.title)
    : commandTitleFromSnapshot(snapshot.text) ?? t("missions.result.verificationUnnamed");
  return (
    <li
      className={`verification-${verification.status}`}
      data-anchor={`verification:${verification.id}`}
      data-testid="result-verification"
      tabIndex={-1}
    >
      <span aria-hidden="true">{verification.status === "passed" ? "✓" : "✗"}</span>{" "}
      <strong className="result-review-command">{name}</strong> · {t(verificationStatusLabelKey(verification.status))}
      {verification.exit_code !== null ? ` · ${t("missions.result.verificationExit", { code: verification.exit_code })}` : ""}
      {verification.status === "passed" ? (
        <span className="muted"> · {t(inputIntegrityNoteKey(verification.input_integrity))}</span>
      ) : null}
      <VerificationLog logRef={verification.log_ref} />
    </li>
  );
}

function VerificationLog(props: { logRef: ArtifactRef }): JSX.Element {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const bytes = Number(props.logRef.bytes);
  return (
    <details
      className="result-review-log"
      onToggle={(event) => setOpen((event.currentTarget as HTMLDetailsElement).open)}
      data-testid="verification-log"
    >
      <summary>{t("missions.result.logOpen", { size: formatManifestBytes(Number.isFinite(bytes) ? bytes : 0) })}</summary>
      {open ? <VerificationLogBody logRef={props.logRef} /> : null}
    </details>
  );
}

function VerificationLogBody(props: { logRef: ArtifactRef }): JSX.Element {
  const { t } = useI18n();
  const { excerpt, error } = useArtifactExcerpt(props.logRef);
  if (error) return <p className="mission-area-error">{t("missions.result.logError")}</p>;
  if (!excerpt) return <p className="muted">{t("missions.result.logLoading")}</p>;
  return (
    <>
      {excerpt.tail !== null ? (
        <p className="muted" data-testid="verification-log-truncated">
          {t("missions.result.logTruncated", {
            size: formatManifestBytes(excerpt.totalBytes),
            head: formatManifestBytes(excerpt.shownHeadBytes),
            tail: formatManifestBytes(excerpt.shownTailBytes),
          })}
        </p>
      ) : null}
      <pre className="mission-finding-body result-review-log-body" data-testid="verification-log-body">
        {excerpt.head}
        {excerpt.tail !== null ? `\n${t("missions.result.logOmitted")}\n${excerpt.tail}` : ""}
      </pre>
    </>
  );
}


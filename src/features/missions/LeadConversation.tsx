/**
 * LeadConversation(05-ui §4): 왼쪽 대화 pane.
 *
 * - user/agent/system 메시지를 구분하고 본문은 body_ref를 artifact.read로
 *   다시 읽는다(8 MiB LRU — bodyCache). store에 원문을 두지 않는다.
 * - 실패·취소로 끝난 작업은 맨 위에 요약 카드(원인 문장 + 설정 열기 / 같은
 *   목표로 새 작업)를 둔다.
 * - 채택된 계획 카드(작업 목록 요약)는 접을 수 있다. 개수는 사용자 의미의 할 일
 *   (대체됨·검증·통합 제외)만 센다.
 * - 결정 답변 시스템 메시지는 원문 JSON 대신 `결정: {선택지}` 카드로 보인다(01 §8
 *   `decision_answer` 문서와 기존 `{decision_id, option_id}` 부분집합 둘 다 읽는다).
 * - 사용자가 위를 읽는 동안 tail-follow를 끊고 `새 활동 N개`만 보여 주며,
 *   위쪽 본문이 늦게 채워져도 읽던 위치를 지킨다(scroll anchor).
 * - composer 수신자는 항상 `작업 전체에 요청`(task 선택으로 바뀌지 않는다).
 *   조합 중 Enter 전송 금지: React composition flag와 nativeEvent.isComposing
 *   둘 다 검사한다(05 §4). Shift+Enter 줄바꿈, Enter 전송은 사용자 설정.
 * - 전송 흐름: artifact upload → mission.message(target null). 오류 시
 *   draft/수신자 보존.
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import type { Decision } from "../../generated/Decision";
import type { Message } from "../../generated/Message";
import type { Mission } from "../../generated/Mission";
import type { Run } from "../../generated/Run";
import { useI18n } from "../../i18n";
import { useMissionStore } from "./store";
import {
  isUserFacingTask,
  selectDecisionByAnswerMessage,
  selectMissionMessages,
  selectPrimaryRun,
  selectTaskList,
} from "./selectors";
import { MessageComposer } from "./MessageComposer";
import { MessageRecovery } from "./MessageRecovery";
import { decisionOptionLabel, deliveryLabelKey } from "./labels";
import { missionError } from "./errors";
import { formatClock, formatElapsedTime, useArtifactText } from "./viewUtils";

export interface LeadConversationProps {
  mission: Mission;
  /** 메시지의 실행 링크 클릭 — 오른쪽 상세로 연결한다. */
  onOpenRun: (run: Run) => void;
  /** 완료 후 `이 결과로 후속 작업 만들기` — MissionCreate 대화상자를 연다. */
  onStartFollowUp: () => void;
  /** 실패·취소 후 `같은 목표로 새 작업`. */
  onStartSameGoal?: () => void;
  /** 실패 요약 카드의 `설정 열기`. */
  onOpenSettings?: () => void;
  /** 후속 작업 안내에서 결과 화면(가져오기 안내)으로 이동. */
  onOpenResult?: () => void;
  /** 연결 끊김 — 전송을 막는다(입력은 유지). */
  disconnected?: boolean;
}

/** 이 요소들 안의 변화는 사용자가 직접 펼친 것 — 끝으로 끌어내리지 않는다. */
const INTERACTIVE_REGIONS = ".mission-plan-card, .mission-goal-card, .mission-outcome-card, .mission-message-recovery";

export function LeadConversation(props: LeadConversationProps): JSX.Element {
  const { t } = useI18n();
  const mission = props.mission;
  // 다른 미션 필드만 갱신되면 목록 참조를 유지한다. tail-follow effect가
  // 새 배열을 받아 자기 상태 갱신으로 다시 실행되는 루프를 막는다.
  const messages = useMissionStore(useShallow((s) => selectMissionMessages(s, mission.id)));
  const tasks = useMissionStore(useShallow((s) => selectTaskList(s, mission.id)));
  const planTasks = useMemo(
    () => tasks.filter(isUserFacingTask).map((task) => ({ id: task.id, title: task.title })),
    [tasks],
  );
  const messageLinks = useMemo(() => ({
    byId: new Map(messages.map(message => [message.id, message])),
    replacement: new Map(messages.filter(message => message.supersedes_message_id).map(message => [message.supersedes_message_id!, message])),
  }), [messages]);

  const scrollRef = useRef<HTMLDivElement | null>(null);
  // follow 판정은 ref로 동기에 내린다 — 스크롤 이벤트와 메시지 effect가
  // 서로 경합할 때(사용자가 올린 직후 끝으로 끌어당기는) ref가 아니면
  // state 커밋 전의 effect가 사용자를 다시 끌어내린다.
  const followRef = useRef(true);
  const [follow, setFollow] = useState(true);
  const seenCountRef = useRef(messages.length);
  const [pendingCount, setPendingCount] = useState(0);
  /** 위를 읽는 동안의 기준 요소: 화면 맨 위에 걸친 카드와 그 위치. */
  const anchorRef = useRef<{ element: HTMLElement; offset: number } | null>(null);

  const rememberAnchor = () => {
    const el = scrollRef.current;
    if (!el) return;
    const top = el.scrollTop;
    for (const child of Array.from(el.children) as HTMLElement[]) {
      if (child.offsetTop + child.offsetHeight > top) {
        anchorRef.current = { element: child, offset: child.offsetTop - top };
        return;
      }
    }
    anchorRef.current = null;
  };

  // 새 메시지: 따라가는 중이면 끝으로 붙이고, 아니면 새 활동 개수만 올린다.
  useEffect(() => {
    if (followRef.current) {
      seenCountRef.current = messages.length;
      setPendingCount(0);
      const el = scrollRef.current;
      if (el) el.scrollTop = el.scrollHeight;
      return;
    }
    setPendingCount(Math.max(0, messages.length - seenCountRef.current));
  }, [messages]);

  // 본문이 늦게 채워지거나 교체돼 높이가 바뀌어도 화면이 튀지 않게 한다:
  // 따라가는 중이면 끝에 붙이고, 위를 읽는 중이면 기준 요소의 위치를 되돌린다.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el || typeof MutationObserver === "undefined") return;
    const observer = new MutationObserver((records) => {
      if (followRef.current) {
        const userExpanded = records.every((record) => {
          const target = record.target instanceof Element ? record.target : record.target.parentElement;
          return target?.closest(INTERACTIVE_REGIONS) != null;
        });
        if (!userExpanded) el.scrollTop = el.scrollHeight;
        return;
      }
      const anchor = anchorRef.current;
      if (!anchor || !anchor.element.isConnected || anchor.element.parentElement !== el) return;
      const target = anchor.element.offsetTop - anchor.offset;
      if (Math.abs(el.scrollTop - target) >= 1) el.scrollTop = target;
    });
    observer.observe(el, { childList: true, subtree: true, characterData: true });
    return () => observer.disconnect();
  }, []);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
    if (nearBottom) {
      anchorRef.current = null;
      if (!followRef.current) {
        followRef.current = true;
        seenCountRef.current = messages.length;
        setPendingCount(0);
        setFollow(true);
      }
      return;
    }
    if (followRef.current) {
      followRef.current = false;
      setFollow(false);
    }
    rememberAnchor();
  };

  const jumpToLatest = () => {
    followRef.current = true;
    anchorRef.current = null;
    seenCountRef.current = messages.length;
    setPendingCount(0);
    setFollow(true);
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  };

  const ended = mission.state === "failed" || mission.state === "cancelled";

  return (
    <div className="mission-lead">
      {/* 새 활동 버튼은 Lead 영역 기준으로 뜬다(position: relative 뷰포트). */}
      <div className="mission-lead-viewport">
        <div
          className="mission-lead-scroll"
          ref={scrollRef}
          onScroll={onScroll}
          data-testid="lead-scroll"
        >
          {ended ? (
            <OutcomeCard
              mission={mission}
              onOpenSettings={props.onOpenSettings}
              onStartSameGoal={props.onStartSameGoal}
            />
          ) : null}
          <GoalCard mission={mission} />
          <PlanCard tasks={planTasks} />
          {messages.length === 0 ? (
            mission.phase === "planning" && mission.state === "running" ? (
              <PlanningEmpty mission={mission} />
            ) : (
              <p className="mission-lead-empty muted">{t("missions.lead.empty")}</p>
            )
          ) : (
            messages.map((message) => (
              <LeadMessage
                key={message.id}
                message={message}
                mission={mission}
                original={messageLinks.byId.get(message.supersedes_message_id ?? "")}
                replacement={messageLinks.replacement.get(message.id)}
                onOpenRun={props.onOpenRun}
              />
            ))
          )}
        </div>
        {pendingCount > 0 && !follow ? (
          <button
            type="button"
            className="mission-new-activity"
            onClick={jumpToLatest}
            data-testid="new-activity"
          >
            {t("missions.lead.newActivity", { count: pendingCount })}
          </button>
        ) : null}
      </div>
      <MessageComposer
        mission={mission}
        recipient={t("missions.lead.composerLabel")}
        placeholder={t("missions.lead.composerPlaceholder")}
        onStartFollowUp={props.onStartFollowUp}
        onStartSameGoal={props.onStartSameGoal}
        onOpenResult={props.onOpenResult}
        disconnected={props.disconnected}
      />
    </div>
  );
}

/** 실패·취소 요약 카드 — failure_code를 사람 문장으로(05 §8 오류 표시). */
function OutcomeCard(props: {
  mission: Mission;
  onOpenSettings?: () => void;
  onStartSameGoal?: () => void;
}): JSX.Element {
  const { t } = useI18n();
  const mission = props.mission;
  const failed = mission.state === "failed";
  // 요약 카드는 끝난 사실의 기록이다 — 오류 알림(role=alert)으로 낭독하지 않는다.
  const explanation = failed && mission.failure_code !== null
    ? missionError(t, { code: mission.failure_code, message: "" })
    : null;
  return (
    <section className="mission-card mission-outcome-card" data-testid="mission-outcome-card" data-state={mission.state}>
      <h3 className="mission-card-title">
        {t(failed ? "missions.outcome.failedTitle" : "missions.outcome.cancelledTitle")}
      </h3>
      <p className="mission-outcome-message" data-testid="mission-outcome-message">
        {failed ? explanation?.message ?? t("missions.outcome.failedNoCode") : t("missions.outcome.cancelledBody")}
      </p>
      {explanation?.code ? <p className="muted mission-outcome-code"><code>{explanation.code}</code></p> : null}
      <div className="mission-outcome-actions">
        {failed && props.onOpenSettings ? (
          <button type="button" onClick={props.onOpenSettings} data-testid="outcome-open-settings">
            {t("missions.outcome.openSettings")}
          </button>
        ) : null}
        {props.onStartSameGoal ? (
          <button type="button" className="primary" onClick={props.onStartSameGoal} data-testid="outcome-same-goal">
            {t("missions.outcome.sameGoal")}
          </button>
        ) : null}
      </div>
    </section>
  );
}

/** 계획 단계의 빈 대화 — Lead가 일하는 중임과 경과 시간을 보인다. */
function PlanningEmpty(props: { mission: Mission }): JSX.Element {
  const { t } = useI18n();
  const missionId = props.mission.id;
  const leadStartedAt = useMissionStore((s) => {
    const lead = selectTaskList(s, missionId).find((task) => task.kind === "plan" || task.role === "lead");
    return lead ? selectPrimaryRun(s, lead)?.started_at ?? null : null;
  });
  const startedAt = leadStartedAt ?? props.mission.created_at;
  const now = useNow(1000);
  const started = new Date(startedAt).getTime();
  const elapsedMs = Number.isNaN(started) ? 0 : Math.max(0, Math.floor(now - started));
  return (
    <p className="mission-lead-empty mission-lead-planning muted" data-testid="lead-planning">
      <span>{t("missions.lead.planning")}</span>
      <span className="mission-elapsed"> · {t("missions.lead.planningElapsed", { elapsed: formatElapsedTime(String(elapsedMs)) })}</span>
    </p>
  );
}

function useNow(intervalMs: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(timer);
  }, [intervalMs]);
  return now;
}

/** 사용자 목표 카드 — mission.goal_ref 본문(05 §1 "사용자 목표"). */
function GoalCard(props: { mission: Mission }): JSX.Element {
  const { t } = useI18n();
  const body = useArtifactText(props.mission.goal_ref);
  return (
    <section className="mission-card mission-goal-card">
      <h3 className="mission-card-title">{t("missions.lead.goal")}</h3>
      <MessageBody text={body.text} error={body.error} />
    </section>
  );
}

/** 채택된 계획 카드 — 접기/펼치기(05 §4). 매 token마다 갱신하지 않는다. */
function PlanCard(props: { tasks: Array<{ id: string; title: string }> }): JSX.Element {
  const [open, setOpen] = useState(false);
  const { t } = useI18n();
  return (
    <section className="mission-card mission-plan-card">
      <button
        type="button"
        className="mission-plan-toggle"
        aria-expanded={open}
        aria-label={open ? t("missions.lead.planCollapse") : t("missions.lead.planExpand")}
        onClick={() => setOpen(!open)}
        data-testid="plan-toggle"
      >
        <span className="mission-plan-glyph" aria-hidden="true">{open ? "▾" : "▸"}</span>
        <span className="mission-card-title">{t("missions.lead.planTitle")}</span>
        <span className="muted" data-testid="plan-count">{t("missions.lead.planTasks", { count: props.tasks.length })}</span>
      </button>
      {open ? (
        props.tasks.length === 0 ? (
          <p className="muted">{t("missions.lead.planEmpty")}</p>
        ) : (
          <ol className="mission-plan-list">
            {props.tasks.map((task, index) => (
              <li key={task.id}>
                <span className="mission-plan-ordinal">{index + 1}.</span> {task.title}
              </li>
            ))}
          </ol>
        )
      ) : null}
    </section>
  );
}

export interface ParsedDecisionAnswer {
  decisionId: string;
  optionId: string | null;
  /** 데몬이 기록한 선택지 label(영어) — 모르는 선택지 id의 폴백으로만 쓴다. */
  optionLabel: string | null;
  /** 결정 종류(recovery, conflict …). 기존 부분집합 형식에는 없다. */
  decisionKind: string | null;
  /** 선택지와 함께 보낸 자유 입력 본문(UTF-8 64 KiB 이하일 때만 실린다). */
  answerText: string | null;
}

function nonEmptyString(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

/**
 * 결정 답변 시스템 메시지 본문을 읽는다(01 §8).
 *
 * - `{"kind":"decision_answer","version":1,"decision_id","decision_kind","option_id",
 *   "option_label","answer_ref","answer_text"}` 문서.
 * - 기존 `{"decision_id","option_id"}` 부분집합(kind 없음)도 그대로 읽는다.
 * - 다른 kind의 JSON(예: 후보 제외 근거 `integration_exclusion`)과 JSON이 아닌 텍스트는
 *   답변 문서가 아니다(null) — 결정 연결이 있으면 결정 기준으로, 없으면 일반 메시지로 보인다.
 */
export function parseDecisionAnswer(text: string | null): ParsedDecisionAnswer | null {
  if (text === null) return null;
  const trimmed = text.trim();
  if (!trimmed.startsWith("{")) return null;
  try {
    const value: unknown = JSON.parse(trimmed);
    if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
    const record = value as {
      kind?: unknown;
      decision_id?: unknown;
      decision_kind?: unknown;
      option_id?: unknown;
      option_label?: unknown;
      answer_text?: unknown;
    };
    if (record.kind !== undefined && record.kind !== "decision_answer") return null;
    const decisionId = nonEmptyString(record.decision_id);
    if (decisionId === null) return null;
    return {
      decisionId,
      optionId: nonEmptyString(record.option_id),
      optionLabel: nonEmptyString(record.option_label),
      decisionKind: nonEmptyString(record.decision_kind),
      answerText: typeof record.answer_text === "string" && record.answer_text.trim().length > 0 ? record.answer_text : null,
    };
  } catch {
    return null;
  }
}

/** 자유 입력 요약 — 공백을 접고 한 줄로 자른다. */
function summarize(text: string, limit = 160): string {
  const flat = text.replace(/\s+/g, " ").trim();
  const chars = Array.from(flat);
  return chars.length > limit ? `${chars.slice(0, limit).join("")}…` : flat;
}

function LeadMessage(props: { message: Message; mission: Mission; original?: Message; replacement?: Message; onOpenRun: (run: Run) => void }): JSX.Element {
  const { t } = useI18n();
  const message = props.message;
  const body = useArtifactText(message.body_ref);
  const roleKey =
    message.role === "user" ? "missions.lead.role.user" : message.role === "agent" ? "missions.lead.role.agent" : "missions.lead.role.system";
  const run = useMissionStore((s) => (message.run_id ? s.runs[message.run_id] ?? null : null));
  const linkedDecision = useMissionStore((s) =>
    message.role === "system" ? selectDecisionByAnswerMessage(s, message.mission_id, message.id) : null,
  );
  const parsedAnswer = message.role === "system" ? parseDecisionAnswer(body.text) : null;
  const parsedDecisionId = parsedAnswer?.decisionId ?? null;
  const bodyDecision = useMissionStore((s) => (parsedDecisionId !== null ? s.decisions[parsedDecisionId] ?? null : null));
  const decision = linkedDecision ?? bodyDecision;
  const isDecisionAnswer = decision !== null || parsedAnswer !== null;
  return (
    <article className={`mission-msg mission-msg-${message.role}`} id={`mission-message-${message.id}`} tabIndex={-1}>
      <header className="mission-msg-header">
        <span className="mission-msg-role">{t(roleKey)}</span>
        <span className="mission-msg-time muted">{formatClock(message.created_at)}</span>
        {message.role === "user" ? (
          <span className={`mission-msg-delivery delivery-${message.delivery}`}>
            {t(deliveryLabelKey(message.delivery))}
          </span>
        ) : null}
        {message.run_id ? (
          <button
            type="button"
            className="mission-msg-run-link"
            title={t("missions.lead.runLink")}
            disabled={run === null}
            onClick={() => run && props.onOpenRun(run)}
            data-testid="msg-run-link"
          >
            ▸ {t("missions.detail.tab.activity")}
          </button>
        ) : null}
      </header>
      {isDecisionAnswer ? (
        <DecisionAnswer
          decision={decision}
          optionId={parsedAnswer ? parsedAnswer.optionId : decision?.selected_option_id ?? null}
          optionLabel={parsedAnswer?.optionLabel ?? null}
          note={
            parsedAnswer !== null
              ? parsedAnswer.answerText
              : decision?.answer_ref && decision.answer_ref.id === message.body_ref.id
                ? body.text
                : null
          }
          loading={body.text === null && !body.error}
        />
      ) : (
        <MessageBody text={body.text} error={body.error} />
      )}
      <MessageRecovery mission={props.mission} message={message} text={body.text} bodyError={body.error} original={props.original} replacement={props.replacement} />
    </article>
  );
}

/** `결정: {선택지 라벨}` 카드(+자유 입력 요약). */
function DecisionAnswer(props: {
  decision: Decision | null;
  optionId: string | null;
  /** 답변 문서에 기록된 선택지 label(결정이 store에 없을 때의 폴백). */
  optionLabel: string | null;
  note: string | null;
  loading: boolean;
}): JSX.Element {
  const { t } = useI18n();
  // 라벨: 선택지 id 번역 → 결정의 선택지 label → 답변 문서의 option_label → id.
  const option = props.optionId === null
    ? null
    : props.decision?.options.find((candidate) => candidate.id === props.optionId)
      ?? { id: props.optionId, label: props.optionLabel ?? props.optionId };
  const label = option ? decisionOptionLabel(t, option) : t("missions.lead.decisionCustom");
  const note = props.note?.trim() ? summarize(props.note) : null;
  return (
    <div className="mission-decision-answer" data-testid="decision-answer-card">
      <p className="mission-decision-answer-title">{t("missions.lead.decisionAnswer", { option: label })}</p>
      {note ? <p className="muted mission-decision-answer-note">{t("missions.lead.decisionNote", { text: note })}</p> : null}
      {props.loading && note === null && props.decision?.answer_ref ? <p className="muted">…</p> : null}
    </div>
  );
}

function MessageBody(props: { text: string | null; error: boolean }): JSX.Element | null {
  const { t } = useI18n();
  if (props.error) return <p className="mission-body-error">{t("missions.lead.bodyError")}</p>;
  if (props.text === null) return <p className="muted">…</p>;
  if (props.text.length === 0) return null;
  return <p className="mission-msg-body">{props.text}</p>;
}

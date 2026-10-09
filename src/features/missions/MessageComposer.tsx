import { useRef } from "react";
import type { Mission } from "../../generated/Mission";
import type { Task } from "../../generated/Task";
import { useI18n } from "../../i18n";
import { usePreferences } from "../../store/preferences";
import { MISSION_TEXT_MAX_BYTES } from "./artifactUpload";
import { messageTargetAcceptsInput, restoreMessageDraft, setMessageDraft, submitMessage, useMessageDraft } from "./messageSubmission";

type Props = {
  mission: Mission;
  task?: Task;
  recipient: string;
  placeholder: string;
  /** 완료 후 `이 결과로 후속 작업 만들기`. */
  onStartFollowUp?: () => void;
  /** 실패·취소 후 `같은 목표로 새 작업`. */
  onStartSameGoal?: () => void;
  /** 후속 작업 안내의 결과 화면(가져오기 안내) 링크. */
  onOpenResult?: () => void;
  /** 연결 끊김 — 전송·확인 버튼을 막는다(입력한 내용은 유지). */
  disconnected?: boolean;
};

/** 잠긴 입력의 이유 문구 — 작업 상태별로 다음에 할 일을 알려 준다. */
export function lockedComposerTextKey(mission: Mission): string {
  if (mission.archived_at) return "missions.composer.lockedArchived";
  switch (mission.state) {
    case "completed":
      return "missions.composer.lockedCompleted";
    case "failed":
      return "missions.composer.lockedFailed";
    case "cancelled":
      return "missions.composer.lockedCancelled";
    case "stopping":
      return "missions.composer.lockedStopping";
    default:
      // 작업은 열려 있지만 이 수신자(할 일)가 끝났거나 받을 수 없다.
      return "missions.composer.closed";
  }
}

export function MessageComposer(props: Props): JSX.Element {
  const { mission, task } = props;
  const { t } = useI18n();
  const enterSend = usePreferences(s => s.missionEnterSend);
  const draft = useMessageDraft(mission.id, task?.id ?? null);
  const composingRef = useRef(false);
  const unresolved = !!draft.attempt?.params;
  const allowed = messageTargetAcceptsInput(mission, task ?? null);
  const offline = props.disconnected === true;
  const tooLarge = new TextEncoder().encode(draft.text.trim()).byteLength > MISSION_TEXT_MAX_BYTES;
  const disabled = offline || !draft.ready || draft.busy || (!unresolved && (!allowed || !draft.text.trim() || tooLarge));
  const send = () => submitMessage(mission, task ?? null);

  // 작업 전체 입력창이 끝난 작업에서 권하는 다음 행동.
  const ended = !task && ["completed", "failed", "cancelled"].includes(mission.state);
  const next = !ended
    ? null
    : mission.state === "completed"
      ? { label: "missions.lead.followUp", run: props.onStartFollowUp, testId: "composer-follow-up" }
      : { label: "missions.outcome.sameGoal", run: props.onStartSameGoal, testId: "composer-same-goal" };
  const nextReady = next !== null && next.run !== undefined && draft.ready && !unresolved && !draft.busy;
  // 확정된 작업의 후속 작업은 확정 결과 커밋 위에서 시작한다(계약 E). 확정되지 않았으면 현재 HEAD에서
  // 시작하므로 결과를 먼저 내 브랜치로 가져오라는 기존 안내를 유지한다.
  const followUpNote = mission.state === "completed" ? (
    <p className="muted mission-follow-up-note" data-testid="follow-up-note">
      {t(mission.accepted_at !== null ? "missions.composer.followUpAcceptedNote" : "missions.composer.followUpBaseNote")}
      {props.onOpenResult ? (
        <>
          {" "}
          <button type="button" className="link" onClick={props.onOpenResult} data-testid="follow-up-open-result">
            {t("missions.composer.openResult")}
          </button>
        </>
      ) : null}
    </p>
  ) : null;

  if (next && nextReady && !draft.error && !draft.text.trim()) {
    return <div className="mission-composer mission-composer-done" data-testid="composer-done">
      <p className="muted" data-testid="composer-locked">{t(lockedComposerTextKey(mission))}</p>
      <button type="button" className="primary" onClick={next.run} data-testid={next.testId}>{t(next.label)}</button>
      {followUpNote}
    </div>;
  }

  return <div className={`mission-composer${task ? " mission-task-composer" : ""}`} data-testid={task ? "task-composer" : undefined}>
    <div className="mission-composer-recipient" data-testid={task ? "task-recipient" : "lead-recipient"}>{props.recipient}</div>
    <div className="mission-composer-row">
      <textarea className="mission-composer-input" value={draft.text} placeholder={props.placeholder}
        aria-label={props.recipient} rows={2} maxLength={MISSION_TEXT_MAX_BYTES}
        disabled={!draft.ready || draft.busy || unresolved || !allowed}
        data-testid={task ? "task-composer-input" : "lead-composer"}
        onChange={event => setMessageDraft(mission.id, task?.id ?? null, event.target.value)}
        onCompositionStart={() => { composingRef.current = true; }}
        onCompositionEnd={() => { composingRef.current = false; }}
        onKeyDown={event => {
          if (event.key !== "Enter" || event.shiftKey || composingRef.current || event.nativeEvent.isComposing) return;
          if (enterSend || event.ctrlKey || event.metaKey) {
            event.preventDefault();
            if (!disabled) void send();
          }
        }} />
      <button type="button" className="primary mission-composer-send" disabled={disabled}
        data-testid={task ? "task-composer-send" : "lead-send"} onClick={() => void send()}>
        {t(draft.busy ? "missions.lead.sending" : unresolved ? "missions.messageRecovery.checkReceipt" : "missions.lead.send")}
      </button>
    </div>
    {!draft.ready ? <p className="muted" role="status">{t("missions.composer.restoring")}</p> : null}
    {!draft.ready && draft.error ? <button type="button" onClick={() => void restoreMessageDraft(mission.id, task?.id ?? null)}>{t("missions.composer.restoreRetry")}</button> : null}
    {unresolved && !draft.busy ? <p className="muted" role="status" data-testid="message-receipt-note">{t("missions.composer.receiptPending")}</p> : null}
    {offline ? <p className="muted" data-testid="composer-offline">{t("missions.composer.lockedOffline")}</p> : null}
    {!allowed && !unresolved ? <p className="muted" data-testid="composer-locked">{t(lockedComposerTextKey(mission))}</p> : null}
    {tooLarge ? <p className="mission-area-error" role="alert">{t("missions.messageRecovery.tooLarge")}</p> : null}
    {draft.error ? <p className="mission-area-error" role="alert">{t(!draft.ready ? "missions.composer.restoreError" : unresolved ? "missions.composer.receiptError" : "missions.lead.sendError", { message: draft.error })}</p> : null}
    {next && nextReady ? <>
      <button type="button" onClick={next.run} data-testid={next.testId}>{t(next.label)}</button>
      {followUpNote}
    </> : null}
  </div>;
}

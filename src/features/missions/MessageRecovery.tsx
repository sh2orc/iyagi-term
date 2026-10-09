import { useRef, useState } from "react";
import type { Message } from "../../generated/Message";
import type { Mission } from "../../generated/Mission";
import { useI18n } from "../../i18n";
import { useMissionStore } from "./store";
import { MISSION_TEXT_MAX_BYTES } from "./artifactUpload";
import { restoreMessageDraft, setMessageDraft, submitMessage, useMessageDraft } from "./messageSubmission";

type Props = { mission: Mission; message: Message; text: string | null; bodyError: boolean; original?: Message; replacement?: Message };

function jump(id: string): void {
  const element = document.getElementById(`mission-message-${id}`);
  element?.scrollIntoView?.({ block: "center" });
  element?.focus({ preventScroll: true });
}

export function MessageRecovery(props: Props): JSX.Element | null {
  const { mission, message } = props;
  const { t } = useI18n();
  const decisionAnswer = useMissionStore(s => Object.values(s.decisions).some(d => d.mission_id === mission.id && d.answer_message_id === message.id));
  const target = useMissionStore(s => message.target_task_id ? s.tasks[message.target_task_id] : undefined);
  const { original, replacement } = props;
  const eligible = message.mission_id === mission.id && message.role === "user"
    && ["unknown", "rejected"].includes(message.delivery) && !decisionAnswer && !replacement
    && !mission.archived_at && !["stopping", "completed", "cancelled", "failed"].includes(mission.state)
    && (!message.target_task_id || (target?.mission_id === mission.id && target.kind !== "verify"
      && !["succeeded", "failed", "cancelled", "superseded"].includes(target.state)));
  const [open, setOpen] = useState(false);
  const state = useMessageDraft(mission.id, message.target_task_id, message.id,
    message.role === "user" && ["unknown", "rejected"].includes(message.delivery));
  const { text: draft, busy, saved, error } = state;
  const current = useRef({ missionId: mission.id, sourceId: message.id, eligible });
  current.current = { missionId: mission.id, sourceId: message.id, eligible };
  const setDraft = (text: string) => setMessageDraft(mission.id, message.target_task_id, text, message.id);
  const tooLarge = new TextEncoder().encode(draft.trim()).byteLength > MISSION_TEXT_MAX_BYTES;
  const unresolved = !!state.attempt?.params;
  const send = () => submitMessage(mission, target ?? null, message, () =>
    current.current.eligible && current.current.missionId === mission.id && current.current.sourceId === message.id);

  return <div className="mission-message-recovery" data-testid="message-recovery">
    {original ? <button type="button" onClick={() => jump(original.id)} data-testid="message-original-link">{t("missions.messageRecovery.original")}</button> : null}
    {replacement ? <button type="button" onClick={() => jump(replacement.id)} data-testid="message-replacement-link">{t("missions.messageRecovery.replacement")}</button> : null}
    {eligible && !saved && !open ? <button type="button" disabled={!state.ready || busy || props.bodyError || props.text === null}
      onClick={() => { setDraft(draft || props.text || ""); setOpen(true); }} data-testid="message-replace-open">
      {t("missions.messageRecovery.open")}
    </button> : null}
    {(open && eligible && !saved) || unresolved ? <div className="mission-message-recovery-form">
      <p className="muted">{t(message.delivery === "unknown" ? "missions.messageRecovery.unknown" : "missions.messageRecovery.rejected")}</p>
      <label htmlFor={`replacement-${message.id}`}>{t("missions.messageRecovery.body")}{target ? ` · ${target.title}` : ` · ${t("missions.lead.composerLabel")}`}</label>
      <textarea id={`replacement-${message.id}`} rows={3} maxLength={MISSION_TEXT_MAX_BYTES} value={draft} disabled={!state.ready || busy || unresolved}
        onChange={event => setDraft(event.target.value)} data-testid="message-replacement-body" />
      {tooLarge ? <p role="alert">{t("missions.messageRecovery.tooLarge")}</p> : null}
      <button type="button" disabled={!state.ready || busy || (!unresolved && (!draft.trim() || tooLarge))}
        onClick={() => void send()} data-testid="message-replacement-send">
        {t(unresolved ? "missions.messageRecovery.checkReceipt" : "missions.messageRecovery.send")}
      </button>
      {!unresolved ? <button type="button" disabled={busy} onClick={() => setOpen(false)}>{t("missions.messageRecovery.close")}</button> : null}
    </div> : null}
    {saved ? <p role="status" data-testid="message-replacement-saved">{t("missions.messageRecovery.saved")}</p> : null}
    {!state.ready && error ? <button type="button" onClick={() => void restoreMessageDraft(mission.id, message.target_task_id, message.id)}>{t("missions.composer.restoreRetry")}</button> : null}
    {error ? <p className="mission-area-error" role="alert">{t(!state.ready ? "missions.composer.restoreError" : unresolved ? "missions.composer.receiptError" : "missions.lead.sendError", { message: error })}</p> : null}
  </div>;
}

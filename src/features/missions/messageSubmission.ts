import { useEffect } from "react";
import { create } from "zustand";
import type { ArtifactRef } from "../../generated/ArtifactRef";
import type { Mission } from "../../generated/Mission";
import type { Message } from "../../generated/Message";
import type { MissionMessageParams } from "../../generated/MissionMessageParams";
import type { Task } from "../../generated/Task";
import { t } from "../../i18n";
import { MISSION_TEXT_MAX_BYTES, uploadTextArtifact } from "./artifactUpload";
import { getMissionClient } from "./clientAccess";
import { useMissionStore } from "./store";
import { errorText, newRequestId, isDeterministicIntegration } from "./viewUtils";
import { getMessageJournal, messageScope, PendingMessageConflict, readPendingMessageBody } from "./messageJournal";

type Attempt = { text: string; uploadId: string; reference?: ArtifactRef; params?: MissionMessageParams };
type Draft = { text: string; busy: boolean; ready: boolean; error: string | null; attempt?: Attempt; operation?: string; deferredText?: string; saved?: boolean };
const EMPTY: Draft = { text: "", busy: false, ready: false, error: null };
const UNUSED: Draft = { ...EMPTY, ready: true };

// Draft text stays in memory. Only a frozen request's metadata is journaled.
const useDraftStore = create<{ entries: Record<string, Draft> }>(() => ({ entries: {} }));

export function useMessageDraft(missionId: string, taskId: string | null, sourceId: string | null = null, enabled = true): Draft {
  const key = messageScope(missionId, taskId, sourceId);
  const draft = useDraftStore(s => enabled ? s.entries[key] ?? EMPTY : UNUSED);
  useEffect(() => { if (enabled) void restoreMessageDraft(missionId, taskId, sourceId); }, [missionId, taskId, sourceId, enabled]);
  return draft;
}

async function adoptPending(key: string, params: MissionMessageParams, deferredText = ""): Promise<void> {
  const operation = newRequestId();
  const attempt: Attempt = { text: "", uploadId: params.request_id, reference: params.body_ref, params };
  useDraftStore.setState(s => ({ entries: { ...s.entries, [key]: { ...EMPTY, ready: true, busy: true, attempt, operation, deferredText } } }));
  let text = "", error: string | null = null;
  try {
    const client = getMissionClient();
    if (!client) throw new Error(t("missions.sync.noClient"));
    text = await readPendingMessageBody(client, params.body_ref);
  } catch { error = t("missions.composer.bodyUnavailable"); }
  useDraftStore.setState(s => s.entries[key]?.operation === operation ? { entries: { ...s.entries,
    [key]: { ...s.entries[key], text, attempt: { ...attempt, text }, busy: false, operation: undefined, error } } } : s);
}

export async function restoreMessageDraft(missionId: string, taskId: string | null, sourceId: string | null = null): Promise<void> {
  const key = messageScope(missionId, taskId, sourceId);
  const draft = useDraftStore.getState().entries[key] ?? EMPTY;
  if (draft.ready || draft.operation) return;
  const operation = newRequestId();
  useDraftStore.setState(s => ({ entries: { ...s.entries, [key]: { ...draft, operation, error: null } } }));
  try {
    const pending = await getMessageJournal().load(key);
    if (useDraftStore.getState().entries[key]?.operation !== operation) return;
    if (pending) await adoptPending(key, pending, draft.text);
    else useDraftStore.setState(s => ({ entries: { ...s.entries, [key]: { ...draft, ready: true, operation: undefined, error: null } } }));
  } catch (cause) {
    useDraftStore.setState(s => s.entries[key]?.operation === operation ? { entries: { ...s.entries,
      [key]: { ...s.entries[key], operation: undefined, error: errorText(cause) } } } : s);
  }
}

export function setMessageDraft(missionId: string, taskId: string | null, text: string, sourceId: string | null = null): void {
  const key = messageScope(missionId, taskId, sourceId);
  useDraftStore.setState(s => {
    const draft = s.entries[key] ?? EMPTY;
    if (!draft.ready || draft.busy || draft.attempt?.params) return s;
    return { entries: { ...s.entries, [key]: { ...draft, text, error: null, saved: false,
      attempt: draft.attempt?.text === text.trim() ? draft.attempt : undefined } } };
  });
}

export function messageTargetAcceptsInput(mission: Mission, task: Task | null): boolean {
  return !mission.archived_at && !["stopping", "completed", "cancelled", "failed"].includes(mission.state)
    && (!task || (task.mission_id === mission.id && task.kind !== "verify" && !isDeterministicIntegration(task)
      && !["succeeded", "failed", "cancelled", "superseded"].includes(task.state)));
}

// These daemon errors prove that this mutation was rejected. Transport/storage
// errors and unknown error codes do not prove that the transaction did not commit.
export function messageRequestRejected(cause: unknown): boolean {
  return !!cause && typeof cause === "object" && "code" in cause && [
    "REVISION_CONFLICT", "INVALID_STATE", "NOT_FOUND", "POLICY_DENIED", "INVALID_ARGUMENT",
    "INTEGRITY_FAILED", "CONTEXT_TOO_LARGE", "PLAN_LIMIT", "MODEL_UNAVAILABLE", "BUDGET_EXCEEDED",
  ].includes(String(cause.code));
}

export async function submitMessage(mission: Mission, task: Task | null, source?: Message, eligible?: () => boolean): Promise<void> {
  const taskId = source ? source.target_task_id : task?.id ?? null;
  const key = messageScope(mission.id, taskId, source?.id ?? null);
  const draft = useDraftStore.getState().entries[key] ?? EMPTY;
  if (!draft.ready || draft.busy) return;
  const latestTarget = () => {
    const store = useMissionStore.getState();
    const latestMission = store.missions[mission.id] ?? mission;
    const latestTask = taskId ? store.tasks[taskId] : null;
    return { mission: latestMission, allowed: (!taskId || !!latestTask)
      && messageTargetAcceptsInput(latestMission, latestTask ?? null) && (!eligible || eligible()) };
  };
  if (!draft.attempt?.params && (!latestTarget().allowed || !draft.text.trim()
    || new TextEncoder().encode(draft.text.trim()).byteLength > MISSION_TEXT_MAX_BYTES)) return;

  const operation = newRequestId();
  let attempt: Attempt = draft.attempt ?? { text: draft.text.trim(), uploadId: newRequestId() };
  // Synchronous shared lock also covers two mounted panes and a second click
  // before React has committed the disabled button.
  useDraftStore.setState(s => ({ entries: { ...s.entries, [key]: { ...draft, attempt, operation, busy: true, error: null } } }));
  const current = () => useDraftStore.getState().entries[key]?.operation === operation;
  const update = (patch: Partial<Draft>) => useDraftStore.setState(s => current()
    ? { entries: { ...s.entries, [key]: { ...s.entries[key], ...patch } } } : s);
  let committed = false;
  try {
    const client = getMissionClient();
    if (!client) throw new Error(t("missions.sync.noClient"));
    if (!attempt.params) {
      const reference = attempt.reference ?? await uploadTextArtifact(client, attempt.text, {
        missionId: mission.id, requestId: attempt.uploadId,
      });
      if (!current()) return;
      attempt = { ...attempt, reference };
      update({ attempt });
      const latest = latestTarget();
      if (!latest.allowed) throw new Error(t(source ? "missions.messageRecovery.changed" : "missions.composer.closed"));
      attempt = { ...attempt, params: { request_id: newRequestId(), mission_id: mission.id,
        expected_revision: latest.mission.revision, target_task_id: taskId, body_ref: reference,
        ...(source ? { supersedes_message_id: source.id } : {}) } };
      update({ attempt });
    }
    // Replay the exact request, including its old revision, even if the target
    // has since stopped. The daemon checks its receipt before current state.
    // An atomic persistent slot prevents a different app window from replacing
    // an unresolved request. Persistence must finish before message mutation.
    await getMessageJournal().save(attempt.params!);
    if (!current()) return;
    try {
      await client.missionMessage(attempt.params!);
      committed = true;
    } catch (cause) {
      if (messageRequestRejected(cause)) {
        await getMessageJournal().forget(attempt.params!);
        attempt = { ...attempt, params: undefined };
        void useMissionStore.getState().syncMission(mission.id);
      }
      throw cause;
    }
    // Retain the exact request if clearing the journal fails, even on success.
    await getMessageJournal().forget(attempt.params!);
    useDraftStore.setState(s => {
      if (!current()) return s;
      return { entries: { ...s.entries, [key]: { ...EMPTY, ready: true, text: draft.deferredText ?? "", saved: !!source } } };
    });
    void useMissionStore.getState().syncMission(mission.id);
  } catch (cause) {
    if (cause instanceof PendingMessageConflict) {
      if (current()) await adoptPending(key, cause.pending, committed ? draft.deferredText : draft.text);
    } else update({ attempt, error: errorText(cause) });
  } finally {
    update({ busy: false, operation: undefined });
  }
}

export function resetMessageDraftsForTests(): void {
  useDraftStore.setState({ entries: {} });
}

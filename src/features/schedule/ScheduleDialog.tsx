/**
 * 예약 명령 대화상자: 대상 터미널·명령·시각을 받아 데몬에 예약하고
 * 목록(pending 카운트다운 + 최근 기록)을 보여 준다.
 *
 * 진실 원본은 데몬이므로 여기 상태는 폼뿐이다 — 목록은 scheduleStore
 * 캐시를 읽고, 만들기/취소 후 다시 가져온다. 구 데몬(schedule.* 없음)에서는
 * 목록이 비고 만들기가 안내 문구로 실패한다.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { useI18n } from "../../i18n";
import { getManagedRunDeps } from "../workloads/managedRunDeps";
import type { DaemonClient } from "../daemon/client";
import { useScheduleStore } from "./scheduleStore";
import {
  COMMAND_MAX_BYTES,
  countdownText,
  fromLocalInputValue,
  scheduleReasonText,
  scheduleStatusKey,
  toLocalInputValue,
  utf8ByteLength,
} from "./scheduleText";
import type { ScheduledCommand } from "../../generated/ScheduledCommand";
import type { PaneMeta } from "../../store/workbenchStore";

/** 카운트다운 갱신 주기 — 대화상자가 열려 있는 동안에만 돈다. */
const NOW_TICK_MS = 1_000;
/** 처음 열 때 시각 입력의 기본값: 5분 뒤. */
const DEFAULT_LEAD_MS = 5 * 60 * 1_000;
/** 퀵칩: [키, 오프셋 밀리초]. */
const QUICK_OFFSETS: ReadonlyArray<readonly [string, number]> = [
  ["schedule.quick.1m", 60 * 1_000],
  ["schedule.quick.5m", 5 * 60 * 1_000],
  ["schedule.quick.30m", 30 * 60 * 1_000],
  ["schedule.quick.1h", 60 * 60 * 1_000],
];
/** 최근 기록 상한(데몬 LIST_ROW_LIMIT보다 작게 — 눈으로 훑을 분량). */
const HISTORY_LIMIT = 20;

export interface ScheduleDialogProps {
  /** 트리거가 된 pane(패널 메뉴) — 기본 대상. 없으면 활성 pane. */
  leafId?: string;
  /** 없으면 모듈 seam(managedRunDeps)의 클라이언트를 쓴다(시험 주입). */
  client?: ScheduleDialogClient | null;
}

/** 만들기·목록·취소에 필요한 최소 클라이언트(시험 주입 seam). */
export type ScheduleDialogClient = Pick<DaemonClient, "scheduleCreate" | "scheduleList" | "scheduleCancel">;

export function ScheduleDialog(props: ScheduleDialogProps): JSX.Element {
  const { t } = useI18n();
  const closeModal = useWorkbenchStore((s) => s.closeModal);
  const panes = useWorkbenchStore((s) => s.panes);
  const focusedLeafId = useWorkbenchStore((s) => s.focusedLeafId);
  const items = useScheduleStore((s) => s.items);
  const refresh = useScheduleStore((s) => s.refresh);
  const client = props.client !== undefined ? props.client : (getManagedRunDeps()?.client ?? null);

  const [command, setCommand] = useState("");
  const [timeValue, setTimeValue] = useState(() => toLocalInputValue(Date.now() + DEFAULT_LEAD_MS));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [now, setNow] = useState(() => Date.now());

  /** sessionId가 있는 pane만 예약 대상이 될 수 있다. */
  const targets = useMemo(
    () => Object.values(panes).filter((pane): pane is PaneMeta & { sessionId: string } => pane.sessionId != null),
    [panes],
  );
  const [targetSession, setTargetSession] = useState(() => {
    const preferred = props.leafId ?? focusedLeafId;
    return (
      targets.find((pane) => pane.leafId === preferred)?.sessionId
      ?? targets[0]?.sessionId
      ?? ""
    );
  });

  // 열려 있는 동안 대상 목록이 바뀌면(pane 닫힘·분할) 선택을 살아 있는 것으로 맞춘다.
  useEffect(() => {
    if (targets.length === 0) {
      if (targetSession !== "") setTargetSession("");
      return;
    }
    if (!targets.some((pane) => pane.sessionId === targetSession)) {
      setTargetSession(targets[0].sessionId);
    }
  }, [targets, targetSession]);

  // 카운트다운은 1초 단위로 흐른다 — 행 데이터는 다시 읽지 않는다.
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), NOW_TICK_MS);
    return () => clearInterval(timer);
  }, []);

  useEffect(() => {
    if (client) void refresh(client);
  }, [client, refresh]);

  const submit = useCallback(() => {
    if (busy || !client) return;
    if (targetSession === "") {
      setError(t("schedule.target.none"));
      return;
    }
    if (command === "" || utf8ByteLength(command) > COMMAND_MAX_BYTES) {
      setError(t("schedule.error.command"));
      return;
    }
    const fireAt = fromLocalInputValue(timeValue);
    if (Number.isNaN(fireAt) || fireAt <= Date.now()) {
      setError(t("schedule.error.past"));
      return;
    }
    setBusy(true);
    setError(null);
    void client
      .scheduleCreate({ session_id: targetSession, command, fire_at_ms: fireAt })
      .then(() => refresh(client))
      .then(() => setCommand(""))
      .catch((err: unknown) => {
        setError(t("schedule.error.create", { message: err instanceof Error ? err.message : String(err) }));
      })
      .finally(() => setBusy(false));
  }, [busy, client, command, refresh, t, targetSession, timeValue]);

  const cancel = useCallback(
    (scheduleId: string) => {
      if (!client) return;
      void client
        .scheduleCancel({ schedule_id: scheduleId })
        .then(() => refresh(client))
        .catch(() => undefined);
    },
    [client, refresh],
  );

  const pending = useMemo(
    () =>
      items
        .filter((item) => item.status === "pending" || item.status === "firing")
        .sort((a, b) => a.fire_at_ms - b.fire_at_ms),
    [items],
  );
  const history = useMemo(
    () => items.filter((item) => item.status !== "pending" && item.status !== "firing").slice(0, HISTORY_LIMIT),
    [items],
  );

  const paneTitleFor = useCallback(
    (sessionId: string): string => {
      const pane = targets.find((candidate) => candidate.sessionId === sessionId);
      if (!pane) return sessionId.slice(0, 8);
      return pane.terminalTitle ?? pane.title;
    },
    [targets],
  );

  return (
    <div
      className="schedule-dialog"
      onKeyDown={(e) => {
        if (e.key === "Escape") closeModal();
      }}
    >
      <h2>{t("schedule.title")}</h2>
      <p className="muted">{t("schedule.hint")}</p>

      <label className="schedule-field">
        <span>{t("schedule.target")}</span>
        <select
          value={targetSession}
          onChange={(e) => setTargetSession(e.target.value)}
          disabled={targets.length === 0}
          aria-label={t("schedule.target")}
        >
          {targets.length === 0 ? <option value="">{t("schedule.target.none")}</option> : null}
          {targets.map((pane) => (
            <option key={pane.sessionId} value={pane.sessionId}>
              {pane.terminalTitle ?? pane.title}
            </option>
          ))}
        </select>
      </label>

      <label className="schedule-field">
        <span>{t("schedule.command")}</span>
        <input
          className="palette-input"
          autoFocus
          value={command}
          placeholder={t("schedule.command.placeholder")}
          aria-label={t("schedule.command")}
          onChange={(e) => setCommand(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submit();
          }}
        />
        <small className="muted">{t("schedule.command.hint")}</small>
      </label>

      <label className="schedule-field">
        <span>{t("schedule.time")}</span>
        <input
          type="datetime-local"
          value={timeValue}
          aria-label={t("schedule.time")}
          onChange={(e) => setTimeValue(e.target.value)}
        />
      </label>
      <div className="schedule-quick" role="group" aria-label={t("schedule.time")}>
        {QUICK_OFFSETS.map(([key, offset]) => (
          <button
            key={key}
            type="button"
            onClick={() => setTimeValue(toLocalInputValue(Date.now() + offset))}
          >
            {t(key)}
          </button>
        ))}
      </div>

      <div className="modal-actions">
        <button type="button" onClick={closeModal}>
          {t("schedule.close")}
        </button>
        <button type="button" disabled={busy || targetSession === ""} onClick={submit}>
          {t("schedule.submit")}
        </button>
      </div>
      {error ? <p className="muted" role="alert">{error}</p> : null}

      <h3>{t("schedule.list.title")}</h3>
      {pending.length === 0 && history.length === 0 ? (
        <p className="muted">{t("schedule.list.empty")}</p>
      ) : null}
      {pending.length > 0 ? (
        <>
          <h4>{t("schedule.list.pending")}</h4>
          <ul className="schedule-list">
            {pending.map((item) => (
              <ScheduleRow
                key={item.schedule_id}
                item={item}
                now={now}
                paneTitle={paneTitleFor(item.session_id)}
                onCancel={item.status === "pending" ? () => cancel(item.schedule_id) : null}
              />
            ))}
          </ul>
        </>
      ) : null}
      {history.length > 0 ? (
        <>
          <h4>{t("schedule.list.history")}</h4>
          <ul className="schedule-list">
            {history.map((item) => (
              <ScheduleRow
                key={item.schedule_id}
                item={item}
                now={now}
                paneTitle={paneTitleFor(item.session_id)}
                onCancel={null}
              />
            ))}
          </ul>
        </>
      ) : null}
    </div>
  );
}

function ScheduleRow(props: {
  item: ScheduledCommand;
  now: number;
  paneTitle: string;
  onCancel: (() => void) | null;
}): JSX.Element {
  const { t } = useI18n();
  const item = props.item;
  const pending = item.status === "pending" || item.status === "firing";
  return (
    <li className={`schedule-row schedule-${item.status}`}>
      <span className="schedule-status" data-status={item.status}>{t(scheduleStatusKey(item.status))}</span>
      <span className="schedule-command" title={item.command}>{item.command}</span>
      <span className="schedule-target" title={props.paneTitle}>{props.paneTitle}</span>
      {pending ? (
        <span className="schedule-when">{countdownText(item.fire_at_ms, props.now, t)}</span>
      ) : item.status === "fired" ? null : (
        <span className="schedule-when">{scheduleReasonText(item.reason_code, t) ?? ""}</span>
      )}
      {item.status === "fired" && item.fired_at_ms != null ? (
        <span className="schedule-when">{new Date(item.fired_at_ms).toLocaleTimeString()}</span>
      ) : null}
      {props.onCancel ? (
        <button type="button" className="schedule-cancel" onClick={props.onCancel}>
          {t("schedule.list.cancel")}
        </button>
      ) : null}
    </li>
  );
}

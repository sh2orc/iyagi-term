/**
 * "최근 에이전트 세션" 대화상자(04-ui.md §5).
 *
 * 데몬이 기록한 세션(`agent_session.list`)을 그대로 보여 주기만 한다.
 * 자동 재실행은 없다 — 살아 있으면 그 pane으로 "이동", 끝났으면 사용자가
 * "이어서 열기"를 눌렀을 때만 기록된 cwd에서 새 실행을 만든다.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { AgentSessionRecord } from "../../generated/AgentSessionRecord";
import type { DaemonClient } from "../daemon/client";
import { getManagedRunDeps } from "../workloads/managedRunDeps";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { useController } from "../../app/controllerContext";
import { agentDisplayName } from "../terminal/agentNames";
import { abbreviateHome } from "../terminal/displayPath";
import { useI18n } from "../../i18n";
import { agentSessionActions } from "./actions";
import { relativeTimeText, resumeInfoFrom, sessionDisplayLabel } from "./types";

/** 목록을 읽고 지우는 데 필요한 최소 클라이언트(시험 주입 seam). */
export type AgentSessionsClient = Pick<DaemonClient, "agentSessionList" | "agentSessionForget">;

export interface AgentSessionsDialogProps {
  /** 없으면 모듈 seam(managedRunDeps)의 클라이언트를 쓴다. */
  client?: AgentSessionsClient | null;
  /** 주면 첫 로딩을 건너뛴다(시험·미리보기). */
  records?: AgentSessionRecord[];
  onClose?: () => void;
}

type LoadState =
  | { status: "loading" }
  | { status: "ready"; records: AgentSessionRecord[] }
  | { status: "error" };

/** 상대 시각 기준점을 다시 잡는 주기 — 목록이 열려 있는 동안에도 흐른다. */
const NOW_REFRESH_MS = 30_000;

export function AgentSessionsDialog(props: AgentSessionsDialogProps): JSX.Element {
  const { t } = useI18n();
  const controller = useController();
  const closeModal = useWorkbenchStore((s) => s.closeModal);
  const close = props.onClose ?? closeModal;
  const client = props.client !== undefined ? props.client : (getManagedRunDeps()?.client ?? null);
  const [state, setState] = useState<LoadState>(() =>
    props.records ? { status: "ready", records: props.records } : { status: "loading" },
  );
  const [actionError, setActionError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());

  /**
   * 로드마다 순번을 올린다 — 제거를 빠르게 두 번 눌러 로드가 겹쳐도 가장
   * 마지막에 시작한 것의 결과만 화면에 들어간다(먼저 끝난 옛 응답이
   * 이기지 않게). 언마운트도 순번을 올려 진행 중인 응답을 버린다.
   */
  const loadSeq = useRef(0);

  const load = useCallback(() => {
    if (!client) {
      setState({ status: "error" });
      return;
    }
    const seq = (loadSeq.current += 1);
    setState({ status: "loading" });
    void client
      .agentSessionList({ limit: 100 })
      .then((records) => {
        if (seq !== loadSeq.current) return;
        setNow(Date.now());
        setState({ status: "ready", records });
      })
      .catch(() => {
        // 구 데몬(메서드 없음)도 여기로 온다 — 목록 대신 안내 문구를 낸다.
        if (seq === loadSeq.current) setState({ status: "error" });
      });
  }, [client]);

  useEffect(() => {
    if (props.records) return;
    load();
    return () => {
      loadSeq.current += 1;
    };
  }, [load, props.records]);

  // 상대 시각은 목록을 열어 둔 동안에도 흘러간다 — 로드 때 말고도 주기적으로
  // 기준점을 다시 잡는다. 행을 다시 읽지는 않는다(목록은 사용자 동작으로만).
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), NOW_REFRESH_MS);
    return () => clearInterval(timer);
  }, []);

  const actions = useMemo(
    () =>
      agentSessionActions({
        client: client ?? { agentSessionForget: async () => ({ forgotten: false }) },
        controller,
        close,
        reload: load,
        onError: setActionError,
      }),
    [client, controller, close, load],
  );

  const records = state.status === "ready" ? state.records : [];

  return (
    <div
      className="agent-sessions-dialog"
      onKeyDown={(e) => {
        if (e.key === "Escape") close();
      }}
    >
      <h2>{t("agentSessions.title")}</h2>
      <p className="muted">{t("agentSessions.hint")}</p>
      {state.status === "loading" ? <p className="muted" role="status">{t("agentSessions.loading")}</p> : null}
      {state.status === "error" ? <p className="muted" role="alert">{t("agentSessions.error")}</p> : null}
      {state.status === "ready" && records.length === 0 ? (
        <p className="muted">{t("agentSessions.empty")}</p>
      ) : null}
      {records.length > 0 ? (
        <ul className="agent-session-list">
          {records.map((record) => (
            <AgentSessionRow key={record.id} record={record} now={now} actions={actions} />
          ))}
        </ul>
      ) : null}
      {actionError ? <p className="muted" role="alert">{actionError}</p> : null}
      <div className="modal-actions">
        {/* 다른 대화상자(Modals.tsx)와 같은 초점 규칙: 닫기 버튼이 초점을
            받아 Escape가 바로 이 컨테이너의 핸들러까지 올라온다. */}
        <button type="button" autoFocus onClick={close}>
          {t("agentSessions.close")}
        </button>
      </div>
    </div>
  );
}

function AgentSessionRow(props: {
  record: AgentSessionRecord;
  now: number;
  actions: ReturnType<typeof agentSessionActions>;
}): JSX.Element {
  const { t } = useI18n();
  const { record, now, actions } = props;
  const homeDir = useWorkbenchStore((s) => s.homeDir);
  const label = sessionDisplayLabel(record.title, record.agent_session_id);
  // 재개 인자를 만들 수 있을 때만 누를 수 있다 — 에이전트 종류와 세션 id의
  // 안전성(isSafeSessionId) 둘 다 resumeInfoFrom이 판정한다.
  const resumable = resumeInfoFrom(record) !== null;
  // 가드 일시정지(08 §5): 살아 있는 대화라도 정지 중이면 "실행 중" 옆에 알린다 —
  // 여기서는 pane 헤더 배지가 보이지 않아 "이동"했을 때 비로소 드러난다. 이유는
  // 파생 인덱스로 문자열 프리미티브 하나만 구독한다(스냅샷마다 행이 다시 그려지지
  // 않게 — TerminalPane의 guardKey와 같은 규율).
  const guardReason = useWorkbenchStore((s) => {
    if (!record.active || !record.pty_session_id) return "";
    const workload = s.workloadBySession.get(record.pty_session_id);
    return workload?.guard?.kind === "SUSPENDED" ? workload.guard.reason : "";
  });
  return (
    <li className={`agent-session-row${record.active ? " active" : ""}`}>
      <span className="agent-session-agent">{agentDisplayName(record.agent)}</span>
      <span className="agent-session-label" title={record.agent_session_id}>{label}</span>
      <span className="agent-session-cwd" title={record.cwd}>{abbreviateHome(record.cwd, homeDir)}</span>
      <span className="agent-session-at" title={record.last_seen_at}>
        {relativeTimeText(record.last_seen_at, now)}
      </span>
      {record.active ? <span className="agent-session-live">{t("agentSessions.active")}</span> : null}
      {guardReason ? (
        <span className="agent-session-suspended" title={t(`queue.guard.reason.${guardReason}`)}>
          {t("agentSessions.suspended")}
        </span>
      ) : null}
      <span className="agent-session-actions">
        {record.active ? (
          <button
            type="button"
            disabled={!record.pty_session_id}
            title={record.pty_session_id ? undefined : t("agentSessions.gotoUnavailable")}
            onClick={() => actions.focus(record)}
          >
            {t("agentSessions.goto")}
          </button>
        ) : (
          <button
            type="button"
            className="primary"
            disabled={!resumable}
            title={resumable ? undefined : t("agentSessions.notResumable")}
            onClick={() => void actions.resume(record)}
          >
            {t("agentSessions.resume")}
          </button>
        )}
        <button
          type="button"
          aria-label={t("agentSessions.forgetAria", { label })}
          onClick={() => void actions.forget(record)}
        >
          {t("agentSessions.forget")}
        </button>
      </span>
    </li>
  );
}

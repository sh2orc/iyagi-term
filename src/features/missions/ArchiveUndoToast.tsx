/**
 * 보관 후 되돌리기 토스트(05 §2·§8).
 *
 * 보관은 탭을 닫으므로 MissionPage 안에는 되돌리기 버튼을 둘 곳이 없고, 워크벤치
 * toast는 문자열만 받는다. 그래서 이 토스트는 document.body에 자기 root를 하나
 * 만들어 스스로 그리고 스스로 치운다(한 번에 하나). 되돌리기는
 * mission.control(unarchive)이고, 성공하면 탭을 다시 연다.
 *
 * 보관한 작업의 작업 공간을 정리할 수 있으면(계약 D) 짧은 안내와 목록 열기만 덧붙인다 —
 * 별도 팝업은 띄우지 않고, 사용량 조회 실패는 조용히 숨긴다.
 *
 * 낭독 영역은 겹치지 않는다: 안내 문구·정리 안내는 각자 role="status", 되돌리기 실패는
 * MissionErrorNotice의 role="alert" — 토스트 전체를 status로 감싸면 alert가 그 안에 중첩된다.
 */

import { useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { useI18n } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { getMissionClient } from "./clientAccess";
import { MissionErrorNotice, missionError, type MissionUiError } from "./errors";
import { mutateWithResync } from "./mutation";
import { useMissionStore } from "./store";
import { newRequestId } from "./viewUtils";
import { useWorkspaceUsage } from "./WorkspaceCleanup";
import { formatStorageSize, workspaceCleanable, workspaceUsageEntry } from "./workspaceUsage";
import "./missionPage.css";

/** 되돌리기를 누르지 않으면 이 시간 뒤 사라진다. */
export const ARCHIVE_UNDO_MS = 8000;

const TERMINAL = new Set(["completed", "failed", "cancelled"]);

interface ActiveToast {
  root: Root;
  host: HTMLElement;
  timer: ReturnType<typeof setTimeout> | null;
}

let active: ActiveToast | null = null;

function dispose(handle: ActiveToast): void {
  if (handle.timer !== null) clearTimeout(handle.timer);
  handle.timer = null;
  if (active === handle) active = null;
  // 자기 이벤트 처리 중에 root를 동기로 치우지 않는다.
  setTimeout(() => {
    handle.root.unmount();
    handle.host.remove();
  }, 0);
}

/** 떠 있는 보관 토스트를 닫는다(새 토스트·시험 정리). */
export function dismissArchiveUndoToast(): void {
  if (active) dispose(active);
}

export function showArchiveUndoToast(params: { missionId: string; title: string }): void {
  dismissArchiveUndoToast();
  if (typeof document === "undefined") return;
  const host = document.createElement("div");
  host.className = "mission-undo-toast-host";
  document.body.appendChild(host);
  const handle: ActiveToast = { root: createRoot(host), host, timer: null };
  handle.timer = setTimeout(() => dispose(handle), ARCHIVE_UNDO_MS);
  active = handle;
  handle.root.render(
    <ArchiveUndoToast
      missionId={params.missionId}
      title={params.title}
      onHold={() => {
        if (handle.timer !== null) clearTimeout(handle.timer);
        handle.timer = null;
      }}
      onDone={() => dispose(handle)}
    />,
  );
}

function ArchiveUndoToast(props: {
  missionId: string;
  title: string;
  /** 사용자가 손을 댔다 — 자동으로 사라지지 않게 한다. */
  onHold: () => void;
  onDone: () => void;
}): JSX.Element {
  const { t } = useI18n();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<MissionUiError | null>(null);
  const workspace = useWorkspaceUsage(props.missionId);
  const workspaceEntry = workspaceUsageEntry(workspace.usage, props.missionId);

  const undo = async () => {
    props.onHold();
    const client = getMissionClient();
    if (!client) {
      setError(missionError(t, new Error(t("missions.sync.noClient"))));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      // 보관 직후라 store의 archived_at은 아직 비어 있을 수 있다 — 종료 상태만 전제로 본다.
      await mutateWithResync(props.missionId, (current) =>
        TERMINAL.has(current.state)
          ? client.missionControl({
              request_id: newRequestId(),
              mission_id: current.id,
              expected_revision: current.revision,
              action: "unarchive",
            })
          : null,
      );
      useWorkbenchStore.getState().openMissionTab(props.missionId, props.title);
      void useMissionStore.getState().syncMission(props.missionId);
      props.onDone();
    } catch (cause) {
      setError(missionError(t, cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="toast mission-undo-toast" data-testid="archive-undo-toast">
      <span role="status">{t("missions.control.archived")}</span>
      <button type="button" className="link" disabled={busy} onClick={() => void undo()} data-testid="archive-undo">
        {t("missions.control.undoArchive")}
      </button>
      <button
        type="button"
        className="mission-undo-toast-close"
        aria-label={t("missions.common.close")}
        onClick={props.onDone}
      >
        ×
      </button>
      {workspaceEntry !== null && workspaceCleanable(workspaceEntry) ? (
        <span className="mission-undo-toast-hint" role="status" data-testid="archive-cleanup-hint">
          {t("missions.workspaceCleanup.archivedHint", { size: formatStorageSize(workspaceEntry.bytes) })}{" "}
          <button
            type="button"
            className="link"
            onClick={() => {
              useWorkbenchStore.getState().openModal({ kind: "mission-list" });
              props.onDone();
            }}
            data-testid="archive-cleanup-open-list"
          >
            {t("missions.workspaceCleanup.openList")}
          </button>
        </span>
      ) : null}
      {error ? (
        <MissionErrorNotice
          error={error}
          onRetry={() => void undo()}
          onAction={(action) => {
            if (action === "resync") void useMissionStore.getState().syncMission(props.missionId);
            else if (action === "retry") void undo();
          }}
        />
      ) : null}
    </div>
  );
}

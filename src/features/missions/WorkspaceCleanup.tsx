/**
 * 작업 공간 정리(계약 D) 화면 조각 — AI 작업 목록 행, 확정 뒤 결과 화면, 보관 토스트가 함께 쓴다.
 *
 * - useWorkspaceUsage: `workspace.usage` 조회. 실패는 조용히 숨긴다(오류 배너 없음).
 * - WorkspaceCleanup: 정리 버튼 → 확인창(지우는 것·남는 것·원래 저장소 불변) →
 *   `workspace.cleanup` → 제거 수·확보 용량·남긴 항목과 이유. 정리 실패는 MissionErrorNotice.
 *   mission revision과 무관한 요청이라 mutateWithResync를 쓰지 않는다.
 */

import { useCallback, useEffect, useState } from "react";
import type { WorkspaceCleanupResult } from "../../generated/WorkspaceCleanupResult";
import type { WorkspaceUsageEntry } from "../../generated/WorkspaceUsageEntry";
import type { WorkspaceUsageResult } from "../../generated/WorkspaceUsageResult";
import { useI18n } from "../../i18n";
import { getMissionClient } from "./clientAccess";
import { MissionErrorNotice, missionError, withSupportedActions, type MissionUiError } from "./errors";
import { newRequestId } from "./viewUtils";
import {
  formatStorageSize,
  workspaceBlockedReasonKey,
  workspaceCleanable,
  workspaceCleanupBlocked,
  workspaceKeptReasonKey,
} from "./workspaceUsage";
import "./integration.css";

export interface WorkspaceUsageState {
  /** null = 아직 모르거나 조회 실패(화면은 숨긴다). */
  usage: WorkspaceUsageResult | null;
  refresh: () => void;
}

/**
 * 작업 공간 사용량. missionId null이면 전체. enabled가 false면 조회하지 않는다.
 * 데몬이 아직 이 RPC를 모르거나 실패하면 null로 남긴다.
 */
export function useWorkspaceUsage(missionId: string | null, enabled = true): WorkspaceUsageState {
  const [usage, setUsage] = useState<WorkspaceUsageResult | null>(null);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    if (!enabled) return;
    const client = getMissionClient();
    if (!client) return;
    let alive = true;
    let pending: Promise<WorkspaceUsageResult>;
    try {
      pending = client.workspaceUsage({ mission_id: missionId });
    } catch {
      return;
    }
    Promise.resolve(pending).then(
      (result) => {
        if (alive) setUsage(result);
      },
      () => {
        if (alive) setUsage(null);
      },
    );
    return () => {
      alive = false;
    };
  }, [missionId, enabled, tick]);
  const refresh = useCallback(() => setTick((value) => value + 1), []);
  return { usage, refresh };
}

export interface WorkspaceCleanupProps {
  missionId: string;
  entry: WorkspaceUsageEntry | null;
  /** row: 목록 행(막힌 이유도 보인다) · suggest: 확정 뒤 제안(정리 가능할 때만). */
  variant: "row" | "suggest";
  /** 정리 후·동기화 요청 시 사용량을 다시 읽는다. */
  onRefresh?: () => void;
}

export function WorkspaceCleanup(props: WorkspaceCleanupProps): JSX.Element | null {
  const { t } = useI18n();
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<MissionUiError | null>(null);
  const [result, setResult] = useState<WorkspaceCleanupResult | null>(null);
  const titleId = `workspace-cleanup-title-${props.missionId}`;

  const cleanup = async () => {
    setConfirmOpen(false);
    const client = getMissionClient();
    if (!client) {
      setError(missionError(t, new Error(t("missions.sync.noClient"))));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const cleaned = await client.workspaceCleanup({ request_id: newRequestId(), mission_id: props.missionId });
      setResult(cleaned);
      props.onRefresh?.();
    } catch (cause) {
      setError(missionError(t, cause));
    } finally {
      setBusy(false);
    }
  };

  const errorNotice = error ? (
    <MissionErrorNotice
      error={withSupportedActions(error, ["resync", "retry"])}
      // 지우는 동작이라 다시 시도도 확인창부터 다시 받는다.
      onRetry={() => {
        setError(null);
        setConfirmOpen(true);
      }}
      onAction={(action) => {
        if (action === "resync") {
          setError(null);
          props.onRefresh?.();
        }
      }}
    />
  ) : null;

  if (result) {
    return (
      <div className="mission-workspace-cleanup" data-testid="workspace-cleanup-result">
        <p className="mission-workspace-cleanup-done">
          {t("missions.workspaceCleanup.done", { removed: result.removed, size: formatStorageSize(result.freed_bytes) })}
        </p>
        {result.kept.length > 0 ? (
          <div className="mission-workspace-kept" data-testid="workspace-cleanup-kept">
            <p className="muted">{t("missions.workspaceCleanup.keptTitle", { count: result.kept.length })}</p>
            <ul>
              {result.kept.map((kept) => (
                <li key={kept.path} data-testid="workspace-kept-item">
                  <code className="mission-workspace-kept-path">{kept.path}</code>
                  <span className="muted"> · {t(workspaceKeptReasonKey(kept.reason))}</span>
                </li>
              ))}
            </ul>
          </div>
        ) : null}
        {errorNotice}
      </div>
    );
  }

  const entry = props.entry;
  if (entry !== null && workspaceCleanable(entry)) {
    const size = formatStorageSize(entry.bytes);
    return (
      <div className="mission-workspace-cleanup" data-testid="workspace-cleanup">
        <button
          type="button"
          className="mission-workspace-cleanup-button"
          disabled={busy}
          onClick={() => {
            setError(null);
            setConfirmOpen(true);
          }}
          data-testid="workspace-cleanup-button"
        >
          {busy
            ? t("missions.workspaceCleanup.running")
            : t(props.variant === "suggest" ? "missions.workspaceCleanup.suggest" : "missions.workspaceCleanup.action", { size })}
        </button>
        {confirmOpen ? (
          <div
            className="mission-inline-confirm mission-cleanup-confirm"
            role="alertdialog"
            aria-labelledby={titleId}
            data-testid="workspace-cleanup-confirm"
          >
            <h4 id={titleId}>{t("missions.workspaceCleanup.confirmTitle")}</h4>
            <p>{t("missions.workspaceCleanup.confirmBody")}</p>
            <div className="mission-cleanup-confirm-actions">
              <button type="button" className="danger" onClick={() => void cleanup()} data-testid="workspace-cleanup-ok">
                {t("missions.workspaceCleanup.confirmOk")}
              </button>
              <button type="button" onClick={() => setConfirmOpen(false)} data-testid="workspace-cleanup-cancel">
                {t("missions.common.cancel")}
              </button>
            </div>
          </div>
        ) : null}
        {errorNotice}
      </div>
    );
  }

  if (props.variant === "row" && entry !== null && workspaceCleanupBlocked(entry)) {
    return (
      <div className="mission-workspace-cleanup" data-testid="workspace-cleanup">
        <span className="muted mission-workspace-blocked" data-testid="workspace-cleanup-blocked">
          {t(workspaceBlockedReasonKey(entry.blocked_reason))} · {formatStorageSize(entry.bytes)}
        </span>
        {errorNotice}
      </div>
    );
  }

  return errorNotice ? <div className="mission-workspace-cleanup">{errorNotice}</div> : null;
}

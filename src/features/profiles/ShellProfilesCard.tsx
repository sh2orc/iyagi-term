/**
 * 셸 명령(ccd/ccg) 설치 카드 — 설정 → 실행 프로필 맨 아래.
 *
 * 다른 연동 카드(IntegrationPanel의 hooks·사용량)와 같은 동의 흐름을 쓴다:
 * 상태 조회 → 추가될 줄 미리보기 → 사용자가 누른 뒤에만 파일을 고친다.
 * 설치 여부·충돌 판정은 shellProfiles.ts의 순수 함수가 하고, 이 파일은
 * 그 판정을 그리기만 한다.
 */

import { useCallback, useEffect, useState } from "react";
import { isTauri } from "../bridge/ipc";
import { useI18n } from "../../i18n";
import { usePreferences } from "../../store/preferences";
import {
  addedLines,
  applyShellProfiles,
  classifyShellProfiles,
  errorText,
  getShellProfilesStatus,
  isConflictError,
  manualSnippet,
  rcBlock,
  removeShellProfiles,
  type ShellProfileConflict,
  type ShellProfilesReason,
  type ShellProfilesStatus,
} from "./shellProfiles";

/** 사유별 문구 키 — 데몬이 모르는 사유를 주더라도 키가 그대로 보이지 않게 한다. */
const UNSUPPORTED_REASONS: readonly ShellProfilesReason[] = [
  "windows",
  "shell_not_zsh",
  "daemon_binary_missing",
  "no_home",
];

function unsupportedKey(reason: ShellProfilesReason | null | undefined): string {
  return reason && UNSUPPORTED_REASONS.includes(reason)
    ? `settings.shellProfiles.unsupported.${reason}`
    : "settings.shellProfiles.unsupported.unknown";
}

/** 충돌 목록 — 어느 파일 몇 번째 줄인지 원문 그대로 보여 준다(손대지 않는다는 증거). */
function ConflictList({ conflicts }: { conflicts: ShellProfileConflict[] }): JSX.Element {
  return (
    <ul className="shell-profiles-conflicts">
      {conflicts.map((conflict) => (
        <li key={`${conflict.file}:${conflict.line}:${conflict.name}`}>
          <code>{conflict.name}</code>
          <span className="muted"> · {conflict.line > 0 ? `${conflict.file}:${conflict.line}` : conflict.file}</span>
          <pre className="hooks-snippet">{conflict.text}</pre>
        </li>
      ))}
    </ul>
  );
}

export function ShellProfilesCard(): JSX.Element {
  const { t } = useI18n();
  const available = isTauri();
  // ccg가 쓸 주 모델 = Z.ai 라우팅 설정과 같은 값. 바뀌면 다시 조회해
  // "다시 적용" 여부(upToDate)를 데몬이 판정하게 한다.
  const mainModel = usePreferences((s) => s.zaiMainModel);
  const [status, setStatus] = useState<ShellProfilesStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // 첫 조회가 끝나기 전에는 "no status" 오류를 띄우지 않는다.
  const [loaded, setLoaded] = useState(false);

  const refresh = useCallback(async () => {
    if (!available) return;
    setBusy(true);
    setError(null);
    try { setStatus(await getShellProfilesStatus(mainModel)); }
    catch (value) { setError(errorText(value)); }
    finally { setBusy(false); setLoaded(true); }
  }, [available, mainModel]);

  useEffect(() => { void refresh(); }, [refresh]);

  // "replace"는 사용자 정의 ccd/ccg를 지우지 않고 우리 블록을 rc 끝으로 옮겨
  // 가리는 적용이다. 이미 교체해 둔 설치를 다시 적용할 때도 같은 길을 쓴다.
  const run = async (action: "apply" | "replace" | "remove"): Promise<void> => {
    setBusy(true);
    setError(null);
    try {
      setStatus(
        action === "remove"
          ? await removeShellProfiles(mainModel)
          : await applyShellProfiles(mainModel, action === "replace"),
      );
    } catch (value) {
      setError(errorText(value));
      // 충돌로 거절됐다면 어떤 줄이 걸렸는지 보여 주려고 상태를 다시 읽는다.
      if (isConflictError(value)) {
        try { setStatus(await getShellProfilesStatus(mainModel)); }
        catch { /* 위에서 담은 오류 문구를 그대로 둔다. */ }
      }
    } finally { setBusy(false); }
  };

  const view = classifyShellProfiles(status, available ? { kind: "ready" } : { kind: "not-tauri" });

  return (
    <section className="integration-block" data-setting-id="shellProfiles">
      <h3>{t("settings.item.shellProfiles.label")}</h3>
      <p className="muted">{t("settings.item.shellProfiles.description")}</p>
      {!available ? (
        <div className="hooks-fallback">
          <p>{t("settings.shellProfiles.notTauri")}</p>
          <pre className="hooks-snippet">{manualSnippet(null)}</pre>
        </div>
      ) : !loaded && error === null ? (
        <p className="muted" role="status">{t("settings.shellProfiles.busy")}</p>
      ) : view.kind === "unavailable" ? (
        view.reason === "unsupported" ? (
          <p className="muted" role="status">
            {t(unsupportedKey(view.unsupported), { shell: view.status?.shell ?? "?" })}
          </p>
        ) : (
          <p className="setting-error" role="alert">
            {t("settings.shellProfiles.error", { message: error ?? view.message ?? "" })}
          </p>
        )
      ) : (
        <>
          <p className="muted">
            {t("settings.shellProfiles.rcPath")}: <code className="hooks-path">{view.status.rcPath}</code>
            {view.status.rcExists ? null : <span> {t("settings.shellProfiles.rcMissing")}</span>}
          </p>
          <p className="muted">
            {t("settings.shellProfiles.daemonBinary")}: <code className="hooks-path">{view.status.daemonBinary ?? "—"}</code>
          </p>
          <p className="muted">{t("settings.shellProfiles.model", { model: view.status.mainModel })}</p>

          {(view.kind === "installed" || view.kind === "outdated") && view.status.overriding ? (
            <>
              <p className="muted">{t("settings.shellProfiles.overriding")}</p>
              <ConflictList conflicts={view.status.conflicts} />
            </>
          ) : null}

          {view.kind === "installed" ? (
            <>
              <p role="status">{t("settings.shellProfiles.installed")}</p>
              <p className="muted">{t("settings.shellProfiles.applied", { rcPath: view.status.rcPath })}</p>
              <button type="button" disabled={busy} onClick={() => void run("remove")}>
                {t("settings.shellProfiles.remove")}
              </button>
            </>
          ) : view.kind === "conflict" ? (
            <>
              <p className="workload-notice notice-warn" role="alert">{t("settings.shellProfiles.conflictTitle")}</p>
              <ConflictList conflicts={view.conflicts} />
              <p className="muted">{t("settings.shellProfiles.conflictHint")}</p>
              {view.replaceable ? (
                <>
                  {/* 교체: 사용자의 줄은 그대로 두고 블록을 rc 맨 끝에 붙여 우리 정의가 이기게 한다. */}
                  <p>{t("settings.shellProfiles.replacePreview")}</p>
                  <pre className="hooks-diff">
                    {addedLines(view.status.proposedBlock ?? rcBlock(view.status.scriptPath))
                      .map((line) => `+ ${line}`)
                      .join("\n")}
                  </pre>
                  <details className="shell-profiles-script">
                    <summary>{t("settings.shellProfiles.scriptPreview")}</summary>
                    <pre className="hooks-snippet">{view.status.scriptPreview}</pre>
                  </details>
                  <p className="muted">{t("settings.shellProfiles.replaceConsent", { rcPath: view.status.rcPath })}</p>
                </>
              ) : (
                <p className="muted">{t("settings.shellProfiles.replaceBlocked")}</p>
              )}
              <div className="integration-actions">
                <button
                  type="button"
                  className="primary"
                  disabled={busy || !view.replaceable}
                  onClick={() => void run("replace")}
                >
                  {t("settings.shellProfiles.replace")}
                </button>
                {view.status.installed ? (
                  <button type="button" disabled={busy} onClick={() => void run("remove")}>
                    {t("settings.shellProfiles.remove")}
                  </button>
                ) : null}
              </div>
            </>
          ) : (
            <>
              {view.kind === "outdated" ? <p role="status">{t("settings.shellProfiles.outdated")}</p> : null}
              <p>{t("settings.shellProfiles.preview")}</p>
              <pre className="hooks-diff">
                {addedLines(view.status.proposedBlock).map((line) => `+ ${line}`).join("\n")}
              </pre>
              <details className="shell-profiles-script">
                <summary>{t("settings.shellProfiles.scriptPreview")}</summary>
                <pre className="hooks-snippet">{view.status.scriptPreview}</pre>
              </details>
              <p className="muted">{t("settings.shellProfiles.consent")}</p>
              <div className="integration-actions">
                <button
                  type="button"
                  className="primary"
                  disabled={busy}
                  // 이미 교체해 둔 설치는 다시 적용해도 사용자 정의를 계속 가려야 한다.
                  onClick={() => void run(view.status.overriding ? "replace" : "apply")}
                >
                  {view.kind === "outdated" ? t("settings.shellProfiles.reapply") : t("settings.shellProfiles.apply")}
                </button>
                {view.kind === "outdated" ? (
                  <button type="button" disabled={busy} onClick={() => void run("remove")}>
                    {t("settings.shellProfiles.remove")}
                  </button>
                ) : null}
              </div>
            </>
          )}
          {busy ? <p className="muted" role="status">{t("settings.shellProfiles.busy")}</p> : null}
          {error ? (
            <p className="setting-error" role="alert">{t("settings.shellProfiles.error", { message: error })}</p>
          ) : null}
          <button type="button" disabled={busy} onClick={() => void refresh()}>
            {t("settings.shellProfiles.refresh")}
          </button>
        </>
      )}
    </section>
  );
}

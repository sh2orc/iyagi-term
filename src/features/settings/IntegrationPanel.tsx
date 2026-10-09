/** CLI integrations. Settings-file mutations always require an explicit click. */

import { useCallback, useEffect, useId, useState } from "react";
import { isTauri, tauriIpcAdapter } from "../bridge/ipc";
import { useI18n } from "../../i18n";
import { usePreferences, ZAI_HAIKU_MODEL, ZAI_MAIN_MODELS, type ZaiMainModel } from "../../store/preferences";
import { useWorkbenchStore } from "../../store/workbenchStore";
import {
  addedLines,
  classifyHooksStatus,
  manualSnippet,
  type HookCli,
  type HooksStatus,
} from "../profiles/hooksIntegration";
import {
  applyClaudeUsage,
  getClaudeUsageStatus,
  getZaiKeyStatus,
  refreshUsageSoon,
  removeClaudeUsage,
  removeZaiKey,
  saveZaiKey,
} from "../subscriptions/client";
import type { ClaudeUsageStatus } from "../subscriptions/types";

import { AutonomyToggle } from "../workloads/AutonomyToggle";

export function IntegrationPanel(): JSX.Element {
  const { t } = useI18n();
  return (
    <>
      <AutonomyToggle kind="claude" description />
      <AutonomyToggle kind="codex" description />
      <section className="integration-block" data-setting-id="codexUsage">
        <h2>{t("settings.item.codexUsage.label")}</h2>
        <p>{t("settings.item.codexUsage.description")}</p>
        <button type="button" onClick={refreshUsageSoon}>{t("settings.usage.refresh")}</button>
      </section>
      <ClaudeUsagePanel />
      <ZaiPanel />
      <HooksPanel
        cli="claude"
        settingId="claudeHooks"
        i18nPrefix="settings.hooks"
        statusCommand="claude_hooks_status"
        applyCommand="claude_hooks_apply"
        removeCommand="claude_hooks_remove"
        fallbackCommand="iyagi-termd hook"
      />
      <HooksPanel
        cli="codex"
        settingId="codexHooks"
        i18nPrefix="settings.codexHooks"
        statusCommand="codex_hooks_status"
        applyCommand="codex_hooks_apply"
        removeCommand="codex_hooks_remove"
        fallbackCommand="iyagi-termd hook --agent codex"
      />
    </>
  );
}

function ClaudeUsagePanel(): JSX.Element {
  const { t } = useI18n();
  const available = isTauri();
  const [status, setStatus] = useState<ClaudeUsageStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!available) return;
    setBusy(true);
    setError(null);
    try { setStatus(await getClaudeUsageStatus()); }
    catch (value) { setError(errorText(value)); }
    finally { setBusy(false); }
  }, [available]);

  useEffect(() => { void refresh(); }, [refresh]);

  const run = async (action: "apply" | "remove") => {
    setBusy(true);
    setError(null);
    try {
      setStatus(await (action === "apply" ? applyClaudeUsage() : removeClaudeUsage()));
      refreshUsageSoon();
    } catch (value) { setError(errorText(value)); }
    finally { setBusy(false); }
  };

  return (
    <section className="integration-block" data-setting-id="claudeUsage">
      <h2>{t("settings.item.claudeUsage.label")}</h2>
      <p>{t("settings.item.claudeUsage.description")}</p>
      {!available ? <p className="muted">{t("settings.usage.desktopOnly")}</p> : status?.active ? (
        <>
          <p role="status">{t("settings.claudeUsage.active")}</p>
          <button type="button" disabled={busy} onClick={() => void run("remove")}>{t("settings.claudeUsage.remove")}</button>
        </>
      ) : (
        <>
          {status?.proposed ? (
            <>
              <p>{t("settings.claudeUsage.preview")}</p>
              <pre className="hooks-diff">{addedLines("claude", status.proposed).map((line) => `+ ${line}`).join("\n")}</pre>
            </>
          ) : null}
          <p className="muted">{t("settings.claudeUsage.consent")}</p>
          <button type="button" className="primary" disabled={busy || status === null} onClick={() => void run("apply")}>
            {t("settings.claudeUsage.apply")}
          </button>
        </>
      )}
      {error ? <p className="setting-error" role="alert">{t("settings.usage.error", { message: error })}</p> : null}
    </section>
  );
}

/** 주 모델 선택지의 표시 이름(제품명이라 번역하지 않는다). */
const ZAI_MODEL_LABELS: Record<ZaiMainModel, string> = {
  "glm-5.3[1m]": "GLM-5.3",
  "glm-5.3-flash[1m]": "GLM-5.3-Flash",
};

/**
 * Z.ai Coding Plan 카드: 키 등록/교체/삭제 + Claude Code 라우팅 스위치.
 *
 * - 키를 저장하면(데몬이 라우팅을 지원할 때) 라우팅을 켜고, 지우면 끈다
 *   (키 없는 라우팅은 뜻이 없다).
 * - 켜는 쪽만 잠근다: 키 상태를 아직 모르거나, 키가 없거나, 데몬이
 *   `claude_provider_routing`을 광고하지 않으면 켤 수 없고 사유를 보인다.
 *   끄는 쪽은 항상 열어 둔다 — 실행 거절 토스트가 "설정에서 라우팅을
 *   끄세요"라고 안내하므로, 이미 켜진 설정을 되돌릴 길이 막히면 안 된다.
 * - 켜져 있어도 실제 실행은 claudeProviderFor가 다시 판정한다(구 데몬이면 거절).
 * - haiku/백그라운드 슬롯은 데몬이 GLM-5.3-Flash로 고정한다 — 읽기 전용 안내.
 */
function ZaiPanel(): JSX.Element {
  const { t } = useI18n();
  const available = isTauri();
  const routeId = useId();
  const modelId = useId();
  const claudeProvider = usePreferences((s) => s.claudeProvider);
  const zaiMainModel = usePreferences((s) => s.zaiMainModel);
  const setClaudeProvider = usePreferences((s) => s.setClaudeProvider);
  const setZaiMainModel = usePreferences((s) => s.setZaiMainModel);
  const routingCapability = useWorkbenchStore((s) => s.claudeProviderRouting);
  const [configured, setConfigured] = useState(false);
  const [keyLoaded, setKeyLoaded] = useState(false);
  const [apiKey, setApiKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [editing, setEditing] = useState(false);
  const routed = claudeProvider === "zai-coding-plan";
  // 키 상태를 아직 모르거나, 키가 없거나, 데몬이 라우팅을 모르면 켤 수 없다
  // (사유는 아래 문구). 끄는 쪽은 잠그지 않는다 — 스위치의 disabled 참고.
  const routingLocked = !keyLoaded || !configured || !routingCapability;

  useEffect(() => {
    if (!available) return;
    let active = true;
    void getZaiKeyStatus().then(
      (status) => {
        if (active) {
          setConfigured(status.configured);
          setKeyLoaded(true);
        }
      },
      (value) => {
        if (active) {
          setError(errorText(value));
          setKeyLoaded(true);
        }
      },
    );
    return () => { active = false; };
  }, [available]);

  const save = async () => {
    setBusy(true);
    setError(null);
    setSaved(false);
    try {
      const status = await saveZaiKey(apiKey);
      setConfigured(status.configured);
      // 키를 등록했다는 건 GLM으로 돌리겠다는 뜻 — 라우팅을 켠다. 단 구 데몬이면
      // 켜자마자 모든 Claude 실행이 거절되므로 스위치와 같은 조건에서만 켠다.
      if (status.configured && routingCapability) setClaudeProvider("zai-coding-plan");
      setKeyLoaded(true);
      setApiKey("");
      setSaved(true);
      setEditing(false);
      refreshUsageSoon();
    } catch (value) { setError(errorText(value)); }
    finally { setBusy(false); }
  };

  const remove = async () => {
    setBusy(true);
    setError(null);
    setSaved(false);
    try {
      const status = await removeZaiKey();
      setConfigured(status.configured);
      // 키가 없으면 라우팅도 끈다(데몬이 zai_key_missing으로 거절하기 전에).
      setClaudeProvider("anthropic");
      setKeyLoaded(true);
      setApiKey("");
      setEditing(false);
      refreshUsageSoon();
    } catch (value) { setError(errorText(value)); }
    finally { setBusy(false); }
  };

  return (
    <section className="integration-block" data-setting-id="zaiCodingPlan">
      <h2>{t("settings.item.zaiCodingPlan.label")}</h2>
      <p>{t("settings.item.zaiCodingPlan.description")}</p>
      {!available ? <p className="muted">{t("settings.usage.desktopOnly")}</p> : (
        <>
          {!keyLoaded ? <p role="status">{t("settings.zai.checking")}</p> : configured && !editing ? (
            <>
              <p role="status">{saved ? t("settings.zai.saved") : t("settings.zai.configured")}</p>
              <div className="integration-actions">
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => {
                    setSaved(false);
                    setError(null);
                    setEditing(true);
                  }}
                >
                  {t("settings.zai.edit")}
                </button>
                <button type="button" disabled={busy} onClick={() => void remove()}>{t("settings.zai.remove")}</button>
              </div>
            </>
          ) : (
            <>
              <p role="status">{configured ? t("settings.zai.editing") : t("settings.zai.notConfigured")}</p>
              <div className="integration-actions">
                <input
                  className="integration-key-field"
                  type="password"
                  value={apiKey}
                  autoComplete="off"
                  spellCheck={false}
                  aria-label={t("settings.zai.apiKey")}
                  placeholder={configured ? t("settings.zai.replacePlaceholder") : t("settings.zai.placeholder")}
                  onChange={(event) => setApiKey(event.target.value)}
                />
                <button type="button" className="primary" disabled={busy || apiKey.trim().length < 8} onClick={() => void save()}>
                  {t("settings.zai.save")}
                </button>
                {configured ? (
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => {
                      setApiKey("");
                      setError(null);
                      setEditing(false);
                    }}
                  >
                    {t("settings.cancel")}
                  </button>
                ) : null}
              </div>
            </>
          )}
          <p className="muted">{t("settings.zai.security")}</p>
          <div className="zai-route">
            <label className="zai-route-switch" htmlFor={routeId}>
              {/* 끄기는 항상 허용 — 켜기만 잠근다(거절 토스트가 여기서 끄라고 안내한다). */}
              <input
                id={routeId}
                type="checkbox"
                role="switch"
                aria-checked={routed}
                checked={routed}
                disabled={busy || (routingLocked && !routed)}
                aria-describedby={`${routeId}-description`}
                onChange={(event) => setClaudeProvider(event.target.checked ? "zai-coding-plan" : "anthropic")}
              />
              <span>{t("settings.zai.route.label")}</span>
            </label>
            <p id={`${routeId}-description`} className="muted">{t("settings.zai.route.description")}</p>
            {/* 키 상태 조회가 끝나기 전에는 사유를 단정하지 않는다(위 "확인하는 중" 문구와 충돌). */}
            {!keyLoaded ? null : !configured ? (
              <p className="muted zai-route-hint">{t("settings.zai.route.needsKey")}</p>
            ) : !routingCapability ? (
              <p className="muted zai-route-hint">{t("settings.zai.route.daemonOutdated")}</p>
            ) : null}
            <label className="zai-route-model" htmlFor={modelId}>
              <span>{t("settings.zai.route.mainModel")}</span>
              <select
                id={modelId}
                className="setting-select"
                value={zaiMainModel}
                disabled={busy || routingLocked || !routed}
                onChange={(event) => setZaiMainModel(event.target.value as ZaiMainModel)}
              >
                {ZAI_MAIN_MODELS.map((model) => (
                  <option key={model} value={model}>
                    {ZAI_MODEL_LABELS[model]} ({model})
                  </option>
                ))}
              </select>
            </label>
            <p className="muted">{t("settings.zai.route.haikuNote", { model: ZAI_HAIKU_MODEL })}</p>
            <ul className="muted zai-route-notes">
              <li>{t("settings.zai.route.note.scope")}</li>
              <li>{t("settings.zai.route.note.login")}</li>
              <li>{t("settings.zai.route.note.unavailable")}</li>
            </ul>
          </div>
        </>
      )}
      {error ? <p className="setting-error" role="alert">{t("settings.usage.error", { message: error })}</p> : null}
    </section>
  );
}

/** `IntegrationPanel`이 CLI별로 두 번 인스턴스화하는 hooks 카드 설정. */
interface HooksPanelProps {
  cli: HookCli;
  /** `<section data-setting-id>` 값(검색·강조 스크롤용). */
  settingId: string;
  /** i18n 키 접두사 — "settings.hooks" | "settings.codexHooks". */
  i18nPrefix: string;
  statusCommand: string;
  applyCommand: string;
  removeCommand: string;
  /** 비-Tauri 폴백 스니펫에 보여 줄 명령(데몬 경로를 모르는 브라우저 dev). */
  fallbackCommand: string;
}

/**
 * Claude Code·Codex 공용 hooks 카드. 두 CLI는 파일 위치·관리 이벤트
 * 목록만 다르고 상태 조회 → 미리보기 → 동의 적용 → 제거 흐름은 완전히
 * 같다(§2.1) — 그래서 이벤트/명령/문구 접두사만 props로 받는 제네릭
 * 컴포넌트 하나로 둔다.
 */
function HooksPanel({
  cli,
  settingId,
  i18nPrefix,
  statusCommand,
  applyCommand,
  removeCommand,
  fallbackCommand,
}: HooksPanelProps): JSX.Element {
  const { t } = useI18n();
  const tp = (key: string, params?: Record<string, string | number>) => t(`${i18nPrefix}.${key}`, params);
  const [status, setStatus] = useState<HooksStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // 첫 상태 조회가 끝나기 전에는 "no status" 오류를 띄우지 않는다.
  const [loaded, setLoaded] = useState(false);
  const available = isTauri();

  const refresh = useCallback(async () => {
    if (!available) return;
    setBusy(true);
    setError(null);
    try { setStatus(await tauriIpcAdapter.invoke<HooksStatus>(statusCommand)); }
    catch (value) { setError(errorText(value)); }
    finally { setBusy(false); setLoaded(true); }
  }, [available, statusCommand]);

  useEffect(() => { void refresh(); }, [refresh]);

  const run = async (command: string) => {
    setBusy(true);
    setError(null);
    try { setStatus(await tauriIpcAdapter.invoke<HooksStatus>(command)); }
    catch (value) { setError(errorText(value)); }
    finally { setBusy(false); }
  };

  const view = classifyHooksStatus(status, available ? { kind: "ready" } : { kind: "not-tauri" });
  return (
    <section className="integration-block" data-setting-id={settingId}>
      <h2>{tp("title")}</h2>
      <p className="muted">{tp("description")}</p>
      {!available ? (
        <div className="hooks-fallback"><p>{tp("notTauri")}</p><pre className="hooks-snippet">{manualSnippet(cli, fallbackCommand)}</pre></div>
      ) : !loaded && error === null ? (
        <p className="muted" role="status">{tp("busy")}</p>
      ) : view.kind === "unavailable" ? (
        <p className="setting-error" role="alert">{tp("error", { message: error ?? view.message ?? "" })}</p>
      ) : (
        <>
          <p><code className="hooks-path">{view.status.path}</code>{view.status.exists ? null : <span className="muted"> {tp("fileMissing")}</span>}</p>
          <p className="muted">{tp("command")}: <code>{view.status.hookCommand}</code></p>
          {view.kind === "registered" ? (
            <>
              <p role="status">{tp("registered", { count: view.status.managedEntries, expected: view.status.expectedEntries })}</p>
              <button type="button" disabled={busy} onClick={() => void run(removeCommand)}>{tp("remove")}</button>
            </>
          ) : view.kind === "partial" ? (
            <>
              <p role="status">{tp("partial", { count: view.status.managedEntries, expected: view.status.expectedEntries })}</p>
              <p>{tp("preview")}</p>
              <pre className="hooks-diff">{addedLines(cli, view.status.proposed).map((line) => `+ ${line}`).join("\n")}</pre>
              <div className="integration-actions">
                <button type="button" className="primary" disabled={busy} onClick={() => void run(applyCommand)}>{tp("apply")}</button>
                <button type="button" disabled={busy} onClick={() => void run(removeCommand)}>{tp("remove")}</button>
              </div>
            </>
          ) : (
            <>
              <p>{tp("preview")}</p>
              <pre className="hooks-diff">{addedLines(cli, view.status.proposed).map((line) => `+ ${line}`).join("\n")}</pre>
              <p className="muted">{tp("consent")}</p>
              <button type="button" className="primary" disabled={busy} onClick={() => void run(applyCommand)}>{tp("apply")}</button>
            </>
          )}
          {busy ? <p className="muted" role="status">{tp("busy")}</p> : null}
          {error ? <p className="setting-error" role="alert">{tp("error", { message: error })}</p> : null}
          <button type="button" disabled={busy} onClick={() => void refresh()}>{tp("refresh")}</button>
        </>
      )}
    </section>
  );
}

function errorText(value: unknown): string {
  if (value instanceof Error) return value.message;
  if (typeof value === "object" && value !== null && "message" in value && typeof value.message === "string") {
    return value.message;
  }
  return String(value);
}

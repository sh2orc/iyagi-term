/** CLI integrations. Settings-file mutations always require an explicit click. */

import { useCallback, useContext, useEffect, useId, useRef, useState } from "react";
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
import { ControllerContext } from "../../app/controllerContext";
import type { GatewayPolicy } from "../../generated/GatewayPolicy";
import type { GatewayPolicyParams } from "../../generated/GatewayPolicyParams";
import type { GatewayStatus } from "../../generated/GatewayStatus";
import type { GatewayPoolStrategy } from "../../generated/GatewayPoolStrategy";
import { RpcClientError } from "../daemon/client";
import type { SessionController } from "../terminal/sessionController";
import {
  addZaiKeyPoolKey,
  listZaiKeyPool,
  removeZaiKeyPoolKey,
  type PoolKeyView,
} from "../subscriptions/client";

export function IntegrationPanel(): JSX.Element {
  const { t } = useI18n();
  // 컨트롤러는 앱 안에서만 산다(테스트 렌더에는 없다) — 없으면 게이트웨이
  // 카드를 데스크톱 전용 안내로 접는다. 상태 폴은 두 카드가 공유한다.
  const controller = useContext(ControllerContext);
  const gateway = useGatewayStatus(controller);
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
      <GatewayPanel controller={controller} gateway={gateway} />
      <KeyPoolPanel controller={controller} gateway={gateway} />
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

/** 게이트웨이 상태 폴 주기(사용량은 이벤트가 없고 폴로 읽는다). */
const GATEWAY_POLL_MS = 2000;

/** `gateway.status` 공유 폴링 결과 — 게이트웨이 카드와 키 풀 카드가 함께 읽는다. */
interface GatewayFeed {
  /** 데스크톱 + 컨트롤러가 있을 때만 폴한다(테스트 렌더에는 컨트롤러가 없다). */
  available: boolean;
  status: GatewayStatus | null;
  /** 구 데몬(gateway.* 없음) — 모르는 메서드 거절일 때만 참. */
  unsupported: boolean;
  /** 일시적 조회 실패 문구. 마지막 상태는 그대로 둔다. */
  error: string | null;
  refresh(): Promise<void>;
  /** 정책 변경 + 상태 새로고침. 그 전에 나간 폴 응답은 버린다. */
  setPolicy(params: GatewayPolicyParams): Promise<GatewayPolicy>;
}

/**
 * `gateway.status` 폴링을 한 곳에서 돌린다(연동 패널이 한 번만 부른다).
 *
 * - 순번 가드: 응답은 마지막으로 보낸 조회의 것만 반영한다. 정책 변경이
 *   시작되면 순번을 올려 진행 중인 폴을 무효로 만든다 — 늦게 도착한 옛
 *   상태가 방금 바꾼 정책을 덮어쓰지 못하게.
 * - 조회가 아직 돌고 있으면 주기 틱은 건너뛴다(느린 데몬에 요청이 쌓이지 않게).
 * - 창이 숨어 있으면 주기 틱을 건너뛰고, 다시 보이는 순간 한 번 읽는다.
 * - 언마운트 뒤 도착한 응답은 버린다.
 * - 모르는 메서드 거절([`isUnknownMethod`])만 "지원 안 함"이다. 나머지 실패는 일시적이라 보고
 *   마지막 상태를 유지한 채 오류 문구만 낸다.
 */
function useGatewayStatus(controller: SessionController | null): GatewayFeed {
  const available = isTauri() && controller !== null;
  const [status, setStatus] = useState<GatewayStatus | null>(null);
  const [unsupported, setUnsupported] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const seqRef = useRef(0);
  const inFlightRef = useRef(0);
  const activeRef = useRef(true);

  useEffect(() => {
    activeRef.current = true;
    return () => { activeRef.current = false; };
  }, []);

  const refresh = useCallback(async () => {
    if (!available || !controller) return;
    const seq = ++seqRef.current;
    inFlightRef.current += 1;
    try {
      const next = await controller.gatewayStatus();
      if (!activeRef.current || seq !== seqRef.current) return;
      setStatus(next);
      setUnsupported(false);
      setError(null);
    } catch (value) {
      if (!activeRef.current || seq !== seqRef.current) return;
      if (isUnknownMethod(value)) {
        setUnsupported(true);
        setStatus(null);
        setError(null);
      } else {
        setError(errorText(value));
      }
    } finally {
      inFlightRef.current -= 1;
    }
  }, [available, controller]);

  useEffect(() => {
    if (!available) return;
    // 창이 숨어 있는 동안(트레이·최소화·다른 앱)은 폴하지 않는다 — 보는 사람이
    // 없는 카드를 위해 2초마다 데몬을 깨울 이유가 없다. 다시 보이면 곧바로 한 번
    // 읽는다(이미 도는 조회가 있으면 그 응답을 기다린다).
    const poll = (): void => {
      if (typeof document !== "undefined" && document.hidden) return;
      if (inFlightRef.current === 0) void refresh();
    };
    poll();
    const timer = window.setInterval(poll, GATEWAY_POLL_MS);
    if (typeof document !== "undefined") document.addEventListener("visibilitychange", poll);
    return () => {
      window.clearInterval(timer);
      if (typeof document !== "undefined") document.removeEventListener("visibilitychange", poll);
    };
  }, [available, refresh]);

  const setPolicy = useCallback(async (params: GatewayPolicyParams): Promise<GatewayPolicy> => {
    if (!controller) throw new Error("gateway controller unavailable");
    // 진행 중인 폴은 바뀌기 전 정책을 읽었을 수 있다 — 응답을 버리게 순번을 올린다.
    seqRef.current += 1;
    const policy = await controller.gatewaySetPolicy(params);
    await refresh();
    return policy;
  }, [controller, refresh]);

  return { available, status, unsupported, error, refresh, setPolicy };
}

/**
 * 관측 게이트웨이 카드: Z.ai 라우팅 Claude pane의 토큰 사용량 계량 + 킬 스위치.
 *
 * - 정책(사용·차단·키 풀 전략)의 참 진실 원본은 preferences다. 데몬 정책은
 *   데몬 수명이라 컨트롤러가 붙을 때·재접속할 때 다시 적용한다
 *   (SessionController.applyGatewayPolicy) — 이 카드는 바꿀 때만 쓴다.
 * - 바꿀 때는 데몬이 받아들인 뒤에만 선호를 저장한다.
 * - 상태는 부모의 공유 폴(2초)로 읽는다 — 데몬 재시작으로 꺼진 것도 보인다.
 * - 구 데몬(gateway.* 없음)은 안내 문구로만 흡수한다.
 */
function GatewayPanel({ controller, gateway }: { controller: SessionController | null; gateway: GatewayFeed }): JSX.Element {
  const { t } = useI18n();
  const { available, status, unsupported } = gateway;
  const enabled = usePreferences((s) => s.gatewayObservability);
  const setEnabled = usePreferences((s) => s.setGatewayObservability);
  const setBlocked = usePreferences((s) => s.setGatewayBlocked);
  const routed = usePreferences((s) => s.claudeProvider) === "zai-coding-plan";
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const toggle = async (value: boolean) => {
    if (!controller) return;
    setBusy(true);
    setError(null);
    try {
      const policy = await gateway.setPolicy({ enabled: value, blocked: null, pool_strategy: null });
      setEnabled(policy.enabled);
    } catch (value_) { setError(errorText(value_)); }
    finally { setBusy(false); }
  };

  const toggleBlock = async () => {
    if (!controller || !status) return;
    setBusy(true);
    setError(null);
    try {
      const policy = await gateway.setPolicy({ enabled: null, blocked: !status.blocked, pool_strategy: null });
      setBlocked(policy.blocked);
    } catch (value) { setError(errorText(value)); }
    finally { setBusy(false); }
  };

  const switchId = useId();
  const fmt = (n: number): string => n.toLocaleString();
  const totals = status?.totals;
  // 동작 오류가 우선, 없으면 폴 오류(마지막 상태는 그대로 보인다).
  const shownError = error ?? gateway.error;

  return (
    <section className="integration-block" data-setting-id="gatewayObservability">
      <h2>{t("settings.item.gatewayObservability.label")}</h2>
      <p>{t("settings.item.gatewayObservability.description")}</p>
      {!available ? <p className="muted">{t("settings.usage.desktopOnly")}</p> : unsupported ? (
        <p className="muted">{t("settings.gateway.daemonUnsupported")}</p>
      ) : (
        <>
          <div className="zai-route">
            <label className="zai-route-switch" htmlFor={switchId}>
              <input
                id={switchId}
                type="checkbox"
                role="switch"
                aria-checked={enabled}
                checked={enabled}
                disabled={busy}
                onChange={(event) => void toggle(event.currentTarget.checked)}
              />
              <span>{t("settings.gateway.toggle.label")}</span>
            </label>
            <p className="muted">{t("settings.gateway.toggle.description")}</p>
            {!routed ? <p className="muted zai-route-hint">{t("settings.gateway.needsRouting")}</p> : null}
            {/* 저장된 선호와 데몬이 어긋남(재적용 실패 등) — 바꾸는 중에는 단정하지 않는다. */}
            {status && !busy && status.enabled !== enabled ? (
              <p className="setting-error">{t("settings.gateway.policyMismatch")}</p>
            ) : null}
          </div>
          {status ? (
            <div className="gateway-status">
              <p role="status">
                {status.listening
                  ? t("settings.gateway.status.listening", { port: status.port })
                  : t("settings.gateway.status.notListening")}
              </p>
              {status.blocked ? (
                <p className="setting-error" role="alert">{t("settings.gateway.blocked")}</p>
              ) : null}
              {totals ? (
                <p className="muted">
                  {t("settings.gateway.totals", {
                    input: fmt(totals.input_tokens),
                    output: fmt(totals.output_tokens),
                    cacheRead: fmt(totals.cache_read_tokens),
                    cacheWrite: fmt(totals.cache_write_tokens),
                    requests: fmt(totals.requests),
                    failures: fmt(totals.failures),
                  })}
                </p>
              ) : null}
              {status.sessions.length > 0 ? (
                <>
                  <p className="muted">{t("settings.gateway.sessions", { count: status.sessions.length })}</p>
                  <ul className="gateway-sessions">
                    {status.sessions.map((session) => (
                      <li key={session.workload_id}>
                        <code>{session.model ?? "—"}</code>{" "}
                        {t("settings.gateway.sessionRow", {
                          input: fmt(session.usage.input_tokens),
                          output: fmt(session.usage.output_tokens),
                          requests: session.usage.requests,
                        })}
                        {session.usage.failures > 0
                          ? ` · ${t("settings.gateway.failures", { count: session.usage.failures })}`
                          : ""}
                      </li>
                    ))}
                  </ul>
                </>
              ) : null}
              <div className="integration-actions">
                {/* 라우팅이 꺼져 있으면 막을 요청이 없다 — 차단만 잠그고 해제는 열어 둔다. */}
                <button
                  type="button"
                  disabled={busy || (!status.blocked && !status.enabled)}
                  onClick={() => void toggleBlock()}
                >
                  {status.blocked ? t("settings.gateway.unblock") : t("settings.gateway.block")}
                </button>
                <button type="button" disabled={busy} onClick={() => void gateway.refresh()}>
                  {t("settings.gateway.refresh")}
                </button>
              </div>
              {!status.enabled ? (
                <p className="muted zai-route-hint">{t("settings.gateway.blockNeedsGateway")}</p>
              ) : null}
            </div>
          ) : (
            <p role="status">{t("settings.gateway.checking")}</p>
          )}
        </>
      )}
      {shownError ? <p className="setting-error" role="alert">{t("settings.usage.error", { message: shownError })}</p> : null}
    </section>
  );
}

/** 백엔드 풀 저장소의 별명 상한(바이트, term-secrets pool). */
const KEY_POOL_LABEL_MAX_BYTES = 64;

/** 브리지가 그대로 넘기는 풀 오류 코드 → 안내 문구 키. 그 밖의 문구는 원문대로. */
const KEY_POOL_ERROR_KEYS: Record<string, string> = {
  key_pool_full: "settings.keyPool.error.full",
  api_key_invalid: "settings.keyPool.error.invalid",
  credential_store_error: "settings.keyPool.error.store",
};

function keyPoolErrorText(t: ReturnType<typeof useI18n>["t"], value: unknown): string {
  const message = errorText(value);
  const key = KEY_POOL_ERROR_KEYS[message];
  return key ? t(key) : message;
}

/**
 * API 키 풀 카드(09): Z.ai 라우팅용 키를 여러 개 등록하고 게이트웨이가
 * 세션 단위로 돌려 쓰게 한다. 비밀은 이 카드에서 브리지(앱 로컬 암호화
 * 저장소)로만 흐르고, 상태(쿨다운·격리)는 게이트웨이가 보고하는 대로
 * 읽기만 한다. 풀은 관측 게이트웨이가 켜져 수신 중일 때만 쓰인다 — 꺼져
 * 있거나 풀이 비어 있으면 기존 단일 키(Z.ai Coding Plan 카드)가 쓰인다.
 */
function KeyPoolPanel({ controller, gateway }: { controller: SessionController | null; gateway: GatewayFeed }): JSX.Element {
  const { t } = useI18n();
  const available = isTauri();
  const strategy = usePreferences((s) => s.gatewayPoolStrategy);
  const setStrategy = usePreferences((s) => s.setGatewayPoolStrategy);
  const [keys, setKeys] = useState<PoolKeyView[] | null>(null);
  const [label, setLabel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const keyId = useId();
  const { status } = gateway;
  const poolStates = status?.pool ?? [];
  // 풀은 게이트웨이를 거치는 요청에만 쓰인다 — 켜져 있고 실제로 수신 중이어야 한다.
  const poolLive = status !== null && status.enabled && status.listening;
  // 상태를 아직 모르는 동안(첫 조회 중)은 안내를 단정하지 않는다.
  const showPoolOff = !gateway.available || gateway.unsupported || (status !== null && !poolLive);
  const labelTooLong = new TextEncoder().encode(label).length > KEY_POOL_LABEL_MAX_BYTES;

  const poolError = (value: unknown): string => keyPoolErrorText(t, value);

  const refresh = useCallback(async () => {
    if (!available) return;
    try {
      setKeys(await listZaiKeyPool());
    } catch (value) {
      // 첫 조회가 실패해도 "확인하는 중"에 머물지 않게 빈 목록으로 접는다.
      setKeys((current) => current ?? []);
      setError(keyPoolErrorText(t, value));
    }
  }, [available, t]);

  useEffect(() => { void refresh(); }, [refresh]);

  const add = async () => {
    setBusy(true);
    setError(null);
    try {
      setKeys(await addZaiKeyPoolKey(label, apiKey));
      setLabel("");
      setApiKey("");
    } catch (value) { setError(poolError(value)); }
    finally { setBusy(false); }
  };

  const remove = async (id: string) => {
    setBusy(true);
    setError(null);
    try { setKeys(await removeZaiKeyPoolKey(id)); }
    catch (value) { setError(poolError(value)); }
    finally { setBusy(false); }
  };

  const changeStrategy = async (value: GatewayPoolStrategy) => {
    if (!controller) return;
    setBusy(true);
    setError(null);
    try {
      const policy = await gateway.setPolicy({ enabled: null, blocked: null, pool_strategy: value });
      setStrategy(policy.pool_strategy);
    } catch (value_) { setError(errorText(value_)); }
    finally { setBusy(false); }
  };

  const stateOf = (id: string): string => {
    const found = poolStates.find((key) => key.id === id);
    if (!found) return t("settings.keyPool.stateUnknown");
    switch (found.state) {
      case "cooling": return t("settings.keyPool.stateCooling", { seconds: Math.ceil((found.cooldown_remaining_ms ?? 0) / 1000) });
      case "quarantined": return t("settings.keyPool.stateQuarantined");
      case "unused": return t("settings.keyPool.stateUnused");
      default: return t("settings.keyPool.stateActive", { requests: found.requests });
    }
  };

  return (
    <section className="integration-block" data-setting-id="apiKeyPool">
      <h2>{t("settings.item.apiKeyPool.label")}</h2>
      <p>{t("settings.item.apiKeyPool.description")}</p>
      {!available ? <p className="muted">{t("settings.usage.desktopOnly")}</p> : (
        <>
          {showPoolOff ? <p className="muted zai-route-hint">{t("settings.keyPool.gatewayOff")}</p> : null}
          {keys === null ? (
            <p role="status">{t("settings.keyPool.checking")}</p>
          ) : keys.length === 0 ? (
            <p className="muted">{t("settings.keyPool.empty")}</p>
          ) : (
            <ul className="gateway-sessions key-pool-list">
              {keys.map((key) => (
                <li key={key.id}>
                  <code title={key.label}>{key.label}</code>
                  <span className="muted"> · {stateOf(key.id)}</span>
                  <button
                    type="button"
                    className="key-pool-remove"
                    disabled={busy}
                    onClick={() => void remove(key.id)}
                  >
                    {t("settings.keyPool.remove")}
                  </button>
                </li>
              ))}
            </ul>
          )}
          <div className="integration-actions">
            <input
              className="integration-key-field key-pool-label"
              type="text"
              value={label}
              maxLength={KEY_POOL_LABEL_MAX_BYTES}
              placeholder={t("settings.keyPool.labelPlaceholder")}
              aria-label={t("settings.keyPool.labelField")}
              aria-invalid={labelTooLong}
              onChange={(event) => setLabel(event.target.value)}
            />
            <input
              className="integration-key-field"
              type="password"
              value={apiKey}
              autoComplete="off"
              spellCheck={false}
              aria-label={t("settings.keyPool.apiKey")}
              placeholder={t("settings.keyPool.placeholder")}
              onChange={(event) => setApiKey(event.target.value)}
            />
            <button
              type="button"
              className="primary"
              disabled={busy || labelTooLong || apiKey.trim().length < 8}
              onClick={() => void add()}
            >
              {t("settings.keyPool.add")}
            </button>
          </div>
          {/* maxLength는 글자 수라 한글 별명(글자당 3바이트)은 바이트 상한을 먼저 넘는다. */}
          {labelTooLong ? (
            <p className="setting-error">{t("settings.keyPool.labelTooLong", { max: KEY_POOL_LABEL_MAX_BYTES })}</p>
          ) : null}
          <label className="zai-route-model" htmlFor={keyId}>
            <span>{t("settings.keyPool.strategy")}</span>
            <select
              id={keyId}
              className="setting-select"
              value={strategy}
              disabled={busy || !controller || !poolLive}
              onChange={(event) => void changeStrategy(event.target.value as GatewayPoolStrategy)}
            >
              <option value="round_robin">{t("settings.keyPool.strategy.roundRobin")}</option>
              <option value="fill_first">{t("settings.keyPool.strategy.fillFirst")}</option>
            </select>
          </label>
          <p className="muted">{t("settings.keyPool.strategy.note")}</p>
          <p className="muted">{t("settings.keyPool.security")}</p>
        </>
      )}
      {error ? <p className="setting-error" role="alert">{t("settings.usage.error", { message: error })}</p> : null}
    </section>
  );
}

/**
 * 구 데몬이 모르는 메서드를 거절한 것인가. 데몬 디스패처는
 * `INVALID_ARGUMENT` + `unknown method "…"`로 답하고(dispatch.rs), 브리지가
 * 붙인 `METHOD_NOT_FOUND`도 받는다 — 후자는 생성된 ErrorCode 합집합에 없어
 * 문자열로 비교한다.
 */
function isUnknownMethod(value: unknown): boolean {
  if (!(value instanceof RpcClientError)) return false;
  if ((value.code as string) === "METHOD_NOT_FOUND") return true;
  return value.code === "INVALID_ARGUMENT" && value.message.startsWith("unknown method");
}

function errorText(value: unknown): string {
  if (value instanceof Error) return value.message;
  if (typeof value === "object" && value !== null && "message" in value && typeof value.message === "string") {
    return value.message;
  }
  return String(value);
}

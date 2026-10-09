/**
 * 관리 실행 대화상자 (04 §1: 프로필 선택 → cwd/argv/자원 정책 확인 →
 * 실행 → 큐 또는 터미널 연결).
 *
 * - 프로필 목록은 profileStore(localStorage 지속화)에서 온다.
 * - argv는 프로필 고정 prefix(interpreter prefix 포함) + 추가 인수로
 *   편집하며 256개/64 KiB 예산을 클라이언트에서 검증한다.
 * - 자원 정책은 enforcement 세그먼트 + snapshot.capabilities의 실제
 *   지원 현실을 보여 준다. require + 미지원/권한 필요 상한 요청은 실행
 *   전에 차단하고 CAPABILITY_UNAVAILABLE 설명을 낸다(daemon도 같은
 *   코드로 거부한다).
 * - 실행 결과: session이 있으면(즉시 입장 허용) pane에 writer로 연결,
 *   없으면(대기) queue drawer만 열고 placeholder pane은 만들지 않는다.
 * - 일반 실행(direct shell) 안내 카드: 관측은 session 단위이며 관리
 *   실행은 전용 PTY/OS 그룹 attribution임을 구분해 안내한다(04 §5).
 *
 * Modals.tsx의 기존 폼을 대체하는 drop-in이다. client/probe는 props가
 * 우선, 없으면 setManagedRunDeps로 wiring된 값을 쓴다.
 */

import { useContext, useEffect, useMemo, useState } from "react";
import { useI18n } from "../../i18n";
import type { DaemonClient, LaunchOutcome } from "../daemon/client";
import type { Capabilities } from "../../generated/Capabilities";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { ControllerContext } from "../../app/controllerContext";
import { detectPlatform, type Platform } from "../terminal/shortcuts";
import type { SystemProbe } from "../profiles/probeTypes";
import { useProfilesStore } from "../profiles/profileStore";
import { type LaunchProfile } from "../profiles/types";
import { argvBudgetMessage, parseArgvText, validateArgvBudget, validateProfileForLaunch, type PlatformFlag } from "../profiles/validation";
import { draftToNumericPolicy, fromProfilePolicy, type PolicyDraft } from "../profiles/policyDraft";
import { PolicyEditor } from "../profiles/PolicyEditor";
import { capabilityGate } from "../profiles/capabilityUi";
import { CompatibilityMatrix } from "../profiles/CompatibilityMatrix";
import { ProfileForm } from "../profiles/ProfileForm";
import { getManagedRunDeps } from "./managedRunDeps";
import { composeLaunchRequest } from "./launchComposer";
import { CLAUDE_PROVIDER_DAEMON_OUTDATED_KEY, claudeProviderFor } from "./claudeProvider";
import { formatLaunchError } from "./managedRunErrors";
import { formatGiB } from "../monitor/format";
import { usePreferences } from "../../store/preferences";
import { autonomyEnabled, effectiveLaunchCommand } from "./autonomy";
import { AutonomyToggle } from "./AutonomyToggle";
import "../profiles/profiles.css";

export interface ManagedRunDialogProps {
  onClose?: () => void;
  onOpenProfiles?: () => void;
  onOpenCompatibility?: () => void;
  /** 없으면 managedRunDeps seam 사용. 둘 다 없으면 실행 버튼 비활성화. */
  client?: DaemonClient | null;
  probe?: SystemProbe | null;
  platform?: Platform;
  /** snapshot capabilities 초기값(이미 알고 있을 때). 없으면 mount 후 systemSnapshot으로 조회. */
  capabilities?: Capabilities | null;
  /**
   * 프로필 목록 override(제어형 마운트/테스트). 없으면 profileStore에서 읽는다.
   * 참고: zustand v4는 SSR(renderToString)에서 생성 시점 state만 보여 주므로
   * 서버 렌더 시험은 이 prop으로 상태를 주입한다.
   */
  profiles?: LaunchProfile[] | null;
  /** 실행 결과 라우팅 override(테스트/커스텀 wiring). 기본: pane 연결 또는 큐. */
  onLaunched?: (outcome: LaunchOutcome) => void;
}

function detectPlatformSafe(): Platform {
  try {
    if (typeof navigator !== "undefined") {
      return detectPlatform(navigator.userAgent, navigator.platform);
    }
  } catch {
    // navigator 없는 환경(ssr 테스트) — Windows 규칙으로 안전하게.
  }
  return "windows";
}

function makeUuid(): () => string {
  const cryptoApi = (globalThis as { crypto?: { randomUUID?: () => string } }).crypto;
  if (cryptoApi && typeof cryptoApi.randomUUID === "function") {
    return () => cryptoApi.randomUUID!();
  }
  let n = 0;
  return () => `req-${Date.now().toString(36)}-${++n}`;
}

const UUID = makeUuid();

export function ManagedRunDialog(props: ManagedRunDialogProps): JSX.Element {
  const { t } = useI18n();
  const deps = getManagedRunDeps();
  const client = props.client !== undefined ? props.client : (deps?.client ?? null);
  const probe = props.probe !== undefined ? props.probe : (deps?.probe ?? null);
  const platform: Platform = props.platform ?? detectPlatformSafe();
  const platformFlag: PlatformFlag = platform === "windows" ? "windows" : "other";
  const controller = useContext(ControllerContext);

  const closeModal = useWorkbenchStore((s) => s.closeModal);
  const close = props.onClose ?? closeModal;
  const storeProfiles = useProfilesStore((s) => s.profiles);
  const profiles = props.profiles ?? storeProfiles;
  const [profileId, setProfileId] = useState<string>(profiles[0]?.id ?? "");
  const storeProfile = useProfilesStore((s) => s.profileById(profileId));
  const profile = props.profiles ? (props.profiles.find((p) => p.id === profileId) ?? null) : storeProfile;
  const defaultCwd = useWorkbenchStore((s) => {
    const pane = s.focusedLeafId ? s.panes[s.focusedLeafId] : null;
    return pane?.cwd ?? null;
  });

  const [cwd, setCwd] = useState("");
  const [argvText, setArgvText] = useState("");
  const [priority, setPriority] = useState(1);
  const [policyDraft, setPolicyDraft] = useState<PolicyDraft>(() =>
    fromProfilePolicy(profiles[0]?.policy ?? null),
  );
  const [capabilities, setCapabilities] = useState<Capabilities | null>(props.capabilities ?? null);
  const [capabilitiesError, setCapabilitiesError] = useState<string | null>(null);
  const [launchError, setLaunchError] = useState<string | null>(null);
  const [launching, setLaunching] = useState(false);

  useEffect(() => {
    if (!client) return;
    let alive = true;
    client
      .systemSnapshot()
      .then((snapshot) => {
        if (alive) setCapabilities(snapshot.capabilities);
      })
      .catch((error: unknown) => {
        if (alive) {
          setCapabilities(null);
          setCapabilitiesError(error instanceof Error ? error.message : t("managed.errorCapabilities"));
        }
      });
    return () => {
      alive = false;
    };
  }, [client]);

  const switchProfile = (id: string): void => {
    const source = props.profiles ?? useProfilesStore.getState().profiles;
    const next = source.find((p) => p.id === id) ?? null;
    setProfileId(id);
    if (next) setPolicyDraft(fromProfilePolicy(next.policy));
  };

  const fullAutonomy = usePreferences((s) => profile ? autonomyEnabled(profile.descriptor.kind, s) : false);
  // Claude 프로필만 제공자 라우팅(Z.ai) 대상. 데몬 지원은 방금 조회한 capabilities가
  // 우선이고, 아직 없으면 snapshot 미러(workbenchStore)를 쓴다. 구 데몬이면
  // 실행 버튼을 잠근다 — 조용히 Anthropic으로 떨어지지 않는다.
  const storeRouting = useWorkbenchStore((s) => s.claudeProviderRouting);
  const claudeProviderPref = usePreferences((s) => s.claudeProvider);
  const zaiMainModel = usePreferences((s) => s.zaiMainModel);
  const routingCapability = capabilities ? capabilities.claude_provider_routing === true : storeRouting;
  const routing = useMemo(
    () =>
      profile
        ? claudeProviderFor(
            profile.descriptor.kind,
            { claudeProvider: claudeProviderPref, zaiMainModel },
            routingCapability,
          )
        : null,
    [profile, claudeProviderPref, zaiMainModel, routingCapability],
  );
  const routingRefused = routing !== null && !routing.ok;
  const claudeProvider = routing?.ok ? routing.provider : null;
  const extraArgs = useMemo(() => parseArgvText(argvText), [argvText]);
  const effective = profile ? effectiveLaunchCommand(profile, extraArgs, fullAutonomy) : null;
  const budget = effective ? validateArgvBudget(effective.argv) : null;
  const programCheck = profile ? validateProfileForLaunch(profile, platformFlag) : null;
  // 표시와 조립이 같은 값을 쓴다: 빈 입력은 프로필 기본 → 포커스 pane cwd 순.
  const cwdValue = cwd !== "" ? cwd : (profile?.cwd || defaultCwd || "");

  const numericPolicy = useMemo(() => draftToNumericPolicy(policyDraft), [policyDraft]);
  const compose = useMemo(
    () =>
      profile
        ? composeLaunchRequest(
            profile,
            { profileId: profile.id, cwd: cwdValue, extraArgv: extraArgs, fullAutonomy, claudeProvider, priority, policy: numericPolicy, cols: 80, rows: 24 },
            { uuid: UUID, platform: platformFlag },
          )
        : null,
    [profile, cwdValue, extraArgs, fullAutonomy, claudeProvider, priority, numericPolicy, platformFlag],
  );

  const gate = capabilities
    ? capabilityGate(numericPolicy, capabilities)
    : { blocked: false, message: null as string | null, blockedLimits: [] };
  const composeErrors = compose && !compose.ok ? compose.errors : [];
  const programInvalid = programCheck !== null && !programCheck.ok;
  const canLaunch =
    !!client && !!compose?.ok && !gate.blocked && !launching && !programInvalid && budget?.ok !== false && !routingRefused;

  /**
   * 이 컴퓨터가 받을 수 없는 크기의 예약은 데몬이 거절하지 않고 받을 수 있는 최대로
   * 줄여 받는다(effective_policy) — 줄었으면 그 값을 알린다.
   */
  const fittedNotice = (outcome: LaunchOutcome): string | null => {
    const asked = Number(compose?.ok ? compose.request.policy.reservation_bytes : NaN);
    const got = Number(outcome.effective_policy?.reservation_bytes);
    return Number.isFinite(asked) && Number.isFinite(got) && got < asked
      ? t("managed.reservationFitted", { size: formatGiB(got) })
      : null;
  };

  const routeOutcome = (outcome: LaunchOutcome): void => {
    const store = useWorkbenchStore.getState();
    if (props.onLaunched) {
      props.onLaunched(outcome);
      return;
    }
    const notify = (message: string): void => {
      if (controller) controller.toast(message);
      else store.setToast(message);
    };
    const fitted = fittedNotice(outcome);
    if (outcome.session_id) {
      // 즉시 입장 허용 — 전용 PTY에 writer로 연결(04 §1).
      if (controller) {
        controller.attachSessionToNewPane(outcome.session_id, outcome.workload_id);
      } else {
        store.toggleQueueDrawer(true);
      }
      if (fitted) notify(fitted);
      return;
    }
    // 대기: placeholder pane/PTY를 미리 만들지 않는다(04 §1, U14).
    store.toggleQueueDrawer(true);
    notify(fitted ? `${t("managed.queuedToast")} · ${fitted}` : t("managed.queuedToast"));
  };

  const submit = async (): Promise<void> => {
    setLaunchError(null);
    if (!client || !compose || !compose.ok || gate.blocked || routingRefused) return;
    setLaunching(true);
    try {
      const outcome = await client.workloadLaunch(compose.request);
      close();
      routeOutcome(outcome);
    } catch (error) {
      setLaunchError(formatLaunchError(error));
    } finally {
      setLaunching(false);
    }
  };

  const fixedPrefix = profile
    ? effectiveLaunchCommand(profile, [], fullAutonomy).argv
    : [];

  return (
    <form
      className="managed-form managed-run"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <h2>{t("managed.title")}</h2>

      {!client ? <p className="workload-notice notice-warn">{t("managed.noDaemon")}</p> : null}

      <label>
        {t("managed.profile")}
        <select value={profileId} onChange={(e) => switchProfile(e.target.value)} aria-label={t("managed.profileAria")}>
          {profiles.map((p) => (
            <option key={p.id} value={p.id}>
              {p.label} — {p.descriptor.program || t("managed.noProgram")}
            </option>
          ))}
          {profiles.length === 0 ? <option value="">{t("managed.noProfiles")}</option> : null}
        </select>
      </label>

      {programCheck && !programCheck.ok ? (
        <div className="workload-notice notice-warn" role="alert">
          <pre>{programCheck.message}</pre>
        </div>
      ) : null}

      {routingRefused ? (
        <div className="workload-notice notice-warn" role="alert">
          <pre>{t(CLAUDE_PROVIDER_DAEMON_OUTDATED_KEY)}</pre>
        </div>
      ) : claudeProvider ? (
        <p className="muted">{t("managed.claudeProviderRouted", { model: claudeProvider.main_model })}</p>
      ) : null}

      <details className="direct-shell-guide">
        <summary>{t("managed.guide.title")}</summary>
        <p className="muted">{t("managed.guide.body")}</p>
      </details>

      <label>
        {t("managed.cwdLabel")}
        <input
          value={cwdValue}
          onChange={(e) => setCwd(e.target.value)}
          placeholder={platformFlag === "windows" ? "D:\\project\\iyagi" : "/home/user/project"}
          aria-label={t("managed.cwdAria")}
        />
      </label>

      {profile ? <AutonomyToggle kind={profile.descriptor.kind} description disabled={launching} /> : null}

      <fieldset className="argv-editor">
        <legend>{t("managed.argvLegend")}</legend>
        {fixedPrefix.length > 0 ? (
          <div className="argv-fixed">
            <span className="muted">{t("managed.fixedPrefix")}</span>
            <pre>{fixedPrefix.join("\n")}</pre>
          </div>
        ) : null}
        <label>
          {t("managed.extraArgs")}
          <textarea value={argvText} onChange={(e) => setArgvText(e.target.value)} rows={2} aria-label={t("managed.extraArgsAria")} />
        </label>
        {budget ? (
          <p className="muted" aria-live="polite">
            {t("managed.argvBudget", {
              count: budget.count,
              maxCount: budget.maxCount,
              bytes: budget.bytes,
              maxBytes: budget.maxBytes,
            })}
            {budget.ok ? null : ` — ${argvBudgetMessage(budget)}`}
          </p>
        ) : null}
        {effective ? (
          <p className="muted">
            {t("managed.effectiveCommand")} <code>{[effective.program, ...effective.argv].join(" ")}</code>
          </p>
        ) : null}
      </fieldset>

      <PolicyEditor
        draft={policyDraft}
        onChange={(p) => setPolicyDraft((d) => ({ ...d, ...p }))}
        capabilities={capabilities}
        idPrefix="managed-run"
      />
      {capabilitiesError ? <p className="muted">{capabilitiesError}</p> : null}

      <label>
        {t("managed.priority")}
        <select value={priority} onChange={(e) => setPriority(Number(e.target.value))} aria-label={t("managed.priority")}>
          <option value={0}>{t("managed.priorityHigh")}</option>
          <option value={1}>{t("managed.priorityNormal")}</option>
          <option value={2}>{t("managed.priorityLow")}</option>
        </select>
      </label>

      {gate.blocked && gate.message ? (
        <div className="workload-notice notice-warn" role="alert">
          <pre>{gate.message}</pre>
        </div>
      ) : null}
      {composeErrors.length > 0 ? (
        <div className="workload-notice notice-warn" role="alert">
          <pre>{composeErrors.join("\n")}</pre>
        </div>
      ) : null}
      {launchError ? (
        <div className="workload-notice notice-warn" role="alert">
          <pre>{launchError}</pre>
        </div>
      ) : null}

      <p className="muted">{t("managed.queueNote")}</p>

      {props.onOpenProfiles ? <button type="button" onClick={props.onOpenProfiles}>{t("managed.editProfile")}</button> : <details className="profile-edit">
        <summary>{t("managed.editProfile")}</summary>
        <ProfileForm
          key={profile?.id ?? "new"}
          profile={profile}
          probe={probe}
          platform={platformFlag}
          capabilities={capabilities}
          onSaved={() => undefined}
        />
      </details>}

      {props.onOpenCompatibility ? <button type="button" onClick={props.onOpenCompatibility}>{t("managed.compatMatrix")}</button> : <details className="compat-section">
        <summary>{t("managed.compatMatrix")}</summary>
        <CompatibilityMatrix capabilities={capabilities} />
      </details>}

      <div className="modal-actions">
        <button type="button" onClick={close}>
          {t("managed.cancel")}
        </button>
        <button
          type="submit"
          disabled={!canLaunch}
          title={gate.blocked ? gate.message ?? undefined : composeErrors[0]}
        >
          {t("managed.submit")}
        </button>
      </div>
    </form>
  );
}

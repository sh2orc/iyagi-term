/**
 * CLI 프로필 편집 폼 (04 §5, 02 §3 마지막 문단, 01 §7).
 *
 * 폼을 열면 SystemProbe.listClis()로 codex/claude/opencode 후보를
 * 제안한다(실행 파일 경로 · symlink target · 설치 형태 표시). 사용자는
 * 후보를 고르거나 절대 경로를 직접 입력한다. 버전은 "검증된 version
 * query"만 2초 timeout으로 조회하고 결과가 없으면 "감지 안 됨"을
 * 보여 준다 — 절대 값을 만들지 않는다.
 *
 * Windows에서 .cmd/.bat/.ps1 shim은 관리 직접 실행이 금지되므로
 * (a) 일반 셸 실행 안내와 (b) interpreter 실행 파일 + argv prefix
 * 형태 등록 안내를 제공하고 저장 시·실행 전에 같은 규칙으로 검증한다.
 *
 * env는 값 또는 OS 보안 저장소 참조(secretRef)만 담는다 — raw 비밀
 * 값을 입력받는 필드는 존재하지 않는다(01 §7).
 */

import { useEffect, useMemo, useState } from "react";
import type { Capabilities } from "../../generated/Capabilities";
import { useI18n } from "../../i18n";
import type { SystemProbe } from "./probeTypes";
import {
  DISCOVERY_IDLE,
  VERSION_PROBE_IDLE,
  candidateSummary,
  candidatesForKind,
  discoverClis,
  interpreterSuggestionFor,
  queryVersionWithTimeout,
  versionProbeText,
  type DiscoveryState,
  type VersionProbeState,
} from "./probe";
import type { CliKind, LaunchProfile, ProfileEnvEntry, ProfileInterpreter } from "./types";
import { CLI_KIND_LABELS, defaultCliCapabilities, effectiveCommand } from "./types";
import {
  normalizeProgramPath,
  parseArgvText,
  validateCwd,
  validateEnvEntries,
  validateProgram,
  type PlatformFlag,
} from "./validation";
import { defaultPolicyDraft, draftToProfilePolicy, fromProfilePolicy, type PolicyDraft } from "./policyDraft";
import { PolicyEditor } from "./PolicyEditor";
import { useProfilesStore } from "./profileStore";

export interface ProfileFormProps {
  /** null이면 새 프로필. */
  profile: LaunchProfile | null;
  probe?: SystemProbe | null;
  platform?: PlatformFlag;
  capabilities?: Capabilities | null;
  onSaved?: (id: string) => void;
  onCancel?: () => void;
}

interface ProfileFormDraft {
  label: string;
  kind: CliKind;
  program: string;
  argvPrefixText: string;
  cwd: string;
  notes: string;
  env: ProfileEnvEntry[];
  interpreterEnabled: boolean;
  interpreterExecutable: string;
  interpreterPrefixText: string;
  policy: PolicyDraft;
}

function initialDraft(profile: LaunchProfile | null): ProfileFormDraft {
  return {
    label: profile?.label ?? "",
    kind: profile?.descriptor.kind ?? "custom",
    program: profile?.descriptor.program ?? "",
    argvPrefixText: (profile?.descriptor.argv_prefix ?? []).join("\n"),
    cwd: profile?.cwd ?? "",
    notes: profile?.notes ?? "",
    env: profile ? profile.env.map((e) => ({ ...e })) : [],
    interpreterEnabled: profile?.interpreter !== null && profile?.interpreter !== undefined,
    interpreterExecutable: profile?.interpreter?.executable ?? "",
    interpreterPrefixText: (profile?.interpreter?.scriptArgvPrefix ?? []).join("\n"),
    policy: profile ? fromProfilePolicy(profile.policy) : defaultPolicyDraft(),
  };
}

export function ProfileForm(props: ProfileFormProps): JSX.Element {
  const { t } = useI18n();
  const platform = props.platform ?? "windows";
  const addProfile = useProfilesStore((s) => s.addProfile);
  const updateProfile = useProfilesStore((s) => s.updateProfile);

  const [draft, setDraft] = useState<ProfileFormDraft>(() => initialDraft(props.profile));
  const [versionProbe, setVersionProbe] = useState<VersionProbeState>(VERSION_PROBE_IDLE);
  const [discovery, setDiscovery] = useState<DiscoveryState>(DISCOVERY_IDLE);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  // 폼을 열면 CLI 후보를 탐지한다(04 §5). 실패해도 폼은 직접 입력으로 동작.
  useEffect(() => {
    let alive = true;
    setDiscovery({ status: "pending", candidates: [], error: null });
    void discoverClis(props.probe ?? null).then((state) => {
      if (alive) setDiscovery(state);
    });
    return () => {
      alive = false;
    };
  }, [props.probe]);

  const patch = (p: Partial<ProfileFormDraft>): void => setDraft((d) => ({ ...d, ...p }));

  const suggestions = useMemo(
    () => (discovery.status === "ready" ? candidatesForKind(discovery.candidates, draft.kind) : []),
    [discovery, draft.kind],
  );

  const interpreter: ProfileInterpreter | null = draft.interpreterEnabled
    ? {
        executable: draft.interpreterExecutable.trim(),
        scriptArgvPrefix: parseArgvText(draft.interpreterPrefixText),
      }
    : null;

  // "그대로 실행되는 형태" 미리보기 — normalizeProgramPath로 표시 경로를 고정.
  const preview = effectiveCommand(
    {
      descriptor: {
        kind: draft.kind,
        program: normalizeProgramPath(draft.program, platform),
        argv_prefix: parseArgvText(draft.argvPrefixText),
        detected_version: null,
        transport: "pty",
        capabilities: defaultCliCapabilities(),
      },
      interpreter,
    },
    [],
  );

  const runVersionQuery = async (): Promise<void> => {
    if (!props.probe || !draft.program.trim() || versionProbe.status === "pending") return;
    setVersionProbe({ status: "pending", value: null });
    const value = await queryVersionWithTimeout(props.probe, draft.program.trim());
    setVersionProbe(value === null ? { status: "unavailable", value: null } : { status: "value", value });
  };

  const save = (): void => {
    setError(null);
    const program = normalizeProgramPath(draft.program, platform);
    const programCheck = validateProgram({ program, platform, interpreter });
    if (!programCheck.ok) {
      setError(programCheck.message);
      return;
    }
    const envCheck = validateEnvEntries(draft.env);
    if (!envCheck.ok) {
      setError(envCheck.issues.map((i) => i.message).join("\n"));
      return;
    }
    if (draft.cwd.trim()) {
      const cwdCheck = validateCwd(draft.cwd.trim(), platform);
      if (!cwdCheck.ok) {
        setError(cwdCheck.message);
        return;
      }
    }
    const nextProfile = {
      label: draft.label.trim(),
      descriptor: {
        kind: draft.kind,
        program,
        argv_prefix: parseArgvText(draft.argvPrefixText),
        detected_version:
          versionProbe.status === "value" ? versionProbe.value : (props.profile?.descriptor.detected_version ?? null),
        transport: "pty" as const,
        capabilities: defaultCliCapabilities(),
      },
      cwd: draft.cwd.trim(),
      policy: draftToProfilePolicy(draft.policy),
      env: draft.env,
      notes: draft.notes,
      interpreter,
    };
    const profile = nextProfile;
    const result = props.profile
      ? updateProfile(props.profile.id, profile)
      : addProfile(profile);
    if (!result.ok) {
      setError(result.error);
      return;
    }
    setSaved(true);
    props.onSaved?.(props.profile ? props.profile.id : (result.id ?? ""));
  };

  return (
    <form
      className="profile-form"
      onSubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <h3>{props.profile ? t("profile.form.editTitle", { label: props.profile.label }) : t("profile.form.newTitle")}</h3>

      <label>
        {t("profile.form.name")}
        <input value={draft.label} onChange={(e) => patch({ label: e.target.value })} required aria-label={t("profile.form.nameAria")} />
      </label>
      <label>
        {t("profile.form.kind")}
        <select value={draft.kind} onChange={(e) => patch({ kind: e.target.value as CliKind })} aria-label={t("profile.form.kindAria")}>
          {(Object.keys(CLI_KIND_LABELS) as CliKind[]).map((kind) => (
            <option key={kind} value={kind}>
              {CLI_KIND_LABELS[kind]}
            </option>
          ))}
        </select>
      </label>

      <fieldset className="discovery-box">
        <legend>{t("profile.form.discovery")}</legend>
        {discovery.status === "pending" ? <p className="muted">{t("profile.form.discovering")}</p> : null}
        {discovery.status === "failed" ? <p className="workload-notice">{discovery.error ?? t("profile.form.discoveryFailed")}</p> : null}
        {discovery.status === "ready" && suggestions.length === 0 ? (
          <p className="muted">{t("profile.form.noCandidates")}</p>
        ) : null}
        {suggestions.map((candidate) => {
          const interp = interpreterSuggestionFor(candidate, platform);
          return (
            <div key={candidate.program} className="cli-candidate">
              <label>
                <input
                  type="radio"
                  name="cli-candidate"
                  value={candidate.program}
                  checked={draft.program === candidate.program}
                  onChange={() => patch({ program: candidate.program })}
                />
                {candidateSummary(candidate)}
              </label>
              {interp ? (
                <div className="workload-notice">
                  {interp.reason}
                  <button
                    type="button"
                    onClick={() =>
                      patch({
                        interpreterEnabled: true,
                        interpreterExecutable: "",
                        interpreterPrefixText: interp.scriptArgvPrefix.join("\n"),
                      })
                    }
                  >
                    {t("profile.form.fillInterpreter", { hint: interp.executableHint })}
                  </button>
                </div>
              ) : null}
              {!interp &&
              candidate.resolvedTarget &&
              platform === "windows" &&
              /\.(cmd|bat|ps1)$/i.test(candidate.program) ? (
                <div className="workload-notice">
                  {t("profile.form.shimTargetNotice", { target: candidate.resolvedTarget })}
                  <button type="button" onClick={() => patch({ program: candidate.resolvedTarget ?? "" })}>
                    {t("profile.form.registerTarget")}
                  </button>
                </div>
              ) : null}
            </div>
          );
        })}
      </fieldset>

      <label>
        {t("profile.form.program")}
        <input
          value={draft.program}
          onChange={(e) => patch({ program: e.target.value })}
          placeholder={platform === "windows" ? "C:\\…\\program.exe" : "/usr/local/bin/program"}
          aria-label={t("profile.form.programAria")}
        />
      </label>
      <p className="muted" aria-label={t("profile.form.previewAria")}>
        {t("profile.form.preview")} <code>{[preview.program, ...preview.argv].join(" ") || "—"}</code>
      </p>

      <div className="version-row">
        <button
          type="button"
          onClick={() => void runVersionQuery()}
          disabled={!props.probe || !draft.program.trim() || versionProbe.status === "pending"}
        >
          {t("profile.form.versionQuery")}
        </button>
        <span aria-live="polite">
          {versionProbe.status === "pending" ? t("profile.version.pending") : versionProbeText(versionProbe)}
        </span>
        {!props.probe ? <span className="muted">{t("profile.form.noProbe")}</span> : null}
      </div>

      <label>
        {t("profile.form.argvPrefix")}
        <textarea
          value={draft.argvPrefixText}
          onChange={(e) => patch({ argvPrefixText: e.target.value })}
          rows={2}
          aria-label={t("profile.form.argvPrefixAria")}
        />
      </label>

      {platform === "windows" ? (
        <fieldset className="interpreter-box">
          <legend>{t("profile.form.interpreterLegend")}</legend>
          <label>
            <input
              type="checkbox"
              checked={draft.interpreterEnabled}
              onChange={(e) => patch({ interpreterEnabled: e.target.checked })}
            />
            {t("profile.form.interpreterToggle")}
          </label>
          {draft.interpreterEnabled ? (
            <>
              <label>
                {t("profile.form.interpreterExe")}
                <input
                  value={draft.interpreterExecutable}
                  onChange={(e) => patch({ interpreterExecutable: e.target.value })}
                  placeholder="C:\\Program Files\\nodejs\\node.exe"
                />
              </label>
              <label>
                {t("profile.form.scriptPrefix")}
                <textarea
                  value={draft.interpreterPrefixText}
                  onChange={(e) => patch({ interpreterPrefixText: e.target.value })}
                  rows={2}
                  placeholder="C:\\Users\\me\\AppData\\Roaming\\npm\\node_modules\\…\\cli.js"
                />
              </label>
              <p className="muted">{t("profile.form.interpreterNote")}</p>
            </>
          ) : null}
        </fieldset>
      ) : null}

      <label>
        {t("profile.form.cwd")}
        <input value={draft.cwd} onChange={(e) => patch({ cwd: e.target.value })} aria-label={t("profile.form.cwdAria")} />
      </label>

      <fieldset className="env-editor">
        <legend>{t("profile.form.envLegend")}</legend>
        {draft.env.length === 0 ? <p className="muted">{t("profile.form.envEmpty")}</p> : null}
        {draft.env.map((entry, index) => {
          const secretMode = entry.secretRef !== null;
          return (
            <div key={index} className="env-row">
              <input
                value={entry.key}
                placeholder={t("profile.form.envName")}
                aria-label={t("profile.form.envNameAria", { index: index + 1 })}
                onChange={(e) => {
                  const env = [...draft.env];
                  env[index] = { ...entry, key: e.target.value };
                  patch({ env });
                }}
              />
              {secretMode ? (
                <input
                  value={entry.secretRef ?? ""}
                  placeholder={t("profile.form.envSecretPlaceholder")}
                  aria-label={t("profile.form.envSecretAria", { index: index + 1 })}
                  onChange={(e) => {
                    const env = [...draft.env];
                    env[index] = { key: entry.key, value: null, secretRef: e.target.value };
                    patch({ env });
                  }}
                />
              ) : (
                <input
                  value={entry.value ?? ""}
                  placeholder={t("profile.form.envValue")}
                  aria-label={t("profile.form.envValueAria", { index: index + 1 })}
                  onChange={(e) => {
                    const env = [...draft.env];
                    env[index] = { key: entry.key, value: e.target.value, secretRef: null };
                    patch({ env });
                  }}
                />
              )}
              <label className="muted">
                <input
                  type="checkbox"
                  checked={secretMode}
                  onChange={(e) => {
                    const env = [...draft.env];
                    env[index] = e.target.checked
                      ? { key: entry.key, value: null, secretRef: "" }
                      : { key: entry.key, value: "", secretRef: null };
                    patch({ env });
                  }}
                />
                {t("profile.form.envSecretToggle")}
              </label>
              <button
                type="button"
                aria-label={t("profile.form.envRemoveAria", { index: index + 1 })}
                onClick={() => patch({ env: draft.env.filter((_, i) => i !== index) })}
              >
                {t("profile.delete")}
              </button>
              {secretMode ? (
                <p className="muted env-secret-note">
                  {t("profile.form.envSecretNote")}
                </p>
              ) : null}
            </div>
          );
        })}
        <button
          type="button"
          onClick={() => patch({ env: [...draft.env, { key: "", value: "", secretRef: null }] })}
        >
          {t("profile.form.addEnv")}
        </button>
      </fieldset>

      <PolicyEditor
        draft={draft.policy}
        onChange={(p) => patch({ policy: { ...draft.policy, ...p } })}
        capabilities={props.capabilities ?? null}
        idPrefix={`profile-${props.profile?.id ?? "new"}`}
      />

      <label>
        {t("profile.form.notes")}
        <textarea value={draft.notes} onChange={(e) => patch({ notes: e.target.value })} rows={2} />
      </label>

      {error ? (
        <div className="workload-notice notice-warn" role="alert">
          <pre>{error}</pre>
        </div>
      ) : null}
      {saved ? <p role="status">{t("profile.form.saved")}</p> : null}

      <div className="modal-actions">
        {props.onCancel ? (
          <button type="button" onClick={props.onCancel}>
            {t("profile.form.cancel")}
          </button>
        ) : null}
        <button type="submit" disabled={versionProbe.status === "pending"}>
          {t("profile.form.save")}
        </button>
      </div>
    </form>
  );
}

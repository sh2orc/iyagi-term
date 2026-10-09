/**
 * 프로필 설정 섹션 (I11): 프로필 목록/편집/삭제/신규 + JSON 내보내기·
 * 가져오기(version 검증) + 호환성 매트릭스.
 *
 * Workbench의 설정 영역에 마운트한다(wiring은 App 에이전트):
 *   <ProfilesPanel probe={probe} capabilities={snapshot.capabilities} />
 */

import { useRef, useState } from "react";
import type { Capabilities } from "../../generated/Capabilities";
import { useI18n } from "../../i18n";
import type { SystemProbe } from "./probeTypes";
import { useProfilesStore } from "./profileStore";
import { CLI_KIND_LABELS, type LaunchProfile } from "./types";
import { ProfileForm } from "./ProfileForm";
import { CompatibilityMatrix } from "./CompatibilityMatrix";
import { ShellProfilesCard } from "./ShellProfilesCard";
import { versionProbeText } from "./probe";
import type { PlatformFlag } from "./validation";
import "./profiles.css";

export interface ProfilesPanelProps {
  showCompatibility?: boolean;
  probe?: SystemProbe | null;
  platform?: PlatformFlag;
  capabilities?: Capabilities | null;
}

export function ProfilesPanel(props: ProfilesPanelProps): JSX.Element {
  const { t } = useI18n();
  const profiles = useProfilesStore((s) => s.profiles);
  const removeProfile = useProfilesStore((s) => s.removeProfile);
  const importJson = useProfilesStore((s) => s.importJson);
  const exportJson = useProfilesStore((s) => s.exportJson);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [importError, setImportError] = useState<string | null>(null);
  const fileRef = useRef<HTMLInputElement | null>(null);

  const editing: LaunchProfile | null = useProfilesStore((s) =>
    editingId ? s.profileById(editingId) : null,
  );

  const onImportFile = async (file: File | null): Promise<void> => {
    if (!file) return;
    setImportError(null);
    const text = await file.text();
    const result = importJson(text);
    if (!result.ok) setImportError(result.error);
  };

  const onExport = (): void => {
    const text = exportJson();
    try {
      const blob = new Blob([text], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = "iyagi.profiles.v1.json";
      a.click();
      URL.revokeObjectURL(url);
    } catch {
      // 브라우저가 아닌 환경(테스트)에서는 다운로드를 건너뛴다.
    }
  };

  return (
    <section className="profiles-panel" aria-label={t("profile.panel.aria")}>
      <h2>{t("profile.panel.title")}</h2>
      <ul className="profile-list">
        {profiles.map((p) => (
          <li key={p.id} className="profile-list-item">
            <span className="workload-title">{p.label}</span>
            <span className="muted">{CLI_KIND_LABELS[p.descriptor.kind]}</span>
            <code title={p.descriptor.program}>{p.descriptor.program || t("profile.programUnset")}</code>
            <span className="muted">{versionProbeText(p.descriptor.detected_version === null ? { status: "unavailable", value: null } : { status: "value", value: p.descriptor.detected_version })}</span>
            <button type="button" onClick={() => { setCreating(false); setEditingId(p.id); }}>
              {t("profile.edit")}
            </button>
            <button type="button" onClick={() => { if (removeProfile(p.id) && editingId === p.id) setEditingId(null); }}>
              {t("profile.delete")}
            </button>
          </li>
        ))}
        {profiles.length === 0 ? <li className="muted">{t("profile.empty")}</li> : null}
      </ul>
      <div className="modal-actions">
        <button type="button" onClick={() => { setEditingId(null); setCreating(true); }}>
          {t("profile.new")}
        </button>
        <button type="button" onClick={onExport}>
          {t("profile.exportJson")}
        </button>
        <button type="button" onClick={() => fileRef.current?.click()}>
          {t("profile.importJson")}
        </button>
        <input
          ref={fileRef}
          type="file"
          accept="application/json,.json"
          style={{ display: "none" }}
          onChange={(e) => void onImportFile(e.target.files?.[0] ?? null)}
        />
      </div>
      {importError ? <p className="workload-notice notice-warn" role="alert">{importError}</p> : null}

      {creating || editing ? (
        <ProfileForm
          key={editing?.id ?? "new"}
          profile={editing}
          probe={props.probe ?? null}
          platform={props.platform}
          capabilities={props.capabilities ?? null}
          onSaved={() => {
            setCreating(false);
          }}
          onCancel={() => {
            setCreating(false);
            setEditingId(null);
          }}
        />
      ) : null}

      {props.showCompatibility !== false ? <>
        <h3>{t("profile.compatHeading")}</h3>
        <CompatibilityMatrix capabilities={props.capabilities ?? null} />
      </> : null}

      {/* 셸에서 바로 쓰는 ccd/ccg — 프로필과 같은 "실행 방법" 묶음이라 여기에 둔다. */}
      <ShellProfilesCard />
    </section>
  );
}

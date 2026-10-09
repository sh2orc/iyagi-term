/**
 * 시작 터미널 선택 대화상자(첫 실행/기본 미지정): 감지된 셸 프로필 중
 * 하나를 고르면 그 셸로 pane 1개를 전체 창에 띄운다. "기본으로 저장"이
 * 켜져 있으면(기본 켬) 다음 실행부터는 묻지 않고 바로 시작한다.
 *
 * 프로필 목록은 shellDeps 캐시에서 동기 초깃값을 즉시 보여주고, 탐지가
 * 끝나면 갱신한다(이 대화상자는 보통 탐지 완료 후 연 시점이라 캐시가
 * 이미 덥다).
 */

import { useEffect, useState } from "react";
import { useI18n } from "../../i18n";
import type { Platform } from "./shortcuts";
import { cachedBuiltinProfiles, detectShellProfiles } from "./shellDeps";
import { profileDisplayLabel, resolveProfile, type ShellProfile } from "./shellProfiles";
import { useShellProfileStore } from "../../stores/shellProfileStore";

export interface ShellSelectDialogProps {
  platform: Platform;
  /** 프로필 선택 시: (선택한 프로필, 기본 저장 여부) */
  onPick: (profile: ShellProfile, saveDefault: boolean) => void;
}

export function ShellSelectDialog(props: ShellSelectDialogProps): JSX.Element {
  const { t } = useI18n();
  const custom = useShellProfileStore((s) => s.custom);
  const [builtin, setBuiltin] = useState(() => cachedBuiltinProfiles(props.platform));
  const [saveDefault, setSaveDefault] = useState(true);

  useEffect(() => {
    let alive = true;
    void detectShellProfiles(props.platform).then((next) => {
      if (alive) setBuiltin(next);
    });
    return () => {
      alive = false;
    };
  }, [props.platform]);

  const profiles = [...builtin, ...custom];
  const recommended = resolveProfile(profiles, null);
  const pick = (profile: ShellProfile) => props.onPick(profile, saveDefault);

  return (
    <div className="shell-select">
      <h2>{t("terminal.select.title")}</h2>
      <p className="muted">{t("terminal.select.body")}</p>
      {profiles.length === 0 ? (
        <p className="shell-select-empty">{t("terminal.select.empty")}</p>
      ) : (
        <div className="shell-select-list" aria-label={t("terminal.menu")}>
          {profiles.map((profile) => (
            <button
              key={profile.id}
              type="button"
              className="shell-select-item"
              autoFocus={profile.id === recommended?.id}
              title={`${profile.program} ${profile.argv.join(" ")}`.trim()}
              onClick={() => pick(profile)}
            >
              <span className="shell-select-label">{profileDisplayLabel(profile, t)}</span>
              {profile.isDefault ? <span className="shell-select-detail">{t("terminal.select.systemDefault")}</span> : null}
              {profile.detail ? <span className="shell-select-detail">{profile.detail}</span> : null}
            </button>
          ))}
        </div>
      )}
      <label className="shell-select-save">
        <input
          type="checkbox"
          checked={saveDefault}
          onChange={(e) => setSaveDefault(e.target.checked)}
        />
        {t("terminal.select.save")}
      </label>
    </div>
  );
}

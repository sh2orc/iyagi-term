/**
 * 셸 프로필 선택 UI(Workbench 헤더): `+ Terminal` 본체 + ▾ 드롭다운.
 * 프로필 클릭 = 그 셸로 새 터미널, "기본으로 설정" = 저장된 기본 프로필.
 */

import { useEffect, useRef, useState } from "react";
import { useI18n } from "../../i18n";
import { useShellProfileStore } from "../../stores/shellProfileStore";
import type { Platform } from "./shortcuts";
import { detectShellProfiles } from "./shellDeps";
import { profileDisplayLabel, resolveProfile, type ShellProfile } from "./shellProfiles";

export interface ShellPickerProps {
  platform: Platform;
  onNewTerminal: (shell: { program: string; argv: string[]; label: string }) => void;
}

export function ShellPicker(props: ShellPickerProps): JSX.Element {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const [profiles, setProfiles] = useState<ShellProfile[]>([]);
  const [selected, setSelected] = useState<ShellProfile | null>(null);
  const wrapRef = useRef<HTMLDivElement>(null);

  const defaultId = useShellProfileStore((s) => s.defaultProfileId);
  const custom = useShellProfileStore((s) => s.custom);
  const setDefault = useShellProfileStore((s) => s.setDefault);

  useEffect(() => {
    let alive = true;
    void detectShellProfiles(props.platform).then((builtin) => {
      if (!alive) return;
      const all = [...builtin, ...custom];
      setProfiles(all);
      setSelected(resolveProfile(all, defaultId));
    });
    return () => {
      alive = false;
    };
    // 탐지는 플랫폼 변경 시에만 다시 한다(기본/커스텀 변경은 아래에서 반영).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.platform]);

  useEffect(() => {
    setSelected(resolveProfile([...profiles, ...custom], defaultId));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [defaultId, custom, profiles]);

  useEffect(() => {
    if (!open) return;
    const onDown = (event: MouseEvent) => {
      if (!wrapRef.current?.contains(event.target as globalThis.Node)) setOpen(false);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const startWith = (profile: ShellProfile) => {
    setOpen(false);
    // 라벨은 화면 표시 언어로 번역해 넘긴다(pane 제목으로 쓰인다).
    props.onNewTerminal({ program: profile.program, argv: profile.argv, label: profileDisplayLabel(profile, t) });
  };

  const display = (profile: ShellProfile | null) => (profile ? profileDisplayLabel(profile, t) : "");
  return (
    <div className="shell-picker" ref={wrapRef}>
      <button
        type="button"
        className="primary"
        aria-label={selected ? t("terminal.newTerminalWith", { label: display(selected) }) : t("terminal.newTerminal")}
        onClick={() => selected && startWith(selected)}
      >
        + {t("terminal.newTerminal")}{selected ? ` · ${display(selected)}` : ""}
      </button>
      <button
        type="button"
        className="primary shell-picker-toggle"
        aria-expanded={open}
        aria-haspopup="menu"
        aria-label={t("terminal.choose")}
        onClick={() => setOpen((v) => !v)}
      >
        ▾
      </button>
      {open ? (
        <div className="shell-picker-menu" role="menu" aria-label={t("terminal.menu")}>
          {profiles.length === 0 ? (
            <p className="shell-picker-empty">{t("terminal.noneDetected")}</p>
          ) : (
            profiles.map((profile) => (
              <div key={profile.id} className="shell-picker-item" role="menuitem">
                <button
                  type="button"
                  className="shell-picker-start"
                  onClick={() => startWith(profile)}
                  title={`${profile.program} ${profile.argv.join(" ")}`.trim()}
                >
                  <span className="shell-picker-label">{display(profile)}</span>
                  {profile.detail ? <span className="shell-picker-detail">{profile.detail}</span> : null}
                </button>
                {defaultId === profile.id ? (
                  <span className="shell-picker-default" aria-label={t("terminal.defaultBadgeAria")}>
                    {t("terminal.defaultBadge")}
                  </span>
                ) : (
                  <button
                    type="button"
                    className="shell-picker-setdefault"
                    onClick={() => setDefault(profile.id)}
                  >
                    {t("terminal.setDefault")}
                  </button>
                )}
              </div>
            ))
          )}
          <p className="shell-picker-note">{t("terminal.note")}</p>
        </div>
      ) : null}
    </div>
  );
}

import { useId } from "react";
import { useI18n } from "../../i18n";
import { usePreferences } from "../../store/preferences";
import type { CliKind } from "../profiles/types";
import { autonomyEnabled, supportsAutonomy } from "./autonomy";
import "./autonomy.css";

export function AutonomyToggle({ kind, description = false, disabled = false }: {
  kind: CliKind; description?: boolean; disabled?: boolean;
}): JSX.Element | null {
  const { t } = useI18n();
  const id = useId();
  const checked = usePreferences((s) => autonomyEnabled(kind, s));
  const setAutonomy = usePreferences((s) => s.setCliAutonomy);
  if (!supportsAutonomy(kind)) return null;
  const setting = `${kind}FullAutonomy`;
  return (
    <div className="cli-autonomy" data-setting-id={setting}>
      <label className="cli-autonomy-label" htmlFor={id}>
        <input id={id} type="checkbox" checked={checked} disabled={disabled}
          aria-describedby={description ? `${id}-description` : undefined}
          onChange={(e) => setAutonomy(kind, e.target.checked)} />
        <span>{t(`settings.item.${setting}.label`)}</span>
      </label>
      {description ? <p id={`${id}-description`} className="muted">{t(`settings.item.${setting}.description`)}</p> : null}
    </div>
  );
}

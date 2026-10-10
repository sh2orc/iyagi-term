/**
 * 설정 → 일반: 언어와 테마, 그리고 백그라운드 실행 관리자 재시작.
 *
 * 언어·테마는 상단 바에서 토글로만 만질 수 있던 값이라(언어) 또는 아예 만질 수
 * 없던 값이라(테마) 설정 페이지에 자리를 만들었다. 고르는 즉시 적용된다.
 * 재시작 행은 재시작 seam이 있는 클라이언트(Tauri)에서만 그린다.
 */

import type { DaemonClient } from "../daemon/client";
import { useI18n, useI18nStore, type Language } from "../../i18n";
import { usePreferences, type ThemePreference } from "../../store/preferences";
import { SettingRow, SettingSelect } from "./controls";
import { DaemonRestartRow } from "./DaemonRestartRow";
import { itemsInGroup } from "./schema";

const [languageItem, themeItem, daemonRestartItem] = itemsInGroup("general");

type LanguageChoice = "auto" | Language;

export function GeneralPanel({ client }: { client?: DaemonClient }): JSX.Element {
  const { t, language } = useI18n();
  const stored = useI18nStore((s) => s.language);
  const setLanguage = useI18nStore((s) => s.setLanguage);
  const theme = usePreferences((s) => s.theme);

  return (
    <>
      <SettingRow
        item={languageItem}
        changed={stored !== null}
        onReset={() => setLanguage(null)}
      >
        {(id) => (
          <SettingSelect<LanguageChoice>
            id={id}
            value={stored ?? "auto"}
            options={[
              { value: "auto", label: t("settings.item.language.auto").replace("{language}", t(`settings.item.language.${language}`)) },
              { value: "ko", label: t("settings.item.language.ko") },
              { value: "en", label: t("settings.item.language.en") },
            ]}
            onChange={(value) => setLanguage(value === "auto" ? null : value)}
          />
        )}
      </SettingRow>

      <SettingRow
        item={themeItem}
        changed={theme !== "system"}
        onReset={() => usePreferences.getState().resetPreference("theme")}
      >
        {(id) => (
          <SettingSelect<ThemePreference>
            id={id}
            value={theme}
            options={[
              { value: "system", label: t("settings.item.theme.system") },
              { value: "dark", label: t("settings.item.theme.dark") },
              { value: "light", label: t("settings.item.theme.light") },
            ]}
            onChange={(value) => usePreferences.getState().setTheme(value)}
          />
        )}
      </SettingRow>

      {client?.restartDaemon ? <DaemonRestartRow item={daemonRestartItem} client={client} /> : null}
    </>
  );
}

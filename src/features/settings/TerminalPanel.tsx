/**
 * 설정 → 터미널: 새 터미널이 시작하는 모습과 닫을 때의 동작.
 *
 * - 기본 글꼴 크기: 지금까지 pane zoom(Ctrl/Cmd +·-)으로만 바꿀 수 있었고
 *   재시작하면 13px로 돌아갔다. 여기 값이 새 pane의 시작 크기이자
 *   되돌리기(Cmd/Ctrl+0)의 기준점이다.
 * - 기본 셸: 헤더 드롭다운의 "기본으로"와 같은 값을 고른다.
 * - 사용자 셸: 탐지되지 않는 셸을 직접 등록한다(shellProfileStore.custom).
 * - 창 닫을 때 종료: 확인 대화상자의 "다시 묻지 않기"와 같은 값이다.
 */

import { useEffect } from "react";
import { useI18n } from "../../i18n";
import {
  CURSOR_STYLES,
  DEFAULT_PREFERENCES,
  HANGUL_TOGGLE_MODES,
  MAX_BASE_FONT_SIZE,
  MAX_SCROLLBACK_LINES,
  MIN_BASE_FONT_SIZE,
  MIN_SCROLLBACK_LINES,
  QUIT_BEHAVIORS,
  usePreferences,
  type HangulToggleMode,
  type PreferenceValues,
  type QuitBehavior,
} from "../../store/preferences";
import { useState } from "react";
import { useShellProfileStore } from "../../stores/shellProfileStore";
import { detectShellProfiles } from "../terminal/shellDeps";
import { AGENT_TINT_KEYS, tintedSurface, type AgentTintKey } from "../terminal/paneBackground";
import { resolveTheme, systemPrefersLight } from "../../app/theme";
import { terminalTheme } from "../terminal/terminalPalette";
import { customProfile, profileDisplayLabel, type ShellProfile } from "../terminal/shellProfiles";
import type { Platform } from "../terminal/shortcuts";
import { SettingNumber, SettingRow, SettingSelect, SettingToggle } from "./controls";
import { itemsInGroup } from "./schema";

const items = itemsInGroup("terminal");
const fontSizeItem = items.find((i) => i.id === "fontSize")!;
const fontFamilyItem = items.find((i) => i.id === "fontFamily")!;
const cursorStyleItem = items.find((i) => i.id === "cursorStyle")!;
const hangulToggleItem = items.find((i) => i.id === "hangulToggle")!;
const scrollbackItem = items.find((i) => i.id === "scrollback")!;
const osc52Item = items.find((i) => i.id === "osc52")!;
const gpuRendererItem = items.find((i) => i.id === "gpuRenderer")!;
const agentBackgroundsItem = items.find((i) => i.id === "agentBackgrounds")!;
const agentBackgroundColorsItem = items.find((i) => i.id === "agentBackgroundColors")!;
/** 색 고르기 행의 에이전트 이름 — 제품 이름이라 번역하지 않는다. */
const AGENT_TINT_LABELS: Record<AgentTintKey, string> = {
  claude: "Claude",
  zai: "Z.ai",
  codex: "Codex",
  opencode: "OpenCode",
};
const badgeItem = items.find((i) => i.id === "interventionTextBadge")!;
const defaultShellItem = items.find((i) => i.id === "defaultShell")!;
const customShellItem = items.find((i) => i.id === "customShell")!;
const terminateItem = items.find((i) => i.id === "terminateOnClose")!;
const quitItem = items.find((i) => i.id === "quitBehavior")!;

export function TerminalPanel(props: { platform: Platform }): JSX.Element {
  const { t } = useI18n();
  const baseFontSize = usePreferences((s) => s.baseFontSize);
  const terminateOnClose = usePreferences((s) => s.terminateOnClose);
  const quitBehavior = usePreferences((s) => s.quitBehavior);
  const defaultProfileId = useShellProfileStore((s) => s.defaultProfileId);
  const custom = useShellProfileStore((s) => s.custom);
  const [builtin, setBuiltin] = useState<ShellProfile[]>([]);

  useEffect(() => {
    let alive = true;
    void detectShellProfiles(props.platform).then((profiles) => {
      if (alive) setBuiltin(profiles);
    });
    return () => {
      alive = false;
    };
  }, [props.platform]);

  const shells = [...builtin, ...custom];

  const fontFamily = usePreferences((s) => s.fontFamily);
  // 입력 중에는 로컬 초안 — 저장 시점의 정리(trim·레거시 스택→자동)가
  // 타이핑 중 공백("Courier New")을 먹지 않게 blur/Enter에 확정한다.
  const [fontDraft, setFontDraft] = useState(fontFamily ?? "");
  useEffect(() => {
    setFontDraft(fontFamily ?? "");
  }, [fontFamily]);
  const commitFontFamily = (): void => {
    usePreferences.getState().setFontFamily(fontDraft);
    setFontDraft(usePreferences.getState().fontFamily ?? "");
  };
  const cursorStyle = usePreferences((s) => s.cursorStyle);
  const hangulToggle = usePreferences((s) => s.hangulToggle);
  const scrollbackLines = usePreferences((s) => s.scrollbackLines);
  const osc52Write = usePreferences((s) => s.osc52Write);
  const gpuRenderer = usePreferences((s) => s.gpuRenderer);
  const agentBackgrounds = usePreferences((s) => s.agentBackgrounds);
  const agentBackgroundColors = usePreferences((s) => s.agentBackgroundColors);
  // 미리보기는 지금 테마 기준이다 — 밝은 테마도 어두운 테마와 같은 면이
  // 나오지만, 더 어두운 테마를 쓰면 면도 그만큼 더 어두워진다.
  const themePreference = usePreferences((s) => s.theme);
  const themeBackground = terminalTheme(
    resolveTheme(themePreference, systemPrefersLight()),
  ).background;
  const interventionTextBadge = usePreferences((s) => s.interventionTextBadge);

  return (
    <>
      <SettingRow
        item={fontFamilyItem}
        changed={fontFamily !== DEFAULT_PREFERENCES.fontFamily}
        onReset={() => usePreferences.getState().resetPreference("fontFamily")}
      >
        {(id) => (
          <input
            id={id}
            className="setting-input"
            aria-label={t(fontFamilyItem.labelKey)}
            value={fontDraft}
            placeholder={'Consolas, Menlo, "Nanum Gothic Coding", monospace'}
            onChange={(e) => setFontDraft(e.target.value)}
            onBlur={commitFontFamily}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitFontFamily();
            }}
          />
        )}
      </SettingRow>

      <SettingRow
        item={cursorStyleItem}
        changed={cursorStyle !== DEFAULT_PREFERENCES.cursorStyle}
        onReset={() => usePreferences.getState().resetPreference("cursorStyle")}
      >
        {(id) => (
          <SettingSelect<PreferenceValues["cursorStyle"]>
            id={id}
            value={cursorStyle}
            options={CURSOR_STYLES.map((style) => ({
              value: style as PreferenceValues["cursorStyle"],
              label: t(`settings.item.cursorStyle.${style}`),
            }))}
            onChange={(value) => usePreferences.getState().setCursorStyle(value)}
          />
        )}
      </SettingRow>

      <SettingRow
        item={hangulToggleItem}
        changed={hangulToggle !== DEFAULT_PREFERENCES.hangulToggle}
        onReset={() => usePreferences.getState().resetPreference("hangulToggle")}
      >
        {(id) => (
          <SettingSelect<HangulToggleMode>
            id={id}
            value={hangulToggle}
            options={HANGUL_TOGGLE_MODES.map((mode) => ({
              value: mode,
              label: t(`settings.item.hangulToggle.${mode}`),
            }))}
            onChange={(value) => usePreferences.getState().setHangulToggle(value)}
          />
        )}
      </SettingRow>

      <SettingRow
        item={scrollbackItem}
        changed={scrollbackLines !== DEFAULT_PREFERENCES.scrollbackLines}
        onReset={() => usePreferences.getState().resetPreference("scrollbackLines")}
      >
        {(id) => (
          <SettingNumber
            id={id}
            value={scrollbackLines}
            min={MIN_SCROLLBACK_LINES}
            max={MAX_SCROLLBACK_LINES}
            unit={t("settings.item.scrollback.unit")}
            ariaLabel={t(scrollbackItem.labelKey)}
            onChange={(value) => usePreferences.getState().setScrollbackLines(value)}
          />
        )}
      </SettingRow>

      <SettingRow
        item={osc52Item}
        changed={osc52Write}
        onReset={() => usePreferences.getState().resetPreference("osc52Write")}
      >
        {(id) => (
          <SettingToggle
            id={id}
            checked={osc52Write}
            stateLabel={t(osc52Write ? "settings.on" : "settings.off")}
            onChange={(checked) => usePreferences.getState().setOsc52Write(checked)}
          />
        )}
      </SettingRow>

      <SettingRow
        item={gpuRendererItem}
        changed={gpuRenderer !== DEFAULT_PREFERENCES.gpuRenderer}
        onReset={() => usePreferences.getState().resetPreference("gpuRenderer")}
      >
        {(id) => (
          <SettingToggle
            id={id}
            checked={gpuRenderer}
            stateLabel={t(gpuRenderer ? "settings.on" : "settings.off")}
            onChange={(checked) => usePreferences.getState().setGpuRenderer(checked)}
          />
        )}
      </SettingRow>

      <SettingRow
        item={agentBackgroundsItem}
        changed={agentBackgrounds !== DEFAULT_PREFERENCES.agentBackgrounds}
        onReset={() => usePreferences.getState().resetPreference("agentBackgrounds")}
      >
        {(id) => (
          <SettingToggle
            id={id}
            checked={agentBackgrounds}
            stateLabel={t(agentBackgrounds ? "settings.on" : "settings.off")}
            onChange={(checked) => usePreferences.getState().setAgentBackgrounds(checked)}
          />
        )}
      </SettingRow>

      <SettingRow
        item={agentBackgroundColorsItem}
        layout="stacked"
        changed={AGENT_TINT_KEYS.some(
          (key) => agentBackgroundColors[key] !== DEFAULT_PREFERENCES.agentBackgroundColors[key],
        )}
        onReset={() => usePreferences.getState().resetPreference("agentBackgroundColors")}
      >
        {() => (
          <div className="agent-tint-row">
            {AGENT_TINT_KEYS.map((key) => (
              <label key={key} className="agent-tint">
                <span>{AGENT_TINT_LABELS[key]}</span>
                <input
                  type="color"
                  className="setting-color"
                  aria-label={`${t(agentBackgroundColorsItem.labelKey)} — ${AGENT_TINT_LABELS[key]}`}
                  value={agentBackgroundColors[key]}
                  onChange={(event) =>
                    usePreferences.getState().setAgentBackgroundColor(key, event.target.value)
                  }
                />
                {/* 고른 색이 아니라 '실제로 칠해질 배경'을 보여 준다 — 색조에서
                    색상·채도만 쓰고 밝기는 어두운 면에서 오므로 둘이 다르다. */}
                <span
                  className="agent-tint-preview"
                  aria-hidden="true"
                  style={{ background: tintedSurface(themeBackground, agentBackgroundColors[key]) }}
                />
              </label>
            ))}
          </div>
        )}
      </SettingRow>

      <SettingRow
        item={badgeItem}
        changed={interventionTextBadge}
        onReset={() => usePreferences.getState().resetPreference("interventionTextBadge")}
      >
        {(id) => (
          <SettingToggle
            id={id}
            checked={interventionTextBadge}
            stateLabel={t(interventionTextBadge ? "settings.on" : "settings.off")}
            onChange={(checked) => usePreferences.getState().setInterventionTextBadge(checked)}
          />
        )}
      </SettingRow>

      <SettingRow
        item={fontSizeItem}
        changed={baseFontSize !== DEFAULT_PREFERENCES.baseFontSize}
        onReset={() => usePreferences.getState().resetPreference("baseFontSize")}
      >
        {(id) => (
          <SettingNumber
            id={id}
            value={baseFontSize}
            min={MIN_BASE_FONT_SIZE}
            max={MAX_BASE_FONT_SIZE}
            unit={t("settings.item.fontSize.unit")}
            ariaLabel={t(fontSizeItem.labelKey)}
            onChange={(value) => usePreferences.getState().setBaseFontSize(value)}
          />
        )}
      </SettingRow>

      <SettingRow
        item={defaultShellItem}
        changed={defaultProfileId !== null}
        onReset={() => useShellProfileStore.getState().setDefault(null)}
      >
        {(id) => (
          <SettingSelect
            id={id}
            value={defaultProfileId ?? "__auto__"}
            options={[
              { value: "__auto__", label: t("settings.item.defaultShell.auto") },
              ...shells.map((profile) => ({
                value: profile.id,
                label: profileDisplayLabel(profile, t),
              })),
            ]}
            onChange={(value) =>
              useShellProfileStore.getState().setDefault(value === "__auto__" ? null : value)
            }
          />
        )}
      </SettingRow>

      <SettingRow item={customShellItem} layout="stacked">
        {(id) => <CustomShells inputId={id} custom={custom} />}
      </SettingRow>

      <SettingRow
        item={terminateItem}
        changed={terminateOnClose}
        onReset={() => usePreferences.getState().resetPreference("terminateOnClose")}
      >
        {(id) => (
          <SettingToggle
            id={id}
            checked={terminateOnClose}
            stateLabel={t(terminateOnClose ? "settings.on" : "settings.off")}
            onChange={(checked) => usePreferences.getState().setTerminateOnClose(checked)}
          />
        )}
      </SettingRow>

      <SettingRow
        item={quitItem}
        changed={quitBehavior !== DEFAULT_PREFERENCES.quitBehavior}
        onReset={() => usePreferences.getState().resetPreference("quitBehavior")}
      >
        {(id) => (
          <SettingSelect<QuitBehavior>
            id={id}
            value={quitBehavior}
            options={QUIT_BEHAVIORS.map((behavior) => ({
              value: behavior,
              label: t(`settings.item.quitBehavior.${behavior}`),
            }))}
            onChange={(value) => usePreferences.getState().setQuitBehavior(value)}
          />
        )}
      </SettingRow>
    </>
  );
}

/** 사용자 셸 추가/삭제 — 이름과 실행 파일 경로만 있으면 목록에 들어간다. */
function CustomShells(props: { inputId: string; custom: ShellProfile[] }): JSX.Element {
  const { t } = useI18n();
  const [label, setLabel] = useState("");
  const [program, setProgram] = useState("");
  const [argv, setArgv] = useState("");
  const [error, setError] = useState<string | null>(null);

  const add = (): void => {
    const trimmedLabel = label.trim();
    const trimmedProgram = program.trim();
    if (!trimmedLabel || !trimmedProgram) {
      setError(t("settings.customShell.invalid"));
      return;
    }
    const profile = customProfile(
      trimmedLabel,
      trimmedProgram,
      argv.trim() ? argv.trim().split(/\s+/) : [],
    );
    if (!useShellProfileStore.getState().addCustom(profile)) {
      setError(t("settings.customShell.duplicate"));
      return;
    }
    setError(null);
    setLabel("");
    setProgram("");
    setArgv("");
  };

  return (
    <div className="custom-shells">
      <div className="custom-shell-form">
        <label>
          <span>{t("settings.customShell.labelField")}</span>
          <input id={props.inputId} value={label} onChange={(e) => setLabel(e.target.value)} />
        </label>
        <label>
          <span>{t("settings.customShell.programField")}</span>
          <input value={program} onChange={(e) => setProgram(e.target.value)} />
        </label>
        <label>
          <span>{t("settings.customShell.argvField")}</span>
          <input value={argv} onChange={(e) => setArgv(e.target.value)} />
        </label>
        <button type="button" onClick={add}>
          {t("settings.customShell.add")}
        </button>
      </div>
      {error ? (
        <p className="setting-error" role="alert">
          {error}
        </p>
      ) : null}
      {props.custom.length === 0 ? (
        <p className="muted">{t("settings.customShell.empty")}</p>
      ) : (
        <ul className="custom-shell-list">
          {props.custom.map((profile) => (
            <li key={profile.id}>
              <span className="custom-shell-name">{profile.label}</span>
              <code>{[profile.program, ...profile.argv].join(" ")}</code>
              <button
                type="button"
                aria-label={t("settings.customShell.removeAria").replace("{label}", profile.label)}
                onClick={() => useShellProfileStore.getState().removeCustom(profile.id)}
              >
                {t("settings.customShell.remove")}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

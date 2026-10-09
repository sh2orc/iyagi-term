/**
 * 설정 → 단축키(W3-2 재정의 포함).
 *
 * - 기본표(읽기 전용)는 R1과 같다 — shortcutHints.test.ts가 표시된 조합이
 *   실제 mapShortcut 결과와 일치함을 지킨다.
 * - 그 아래 <details>로 접힌 "고급" 영역에서 액션별 바인딩을 재정의한다.
 *   토론 §2.4의 조건 4개: ① 바인딩 테이블만 사용자 소유(가드·repeat
 *   정책 불가침) ② 터미널 통과키(맨 Ctrl 조합·맨 키)는 예약 ③ 충돌
 *   검사(다른 재정의·기본표와) ④ 숨겨진 고급 + 기본 복원만.
 * - "통과(pass)" 재정의는 그 액션의 기본 조합을 터미널로 되돌려보낸다 —
 *   앱 조합을 셸에 돌려주고 싶은 사용자의 유일한 출구.
 */

import { useEffect, useState } from "react";
import { useI18n } from "../../i18n";
import {
  OVERRIDABLE_ACTIONS,
  bindingLabel,
  isReservedForTerminal,
  validateShortcutOverrides,
  type KeyBinding,
  type Platform,
  type ShortcutAction,
  type ShortcutOverrides,
} from "../terminal/shortcuts";
import { usePreferences } from "../../store/preferences";
import { shortcutHints } from "./shortcutHints";

export function ShortcutsPanel(props: { platform: Platform }): JSX.Element {
  const { t } = useI18n();
  const hints = shortcutHints(props.platform);
  const overrides = usePreferences((s) => s.shortcutOverrides);
  const [capturing, setCapturing] = useState<ShortcutAction | null>(null);
  const [captureError, setCaptureError] = useState<string | null>(null);

  useEffect(() => {
    if (!capturing) return;
    const onKeyDown = (event: KeyboardEvent): void => {
      event.preventDefault();
      event.stopPropagation();
      if (event.key === "Escape") {
        setCapturing(null);
        setCaptureError(null);
        return;
      }
      // 수정키 단독 입력은 조합이 아니므로 계속 기다린다.
      if (["Control", "Shift", "Alt", "Meta"].includes(event.key)) return;
      const binding: KeyBinding = {
        code: event.code,
        ctrl: event.ctrlKey,
        meta: event.metaKey,
        shift: event.shiftKey,
      };
      const errorKey = validateOverrideFor(capturing, binding, overrides, props.platform);
      if (errorKey) {
        setCaptureError(t(errorKey));
        return;
      }
      usePreferences.getState().setShortcutOverride(capturing, binding);
      setCapturing(null);
      setCaptureError(null);
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [capturing, overrides, props.platform, t]);

  const persistedErrors = validateShortcutOverrides(overrides, props.platform);
  const labelFor = (action: ShortcutAction): string =>
    t(hints.find((hint) => hint.action === action)?.labelKey ?? "settings.shortcuts.default");

  return (
    <>
      <p className="muted">{t("settings.shortcuts.note")}</p>
      <table className="shortcut-table">
        <thead>
          <tr>
            <th scope="col">{t("settings.shortcuts.action")}</th>
            <th scope="col">{t("settings.shortcuts.keys")}</th>
          </tr>
        </thead>
        <tbody>
          {hints.map((hint) => (
            <tr key={hint.action}>
              <th scope="row">
                {t(hint.labelKey)}
                {hint.requiresSelection ? (
                  <small className="muted"> {t("settings.shortcuts.needsSelection")}</small>
                ) : null}
              </th>
              <td>
                {overrideLabel(hint.action, overrides) ?? hint.keys.map((key) => <kbd key={key}>{key}</kbd>)}
              </td>
            </tr>
          ))}
        </tbody>
      </table>

      <details className="shortcut-advanced">
        <summary>{t("settings.shortcuts.advanced")}</summary>
        <p className="muted">{t("settings.shortcuts.advancedHint")}</p>
        {persistedErrors.length > 0 ? (
          <p className="setting-error" role="alert">
            {t("settings.shortcuts.persistedError", { count: persistedErrors.length })}
          </p>
        ) : null}
        <ul className="shortcut-override-list">
          {OVERRIDABLE_ACTIONS.map((action) => (
            <li key={action}>
              <span className="shortcut-override-action">{labelFor(action)}</span>
              <span className="shortcut-override-binding">
                {overrideLabel(action, overrides) ?? t("settings.shortcuts.default")}
              </span>
              {capturing === action ? (
                <span className="shortcut-capturing" role="status">
                  {t("settings.shortcuts.capturing")}
                </span>
              ) : (
                <>
                  <button
                    type="button"
                    onClick={() => {
                      setCapturing(action);
                      setCaptureError(null);
                    }}
                  >
                    {t("settings.shortcuts.change")}
                  </button>
                  <button
                    type="button"
                    onClick={() => usePreferences.getState().setShortcutOverride(action, "pass")}
                  >
                    {t("settings.shortcuts.pass")}
                  </button>
                  <button
                    type="button"
                    disabled={overrides[action] === undefined}
                    onClick={() => usePreferences.getState().setShortcutOverride(action, null)}
                  >
                    {t("settings.shortcuts.reset")}
                  </button>
                </>
              )}
            </li>
          ))}
        </ul>
        {captureError ? (
          <p className="setting-error" role="alert">
            {captureError}
          </p>
        ) : null}
      </details>
    </>
  );
}

function overrideLabel(action: ShortcutAction, overrides: ShortcutOverrides): string | null {
  const value = overrides[action];
  if (!value) return null;
  if (value === "pass") return "→ terminal";
  return bindingLabel(value);
}

/** 저장 전 단일 후보 검증 — 오류 사유의 i18n 키를 반환한다. */
function validateOverrideFor(
  action: ShortcutAction,
  binding: KeyBinding,
  overrides: ShortcutOverrides,
  platform: Platform,
): string | null {
  if (isReservedForTerminal(binding)) return "settings.shortcuts.error.reserved";
  const candidate: ShortcutOverrides = { ...overrides };
  // 같은 액션의 기존 재정의를 교체할 때는 자기 자신과 충돌하지 않게 뺀다.
  delete candidate[action];
  const errors = validateShortcutOverrides({ ...candidate, [action]: binding }, platform);
  const own = errors.find(([errorAction]) => errorAction === action);
  if (own) {
    return own[1].startsWith("conflicts")
      ? "settings.shortcuts.error.conflicts"
      : "settings.shortcuts.error.reserved";
  }
  return null;
}

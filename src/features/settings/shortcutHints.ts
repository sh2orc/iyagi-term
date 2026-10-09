/**
 * 단축키 안내표(설정 → 단축키). 04-ui.md §3 계약을 사람이 읽는 형태로
 * 옮긴 것이며, R1에서는 재정의가 없으므로 읽기 전용이다.
 *
 * 표가 실제 동작과 어긋나는 것이 이런 화면의 유일한 실패 방식이라,
 * 각 행은 mapShortcut에 그대로 먹일 수 있는 KeyEventLike를 함께 들고
 * 다닌다. shortcutHints.test.ts가 모든 행을 mapShortcut에 통과시켜
 * 표시된 조합이 정말 그 동작을 부르는지 검증한다.
 */

import { keyEvent, type KeyEventLike, type Platform, type ShortcutAction } from "../terminal/shortcuts";

export interface ShortcutHint {
  action: ShortcutAction;
  /** i18n 키 — settings.shortcut.<action>. */
  labelKey: string;
  /** 표시용 키 조각(⌘ · Shift · D …). */
  keys: readonly string[];
  /** 이 조합을 눌렀을 때의 이벤트 — 시험이 mapShortcut으로 되짚는다. */
  event: KeyEventLike;
  /** macOS Cmd+C처럼 선택이 있어야 발동하는 동작. */
  requiresSelection?: boolean;
}

interface Combo {
  action: ShortcutAction;
  code: string;
  /** 표시용 마지막 글쇠. */
  keyLabel: string;
  shift?: boolean;
  requiresSelection?: boolean;
}

const DARWIN: readonly Combo[] = [
  { action: "split-row", code: "KeyD", keyLabel: "D" },
  { action: "split-column", code: "KeyD", keyLabel: "D", shift: true },
  { action: "close-pane", code: "KeyW", keyLabel: "W" },
  { action: "next-tab", code: "BracketRight", keyLabel: "]", shift: true },
  { action: "prev-tab", code: "BracketLeft", keyLabel: "[", shift: true },
  { action: "palette", code: "KeyP", keyLabel: "P", shift: true },
  { action: "queue-toggle", code: "KeyB", keyLabel: "B" },
  { action: "broadcast-toggle", code: "KeyB", keyLabel: "B", shift: true },
  { action: "layout-editor", code: "KeyG", keyLabel: "G", shift: true },
  { action: "new-mission", code: "KeyM", keyLabel: "M", shift: true },
  { action: "resume-all", code: "KeyR", keyLabel: "R", shift: true },
  { action: "copy", code: "KeyC", keyLabel: "C", requiresSelection: true },
  { action: "paste", code: "KeyV", keyLabel: "V" },
  { action: "search", code: "KeyF", keyLabel: "F" },
  { action: "zoom-in", code: "Equal", keyLabel: "=" },
  { action: "zoom-out", code: "Minus", keyLabel: "-" },
  { action: "zoom-reset", code: "Digit0", keyLabel: "0" },
];

const OTHER: readonly Combo[] = [
  { action: "split-row", code: "KeyD", keyLabel: "D", shift: true },
  { action: "split-column", code: "KeyE", keyLabel: "E", shift: true },
  { action: "close-pane", code: "KeyW", keyLabel: "W", shift: true },
  { action: "next-tab", code: "PageDown", keyLabel: "PgDn" },
  { action: "prev-tab", code: "PageUp", keyLabel: "PgUp" },
  { action: "palette", code: "KeyP", keyLabel: "P", shift: true },
  { action: "queue-toggle", code: "KeyB", keyLabel: "B" },
  { action: "broadcast-toggle", code: "KeyB", keyLabel: "B", shift: true },
  { action: "layout-editor", code: "KeyG", keyLabel: "G", shift: true },
  { action: "new-mission", code: "KeyM", keyLabel: "M", shift: true },
  { action: "resume-all", code: "KeyR", keyLabel: "R", shift: true },
  { action: "copy", code: "KeyC", keyLabel: "C", shift: true },
  { action: "paste", code: "KeyV", keyLabel: "V", shift: true },
  { action: "search", code: "KeyF", keyLabel: "F", shift: true },
  { action: "zoom-in", code: "Equal", keyLabel: "=" },
  { action: "zoom-out", code: "Minus", keyLabel: "-" },
  { action: "zoom-reset", code: "Digit0", keyLabel: "0" },
];

export function shortcutHints(platform: Platform): ShortcutHint[] {
  const darwin = platform === "darwin";
  const combos = darwin ? DARWIN : OTHER;
  return combos.map((combo) => {
    const keys = [darwin ? "⌘" : "Ctrl"];
    if (combo.shift) keys.push(darwin ? "⇧" : "Shift");
    keys.push(combo.keyLabel);
    return {
      action: combo.action,
      labelKey: `settings.shortcut.${combo.action}`,
      keys,
      requiresSelection: combo.requiresSelection,
      event: keyEvent({
        code: combo.code,
        key: combo.keyLabel.toLowerCase(),
        metaKey: darwin,
        ctrlKey: !darwin,
        shiftKey: combo.shift === true,
      }),
    };
  });
}

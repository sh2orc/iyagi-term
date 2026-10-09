/**
 * 안내표가 실제 동작과 어긋나지 않는지 — 표시된 조합을 그대로 mapShortcut에
 * 먹여 같은 액션이 나오는지 확인한다(문서와 구현이 갈라지는 것을 막는 유일한
 * 방법이다).
 */

import { describe, expect, it } from "vitest";
import { ko } from "../../i18n/sections/settings";
import { mapShortcut, type Platform, type ShortcutContext } from "../terminal/shortcuts";
import { shortcutHints } from "./shortcutHints";

const platforms: Platform[] = ["darwin", "windows", "linux"];

const context = (platform: Platform, hasSelection: boolean): ShortcutContext => ({
  platform,
  imeComposing: false,
  modalOpen: false,
  hasSelection,
});

describe.each(platforms)("shortcut hints (%s)", (platform) => {
  const hints = shortcutHints(platform);

  it("표시한 조합이 정말 그 동작을 부른다", () => {
    for (const hint of hints) {
      const action = mapShortcut(hint.event, context(platform, hint.requiresSelection === true));
      expect(action, `${hint.action}: ${hint.keys.join("+")}`).toBe(hint.action);
    }
  });

  it("모든 행의 문구가 사전에 있다", () => {
    for (const hint of hints) {
      expect(ko, hint.labelKey).toHaveProperty(hint.labelKey);
    }
  });

  it("동작이 중복되지 않는다", () => {
    const actions = hints.map((h) => h.action);
    expect(new Set(actions).size).toBe(actions.length);
  });

  it("수정 키 표기가 플랫폼을 따른다", () => {
    const first = hints[0].keys[0];
    expect(first).toBe(platform === "darwin" ? "⌘" : "Ctrl");
  });
});

describe("macOS 복사", () => {
  it("선택이 없으면 발동하지 않는다(04 §3 no-op 규칙)", () => {
    const copy = shortcutHints("darwin").find((h) => h.action === "copy");
    expect(copy?.requiresSelection).toBe(true);
    expect(mapShortcut(copy!.event, context("darwin", false))).toBeNull();
  });
});

describe("새 AI 작업 단축키", () => {
  it.each(platforms)("설정 → 단축키 표에 새 AI 작업이 보인다(%s)", (platform) => {
    const hint = shortcutHints(platform).find((h) => h.action === "new-mission");
    expect(hint?.labelKey).toBe("settings.shortcut.new-mission");
    expect(hint?.keys).toEqual(platform === "darwin" ? ["⌘", "⇧", "M"] : ["Ctrl", "Shift", "M"]);
  });
});

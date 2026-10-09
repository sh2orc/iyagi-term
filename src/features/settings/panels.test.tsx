/**
 * 패널이 실제로 그려지는지 + 사전이 등록돼 원시 키가 새어 나가지 않는지.
 * (i18n index를 거쳐야 사전이 등록된다 — core만 import하면 키가 그대로 보인다.)
 */

import { afterEach, describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";
import { GeneralPanel } from "./GeneralPanel";
import { ShortcutsPanel } from "./ShortcutsPanel";
import { TerminalPanel } from "./TerminalPanel";
import { TrackpadPanel } from "./TrackpadPanel";
import { DEFAULT_PREFERENCES, usePreferences } from "../../store/preferences";
import { t } from "../../i18n";
import { SETTINGS_ITEMS } from "./schema";
import { SettingNumber, SettingRow } from "./controls";

afterEach(() => usePreferences.setState({ ...DEFAULT_PREFERENCES }));

describe("설정 패널 렌더", () => {
  it("일반 패널이 언어·테마 행을 낸다", () => {
    const html = renderToString(<GeneralPanel />);
    expect(html).toContain('data-setting-id="language"');
    expect(html).toContain('data-setting-id="theme"');
    expect(html).toContain(t("settings.item.theme.system"));
  });

  it("터미널 패널이 네 항목을 모두 낸다", () => {
    const html = renderToString(<TerminalPanel platform="darwin" />);
    for (const item of SETTINGS_ITEMS.filter((i) => i.group === "terminal")) {
      expect(html, item.id).toContain(`data-setting-id="${item.id}"`);
    }
    expect(html).toContain(t("settings.item.defaultShell.auto"));
  });

  it("트랙패드 패널이 제스처 네 항목을 모두 낸다", () => {
    const html = renderToString(<TrackpadPanel />);
    for (const item of SETTINGS_ITEMS.filter((i) => i.group === "trackpad")) {
      expect(html, item.id).toContain(`data-setting-id="${item.id}"`);
    }
    expect(html).toContain(t("settings.item.tabSwipe.label"));
    expect(html).toContain(t("settings.item.tabSwipeSensitivity.medium"));
    expect(html).not.toContain("settings.item.");
  });

  it("단축키 패널이 플랫폼에 맞는 수정 키를 보여 준다", () => {
    expect(renderToString(<ShortcutsPanel platform="darwin" />)).toContain("⌘");
    expect(renderToString(<ShortcutsPanel platform="linux" />)).toContain("Ctrl");
  });

  // 되돌리기 버튼은 SettingRow에 직접 물어본다. zustand v4는 renderToString에서
  // 생성 시점 state만 돌려주므로(ManagedRunDialog와 같은 제약) 스토어를 바꿔
  // 놓고 SSR로 확인하는 방식은 성립하지 않는다.
  it("기본값에서 벗어난 항목만 되돌리기 버튼을 낸다", () => {
    const item = SETTINGS_ITEMS[0];
    const row = (changed: boolean) =>
      renderToString(
        <SettingRow item={item} changed={changed} onReset={() => undefined}>
          {(id) => <input id={id} readOnly value="" />}
        </SettingRow>,
      );
    expect(row(false)).not.toContain(t("settings.resetItem"));
    expect(row(true)).toContain(t("settings.resetItem"));
  });

  it("사전이 등록돼 원시 키가 화면에 남지 않는다", () => {
    const html = renderToString(<GeneralPanel />) + renderToString(<ShortcutsPanel platform="linux" />);
    expect(html).not.toContain("settings.item.");
    expect(html).not.toContain("settings.shortcut.");
  });
});

describe("숫자 입력 초안", () => {
  it("슬라이더와 숫자 입력이 같은 값을 보여 준다(확정은 blur/Enter)", () => {
    const html = renderToString(
      <SettingNumber id="n" value={12} min={9} max={24} ariaLabel="size" onChange={() => undefined} />,
    );
    expect(html.match(/value="12"/g)?.length).toBe(2);
  });
});

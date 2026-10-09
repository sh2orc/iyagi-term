/**
 * 설정 → 트랙패드: 두 손가락 가로 스와이프로 탭을 넘기는 제스처
 * (features/terminal/tabSwipe.ts)의 네 가지 값.
 *
 * - 켜기/끄기: 꺼 두면 wheel 이벤트를 아예 건드리지 않는다.
 * - 감도: 탭이 넘어가는 문턱(px). 낮음 220 · 보통 120 · 높음 60.
 * - 방향 반전: 자연스러운 스크롤을 끈 사용자를 위한 것이다.
 * - 순환: 끝 탭에서 반대편 끝으로 갈지. 단축키(다음/이전 탭)도 이 값을 따른다.
 */

import { useI18n } from "../../i18n";
import { DEFAULT_PREFERENCES, usePreferences } from "../../store/preferences";
import {
  DEFAULT_TAB_SWIPE,
  TAB_SWIPE_SENSITIVITIES,
  type TabSwipePrefs,
  type TabSwipeSensitivity,
} from "../terminal/tabSwipe";
import { SettingRow, SettingSelect, SettingToggle } from "./controls";
import { itemsInGroup } from "./schema";

const items = itemsInGroup("trackpad");
const enabledItem = items.find((i) => i.id === "tabSwipe")!;
const sensitivityItem = items.find((i) => i.id === "tabSwipeSensitivity")!;
const reverseItem = items.find((i) => i.id === "tabSwipeReverse")!;
const wrapItem = items.find((i) => i.id === "tabSwipeWrap")!;
const effectItem = items.find((i) => i.id === "tabSwitchEffect")!;

export function TrackpadPanel(): JSX.Element {
  const { t } = useI18n();
  const tabSwipe = usePreferences((s) => s.tabSwipe);
  const tabSwitchEffect = usePreferences((s) => s.tabSwitchEffect);
  const update = (patch: Partial<TabSwipePrefs>): void => usePreferences.getState().setTabSwipe(patch);
  const onOff = (value: boolean): string => t(value ? "settings.on" : "settings.off");

  return (
    <>
      <SettingRow
        item={enabledItem}
        changed={tabSwipe.enabled !== DEFAULT_TAB_SWIPE.enabled}
        onReset={() => update({ enabled: DEFAULT_TAB_SWIPE.enabled })}
      >
        {(id) => (
          <SettingToggle
            id={id}
            checked={tabSwipe.enabled}
            stateLabel={onOff(tabSwipe.enabled)}
            onChange={(checked) => update({ enabled: checked })}
          />
        )}
      </SettingRow>

      <SettingRow
        item={sensitivityItem}
        changed={tabSwipe.sensitivity !== DEFAULT_TAB_SWIPE.sensitivity}
        onReset={() => update({ sensitivity: DEFAULT_TAB_SWIPE.sensitivity })}
      >
        {(id) => (
          <SettingSelect<TabSwipeSensitivity>
            id={id}
            value={tabSwipe.sensitivity}
            disabled={!tabSwipe.enabled}
            options={TAB_SWIPE_SENSITIVITIES.map((sensitivity) => ({
              value: sensitivity,
              label: t(`settings.item.tabSwipeSensitivity.${sensitivity}`),
            }))}
            onChange={(value) => update({ sensitivity: value })}
          />
        )}
      </SettingRow>

      <SettingRow
        item={reverseItem}
        changed={tabSwipe.reverse !== DEFAULT_TAB_SWIPE.reverse}
        onReset={() => update({ reverse: DEFAULT_TAB_SWIPE.reverse })}
      >
        {(id) => (
          <SettingToggle
            id={id}
            checked={tabSwipe.reverse}
            stateLabel={onOff(tabSwipe.reverse)}
            onChange={(checked) => update({ reverse: checked })}
          />
        )}
      </SettingRow>

      <SettingRow
        item={wrapItem}
        changed={tabSwipe.wrap !== DEFAULT_TAB_SWIPE.wrap}
        onReset={() => update({ wrap: DEFAULT_TAB_SWIPE.wrap })}
      >
        {(id) => (
          <SettingToggle
            id={id}
            checked={tabSwipe.wrap}
            stateLabel={onOff(tabSwipe.wrap)}
            onChange={(checked) => update({ wrap: checked })}
          />
        )}
      </SettingRow>

      <SettingRow
        item={effectItem}
        changed={tabSwitchEffect !== DEFAULT_PREFERENCES.tabSwitchEffect}
        onReset={() => usePreferences.getState().setTabSwitchEffect(DEFAULT_PREFERENCES.tabSwitchEffect)}
      >
        {(id) => (
          <SettingToggle
            id={id}
            checked={tabSwitchEffect}
            stateLabel={onOff(tabSwitchEffect)}
            onChange={(checked) => usePreferences.getState().setTabSwitchEffect(checked)}
          />
        )}
      </SettingRow>
    </>
  );
}

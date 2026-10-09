/**
 * MissionGuide: 펼치면 기본 흐름 다섯 줄(준비·시작·진행·확인·확정)만 보이고,
 * 나머지 안내는 접힌 `문제가 생겼을 때` 안에 둔다. 개발자 식별자는 사용자 문구에 없다.
 */

import { afterEach, expect, it } from "vitest";
import { t, useI18nStore } from "../../i18n";
import { MISSION_GUIDE_STEPS, MISSION_GUIDE_TROUBLE, MissionGuide } from "./MissionGuide";
import { renderUi } from "./testSupport";

afterEach(() => useI18nStore.setState({ language: null }));

it.each(["ko", "en"] as const)("%s 기본 흐름은 다섯 줄이고 나머지는 접힌 문제 해결 안에 있다", (language) => {
  useI18nStore.setState({ language });
  const ui = renderUi(<MissionGuide />);
  try {
    const guide = ui.container.querySelector("[data-testid=mission-guide]")!;
    const direct = [...guide.children];
    expect(direct.find((child) => child.tagName === "SUMMARY")?.textContent).toBe(t("missions.guide.title"));
    const steps = [...guide.querySelectorAll("[data-testid=mission-guide-steps] > li")].map((li) => li.textContent);
    expect(steps).toEqual(MISSION_GUIDE_STEPS.map((step) => t(`missions.guide.${step}`)));
    expect(steps).toHaveLength(5);
    // 기본 흐름 밖의 문단은 모두 접힌 details 안에만 있다.
    expect(direct.map((child) => child.tagName)).toEqual(["SUMMARY", "OL", "DETAILS"]);
    const trouble = guide.querySelector<HTMLDetailsElement>("[data-testid=mission-guide-trouble]")!;
    expect(trouble.open).toBe(false);
    expect(trouble.querySelector("summary")?.textContent).toBe(t("missions.guide.troubleTitle"));
    expect([...trouble.querySelectorAll("p")].map((p) => p.textContent)).toEqual(
      MISSION_GUIDE_TROUBLE.map((item) => t(`missions.guide.${item}`)),
    );
    const text = guide.textContent ?? "";
    for (const internal of ["IYAGI_VERIFICATION_OUTPUT", "mission_protocol", "worktree", "binding"]) {
      expect(text).not.toContain(internal);
    }
  } finally {
    ui.unmount();
  }
});

it("각 문제 해결 문단은 두 문장 이내다(ko)", () => {
  useI18nStore.setState({ language: "ko" });
  for (const item of MISSION_GUIDE_TROUBLE) {
    const sentences = t(`missions.guide.${item}`).split(/(?<=[.?!])\s+/).filter((part) => part.trim().length > 0);
    expect(sentences.length, item).toBeLessThanOrEqual(2);
  }
});

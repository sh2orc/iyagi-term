/**
 * 명령 팔레트 목록 계약 — 재배치 명령(04-ui §2-5)이 팔레트에도 있어야
 * 마우스 없이 같은 일을 할 수 있다. node 환경이라 실행은 흉내 낼 수 없고
 * (동작은 controller·store 시험이 맡는다), 어떤 명령이 어떤 순서로 보이는지를
 * 고정한다.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ControllerContext } from "./controllerContext";
import { MISSION_PALETTE_KEYWORDS, ModalHost, paletteCommandMatches } from "./Modals";
import { t } from "../i18n";
import type { SessionController } from "../features/terminal/sessionController";
import { useWorkbenchStore } from "../store/workbenchStore";

// TerminalPane.test.tsx와 같은 이유: react-dom/server가 스토어 생성 시점의
// 스냅샷을 고정하지 않도록 현재 상태를 그대로 읽게 한다.
vi.mock("../store/workbenchStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../store/workbenchStore")>();
  const store = actual.useWorkbenchStore;
  return {
    ...actual,
    useWorkbenchStore: Object.assign(
      (selector: (state: ReturnType<typeof store.getState>) => unknown) => selector(store.getState()),
      store,
    ),
  };
});

function renderPalette(missionProtocol: number | null = null): string {
  useWorkbenchStore.setState({ modal: { kind: "palette" }, activeTabId: "tab-1", focusedLeafId: "leaf-1", missionProtocol });
  return renderToStaticMarkup(
    <ControllerContext.Provider value={{} as SessionController}>
      <ModalHost platform="darwin" />
    </ControllerContext.Provider>,
  );
}

afterEach(() => useWorkbenchStore.setState({ modal: null, activeTabId: null, focusedLeafId: null, missionProtocol: null }));

describe("command palette — 탭 재그룹핑 명령", () => {
  it("이름 바꾸기 바로 뒤에 창 이동·탭 이동·합치기·다시 묶기가 이어진다", () => {
    const html = renderPalette();
    const order = [
      "palette.renameTab",
      "palette.detachPane",
      "palette.movePane",
      "palette.moveTabLeft",
      "palette.moveTabRight",
      "palette.mergeTab",
      "palette.regroup",
      "palette.broadcast",
    ].map((key) => ({ key, at: html.indexOf(t(key)) }));

    for (const entry of order) {
      expect(entry.at, `${entry.key} missing from the palette`).toBeGreaterThanOrEqual(0);
    }
    for (let i = 1; i < order.length; i += 1) {
      expect(order[i].at, `${order[i].key} must follow ${order[i - 1].key}`).toBeGreaterThan(order[i - 1].at);
    }
  });
});

describe("command palette — AI 작업 명령과 검색어", () => {
  it("AI 작업: 새로 만들기·목록·설정·사용법이 이 순서로 있다", () => {
    const html = renderPalette(1);
    const at = ["palette.missionNew", "palette.missionList", "palette.missionSettings", "palette.missionGuide"].map((key) => ({
      key,
      at: html.indexOf(t(key)),
    }));
    for (const entry of at) expect(entry.at, `${entry.key} missing from the palette`).toBeGreaterThanOrEqual(0);
    for (let i = 1; i < at.length; i += 1) expect(at[i].at).toBeGreaterThan(at[i - 1].at);
    // 쓸 수 있을 때도 새 AI 작업에는 무엇을 하는지 한 줄 설명이 붙는다.
    expect(html).toContain(`title="${t("missions.newMission.hint")}"`);
  });

  it("데몬이 프로토콜을 선언하지 않으면(개발 빌드) 새로 만들기·목록은 사유와 함께 비활성", () => {
    const html = renderPalette(null);
    expect(html).toContain(t("palette.missionNew"));
    expect(html).toContain(`title="${t("missions.newMission.unavailable")}"`);
  });

  it("라벨에 없는 검색어(mission·미션·agent·에이전트·team·팀·AI)로도 AI 작업 명령을 찾는다", () => {
    const command = { label: t("palette.missionList"), keywords: MISSION_PALETTE_KEYWORDS };
    for (const query of ["mission", "미션", "Agent", "에이전트", "team", "팀", "ai", "  MISSION  "]) {
      expect(paletteCommandMatches(command, query), query).toBe(true);
    }
    expect(paletteCommandMatches(command, "zoom")).toBe(false);
    // 검색어가 없는 명령은 라벨로만 찾는다.
    expect(paletteCommandMatches({ label: t("palette.newTab") }, "mission")).toBe(false);
    expect(paletteCommandMatches({ label: t("palette.newTab") }, "")).toBe(true);
  });
});

/**
 * 다시 묶기 미리보기 마크업(04-ui §2-5): 사용자가 "적용" 전에 무엇을 보고
 * 판단하는지 — 결과 탭 수, 완전히 그대로인 탭·배치만 다시 짜는 탭·새로
 * 생기는 탭, 각 탭에 들어갈 터미널 — 을 고정한다. 적용 자체는 store·
 * controller 시험이 맡는다.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ControllerContext } from "./controllerContext";
import { RegroupDialog } from "./RegroupDialog";
import { t } from "../i18n";
import type { RegroupPlan } from "../features/terminal/regroup";
import type { SessionController } from "../features/terminal/sessionController";
import { useWorkbenchStore, type PaneMeta } from "../store/workbenchStore";

// TerminalPane.test.tsx와 같은 이유: react-dom/server는 zustand 스토어가
// 만들어질 때의 스냅샷을 고정한다 — 사례마다 현재 상태를 읽게 한다.
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

function pane(leafId: string, title: string, agent?: string): PaneMeta {
  return {
    leafId,
    viewId: `view-${leafId}`,
    sessionId: null,
    workloadId: null,
    title,
    cwd: null,
    phase: "live",
    error: null,
    usage: null,
    flowBlocked: false,
    agent: agent
      ? {
          agent,
          pid: 1,
          detected_at_ms: 1,
          session_id: null,
          session_name: null,
          session_source: null,
          session_status: null,
        }
      : null,
  };
}

const plan: RegroupPlan = {
  groups: [
    { key: "/work/iyagi", title: "iyagi", leafIds: ["leaf-1", "leaf-2"], fromTabIds: ["tab-1"], keepTabId: "tab-1", relayout: false },
    { key: "/work/api", title: "api", leafIds: ["leaf-3"], fromTabIds: ["tab-1", "tab-2"], keepTabId: null, relayout: false },
  ],
  changed: true,
  tabsBefore: 3,
  tabsAfter: 2,
};

const relayoutPlan: RegroupPlan = {
  groups: [
    { key: "/work/iyagi", title: "iyagi", leafIds: ["leaf-1", "leaf-2"], fromTabIds: ["tab-1"], keepTabId: "tab-1", relayout: true },
  ],
  changed: true,
  tabsBefore: 1,
  tabsAfter: 1,
};

function render(next: RegroupPlan): string {
  useWorkbenchStore.setState({
    panes: {
      "leaf-1": pane("leaf-1", "zsh"),
      "leaf-2": pane("leaf-2", "claude", "claude"),
      "leaf-3": pane("leaf-3", "/usr/bin/env -u NO_COLOR -u FORCE_COLOR -u CLICOLOR -u CLICOLOR_FORCE /bin/zsh -l"),
    },
  });
  return renderToStaticMarkup(
    <ControllerContext.Provider value={{} as SessionController}>
      <RegroupDialog plan={next} />
    </ControllerContext.Provider>,
  );
}

afterEach(() => useWorkbenchStore.setState({ panes: {} }));

describe("RegroupDialog preview", () => {
  it("탭 수 변화와 결과 탭들을 순서대로 보여 준다", () => {
    const html = render(plan);
    expect(html).toContain(t("regroup.title"));
    expect(html).toContain(t("regroup.description"));
    expect(html).toContain(t("regroup.summary", { before: 3, after: 2 }));
    expect(html.indexOf("iyagi")).toBeLessThan(html.indexOf(">api<"));
  });

  it("그대로 남는 탭과 새로 생기는 탭을 배지로 가른다", () => {
    const html = render(plan);
    expect(html).toContain('class="regroup-badge kept"');
    expect(html).toContain('class="regroup-badge new"');
    expect(html).toContain(t("regroup.kept"));
    expect(html).toContain(t("regroup.new"));
    // 유지 배지가 먼저다(첫 그룹이 keepTabId를 가진 그룹).
    expect(html.indexOf("regroup-badge kept")).toBeLessThan(html.indexOf("regroup-badge new"));
  });

  it("배치만 다시 짜는 탭(id·이름은 그대로)은 relayout 배지로 가른다", () => {
    const html = render(relayoutPlan);
    expect(html).toContain('class="regroup-badge relayout"');
    expect(html).toContain(t("regroup.relayout"));
    expect(html).not.toContain('class="regroup-badge kept"');
    expect(html).not.toContain('class="regroup-badge new"');
  });

  it("각 탭에 들어갈 터미널을 표시 이름으로 적고, 에이전트는 아이콘을 붙인다", () => {
    const html = render(plan);
    expect(html).toContain(">zsh<");
    expect(html).toContain(">claude<");
    expect(html).toContain("agent-icon");
    // 내부 런처(env …)는 벗겨 낸 이름으로 보인다.
    expect(html).not.toContain("NO_COLOR");
  });

  it("적용·취소 두 갈래를 두고, 적용에 초점을 준다", () => {
    const html = render(plan);
    expect(html).toContain(t("regroup.apply"));
    expect(html).toContain(t("regroup.cancel"));
    expect(html).toContain('class="primary"');
  });

  it("바뀔 것이 없으면 미리보기 대신 안내와 닫기만 낸다(방어적 경로)", () => {
    const html = render({ groups: plan.groups, changed: false, tabsBefore: 2, tabsAfter: 2 });
    expect(html).toContain(t("regroup.noChange"));
    // 적용 버튼(primary)도 목록도 없다 — "다시 묶기"는 제목에만 남는다.
    expect(html).not.toContain('class="primary"');
    expect(html).not.toContain("regroup-list");
    expect(html).toContain(t("notice.confirm"));
  });
});

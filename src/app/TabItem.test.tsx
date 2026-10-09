/**
 * 탭 렌더링: 이름 표시 ↔ 인라인 편집기 전환. node 환경이라 상호작용은
 * store 계약 시험(tests/workbenchStore.test.ts)이 맡고, 여기서는
 * props → 화면 매핑만 확인한다.
 */

import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";
import { t } from "../i18n";
import { TabItem, missionTabBadgeText, type TabItemProps } from "./TabItem";
import type { MissionTabBadge } from "../features/missions/missionStatus";
import type { TabState } from "../store/workbenchStore";

const tab: TabState = { kind: "terminal", id: "tab-1", title: "배포 로그", root: null };
const noop = () => undefined;

function render(renaming: boolean): string {
  return renderToString(
    <TabItem
      tab={tab}
      index={0}
      tabCount={1}
      active
      renaming={renaming}
      onSelect={noop}
      onClose={noop}
      onCloseAll={noop}
      onRenameStart={noop}
      onRenameCommit={noop}
      onRenameCancel={noop}
    />,
  );
}

describe("TabItem", () => {
  it("shows the name and a close button when not renaming", () => {
    const html = render(false);
    expect(html).toContain("배포 로그");
    expect(html).toContain("tab-close");
    expect(html).not.toContain("tab-title-input");
    expect(html).toContain('tabindex="0"');
  });

  it("swaps the title for a labelled input seeded with the current name", () => {
    const html = render(true);
    expect(html).toContain("tab-title-input");
    expect(html).toContain(`aria-label="${t("app.renameTab")}"`);
    expect(html).toContain('value="배포 로그"');
    // 편집 중에는 닫기 버튼이 자리를 비운다(오타 한 번에 탭이 닫히지 않게).
    expect(html).not.toContain("tab-close");
    // 편집 중인 탭은 탭 순회에서 빠진다(포커스는 input이 가진다).
    expect(html).toContain('tabindex="-1"');
  });

  it("falls back to the numbered default name", () => {
    const html = renderToString(
      <TabItem
        tab={{ kind: "terminal", id: "tab-2", title: null as unknown as string, root: null }}
        index={2}
        tabCount={3}
        active={false}
        renaming={false}
        onSelect={noop}
        onClose={noop}
        onCloseAll={noop}
        onRenameStart={noop}
        onRenameCommit={noop}
        onRenameCancel={noop}
      />,
    );
    expect(html).toContain(t("app.tabTitle", { index: 3 }));
  });
});

/**
 * 타이틀 앞 활동 마커: 에이전트가 있는 탭은 자체 보고/출력 활동으로 판정한
 * 상태를 스피너·점으로 보이고, 그때는 출력 활동 점(tab-running-dot)을 숨긴다.
 */
describe("TabItem agent activity marker", () => {
  function renderWith(overrides: Partial<TabItemProps>): string {
    return renderToString(
      <TabItem
        tab={tab}
        index={0}
        tabCount={1}
        active
        renaming={false}
        onSelect={noop}
        onClose={noop}
        onCloseAll={noop}
        onRenameStart={noop}
        onRenameCommit={noop}
        onRenameCancel={noop}
        {...overrides}
      />,
    );
  }

  it("작업 중이면 타이틀 앞에 숨 쉬는 점을 두고 출력 활동 점은 숨긴다", () => {
    const html = renderWith({ agentId: "claude", agentActivity: "working", running: true });
    expect(html).toContain('class="tab-agent-marker working"');
    expect(html).toContain(t("terminal.agent.working", { name: t("terminal.agent.claude") }));
    expect(html.indexOf("tab-agent-marker")).toBeLessThan(html.indexOf("tab-title"));
    expect(html).not.toContain("tab-running-dot");
  });

  it("확인 대기면 대기 마커를 둔다", () => {
    const html = renderWith({ agentId: "codex", agentActivity: "waiting" });
    expect(html).toContain('class="tab-agent-marker waiting"');
    expect(html).toContain(t("terminal.agent.waiting", { name: t("terminal.agent.codex") }));
  });

  it("idle이면 마커도 출력 활동 점도 없다", () => {
    const html = renderWith({ agentId: "claude", agentActivity: "idle", running: true });
    expect(html).not.toContain("tab-agent-marker");
    expect(html).not.toContain("tab-running-dot");
  });

  it("에이전트가 없으면 기존 출력 활동 점 그대로", () => {
    const html = renderWith({ running: true });
    expect(html).toContain("tab-running-dot");
    expect(html).toContain(t("app.tabRunning"));
    expect(html).not.toContain("tab-agent-marker");
  });
});

/** 끌어 놓기(04-ui §2-5): 끌기 세션은 탭 바에서 data-tab-id로 탭 자리를 읽는다. */
describe("TabItem drag handle", () => {
  it("marks the tab with its id for the drag session's hit test", () => {
    const html = renderToString(
      <TabItem
        tab={tab}
        index={0}
        tabCount={1}
        active
        renaming={false}
        onSelect={noop}
        onClose={noop}
        onCloseAll={noop}
        onRenameStart={noop}
        onRenameCommit={noop}
        onRenameCancel={noop}
      />,
    );
    expect(html).toContain('data-tab-id="tab-1"');
  });
});

/** mission 탭 배지: 상태 우선 한 개, 결정 필요는 경고 스타일, 탭마다 낭독 영역을 두지 않는다. */
describe("TabItem mission badge", () => {
  const missionTab: TabState = { kind: "mission", id: "tab-m", title: "로그인", missionId: "m-1" };

  function renderBadge(badge: MissionTabBadge | null): string {
    return renderToString(
      <TabItem
        tab={missionTab}
        index={1}
        tabCount={2}
        active={false}
        renaming={false}
        titleText="로그인"
        missionBadge={badge}
        onSelect={noop}
        onClose={noop}
        onCloseAll={noop}
        onRenameStart={noop}
        onRenameCommit={noop}
        onRenameCancel={noop}
      />,
    );
  }

  it.each<[MissionTabBadge, string]>([
    [{ kind: "decision", count: 2 }, "missions.tabBadge.decision"],
    [{ kind: "acceptance" }, "missions.tabBadge.acceptance"],
    [{ kind: "failed" }, "missions.tabBadge.failed"],
    [{ kind: "running", count: 3 }, "missions.tabBadge.runningCount"],
    [{ kind: "running", count: null }, "missions.tabBadge.running"],
    [{ kind: "done" }, "missions.tabBadge.done"],
  ])("%j 배지는 상태 문구와 상태 클래스로 그린다", (badge, key) => {
    const html = renderBadge(badge);
    const count = "count" in badge && badge.count !== null ? badge.count : undefined;
    expect(html).toContain(t(key, count === undefined ? undefined : { count }));
    expect(html).toContain(`tab-badge tab-badge-mission tab-badge-mission-${badge.kind}`);
    expect(missionTabBadgeText(t, badge)).toBe(t(key, count === undefined ? undefined : { count }));
  });

  it("배지는 role=status가 아니다(탭마다 갱신이 낭독되지 않게)", () => {
    const html = renderBadge({ kind: "decision", count: 1 });
    const start = html.lastIndexOf("<span", html.indexOf("tab-badge-mission"));
    const badgeTag = html.slice(start, html.indexOf(">", start));
    expect(badgeTag).toContain("tab-badge-mission-decision");
    expect(badgeTag).not.toContain("role=");
  });

  it("보일 상태가 없으면 배지를 그리지 않는다", () => {
    expect(renderBadge(null)).not.toContain("tab-badge-mission");
  });
});

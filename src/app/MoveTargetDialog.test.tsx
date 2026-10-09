/**
 * "어느 탭으로?" 목록 계약(04-ui §2-5).
 *
 * 이 환경에는 DOM이 없어 클릭·키보드를 흉내 낼 수 없으므로, 화면이 그리는
 * 것과 같은 순수 함수(moveTargets)로 목록 규칙을 고정한다: 자기 자신은
 * 빠지고, 상한에 걸리는 탭은 지워지는 대신 이유와 함께 남고, "새 탭"은
 * 옮길 이유가 있을 때만 있다.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ControllerContext } from "./controllerContext";
import { MoveTargetDialog } from "./MoveTargetDialog";
import { moveTargets, type MoveTargetModal } from "./moveTargets";
import { gridLayout, makeLeaf, MAX_PANES_PER_TAB } from "../features/terminal/splitTree";
import { t } from "../i18n";
import type { SessionController } from "../features/terminal/sessionController";
import { useWorkbenchStore, type TabState } from "../store/workbenchStore";

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

let seq = 0;
const nextId = () => `split-${(seq += 1)}`;

function tab(id: string, title: string, panes: number): TabState {
  const leaves = Array.from({ length: panes }, (_, i) => makeLeaf(`${id}-leaf-${i}`, `view-${id}-${i}`));
  return { kind: "terminal", id, title, root: gridLayout(leaves, nextId) };
}

const tabs = [tab("tab-1", "배포 로그", 2), tab("tab-2", "웹", 3), tab("tab-3", "가득", MAX_PANES_PER_TAB)];

/** pane 트리가 없는 mission 탭(05 §2). */
const missionTab: TabState = { kind: "mission", id: "mission-1", title: "로그인 기능", missionId: "m-1" };

describe("moveTargets — mission 탭(05 §2)", () => {
  it("mission 탭은 pane을 받지 않으므로 이동·합치기 대상 목록에 없다", () => {
    const withMission = [tabs[0], missionTab, tabs[1]];
    const moveRows = moveTargets({ tabs: withMission }, { kind: "move-pane", leafId: "tab-1-leaf-0" });
    expect(moveRows.map((row) => row.tabId)).toEqual([null, "tab-2"]);
    const mergeRows = moveTargets({ tabs: withMission }, { kind: "merge-tab", tabId: "tab-1" });
    expect(mergeRows.map((row) => row.tabId)).toEqual(["tab-2"]);
  });

  it("mission 탭을 합치려 하면 고를 대상이 없다", () => {
    expect(moveTargets({ tabs: [missionTab, ...tabs] }, { kind: "merge-tab", tabId: "mission-1" })).toEqual([]);
  });
});

describe("moveTargets — pane 이동", () => {
  const modal = { kind: "move-pane", leafId: "tab-1-leaf-0" } as const;

  it("출발 탭을 빼고 나머지 탭을 창 수와 함께 낸다", () => {
    const rows = moveTargets({ tabs }, modal);
    expect(rows.map((row) => row.tabId)).toEqual([null, "tab-2", "tab-3"]);
    expect(rows[1]).toMatchObject({ kind: "tab", title: "웹", paneCount: 3, disabled: false, reason: null });
  });

  it("창이 여럿인 탭에서 왔을 때만 '새 탭' 행이 맨 앞에 붙는다", () => {
    const rows = moveTargets({ tabs }, modal);
    expect(rows[0]).toMatchObject({ kind: "new", tabId: null, paneCount: null, disabled: false });

    // 이미 탭 하나를 혼자 쓰는 창에게 "새 탭"은 제자리걸음이다.
    const alone = [tab("solo", "혼자", 1), tab("other", "다른", 1)];
    const soloRows = moveTargets({ tabs: alone }, { kind: "move-pane", leafId: "solo-leaf-0" });
    expect(soloRows.map((row) => row.kind)).toEqual(["tab"]);
  });

  it("가득 찬 탭은 지우지 않고 이유와 함께 흐리게 둔다", () => {
    const rows = moveTargets({ tabs }, modal);
    const full = rows.find((row) => row.tabId === "tab-3");
    expect(full).toMatchObject({
      disabled: true,
      reason: { key: "moveTarget.full", max: MAX_PANES_PER_TAB },
    });
  });

  it("탭이 하나뿐이고 그 창이 혼자면 고를 것이 없다", () => {
    const rows = moveTargets({ tabs: [tab("only", "하나", 1)] }, { kind: "move-pane", leafId: "only-leaf-0" });
    expect(rows).toEqual([]);
  });

  it("탭 이름은 내부 런처(env …)를 벗겨 낸 표시 이름이다", () => {
    const launcher = tab("tab-9", "/usr/bin/env -u NO_COLOR -u FORCE_COLOR -u CLICOLOR -u CLICOLOR_FORCE /bin/zsh -l", 1);
    const rows = moveTargets({ tabs: [tab("src", "출발", 2), launcher] }, { kind: "move-pane", leafId: "src-leaf-0" });
    expect(rows.find((row) => row.tabId === "tab-9")?.title).toBe("zsh");
  });
});

describe("moveTargets — 탭 합치기", () => {
  it("합친 뒤 상한을 넘으면 합계와 함께 흐리게 둔다", () => {
    const rows = moveTargets({ tabs }, { kind: "merge-tab", tabId: "tab-2" });
    // 새 탭 행은 합치기에 없다 — 합칠 대상은 언제나 기존 탭이다.
    expect(rows.map((row) => row.tabId)).toEqual(["tab-1", "tab-3"]);
    expect(rows[0]).toMatchObject({ disabled: false, reason: null, paneCount: 2 });
    expect(rows[1]).toMatchObject({
      disabled: true,
      reason: { key: "moveTarget.tooMany", n: 3 + MAX_PANES_PER_TAB, max: MAX_PANES_PER_TAB },
    });
  });

  it("합칠 다른 탭이 없으면 빈 목록", () => {
    expect(moveTargets({ tabs: [tab("only", "하나", 2)] }, { kind: "merge-tab", tabId: "only" })).toEqual([]);
  });

  it("정확히 상한에 맞아떨어지면 고를 수 있다(> 상한일 때만 막는다)", () => {
    const pair = [tab("a", "A", 5), tab("b", "B", 3)];
    const rows = moveTargets({ tabs: pair }, { kind: "merge-tab", tabId: "a" });
    expect(rows[0]).toMatchObject({ tabId: "b", disabled: false, reason: null });
  });
});

function render(modal: MoveTargetModal, list: TabState[]): string {
  useWorkbenchStore.setState({ tabs: list });
  return renderToStaticMarkup(
    <ControllerContext.Provider value={{} as SessionController}>
      <MoveTargetDialog modal={modal} />
    </ControllerContext.Provider>,
  );
}

afterEach(() => useWorkbenchStore.setState({ tabs: [] }));

describe("MoveTargetDialog markup", () => {
  it("목록은 listbox이고, 첫 번째로 고를 수 있는 행이 미리 선택돼 있다", () => {
    const html = render({ kind: "move-pane", leafId: "tab-1-leaf-0" }, tabs);
    expect(html).toContain(t("moveTarget.paneTitle"));
    expect(html).toContain('role="listbox"');
    expect(html).toContain(`aria-label="${t("moveTarget.aria")}"`);
    expect(html).toContain('aria-activedescendant="move-target-option-0"');
    expect(html).toContain(t("moveTarget.newTab"));
    expect(html).toContain(t("moveTarget.paneCount", { n: 3 }));
    expect(html).toContain(t("moveTarget.cancel"));
  });

  it("고를 수 없는 행은 이유를 함께 적고 aria-disabled로 알린다", () => {
    const html = render({ kind: "move-pane", leafId: "tab-1-leaf-0" }, tabs);
    expect(html).toContain(t("moveTarget.full", { max: MAX_PANES_PER_TAB }));
    expect(html).toContain('aria-disabled="true"');
    expect(html).toContain("move-target-row disabled");
  });

  it("합칠 탭이 없으면 목록 대신 안내만 낸다", () => {
    const html = render({ kind: "merge-tab", tabId: "only" }, [tab("only", "하나", 2)]);
    expect(html).toContain(t("moveTarget.tabTitle"));
    expect(html).toContain(t("moveTarget.empty"));
    expect(html).not.toContain('role="listbox"');
  });
});

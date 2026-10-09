/**
 * pane 컨텍스트 메뉴 모델: 구획 순서, 상황별 표시·비활성, 글꼴 줄, 구분선 규칙.
 */

import { describe, expect, it } from "vitest";
import type { ContextMenuEntry, ContextMenuItem, ContextMenuRow } from "../../app/contextMenuTypes";
import type { PanePhase } from "../monitor/statusStrings";
import { t } from "../../i18n";
import { MAX_FONT_SIZE, MIN_FONT_SIZE } from "./zoom";
import {
  buildPaneMenu,
  isPaneMenuAction,
  PANE_MENU_ACTIONS,
  type PaneMenuContext,
} from "./paneContextMenu";
import { missionEntryState } from "../missions/capability";

function ctx(overrides: Partial<PaneMenuContext> = {}): PaneMenuContext {
  return {
    platform: "darwin",
    phase: "live",
    hasSelection: false,
    link: null,
    cwd: null,
    project: null,
    missionEntry: { hidden: false, enabled: true, reasonKey: null },
    broadcast: false,
    canSplit: true,
    canDetach: true,
    canMoveToTab: true,
    fontSize: 13,
    baseFontSize: 13,
    resumeCommand: null,
    backgroundColor: null,
    ...overrides,
  };
}

/** 구분선은 "|", 줄은 "row:<id>"로 적은 순서. */
function shape(entries: ContextMenuEntry[]): string[] {
  return entries.map((entry) =>
    entry.kind === "separator" ? "|" : entry.kind === "row" ? `row:${entry.id}` : entry.id,
  );
}

function allItems(entries: ContextMenuEntry[]): ContextMenuItem[] {
  return entries.flatMap((entry) =>
    entry.kind === "item" ? [entry] : entry.kind === "row" ? entry.items : [],
  );
}

function item(entries: ContextMenuEntry[], id: string): ContextMenuItem | undefined {
  return allItems(entries).find((candidate) => candidate.id === id);
}

function fontRow(entries: ContextMenuEntry[]): ContextMenuRow {
  const row = entries.find((entry): entry is ContextMenuRow => entry.kind === "row" && entry.id === "font-size");
  if (!row) throw new Error("font-size row missing");
  return row;
}

describe("buildPaneMenu — 구획과 순서", () => {
  it("평범한 live pane: 클립보드 · 보기 · 배치 · 탭 재배치 · 닫기", () => {
    expect(shape(buildPaneMenu(ctx()))).toEqual([
      "copy",
      "paste",
      "select-all",
      "|",
      "find",
      "clear",
      "row:font-size",
      "bg-color",
      "bg-reset",
      "|",
      "split-row",
      "split-column",
      "broadcast",
      "|",
      "detach",
      "move-to",
      "|",
      "close",
    ]);
  });

  it("링크 위에서 열면 링크 구획이 맨 위에 붙는다", () => {
    const url = "https://example.com/docs";
    const entries = buildPaneMenu(ctx({ link: url }));
    expect(shape(entries).slice(0, 3)).toEqual(["open-link", "copy-link", "|"]);
    expect(item(entries, "open-link")?.detail).toBe(url);
    expect(item(entries, "copy-link")?.detail).toBe(url);
    expect(item(buildPaneMenu(ctx()), "open-link")).toBeUndefined();
  });

  it("경로·재개 명령은 값이 있을 때만, 전체 값을 detail로", () => {
    const entries = buildPaneMenu(ctx({ cwd: "/work/app", resumeCommand: "claude --resume abc" }));
    expect(shape(entries).slice(-6)).toEqual(["|", "copy-cwd", "copy-resume", "new-mission-here", "|", "close"]);
    expect(item(entries, "copy-cwd")?.detail).toBe("/work/app");
    expect(item(entries, "copy-resume")?.detail).toBe("claude --resume abc");
    const cwdOnly = buildPaneMenu(ctx({ cwd: "/work/app" }));
    expect(item(cwdOnly, "copy-resume")).toBeUndefined();
    expect(item(buildPaneMenu(ctx({ resumeCommand: "codex resume abc" })), "copy-cwd")).toBeUndefined();
  });

  it("이 폴더에서 AI 작업은 경로가 있을 때만, 저장소 최상위(없으면 cwd)를 detail로", () => {
    const inRepo = buildPaneMenu(ctx({ cwd: "/work/app/src", project: "/work/app" }));
    expect(item(inRepo, "new-mission-here")).toMatchObject({
      label: t("terminal.contextMenu.newMissionHere"),
      detail: "/work/app",
      disabled: false,
    });
    expect(shape(inRepo).slice(-4)).toEqual(["copy-cwd", "new-mission-here", "|", "close"]);
    expect(item(buildPaneMenu(ctx({ cwd: "/work/plain" })), "new-mission-here")?.detail).toBe("/work/plain");
    expect(item(buildPaneMenu(ctx()), "new-mission-here")).toBeUndefined();
    expect(item(buildPaneMenu(ctx({ project: "/work/app" })), "new-mission-here")).toBeUndefined();
  });

  it("다시 시작은 종료·실패한 pane에만, 닫기는 경고색으로 맨 끝", () => {
    const phases: PanePhase[] = ["starting", "replaying", "live", "detached", "exited", "failed"];
    for (const phase of phases) {
      const entries = buildPaneMenu(ctx({ phase }));
      expect(item(entries, "restart") !== undefined, phase).toBe(phase === "exited" || phase === "failed");
      const last = entries[entries.length - 1];
      expect(last.kind === "item" && last.id).toBe("close");
      expect(item(entries, "close")?.danger).toBe(true);
    }
    expect(shape(buildPaneMenu(ctx({ phase: "exited" }))).slice(-2)).toEqual(["restart", "close"]);
  });
});

describe("buildPaneMenu — 상황별 비활성", () => {
  it("복사는 선택이 있을 때만", () => {
    expect(item(buildPaneMenu(ctx()), "copy")?.disabled).toBe(true);
    expect(item(buildPaneMenu(ctx({ hasSelection: true })), "copy")?.disabled).toBe(false);
  });

  it("붙여넣기는 live pane에서만", () => {
    for (const phase of ["starting", "replaying", "detached", "exited", "failed"] as const) {
      expect(item(buildPaneMenu(ctx({ phase })), "paste")?.disabled, phase).toBe(true);
    }
    expect(item(buildPaneMenu(ctx()), "paste")?.disabled).toBe(false);
  });

  it("화면 지우기는 시작·재생 중에는 막고, 영향 범위를 detail로 알린다", () => {
    expect(item(buildPaneMenu(ctx({ phase: "starting" })), "clear")?.disabled).toBe(true);
    expect(item(buildPaneMenu(ctx({ phase: "replaying" })), "clear")?.disabled).toBe(true);
    expect(item(buildPaneMenu(ctx({ phase: "exited" })), "clear")?.disabled).toBe(false);
    expect(item(buildPaneMenu(ctx()), "clear")?.detail).toBe(t("terminal.contextMenu.clearDetail"));
  });

  it("분할은 탭 상한이면 막는다", () => {
    const entries = buildPaneMenu(ctx({ canSplit: false }));
    expect(item(entries, "split-row")?.disabled).toBe(true);
    expect(item(entries, "split-column")?.disabled).toBe(true);
    expect(item(buildPaneMenu(ctx()), "split-row")?.disabled).toBe(false);
  });

  it("동시 입력은 현재 상태를 체크로 보인다", () => {
    expect(item(buildPaneMenu(ctx()), "broadcast")?.checked).toBe(false);
    expect(item(buildPaneMenu(ctx({ broadcast: true })), "broadcast")?.checked).toBe(true);
  });

  it("새 탭으로 분리·다른 탭으로 이동은 숨기지 않고 막기만 한다", () => {
    const alone = buildPaneMenu(ctx({ canDetach: false }));
    expect(item(alone, "detach")?.disabled).toBe(true);
    expect(item(alone, "detach")).toBeDefined();
    expect(item(buildPaneMenu(ctx()), "detach")?.disabled).toBe(false);

    const onlyTab = buildPaneMenu(ctx({ canMoveToTab: false }));
    expect(item(onlyTab, "move-to")?.disabled).toBe(true);
    expect(item(onlyTab, "move-to")).toBeDefined();
    expect(item(buildPaneMenu(ctx()), "move-to")?.disabled).toBe(false);
  });

  it("AI 작업 프로토콜이 없으면(개발 빌드) 이 폴더에서 AI 작업을 흐리게 두고 사유를 툴팁으로", () => {
    const preview = missionEntryState(null, false);
    const locked = item(buildPaneMenu(ctx({ cwd: "/work/app", missionEntry: preview })), "new-mission-here");
    expect(locked).toBeDefined();
    expect(locked?.disabled).toBe(true);
    expect(locked?.detail).toBe(t("missions.newMission.unavailable"));
  });

  it("프로덕션 빌드에서 프로토콜이 없으면 이 폴더에서 AI 작업을 숨긴다", () => {
    const entries = buildPaneMenu(ctx({ cwd: "/work/app", missionEntry: missionEntryState(null, true) }));
    expect(item(entries, "new-mission-here")).toBeUndefined();
    expect(item(entries, "copy-cwd")).toBeDefined();
  });

  it("데몬 프로토콜이 앱보다 새로우면 흐리게 두고 앱 업데이트 사유를 보인다(빌드 종류와 무관)", () => {
    for (const production of [false, true]) {
      const update = item(buildPaneMenu(ctx({ cwd: "/work/app", missionEntry: missionEntryState(99, production) })), "new-mission-here");
      expect(update?.disabled).toBe(true);
      expect(update?.detail).toBe(t("missions.newMission.updateApp"));
    }
    const ready = item(buildPaneMenu(ctx({ cwd: "/work/app", missionEntry: missionEntryState(1, true) })), "new-mission-here");
    expect(ready?.disabled).toBe(false);
    expect(ready?.detail).toBe("/work/app");
  });
});

describe("buildPaneMenu — 글꼴 크기 줄", () => {
  it("기본 대비 백분율과 − 기본 + 순서", () => {
    const row = fontRow(buildPaneMenu(ctx({ fontSize: 20, baseFontSize: 13 })));
    expect(row.label).toBe(t("terminal.contextMenu.fontSize", { percent: 154 }));
    expect(row.items.map((entry) => entry.id)).toEqual(["zoom-out", "zoom-reset", "zoom-in"]);
    expect(row.items[0]).toMatchObject({ label: "−", ariaLabel: t("terminal.contextMenu.zoomOut") });
    expect(row.items[2]).toMatchObject({ label: "+", ariaLabel: t("terminal.contextMenu.zoomIn") });
    expect(fontRow(buildPaneMenu(ctx({ baseFontSize: 0 }))).label).toBe(
      t("terminal.contextMenu.fontSize", { percent: 100 }),
    );
  });

  it("상·하한과 기본 크기에서 해당 버튼을 막는다", () => {
    const atBase = fontRow(buildPaneMenu(ctx())).items;
    expect(atBase.map((entry) => entry.disabled)).toEqual([false, true, false]);
    const atMin = fontRow(buildPaneMenu(ctx({ fontSize: MIN_FONT_SIZE }))).items;
    expect(atMin.map((entry) => entry.disabled)).toEqual([true, false, false]);
    const atMax = fontRow(buildPaneMenu(ctx({ fontSize: MAX_FONT_SIZE }))).items;
    expect(atMax.map((entry) => entry.disabled)).toEqual([false, false, true]);
  });
});

describe("buildPaneMenu — 단축키 표시", () => {
  it("플랫폼 기본표와 사용자 재정의를 따른다", () => {
    const mac = buildPaneMenu(ctx());
    expect(item(mac, "copy")?.shortcut).toBe("⌘C");
    expect(item(mac, "split-column")?.shortcut).toBe("⇧⌘D");
    expect(item(mac, "close")?.shortcut).toBe("⌘W");
    expect(item(mac, "select-all")?.shortcut).toBeUndefined();
    const win = buildPaneMenu(ctx({ platform: "windows" }));
    expect(item(win, "split-column")?.shortcut).toBe("Ctrl+Shift+E");
    const passed = buildPaneMenu(ctx({ overrides: { copy: "pass" } }));
    expect(item(passed, "copy")?.shortcut).toBeNull();
  });
});

describe("buildPaneMenu — 구분선 규칙", () => {
  it("어떤 조합에서도 앞·뒤·연속 구분선이 없고 id가 겹치지 않는다", () => {
    const phases: PanePhase[] = ["starting", "replaying", "live", "detached", "exited", "failed"];
    for (const phase of phases) {
      for (const link of [null, "https://example.com"]) {
        for (const cwd of [null, "/w"]) {
          for (const resumeCommand of [null, "claude --resume x"]) {
            const missionEntry = { hidden: false, enabled: resumeCommand === null, reasonKey: resumeCommand === null ? null : "missions.newMission.unavailable" };
            const entries = buildPaneMenu(ctx({ phase, link, cwd, resumeCommand, missionEntry }));
            const label = JSON.stringify({ phase, link, cwd, resumeCommand, missionEntry });
            expect(entries[0].kind, label).not.toBe("separator");
            expect(entries[entries.length - 1].kind, label).not.toBe("separator");
            entries.forEach((entry, index) => {
              if (entry.kind === "separator" && index > 0) {
                expect(entries[index - 1].kind, label).not.toBe("separator");
              }
            });
            const ids = entries.map((entry) => entry.id);
            expect(new Set(ids).size, label).toBe(ids.length);
          }
        }
      }
    }
  });
});

describe("isPaneMenuAction", () => {
  it("모든 항목 id는 pane 동작이고, 구분선·줄 id는 아니다", () => {
    const everything = buildPaneMenu(
      ctx({ phase: "exited", link: "https://example.com", cwd: "/w", resumeCommand: "claude --resume x" }),
    );
    const ids = allItems(everything).map((entry) => entry.id);
    expect(new Set(ids)).toEqual(new Set(PANE_MENU_ACTIONS));
    expect(ids.every(isPaneMenuAction)).toBe(true);
    expect(isPaneMenuAction("font-size")).toBe(false);
    expect(isPaneMenuAction("sep-1")).toBe(false);
  });
});

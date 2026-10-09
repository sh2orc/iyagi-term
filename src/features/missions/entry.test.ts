/**
 * 새 AI 작업 진입점: 보고 있는 터미널의 저장소 후보(git 최상위 → cwd)를 고르고,
 * 대화상자를 그 경로로 연다.
 */

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { makeLeaf, type SplitNode } from "../terminal/splitTree";
import { useWorkbenchStore, type PaneMeta } from "../../store/workbenchStore";
import { focusedRepositoryHint, openMissionCreate, paneRepositoryHint } from "./entry";

function row(ids: readonly string[]): SplitNode {
  const head = ids[0] ?? "leaf";
  const leaf = makeLeaf(head, `view-${head}`);
  if (ids.length <= 1) return leaf;
  return { kind: "split", id: `split-${head}`, axis: "row", ratio: 0.5, first: leaf, second: row(ids.slice(1)) };
}

function pane(leafId: string, patch: Partial<PaneMeta> = {}): PaneMeta {
  return {
    leafId,
    viewId: `view-${leafId}`,
    sessionId: null,
    workloadId: null,
    title: leafId,
    cwd: null,
    phase: "live",
    error: null,
    usage: null,
    flowBlocked: false,
    ...patch,
  };
}

beforeEach(() => {
  useWorkbenchStore.setState({
    modal: null, missionCreate: null,
    tabs: [
      { kind: "terminal", id: "t1", title: "one", root: row(["a", "b"]) },
      { kind: "terminal", id: "t2", title: "two", root: row(["c"]) },
      { kind: "mission", id: "m1", title: "mission", missionId: "mission-1" },
    ],
    activeTabId: "t1",
    focusedLeafId: "a",
    panes: {
      a: pane("a", { cwd: "/work/app/src", project: "/work/app" }),
      b: pane("b", { cwd: "/work/other" }),
      c: pane("c", { cwd: "/work/third" }),
    },
  });
});

afterEach(() => {
  useWorkbenchStore.setState({ modal: null, missionCreate: null, tabs: [], activeTabId: null, focusedLeafId: null, panes: {} });
});

describe("paneRepositoryHint", () => {
  it("prefers the git top-level, then the cwd, and treats empty values as missing", () => {
    expect(paneRepositoryHint({ cwd: "/w/app/src", project: "/w/app" })).toBe("/w/app");
    expect(paneRepositoryHint({ cwd: "/w/app/src", project: null })).toBe("/w/app/src");
    expect(paneRepositoryHint({ cwd: "/w/app/src" })).toBe("/w/app/src");
    expect(paneRepositoryHint({ cwd: "", project: "" })).toBeNull();
    expect(paneRepositoryHint({ cwd: null, project: null })).toBeNull();
    expect(paneRepositoryHint(undefined)).toBeNull();
  });
});

describe("focusedRepositoryHint", () => {
  it("uses the focused pane of the terminal tab being viewed", () => {
    expect(focusedRepositoryHint(useWorkbenchStore.getState())).toBe("/work/app");
    useWorkbenchStore.setState({ focusedLeafId: "b" });
    expect(focusedRepositoryHint(useWorkbenchStore.getState())).toBe("/work/other");
  });

  it("returns null while a mission tab is visible", () => {
    useWorkbenchStore.setState({ activeTabId: "m1", focusedLeafId: null });
    expect(focusedRepositoryHint(useWorkbenchStore.getState())).toBeNull();
  });

  it("ignores a focus left behind in another tab", () => {
    useWorkbenchStore.setState({ activeTabId: "t2", focusedLeafId: "a" });
    expect(focusedRepositoryHint(useWorkbenchStore.getState())).toBeNull();
  });

  it("returns null without a focused pane or its metadata", () => {
    useWorkbenchStore.setState({ focusedLeafId: null });
    expect(focusedRepositoryHint(useWorkbenchStore.getState())).toBeNull();
    useWorkbenchStore.setState({ focusedLeafId: "b", panes: {} });
    expect(focusedRepositoryHint(useWorkbenchStore.getState())).toBeNull();
  });
});

describe("openMissionCreate", () => {
  it("opens the sidebar without blocking or switching the terminal", () => {
    openMissionCreate("/work/app");
    expect(useWorkbenchStore.getState().modal).toBeNull();
    expect(useWorkbenchStore.getState().activeTabId).toBe("t1");
    expect(useWorkbenchStore.getState().missionCreate).toEqual({ kind: "mission-create", repositoryPath: "/work/app" });
  });

  it("opens an empty dialog without a path", () => {
    openMissionCreate();
    expect(useWorkbenchStore.getState().missionCreate).toEqual({ kind: "mission-create", repositoryPath: null });
    openMissionCreate("   ");
    expect(useWorkbenchStore.getState().missionCreate).toEqual({ kind: "mission-create", repositoryPath: null });
  });

  it("carries the follow-up source mission only when given (계약 E)", () => {
    openMissionCreate("/work/app", { goal: "이어서", followUpOf: "mission-9" });
    expect(useWorkbenchStore.getState().missionCreate).toStrictEqual({
      kind: "mission-create",
      repositoryPath: "/work/app",
      goal: "이어서",
      followUpOf: "mission-9",
    });
    openMissionCreate("/work/app", { followUpOf: "mission-9" });
    expect(useWorkbenchStore.getState().missionCreate).toStrictEqual({ kind: "mission-create", repositoryPath: "/work/app", followUpOf: "mission-9" });
    openMissionCreate("/work/app", { goal: null, followUpOf: "  " });
    expect(useWorkbenchStore.getState().missionCreate).toStrictEqual({ kind: "mission-create", repositoryPath: "/work/app" });
  });
});

/**
 * 배치 편집의 "실행 중인 터미널" 목록(04-ui §2-6): 배치됨은 탭·화면 순서, 배치 안 됨은 어느
 * pane에도 붙지 않은 살아 있는 세션. 같은 터미널이 두 목록에 겹치지 않는다.
 */

import { describe, expect, it } from "vitest";
import { layoutRoster } from "../src/features/terminal/layoutRoster";
import { makeLeaf, type SplitNode } from "../src/features/terminal/splitTree";
import type { PaneMeta, TabState } from "../src/store/workbenchStore";
import type { WorkloadSummary } from "../src/generated/WorkloadSummary";
import { t } from "../src/i18n";

const leaf = (id: string) => makeLeaf(id, `v-${id}`, `s-${id}`);
const row = (first: SplitNode, second: SplitNode, id: string): SplitNode => ({
  kind: "split", id, axis: "row", ratio: 0.5, first, second,
});

function pane(id: string, extra: Partial<PaneMeta> = {}): PaneMeta {
  return {
    leafId: id, viewId: `v-${id}`, sessionId: `s-${id}`, workloadId: `w-${id}`, title: `title-${id}`,
    cwd: `/work/${id}`, phase: "live", error: null, usage: null, flowBlocked: false, ...extra,
  };
}

function workload(id: string, extra: Partial<WorkloadSummary> = {}): WorkloadSummary {
  return {
    workload_id: `w-${id}`, session_id: `s-${id}`, mode: "shell", state: "RUNNING", priority: "normal",
    title: `workload-${id}`, cwd: `/work/${id}`, program: "zsh", reservation_bytes: "0", cpu_slots: 1,
    enforcement: "none", root_exited: false, cancel_requested: false, connection: "attached", ...extra,
  } as unknown as WorkloadSummary;
}

describe("layoutRoster", () => {
  const tabs: TabState[] = [
    { kind: "terminal", id: "t1", title: "api", root: row(leaf("a"), leaf("b"), "sp1") },
    { kind: "mission", id: "m1", title: "미션", missionId: "m" },
    { kind: "terminal", id: "t2", title: "", root: leaf("c") },
  ];
  const panes = { a: pane("a"), b: pane("b", { phase: "exited" }), c: pane("c") };

  it("lists placed panes in tab order and visual order, with their group name", () => {
    const roster = layoutRoster({ tabs, panes, workloads: [], workloadMemory: {} });
    expect(roster.placed.map((item) => [item.leafId, item.tabId, item.tabTitle])).toEqual([
      ["a", "t1", "api"],
      ["b", "t1", "api"],
      ["c", "t2", t("app.tabTitle", { index: 3 })],
    ]);
    expect(roster.placed[0]).toMatchObject({ title: "title-a", cwd: "/work/a", phase: "live" });
    expect(roster.placed[1].phase).toBe("exited");
    expect(roster.unplaced).toEqual([]);
  });

  it("lists live sessions no pane holds as unplaced — never queued, finished, or already attached ones", () => {
    const roster = layoutRoster({
      tabs,
      panes,
      workloads: [
        workload("a"), // 세션이 pane에 붙어 있다
        workload("x"), // 창만 닫기로 떨어져 나갔다
        workload("y", { state: "STARTING" }), // 시작 중에도 붙일 수 있다
        workload("q", { state: "QUEUED", session_id: null }), // 세션이 아직 없다
        workload("f", { state: "SUCCEEDED" }), // 끝났다
        workload("z", { session_id: "s-other", workload_id: "w-c" }), // 같은 작업이 pane c에 붙어 있다
      ],
      workloadMemory: {},
    });
    expect(roster.unplaced.map((item) => item.sessionId)).toEqual(["s-x", "s-y"]);
    expect(roster.unplaced[0]).toMatchObject({ workloadId: "w-x", title: "workload-x", cwd: "/work/x", state: "RUNNING" });
  });

  it("names an unplaced terminal by the title it reported before its window was closed", () => {
    const roster = layoutRoster({
      tabs,
      panes,
      workloads: [workload("x"), workload("y")],
      workloadMemory: { "w-x": { title: "✳ Fix login bug", agent: "claude" } },
    });
    expect(roster.unplaced.map((item) => [item.sessionId, item.title])).toEqual([
      ["s-x", "✳ Fix login bug"],
      ["s-y", "workload-y"],
    ]);
  });
});

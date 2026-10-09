import { describe, expect, it, vi } from "vitest";
import { findLeaf, layoutShape, makeLeaf, split, type SplitNode } from "../src/features/terminal/splitTree";
import { paneLimitText } from "../src/features/monitor/statusStrings";
import { planRegroup } from "../src/features/terminal/regroup";
import {
  FINISHED_WORKLOADS_RETAINED,
  FLASH_TOAST_MS,
  MAX_MISSION_TABS,
  flashToast,
  pruneFinishedWorkloads,
  selectActiveRoot,
  selectHost,
  useWorkbenchStore,
  type WorkbenchState,
} from "../src/store/workbenchStore";
import type { WorkloadSummary } from "../src/generated/WorkloadSummary";
import type { AgentResumeInfo } from "../src/features/agentSessions/types";
import { t } from "../src/i18n";
import { SessionController } from "../src/features/terminal/sessionController";
import type { DaemonClient } from "../src/features/daemon/client";
import type { TerminalRegistry } from "../src/features/terminal/registry";

function resetStore() {
  useWorkbenchStore.setState((s) => ({
    tabs: [],
    activeTabId: null,
    focusedLeafId: null,
    panes: {},
    workloads: [],
    workloadById: new Map(),
    workloadBySession: new Map(),
    queue: [],
    host: null,
    revision: 0,
    queueDrawerOpen: false,
    graphDrawerOpen: false,
    modal: null,
    toast: null,
    renamingTabId: null,
    broadcastInput: false,
    hiddenMissions: new Set<string>(),
    missionProtocol: null,
  } satisfies Partial<WorkbenchState>));
}

describe("workbench store — split/close/ratio", () => {
  it("applySplit adds the new pane, focuses it, and preserves the existing leaf", () => {
    resetStore();
    const store = useWorkbenchStore.getState();
    store.addTab("tab-1", "탭 1");
    useWorkbenchStore.setState((s) => ({ tabs: s.tabs.map((t) => (t.id === "tab-1" ? { ...t, root: makeLeaf("leaf-1", "view-1", "sess-1") } : t)) }));
    useWorkbenchStore.setState((s) => ({
      panes: {
        ...s.panes,
        "leaf-1": {
          leafId: "leaf-1",
          viewId: "view-1",
          sessionId: "sess-1",
          workloadId: "w-1",
          title: "셸",
          cwd: "D:\\p",
          phase: "live",
          error: null,
          usage: null,
          flowBlocked: false,
        },
      },
      focusedLeafId: "leaf-1",
    }));

    const ok = useWorkbenchStore.getState().applySplit(
      "tab-1",
      "leaf-1",
      { leafId: "leaf-2", viewId: "view-2", sessionId: null, title: "새 터미널", cwd: "D:\\p" },
      "split-1",
      "row",
    );
    expect(ok).toBe(true);
    const after = useWorkbenchStore.getState();
    const root = selectActiveRoot(after);
    if (!root || root.kind !== "split") throw new Error("expected split root");
    expect(root.first).toMatchObject({ kind: "leaf", id: "leaf-1", session_id: "sess-1" });
    expect(root.second).toMatchObject({ kind: "leaf", id: "leaf-2", view_id: "view-2" });
    expect(root.ratio).toBe(0.5);
    expect(after.focusedLeafId).toBe("leaf-2"); // 새 pane에 포커스
    expect(after.panes["leaf-2"].phase).toBe("starting");
  });

  it("applyClose promotes the sibling and moves focus", () => {
    resetStore();
    const root = split(makeLeaf("leaf-1", "view-1", null), "leaf-1", { id: "leaf-2", view_id: "view-2" }, { splitId: "s1", axis: "row" });
    const store = useWorkbenchStore.getState();
    store.addTab("tab-1", "탭 1");
    useWorkbenchStore.setState((s) => ({
      tabs: s.tabs.map((t) => (t.id === "tab-1" ? { ...t, root } : t)),
      focusedLeafId: "leaf-2",
      panes: {
        "leaf-1": pane("leaf-1", "view-1"),
        "leaf-2": pane("leaf-2", "view-2"),
      },
    }));
    useWorkbenchStore.getState().applyClose("leaf-2");
    const after = useWorkbenchStore.getState();
    expect(selectActiveRoot(after)).toMatchObject({ kind: "leaf", id: "leaf-1" });
    expect(after.focusedLeafId).toBe("leaf-1");
    expect(after.panes["leaf-2"]).toBeUndefined();
  });

  it("연속 분할하면 pane들이 항상 같은 비율을 나눠 갖는다(50:50 → 1/3 → 1/4)", () => {
    resetStore();
    useWorkbenchStore.getState().addTab("tab-1", "탭 1");
    useWorkbenchStore.setState((s) => ({
      tabs: s.tabs.map((t) => (t.id === "tab-1" ? { ...t, root: makeLeaf("leaf-1", "view-1", null) } : t)),
      focusedLeafId: "leaf-1",
      panes: { ...s.panes, "leaf-1": pane("leaf-1", "view-1") },
    }));

    // 1번째 분할: 50:50
    splitFocused("leaf-2", "s1");
    expectEqualShares(2);

    // 2번째 분할: 1/3씩 — 기존 두 pane도 함께 재조정된다.
    splitFocused("leaf-3", "s2");
    expectEqualShares(3);

    // 3번째 분할: 1/4씩
    splitFocused("leaf-4", "s3");
    expectEqualShares(4);
    expect(useWorkbenchStore.getState().focusedLeafId).toBe("leaf-4");
  });

  it.each(["row", "column"] as const)("%s pane을 닫으면 4 → 3 → 2개가 매번 균등 비율로 돌아간다", (axis) => {
    resetStore();
    useWorkbenchStore.getState().addTab("tab-1", "탭 1");
    useWorkbenchStore.setState((s) => ({
      tabs: s.tabs.map((t) => (t.id === "tab-1" ? { ...t, root: makeLeaf("leaf-1", "view-1", null) } : t)),
      focusedLeafId: "leaf-1",
      panes: { ...s.panes, "leaf-1": pane("leaf-1", "view-1") },
    }));
    splitFocused("leaf-2", "s1", axis);
    splitFocused("leaf-3", "s2", axis);
    splitFocused("leaf-4", "s3", axis);
    expectEqualShares(4);

    useWorkbenchStore.getState().applyClose("leaf-4");
    expectEqualShares(3);
    useWorkbenchStore.getState().applyClose("leaf-3");
    expectEqualShares(2);

    const root = selectActiveRoot(useWorkbenchStore.getState());
    expect(root?.kind === "split" ? root.ratio : null).toBe(0.5);
  });

  it("사용자가 드래그로 맞춘 비율도 다시 분할하면 균등 기준으로 재조정된다", () => {
    resetStore();
    useWorkbenchStore.getState().addTab("tab-1", "탭 1");
    useWorkbenchStore.setState((s) => ({
      tabs: s.tabs.map((t) => (t.id === "tab-1" ? { ...t, root: makeLeaf("leaf-1", "view-1", null) } : t)),
      focusedLeafId: "leaf-1",
      panes: { ...s.panes, "leaf-1": pane("leaf-1", "view-1") },
    }));
    splitFocused("leaf-2", "s1");
    useWorkbenchStore.getState().setRatio("tab-1", "s1", 0.8);
    splitFocused("leaf-3", "s2");
    expectEqualShares(3);
  });

  it("divider 더블클릭(balanceRatios)은 옮겨 둔 경계선을 균등 비율로 되돌린다", () => {
    resetStore();
    useWorkbenchStore.getState().addTab("tab-1", "탭 1");
    useWorkbenchStore.setState((s) => ({
      tabs: s.tabs.map((t) => (t.id === "tab-1" ? { ...t, root: makeLeaf("leaf-1", "view-1", null) } : t)),
      focusedLeafId: "leaf-1",
      panes: { ...s.panes, "leaf-1": pane("leaf-1", "view-1") },
    }));
    splitFocused("leaf-2", "s1");
    splitFocused("leaf-3", "s2");
    useWorkbenchStore.getState().setRatio("tab-1", "s1", 0.8);
    useWorkbenchStore.getState().setRatio("tab-1", "s2", 0.2);

    // 그룹 안 아무 divider나 더블클릭하면 그룹 전체가 1/3씩으로 돌아온다.
    useWorkbenchStore.getState().balanceRatios("tab-1", "s2");
    expectEqualShares(3);
  });

  it("balanceRatios는 이미 균등하거나 대상이 없으면 상태를 바꾸지 않는다", () => {
    resetStore();
    useWorkbenchStore.getState().addTab("tab-1", "탭 1");
    useWorkbenchStore.setState((s) => ({
      tabs: s.tabs.map((t) => (t.id === "tab-1" ? { ...t, root: makeLeaf("leaf-1", "view-1", null) } : t)),
      focusedLeafId: "leaf-1",
      panes: { ...s.panes, "leaf-1": pane("leaf-1", "view-1") },
    }));
    splitFocused("leaf-2", "s1");
    const before = selectActiveRoot(useWorkbenchStore.getState());

    useWorkbenchStore.getState().balanceRatios("tab-1", "s1"); // 이미 50:50
    expect(selectActiveRoot(useWorkbenchStore.getState())).toBe(before); // 같은 참조 → 재렌더 없음
    useWorkbenchStore.getState().balanceRatios("tab-1", "nope"); // 모르는 divider
    expect(selectActiveRoot(useWorkbenchStore.getState())).toBe(before);
    useWorkbenchStore.getState().balanceRatios("tab-none", "s1"); // 모르는 탭
    expect(selectActiveRoot(useWorkbenchStore.getState())).toBe(before);
  });

  it("setRatio rewrites only the target node's ratio", () => {
    resetStore();
    const inner = split(makeLeaf("leaf-2", "v2", null), "leaf-2", { id: "leaf-3", view_id: "v3" }, { splitId: "inner", axis: "column" });
    const root = { ...split(makeLeaf("leaf-1", "v1", null), "leaf-1", { id: "leaf-2", view_id: "v2" }, { splitId: "outer", axis: "row" }), second: inner } as never;
    useWorkbenchStore.getState().addTab("tab-1", "탭 1");
    useWorkbenchStore.setState((s) => ({ tabs: s.tabs.map((t) => (t.id === "tab-1" ? { ...t, root } : t)) }));
    useWorkbenchStore.getState().setRatio("tab-1", "inner", 0.75);
    const after = selectActiveRoot(useWorkbenchStore.getState());
    if (!after || after.kind !== "split") throw new Error("root");
    expect(after.ratio).toBe(0.5);
    if (after.second.kind !== "split") throw new Error("inner");
    expect(after.second.ratio).toBe(0.75);
    // out-of-range values are clamped into [0,1]
    useWorkbenchStore.getState().setRatio("tab-1", "outer", 1.5);
    expect(selectActiveRoot(useWorkbenchStore.getState()) && selectActiveRoot(useWorkbenchStore.getState())!.kind === "split" && (selectActiveRoot(useWorkbenchStore.getState()) as { ratio: number }).ratio).toBe(1);
  });
});

describe("selector separation — U12", () => {
  it("tree mutations leave the host sample slice identical (strip must not re-render)", () => {
    resetStore();
    const hostSample = {
      monotonic_ms: 1,
      physical_total_bytes: { value: "34359738368", source: "t", quality: "measured" as const, reason: null },
      physical_available_bytes: { value: "12884901888", source: "t", quality: "measured" as const, reason: null },
      swap_used_bytes: { value: "0", source: "t", quality: "measured" as const, reason: null },
      pressure: "NORMAL" as const,
      cpu_cores_used: { value: 2.4, source: "t", quality: "measured" as const, reason: null },
      logical_cpu_count: 12,
      disks: [],
      interfaces: [],
    };
    useWorkbenchStore.getState().setHostSample(hostSample);
    const before = selectHost(useWorkbenchStore.getState());

    useWorkbenchStore.getState().addTab("tab-1", "탭 1");
    useWorkbenchStore.setState((s) => ({
      tabs: s.tabs.map((t) => (t.id === "tab-1" ? { ...t, root: makeLeaf("leaf-1", "view-1", null) } : t)),
      focusedLeafId: "leaf-1",
      panes: { ...s.panes, "leaf-1": pane("leaf-1", "view-1") },
    }));
    useWorkbenchStore.getState().applySplit(
      "tab-1",
      "leaf-1",
      { leafId: "leaf-2", viewId: "view-2", sessionId: null, title: "x", cwd: null },
      "s1",
      "row",
    );

    const after = selectHost(useWorkbenchStore.getState());
    expect(after).toBe(before); // Object.is — selector-based subscribers stay silent
  });

  it("host updates leave the tree root slice identical", () => {
    resetStore();
    useWorkbenchStore.getState().addTab("tab-1", "탭 1");
    useWorkbenchStore.setState((s) => ({ tabs: s.tabs.map((t) => (t.id === "tab-1" ? { ...t, root: makeLeaf("leaf-1", "view-1", null) } : t)) }));
    const before = selectActiveRoot(useWorkbenchStore.getState());
    useWorkbenchStore.getState().setHostSample({
      monotonic_ms: 2,
      physical_total_bytes: { value: "1", source: "t", quality: "measured", reason: null },
      physical_available_bytes: { value: "1", source: "t", quality: "measured", reason: null },
      swap_used_bytes: { value: "1", source: "t", quality: "measured", reason: null },
      pressure: "NORMAL",
      cpu_cores_used: { value: null, source: "t", quality: "measured", reason: null },
      logical_cpu_count: 8,
      disks: [],
      interfaces: [],
    });
    expect(selectActiveRoot(useWorkbenchStore.getState())).toBe(before);
  });
});

describe("snapshot revision guard", () => {
  it("drops stale snapshots", () => {
    resetStore();
    const store = useWorkbenchStore.getState();
    store.addTab("tab-1", "탭 1");
    const snap = (revision: number) => ({
      revision,
      host: {
        monotonic_ms: revision,
        physical_total_bytes: { value: "1", source: "t", quality: "measured" as const, reason: null },
        physical_available_bytes: { value: "1", source: "t", quality: "measured" as const, reason: null },
        swap_used_bytes: { value: "1", source: "t", quality: "measured" as const, reason: null },
        pressure: "NORMAL" as const,
        cpu_cores_used: { value: 1, source: "t", quality: "measured" as const, reason: null },
        logical_cpu_count: 8,
        disks: [],
        interfaces: [],
      },
      workloads: [],
      queue: [],
      capabilities: {
        memory_limit_kind: { support: "unsupported" as const },
        cpu_quota: { support: "supported" as const },
        process_count_limit: { support: "supported" as const },
        tree_accounting: { support: "unsupported" as const },
        reattach: { support: "supported" as const },
        resume: { support: "unsupported" as const },
        platform: "test",
      },
      reconciliation_required: false,
    });
    useWorkbenchStore.getState().applySnapshot(snap(10));
    expect(useWorkbenchStore.getState().revision).toBe(10);
    useWorkbenchStore.getState().applySnapshot(snap(5)); // stale → discarded
    expect(useWorkbenchStore.getState().revision).toBe(10);
  });
});

/** 현재 focus된 pane을 좌우 분할한다(sessionController.splitFocused와 같은 호출). */
function splitFocused(newLeafId: string, splitId: string, axis: "row" | "column" = "row"): void {
  const focused = useWorkbenchStore.getState().focusedLeafId;
  if (!focused) throw new Error("focused pane required");
  const ok = useWorkbenchStore.getState().applySplit(
    "tab-1",
    focused,
    { leafId: newLeafId, viewId: `view-${newLeafId}`, sessionId: null, title: "새 터미널", cwd: null },
    splitId,
    axis,
  );
  expect(ok).toBe(true);
}

/** 모든 pane이 1/count씩 차지하는지 확인한다(divider 두께는 무시). */
function expectEqualShares(count: number): void {
  const root = selectActiveRoot(useWorkbenchStore.getState());
  if (!root) throw new Error("root required");
  const values = Object.values(shares(root));
  expect(values).toHaveLength(count);
  for (const value of values) expect(value).toBeCloseTo(1 / count, 10);
}

function shares(node: NonNullable<ReturnType<typeof selectActiveRoot>>, share = 1): Record<string, number> {
  if (node.kind === "leaf") return { [node.id]: share };
  return {
    ...shares(node.first, share * node.ratio),
    ...shares(node.second, share * (1 - node.ratio)),
  };
}

function pane(leafId: string, viewId: string) {
  return {
    leafId,
    viewId,
    sessionId: null,
    workloadId: null,
    title: "t",
    cwd: null,
    phase: "live" as const,
    error: null,
    usage: null,
    flowBlocked: false,
  };
}

describe("management settings navigation", () => {
  it("opens a page instead of a modal and retains the active terminal layout on return", () => {
    resetStore();
    const store = useWorkbenchStore.getState();
    store.addTab("settings-tab", "Terminal");
    const root = makeLeaf("settings-leaf", "settings-view", "settings-session");
    useWorkbenchStore.setState(s => ({ tabs: s.tabs.map(tab => ({ ...tab, root })), focusedLeafId: "settings-leaf" }));
    const before = useWorkbenchStore.getState();
    store.openModal({ kind: "managed-run" });
    // 관리 실행 진입점은 설정 › 실행 그룹으로 곧장 간다(일반 그룹이 아니라).
    expect(useWorkbenchStore.getState()).toMatchObject({ page: "settings", modal: null, settingsGroup: "run" });
    store.setPage("terminal");
    const after = useWorkbenchStore.getState();
    expect(after.page).toBe("terminal");
    expect(after.settingsGroup).toBeNull();
    expect(after.tabs).toBe(before.tabs);
    expect(after.panes).toBe(before.panes);
    expect(after.focusedLeafId).toBe(before.focusedLeafId);
  });
});

describe("tab rename", () => {
  it("renames a tab and trims surrounding whitespace", () => {
    resetStore();
    const store = useWorkbenchStore.getState();
    store.addTab("tab-1", "탭 1");
    store.renameTab("tab-1", "  배포 로그  ");
    expect(useWorkbenchStore.getState().tabs[0].title).toBe("배포 로그");
  });

  it("rejects a blank name so a tab is never left unnamed", () => {
    resetStore();
    const store = useWorkbenchStore.getState();
    store.addTab("tab-1", "탭 1");
    store.renameTab("tab-1", "   ");
    expect(useWorkbenchStore.getState().tabs[0].title).toBe("탭 1");
  });

  it("ignores unknown tabs and keeps the tabs array identity when nothing changes", () => {
    resetStore();
    const store = useWorkbenchStore.getState();
    store.addTab("tab-1", "탭 1");
    const before = useWorkbenchStore.getState().tabs;
    store.renameTab("missing", "x");
    store.renameTab("tab-1", "탭 1"); // 같은 이름 — 재렌더를 유발하지 않는다
    expect(useWorkbenchStore.getState().tabs).toBe(before);
  });

  it("tracks which tab is being edited and only accepts existing tabs", () => {
    resetStore();
    const store = useWorkbenchStore.getState();
    store.addTab("tab-1", "탭 1");
    store.startTabRename("tab-1");
    expect(useWorkbenchStore.getState().renamingTabId).toBe("tab-1");
    store.startTabRename("missing");
    expect(useWorkbenchStore.getState().renamingTabId).toBe("tab-1");
    store.startTabRename(null);
    expect(useWorkbenchStore.getState().renamingTabId).toBeNull();
  });

  it("drops the edit state when the tab being renamed is closed", () => {
    resetStore();
    const store = useWorkbenchStore.getState();
    store.addTab("tab-1", "탭 1");
    store.startTabRename("tab-1");
    store.closeTab("tab-1");
    expect(useWorkbenchStore.getState().renamingTabId).toBeNull();
  });
});

describe("broadcast input toggle", () => {
  it("is off by default and flips, or takes an explicit state", () => {
    resetStore();
    const store = useWorkbenchStore.getState();
    expect(useWorkbenchStore.getState().broadcastInput).toBe(false);
    store.toggleBroadcastInput();
    expect(useWorkbenchStore.getState().broadcastInput).toBe(true);
    store.toggleBroadcastInput(true); // 팔레트/버튼이 같은 상태를 두 번 눌러도 안전
    expect(useWorkbenchStore.getState().broadcastInput).toBe(true);
    store.toggleBroadcastInput(false);
    expect(useWorkbenchStore.getState().broadcastInput).toBe(false);
  });
});

describe("workbench store — bounded workload mirror", () => {
  const summary = (id: string, state: WorkloadSummary["state"]): WorkloadSummary =>
    ({ workload_id: id, state, mode: "shell", title: id } as unknown as WorkloadSummary);

  it("keeps every running workload and only the most recent finished ones", () => {
    const running = Array.from({ length: 5 }, (_, i) => summary(`run-${i}`, "RUNNING"));
    const finished = Array.from({ length: FINISHED_WORKLOADS_RETAINED + 20 }, (_, i) =>
      summary(`done-${i}`, i % 2 === 0 ? "SUCCEEDED" : "CANCELLED"),
    );
    // Interleave so eviction order (oldest first) is exercised, not array position luck.
    const workloads: WorkloadSummary[] = [];
    finished.forEach((w, i) => {
      workloads.push(w);
      if (running[i]) workloads.push(running[i]);
    });
    const pruned = pruneFinishedWorkloads(workloads, {});
    expect(pruned.filter((w) => w.state === "RUNNING")).toHaveLength(5);
    const keptFinished = pruned.filter((w) => w.state !== "RUNNING");
    expect(keptFinished).toHaveLength(FINISHED_WORKLOADS_RETAINED);
    // The oldest 20 finished ones are the ones dropped.
    expect(keptFinished[0].workload_id).toBe("done-20");
    expect(keptFinished.at(-1)?.workload_id).toBe(`done-${FINISHED_WORKLOADS_RETAINED + 19}`);
  });

  it("never drops a finished workload a pane still shows (exit reason overlay)", () => {
    // 52 finished: done-0 is referenced by a pane, so 51 are prunable → the
    // oldest prunable one (done-1) goes, done-0 stays regardless of age.
    const finished = Array.from({ length: FINISHED_WORKLOADS_RETAINED + 2 }, (_, i) => summary(`done-${i}`, "FAILED"));
    const panes = {
      "leaf-1": {
        leafId: "leaf-1", viewId: "view-1", sessionId: "s", workloadId: "done-0",
        title: "x", cwd: null, phase: "exited" as const, error: null, usage: null, flowBlocked: false,
      },
    };
    const pruned = pruneFinishedWorkloads(finished, panes);
    expect(pruned.map((w) => w.workload_id)).toContain("done-0");
    expect(pruned.map((w) => w.workload_id)).not.toContain("done-1");
    expect(pruned).toHaveLength(FINISHED_WORKLOADS_RETAINED + 1);
  });

  it("returns the same array when nothing needs pruning", () => {
    const workloads = [summary("a", "RUNNING"), summary("b", "SUCCEEDED")];
    expect(pruneFinishedWorkloads(workloads, {})).toBe(workloads);
  });

  it("upsertWorkload prunes on the way in so a day of terminals cannot grow the mirror forever", () => {
    resetStore();
    for (let i = 0; i < FINISHED_WORKLOADS_RETAINED * 3; i++) {
      useWorkbenchStore.getState().upsertWorkload(summary(`w-${i}`, "RUNNING"));
      useWorkbenchStore.getState().upsertWorkload(summary(`w-${i}`, "SUCCEEDED"));
    }
    expect(useWorkbenchStore.getState().workloads).toHaveLength(FINISHED_WORKLOADS_RETAINED);
  });
});

describe("workbench store — 파생 워크로드 인덱스", () => {
  const summary = (
    id: string,
    sessionId: string | null | undefined,
    state: WorkloadSummary["state"] = "RUNNING",
  ): WorkloadSummary =>
    ({ workload_id: id, session_id: sessionId, state, mode: "shell", title: id } as unknown as WorkloadSummary);

  it("upsertWorkload가 id·세션 색인을 배열과 함께 유지한다", () => {
    resetStore();
    const store = useWorkbenchStore.getState();
    store.upsertWorkload(summary("w-1", "s-1"));
    store.upsertWorkload(summary("w-2", "s-2"));
    // 같은 id의 갱신은 색인도 교체한다.
    store.upsertWorkload(summary("w-1", "s-1", "SUCCEEDED"));
    const s = useWorkbenchStore.getState();
    expect(s.workloadById.get("w-1")?.state).toBe("SUCCEEDED");
    expect(s.workloadById.get("w-2")?.state).toBe("RUNNING");
    expect(s.workloadBySession.get("s-1")).toBe(s.workloadById.get("w-1"));
    expect(s.workloadBySession.get("s-2")?.workload_id).toBe("w-2");
    expect(s.workloadById.get("missing")).toBeUndefined();
  });

  it("세션 중복은 배열 순서의 첫 항목을 고른다(.find와 같은 의미)", () => {
    resetStore();
    const store = useWorkbenchStore.getState();
    store.upsertWorkload(summary("w-1", "s-shared"));
    store.upsertWorkload(summary("w-2", "s-shared"));
    const s = useWorkbenchStore.getState();
    const first = s.workloads.find((w) => w.session_id === "s-shared");
    expect(s.workloadBySession.get("s-shared")).toBe(first);
    expect(s.workloadBySession.get("s-shared")?.workload_id).toBe("w-1");
  });

  it("session_id가 없는(null·undefined) 워크로드는 세션 색인에 없다", () => {
    resetStore();
    const store = useWorkbenchStore.getState();
    store.upsertWorkload(summary("queued-1", null, "QUEUED"));
    store.upsertWorkload(summary("queued-2", undefined, "QUEUED"));
    const s = useWorkbenchStore.getState();
    expect(s.workloadBySession.size).toBe(0);
    expect(s.workloadById.get("queued-1")).toBeDefined();
    expect(s.workloadById.get("queued-2")).toBeDefined();
  });
});

describe("tab focus follows the active tab", () => {
  function twoTabs() {
    resetStore();
    const store = useWorkbenchStore.getState();
    store.addTab("tab-a", "A");
    store.addTab("tab-b", "B");
    useWorkbenchStore.setState((s) => ({
      tabs: s.tabs.map((t) =>
        t.id === "tab-a"
          ? { ...t, root: makeLeaf("leaf-a", "view-a", "sess-a") }
          : { ...t, root: makeLeaf("leaf-b", "view-b", "sess-b") },
      ),
      focusedLeafId: "leaf-a",
    }));
    return store;
  }

  it("setActiveTab moves focus into the new tab so shortcuts never target a hidden pane", () => {
    const store = twoTabs();
    store.setActiveTab("tab-b");
    expect(useWorkbenchStore.getState()).toMatchObject({ activeTabId: "tab-b", focusedLeafId: "leaf-b" });
    store.setActiveTab("tab-a");
    expect(useWorkbenchStore.getState().focusedLeafId).toBe("leaf-a");
  });

  it("setActiveTab is a no-op when the focused leaf already belongs to that tab", () => {
    const store = twoTabs();
    const before = useWorkbenchStore.getState();
    store.setActiveTab("tab-a");
    expect(useWorkbenchStore.getState()).toBe(before);
  });

  it("setActiveTab onto an empty tab clears the focus (no leaf to own it)", () => {
    const store = twoTabs();
    store.addTab("tab-c", "C");
    store.setActiveTab("tab-c");
    expect(useWorkbenchStore.getState()).toMatchObject({ activeTabId: "tab-c", focusedLeafId: null });
  });

  it("closeTab of the active tab refocuses the first leaf of the tab that takes over", () => {
    const store = twoTabs();
    store.closeTab("tab-a");
    expect(useWorkbenchStore.getState()).toMatchObject({ activeTabId: "tab-b", focusedLeafId: "leaf-b" });
  });

  it("closeTab of a background tab leaves the active tab's focus alone", () => {
    const store = twoTabs();
    store.closeTab("tab-b");
    expect(useWorkbenchStore.getState()).toMatchObject({ activeTabId: "tab-a", focusedLeafId: "leaf-a" });
  });
});

describe("workbench store — 에이전트 세션 재개 기록(04-ui §5)", () => {
  function paneWithoutSession() {
    resetStore();
    useWorkbenchStore.setState({
      panes: {
        "leaf-1": {
          leafId: "leaf-1",
          viewId: "view-1",
          sessionId: null,
          workloadId: null,
          title: "zsh",
          cwd: "/work",
          phase: "exited",
          error: null,
          usage: null,
          flowBlocked: false,
        },
      },
    });
    return useWorkbenchStore.getState();
  }

  const resume: AgentResumeInfo = {
    recordId: "rec-1",
    agent: "codex",
    agentSessionId: "agent-session-0001",
    cwd: "/work/iyagi",
    title: null,
    program: null,
  };

  it("paneResume은 기록을 붙이고 null로 다시 지운다", () => {
    const store = paneWithoutSession();
    store.paneResume("leaf-1", resume);
    expect(useWorkbenchStore.getState().panes["leaf-1"].resume).toEqual(resume);

    useWorkbenchStore.getState().paneResume("leaf-1", null);
    expect(useWorkbenchStore.getState().panes["leaf-1"].resume).toBeNull();
  });

  it("같은 값이면 상태를 갈아 끼우지 않는다(불필요한 리렌더 방지)", () => {
    const store = paneWithoutSession();
    store.paneResume("leaf-1", resume);
    const after = useWorkbenchStore.getState();
    after.paneResume("leaf-1", resume);
    expect(useWorkbenchStore.getState()).toBe(after);
  });

  it("없는 pane에는 아무 일도 하지 않는다", () => {
    const store = paneWithoutSession();
    const before = useWorkbenchStore.getState();
    store.paneResume("leaf-missing", resume);
    expect(useWorkbenchStore.getState()).toBe(before);
  });

  it("기록이 없는 pane을 null로 지워도 상태를 갈아 끼우지 않는다", () => {
    // 새 PTY 준비(resetPaneForFreshPty)가 항상 부르는 경로다 — 재개 기록이
    // 없는 평범한 pane에서 공짜 리렌더가 생기지 않아야 한다.
    const store = paneWithoutSession();
    const before = useWorkbenchStore.getState();
    store.paneResume("leaf-1", null);
    expect(useWorkbenchStore.getState()).toBe(before);
  });
});

describe("workbench store — 재배치(04-ui §2-5): moveTab / movePaneToTab / detachPaneToNewTab / mergeTabs / applyRegroup", () => {
  const meta = (leafId: string, extra: Partial<import("../src/store/workbenchStore").PaneMeta> = {}) => ({
    leafId,
    viewId: `v-${leafId}`,
    sessionId: `s-${leafId}`,
    workloadId: `w-${leafId}`,
    title: leafId,
    cwd: null as string | null,
    phase: "live" as const,
    error: null,
    usage: null,
    flowBlocked: false,
    ...extra,
  });
  const leaf = (id: string) => makeLeaf(id, `v-${id}`, `s-${id}`);
  const row = (a: string, b: string, splitId = `sp-${a}-${b}`) =>
    split(leaf(a), a, { id: b, view_id: `v-${b}`, session_id: `s-${b}` }, { splitId, axis: "row" })!;
  const leafIds = (tabId: string) => {
    const tab = useWorkbenchStore.getState().tabs.find((t) => t.id === tabId);
    return tab ? listLeavesIds(tab.root) : null;
  };
  function listLeavesIds(root: import("../src/features/terminal/splitTree").SplitNode | null): string[] {
    if (!root) return [];
    if (root.kind === "leaf") return [root.id];
    return [...listLeavesIds(root.first), ...listLeavesIds(root.second)];
  }
  /** 탭 1 = a|b, 탭 2 = c, 탭 3 = 빈 탭. 초점 a. */
  function seed() {
    resetStore();
    useWorkbenchStore.setState({
      tabs: [
        { kind: "terminal", id: "t1", title: "탭 1", root: row("a", "b") },
        { kind: "terminal", id: "t2", title: "탭 2", root: leaf("c") },
        { kind: "terminal", id: "t3", title: "빈 탭", root: null },
      ],
      panes: { a: meta("a"), b: meta("b"), c: meta("c") },
      activeTabId: "t1",
      focusedLeafId: "a",
    });
  }

  it("moveTab reorders and clamps; same slot or unknown tab keeps the array identity", () => {
    seed();
    const before = useWorkbenchStore.getState().tabs;
    useWorkbenchStore.getState().moveTab("t1", 99);
    expect(useWorkbenchStore.getState().tabs.map((t) => t.id)).toEqual(["t2", "t3", "t1"]);
    useWorkbenchStore.getState().moveTab("t1", -5);
    expect(useWorkbenchStore.getState().tabs.map((t) => t.id)).toEqual(["t1", "t2", "t3"]);
    const now = useWorkbenchStore.getState().tabs;
    useWorkbenchStore.getState().moveTab("t1", 0);
    useWorkbenchStore.getState().moveTab("nope", 1);
    expect(useWorkbenchStore.getState().tabs).toBe(now);
    expect(before).not.toBe(now);
  });

  it("movePaneToTab splits next to the target's last pane, follows with focus, and rebalances the source", () => {
    seed();
    const ok = useWorkbenchStore.getState().movePaneToTab("a", "t2", "sp-new");
    expect(ok).toBe(true);
    const s = useWorkbenchStore.getState();
    expect(leafIds("t1")).toEqual(["b"]);
    expect(leafIds("t2")).toEqual(["c", "a"]);
    expect(s.activeTabId).toBe("t2");
    expect(s.focusedLeafId).toBe("a");
    // pane 메타·leaf의 view/session은 그대로.
    expect(s.panes.a.viewId).toBe("v-a");
    const t2 = s.tabs.find((t) => t.id === "t2")!;
    expect(t2.root).toMatchObject({ kind: "split", id: "sp-new", axis: "row", ratio: 0.5 });
    expect(findLeafIn(t2.root, "a")).toMatchObject({ view_id: "v-a", session_id: "s-a" });
  });

  it("movePaneToTab into an empty tab makes the pane its root; the emptied source tab closes", () => {
    seed();
    useWorkbenchStore.getState().movePaneToTab("c", "t3", "sp-x");
    let s = useWorkbenchStore.getState();
    expect(s.tabs.map((t) => t.id)).toEqual(["t1", "t3"]); // t2가 비어 닫혔다
    expect(leafIds("t3")).toEqual(["c"]);
    expect(s.activeTabId).toBe("t3");
    expect(s.focusedLeafId).toBe("c");
    // 초점이 대상 탭 안에 있으면 그 옆에 끼운다.
    useWorkbenchStore.getState().movePaneToTab("a", "t3", "sp-y");
    s = useWorkbenchStore.getState();
    expect(leafIds("t3")).toEqual(["c", "a"]);
    expect(leafIds("t1")).toEqual(["b"]);
  });

  it("movePaneToTab refuses the same tab, unknown ids, and a full target (8 panes)", () => {
    seed();
    const store = useWorkbenchStore.getState();
    expect(store.movePaneToTab("a", "t1", "x")).toBe(false);
    expect(store.movePaneToTab("zzz", "t2", "x")).toBe(false);
    expect(store.movePaneToTab("a", "nope", "x")).toBe(false);
    // t2를 8개까지 채운다.
    let root = leaf("c");
    const panes: Record<string, ReturnType<typeof meta>> = { a: meta("a"), b: meta("b"), c: meta("c") };
    for (let i = 1; i < 8; i += 1) {
      root = split(root, i === 1 ? "c" : `f${i - 1}`, { id: `f${i}`, view_id: `v-f${i}`, session_id: `s-f${i}` }, { splitId: `fs${i}`, axis: "row" })!;
      panes[`f${i}`] = meta(`f${i}`);
    }
    useWorkbenchStore.setState((s) => ({ tabs: s.tabs.map((t) => (t.id === "t2" ? { ...t, root } : t)), panes }));
    expect(useWorkbenchStore.getState().movePaneToTab("a", "t2", "x")).toBe(false);
    expect(leafIds("t1")).toEqual(["a", "b"]);
  });

  it("detachPaneToNewTab opens the new tab right after the source and refuses a pane that is alone", () => {
    seed();
    expect(useWorkbenchStore.getState().detachPaneToNewTab("c", "new", "새 탭")).toBe(false); // 혼자 → 이미 탭
    expect(useWorkbenchStore.getState().detachPaneToNewTab("b", "new", "새 탭")).toBe(true);
    const s = useWorkbenchStore.getState();
    expect(s.tabs.map((t) => t.id)).toEqual(["t1", "new", "t2", "t3"]);
    expect(leafIds("t1")).toEqual(["a"]);
    expect(leafIds("new")).toEqual(["b"]);
    expect(s.activeTabId).toBe("new");
    expect(s.focusedLeafId).toBe("b");
    // 이미 있는 id로는 만들지 않는다.
    expect(useWorkbenchStore.getState().detachPaneToNewTab("a", "t2", "x")).toBe(false);
  });

  it("mergeTabs joins both trees under one row split, keeps inner layouts, focuses the merged-in side", () => {
    seed();
    useWorkbenchStore.getState().setRatio("t1", "sp-a-b", 0.3); // 사용자가 맞춘 비율
    expect(useWorkbenchStore.getState().mergeTabs("t1", "t2", "join")).toBe(true);
    const s = useWorkbenchStore.getState();
    expect(s.tabs.map((t) => t.id)).toEqual(["t2", "t3"]);
    const t2 = s.tabs.find((t) => t.id === "t2")!;
    expect(leafIds("t2")).toEqual(["c", "a", "b"]);
    if (!t2.root || t2.root.kind !== "split") throw new Error("expected split");
    expect(t2.root.id).toBe("join");
    // c | (a | b) 는 row 그룹 member 3개 → 1/3 : 2/3, 안쪽 a|b 비율도 균등화된다.
    expect(t2.root.ratio).toBeCloseTo(1 / 3);
    expect(s.activeTabId).toBe("t2");
    expect(s.focusedLeafId).toBe("a");
  });

  it("mergeTabs of an empty tab just closes it; over-cap merges are refused untouched", () => {
    seed();
    useWorkbenchStore.getState().setActiveTab("t3");
    expect(useWorkbenchStore.getState().mergeTabs("t3", "t1", "j")).toBe(true);
    let s = useWorkbenchStore.getState();
    expect(s.tabs.map((t) => t.id)).toEqual(["t1", "t2"]);
    expect(s.activeTabId).toBe("t1");
    expect(s.focusedLeafId).toBe("a");
    // 5 + 4 > 8 → 거절.
    const five = ["p1", "p2", "p3", "p4", "p5"];
    const four = ["q1", "q2", "q3", "q4"];
    const chain = (ids: string[]) => ids.slice(1).reduce((acc, id, i) => split(acc, ids[i], { id, view_id: `v-${id}`, session_id: `s-${id}` }, { splitId: `${id}-s`, axis: "row" })!, leaf(ids[0]));
    useWorkbenchStore.setState({
      tabs: [
        { kind: "terminal", id: "A", title: "A", root: chain(five) },
        { kind: "terminal", id: "B", title: "B", root: chain(four) },
      ],
      panes: Object.fromEntries([...five, ...four].map((id) => [id, meta(id)])),
      activeTabId: "A",
      focusedLeafId: "p1",
    });
    const before = useWorkbenchStore.getState().tabs;
    expect(useWorkbenchStore.getState().mergeTabs("B", "A", "j")).toBe(false);
    expect(useWorkbenchStore.getState().tabs).toBe(before);
    s = useWorkbenchStore.getState();
    expect(s.tabs.map((t) => t.id)).toEqual(["A", "B"]);
  });

  it("applyRegroup materialises a plan, keeps every pane meta, and clears any tab rename in progress", () => {
    seed();
    useWorkbenchStore.setState((s) => ({
      panes: {
        a: { ...s.panes.a, cwd: "/repo/x" },
        b: { ...s.panes.b, cwd: "/repo/y" },
        c: { ...s.panes.c, cwd: "/repo/x" },
      },
      renamingTabId: "t1",
    }));
    const st = useWorkbenchStore.getState();
    const plan = planRegroup(
      { tabs: st.tabs, panes: st.panes, activeTabId: st.activeTabId, focusedLeafId: st.focusedLeafId },
      { noProjectTitle: "기타" },
    );
    expect(plan.changed).toBe(true);
    let n = 0;
    expect(useWorkbenchStore.getState().applyRegroup(plan, () => `re-${++n}`)).toBe(true);
    const s = useWorkbenchStore.getState();
    expect(s.tabs.map((t) => t.title)).toEqual(["x", "y", "빈 탭"]);
    expect(s.tabs[2].id).toBe("t3"); // 빈 탭은 같은 객체 그대로
    expect(leafIds(s.tabs[0].id)).toEqual(["a", "c"]);
    expect(leafIds(s.tabs[1].id)).toEqual(["b"]);
    expect(Object.keys(s.panes).sort()).toEqual(["a", "b", "c"]);
    expect(s.focusedLeafId).toBe("a");
    expect(s.activeTabId).toBe(s.tabs[0].id);
    expect(s.renamingTabId).toBeNull();
    // 계획을 세운 뒤 pane이 늘어났으면(대화상자를 열어 둔 사이 분할) 거절 — 부분 적용 없음.
    useWorkbenchStore.getState().applySplit(
      s.tabs[1].id, "b", { leafId: "late", viewId: "v-late", sessionId: null, title: "late", cwd: null }, "sp-late", "row",
    );
    const beforeStale = useWorkbenchStore.getState().tabs;
    expect(useWorkbenchStore.getState().applyRegroup(plan, () => `re-${++n}`)).toBe(false);
    expect(useWorkbenchStore.getState().tabs).toBe(beforeStale);
  });

  function findLeafIn(root: import("../src/features/terminal/splitTree").SplitNode | null, id: string) {
    if (!root) return null;
    if (root.kind === "leaf") return root.id === id ? root : null;
    return findLeafIn(root.first, id) ?? findLeafIn(root.second, id);
  }
});
describe("mission 탭 — tab union kind (05-ui §2)", () => {
  function seedTerminalTab(tabId: string, leafId = "leaf-1"): void {
    useWorkbenchStore.getState().addTab(tabId, "터미널");
    useWorkbenchStore.setState((s) => ({
      tabs: s.tabs.map((tab) =>
        tab.id === tabId && tab.kind === "terminal" ? { ...tab, root: makeLeaf(leafId, `view-${leafId}`, null) } : tab,
      ),
      panes: { ...s.panes, [leafId]: pane(leafId, `view-${leafId}`) },
      focusedLeafId: leafId,
    }));
  }

  it("openMissionTab은 탭을 만들고 초점을 옮긴다(leaf 초점 없음)", () => {
    resetStore();
    const id = useWorkbenchStore.getState().openMissionTab("m-1", "로그인 기능");
    expect(id).not.toBeNull();
    const tab = useWorkbenchStore.getState().tabs.find(t => t.id === id);
    expect(tab).toMatchObject({ kind: "mission", missionId: "m-1", title: "로그인 기능" });
    expect(useWorkbenchStore.getState()).toMatchObject({ activeTabId: id, focusedLeafId: null });
  });

  it("같은 mission은 기존 탭으로 이동한다(중복 탭 없음)", () => {
    resetStore();
    useWorkbenchStore.getState().addTab("term", "터미널");
    const first = useWorkbenchStore.getState().openMissionTab("m-1", "미션 1")!;
    useWorkbenchStore.getState().setActiveTab("term");
    const again = useWorkbenchStore.getState().openMissionTab("m-1", "다른 제목")!;
    expect(again).toBe(first);
    expect(useWorkbenchStore.getState().tabs.filter(tab => tab.kind === "mission")).toHaveLength(1);
    expect(useWorkbenchStore.getState().activeTabId).toBe(first);
    expect(useWorkbenchStore.getState().focusedLeafId).toBeNull();
  });

  it("설정·배치 편집 화면에서 AI 작업 탭을 열거나 그 탭으로 옮기면 탭 화면으로 돌아간다(상한으로 못 열면 그대로)", () => {
    resetStore();
    useWorkbenchStore.getState().openSettings("missions");
    const id = useWorkbenchStore.getState().openMissionTab("m-1", "미션 1")!;
    expect(useWorkbenchStore.getState()).toMatchObject({ page: "terminal", settingsGroup: null, activeTabId: id });
    // 이미 열린 탭으로 옮기는 경로(목록·알림에서 다시 열기)도 같다.
    useWorkbenchStore.getState().setPage("layout");
    expect(useWorkbenchStore.getState().openMissionTab("m-1", "미션 1")).toBe(id);
    expect(useWorkbenchStore.getState().page).toBe("terminal");
    useWorkbenchStore.getState().openSettings("general");
    expect(useWorkbenchStore.getState().openAgentViewTab("m-1", "t-1", "API 구현")).not.toBeNull();
    expect(useWorkbenchStore.getState().page).toBe("terminal");
    // 상한에 걸려 탭을 못 열면 보던 화면을 바꾸지 않는다.
    for (let i = 0; i < MAX_MISSION_TABS; i++) useWorkbenchStore.getState().openMissionTab(`m-cap-${i}`, `미션 ${i}`);
    useWorkbenchStore.getState().openSettings("general");
    expect(useWorkbenchStore.getState().openMissionTab("m-over", "넘침")).toBeNull();
    expect(useWorkbenchStore.getState().page).toBe("settings");
    useWorkbenchStore.getState().setPage("terminal");
  });

  it("mission 탭 닫기는 로컬 숨김이다 — pane은 무사하고 재오픈하면 숨김이 풀린다", () => {
    resetStore();
    seedTerminalTab("term");
    const missionTab = useWorkbenchStore.getState().openMissionTab("m-1", "미션 1")!;
    useWorkbenchStore.getState().closeTab(missionTab);
    const after = useWorkbenchStore.getState();
    expect(after.tabs.map(t => t.id)).toEqual(["term"]);
    expect([...after.hiddenMissions]).toEqual(["m-1"]);
    expect(after.panes).toHaveProperty("leaf-1"); // terminal pane은 그대로
    expect(after).toMatchObject({ activeTabId: "term", focusedLeafId: "leaf-1" });
    const reopened = useWorkbenchStore.getState().openMissionTab("m-1", "미션 1")!;
    expect(reopened).not.toBe(missionTab);
    expect(useWorkbenchStore.getState().hiddenMissions.has("m-1")).toBe(false);
  });

  it("mission 탭은 최대 16개 — 초과해도 실행을 취소하지 않고 toast로 안내한다", () => {
    resetStore();
    for (let i = 0; i < MAX_MISSION_TABS; i++) {
      expect(useWorkbenchStore.getState().openMissionTab(`m-${i}`, `미션 ${i}`)).not.toBeNull();
    }
    const before = useWorkbenchStore.getState().tabs;
    expect(useWorkbenchStore.getState().openMissionTab("m-extra", "추가 미션")).toBeNull();
    expect(useWorkbenchStore.getState().tabs).toBe(before); // 상태를 바꾸지 않는다
    expect(useWorkbenchStore.getState().toast).toBe(t("missions.tabLimit", { count: MAX_MISSION_TABS }));
    // 일반 terminal 탭은 mission 상한과 무관하다(05 §2 별개 cap).
    useWorkbenchStore.getState().addTab("term", "터미널");
    expect(useWorkbenchStore.getState().tabs).toHaveLength(MAX_MISSION_TABS + 1);
    expect(useWorkbenchStore.getState().openMissionTab("m-extra2", "또")).toBeNull();
  });

  it("agent-view는 task ID로 중복 검사하고 닫아도 숨김 목록에 남지 않는다", () => {
    resetStore();
    const a = useWorkbenchStore.getState().openAgentViewTab("m-1", "t-1", "API 구현")!;
    const b = useWorkbenchStore.getState().openAgentViewTab("m-1", "t-1", "같은 task")!;
    expect(b).toBe(a);
    const c = useWorkbenchStore.getState().openAgentViewTab("m-1", "t-2", "다른 task")!;
    expect(c).not.toBe(a);
    useWorkbenchStore.getState().closeTab(c);
    expect(useWorkbenchStore.getState().tabs.map(tab => tab.id)).toEqual([a]);
    expect(useWorkbenchStore.getState().hiddenMissions.size).toBe(0);
  });

  it("이름 변경은 terminal 전용 — mission 탭은 renameTab이 상태를 바꾸지 않는다", () => {
    resetStore();
    const id = useWorkbenchStore.getState().openMissionTab("m-1", "미션 1")!;
    const before = useWorkbenchStore.getState();
    before.renameTab(id, "새 이름");
    expect(useWorkbenchStore.getState()).toBe(before);
  });

  it("mission 탭에는 분할/pane을 만들지 않는다(가짜 PTY 금지)", () => {
    resetStore();
    const id = useWorkbenchStore.getState().openMissionTab("m-1", "미션 1")!;
    const ok = useWorkbenchStore.getState().applySplit(
      id,
      "nope",
      { leafId: "l9", viewId: "v9", sessionId: null, title: "x", cwd: null },
      "s1",
      "row",
    );
    expect(ok).toBe(false);
    expect(useWorkbenchStore.getState().panes).toEqual({});
  });
});

describe("close-all/quit 흐름 — mission은 숨김, 취소 없음 (05 §8)", () => {
  function stubRegistry(): TerminalRegistry {
    return {
      setFitHandler: () => undefined,
      setVisibilityListener: () => undefined,
      get: () => undefined,
      disposeAll: () => undefined,
    } as unknown as TerminalRegistry;
  }

  function makeClient() {
    const calls = { detach: [] as string[], cancel: [] as string[] };
    const client = {
      events: { subscribe: () => ({ dispose: () => undefined }) },
      systemSnapshot: async () => { throw new Error("unused"); },
      interventionList: async () => [],
      agentSessionList: async () => [],
      sessionDetach: async (p: { session_id: string }) => {
        calls.detach.push(p.session_id);
        return { detached: true as const };
      },
      workloadCancel: async (p: { workload_id: string }) => {
        calls.cancel.push(p.workload_id);
        return { state: "STOPPING" as const };
      },
    } as unknown as DaemonClient;
    return { client, calls };
  }

  function seedWorkspace(): void {
    resetStore();
    useWorkbenchStore.getState().addTab("term", "터미널");
    useWorkbenchStore.setState((s) => ({
      tabs: s.tabs.map(tab =>
        tab.id === "term" && tab.kind === "terminal"
          ? { ...tab, root: makeLeaf("leaf-1", "view-1", "sess-1") }
          : tab,
      ),
      panes: {
        ...s.panes,
        "leaf-1": {
          leafId: "leaf-1",
          viewId: "view-1",
          sessionId: "sess-1",
          workloadId: "w-1",
          title: "셸",
          cwd: null,
          phase: "live" as const,
          error: null,
          usage: null,
          flowBlocked: false,
        },
      },
      focusedLeafId: "leaf-1",
    }));
    useWorkbenchStore.getState().openMissionTab("m-1", "로그인 기능");
  }

  it("close all 확인은 일반 terminal pane만 대상으로 삼는다", () => {
    seedWorkspace();
    const controller = new SessionController({
      client: makeClient().client,
      registry: stubRegistry(),
      platform: "windows",
    });
    controller.requestCloseAllTabs();
    const modal = useWorkbenchStore.getState().modal;
    expect(modal?.kind).toBe("close-panes");
    if (modal?.kind !== "close-panes") return;
    expect(modal.leafIds).toEqual(["leaf-1"]); // mission 탭은 종료 확인에 없다
    expect(modal.closeAllTabs).toBe(true);
  });

  it("분리 닫기: mission은 숨겨지고 workloadCancel은 한 번도 불리지 않는다", async () => {
    seedWorkspace();
    const { client, calls } = makeClient();
    const controller = new SessionController({ client, registry: stubRegistry(), platform: "windows" });
    await controller.confirmClosePanes(["leaf-1"], false, undefined, true);
    expect(calls.cancel).toEqual([]); // 취소 요청 0회 — mission 실행은 계속된다
    expect(calls.detach).toEqual(["sess-1"]);
    const after = useWorkbenchStore.getState();
    expect(after.tabs).toEqual([]);
    expect([...after.hiddenMissions]).toEqual(["m-1"]);
  });

  it("종료 닫기: 일반 terminal만 취소하고 mission에 대한 취소는 만들지 않는다", async () => {
    seedWorkspace();
    const { client, calls } = makeClient();
    const controller = new SessionController({ client, registry: stubRegistry(), platform: "windows" });
    await controller.confirmClosePanes(["leaf-1"], true, undefined, true);
    expect(calls.cancel).toEqual(["w-1"]); // terminal workload 1건만
    expect([...useWorkbenchStore.getState().hiddenMissions]).toEqual(["m-1"]);
    expect(useWorkbenchStore.getState().tabs).toEqual([]);
  });

  it("terminal이 없으면 close all은 확인 없이 mission 숨김만 실행한다", () => {
    resetStore();
    useWorkbenchStore.getState().openMissionTab("m-1", "미션");
    const controller = new SessionController({
      client: makeClient().client,
      registry: stubRegistry(),
      platform: "windows",
    });
    controller.requestCloseAllTabs();
    expect(useWorkbenchStore.getState().modal).toBeNull(); // 물을 게 없다
    expect(useWorkbenchStore.getState().tabs).toEqual([]);
    expect([...useWorkbenchStore.getState().hiddenMissions]).toEqual(["m-1"]);
  });

  it("워크벤치 flashToast와 세션 제어기 toast는 서로의 타이머로 나중에 뜬 토스트를 지우지 않는다", () => {
    vi.useFakeTimers();
    const controller = new SessionController({ client: makeClient().client, registry: stubRegistry(), platform: "windows" });
    try {
      resetStore();
      // 안내 토스트 뒤에 제어기 토스트 — 안내의 타이머(4s)가 제어기 토스트를 지우지 않는다.
      flashToast("탭을 닫았습니다");
      vi.advanceTimersByTime(2000);
      controller.toast("이동하지 못했습니다");
      vi.advanceTimersByTime(FLASH_TOAST_MS - 2000);
      expect(useWorkbenchStore.getState().toast).toBe("이동하지 못했습니다");
      vi.advanceTimersByTime(2000);
      expect(useWorkbenchStore.getState().toast).toBeNull();

      // 반대 순서, 같은 문구여도 마지막에 띄운 쪽의 시간만큼 보인다.
      controller.toast("같은 안내");
      vi.advanceTimersByTime(3000);
      flashToast("같은 안내");
      vi.advanceTimersByTime(1000);
      expect(useWorkbenchStore.getState().toast).toBe("같은 안내");
      vi.advanceTimersByTime(FLASH_TOAST_MS - 1000);
      expect(useWorkbenchStore.getState().toast).toBeNull();

      // 그사이 직접 띄운 더 중요한 안내는 어느 타이머도 지우지 않는다.
      controller.toast("잠깐 안내");
      useWorkbenchStore.getState().setToast("탭 상한 안내");
      vi.advanceTimersByTime(10_000);
      expect(useWorkbenchStore.getState().toast).toBe("탭 상한 안내");
    } finally {
      controller.dispose();
      vi.useRealTimers();
      useWorkbenchStore.setState({ toast: null });
    }
  });
});

describe("workbench store — 끌어 놓기(04-ui §2-5): dockPane / swapPanes / detachPaneToNewTab(atIndex) / applyLayoutDrop", () => {
  const meta = (leafId: string) => ({
    leafId,
    viewId: `v-${leafId}`,
    sessionId: `s-${leafId}`,
    workloadId: `w-${leafId}`,
    title: leafId,
    cwd: null as string | null,
    phase: "live" as const,
    error: null,
    usage: null,
    flowBlocked: false,
  });
  const leaf = (id: string) => makeLeaf(id, `v-${id}`, `s-${id}`);
  const row = (a: string, b: string) =>
    split(leaf(a), a, { id: b, view_id: `v-${b}`, session_id: `s-${b}` }, { splitId: `sp-${a}-${b}`, axis: "row" })!;
  const rootOf = (tabId: string): SplitNode | null => {
    const tab = useWorkbenchStore.getState().tabs.find((candidate) => candidate.id === tabId);
    return tab?.kind === "terminal" ? tab.root : null;
  };
  const tabIds = () => useWorkbenchStore.getState().tabs.map((tab) => tab.id);

  /** t1 = a|b, t2 = c|d, t3 = e(혼자). 활성 t1, 초점 a. */
  function seed() {
    resetStore();
    useWorkbenchStore.setState({
      tabs: [
        { kind: "terminal", id: "t1", title: "탭 1", root: row("a", "b") },
        { kind: "terminal", id: "t2", title: "탭 2", root: row("c", "d") },
        { kind: "terminal", id: "t3", title: "탭 3", root: leaf("e") },
      ],
      panes: { a: meta("a"), b: meta("b"), c: meta("c"), d: meta("d"), e: meta("e") },
      activeTabId: "t1",
      focusedLeafId: "a",
    });
  }

  /** t2를 창 8개(p0…p7)로 채운다 — 탭당 상한. */
  function fillT2() {
    const ids = ["p0", "p1", "p2", "p3", "p4", "p5", "p6", "p7"];
    let root: SplitNode = leaf(ids[0]);
    for (let i = 1; i < ids.length; i += 1) {
      root = { kind: "split", id: `full-${i}`, axis: "row", ratio: 0.5, first: root, second: leaf(ids[i]) };
    }
    useWorkbenchStore.setState((s) => ({
      tabs: s.tabs.map((tab) => (tab.id === "t2" && tab.kind === "terminal" ? { ...tab, root } : tab)),
      panes: { ...s.panes, ...Object.fromEntries(ids.map((id) => [id, meta(id)])) },
    }));
  }

  /** 데몬·xterm 없이 재배치 메서드만 부르는 controller. */
  function makeController(): SessionController {
    const registry = {
      setFitHandler: () => undefined,
      setVisibilityListener: () => undefined,
      get: () => undefined,
      disposeAll: () => undefined,
    } as unknown as TerminalRegistry;
    const client = {
      events: { subscribe: () => ({ dispose: () => undefined }) },
      systemSnapshot: async () => {
        throw new Error("unused");
      },
      interventionList: async () => [],
      agentSessionList: async () => [],
    } as unknown as DaemonClient;
    return new SessionController({ client, registry, platform: "windows" });
  }

  it("dockPane rearranges inside a tab: same leaf objects, focus follows, the tab stays active", () => {
    seed();
    const a = findLeaf(rootOf("t1"), "a");
    expect(useWorkbenchStore.getState().dockPane("b", "a", "top", "dock-1")).toBe(true);
    expect(layoutShape(rootOf("t1"))).toBe('column("b","a")');
    expect(findLeaf(rootOf("t1"), "a")).toBe(a);
    expect(useWorkbenchStore.getState().focusedLeafId).toBe("b");
    expect(useWorkbenchStore.getState().activeTabId).toBe("t1");
  });

  it("dockPane onto the spot the pane already holds keeps the tree and its tuned ratios", () => {
    seed();
    useWorkbenchStore.getState().setRatio("t1", "sp-a-b", 0.7);
    const before = useWorkbenchStore.getState().tabs;
    expect(useWorkbenchStore.getState().dockPane("b", "a", "right", "dock-1")).toBe(true);
    expect(useWorkbenchStore.getState().tabs).toBe(before);
    expect(useWorkbenchStore.getState().focusedLeafId).toBe("b");
  });

  it("dockPane across tabs docks beside the target, closes or rebalances the source, and follows focus", () => {
    seed();
    expect(useWorkbenchStore.getState().dockPane("e", "c", "left", "dock-1")).toBe(true);
    expect(tabIds()).toEqual(["t1", "t2"]); // 비어 버린 t3는 닫힌다
    expect(layoutShape(rootOf("t2"))).toBe('row("e","c","d")');
    expect(useWorkbenchStore.getState().activeTabId).toBe("t2");
    expect(useWorkbenchStore.getState().focusedLeafId).toBe("e");
    expect(useWorkbenchStore.getState().dockPane("a", "d", "bottom", "dock-2")).toBe(true);
    expect(layoutShape(rootOf("t1"))).toBe('"b"');
    expect(layoutShape(rootOf("t2"))).toBe('row("e","c",column("d","a"))');
  });

  it("dockPane refuses a full target across tabs, yet rearranges freely inside a full tab", () => {
    seed();
    fillT2();
    const before = useWorkbenchStore.getState().tabs;
    expect(useWorkbenchStore.getState().dockPane("a", "p0", "left", "x")).toBe(false);
    expect(useWorkbenchStore.getState().tabs).toBe(before);
    expect(useWorkbenchStore.getState().dockPane("p7", "p0", "left", "y")).toBe(true);
    expect(layoutShape(rootOf("t2")).startsWith('row("p7","p0"')).toBe(true);
  });

  it("swapPanes trades places inside a tab and keeps the split id and ratio", () => {
    seed();
    useWorkbenchStore.getState().setRatio("t1", "sp-a-b", 0.7);
    expect(useWorkbenchStore.getState().swapPanes("a", "b")).toBe(true);
    const root = rootOf("t1");
    expect(layoutShape(root)).toBe('row("b","a")');
    expect(root?.kind === "split" ? [root.id, root.ratio] : null).toEqual(["sp-a-b", 0.7]);
    expect(useWorkbenchStore.getState().focusedLeafId).toBe("a");
    expect(useWorkbenchStore.getState().activeTabId).toBe("t1");
  });

  it("swapPanes across tabs swaps both panes and follows the dragged one to its new tab", () => {
    seed();
    expect(useWorkbenchStore.getState().swapPanes("a", "d")).toBe(true);
    expect(layoutShape(rootOf("t1"))).toBe('row("d","b")');
    expect(layoutShape(rootOf("t2"))).toBe('row("c","a")');
    expect(useWorkbenchStore.getState().activeTabId).toBe("t2");
    expect(useWorkbenchStore.getState().focusedLeafId).toBe("a");
    expect(useWorkbenchStore.getState().swapPanes("a", "a")).toBe(false);
    expect(useWorkbenchStore.getState().swapPanes("a", "nope")).toBe(false);
  });

  it("detachPaneToNewTab(atIndex) opens the new tab at the dropped gap (clamped to the ends)", () => {
    seed();
    expect(useWorkbenchStore.getState().detachPaneToNewTab("b", "n1", "새 탭", 0)).toBe(true);
    expect(tabIds()).toEqual(["n1", "t1", "t2", "t3"]);
    expect(useWorkbenchStore.getState().detachPaneToNewTab("d", "n2", "새 탭", 99)).toBe(true);
    expect(tabIds()).toEqual(["n1", "t1", "t2", "t3", "n2"]);
  });

  it("controller.applyLayoutDrop sends each drop through the matching rearrangement and cap message", () => {
    seed();
    const controller = makeController();

    expect(controller.applyLayoutDrop({ kind: "reorder-tab", tabId: "t3", toIndex: 0 })).toBe(true);
    expect(tabIds()).toEqual(["t3", "t1", "t2"]);

    expect(controller.applyLayoutDrop({ kind: "detach-pane", leafId: "b", atIndex: 1 })).toBe(true);
    const detached = useWorkbenchStore.getState().activeTabId;
    expect(tabIds()).toEqual(["t3", detached, "t1", "t2"]);

    expect(controller.applyLayoutDrop({ kind: "swap-panes", leafId: "c", targetLeafId: "e" })).toBe(true);
    expect(layoutShape(rootOf("t3"))).toBe('"c"');
    expect(layoutShape(rootOf("t2"))).toBe('row("e","d")');

    expect(controller.applyLayoutDrop({ kind: "dock-pane", leafId: "e", targetLeafId: "d", edge: "top" })).toBe(true);
    expect(layoutShape(rootOf("t2"))).toBe('column("e","d")');

    expect(controller.applyLayoutDrop({ kind: "merge-tab", sourceTabId: "t3", targetTabId: "t2" })).toBe(true);
    expect(tabIds()).toEqual([detached, "t1", "t2"]);

    expect(controller.applyLayoutDrop({ kind: "move-pane-to-tab", leafId: "a", targetTabId: "t2" })).toBe(true);
    expect(tabIds()).toEqual([detached, "t2"]); // 비어 버린 t1은 닫힌다

    // 다른 탭으로 붙일 때의 상한은 분할과 같은 문구로 거절하고, 배치는 그대로다.
    fillT2();
    const before = useWorkbenchStore.getState().tabs;
    expect(controller.applyLayoutDrop({ kind: "dock-pane", leafId: "b", targetLeafId: "p0", edge: "left" })).toBe(false);
    expect(useWorkbenchStore.getState().toast).toBe(paneLimitText(8));
    expect(useWorkbenchStore.getState().tabs).toBe(before);
  });

  it("controller.applyLayoutDrop brings a lone pane's tab back into view when its header was dropped on a gap", () => {
    seed();
    // 끌며 머물러 다른 탭(t2)을 열어 둔 상태에서, 혼자인 창 e의 헤더를 틈에 놓았다.
    useWorkbenchStore.getState().setActiveTab("t2");
    const controller = makeController();
    expect(controller.applyLayoutDrop({ kind: "reorder-tab", tabId: "t3", toIndex: 0, focusLeafId: "e" })).toBe(true);
    expect(tabIds()).toEqual(["t3", "t1", "t2"]);
    expect(useWorkbenchStore.getState().activeTabId).toBe("t3");
    expect(useWorkbenchStore.getState().focusedLeafId).toBe("e");
    // 탭을 직접 끈 순서 바꾸기는 보고 있던 탭을 바꾸지 않는다.
    expect(controller.applyLayoutDrop({ kind: "reorder-tab", tabId: "t1", toIndex: 2 })).toBe(true);
    expect(tabIds()).toEqual(["t3", "t2", "t1"]);
    expect(useWorkbenchStore.getState().activeTabId).toBe("t3");
  });
});

describe("workbench store — 배치 편집(04-ui §2-6): placeNewPane / page", () => {
  const meta = (leafId: string) => ({
    leafId,
    viewId: `v-${leafId}`,
    sessionId: `s-${leafId}`,
    workloadId: `w-${leafId}`,
    title: leafId,
    cwd: null as string | null,
    phase: "live" as const,
    error: null,
    usage: null,
    flowBlocked: false,
  });
  const leaf = (id: string) => makeLeaf(id, `v-${id}`, `s-${id}`);
  const rootOf = (tabId: string): SplitNode | null => {
    const tab = useWorkbenchStore.getState().tabs.find((candidate) => candidate.id === tabId);
    return tab?.kind === "terminal" ? tab.root : null;
  };
  const tabIds = () => useWorkbenchStore.getState().tabs.map((tab) => tab.id);
  const newPane = (id: string) => ({
    leafId: id,
    viewId: `v-${id}`,
    sessionId: `s-${id}`,
    workloadId: `w-${id}`,
    title: id,
    cwd: "/work",
    phase: "replaying" as const,
  });
  const ids = { splitId: "x", tabId: "fresh", tabTitle: "work" };

  /** t1 = a|b(초점 a), t2 = 빈 탭, m1 = mission. */
  function seed() {
    resetStore();
    useWorkbenchStore.setState({
      tabs: [
        {
          kind: "terminal",
          id: "t1",
          title: "탭 1",
          root: split(leaf("a"), "a", { id: "b", view_id: "v-b", session_id: "s-b" }, { splitId: "sp-ab", axis: "row" })!,
        },
        { kind: "terminal", id: "t2", title: "빈 탭", root: null },
        { kind: "mission", id: "m1", title: "미션", missionId: "m" },
      ],
      panes: { a: meta("a"), b: meta("b") },
      activeTabId: "t1",
      focusedLeafId: "a",
    });
  }

  it("places a new pane in a tab beside its focused pane, replaying, and follows it", () => {
    seed();
    expect(useWorkbenchStore.getState().placeNewPane({ kind: "tab", tabId: "t1" }, newPane("u"), ids)).toBe("t1");
    const s = useWorkbenchStore.getState();
    expect(layoutShape(rootOf("t1"))).toBe('row("a","u","b")');
    expect(s.panes.u).toMatchObject({ sessionId: "s-u", workloadId: "w-u", phase: "replaying", cwd: "/work" });
    expect(s.focusedLeafId).toBe("u");
    expect(s.activeTabId).toBe("t1");
  });

  it("fills an empty tab, docks beside a given pane edge, and opens a new tab at a gap", () => {
    seed();
    expect(useWorkbenchStore.getState().placeNewPane({ kind: "tab", tabId: "t2" }, newPane("u"), ids)).toBe("t2");
    expect(layoutShape(rootOf("t2"))).toBe('"u"');
    expect(useWorkbenchStore.getState().activeTabId).toBe("t2");
    expect(
      useWorkbenchStore.getState().placeNewPane({ kind: "beside", leafId: "b", edge: "bottom" }, newPane("v"), { ...ids, splitId: "x2" }),
    ).toBe("t1");
    expect(layoutShape(rootOf("t1"))).toBe('row("a",column("b","v"))');
    expect(
      useWorkbenchStore.getState().placeNewPane({ kind: "new-tab", atIndex: 1 }, newPane("w"), { ...ids, tabId: "n1" }),
    ).toBe("n1");
    expect(tabIds()).toEqual(["t1", "n1", "t2", "m1"]);
    expect(useWorkbenchStore.getState().tabs[1]).toMatchObject({ kind: "terminal", title: "work" });
    expect(layoutShape(rootOf("n1"))).toBe('"w"');
  });

  it("refuses a mission tab, an unknown pane, an existing leaf or tab id, and a full tab — changing nothing", () => {
    seed();
    const before = useWorkbenchStore.getState().tabs;
    const store = useWorkbenchStore.getState();
    expect(store.placeNewPane({ kind: "tab", tabId: "m1" }, newPane("u"), ids)).toBeNull();
    expect(store.placeNewPane({ kind: "beside", leafId: "nope", edge: "left" }, newPane("u"), ids)).toBeNull();
    expect(store.placeNewPane({ kind: "tab", tabId: "t1" }, newPane("a"), ids)).toBeNull();
    expect(store.placeNewPane({ kind: "new-tab", atIndex: 0 }, newPane("u"), { ...ids, tabId: "t1" })).toBeNull();
    expect(useWorkbenchStore.getState().tabs).toBe(before);

    const full = ["p0", "p1", "p2", "p3", "p4", "p5", "p6", "p7"];
    let root: SplitNode = leaf(full[0]);
    for (let i = 1; i < full.length; i += 1) {
      root = { kind: "split", id: `full-${i}`, axis: "row", ratio: 0.5, first: root, second: leaf(full[i]) };
    }
    useWorkbenchStore.setState((s) => ({
      tabs: s.tabs.map((tab) => (tab.id === "t2" && tab.kind === "terminal" ? { ...tab, root } : tab)),
    }));
    const beforeFull = useWorkbenchStore.getState().tabs;
    expect(useWorkbenchStore.getState().placeNewPane({ kind: "tab", tabId: "t2" }, newPane("u"), ids)).toBeNull();
    expect(useWorkbenchStore.getState().placeNewPane({ kind: "beside", leafId: "p3", edge: "top" }, newPane("u"), ids)).toBeNull();
    expect(useWorkbenchStore.getState().tabs).toBe(beforeFull);
  });

  it("setPage opens the layout editor and comes back, closing any modal on the way", () => {
    seed();
    useWorkbenchStore.getState().openModal({ kind: "palette" });
    useWorkbenchStore.getState().setPage("layout");
    expect(useWorkbenchStore.getState().page).toBe("layout");
    expect(useWorkbenchStore.getState().modal).toBeNull();
    useWorkbenchStore.getState().setPage("terminal");
    expect(useWorkbenchStore.getState().page).toBe("terminal");
  });
});

describe("workbench store — 배경 탭 창 닫기의 초점·화면 전환(04-ui §2-6 리뷰)", () => {
  const leaf = (id: string) => makeLeaf(id, `v-${id}`, `s-${id}`);
  const pair = (a: string, b: string, splitId: string) =>
    split(leaf(a), a, { id: b, view_id: `v-${b}`, session_id: `s-${b}` }, { splitId, axis: "row" })!;

  it("closing panes of a background tab keeps the focus of the tab being viewed", () => {
    resetStore();
    useWorkbenchStore.setState({
      tabs: [
        { kind: "terminal", id: "t1", title: "a|b", root: pair("a", "b", "sp1") },
        { kind: "terminal", id: "t2", title: "c|d", root: pair("c", "d", "sp2") },
      ],
      activeTabId: "t1",
      focusedLeafId: "b",
    });
    useWorkbenchStore.getState().applyClose("c");
    expect(useWorkbenchStore.getState().focusedLeafId).toBe("b");
    useWorkbenchStore.getState().applyClose("d");
    useWorkbenchStore.getState().closeTab("t2");
    expect(useWorkbenchStore.getState().focusedLeafId).toBe("b");
    // 보고 있는(초점) 창을 닫으면 여전히 옆 창으로 옮긴다.
    useWorkbenchStore.getState().applyClose("b");
    expect(useWorkbenchStore.getState().focusedLeafId).toBe("a");
  });

  it("switching pages drops a tab rename that the hidden page could never finish", () => {
    resetStore();
    useWorkbenchStore.setState({ tabs: [{ kind: "terminal", id: "t1", title: "x", root: null }], renamingTabId: "t1" });
    useWorkbenchStore.getState().setPage("layout");
    expect(useWorkbenchStore.getState().renamingTabId).toBeNull();
    useWorkbenchStore.getState().setPage("terminal");
    expect(useWorkbenchStore.getState().page).toBe("terminal");
  });
});

describe("workbench store — 탭 순환(cycleTab): 트랙패드 스와이프·다음/이전 탭 단축키", () => {
  function threeTabs(): void {
    resetStore();
    const store = useWorkbenchStore.getState();
    store.addTab("t1", "탭 1");
    store.addTab("t2", "탭 2");
    store.addTab("t3", "탭 3");
    useWorkbenchStore.setState({ activeTabId: "t1" });
  }

  it("탭 배열 순서를 따라 움직이고, 순환을 끄면 끝에서 멈춘다", () => {
    threeTabs();
    expect(useWorkbenchStore.getState().cycleTab(1, false)).toBe(true);
    expect(useWorkbenchStore.getState().activeTabId).toBe("t2");
    expect(useWorkbenchStore.getState().cycleTab(1, false)).toBe(true);
    expect(useWorkbenchStore.getState().activeTabId).toBe("t3");
    // 마지막 탭에서 더 밀어도 바뀌지 않는다(false를 돌려준다).
    expect(useWorkbenchStore.getState().cycleTab(1, false)).toBe(false);
    expect(useWorkbenchStore.getState().activeTabId).toBe("t3");
    expect(useWorkbenchStore.getState().cycleTab(-1, false)).toBe(true);
    expect(useWorkbenchStore.getState().activeTabId).toBe("t2");
  });

  it("첫 탭에서 뒤로 가면 순환을 끈 동안에는 멈춘다", () => {
    threeTabs();
    expect(useWorkbenchStore.getState().cycleTab(-1, false)).toBe(false);
    expect(useWorkbenchStore.getState().activeTabId).toBe("t1");
  });

  it("순환을 켜면 끝에서 반대편 끝으로 간다", () => {
    threeTabs();
    expect(useWorkbenchStore.getState().cycleTab(-1, true)).toBe(true);
    expect(useWorkbenchStore.getState().activeTabId).toBe("t3");
    expect(useWorkbenchStore.getState().cycleTab(1, true)).toBe(true);
    expect(useWorkbenchStore.getState().activeTabId).toBe("t1");
  });

  it("탭이 2개 미만이면 아무 일도 하지 않는다", () => {
    resetStore();
    expect(useWorkbenchStore.getState().cycleTab(1, true)).toBe(false);
    useWorkbenchStore.getState().addTab("t1", "탭 1");
    expect(useWorkbenchStore.getState().cycleTab(1, true)).toBe(false);
    expect(useWorkbenchStore.getState().cycleTab(-1, false)).toBe(false);
    expect(useWorkbenchStore.getState().activeTabId).toBe("t1");
  });

  it("setActiveTab을 거치므로 초점이 새 탭 안의 pane으로 따라간다(클릭과 같다)", () => {
    threeTabs();
    useWorkbenchStore.setState((s) => ({
      tabs: s.tabs.map((tab) =>
        tab.kind === "terminal" && tab.id === "t2"
          ? { ...tab, root: makeLeaf("leaf-2", "view-2", "sess-2") }
          : tab,
      ),
      focusedLeafId: null,
    }));
    expect(useWorkbenchStore.getState().cycleTab(1, false)).toBe(true);
    expect(useWorkbenchStore.getState().focusedLeafId).toBe("leaf-2");
  });

  it("mission 탭도 탭 배열의 한 칸이다(순환에서 건너뛰지 않는다)", () => {
    resetStore();
    useWorkbenchStore.getState().addTab("t1", "탭 1");
    useWorkbenchStore.getState().openMissionTab("mission-1", "작업");
    useWorkbenchStore.setState({ activeTabId: "t1" });
    expect(useWorkbenchStore.getState().cycleTab(1, false)).toBe(true);
    expect(useWorkbenchStore.getState().tabs[useWorkbenchStore.getState().tabs.findIndex((t) => t.id === useWorkbenchStore.getState().activeTabId)].kind).toBe("mission");
  });
});

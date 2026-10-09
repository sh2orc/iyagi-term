import { afterEach, describe, expect, it, vi } from "vitest";
import {
  WORKSPACE_STORAGE_KEY,
  WORKSPACE_STORAGE_KEY_V2,
  decodeWorkspace,
  decodeWorkspaceV1,
  encodeWorkspace,
  readWorkspace,
  saveWorkspace,
  type SavedWorkspace,
} from "./workspaceStorage";
import type { SplitNode } from "../features/terminal/splitTree";

function workspace(): SavedWorkspace {
  const pane = (id: string) => ({ leafId: id, viewId: `view-${id}`, sessionId: `session-${id}`, workloadId: `workload-${id}`, title: "zsh", cwd: "/project", phase: "live" as const, error: null, usage: {cpuCores: 1, residentBytes: 100}, flowBlocked: false });
  return { tabs: [{kind: "terminal", id: "tab", title: "My project", root: {kind: "split", id: "split", axis: "column", ratio: .37, first: {kind: "leaf", id: "a", view_id: "view-a", session_id: "session-a"}, second: {kind: "leaf", id: "b", view_id: "view-b", session_id: "session-b"}}}], panes: {a: pane("a"), b: pane("b")}, activeTabId: "tab", focusedLeafId: "b" };
}

/** v1 저장 형식(kind 없음)을 손으로 만든다 — 마이그레이션 입력용. */
function v1JsonFor(state: SavedWorkspace): string {
  const tabs = state.tabs.map(tab =>
    tab.kind === "terminal" ? { id: tab.id, title: tab.title, root: tab.root } : null,
  ).filter(Boolean);
  return JSON.stringify({ version: 1, tabs, panes: state.panes, activeTabId: state.activeTabId, focusedLeafId: state.focusedLeafId });
}

/** readWorkspace/saveWorkspace가 보는 localStorage를 메모리로 갈아끼운다. */
function stubLocalStorage(initial: Record<string, string> = {}): Map<string, string> {
  const storage = new Map<string, string>(Object.entries(initial));
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => void storage.set(key, value),
    removeItem: (key: string) => void storage.delete(key),
  });
  return storage;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("saved workspace", () => {
  it("preserves a marked conversation even before its replacement PTY exists", () => {
    const before = workspace();
    before.panes.a = { ...before.panes.a, sessionId: null, workloadId: null, resume: {
      recordId: null, agent: "codex", agentSessionId: "saved-thread", cwd: "/project", program: null, title: null,
    } };
    const restored = decodeWorkspace(encodeWorkspace(before))!;
    expect(restored.panes.a.resume).toEqual(before.panes.a.resume);
    expect(restored.panes.a.phase).toBe("exited");
    expect(decodeWorkspace(encodeWorkspace(restored))?.panes.a.resume).toEqual(before.panes.a.resume);
  });

  it("rejects invalid saved conversation IDs without losing valid terminal history", () => {
    const before = workspace();
    const raw = JSON.parse(encodeWorkspace(before));
    raw.panes.a.resume = { agent: "codex", agentSessionId: "--yolo", cwd: "/project" };
    const restored = decodeWorkspace(JSON.stringify(raw))!;
    expect(restored.panes.a.resume).toBeNull();
    expect(restored.panes.a.sessionId).toBe("session-a");
  });
  it("round-trips tab names, split ratios, selection and session references without transient metrics", () => {
    const before = workspace();
    const restored = decodeWorkspace(encodeWorkspace(before))!;
    expect(restored.tabs).toEqual(before.tabs);
    expect(restored.activeTabId).toBe("tab");
    expect(restored.focusedLeafId).toBe("b");
    expect(restored.panes.a).toMatchObject({ sessionId: "session-a", workloadId: "workload-a", phase: "replaying", usage: null });
  });
  it("prunes failed launches and collapses their split instead of restoring a new launch", () => {
    const before = workspace();
    before.panes.a.sessionId = null;
    const restored = decodeWorkspace(encodeWorkspace(before))!;
    const root = restored.tabs[0].kind === "terminal" ? restored.tabs[0].root : null;
    expect(root).toMatchObject({kind: "leaf", id: "b"});
    expect(Object.keys(restored.panes)).toEqual(["b"]);
  });
  it("rejects corrupt, unsupported and duplicate session layouts", () => {
    expect(decodeWorkspace("{broken")).toBeNull();
    expect(decodeWorkspace('{"version":1}')).toBeNull(); // v1 페이로드는 v2 decoder가 거절
    const duplicate = workspace();
    duplicate.panes.b.sessionId = duplicate.panes.a.sessionId;
    // v2: 세션 중복 탭은 그 탭만 격리된다(전체 폐기 대신 — 05 §2).
    const quarantined = decodeWorkspace(encodeWorkspace(duplicate))!;
    expect(quarantined.tabs).toEqual([]);
    expect(quarantined.panes).toEqual({});
    expect(decodeWorkspace(" ".repeat(512_001))).toBeNull(); // 512,000 bytes 한도 유지
  });
  it("keeps restored focus inside the selected tab", () => {
    const before = workspace();
    const root = before.tabs[0].kind === "terminal" ? before.tabs[0].root : null;
    if (!root || root.kind !== "split") throw new Error("expected split");
    before.tabs = [
      { kind: "terminal", id: "first", title: "First", root: root.first },
      { kind: "terminal", id: "second", title: "Second", root: root.second },
    ];
    before.activeTabId = "second";
    before.focusedLeafId = "a";
    expect(decodeWorkspace(encodeWorkspace(before))?.focusedLeafId).toBe("b");
  });
});

describe("workspace v2 — mission tab persistence (05-ui §2)", () => {
  it("mission/agent-view 탭은 참조만 저장하고 복원된다(제목은 폴백 공백)", () => {
    const before = workspace();
    before.tabs = [
      ...before.tabs,
      { kind: "mission", id: "mt", title: "스냅샷에 없는 제목", missionId: "m-1" },
      { kind: "agent-view", id: "at", title: "task 뷰", missionId: "m-1", taskId: "t-1" },
    ];
    const encoded = encodeWorkspace(before);
    expect(JSON.parse(encoded).version).toBe(2);
    const restored = decodeWorkspace(encoded)!;
    expect(restored.tabs.find(t => t.id === "mt")).toEqual({ kind: "mission", id: "mt", title: "", missionId: "m-1" });
    expect(restored.tabs.find(t => t.id === "at")).toEqual({ kind: "agent-view", id: "at", title: "", missionId: "m-1", taskId: "t-1" });
    // mission 탭이 활성 탭이면 leaf 초점은 없다.
    before.activeTabId = "mt";
    expect(decodeWorkspace(encodeWorkspace(before))?.focusedLeafId).toBeNull();
  });

  it("malformed v2 entry는 해당 탭만 격리하고 나머지 탭·pane을 복원한다", () => {
    const good = workspace();
    const parsed = JSON.parse(encodeWorkspace(good));
    // 잘못된 terminal 탭(비율 9) — 뒤의 좋은 탭은 살아야 한다.
    const badTerminal: { kind: string; id: string; title: string; root: SplitNode } = {
      kind: "terminal", id: "tab-bad", title: "bad",
      root: { kind: "split", id: "sb", axis: "row", ratio: 9, first: { kind: "leaf", id: "x", view_id: "vx", session_id: "sx" }, second: { kind: "leaf", id: "y", view_id: "vy", session_id: "sy" } },
    };
    // missionId 없는 mission 탭, missionId 중복 탭.
    const badMission = { kind: "mission", id: "m-bad" };
    const dupMissionA = { kind: "mission", id: "ma", missionId: "same" };
    const dupMissionB = { kind: "mission", id: "mb", missionId: "same" };
    parsed.tabs = [badTerminal, ...parsed.tabs, badMission, dupMissionA, dupMissionB];
    const restored = decodeWorkspace(JSON.stringify(parsed))!;
    expect(restored.tabs.map(t => ({ kind: t.kind, id: t.id }))).toEqual([
      { kind: "terminal", id: "tab" },
      { kind: "mission", id: "ma" },
    ]);
    // 격리된 탭의 pane은 orphan로 남지 않는다.
    expect(restored.panes).toHaveProperty("a");
    expect(restored.panes).toHaveProperty("b");
    expect(Object.keys(restored.panes)).toHaveLength(2);
    expect(restored.activeTabId).toBe("tab");
  });
});

describe("workspace v1 → v2 migration (U01)", () => {
  it("v1 페이로드(kind 없음)를 terminal로 마이그레이션해 id/title/root/panes/ratio/focus를 그대로 보존한다", () => {
    const before = workspace();
    const restored = decodeWorkspaceV1(v1JsonFor(before))!;
    expect(restored.tabs).toEqual(before.tabs.map(tab => ({ ...tab, kind: "terminal" })));
    // pane은 참조 필드가 그대로 이어진다(복원 규칙대로 phase/usage는 정규화).
    expect(restored.panes.a).toMatchObject({
      sessionId: before.panes.a.sessionId,
      workloadId: before.panes.a.workloadId,
      viewId: before.panes.a.viewId,
      title: before.panes.a.title,
      cwd: before.panes.a.cwd,
      phase: "replaying",
      usage: null,
    });
    expect(restored.panes.b).toMatchObject({ sessionId: "session-b" });
    expect(restored.activeTabId).toBe("tab");
    expect(restored.focusedLeafId).toBe("b");
    const root = restored.tabs[0].kind === "terminal" ? restored.tabs[0].root : null;
    if (!root || root.kind !== "split") throw new Error("expected split");
    expect(root.ratio).toBe(0.37);
    // 깨진 v1은 여전히 전체 거부(부분 복원 없음).
    expect(decodeWorkspaceV1('{"version":1,"tabs":[{"id":"x"}],"panes":{}}')).toBeNull();
    expect(decodeWorkspaceV1('{"version":2,"tabs":[],"panes":{}}')).toBeNull();
  });

  it("read 순서는 v2 → 유효한 v1 → 빈 상태이고, v2 저장 후에도 v1 키를 지우지 않는다", () => {
    const v1State = workspace();
    const v1 = v1JsonFor(v1State);
    const storage = stubLocalStorage({ [WORKSPACE_STORAGE_KEY]: v1 });
    // v2 없음 → v1 마이그레이션.
    let restored = readWorkspace()!;
    expect(restored.tabs[0]).toMatchObject({ kind: "terminal", id: "tab" });
    // 저장은 v2 키에만.
    saveWorkspace(restored);
    expect(storage.has(WORKSPACE_STORAGE_KEY_V2)).toBe(true);
    expect(JSON.parse(storage.get(WORKSPACE_STORAGE_KEY_V2)!).version).toBe(2);
    expect(storage.get(WORKSPACE_STORAGE_KEY)).toBe(v1); // 자동 삭제 금지(05 §2)
    // v2가 있으면 v2가 이긴다.
    const v2State = workspace();
    v2State.tabs = [{ kind: "terminal", id: "v2-tab", title: "V2", root: v2State.tabs[0].kind === "terminal" ? v2State.tabs[0].root : null }];
    storage.set(WORKSPACE_STORAGE_KEY_V2, encodeWorkspace(v2State));
    restored = readWorkspace()!;
    expect(restored.tabs[0].id).toBe("v2-tab");
    // 둘 다 없으면 빈 상태.
    storage.clear();
    expect(readWorkspace()).toBeNull();
  });
});

describe("scheduleWorkspaceSave — LocalStorage write throttle", () => {
  /** 모듈 상태(lastWriteAt·pending)가 테스트 사이에 새지 않게 매번 새로 불러온다. */
  async function freshModule() {
    vi.resetModules();
    return import("./workspaceStorage");
  }
  const withTitle = (base: SavedWorkspace, title: string): SavedWorkspace =>
    ({ ...base, panes: { ...base.panes, a: { ...base.panes.a, title } } });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("writes the first change at once, then coalesces rapid title churn into one trailing write", async () => {
    vi.useFakeTimers();
    const storage = stubLocalStorage();
    const setItem = vi.spyOn(globalThis.localStorage, "setItem");
    const mod = await freshModule();
    const base = workspace();

    mod.scheduleWorkspaceSave(withTitle(base, "step 0"));
    expect(setItem).toHaveBeenCalledTimes(1);

    for (const step of [1, 2, 3, 4, 5, 6, 7, 8]) {
      vi.advanceTimersByTime(50);
      mod.scheduleWorkspaceSave(withTitle(base, `step ${step}`));
    }
    expect(setItem).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(mod.WORKSPACE_SAVE_INTERVAL_MS);
    expect(setItem).toHaveBeenCalledTimes(2);
    const saved = mod.decodeWorkspace(storage.get(mod.WORKSPACE_STORAGE_KEY_V2) ?? null);
    expect(saved?.panes.a.title).toBe("step 8");
  });

  it("flushWorkspaceSave writes the pending change immediately and leaves nothing for the timer", async () => {
    vi.useFakeTimers();
    stubLocalStorage();
    const setItem = vi.spyOn(globalThis.localStorage, "setItem");
    const mod = await freshModule();
    const base = workspace();

    mod.scheduleWorkspaceSave(withTitle(base, "one"));
    mod.scheduleWorkspaceSave(withTitle(base, "two"));
    expect(setItem).toHaveBeenCalledTimes(1);

    mod.flushWorkspaceSave();
    expect(setItem).toHaveBeenCalledTimes(2);
    vi.advanceTimersByTime(mod.WORKSPACE_SAVE_INTERVAL_MS * 3);
    expect(setItem).toHaveBeenCalledTimes(2);
  });

  it("does not write again for the same state reference", async () => {
    vi.useFakeTimers();
    stubLocalStorage();
    const setItem = vi.spyOn(globalThis.localStorage, "setItem");
    const mod = await freshModule();
    const state = workspace();

    mod.scheduleWorkspaceSave(state);
    mod.scheduleWorkspaceSave(state);
    vi.advanceTimersByTime(mod.WORKSPACE_SAVE_INTERVAL_MS * 3);
    expect(setItem).toHaveBeenCalledTimes(1);
  });
});

describe("spinner glyph never reaches storage", () => {
  const spinnerFrames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

  it("persistedTitle drops only a leading braille spinner", async () => {
    const { persistedTitle } = await import("../features/terminal/osc");
    expect(persistedTitle("⠧ 오픈월드 검토 | seoul-3d")).toBe("오픈월드 검토 | seoul-3d");
    expect(persistedTitle("⠇⠏  two glyphs")).toBe("two glyphs");
    expect(persistedTitle("plain ⠋ inside")).toBe("plain ⠋ inside");
    expect(persistedTitle("⠋")).toBe("⠋");
    expect(persistedTitle(null)).toBeNull();
  });

  it("encodeWorkspace is identical across spinner frames, so saves are deduplicated", () => {
    const base = workspace();
    const encoded = new Set(spinnerFrames.map((frame) =>
      encodeWorkspace({ ...base, panes: { ...base.panes, a: { ...base.panes.a, title: `${frame} working` } } })));
    expect(encoded.size).toBe(1);
    expect(JSON.parse([...encoded][0]).panes.a.title).toBe("working");
  });

  it("encodeWorkloadMemory is identical across spinner frames", async () => {
    const { encodeWorkloadMemory } = await import("./workloadMemory");
    const encoded = new Set(spinnerFrames.map((frame) =>
      encodeWorkloadMemory({ w1: { title: `${frame} working`, agent: null } })));
    expect(encoded.size).toBe(1);
  });
});

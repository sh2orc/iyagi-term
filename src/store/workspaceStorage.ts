/**
 * Workspace persistence v2 (05-ui §2).
 *
 * v2 adds the tab-kind union: terminal tabs keep their split tree, mission and
 * agent-view tabs persist only identity references (id/missionId[/taskId]) —
 * execution state and conversation bodies never enter storage. Reading tries
 * v2 first, then a valid v1 payload (kind absent → every tab is terminal,
 * preserving id/title/root/panes/ratios/focus exactly, U01), then empty. v2
 * writes never delete the v1 key. A malformed v2 tab entry quarantines only
 * that tab; the others still restore.
 */

import type { PaneMeta, TabState } from "./workbenchStore";
import { persistedTitle } from "../features/terminal/osc";
import type { SplitNode } from "../features/terminal/splitTree";
import { decodeResumeInfo } from "../features/agentSessions/types";

export const WORKSPACE_STORAGE_KEY = "iyagi.workspace.v1";
export const WORKSPACE_STORAGE_KEY_V2 = "iyagi.workspace.v2";
export interface SavedWorkspace {
  tabs: TabState[];
  panes: Record<string, PaneMeta>;
  activeTabId: string | null;
  focusedLeafId: string | null;
}
const text = (value: unknown): value is string => typeof value === "string" && value.length > 0 && value.length <= 4096;

/**
 * 저장되는 탭 표현(05 §2): terminal은 id/title/root, mission은 id/missionId,
 * agent-view는 id/missionId/taskId만 남긴다. 실행 상태·대화 원문은 저장하지
 * 않는다 — kind는 항상 쓴다.
 */
export type PersistedTab =
  | { kind: "terminal"; id: string; title: string; root: SplitNode }
  | { kind: "mission"; id: string; missionId: string }
  | { kind: "agent-view"; id: string; missionId: string; taskId: string };

/** Only layout and session references are stored: never terminal output or env. */
export function encodeWorkspace(state: SavedWorkspace): string {
  const panes: Record<string, PaneMeta> = {};
  const prune = (node: SplitNode): SplitNode | null => {
    if (node.kind === "leaf") {
      const pane = state.panes[node.id];
      if (!pane) return null;
      const resume = decodeResumeInfo(pane.resume);
      const hasSession = Boolean(pane.sessionId && pane.workloadId);
      if (!hasSession && !resume) return null;
      panes[node.id] = {
        leafId: pane.leafId, viewId: pane.viewId,
        sessionId: hasSession ? pane.sessionId : null,
        workloadId: hasSession ? pane.workloadId : null,
        title: persistedTitle(pane.title), cwd: pane.cwd, resume,
        // 사용자가 고른 창별 배경색은 leafId와 함께 보존한다(paneBackground).
        backgroundColor: pane.backgroundColor ?? null,
        usage: null, error: null, flowBlocked: false, phase: hasSession ? "replaying" : "exited",
      };
      return { ...node, session_id: pane.sessionId };
    }
    const first = prune(node.first), second = prune(node.second);
    return first && second ? { ...node, first, second } : first ?? second;
  };
  const tabs = state.tabs.flatMap((tab): PersistedTab[] => {
    if (tab.kind === "mission") return [{ kind: "mission", id: tab.id, missionId: tab.missionId }];
    if (tab.kind === "agent-view") return [{ kind: "agent-view", id: tab.id, missionId: tab.missionId, taskId: tab.taskId }];
    const root = tab.root ? prune(tab.root) : null;
    return root ? [{ kind: "terminal", id: tab.id, title: tab.title, root }] : [];
  });
  const activeTab = state.tabs.find(tab => tab.id === state.activeTabId) ?? null;
  const focusedLeafId = activeTab?.kind === "terminal" ? state.focusedLeafId : null;
  return JSON.stringify({ version: 2, tabs, panes, activeTabId: state.activeTabId, focusedLeafId });
}

/**
 * Shared leaf/split walker for both versions: fills `panes` from the stored
 * pane map, enforces id/session uniqueness, and depth/width limits.
 */
function makeDecoder(value: {
  panes: Record<string, { sessionId: unknown; workloadId: unknown; viewId: unknown; title: unknown; cwd: unknown; resume?: unknown; backgroundColor?: unknown }>;
}) {
  const panes: Record<string, PaneMeta> = {};
  const ids = new Set<string>(), sessions = new Set<string>();
  const node = (n: any, depth = 0): SplitNode => {
    if (!n || depth > 16 || !text(n.id) || ids.has(n.id)) throw new Error("invalid layout");
    ids.add(n.id);
    if (n.kind === "leaf") {
      const p = value.panes[n.id];
      const resume = decodeResumeInfo(p?.resume);
      const hasSession = p && text(p.sessionId) && text(p.workloadId);
      if (!p || (!hasSession && !(p.sessionId == null && p.workloadId == null && resume)) ||
        !text(p.viewId) || !text(p.title) || (hasSession && sessions.has(p.sessionId as string)) || Object.keys(panes).length >= 32) throw new Error("invalid pane");
      const sessionId = hasSession ? p.sessionId as string : null;
      const workloadId = hasSession ? p.workloadId as string : null;
      if (sessionId) sessions.add(sessionId);
      // 완화 상태(08 §2)는 저장하지 않는다 — 데몬 상태라 첫 snapshot이 정한다.
      panes[n.id] = { leafId: n.id, viewId: p.viewId, sessionId, workloadId, title: p.title, cwd: typeof p.cwd === "string" ? p.cwd : null, resume, phase: sessionId ? "replaying" : "exited", error: null, usage: null, flowBlocked: false, relief: { kind: "NONE" }, protected: false, backgroundColor: /^#[0-9a-fA-F]{6}$/.test(String(p.backgroundColor)) ? String(p.backgroundColor).toLowerCase() : null };
      return { kind: "leaf", id: n.id, view_id: p.viewId, session_id: sessionId };
    }
    if (n.kind !== "split" || !["row", "column"].includes(n.axis) || !Number.isFinite(n.ratio) || n.ratio <= 0 || n.ratio >= 1) throw new Error("invalid split");
    return { kind: "split", id: n.id, axis: n.axis, ratio: n.ratio, first: node(n.first, depth + 1), second: node(n.second, depth + 1) };
  };
  return { panes, node, sessions, ids };
}

const leaves = (root: SplitNode): string[] => root.kind === "leaf" ? [root.id] : [...leaves(root.first), ...leaves(root.second)];

/** Decode the v1 payload (kind absent) — every tab migrates to terminal (U01). */
export function decodeWorkspaceV1(raw: string | null): SavedWorkspace | null {
  if (!raw || raw.length > 512_000) return null;
  try {
    const value = JSON.parse(raw);
    if (value?.version !== 1 || !Array.isArray(value.tabs) || value.tabs.length > 32 || !value.panes) return null;
    const { panes, node } = makeDecoder(value);
    const tabIds = new Set<string>();
    const tabs = value.tabs.map((tab: any): TabState => {
      if (!text(tab.id) || tabIds.has(tab.id) || !text(tab.title)) throw new Error("invalid tab");
      tabIds.add(tab.id);
      const before = Object.keys(panes).length;
      const root = node(tab.root);
      if (Object.keys(panes).length - before > 8) throw new Error("too many panes");
      // v1에는 kind가 없다 — 전부 terminal로 migration한다(id/title/root/pane 보존).
      return { kind: "terminal", id: tab.id, title: tab.title, root };
    });
    const activeTab = tabs.find((tab: TabState) => tab.id === value.activeTabId) ?? tabs[0];
    const root = activeTab?.kind === "terminal" ? activeTab.root : null;
    const visible = root ? leaves(root) : [];
    return { tabs, panes, activeTabId: activeTab?.id ?? null, focusedLeafId: visible.includes(value.focusedLeafId) ? value.focusedLeafId : visible[0] ?? null };
  } catch { return null; }
}

/**
 * Decode the v2 payload. Tab entries are validated per-tab: a malformed entry
 * quarantines only itself and the remaining tabs (and their panes) restore.
 */
export function decodeWorkspace(raw: string | null): SavedWorkspace | null {
  if (!raw || raw.length > 512_000) return null;
  try {
    const value = JSON.parse(raw);
    if (value?.version !== 2 || !Array.isArray(value.tabs) || value.tabs.length > 32 || !value.panes) return null;
    const { panes, node, sessions, ids } = makeDecoder(value);
    const tabIds = new Set<string>(), missionIds = new Set<string>(), taskIds = new Set<string>();
    const tabs: TabState[] = [];
    for (const entry of value.tabs) {
      // 탭별 checkpoint: 이 탭이 실패하면 여기까지의 부분 부작용(pane·id·session)을
      // 되돌린다 — 격리된 탭의 orphan pane이 살아남아 attach를 시도하지 않게.
      const nodeIds = [...ids], sessionList = [...sessions], paneKeys = Object.keys(panes);
      const registeredTabs = [...tabIds], registeredMissions = [...missionIds], registeredTasks = [...taskIds];
      try {
        if (entry?.kind === "mission") {
          if (!text(entry.id) || tabIds.has(entry.id) || !text(entry.missionId) || missionIds.has(entry.missionId)) throw new Error("invalid mission tab");
          tabIds.add(entry.id);
          missionIds.add(entry.missionId);
          tabs.push({ kind: "mission", id: entry.id, title: "", missionId: entry.missionId });
          continue;
        }
        if (entry?.kind === "agent-view") {
          if (!text(entry.id) || tabIds.has(entry.id) || !text(entry.missionId) || !text(entry.taskId) || taskIds.has(entry.taskId)) throw new Error("invalid agent-view tab");
          tabIds.add(entry.id);
          taskIds.add(entry.taskId);
          tabs.push({ kind: "agent-view", id: entry.id, title: "", missionId: entry.missionId, taskId: entry.taskId });
          continue;
        }
        if (entry?.kind !== "terminal" || !text(entry.id) || tabIds.has(entry.id) || !text(entry.title)) throw new Error("invalid tab");
        tabIds.add(entry.id);
        const before = Object.keys(panes).length;
        const root = node(entry.root);
        if (Object.keys(panes).length - before > 8) throw new Error("too many panes");
        tabs.push({ kind: "terminal", id: entry.id, title: entry.title, root });
      } catch {
        // 격리(05 §2): 잘못된 탭 하나만 버리고 나머지는 복원한다.
        for (const key of Object.keys(panes)) if (!paneKeys.includes(key)) delete panes[key];
        ids.clear(); for (const v of nodeIds) ids.add(v);
        sessions.clear(); for (const v of sessionList) sessions.add(v);
        tabIds.clear(); for (const v of registeredTabs) tabIds.add(v);
        missionIds.clear(); for (const v of registeredMissions) missionIds.add(v);
        taskIds.clear(); for (const v of registeredTasks) taskIds.add(v);
      }
    }
    const activeTab = tabs.find((tab) => tab.id === value.activeTabId) ?? tabs[0];
    const root = activeTab?.kind === "terminal" ? activeTab.root : null;
    const visible = root ? leaves(root) : [];
    return { tabs, panes, activeTabId: activeTab?.id ?? null, focusedLeafId: visible.includes(value.focusedLeafId) ? value.focusedLeafId : visible[0] ?? null };
  } catch { return null; }
}

/** Read order: v2 → valid v1 → empty. The v1 key is never auto-deleted. */
export function readWorkspace(): SavedWorkspace | null {
  try {
    return decodeWorkspace(localStorage.getItem(WORKSPACE_STORAGE_KEY_V2)) ?? decodeWorkspaceV1(localStorage.getItem(WORKSPACE_STORAGE_KEY));
  } catch { return null; }
}

let lastSaved: string | null = null;
export function saveWorkspace(state: SavedWorkspace): void {
  try {
    const encoded = encodeWorkspace(state);
    if (encoded === lastSaved) return;
    localStorage.setItem(WORKSPACE_STORAGE_KEY_V2, encoded);
    lastSaved = encoded;
  } catch { /* Storage may be disabled or full; live terminals stay usable. */ }
}

/** 워크스페이스 저장 최소 간격. 같은 키를 초당 여러 번 덮어쓰면 WebKit LocalStorage WAL이 GB 단위로 불어난다. */
export const WORKSPACE_SAVE_INTERVAL_MS = 1000;

let pendingWorkspace: SavedWorkspace | null = null;
let lastScheduled: SavedWorkspace | null = null;
let saveTimer: ReturnType<typeof setTimeout> | null = null;
let lastWriteAt = 0;

/**
 * store 구독용 저장. 한동안 쓰지 않았으면 곧바로 쓰고, 간격 안의 변경은 마지막 것
 * 하나만 간격이 끝날 때 쓴다. 터미널 제목·cwd처럼 자주 바뀌는 값이 매번 LocalStorage
 * 쓰기가 되지 않게 한다(인코딩도 실제로 쓸 때 한 번만 한다).
 */
export function scheduleWorkspaceSave(state: SavedWorkspace): void {
  if (state === (pendingWorkspace ?? lastScheduled)) return;
  pendingWorkspace = state;
  if (saveTimer !== null) return;
  const wait = lastWriteAt + WORKSPACE_SAVE_INTERVAL_MS - Date.now();
  if (wait <= 0) {
    flushWorkspaceSave();
    return;
  }
  saveTimer = setTimeout(flushWorkspaceSave, wait);
}

/** 모아 둔 변경을 지금 쓴다(페이지를 떠날 때도 부른다). */
export function flushWorkspaceSave(): void {
  if (saveTimer !== null) clearTimeout(saveTimer);
  saveTimer = null;
  const state = pendingWorkspace;
  pendingWorkspace = null;
  if (state === null) return;
  lastScheduled = state;
  lastWriteAt = Date.now();
  saveWorkspace(state);
}

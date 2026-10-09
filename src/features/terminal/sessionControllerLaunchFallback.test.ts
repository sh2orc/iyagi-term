/**
 * 새 터미널(첫 pane·분할)이 시작 조건이 나빠도 열리는 계약(04-ui §2-4):
 * - 시작 경로를 쓸 수 없으면(CWD_UNAVAILABLE) 가까운 상위 경로 → 프로젝트 root → home → 뿌리.
 * - 고른 셸이 없으면(PROGRAM_NOT_FOUND) 플랫폼 기본 셸 → /bin/sh로 바꿔 열고 알린다.
 * - 바꿔서도 안 되는 실패(세션 한도·기록 공간)는 다시 시도하지 않고 할 일을 알려 준다.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import type { LaunchRequest } from "../../generated/LaunchRequest";
import type { DaemonClient } from "../daemon/client";
import { RpcClientError } from "../daemon/client";
import { t } from "../../i18n";
import { useWorkbenchStore, type PaneMeta } from "../../store/workbenchStore";
import { SessionController, type ShellSpec } from "./sessionController";
import { TerminalRegistry, type TerminalLike } from "./registry";

const HOME = "/Users/me";
const ROOT = `${HOME}/src/iyagi`;

function fakeTerminal(): TerminalLike {
  return {
    open: () => undefined,
    write: () => undefined,
    resize: () => undefined,
    dispose: () => undefined,
    onData: () => ({ dispose: () => undefined }),
    hasSelection: () => false,
    getSelection: () => "",
    attachCustomKeyEventHandler: () => undefined,
    loadAddon: () => undefined,
    element: null,
    options: { fontSize: 13 },
  };
}

/** 실제 실행 파일: `/usr/bin/env -u … <셸> …`에서 셸 자리. */
function shellOf(request: LaunchRequest): string | undefined {
  return request.argv.find(arg => arg.startsWith("/"));
}

function setup(options: {
  missingCwds?: string[];
  missingPrograms?: string[];
  failWith?: RpcClientError;
  resolveShell?: () => ShellSpec;
  projectRoot?: string | null;
}) {
  const launches: LaunchRequest[] = [];
  const client = {
    events: { subscribe: () => ({ dispose: () => undefined }) },
    agentSessionList: async () => [],
    workloadLaunch: async (request: LaunchRequest) => {
      launches.push(request);
      if (options.failWith) throw options.failWith;
      const program = shellOf(request);
      if (program && options.missingPrograms?.includes(program)) {
        throw new RpcClientError("PROGRAM_NOT_FOUND", "program file not found", false, { reason_code: "wrapped_program_missing" });
      }
      if (options.missingCwds?.includes(request.cwd)) {
        throw new RpcClientError("CWD_UNAVAILABLE", "cwd cannot be canonicalized", false, { reason_code: "cwd_missing" });
      }
      return { session_id: `s-${launches.length}`, workload_id: `w-${launches.length}` };
    },
    sessionAttach: () => new Promise(() => undefined),
  } as unknown as DaemonClient;
  const controller = new SessionController({
    client,
    registry: new TerminalRegistry({
      createTerminal: () => fakeTerminal(),
      createDom: () => ({ className: "", parentElement: null, remove: () => undefined }),
    }),
    platform: "darwin",
    resolveShell: options.resolveShell,
    config: { projectRoot: options.projectRoot === undefined ? ROOT : options.projectRoot, home: HOME },
  });
  return { controller, launches };
}

const panes = (): PaneMeta[] => Object.values(useWorkbenchStore.getState().panes);
const toast = (): string | null => useWorkbenchStore.getState().toast;

beforeEach(() => {
  useWorkbenchStore.setState({ tabs: [], activeTabId: null, focusedLeafId: null, panes: {}, workloads: [], toast: null, modal: null });
});

describe("새 터미널: 시작 경로를 쓸 수 없을 때", () => {
  it("프로젝트 root가 사라졌으면 가까운 상위 경로에서 연다", async () => {
    const { controller, launches } = setup({ missingCwds: [ROOT] });
    try {
      controller.newTerminal();
      await vi.waitFor(() => expect(panes()[0]?.sessionId).toBe("s-2"));
      expect(launches.map(r => r.cwd)).toEqual([ROOT, `${HOME}/src`]);
      expect(panes()[0].cwd).toBe(`${HOME}/src`);
      expect(toast()).toBe(t("terminal.session.cwdFallback", { from: ROOT, cwd: `${HOME}/src` }));
    } finally { controller.dispose(); }
  });

  it("분할은 셸이 마지막으로 알린 경로(지워진 worktree)의 가까운 상위 경로에서 연다", async () => {
    const worktree = `${ROOT}/.worktrees/feature-x/crates`;
    const { controller, launches } = setup({ missingCwds: [worktree, `${ROOT}/.worktrees/feature-x`] });
    try {
      controller.newTerminal();
      await vi.waitFor(() => expect(panes()[0]?.sessionId).toBe("s-1"));
      // 첫 pane의 셸이 worktree로 cd했다고 알렸다(OSC 7) — 그 뒤 worktree가 지워졌다.
      useWorkbenchStore.getState().paneTitle(panes()[0].leafId, "zsh", worktree);
      controller.splitFocused("row");
      await vi.waitFor(() => expect(panes().filter(p => p.sessionId)).toHaveLength(2));
      expect(launches.slice(1).map(r => r.cwd)).toEqual([worktree, `${ROOT}/.worktrees/feature-x`, `${ROOT}/.worktrees`]);
      expect(panes().every(p => p.phase !== "failed")).toBe(true);
    } finally { controller.dispose(); }
  });

  it("home 아래가 모두 없으면 home, 그다음 뿌리에서라도 연다", async () => {
    const { controller, launches } = setup({ missingCwds: [ROOT, `${HOME}/src`, HOME] });
    try {
      controller.newTerminal();
      await vi.waitFor(() => expect(panes()[0]?.sessionId).not.toBeNull());
      expect(launches.map(r => r.cwd)).toEqual([ROOT, `${HOME}/src`, HOME, "/"]);
      expect(panes()[0].cwd).toBe("/");
    } finally { controller.dispose(); }
  });
});

describe("새 터미널: 고른 셸을 쓸 수 없을 때", () => {
  const fish: ShellSpec = { program: "/opt/homebrew/bin/fish", argv: ["-l"], label: "fish" };

  it("지워진 셸 프로필이면 기본 셸로 열고 알린다", async () => {
    const { controller, launches } = setup({ missingPrograms: [fish.program], resolveShell: () => fish });
    try {
      controller.newTerminal();
      await vi.waitFor(() => expect(panes()[0]?.sessionId).toBe("s-2"));
      expect(launches.map(shellOf)).toEqual([fish.program, "/bin/zsh"]);
      expect(launches[1].argv.slice(-2)).toEqual(["-l", "-i"]); // 기본 셸은 로그인 인자로
      expect(panes()[0].title).toBe("zsh");
      expect(toast()).toBe(t("terminal.session.shellFallback", { shell: "fish", fallback: "zsh" }));
    } finally { controller.dispose(); }
  });

  it("셸과 경로가 둘 다 바뀌면 둘 다 알린다", async () => {
    const { controller } = setup({ missingPrograms: [fish.program], missingCwds: [ROOT], resolveShell: () => fish });
    try {
      controller.newTerminal();
      await vi.waitFor(() => expect(panes()[0]?.sessionId).not.toBeNull());
      expect(toast()).toBe([
        t("terminal.session.cwdFallback", { from: ROOT, cwd: `${HOME}/src` }),
        t("terminal.session.shellFallback", { shell: "fish", fallback: "zsh" }),
      ].join(" · "));
    } finally { controller.dispose(); }
  });

  it("기본 셸까지 없으면 /bin/sh로 연다", async () => {
    const { controller, launches } = setup({ missingPrograms: [fish.program, "/bin/zsh"], resolveShell: () => fish });
    try {
      controller.newTerminal();
      await vi.waitFor(() => expect(panes()[0]?.sessionId).not.toBeNull());
      expect(launches.map(shellOf)).toEqual([fish.program, "/bin/zsh", "/bin/sh"]);
      expect(panes()[0].title).toBe("sh");
    } finally { controller.dispose(); }
  });
});

describe("바꿔서도 안 되는 실패는 다시 시도하지 않고 할 일을 알린다", () => {
  it("세션 한도", async () => {
    const { controller, launches } = setup({ failWith: new RpcClientError("SESSION_LIMIT", "session limit 32 reached") });
    try {
      controller.newTerminal();
      await vi.waitFor(() => expect(panes()[0]?.phase).toBe("failed"));
      expect(launches).toHaveLength(1);
      expect(panes()[0].error).toBe(
        `${t("terminal.session.launchFailed")} — ${t("terminal.session.limitHint", { count: 32 })} (SESSION_LIMIT)`,
      );
    } finally { controller.dispose(); }
  });

  it("기록 공간 부족", async () => {
    const { controller, launches } = setup({ failWith: new RpcClientError("JOURNAL_LIMIT", "journal open failed") });
    try {
      controller.newTerminal();
      await vi.waitFor(() => expect(panes()[0]?.phase).toBe("failed"));
      expect(launches).toHaveLength(1);
      expect(panes()[0].error).toContain(t("terminal.session.storageFull"));
    } finally { controller.dispose(); }
  });
});

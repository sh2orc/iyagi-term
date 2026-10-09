/**
 * AI 에이전트 세션 이어서 열기(04-ui.md §5).
 *
 * 네 가지 계약을 고정한다:
 * 1) 복원 — PTY가 사라진 저장 pane에 "지금 돌고 있지 않은" 기록이 있으면
 *    닫지 않고 재개 가능한 자리로 남긴다. 일반 터미널은 저널을 재생한다.
 *    어느 쪽이든 자동으로 다시 실행하지 않는다(01 §6).
 * 2) 재개 — 사용자가 눌렀을 때만, 기록된 cwd에서 claude/codex의 재개
 *    인자로 셸 워크로드를 만든다(자율 실행 설정 on/off 모두). 인자로 쓸 수
 *    없는 세션 id나 자리를 못 만든 경우에는 조용히 끝나지 않는다.
 * 3) 배지 — 같은 값의 감지 결과로는 pane을 다시 쓰지 않는다.
 * 4) 목록 seam — mock이 데몬과 같은 순서·중복 제거를 한다.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AgentSessionRecord } from "../../generated/AgentSessionRecord";
import type { LaunchRequest } from "../../generated/LaunchRequest";
import type { DaemonClient, DaemonEvent } from "../daemon/client";
import { RpcClientError } from "../daemon/client";
import { MockDaemonClient } from "../daemon/mockClient";
import { usePreferences } from "../../store/preferences";
import { useWorkbenchStore, type PaneMeta } from "../../store/workbenchStore";
import { MAX_PANES_PER_TAB, makeLeaf } from "./splitTree";
import { paneLimitText } from "../monitor/statusStrings";
import { SessionController } from "./sessionController";
import { TerminalRegistry, type TerminalLike } from "./registry";
import type { AgentResumeInfo } from "../agentSessions/types";
import { t } from "../../i18n";
import { decodeWorkspace, encodeWorkspace } from "../../store/workspaceStorage";

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
  } satisfies TerminalLike;
}

function fakeDom(): { className: string; parentElement: null; remove: () => void } {
  return { className: "", parentElement: null, remove: () => undefined };
}

function registry(): TerminalRegistry {
  return new TerminalRegistry({ createTerminal: () => fakeTerminal(), createDom: fakeDom });
}

function record(overrides: Partial<AgentSessionRecord> = {}): AgentSessionRecord {
  return {
    id: "rec-1",
    workload_id: "w-old",
    pty_session_id: "s-old",
    agent: "claude",
    agent_session_id: "agent-session-0001",
    cwd: "/work/iyagi",
    title: "iyagi",
    program: "/opt/bin/claude",
    source: "registry",
    first_seen_at: "2026-09-12T00:00:00Z",
    last_seen_at: "2026-09-12T01:00:00Z",
    ended_at: "2026-09-12T01:05:00Z",
    end_reason: "workload_exited",
    active: false,
    ...overrides,
  };
}

function savedPane(overrides: Partial<PaneMeta> = {}): PaneMeta {
  return {
    leafId: "leaf-1",
    viewId: "view-1",
    sessionId: "s-old",
    workloadId: "w-old",
    title: "zsh",
    cwd: "/work/iyagi",
    phase: "replaying",
    error: null,
    usage: null,
    flowBlocked: false,
    ...overrides,
  };
}

function seedWorkspace(...panes: PaneMeta[]): void {
  useWorkbenchStore.setState({
    tabs: [
      {
        kind: "terminal",
        id: "tab-1",
        title: "탭 1",
        root: panes.reduce<ReturnType<typeof makeLeaf> | null>(
          (acc, pane) =>
            acc === null
              ? makeLeaf(pane.leafId, pane.viewId, pane.sessionId)
              : ({
                  kind: "split",
                  id: `split-${pane.leafId}`,
                  axis: "row",
                  ratio: 0.5,
                  first: acc,
                  second: makeLeaf(pane.leafId, pane.viewId, pane.sessionId),
                } as ReturnType<typeof makeLeaf>),
          null,
        ),
      },
    ],
    panes: Object.fromEntries(panes.map((pane) => [pane.leafId, pane])),
    activeTabId: "tab-1",
    focusedLeafId: panes[0]?.leafId ?? null,
    workloads: [],
    queue: [],
    revision: 0,
    modal: null,
    toast: null,
  });
}

function resetWorkspace(): void {
  useWorkbenchStore.setState({
    tabs: [],
    panes: {},
    activeTabId: null,
    focusedLeafId: null,
    workloads: [],
    queue: [],
    revision: 0,
    modal: null,
    toast: null,
  });
}

beforeEach(() => {
  resetWorkspace();
  usePreferences.setState({ claudeFullAutonomy: true, codexFullAutonomy: true });
});

describe("복원: 기록이 있으면 pane을 남기고, 없으면 닫는다", () => {
  it("돌고 있지 않은 기록이 일치하면 pane을 재개 가능한 빈 자리로 남긴다", async () => {
    const client = new MockDaemonClient({ resourceIntervalMs: 0 });
    client.seedAgentSessions([record()]);
    seedWorkspace(savedPane());

    const controller = new SessionController({ client, registry: registry(), platform: "darwin" });
    await controller.restoreWorkspace();

    const pane = useWorkbenchStore.getState().panes["leaf-1"];
    expect(pane).toBeDefined();
    expect(pane.phase).toBe("exited");
    expect(pane.sessionId).toBe("s-old");
    expect(pane.workloadId).toBe("w-old");
    expect(pane.agent ?? null).toBeNull();
    expect(pane.viewId).not.toBe("view-1"); // 예전 뷰는 버린다
    expect(pane.resume).toEqual({
      recordId: "rec-1",
      agent: "claude",
      agentSessionId: "agent-session-0001",
      cwd: "/work/iyagi",
      title: "iyagi",
      program: "/opt/bin/claude",
    } satisfies AgentResumeInfo);
    // 자동 재실행 없음: 아무 워크로드도 만들지 않았다.
    expect(useWorkbenchStore.getState().workloads).toEqual([]);
    controller.dispose();
  });

  it("PTY session id로도 기록을 찾는다(workload id가 달라졌을 때)", async () => {
    const client = new MockDaemonClient({ resourceIntervalMs: 0 });
    client.seedAgentSessions([record({ workload_id: "w-other", pty_session_id: "s-old" })]);
    seedWorkspace(savedPane());

    const controller = new SessionController({ client, registry: registry(), platform: "darwin" });
    await controller.restoreWorkspace();

    expect(useWorkbenchStore.getState().panes["leaf-1"].resume?.recordId).toBe("rec-1");
    controller.dispose();
  });

  it.each([
    ["기록이 없으면", [] as AgentSessionRecord[]],
    ["그 세션이 아직 살아 있으면", [record({ active: true })]],
    ["재개 인자가 없는 에이전트면", [record({ agent: "unknown-agent" })]],
    ["다른 워크로드의 기록이면", [record({ workload_id: "w-zzz", pty_session_id: "s-zzz" })]],
  ])("%s 원래 터미널의 출력 복원을 시도한다", async (_name, records) => {
    const client = new MockDaemonClient({ resourceIntervalMs: 0 });
    client.seedAgentSessions(records);
    seedWorkspace(savedPane());

    const controller = new SessionController({ client, registry: registry(), platform: "darwin" });
    await controller.restoreWorkspace();

    expect(useWorkbenchStore.getState().panes["leaf-1"].sessionId).toBe("s-old");
    expect(useWorkbenchStore.getState().panes["leaf-1"].resume ?? null).toBeNull();
    controller.dispose();
  });

  it("agent_session.list가 없는 구 데몬에서도 출력 복원을 시도한다", async () => {
    const client = new MockDaemonClient({ resourceIntervalMs: 0 });
    (client as unknown as { agentSessionList: () => Promise<never> }).agentSessionList = () =>
      Promise.reject(new Error("METHOD_NOT_FOUND"));
    seedWorkspace(savedPane());

    const controller = new SessionController({ client, registry: registry(), platform: "darwin" });
    await controller.restoreWorkspace();

    expect(useWorkbenchStore.getState().panes["leaf-1"].sessionId).toBe("s-old");
    controller.dispose();
  });
});

interface LaunchProbe {
  client: DaemonClient;
  launches: LaunchRequest[];
}

function launchProbe(): LaunchProbe {
  const launches: LaunchRequest[] = [];
  const client = {
    events: { subscribe: () => ({ dispose: () => undefined }) },
    workloadLaunch: async (request: LaunchRequest) => {
      launches.push(request);
      return { session_id: `s-${launches.length}`, workload_id: `w-${launches.length}` };
    },
    sessionAttach: async () => {
      throw new Error("attach not needed in this test");
    },
  } as unknown as DaemonClient;
  return { client, launches };
}

function info(overrides: Partial<AgentResumeInfo> = {}): AgentResumeInfo {
  return {
    recordId: "rec-1",
    agent: "claude",
    agentSessionId: "agent-session-0001",
    cwd: "/work/iyagi",
    title: "iyagi",
    program: "/opt/bin/claude",
    ...overrides,
  };
}

describe("resumeAgentSession: 프로그램·인자·cwd", () => {
  it("Claude 대화 파일이 없으면 저장된 ID를 바꾸지 않고 구체적인 원인을 보여 준다", async () => {
    const probe = launchProbe();
    probe.client.workloadLaunch = vi.fn().mockRejectedValue(new RpcClientError(
      "INVALID_ARGUMENT", "Missing transcript", false, { reason_code: "claude_transcript_missing" },
    ));
    const resume = info();
    seedWorkspace(savedPane({ phase: "exited", resume }));
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    try {
      await controller.resumeAgentSession(resume, { leafId: "leaf-1" });
      const pane = useWorkbenchStore.getState().panes["leaf-1"];
      expect(pane.phase).toBe("failed");
      expect(pane.error).toBe(t("terminal.resume.claudeTranscriptMissing"));
      expect(pane.resume?.agentSessionId).toBe(resume.agentSessionId);
    } finally { controller.dispose(); }
  });

  it.each([
    [
      "claude · 자율 실행 켬",
      info(),
      true,
      ["--dangerously-skip-permissions", "--resume", "agent-session-0001"],
    ],
    ["claude · 자율 실행 끔", info(), false, ["--resume", "agent-session-0001"]],
    ["opencode · 같은 세션 재개", info({ agent: "opencode", program: "/opt/bin/opencode" }), true, ["--session", "agent-session-0001"]],
    [
      "codex · 자율 실행 켬(하위 명령이 먼저)",
      info({ agent: "codex", program: "/opt/bin/codex" }),
      true,
      ["resume", "agent-session-0001", "--dangerously-bypass-approvals-and-sandbox"],
    ],
    [
      "codex · 자율 실행 끔",
      info({ agent: "codex", program: "/opt/bin/codex" }),
      false,
      ["resume", "agent-session-0001"],
    ],
  ])("%s", async (_name, resume, autonomy, argv) => {
    usePreferences.setState({ claudeFullAutonomy: autonomy, codexFullAutonomy: autonomy });
    const probe = launchProbe();
    const controller = new SessionController({
      client: probe.client,
      registry: registry(),
      platform: "windows", // cleanShellCommand가 감싸지 않아 program/argv가 1:1이다
      config: { home: "C:\\Users\\other" },
    });

    await controller.resumeAgentSession(resume, { newPane: true });
    await new Promise((done) => setTimeout(done, 0));

    expect(probe.launches).toHaveLength(1);
    expect(probe.launches[0]).toMatchObject({
      program: resume.program,
      argv,
      mode: "shell",
      cwd: "/work/iyagi", // 포커스 pane의 cwd가 아니라 기록된 cwd
    });
    controller.dispose();
  });

  describe("Claude 제공자 라우팅(설정 → Z.ai Coding Plan)", () => {
    afterEach(() => {
      usePreferences.setState({ claudeProvider: "anthropic", zaiMainModel: "glm-5.3[1m]" });
      useWorkbenchStore.setState({ claudeProviderRouting: false });
    });

    it("설정이 Z.ai이고 데몬이 광고하면 선택자만 붙는다 — 키·환경 변수·--model은 없다", async () => {
      usePreferences.setState({ claudeProvider: "zai-coding-plan", zaiMainModel: "glm-5.3-flash[1m]" });
      useWorkbenchStore.setState({ claudeProviderRouting: true });
      const probe = launchProbe();
      const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });

      await controller.resumeAgentSession(info(), { newPane: true });
      await new Promise((done) => setTimeout(done, 0));

      expect(probe.launches).toHaveLength(1);
      expect(probe.launches[0].claude_provider).toEqual({ kind: "zai_coding_plan", main_model: "glm-5.3-flash[1m]" });
      expect(probe.launches[0].env_overrides).toEqual({});
      expect(probe.launches[0].argv).not.toContain("--model");
      expect(JSON.stringify(probe.launches[0])).not.toMatch(/ANTHROPIC|AUTH_TOKEN|api\.z\.ai/);
      controller.dispose();
    });

    it("Anthropic 설정이면 claude_provider 키 자체가 없다(구 데몬과 같은 요청)", async () => {
      useWorkbenchStore.setState({ claudeProviderRouting: true });
      const probe = launchProbe();
      const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });

      await controller.resumeAgentSession(info(), { newPane: true });
      await new Promise((done) => setTimeout(done, 0));

      expect(probe.launches).toHaveLength(1);
      expect("claude_provider" in probe.launches[0]).toBe(false);
      controller.dispose();
    });

    it("codex 재개에는 Z.ai 설정이 켜져 있어도 붙지 않는다", async () => {
      usePreferences.setState({ claudeProvider: "zai-coding-plan" });
      useWorkbenchStore.setState({ claudeProviderRouting: true });
      const probe = launchProbe();
      const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });

      await controller.resumeAgentSession(info({ agent: "codex", program: "/opt/bin/codex" }), { newPane: true });
      await new Promise((done) => setTimeout(done, 0));

      expect(probe.launches).toHaveLength(1);
      expect("claude_provider" in probe.launches[0]).toBe(false);
      controller.dispose();
    });

    it("구 데몬(라우팅 미광고)이면 실행하지 않고 pane 실패 + 토스트로 거절한다", async () => {
      usePreferences.setState({ claudeProvider: "zai-coding-plan" });
      useWorkbenchStore.setState({ claudeProviderRouting: false });
      const probe = launchProbe();
      const resume = info();
      seedWorkspace(savedPane({ phase: "exited", resume }));
      const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
      try {
        await controller.resumeAgentSession(resume, { leafId: "leaf-1" });
        // 조용한 Anthropic 폴백 금지: 요청 자체가 나가지 않는다.
        expect(probe.launches).toHaveLength(0);
        const pane = useWorkbenchStore.getState().panes["leaf-1"];
        expect(pane.phase).toBe("failed");
        expect(pane.error).toBe(t("settings.zai.launch.daemonOutdated"));
        expect(useWorkbenchStore.getState().toast).toBe(t("settings.zai.launch.daemonOutdated"));
      } finally { controller.dispose(); }
    });
  });

  it("기록에 실행 경로가 없으면 시스템 탐지 결과를 쓴다", async () => {
    const probe = launchProbe();
    const controller = new SessionController({
      client: probe.client,
      registry: registry(),
      platform: "windows",
      resolveAgentProgram: async (kind) => `C:\\tools\\${kind}.exe`,
    });

    await controller.resumeAgentSession(info({ program: null }), { newPane: true });
    await new Promise((done) => setTimeout(done, 0));

    expect(probe.launches[0].program).toBe("C:\\tools\\claude.exe");
    controller.dispose();
  });

  it("실행 파일을 못 찾으면 아무것도 실행하지 않고 문구만 남긴다", async () => {
    const probe = launchProbe();
    const controller = new SessionController({
      client: probe.client,
      registry: registry(),
      platform: "windows",
      resolveAgentProgram: async () => null,
    });

    await controller.resumeAgentSession(info({ program: null }), { newPane: true });

    expect(probe.launches).toHaveLength(0);
    expect(useWorkbenchStore.getState().toast).toContain("Claude Code");
    controller.dispose();
  });

  it("같은 pane에서 이어서 열면 재개 정보를 보존하고 새 PTY를 만든다", async () => {
    const probe = launchProbe();
    const resume = info();
    seedWorkspace(
      savedPane({ sessionId: null, workloadId: null, phase: "exited", resume, cwd: "/elsewhere" }),
    );
    const controller = new SessionController({
      client: probe.client,
      registry: registry(),
      platform: "windows",
    });

    await controller.resumeAgentSession(resume, { leafId: "leaf-1" });
    await new Promise((done) => setTimeout(done, 0));

    expect(probe.launches[0]).toMatchObject({ mode: "shell", cwd: "/work/iyagi" });
    const pane = useWorkbenchStore.getState().panes["leaf-1"];
    expect(pane.resume).toEqual(resume);
    expect(pane.viewId).not.toBe("view-1");
    expect(pane.sessionId).toBe("s-1");
    controller.dispose();
  });

  it("연결에 실패한 에이전트 pane의 재시도는 작업이 끝났으면 곧바로 이어서 연다(클릭 한 번)", async () => {
    const probe = launchProbe();
    const base = await new MockDaemonClient({ resourceIntervalMs: 0 }).systemSnapshot();
    // 실패 시점의 스토어는 작업을 모른다(복구가 스냅샷을 받기 전에 실패했다) —
    // 재시도는 최신 스냅샷을 받아 INTERRUPTED를 보고 이어서 연다.
    (probe.client as { systemSnapshot: unknown }).systemSnapshot = async () => ({
      ...base,
      workloads: [{
        workload_id: "w-old",
        session_id: "s-old",
        state: "INTERRUPTED",
        last_error_code: "DAEMON_RESTART",
        title: "claude",
        cwd: "/work/iyagi",
      }],
    });
    (probe.client as { agentSessionList: unknown }).agentSessionList = async () => [record()];
    seedWorkspace(savedPane({ phase: "failed", error: "DAEMON_UNAVAILABLE", resume: info() }));
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    try {
      controller.retryPane("leaf-1");
      await new Promise((done) => setTimeout(done, 0));
      expect(probe.launches).toHaveLength(1);
      expect(JSON.stringify(probe.launches[0].argv)).toContain("agent-session-0001");
      expect(probe.launches[0].cwd).toBe("/work/iyagi");
      const pane = useWorkbenchStore.getState().panes["leaf-1"];
      expect(pane.sessionId).toBe("s-1");
      expect(pane.resume?.agentSessionId).toBe("agent-session-0001");
    } finally {
      controller.dispose();
    }
  });

  /** 재시도 시점의 최신 스냅샷: 이 pane의 작업은 데몬 재시작으로 끝났다(추가 작업은 덧붙인다). */
  async function interruptedSnapshot(probe: LaunchProbe, ...extra: object[]): Promise<void> {
    const base = await new MockDaemonClient({ resourceIntervalMs: 0 }).systemSnapshot();
    (probe.client as { systemSnapshot: unknown }).systemSnapshot = async () => ({
      ...base,
      workloads: [{
        workload_id: "w-old", session_id: "s-old", state: "INTERRUPTED", last_error_code: "DAEMON_RESTART",
        title: "claude", cwd: "/work/iyagi",
      }, ...extra],
    });
  }

  it("재접속 실패로 살아 있다고 남은 세션이어도 실패 pane의 재시도는 대화를 곧바로 이어서 연다", async () => {
    const probe = launchProbe();
    await interruptedSnapshot(probe);
    (probe.client as { agentSessionList: unknown }).agentSessionList = async () => [record()];
    seedWorkspace(savedPane({ phase: "failed", error: "DAEMON_UNAVAILABLE", resume: info() }));
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    // 연결 실패(markAttachedPanesFailed)는 liveSessions를 비우지 않는다 — 이 pane이 자기 대화를
    // "다른 곳에서 실행 중"으로 잡으면 기록 재생으로 빠져 "이어서 열기"를 또 눌러야 했다.
    (controller as unknown as { liveSessions: Set<string> }).liveSessions.add("s-old");
    try {
      controller.retryPane("leaf-1");
      await new Promise((done) => setTimeout(done, 0));
      expect(probe.launches).toHaveLength(1);
      expect(probe.launches[0].argv).toContain("agent-session-0001");
    } finally {
      controller.dispose();
    }
  });

  it("실패 pane의 재시도는 같은 대화가 다른 탭에서 실행 중이어도 그 탭으로 옮겨 가거나 재생 중에 멈추지 않는다", async () => {
    const probe = launchProbe();
    await interruptedSnapshot(probe, {
      workload_id: "w-live", session_id: "s-live", state: "RUNNING", title: "claude", cwd: "/work/iyagi",
      agent: { agent: "claude", pid: 7, detected_at_ms: 1, session_id: "agent-session-0001" },
    });
    (probe.client as { agentSessionList: unknown }).agentSessionList = async () => [record()];
    useWorkbenchStore.setState({
      tabs: [
        { kind: "terminal", id: "tab-1", title: "탭 1", root: makeLeaf("leaf-live", "view-live", "s-live") },
        { kind: "terminal", id: "tab-2", title: "탭 2", root: makeLeaf("leaf-1", "view-1", "s-old") },
      ],
      panes: {
        "leaf-live": savedPane({ leafId: "leaf-live", viewId: "view-live", sessionId: "s-live", workloadId: "w-live", phase: "live" }),
        "leaf-1": savedPane({ phase: "failed", error: "DAEMON_UNAVAILABLE" }),
      },
      activeTabId: "tab-2",
      focusedLeafId: "leaf-1",
    });
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    try {
      controller.retryPane("leaf-1");
      await new Promise((done) => setTimeout(done, 0));
      expect(useWorkbenchStore.getState().activeTabId).toBe("tab-2");
      expect(probe.launches).toHaveLength(0);
      // 이 pane의 기록을 다시 붙인다(이 시험의 attach는 실패한다) — "재생 중"에 남지 않는다.
      expect(useWorkbenchStore.getState().panes["leaf-1"].phase).not.toBe("replaying");
    } finally {
      controller.dispose();
    }
  });

  it("실패 pane의 재시도가 대화를 재개하지 못하면(실행 파일 없음) 실패 오버레이로 돌아간다", async () => {
    const probe = launchProbe();
    await interruptedSnapshot(probe);
    (probe.client as { agentSessionList: unknown }).agentSessionList = async () => [record({ program: null })];
    seedWorkspace(savedPane({ phase: "failed", error: "DAEMON_UNAVAILABLE" }));
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    try {
      controller.retryPane("leaf-1");
      await new Promise((done) => setTimeout(done, 0));
      expect(probe.launches).toHaveLength(0);
      expect(useWorkbenchStore.getState().toast).toBeTruthy();
      const pane = useWorkbenchStore.getState().panes["leaf-1"];
      expect(pane.phase).toBe("failed");
      expect(pane.error).toBe("DAEMON_UNAVAILABLE");
    } finally {
      controller.dispose();
    }
  });

  it("새 셸(재시도)도 재개 기록을 지운다 — 오버레이가 남지 않게", async () => {
    const probe = launchProbe();
    seedWorkspace(
      savedPane({ sessionId: null, workloadId: null, phase: "exited", resume: info() }),
    );
    const controller = new SessionController({
      client: probe.client,
      registry: registry(),
      platform: "windows",
    });

    controller.retryPane("leaf-1", { newShell: true });
    await new Promise((done) => setTimeout(done, 0));

    expect(useWorkbenchStore.getState().panes["leaf-1"].resume ?? null).toBeNull();
    controller.dispose();
  });

  it("플래그처럼 보이는 세션 id는 실행 파일을 찾아보지도 않는다", async () => {
    const probe = launchProbe();
    let probed = false;
    const controller = new SessionController({
      client: probe.client,
      registry: registry(),
      platform: "windows",
      resolveAgentProgram: async () => {
        probed = true;
        return "C:\\tools\\claude.exe";
      },
    });

    await controller.resumeAgentSession(info({ agentSessionId: "--yolo" }), { newPane: true });
    await new Promise((done) => setTimeout(done, 0));

    expect(probe.launches).toHaveLength(0);
    expect(probed).toBe(false);
    expect(useWorkbenchStore.getState().toast).toBe(t("terminal.resume.launchFailed"));
    controller.dispose();
  });

  it("자리를 못 만들었는데 아무 문구도 없으면 재개 실패를 알린다", async () => {
    const probe = launchProbe();
    seedWorkspace(savedPane());
    // 포커스된 pane이 없으면 분할은 조용히 거절된다 — 예전에는 아무 일도
    // 일어나지 않은 것처럼 보였다.
    useWorkbenchStore.setState({ focusedLeafId: null });
    const controller = new SessionController({
      client: probe.client,
      registry: registry(),
      platform: "windows",
    });

    await controller.resumeAgentSession(info(), { newPane: true });
    await new Promise((done) => setTimeout(done, 0));

    expect(probe.launches).toHaveLength(0);
    expect(useWorkbenchStore.getState().toast).toBe(t("terminal.resume.launchFailed"));
    controller.dispose();
  });

  it("한도 때문에 거절됐으면 더 구체적인 문구를 덮지 않는다", async () => {
    const probe = launchProbe();
    seedWorkspace(
      ...Array.from({ length: MAX_PANES_PER_TAB }, (_unused, index) =>
        savedPane({
          leafId: `leaf-${index}`,
          viewId: `view-${index}`,
          sessionId: null,
          workloadId: null,
        }),
      ),
    );
    const controller = new SessionController({
      client: probe.client,
      registry: registry(),
      platform: "windows",
    });

    await controller.resumeAgentSession(info(), { newPane: true });
    await new Promise((done) => setTimeout(done, 0));

    expect(probe.launches).toHaveLength(0);
    expect(useWorkbenchStore.getState().toast).toBe(paneLimitText(MAX_PANES_PER_TAB));
    controller.dispose();
  });
});

/**
 * 3) 감지된 에이전트 배지(04-ui §5-1): 텔레메트리는 내용이 같아도 매 틱
 *    새 객체를 싣고 오므로, 값이 같으면 pane을 갈아 끼우지 않는다(참조
 *    비교였을 때는 workload.changed마다 모든 header가 다시 그려졌다).
 */
describe("refreshPaneAgents: 값이 같은 감지 결과는 pane을 건드리지 않는다", () => {
  function agentStatus(overrides: Record<string, unknown> = {}): Record<string, unknown> {
    return {
      agent: "claude",
      pid: 4321,
      detected_at_ms: 100,
      session_id: "agent-session-0001",
      session_name: "iyagi",
      session_source: "registry",
      session_status: "idle",
      ...overrides,
    };
  }

  function changed(agent: Record<string, unknown> | null): DaemonEvent {
    return {
      kind: "workload.changed",
      payload: {
        workload_id: "w-old",
        session_id: "s-old",
        mode: "shell",
        state: "RUNNING",
        priority: 1,
        title: "zsh",
        cwd: "/work/iyagi",
        program: "/bin/zsh",
        reservation_bytes: "2147483648",
        cpu_slots: 1,
        enforcement: "observe",
        root_exited: false,
        cancel_requested: false,
        exit_code: null,
        queue_reason: null,
        connection: "attached",
        usage: null,
        agent,
      },
    } as unknown as DaemonEvent;
  }

  function liveController(): SessionController {
    seedWorkspace(savedPane({ phase: "live" }));
    return new SessionController({
      client: launchProbe().client,
      registry: registry(),
      platform: "darwin",
    });
  }

  it("같은 내용의 새 객체가 와도 pane을 다시 쓰지 않는다", () => {
    const controller = liveController();

    controller.handleEvent(changed(agentStatus()));
    const pane = useWorkbenchStore.getState().panes["leaf-1"];
    expect(pane.agent?.session_name).toBe("iyagi");

    controller.handleEvent(changed(agentStatus()));
    expect(useWorkbenchStore.getState().panes["leaf-1"]).toBe(pane);

    // 값이 바뀌면 그대로 따라간다.
    controller.handleEvent(changed(agentStatus({ session_status: "busy" })));
    expect(useWorkbenchStore.getState().panes["leaf-1"].agent?.session_status).toBe("busy");
    controller.dispose();
  });

  it("없는 선택 필드·undefined·null은 같은 값으로 본다(계약이 늘어도 안전)", () => {
    const controller = liveController();

    controller.handleEvent(changed(agentStatus({ model: null })));
    const pane = useWorkbenchStore.getState().panes["leaf-1"];

    controller.handleEvent(changed(agentStatus({ model: undefined })));
    expect(useWorkbenchStore.getState().panes["leaf-1"]).toBe(pane);

    controller.handleEvent(changed(agentStatus()));
    expect(useWorkbenchStore.getState().panes["leaf-1"]).toBe(pane);

    // 실제 값이 들어오면 갱신한다.
    controller.handleEvent(changed(agentStatus({ model: "Opus 5 (1M context)" })));
    expect(useWorkbenchStore.getState().panes["leaf-1"].agent?.model).toBe("Opus 5 (1M context)");
    controller.dispose();
  });

  it("에이전트가 사라지면 배지를 내린다", () => {
    const controller = liveController();
    controller.handleEvent(changed(agentStatus()));
    controller.handleEvent(changed(null));
    expect(useWorkbenchStore.getState().panes["leaf-1"].agent).toBeNull();
    controller.dispose();
  });
});

/**
 * 4) 목록 seam: mock이 데몬과 같은 순서·중복 제거를 하지 않으면 순서에
 *    의존하는 버그가 시험에서 드러나지 않는다(02 §8).
 */
describe("MockDaemonClient.agentSessionList: 데몬과 같은 순서·중복 제거", () => {
  it("last_seen_at 내림차순으로 돌려주고 (agent, 세션 id) 중복은 최근 것만 남긴다", async () => {
    const client = new MockDaemonClient({ resourceIntervalMs: 0 });
    client.seedAgentSessions([
      record({ id: "rec-a-old", agent_session_id: "sess-a", last_seen_at: "2026-09-12T01:00:00Z" }),
      record({ id: "rec-b", agent_session_id: "sess-b", last_seen_at: "2026-09-12T02:00:00Z" }),
      record({ id: "rec-a-new", agent_session_id: "sess-a", last_seen_at: "2026-09-12T03:00:00Z" }),
      // 같은 세션 id라도 에이전트가 다르면 다른 대화다.
      record({
        id: "rec-codex",
        agent: "codex",
        agent_session_id: "sess-a",
        last_seen_at: "2026-09-12T00:30:00Z",
      }),
    ]);

    const list = await client.agentSessionList({});
    expect(list.map((r) => r.id)).toEqual(["rec-a-new", "rec-b", "rec-codex"]);
    client.dispose();
  });

  it("last_seen_at 동률은 id 내림차순으로 가른다", async () => {
    const client = new MockDaemonClient({ resourceIntervalMs: 0 });
    const at = "2026-09-12T01:00:00Z";
    client.seedAgentSessions([
      record({ id: "rec-1", agent_session_id: "sess-a", last_seen_at: at }),
      record({ id: "rec-2", agent_session_id: "sess-a", last_seen_at: at }),
      record({ id: "rec-3", agent_session_id: "sess-b", last_seen_at: at }),
    ]);

    const list = await client.agentSessionList({});
    expect(list.map((r) => r.id)).toEqual(["rec-3", "rec-2"]);
    client.dispose();
  });

  it("cwd로 걸러낸 뒤 limit을 적용한다", async () => {
    const client = new MockDaemonClient({ resourceIntervalMs: 0 });
    client.seedAgentSessions([
      record({ id: "rec-here-1", agent_session_id: "sess-a", last_seen_at: "2026-09-12T01:00:00Z" }),
      record({ id: "rec-here-2", agent_session_id: "sess-b", last_seen_at: "2026-09-12T02:00:00Z" }),
      record({ id: "rec-there", agent_session_id: "sess-c", cwd: "/elsewhere" }),
    ]);

    expect((await client.agentSessionList({ cwd: "/elsewhere" })).map((r) => r.id)).toEqual([
      "rec-there",
    ]);
    expect((await client.agentSessionList({ limit: 1 })).map((r) => r.id)).toEqual(["rec-here-2"]);
    client.dispose();
  });
});


describe("대기열 터미널 연결에서 종료된 에이전트 복구", () => {
  function workload(state: "FAILED" | "RUNNING" | "INTERRUPTED" = "FAILED", sessionId: string | null = "s-old") {
    return {
      workload_id: "w-old", session_id: sessionId, state, mode: "shell" as const,
      priority: 1 as const, title: "zsh", cwd: "/work/iyagi", program: "/bin/zsh",
      reservation_bytes: "0", cpu_slots: 1, enforcement: "observe" as const,
      root_exited: state !== "RUNNING", cancel_requested: false, connection: "detached" as const,
      relief: { kind: "NONE" } as const, guard: { kind: "NONE" } as const, guard_warning: null, protected: false,
    };
  }

  it.each(["claude", "codex", "opencode"] as const)("%s: 연결 클릭이 정확한 세션과 cwd로 재개한다", async (agent) => {
    const probe = launchProbe();
    probe.client.agentSessionList = vi.fn().mockResolvedValue([
      record({ id: "unrelated", workload_id: "other", pty_session_id: "other", agent_session_id: "wrong" }),
      record({ agent, program: `/opt/bin/${agent}` }),
    ]);
    useWorkbenchStore.setState({ workloads: [workload()] });
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    await Promise.all([controller.attachWorkloadTerminal("w-old"), controller.attachWorkloadTerminal("w-old")]);
    expect(probe.launches).toHaveLength(1);
    expect(probe.launches[0]).toMatchObject({ program: `/opt/bin/${agent}`, cwd: "/work/iyagi" });
    expect(probe.launches[0].argv).toContain("agent-session-0001");
    expect(probe.launches[0].argv).not.toContain("wrong");
    // 재개한 새 작업이 끝난 작업을 이어받아 최근 종료에서 빠진다.
    await vi.waitFor(() => expect(useWorkbenchStore.getState().workloadMemory["w-old"]?.recoveredBy).toBe("w-1"));
    controller.dispose();
  });

  it("PTY가 사라진 INTERRUPTED 작업도 기록으로 복구한다", async () => {
    const probe = launchProbe();
    probe.client.agentSessionList = vi.fn().mockResolvedValue([record()]);
    useWorkbenchStore.setState({ workloads: [workload("INTERRUPTED", null)] });
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    await controller.attachWorkloadTerminal("w-old");
    expect(probe.launches).toHaveLength(1);
    controller.dispose();
  });

  it("종료 pane이 남아 있으면 그 자리에서 복구한다", async () => {
    const probe = launchProbe();
    probe.client.agentSessionList = vi.fn().mockResolvedValue([record()]);
    seedWorkspace(savedPane({ phase: "exited" }));
    useWorkbenchStore.setState({ workloads: [workload()] });
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    await controller.attachWorkloadTerminal("w-old");
    expect(Object.keys(useWorkbenchStore.getState().panes)).toEqual(["leaf-1"]);
    expect(useWorkbenchStore.getState().panes["leaf-1"].sessionId).toBe("s-1");
    expect(probe.launches).toHaveLength(1);
    controller.dispose();
  });

  it("끝남을 듣지 못해 살아 보이는 일반 터미널 창도 오지 않을 끝남을 기다리지 않고 그 자리에서 새 셸로 잇는다", async () => {
    const probe = launchProbe();
    probe.client.agentSessionList = vi.fn().mockResolvedValue([]);
    seedWorkspace(savedPane({ phase: "live" }));
    useWorkbenchStore.setState({ workloadMemory: {}, workloads: [workload()] });
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    await controller.attachWorkloadTerminal("w-old");
    await vi.waitFor(() => expect(useWorkbenchStore.getState().panes["leaf-1"]?.sessionId).toBe("s-1"));
    expect(probe.launches).toHaveLength(1);
    expect(Object.keys(useWorkbenchStore.getState().panes)).toEqual(["leaf-1"]);
    expect(useWorkbenchStore.getState().workloadMemory["w-old"]?.recoveredBy).toBe("w-1");
    controller.dispose();
  });

  it.each([
    ["살아 있는 작업은 재실행 없이", "RUNNING", [record()]],
    // 일반 터미널: 이전 출력부터 재생한다. 새 셸은 재생이 끝난 뒤 같은 창에서 잇는다(통합 테스트).
    ["다른 작업의 기록만 있는 일반 터미널은 재생부터", "FAILED", [record({ workload_id: "other", pty_session_id: "other" })]],
    ["기록 없는 일반 터미널은 재생부터", "FAILED", []],
  ] as const)("%s 기존 PTY에 연결한다", async (_name, state, records) => {
    const probe = launchProbe();
    probe.client.agentSessionList = vi.fn().mockResolvedValue(records);
    useWorkbenchStore.setState({ workloads: [workload(state)] });
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    const attach = vi.spyOn(controller, "attachSessionToNewPane").mockImplementation(() => undefined);
    await controller.attachWorkloadTerminal("w-old");
    expect(attach).toHaveBeenCalledWith("s-old", "w-old");
    expect(probe.launches).toHaveLength(0);
    controller.dispose();
  });

  it("같은 대화가 다른 작업에서 실행 중이면 끝난 출력 대신 그 터미널로 간다", async () => {
    const probe = launchProbe();
    // 데몬은 같은 대화를 관찰 중인 살아 있는 작업이 있으면 과거 기록도 active로 알린다.
    probe.client.agentSessionList = vi.fn().mockResolvedValue([record({ active: true })]);
    seedWorkspace(
      savedPane({ sessionId: "s-other", workloadId: "w-other", phase: "live" }),
      savedPane({ leafId: "leaf-2", viewId: "view-2", sessionId: "s-live", workloadId: "w-live", phase: "live" }),
    );
    useWorkbenchStore.setState({
      workloadMemory: {},
      workloads: [
        workload(),
        {
          ...workload("RUNNING", "s-live"),
          workload_id: "w-live",
          agent: { agent: "claude", pid: 7, detected_at_ms: 1, session_id: "agent-session-0001" },
        },
      ],
    });
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    const attach = vi.spyOn(controller, "attachSessionToNewPane");
    await controller.attachWorkloadTerminal("w-old");
    expect(attach).not.toHaveBeenCalled();
    expect(probe.launches).toHaveLength(0);
    expect(useWorkbenchStore.getState().focusedLeafId).toBe("leaf-2");
    expect(useWorkbenchStore.getState().workloadMemory["w-old"]?.recoveredBy).toBe("w-live");
    controller.dispose();
  });

  it("실행 중인 대화가 어느 창에도 없으면 그 실행을 새 창으로 연결한다", async () => {
    const probe = launchProbe();
    probe.client.agentSessionList = vi.fn().mockResolvedValue([record({ active: true })]);
    useWorkbenchStore.setState({
      workloadMemory: {},
      workloads: [
        workload(),
        {
          ...workload("RUNNING", "s-live"),
          workload_id: "w-live",
          agent: { agent: "claude", pid: 7, detected_at_ms: 1, session_id: "agent-session-0001" },
        },
      ],
    });
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    const attach = vi.spyOn(controller, "attachSessionToNewPane").mockImplementation(() => undefined);
    await controller.attachWorkloadTerminal("w-old");
    expect(attach).toHaveBeenCalledWith("s-live", "w-live");
    expect(probe.launches).toHaveLength(0);
    controller.dispose();
  });

  it("실행 중이라는 기록만 있고 그 실행을 모르면 알리고 끝난 출력은 열지 않는다", async () => {
    const probe = launchProbe();
    probe.client.agentSessionList = vi.fn().mockResolvedValue([record({ active: true })]);
    useWorkbenchStore.setState({ workloadMemory: {}, workloads: [workload()] });
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    const attach = vi.spyOn(controller, "attachSessionToNewPane").mockImplementation(() => undefined);
    await controller.attachWorkloadTerminal("w-old");
    expect(attach).not.toHaveBeenCalled();
    expect(probe.launches).toHaveLength(0);
    expect(useWorkbenchStore.getState().toast).toBe(t("terminal.resume.alreadyRunning"));
    expect(useWorkbenchStore.getState().workloadMemory["w-old"]?.recoveredBy ?? null).toBeNull();
    controller.dispose();
  });

  it("일괄 재개는 기록이 있는 대화만 차례로 열고 결과를 한 줄로 알린다", async () => {
    const probe = launchProbe();
    // 기록이 있는 대화 둘과, 기록이 사라진 일반 터미널 하나.
    probe.client.agentSessionList = vi.fn().mockImplementation(async (params: { workload_id?: string }) =>
      params.workload_id === "w-plain"
        ? []
        : [record({ workload_id: params.workload_id, pty_session_id: `s-${params.workload_id}`, agent_session_id: `agent-${params.workload_id}` })],
    );
    useWorkbenchStore.setState({
      workloadMemory: {},
      workloads: [
        { ...workload(), workload_id: "w-a", session_id: "s-w-a" },
        { ...workload(), workload_id: "w-b", session_id: "s-w-b" },
        { ...workload(), workload_id: "w-plain", session_id: "s-w-plain" },
      ],
    });
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    await controller.resumeAgentWorkloads(["w-a", "w-b", "w-plain"]);
    // 기록 없는 터미널은 새 셸로 잇지 않는다 — 대화 둘만 실행한다.
    expect(probe.launches).toHaveLength(2);
    expect(probe.launches.map((launch) => launch.argv.join(" "))).toEqual([
      expect.stringContaining("agent-w-a"),
      expect.stringContaining("agent-w-b"),
    ]);
    expect(useWorkbenchStore.getState().toast).toBe(t("queue.resumeAll.partial", { n: 2, failed: 1 }));
    controller.dispose();
  });

  it("일괄 재개가 도는 중에는 같은 요청을 겹쳐 실행하지 않는다", async () => {
    const probe = launchProbe();
    probe.client.agentSessionList = vi.fn().mockResolvedValue([record()]);
    useWorkbenchStore.setState({ workloadMemory: {}, workloads: [workload()] });
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    await Promise.all([
      controller.resumeAgentWorkloads(["w-old"]),
      controller.resumeAgentWorkloads(["w-old"]),
    ]);
    expect(probe.launches).toHaveLength(1);
    controller.dispose();
  });
});


describe("비정상 종료 복구 회귀", () => {
  it.each(["claude", "codex", "opencode"] as const)("%s: 프로세스 마킹은 저장·재시작 뒤 연결 클릭에서 정확한 ID로 복구한다", async agent => {
    seedWorkspace(savedPane({ phase: "live" }));
    useWorkbenchStore.getState().paneAgent("leaf-1", {
      agent, pid: 1234, detected_at_ms: 1, session_id: "marked-conversation",
    });
    useWorkbenchStore.getState().paneAgent("leaf-1", null);
    const saved = decodeWorkspace(encodeWorkspace(useWorkbenchStore.getState()))!;
    resetWorkspace();
    useWorkbenchStore.setState(saved);
    const probe = launchProbe();
    probe.client.agentSessionList = vi.fn().mockResolvedValue([]);
    const base = await new MockDaemonClient({ resourceIntervalMs: 0 }).systemSnapshot();
    probe.client.systemSnapshot = async () => base;
    probe.client.interventionList = async () => [];
    const controller = new SessionController({
      client: probe.client, registry: registry(), platform: "windows",
      resolveAgentProgram: async kind => `/opt/bin/${kind}`,
    });
    try {
      await controller.restoreWorkspace();
      expect(probe.launches).toHaveLength(0);
      expect(useWorkbenchStore.getState().panes["leaf-1"].resume?.agentSessionId).toBe("marked-conversation");
      useWorkbenchStore.setState({ workloads: [{ workload_id: "w-old", session_id: "s-old", state: "INTERRUPTED" } as never] });
      await Promise.all([controller.attachWorkloadTerminal("w-old"), controller.attachWorkloadTerminal("w-old")]);
      expect(probe.launches).toHaveLength(1);
      expect(probe.launches[0]).toMatchObject({ program: `/opt/bin/${agent}`, cwd: "/work/iyagi" });
      expect(probe.launches[0].argv).toContain("marked-conversation");
      expect(Object.keys(useWorkbenchStore.getState().panes)).toEqual(["leaf-1"]);
    } finally { controller.dispose(); }
  });

  it.each(["claude", "codex", "opencode"] as const)("%s: 종료 pane의 재실행도 새 셸 대신 대화를 한 번 재개한다", async agent => {
    const probe = launchProbe();
    probe.client.agentSessionList = vi.fn().mockResolvedValue([]);
    seedWorkspace(savedPane({ phase: "exited", resume: info({ agent, program: `/opt/bin/${agent}` }) }));
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    controller.retryPane("leaf-1");
    controller.retryPane("leaf-1");
    await new Promise(done => setTimeout(done, 0));
    expect(probe.launches).toHaveLength(1);
    expect(probe.launches[0].argv).toContain("agent-session-0001");
    controller.dispose();
  });

  it("같은 대화가 다른 탭에서 실행 중이면 종료 pane의 다시 시작은 그 탭으로 옮겨 가지 않고 이 자리에서 새 셸을 연다", async () => {
    const probe = launchProbe();
    // 이 pane의 작업에는 끝난 대화 기록이 있지만, 같은 대화가 첫 탭에서 돌고 있다.
    probe.client.agentSessionList = vi.fn().mockResolvedValue([record()]);
    useWorkbenchStore.setState({
      tabs: [
        { kind: "terminal", id: "tab-1", title: "탭 1", root: makeLeaf("leaf-live", "view-live", "s-live") },
        { kind: "terminal", id: "tab-2", title: "탭 2", root: makeLeaf("leaf-1", "view-1", "s-old") },
      ],
      panes: {
        "leaf-live": savedPane({ leafId: "leaf-live", viewId: "view-live", sessionId: "s-live", workloadId: "w-live", phase: "live" }),
        "leaf-1": savedPane({ phase: "exited" }),
      },
      activeTabId: "tab-2",
      focusedLeafId: "leaf-1",
      workloads: [{
        workload_id: "w-live", session_id: "s-live", state: "RUNNING", mode: "shell",
        agent: { agent: "claude", pid: 7, detected_at_ms: 1, session_id: "agent-session-0001" },
      } as never],
    });
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    try {
      controller.retryPane("leaf-1");
      await new Promise(done => setTimeout(done, 0));
      expect(useWorkbenchStore.getState().activeTabId).toBe("tab-2");
      expect(useWorkbenchStore.getState().focusedLeafId).toBe("leaf-1");
      expect(probe.launches).toHaveLength(1);
      expect(probe.launches[0].argv).not.toContain("agent-session-0001");
      expect(useWorkbenchStore.getState().panes["leaf-1"].sessionId).toBe("s-1");
    } finally {
      controller.dispose();
    }
  });

  it("재개 기록이 있는 종료 pane도 같은 대화가 다른 창에서 실행 중이면 다시 시작이 그 자리에서 새 셸을 연다", async () => {
    // 메뉴의 "다시 시작": 예전에는 아무 일도 하지 않았다(알림·이동·실행 모두 없음).
    const probe = launchProbe();
    probe.client.agentSessionList = vi.fn().mockResolvedValue([record()]);
    seedWorkspace(
      savedPane({ phase: "exited", resume: info() }),
      savedPane({ leafId: "leaf-live", viewId: "view-live", sessionId: "s-live", workloadId: "w-live", phase: "live" }),
    );
    useWorkbenchStore.setState({
      workloads: [{
        workload_id: "w-live", session_id: "s-live", state: "RUNNING", mode: "shell",
        agent: { agent: "claude", pid: 7, detected_at_ms: 1, session_id: "agent-session-0001" },
      } as never],
    });
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    try {
      controller.retryPane("leaf-1");
      await new Promise(done => setTimeout(done, 0));
      expect(probe.launches).toHaveLength(1);
      expect(probe.launches[0].argv).not.toContain("agent-session-0001");
      const pane = useWorkbenchStore.getState().panes["leaf-1"];
      expect(pane.sessionId).toBe("s-1");
      expect(pane.resume ?? null).toBeNull();
      expect(useWorkbenchStore.getState().focusedLeafId).toBe("leaf-1");
    } finally {
      controller.dispose();
    }
  });

  it("mission 탭을 보던 중에 살아 있는 세션을 연결하면 새 터미널 탭을 하나만 열고 그리로 간다", () => {
    // 예전에는 addTab이 활성 탭을 바꾸지 않아 스스로를 끝없이 다시 불렀다(빈 탭 수천 개 + 스택 넘침).
    const probe = launchProbe();
    useWorkbenchStore.setState({
      tabs: [{ kind: "mission", id: "m-1", title: "mission", missionId: "mission-1" }],
      panes: {},
      activeTabId: "m-1",
      focusedLeafId: null,
    });
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    try {
      controller.attachSessionToNewPane("s-live", "w-live");
      const state = useWorkbenchStore.getState();
      expect(state.tabs).toHaveLength(2);
      expect(state.tabs.find(tab => tab.id === state.activeTabId)?.kind).toBe("terminal");
      expect(Object.values(state.panes).map(pane => pane.sessionId)).toEqual(["s-live"]);
    } finally {
      controller.dispose();
    }
  });

  it.each(["claude", "codex", "opencode"] as const)("%s: 최근 200건 밖의 기록도 원래 pane에서 복구한다", async (agent) => {
    const client = new MockDaemonClient({ resourceIntervalMs: 0 });
    client.seedAgentSessions([record({ agent }), ...Array.from({ length: 200 }, (_, i) => record({
      id: `new-${i}`, workload_id: `new-w-${i}`, pty_session_id: `new-s-${i}`,
      agent_session_id: `new-agent-${i}`, last_seen_at: "2026-09-13T00:00:00Z",
    }))]);
    seedWorkspace(savedPane());
    const list = vi.spyOn(client, "agentSessionList");
    const launch = vi.spyOn(client, "workloadLaunch");
    const controller = new SessionController({ client, registry: registry(), platform: "darwin" });
    try {
      await controller.restoreWorkspace();
      expect(useWorkbenchStore.getState().panes["leaf-1"].resume?.agentSessionId).toBe("agent-session-0001");
      expect(list).toHaveBeenCalledWith({ workload_id: "w-old", pty_session_id: "s-old", limit: 1 });
      expect(launch).not.toHaveBeenCalled();
    } finally { controller.dispose(); }
  });

  it.each(["claude", "codex", "opencode"] as const)("%s: 재시작으로 cwd와 PTY가 사라져도 작업 ID로 재개한다", async (agent) => {
    const probe = launchProbe();
    const history = new MockDaemonClient({ resourceIntervalMs: 0 });
    history.seedAgentSessions([record({ agent, program: `/opt/bin/${agent}` })]);
    probe.client.agentSessionList = vi.fn(params => history.agentSessionList(params));
    useWorkbenchStore.setState({ workloads: [{
      workload_id: "w-old", session_id: null, state: "INTERRUPTED", cwd: "", last_error_code: "DAEMON_RESTART",
    } as never] });
    const controller = new SessionController({ client: probe.client, registry: registry(), platform: "windows" });
    try {
      await controller.attachWorkloadTerminal("w-old");
      expect(probe.launches).toHaveLength(1);
      expect(probe.launches[0]).toMatchObject({ cwd: "/work/iyagi", program: `/opt/bin/${agent}` });
      expect(probe.launches[0].argv).toContain("agent-session-0001");
      expect(probe.client.agentSessionList).toHaveBeenCalledWith({ workload_id: "w-old", pty_session_id: null, limit: 1 });
    } finally { controller.dispose(); }
  });

  it("mock도 실제 DB처럼 빈 cwd를 정확히 일치시킨다", async () => {
    const client = new MockDaemonClient({ resourceIntervalMs: 0 });
    client.seedAgentSessions([record()]);
    expect(await client.agentSessionList({ cwd: "" })).toEqual([]);
    expect(await client.agentSessionList({})).toHaveLength(1);
  });

  it("다른 pane에서 이미 재개한 대화는 과거 작업을 조회해도 재실행하지 않는다", async () => {
    const client = new MockDaemonClient({ resourceIntervalMs: 0 });
    client.seedAgentSessions([record(), record({
      id: "new-record", workload_id: "new-workload", pty_session_id: "new-pty", active: true,
      last_seen_at: "2026-09-13T00:00:00Z",
    })]);
    const records = await client.agentSessionList({ workload_id: "w-old" });
    expect(records).toHaveLength(1);
    expect(records[0].active).toBe(true);
    seedWorkspace(savedPane());
    const controller = new SessionController({ client, registry: registry(), platform: "darwin" });
    try {
      await controller.restoreWorkspace();
      expect(useWorkbenchStore.getState().panes["leaf-1"].resume ?? null).toBeNull();
    } finally { controller.dispose(); }
  });
});

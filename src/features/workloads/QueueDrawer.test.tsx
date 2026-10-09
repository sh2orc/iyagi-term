/**
 * 관리 작업 목록 이름: 붙은 터미널의 최신 제목(OSC 0/2)을 보여 주고,
 * 제목 보고 전에는 실행 제목을 보여 준다(최근 종료 목록도 같다). 창을
 * 닫은 터미널은 마지막 제목으로 남고, 마지막으로 실행한 에이전트가
 * 있으면 그 표시를 이름 앞에 붙인다.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { WorkloadSummary } from "../../generated/WorkloadSummary";
import type { SessionController } from "../terminal/sessionController";
import { ControllerContext } from "../../app/controllerContext";
import { useWorkbenchStore, type PaneMeta } from "../../store/workbenchStore";
import { QueueDrawer } from "./QueueDrawer";

// 서비스 마크 SVG 대신 어떤 에이전트를 그렸는지만 남긴다.
vi.mock("../terminal/AgentIcon", () => ({
  AgentIcon: ({ agent }: { agent: string }) => <i className="agent-icon" data-agent={agent} />,
}));

// SSR은 Zustand의 초기 스냅샷을 읽는다 — 테스트가 넣은 현재 상태를 고르게 한다.
vi.mock("../../store/workbenchStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../store/workbenchStore")>();
  const store = actual.useWorkbenchStore;
  return {
    ...actual,
    useWorkbenchStore: Object.assign(
      (selector: (state: ReturnType<typeof store.getState>) => unknown) => selector(store.getState()),
      store,
    ),
  };
});

function workload(overrides: Partial<WorkloadSummary> = {}): WorkloadSummary {
  return {
    workload_id: "workload-1",
    session_id: "session-1",
    mode: "managed",
    state: "RUNNING",
    priority: 1,
    title: "claude",
    cwd: "/work",
    program: "claude",
    reservation_bytes: "1073741824",
    cpu_slots: 1,
    enforcement: "observe",
    root_exited: false,
    cancel_requested: false,
    connection: "attached",
    relief: { kind: "NONE" },
    guard: { kind: "NONE" } as const,
    guard_warning: null,
    protected: false,
    ...overrides,
  };
}

function pane(overrides: Partial<PaneMeta> = {}): PaneMeta {
  return {
    leafId: "leaf-1",
    viewId: "view-1",
    sessionId: "session-1",
    workloadId: "workload-1",
    title: "zsh",
    cwd: null,
    phase: "live",
    error: null,
    usage: null,
    flowBlocked: false,
    ...overrides,
  };
}

function drawerRows(): Array<{ title: string; agent: string | null }> {
  const html = renderToStaticMarkup(
    <ControllerContext.Provider value={{} as SessionController}>
      <QueueDrawer />
    </ControllerContext.Provider>,
  );
  return [...html.matchAll(/class="workload-title"[^>]*>(.*?)<\/span>/g)].map((m) => ({
    title: m[1].replace(/<[^>]*>/g, ""),
    agent: /data-agent="([^"]*)"/.exec(m[1])?.[1] ?? null,
  }));
}

function drawerTitles(): string[] {
  return drawerRows().map((row) => row.title);
}

afterEach(() =>
  useWorkbenchStore.setState({ queueDrawerOpen: false, workloads: [], queue: [], panes: {}, workloadMemory: {} }),
);

describe("QueueDrawer workload names", () => {
  it("shows the attached terminal's latest title instead of the launch title", () => {
    useWorkbenchStore.setState({
      queueDrawerOpen: true,
      queue: [],
      workloads: [workload()],
      panes: { "leaf-1": pane({ title: "✳ Fix login bug", terminalTitle: "✳ Fix login bug" }) },
    });
    expect(drawerTitles()).toEqual(["✳ Fix login bug"]);
  });

  it("keeps the launch title until the terminal reports one", () => {
    useWorkbenchStore.setState({
      queueDrawerOpen: true,
      queue: [],
      workloads: [workload()],
      panes: { "leaf-1": pane({ title: "재연결" }) },
    });
    expect(drawerTitles()).toEqual(["claude"]);
  });

  it("uses the last reported title for recently finished workloads", () => {
    useWorkbenchStore.setState({
      queueDrawerOpen: true,
      queue: [],
      workloads: [workload({ state: "SUCCEEDED", connection: "detached" })],
      panes: { "leaf-1": pane({ phase: "exited", terminalTitle: "✓ Done: login bug" }) },
    });
    expect(drawerTitles()).toEqual(["✓ Done: login bug"]);
  });

  it("keeps a closed terminal's final title instead of the launch title", () => {
    useWorkbenchStore.setState({
      queueDrawerOpen: true,
      queue: [],
      workloads: [workload({ mode: "shell", title: "zsh", program: "/usr/bin/env", state: "SUCCEEDED", connection: "detached" })],
      panes: {},
      workloadMemory: { "workload-1": { title: "✳ Fix login bug", agent: null } },
    });
    expect(drawerRows()).toEqual([{ title: "✳ Fix login bug", agent: null }]);
  });

  it("marks recently finished terminals whose last program was an agent", () => {
    useWorkbenchStore.setState({
      queueDrawerOpen: true,
      queue: [],
      workloads: [
        workload({ workload_id: "w-claude", session_id: "s-claude", mode: "shell", title: "zsh", state: "SUCCEEDED", connection: "detached" }),
        workload({ workload_id: "w-codex", session_id: "s-codex", mode: "shell", title: "zsh", state: "CANCELLED", connection: "detached" }),
        // 종료 순간에도 돌고 있던 에이전트는 요약에 남아 있다.
        workload({ workload_id: "w-opencode", session_id: "s-opencode", mode: "shell", title: "zsh", state: "SUCCEEDED", connection: "detached", agent: { agent: "opencode", pid: 7, detected_at_ms: 1 } }),
        workload({ workload_id: "w-shell", session_id: "s-shell", mode: "shell", title: "zsh", state: "SUCCEEDED", connection: "detached" }),
      ],
      panes: {},
      workloadMemory: {
        "w-claude": { title: "✳ Fix login bug", agent: "claude" },
        "w-codex": { title: null, agent: "codex" },
      },
    });
    // 최근 종료는 최신이 위다.
    expect(drawerRows()).toEqual([
      { title: "zsh", agent: null },
      { title: "zsh", agent: "opencode" },
      { title: "zsh", agent: "codex" },
      { title: "✳ Fix login bug", agent: "claude" },
    ]);
  });

  it("drops finished terminals that were recovered into a new workload", () => {
    useWorkbenchStore.setState({
      queueDrawerOpen: true,
      queue: [],
      workloads: [
        workload({ workload_id: "w-recovered", session_id: "s-recovered", mode: "shell", title: "zsh", state: "SUCCEEDED", connection: "detached" }),
        workload({ workload_id: "w-left", session_id: "s-left", mode: "shell", title: "zsh", state: "SUCCEEDED", connection: "detached" }),
        // 복구한 새 셸 — 관리 작업에 보인다.
        workload({ workload_id: "w-new", session_id: "s-new", mode: "shell", title: "zsh" }),
      ],
      panes: {},
      workloadMemory: {
        "w-recovered": { title: "sh2orc@mac:~/project", agent: null, recoveredBy: "w-new" },
        "w-left": { title: "sh2orc@mac:~/other", agent: null },
      },
    });
    const html = renderToStaticMarkup(
      <ControllerContext.Provider value={{} as SessionController}>
        <QueueDrawer />
      </ControllerContext.Provider>,
    );
    const finished = html.slice(html.indexOf('class="finished-list"'));
    expect(finished).toContain("sh2orc@mac:~/other");
    expect(finished).not.toContain("sh2orc@mac:~/project");
    expect(drawerTitles()).toEqual(["zsh", "sh2orc@mac:~/other"]);
  });

  it("does not mark running workloads with the remembered agent", () => {
    useWorkbenchStore.setState({
      queueDrawerOpen: true,
      queue: [],
      workloads: [workload({ mode: "shell", title: "zsh" })],
      panes: {},
      workloadMemory: { "workload-1": { title: "✳ Fix login bug", agent: "claude" } },
    });
    expect(drawerRows()).toEqual([{ title: "✳ Fix login bug", agent: null }]);
  });

  it("offers to connect every live terminal that no window is attached to", () => {
    useWorkbenchStore.setState({
      queueDrawerOpen: true,
      queue: [],
      // 양보 토글은 지원하지 않는 플랫폼이면 disabled로 렌더된다 — 전역
      // disabled 부정 단언을 위해 지원 플랫폼으로 둔다.
      schedulingYield: { support: "supported" },
      workloads: [
        workload({ workload_id: "w-1", session_id: "s-1", connection: "detached" }),
        workload({
          workload_id: "w-2", session_id: "s-2", mode: "shell", title: "zsh", program: "/usr/bin/env",
          connection: "detached",
        }),
      ],
      panes: {},
    });
    const html = renderToStaticMarkup(
      <ControllerContext.Provider value={{} as SessionController}>
        <QueueDrawer />
      </ControllerContext.Provider>,
    );
    expect(html).toContain("모두 연결(2)");
    // "일시정지 모두 재개"는 이 시나리오에 대상이 없어 비활성으로 뜬다 —
    // 비활성 부정은 모두 연결 버튼의 마크업에만 건다.
    expect(html).toContain(
      '<button type="button" title="다른 창이 붙지 않은 살아 있는 터미널을 한 번에 다시 연결합니다">모두 연결(2)</button>',
    );
  });

  it("disables connect-all when every live terminal already has a window", () => {
    useWorkbenchStore.setState({
      queueDrawerOpen: true,
      queue: [],
      workloads: [workload()],
      panes: { "leaf-1": pane() },
    });
    const html = renderToStaticMarkup(
      <ControllerContext.Provider value={{} as SessionController}>
        <QueueDrawer />
      </ControllerContext.Provider>,
    );
    expect(html).toContain("모두 연결(0)");
    expect(html).toContain('disabled=""');
  });

  it("counts only live sessions for connect-all, not queued or finished ones", () => {
    useWorkbenchStore.setState({
      queueDrawerOpen: true,
      queue: [{ workload_id: "w-queued", request_id: "req-1", priority: 1, effective_priority: 1, queued_at_ms: 1 }],
      workloads: [
        // 대기 중(session 없음)과 끝난 작업은 연결 대상이 아니다.
        workload({ workload_id: "w-queued", session_id: null, state: "QUEUED", connection: "detached" }),
        workload({ workload_id: "w-done", session_id: "s-done", state: "SUCCEEDED", connection: "detached" }),
        workload({ workload_id: "w-live", session_id: "s-live", connection: "detached" }),
      ],
      panes: {},
    });
    const html = renderToStaticMarkup(
      <ControllerContext.Provider value={{} as SessionController}>
        <QueueDrawer />
      </ControllerContext.Provider>,
    );
    expect(html).toContain("모두 연결(1)");
  });

  it("counts only finished terminals that last ran an agent for resume-all", () => {
    useWorkbenchStore.setState({
      queueDrawerOpen: true,
      queue: [],
      workloads: [
        // 기록으로 남은 에이전트, 종료 순간의 에이전트 → 재개 대상.
        workload({ workload_id: "w-claude", session_id: "s-claude", mode: "shell", title: "zsh", state: "SUCCEEDED", connection: "detached" }),
        workload({ workload_id: "w-opencode", session_id: "s-opencode", mode: "shell", title: "zsh", state: "SUCCEEDED", connection: "detached", agent: { agent: "opencode", pid: 7, detected_at_ms: 1 } }),
        // 에이전트를 실행한 적 없는 일반 셸과 살아 있는 작업은 제외한다.
        workload({ workload_id: "w-shell", session_id: "s-shell", mode: "shell", title: "zsh", state: "SUCCEEDED", connection: "detached" }),
        workload({ workload_id: "w-live", session_id: "s-live", connection: "detached" }),
      ],
      panes: {},
      workloadMemory: { "w-claude": { title: "✳ Fix login bug", agent: "claude" } },
    });
    const html = renderToStaticMarkup(
      <ControllerContext.Provider value={{} as SessionController}>
        <QueueDrawer />
      </ControllerContext.Provider>,
    );
    expect(html).toContain("모두 재개(2)");
  });

  it("disables resume-all when no finished terminal ran an agent", () => {
    useWorkbenchStore.setState({
      queueDrawerOpen: true,
      queue: [],
      workloads: [
        workload({ workload_id: "w-shell", session_id: "s-shell", mode: "shell", title: "zsh", state: "SUCCEEDED", connection: "detached" }),
      ],
      panes: {},
    });
    const html = renderToStaticMarkup(
      <ControllerContext.Provider value={{} as SessionController}>
        <QueueDrawer />
      </ControllerContext.Provider>,
    );
    expect(html).toContain("모두 재개(0)");
    expect(html).toContain('disabled=""');
  });
});

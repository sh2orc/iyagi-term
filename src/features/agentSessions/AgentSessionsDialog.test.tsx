/**
 * "최근 에이전트 세션" 대화상자(04-ui.md §5): 행 렌더링과 행 동작.
 *
 * 이 저장소의 시험 환경에는 DOM이 없어 클릭을 흉내 낼 수 없다 — 버튼이
 * 부르는 것과 똑같은 agentSessionActions를 직접 호출해 계약을 고정한다.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ControllerContext } from "../../app/controllerContext";
import { useWorkbenchStore } from "../../store/workbenchStore";
import type { SessionController } from "../terminal/sessionController";
import type { AgentSessionRecord } from "../../generated/AgentSessionRecord";
import { AgentSessionsDialog } from "./AgentSessionsDialog";
import { agentSessionActions } from "./actions";
import { t } from "../../i18n";

// SSR은 zustand의 초기 스냅샷을 읽는다 — 시험이 넣은 현재 상태(workloadBySession)를
// 고르게 selector를 현재 getState로 직접 부른다(ResourceStrip.test와 같은 처방).
vi.mock("../../store/workbenchStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../store/workbenchStore")>();
  const store = actual.useWorkbenchStore;
  return { ...actual, useWorkbenchStore: Object.assign((selector: (state: ReturnType<typeof store.getState>) => unknown) => selector(store.getState()), store) };
});

function record(overrides: Partial<AgentSessionRecord> = {}): AgentSessionRecord {
  return {
    id: "rec-1",
    workload_id: "w-1",
    pty_session_id: "s-1",
    agent: "claude",
    agent_session_id: "agent-session-0001",
    cwd: "/work/iyagi",
    title: "iyagi",
    program: "/opt/bin/claude",
    source: "registry",
    first_seen_at: "2026-09-10T00:00:00Z",
    last_seen_at: new Date(Date.now() - 3 * 60_000).toISOString(),
    ended_at: null,
    end_reason: null,
    active: false,
    ...overrides,
  };
}

function render(records: AgentSessionRecord[]): string {
  return renderToStaticMarkup(
    <ControllerContext.Provider value={{} as SessionController}>
      <AgentSessionsDialog client={null} records={records} />
    </ControllerContext.Provider>,
  );
}

describe("AgentSessionsDialog 렌더", () => {
  it("에이전트·제목·경로·상대 시각과 행 동작을 낸다", () => {
    const html = render([
      record(),
      record({
        id: "rec-2",
        agent: "codex",
        title: null,
        agent_session_id: "0123456789abcdef",
        cwd: "/work/other",
        active: true,
      }),
    ]);

    expect(html).toContain("agent-sessions-dialog");
    expect(html).toContain(t("agentSessions.title"));
    expect(html).toContain("Claude Code");
    expect(html).toContain("iyagi");
    expect(html).toContain("/work/iyagi");
    expect(html).toContain(t("agentSessions.time.minutes", { n: 3 }));
    // 제목이 없으면 세션 id 앞 8자로 대신한다.
    expect(html).toContain("01234567");
    // 살아 있는 세션은 "실행 중" 표시 + 이동, 끝난 세션은 이어서 열기.
    expect(html).toContain(t("agentSessions.active"));
    expect(html).toContain(t("agentSessions.goto"));
    expect(html).toContain(t("agentSessions.resume"));
    expect(html).toContain(t("agentSessions.forget"));
    expect(html).not.toContain("agentSessions.");
  });

  it("빈 목록과 오류 상태를 각각 안내한다", () => {
    expect(render([])).toContain(t("agentSessions.empty"));
    const failing = renderToStaticMarkup(
      <ControllerContext.Provider value={{} as SessionController}>
        <AgentSessionsDialog client={null} />
      </ControllerContext.Provider>,
    );
    // client가 없으면 첫 렌더는 loading이고 effect에서 error로 넘어간다.
    expect(failing).toContain(t("agentSessions.loading"));
  });

  it("OpenCode 기록에서도 이어서 열기 버튼을 제공한다", () => {
    const html = render([record({ agent: "opencode", active: false, agent_session_id: "ses_123abc" })]);
    expect(html).toContain(t("agentSessions.resume"));
    expect(html).not.toContain("disabled");
    expect(html).not.toContain(t("agentSessions.notResumable"));
  });

  it("이어서 열기가 없는 에이전트(unknown-agent)는 버튼을 잠근다", () => {
    const html = render([record({ agent: "unknown-agent", active: false })]);
    expect(html).toContain("disabled");
    expect(html).toContain(t("agentSessions.notResumable"));
  });

  it("PTY를 모르는 살아 있는 기록은 이동을 잠근다", () => {
    const html = render([record({ active: true, pty_session_id: null })]);
    expect(html).toContain("disabled");
    expect(html).toContain(t("agentSessions.gotoUnavailable"));
  });

  it("인자로 쓸 수 없는 세션 id(플래그처럼 보이는 값)도 이어서 열기를 잠근다", () => {
    const html = render([record({ agent_session_id: "--yolo", title: null })]);
    expect(html).toContain("disabled");
    expect(html).toContain(t("agentSessions.notResumable"));
  });

  it("닫기 버튼이 초점을 받는다(다른 대화상자와 같은 Escape 경로)", () => {
    // Modals.tsx의 다른 대화상자처럼 컨테이너 onKeyDown + 버튼 autoFocus다.
    expect(render([record()])).toContain("autofocus");
  });
});

// 가드 일시정지(08 §5): 살아 있는 행이 정지 중이면 "실행 중" 옆에 칩을 얹는다 —
// 이 대화상자에서는 pane 헤더 배지가 보이지 않으니 여기서 알려야 한다.
describe("AgentSessionsDialog 가드 일시정지 칩", () => {
  afterEach(() => useWorkbenchStore.setState({ workloads: [], workloadBySession: new Map() }));

  function storeWorkload(sessionId: string, guard: object): void {
    const workload = {
      workload_id: "w-guard",
      session_id: sessionId,
      mode: "shell",
      state: "RUNNING",
      guard,
    } as unknown as ReturnType<typeof useWorkbenchStore.getState>["workloads"][number];
    useWorkbenchStore.setState({
      workloads: [workload],
      workloadBySession: new Map([[sessionId, workload]]),
    });
  }

  it("정지 중인 살아 있는 세션에 칩과 이유 tooltip을 단다", () => {
    storeWorkload("s-1", { kind: "SUSPENDED", since_ms: "1", reason: "cpu_limit", manual: false, partial: false });
    const html = render([record({ active: true })]);
    expect(html).toContain("agent-session-suspended");
    expect(html).toContain(t("agentSessions.suspended"));
    expect(html).toContain(t("queue.guard.reason.cpu_limit"));
  });

  it("워크로드가 살아 있어도 정지가 아니면 칩을 그리지 않는다", () => {
    storeWorkload("s-1", { kind: "NONE" });
    const html = render([record({ active: true })]);
    expect(html).not.toContain("agent-session-suspended");
  });

  it("끝난 세션에는 칩을 달지 않는다", () => {
    storeWorkload("s-1", { kind: "SUSPENDED", since_ms: "1", reason: "cpu_limit", manual: false, partial: false });
    const html = render([record({ active: false })]);
    expect(html).not.toContain("agent-session-suspended");
  });
});

describe("AgentSessionsDialog 행 동작", () => {
  function deps() {
    return {
      client: { agentSessionForget: vi.fn(async () => ({ forgotten: true })) },
      controller: {
        resumeAgentSession: vi.fn(async () => undefined),
        focusSessionPane: vi.fn(() => undefined),
      },
      close: vi.fn(),
      reload: vi.fn(),
      onError: vi.fn(),
    };
  }

  it("이어서 열기는 기록된 정보로 새 pane을 열고 대화상자를 닫는다", async () => {
    const d = deps();
    await agentSessionActions(d).resume(record());

    expect(d.controller.resumeAgentSession).toHaveBeenCalledWith(
      {
        recordId: "rec-1",
        agent: "claude",
        agentSessionId: "agent-session-0001",
        cwd: "/work/iyagi",
        title: "iyagi",
        program: "/opt/bin/claude",
      },
      { newPane: true },
    );
    expect(d.close).toHaveBeenCalled();
  });

  it("재개를 지원하지 않는 에이전트는 아무것도 하지 않는다", async () => {
    const d = deps();
    await agentSessionActions(d).resume(record({ agent: "unknown-agent" }));
    expect(d.controller.resumeAgentSession).not.toHaveBeenCalled();
    expect(d.close).not.toHaveBeenCalled();
  });

  it("인자로 쓸 수 없는 세션 id는 실행을 시도하지 않는다", async () => {
    const d = deps();
    await agentSessionActions(d).resume(record({ agent_session_id: "--yolo" }));
    expect(d.controller.resumeAgentSession).not.toHaveBeenCalled();
    expect(d.close).not.toHaveBeenCalled();
  });

  it("이어서 열기가 실패하면 닫지 않고 사유를 보여 준다", async () => {
    const d = deps();
    d.controller.resumeAgentSession = vi.fn(async () => {
      throw new Error("launch exploded");
    });
    await agentSessionActions(d).resume(record());
    expect(d.close).not.toHaveBeenCalled();
    expect(d.onError).toHaveBeenCalledWith(t("terminal.resume.launchFailed"));
  });

  it("실행이 끝나기 전에는 대화상자를 닫지 않는다", async () => {
    const d = deps();
    let release = (): void => undefined;
    d.controller.resumeAgentSession = vi.fn(
      () =>
        new Promise<undefined>((resolve) => {
          release = () => resolve(undefined);
        }),
    );
    const pending = agentSessionActions(d).resume(record());
    expect(d.close).not.toHaveBeenCalled();
    release();
    await pending;
    expect(d.close).toHaveBeenCalled();
    expect(d.onError).toHaveBeenCalledWith(null);
  });

  it("이동은 기록된 PTY 세션의 pane으로 보낸다", () => {
    const d = deps();
    agentSessionActions(d).focus(record({ active: true, pty_session_id: "s-live" }));
    expect(d.controller.focusSessionPane).toHaveBeenCalledWith("s-live");
  });

  it("제거는 forget 후 목록을 다시 읽는다", async () => {
    const d = deps();
    await agentSessionActions(d).forget(record());
    expect(d.client.agentSessionForget).toHaveBeenCalledWith({ id: "rec-1" });
    expect(d.reload).toHaveBeenCalled();
    expect(d.onError).toHaveBeenCalledWith(null);
  });

  it("제거가 실패하면 목록을 다시 읽지 않고 사유만 알린다", async () => {
    const d = deps();
    d.client.agentSessionForget = vi.fn(async () => {
      throw new Error("nope");
    });
    await agentSessionActions(d).forget(record());
    expect(d.reload).not.toHaveBeenCalled();
    expect(d.onError).toHaveBeenCalledWith(t("agentSessions.forgetFailed"));
  });
});

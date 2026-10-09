import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ControllerContext } from "../../app/controllerContext";
import { useWorkbenchStore, type PaneExitInfo, type PaneMeta } from "../../store/workbenchStore";
import type { SessionController } from "./sessionController";
import { agentMissionMenuItems, TerminalPane } from "./TerminalPane";
import type { AgentStatus } from "../../generated/AgentStatus";
import { t } from "../../i18n";
import { clearTerminalActivity, recordSessionOutput } from "./activity";

// React's server snapshot is fixed when the Zustand store is created. Read the
// current state directly so each case can exercise the requested pane phase.
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

const pane: PaneMeta = {
  leafId: "leaf-1",
  viewId: "view-1",
  sessionId: "session-1",
  workloadId: "workload-1",
  title: "zsh",
  cwd: "/tmp",
  phase: "replaying",
  error: null,
  usage: null,
  flowBlocked: false,
};

function renderPaneWith(overrides: Partial<PaneMeta>): string {
  useWorkbenchStore.setState({
    panes: { [pane.leafId]: { ...pane, ...overrides } },
    focusedLeafId: pane.leafId,
    broadcastInput: false,
  });
  return renderToStaticMarkup(
    <ControllerContext.Provider value={{} as SessionController}>
      <TerminalPane leafId={pane.leafId} />
    </ControllerContext.Provider>,
  );
}

function renderPane(phase: PaneMeta["phase"]): string {
  return renderPaneWith({ phase });
}

afterEach(() => {
  useWorkbenchStore.setState({ panes: {}, focusedLeafId: null, broadcastInput: false });
});

describe("TerminalPane replay visibility", () => {
  it("복구 중인 pane을 busy로 표시하고 replay phase를 DOM에 노출한다", () => {
    const html = renderPane("replaying");

    expect(html).toContain('class="pane-body" data-phase="replaying" aria-busy="true"');
    expect(html).toContain("기록 재생 중…");
  });

  it("재생 중 오버레이는 터미널을 가리지 않고 반투명 배지로만 알린다", () => {
    const html = renderPane("replaying");

    // 화면을 통째로 덮는 오버레이가 아니라 위에 얹는 배지 판(pill)이다 —
    // 터미널이 비치므로 재생이 살아 보이고 숨김→드러냄 깜빡임도 없다.
    expect(html).toContain('class="pane-overlay pane-overlay-replay" role="status"');
    expect(html).toContain('class="pane-replay-badge"');
    expect(html).toContain("기록 재생 중…");
  });

  it("시작·실패·종료 오버레이는 그대로 화면을 덮는다", () => {
    expect(renderPane("starting")).toContain('<div class="pane-overlay" role="status">');
    expect(renderPaneWith({ phase: "failed", error: "boom" })).toContain(
      'class="pane-overlay pane-overlay-error" role="alert"',
    );
    expect(renderPane("exited")).toContain('class="pane-overlay pane-overlay-exited" role="status"');
  });

  it("live 전환 뒤에는 busy 표시를 제거한다", () => {
    const html = renderPane("live");

    expect(html).toContain('class="pane-body" data-phase="live"');
    expect(html).not.toContain('aria-busy="true"');
  });

  it("정상 상태(live)의 '실행 중' 문구는 헤더에 적지 않고, 그 밖의 phase만 적는다", () => {
    expect(renderPane("live")).not.toContain("pane-state");
    const replaying = renderPane("replaying");
    expect(replaying).toContain("pane-state state-replaying");
    expect(replaying).toContain(t("monitor.phase.replaying"));
  });
});

/**
 * pane 메뉴 버튼(04-ui §2-5): 메뉴 자체는 클릭으로만 열리므로(포털·초점은
 * 브라우저의 일) 여기서는 "여는 길이 헤더에 있고, 처음부터 열려 있지는
 * 않다"만 고정한다.
 */
describe("pane 메뉴 버튼(04-ui §2-5)", () => {
  it("닫기 버튼 앞에 서고, 메뉴를 여는 버튼임을 알린다", () => {
    const html = renderPaneWith({ phase: "live" });

    expect(html).toContain('class="pane-menu"');
    expect(html).toContain(`aria-label="${t("app.paneMenu.aria")}"`);
    expect(html).toContain('aria-haspopup="menu"');
    expect(html.indexOf("pane-menu")).toBeLessThan(html.indexOf("pane-close"));
    // 눌러야 열린다 — 처음부터 떠 있지 않다.
    expect(html).not.toContain('role="menu"');
  });
});

describe("에이전트 세션 배지(04-ui §5)", () => {
  const agent = {
    agent: "claude",
    pid: 4321,
    detected_at_ms: 1000,
    session_id: "0123456789abcdef",
    session_name: null,
    session_source: "registry" as const,
    session_status: null as string | null,
  };

  it("세션 이름이 없으면 id 앞 8자를 이름 뒤에 붙이고 툴팁에 재개 명령을 담는다", () => {
    const html = renderPaneWith({ phase: "live", agent });

    expect(html).toContain("pane-agent-badge agent-claude");
    expect(html).toContain("Claude Code");
    expect(html).toContain("01234567");
    expect(html).toContain("세션 id: 0123456789abcdef");
    expect(html).toContain("claude --resume 0123456789abcdef");
    expect(html).not.toContain("agent-busy");
  });

  it("에이전트가 붙인 이름이 있으면 그 이름을 쓴다", () => {
    const html = renderPaneWith({ phase: "live", agent: { ...agent, session_name: "iyagi-7d" } });
    // 보이는 표시는 이름, 전체 id는 툴팁에만 남는다.
    expect(html).toContain('<span class="pane-agent-session"> \u00b7 iyagi-7d</span>');
    expect(html).not.toContain("\u00b7 01234567");
  });

  it("세션 id를 모르면 아무 표시도 붙이지 않는다", () => {
    const html = renderPaneWith({ phase: "live", agent: { ...agent, session_id: null } });
    expect(html).toContain("Claude Code");
    expect(html).not.toContain("pane-agent-session");
    expect(html).not.toContain("세션 id");
  });

  it("busy를 보고해도 배지에는 상태 점을 붙이지 않는다 — 타이틀 앞 마커로만 알린다", () => {
    const html = renderPaneWith({ phase: "live", agent: { ...agent, session_status: "busy" } });
    expect(html).toContain('class="pane-agent-badge agent-claude"');
    expect(html).not.toContain("agent-busy");
  });

  it("busy·shell이면 타이틀 앞에 작업 중 마커를 둔다", () => {
    for (const session_status of ["busy", "shell"]) {
      const html = renderPaneWith({ phase: "live", agent: { ...agent, session_status } });
      expect(html).toContain('class="pane-agent-marker working"');
      expect(html).toContain(t("terminal.agent.working", { name: "Claude Code" }));
      // 마커는 타이틀 앞이다 — iTerm2에서 CLI가 제목에 스피너를 넣는 그 자리.
      expect(html.indexOf("pane-agent-marker")).toBeLessThan(html.indexOf("pane-title"));
    }
  });

  it("waiting이면 확인 대기 마커를 둔다", () => {
    const html = renderPaneWith({ phase: "live", agent: { ...agent, session_status: "waiting" } });
    expect(html).toContain('class="pane-agent-marker waiting"');
    expect(html).toContain(t("terminal.agent.waiting", { name: "Claude Code" }));
    expect(html).not.toContain("agent-busy");
  });

  it("idle이거나 상태를 모르고 출력도 없으면 마커가 없다", () => {
    for (const session_status of ["idle", null]) {
      const html = renderPaneWith({ phase: "live", agent: { ...agent, session_status } });
      expect(html).not.toContain("pane-agent-marker");
    }
  });

  it("live가 아닌 pane에는 마커를 두지 않는다", () => {
    const html = renderPaneWith({ phase: "exited", agent: { ...agent, session_status: "busy" } });
    expect(html).not.toContain("pane-agent-marker");
  });

  it("codex는 그 CLI의 재개 명령을 툴팁에 넣는다", () => {
    const html = renderPaneWith({ phase: "live", agent: { ...agent, agent: "codex" } });
    expect(html).toContain("codex resume 0123456789abcdef");
  });
});

describe("이어서 열 수 있는 종료 pane 오버레이(04-ui §5)", () => {
  const resume = {
    recordId: "rec-1",
    agent: "claude" as const,
    agentSessionId: "0123456789abcdef",
    cwd: "/work/iyagi",
    title: "iyagi",
    program: "/opt/bin/claude",
  };

  it("어떤 세션을 어디서 여는지와 세 가지 선택지를 낸다", () => {
    const html = renderPaneWith({ phase: "exited", sessionId: null, workloadId: null, resume });

    expect(html).toContain("pane-overlay-resume");
    expect(html).toContain(t("terminal.resume.title", { agent: "Claude Code" }));
    expect(html).toContain("iyagi");
    expect(html).toContain("/work/iyagi");
    expect(html).toContain(t("terminal.resume.open"));
    expect(html).toContain(t("terminal.resume.newShell"));
    expect(html).toContain(t("terminal.overlay.close"));
    // 평범한 종료 문구는 이 오버레이가 대신한다.
    expect(html).not.toContain(t("terminal.overlay.exited"));
  });

  it("세션 이름과 작업 경로를 한 줄에 둬 제목·위치·선택지 세 줄로 그린다", () => {
    const html = renderPaneWith({ phase: "exited", sessionId: null, workloadId: null, resume });
    const where = html.match(/<p class="pane-resume-where">(.*?)<\/p>/s)?.[1] ?? "";
    expect(where).toContain("iyagi");
    expect(where).toContain("/work/iyagi");
    // 예전처럼 경로를 별도 <p>로 그리지 않는다.
    expect(html).not.toMatch(/<p class="pane-resume-cwd"/);
  });

  it("홈 아래 경로는 ~로 줄여 보이고 툴팁에는 원문을 남긴다", () => {
    useWorkbenchStore.setState({ homeDir: "/work" });
    try {
      const html = renderPaneWith({ phase: "exited", sessionId: null, workloadId: null, resume, cwd: "/work/other" });
      expect(html).toContain(">~/iyagi<");
      expect(html).toContain(`${t("terminal.resume.cwdLabel")}: /work/iyagi`);
      // 헤더의 cwd도 같은 규칙, 툴팁은 원문.
      expect(html).toContain('<span class="pane-cwd">~/other</span>');
      expect(html).toContain('title="/work/other"');
    } finally {
      useWorkbenchStore.setState({ homeDir: null });
    }
  });

  it("세션 이름 툴팁에 전체 id와 그 CLI의 재개 명령을 넣는다(배지와 같은 규칙)", () => {
    const html = renderPaneWith({ phase: "exited", sessionId: null, workloadId: null, resume });
    expect(html).toContain(t("terminal.agent.sessionId", { id: "0123456789abcdef" }));
    expect(html).toContain("claude --resume 0123456789abcdef");
  });

  it("비정상 종료면 제목 아래에 사유 한 줄을 더한다", () => {
    const exit: PaneExitInfo = { code: 1, reason: "process_exit", detail: null };
    const html = renderPaneWith({ phase: "exited", sessionId: null, workloadId: null, resume, exit });
    expect(html).toContain("pane-exit-reason");
    expect(html).toContain(t("monitor.exit.processExit", { code: 1 }));
    // 사유는 제목과 위치 줄 사이에 온다.
    expect(html.indexOf("pane-resume-title")).toBeLessThan(html.indexOf("pane-exit-reason"));
    expect(html.indexOf("pane-exit-reason")).toBeLessThan(html.indexOf("pane-resume-where"));
  });

  const quietExits: Array<[string, PaneExitInfo]> = [
    ["정상 종료(코드 0)", { code: 0, reason: "process_exit", detail: null }],
    ["사용자 취소", { code: null, reason: "cancelled", detail: null }],
  ];
  it.each(quietExits)("%s는 세 줄 그대로 — 사유 줄을 붙이지 않는다", (_label, exit) => {
    const html = renderPaneWith({ phase: "exited", sessionId: null, workloadId: null, resume, exit });
    expect(html).not.toContain("pane-exit-reason");
  });

  it("재개 기록이 없으면 기존 종료 오버레이 그대로다", () => {
    const html = renderPaneWith({ phase: "exited", resume: null });
    expect(html).toContain(t("terminal.overlay.exited"));
    expect(html).not.toContain("pane-overlay-resume");
  });
});

describe("에이전트 배지의 현재 모델·effort(/model·/effort 즉시 반영)", () => {
  const base = {
    agent: "claude",
    pid: 42,
    detected_at_ms: 1,
    session_id: "0123456789abcdef",
    session_name: null,
    session_source: null,
    session_status: "idle",
  };

  it.each(["glm-5", "GLM-4.7", " glm-5.1 "])("Claude의 %s 모델은 Z.ai로 표시한다", (model) => {
    const html = renderPaneWith({ phase: "live", agent: { ...base, model } });
    expect(html).toContain("Z.ai");
    expect(html).toContain(t("terminal.agent.tooltip", { name: "Z.ai" }));
    expect(html).not.toContain(">Claude Code");
  });

  it("GLM이 아닌 Claude와 다른 CLI는 이름을 유지한다", () => {
    expect(renderPaneWith({ phase: "live", agent: { ...base, model: "Opus 5" } })).toContain(">Claude Code");
    expect(renderPaneWith({ phase: "live", agent: { ...base, agent: "codex", model: "glm-5" } })).toContain(">Codex");
  });

  it("모델과 effort를 줄여 배지에 붙이고 원문은 툴팁에 둔다", () => {
    const html = renderPaneWith({
      phase: "live",
      agent: { ...base, model: "Opus 5 (1M context)", effort: "xhigh", model_source: "status_line" },
    });
    expect(html).toContain('<span class="pane-agent-model"> · Opus 5 (1M) · xhigh</span>');
    expect(html).toContain(t("terminal.agent.model", { model: "Opus 5 (1M context)" }));
    expect(html).toContain(t("terminal.agent.effort", { effort: "xhigh" }));
    expect(html).not.toContain(t("terminal.agent.modelProvisional"));
  });

  it("세션 기록 전 잠정값은 provisional로 구분하고 툴팁에 알린다", () => {
    const html = renderPaneWith({
      phase: "live",
      agent: { ...base, model: "sonnet", effort: "high", model_source: "defaults" },
    });
    expect(html).toContain('class="pane-agent-model provisional"');
    expect(html).toContain(t("terminal.agent.modelProvisional"));
  });

  it("모델·effort를 모르면 모델 표시를 붙이지 않는다", () => {
    const html = renderPaneWith({ phase: "live", agent: { ...base } });
    expect(html).not.toContain("pane-agent-model");
  });
});

describe("마지막 출력 경과 배지(에이전트 pane)", () => {
  const agent = {
    agent: "claude",
    pid: 4321,
    detected_at_ms: 1000,
    session_id: null,
    session_name: null,
    session_source: null,
    session_status: null as string | null,
  };

  afterEach(() => {
    clearTerminalActivity(pane.sessionId ?? "session-1");
    vi.useRealTimers();
  });

  /** 시각을 고정하고 pane 세션의 마지막 출력을 elapsedMs 전으로 둔다. */
  function seedLastOutput(elapsedMs: number): void {
    vi.useFakeTimers({ now: new Date("2026-01-15T14:00:00").getTime() });
    recordSessionOutput("session-1");
    vi.advanceTimersByTime(elapsedMs);
  }

  it("에이전트가 감지된 live pane에 경과를 적는다 — 90초면 '1분'", () => {
    seedLastOutput(90_000);
    const html = renderPaneWith({ phase: "live", agent });

    expect(html).toContain("pane-last-output");
    expect(html).toContain(t("terminal.pane.lastOutput.label", { elapsed: t("terminal.pane.lastOutput.minutes", { n: 1 }) }));
    // 절대 시각은 툴팁에 — 배지 텍스트와 함께 근거를 남긴다.
    expect(html).toContain(t("terminal.pane.lastOutput.tooltip", { time: new Date("2026-01-15T14:00:00").toLocaleTimeString() }));
    expect(html).not.toContain("pane-last-output stale");
  });

  it("30분 이상 조용하면 한 단계 더 강조한다", () => {
    seedLastOutput(40 * 60_000);
    const html = renderPaneWith({ phase: "live", agent });

    expect(html).toContain('class="pane-last-output stale-old"');
    expect(html).toContain(t("terminal.pane.lastOutput.minutes", { n: 40 }));
  });

  it("마지막 출력이 10초 이내면 띄우지 않고, 10초부터 초 단위로 적는다", () => {
    seedLastOutput(9_000);
    expect(renderPaneWith({ phase: "live", agent })).not.toContain("pane-last-output");

    vi.advanceTimersByTime(1_000);
    const html = renderPaneWith({ phase: "live", agent });
    expect(html).toContain(t("terminal.pane.lastOutput.label", { elapsed: t("terminal.pane.lastOutput.seconds", { n: 10 }) }));
  });

  it("출력 기록이 없으면(세션 막 시작) 배지를 띄우지 않는다", () => {
    const html = renderPaneWith({ phase: "live", agent });
    expect(html).not.toContain("pane-last-output");
  });

  it("에이전트가 없는 pane에는 경과를 보이지 않는다", () => {
    seedLastOutput(40 * 60_000);
    const html = renderPaneWith({ phase: "live", agent: null });
    expect(html).not.toContain("pane-last-output");
  });

  it("live가 아닌 pane에는 경과를 보이지 않는다", () => {
    seedLastOutput(90_000);
    const html = renderPaneWith({ phase: "exited", agent });
    expect(html).not.toContain("pane-last-output");
  });
});

/**
 * 압력 완화 배지(08-pressure-relief §2): 상태만 알리고, 툴팁이 자동/수동·
 * 경과·부분 적용을 설명한다. 보호 표시는 따로 선다.
 */
describe("TerminalPane 완화 배지", () => {
  afterEach(() => useWorkbenchStore.setState({ host: null }));

  it("양보 중인 pane에 배지를 세우고 자동 양보 사유를 툴팁에 담는다", () => {
    const html = renderPaneWith({
      phase: "live",
      relief: { kind: "YIELDED", sinceMs: null, manual: false, partial: false },
    });

    expect(html).toContain('class="pane-relief" role="status"');
    expect(html).toContain(t("terminal.pane.yielded"));
    expect(html).toContain(t("terminal.pane.yieldedAuto"));
    expect(html).not.toContain(t("terminal.pane.yieldedPartial"));
    expect(html).not.toContain("pane-protected");
  });

  it("수동 양보와 부분 적용, 경과를 툴팁 한 줄에 잇는다", () => {
    // 경과는 데몬의 monotonic 시계 기준이다 — 마지막 host sample과 비교한다.
    useWorkbenchStore.setState({
      host: { monotonic_ms: 100_000 } as unknown as NonNullable<
        ReturnType<typeof useWorkbenchStore.getState>["host"]
      >,
    });
    const html = renderPaneWith({
      phase: "live",
      relief: { kind: "YIELDED", sinceMs: 40_000, manual: true, partial: true },
    });

    expect(html).toContain(t("terminal.pane.yieldedManual"));
    expect(html).toContain(
      t("terminal.pane.yieldedSince", { elapsed: t("terminal.pane.lastOutput.minutes", { n: 1 }) }),
    );
    expect(html).toContain(t("terminal.pane.yieldedPartial"));
  });

  it("보호 표시는 양보와 별개로 선다", () => {
    const html = renderPaneWith({ phase: "live", protected: true });

    expect(html).toContain("pane-protected");
    expect(html).toContain(t("terminal.pane.protected"));
    expect(html).not.toContain("pane-relief");
  });

  it("완화가 없으면 아무 배지도 그리지 않는다", () => {
    const html = renderPaneWith({ phase: "live", relief: { kind: "NONE" }, protected: false });

    expect(html).not.toContain("pane-relief");
    expect(html).not.toContain("pane-protected");
  });
});

describe("TerminalPane ⋯ 메뉴 — 이 저장소에서 AI 팀에 맡기기", () => {
  const agent = { agent: "claude" } as AgentStatus;

  afterEach(() => useWorkbenchStore.setState({ modal: null }));

  it("에이전트가 없는 pane에는 항목이 없다", () => {
    expect(agentMissionMenuItems({ agent: null, project: "/repo", cwd: "/repo/src" }, 1, t)).toEqual([]);
  });

  it("에이전트 pane은 git 최상위(없으면 cwd)로 새 AI 작업 대화상자를 열고, 대화가 옮겨지지 않음을 툴팁으로 알린다", () => {
    const [item] = agentMissionMenuItems({ agent, project: "/repo", cwd: "/repo/src" }, 1, t);
    expect(item).toMatchObject({
      label: t("terminal.pane.missionFromAgent"),
      disabled: false,
      title: t("terminal.pane.missionFromAgentHint"),
    });
    item.onSelect();
    expect(useWorkbenchStore.getState().missionCreate).toEqual({ kind: "mission-create", repositoryPath: "/repo" });
    const [fromCwd] = agentMissionMenuItems({ agent, project: null, cwd: "/work/app" }, 1, t);
    fromCwd.onSelect();
    expect(useWorkbenchStore.getState().missionCreate).toEqual({ kind: "mission-create", repositoryPath: "/work/app" });
  });

  it("데몬이 프로토콜을 선언하지 않으면(개발 빌드) 사유와 함께 흐리게 둔다", () => {
    const [item] = agentMissionMenuItems({ agent, project: "/repo", cwd: "/repo" }, null, t);
    expect(item).toMatchObject({ disabled: true, title: t("missions.newMission.unavailable") });
  });
});

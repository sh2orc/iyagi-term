/**
 * 입력 건강 배지(04-ui §5-1)의 suspended 클리어 계약 — happy-dom(*.dom.test.tsx).
 *
 * 'suspended' 입력 문제는 가드 미러(스냅샷 workloads)가 일시정지→실행
 * "전이"를 확인할 때만 걷힌다. 거절 직후 도착한 오래된 Running 스냅샷이
 * 방금 설정된 표시를 지우면, 데몬이 실제로 정지시킨 상태를 UI가 숨긴다
 * (스냅샷 지연 ≈1 s). 반대로 가드가 포커스로 재개한 뒤에도 표시가 남으면
 * 돌아와 있는 세션을 틀리게 알린다 — 두 방향을 모두 잰다.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ControllerContext } from "../../app/controllerContext";
import { TerminalRegistry } from "./registry";
import { TerminalPane } from "./TerminalPane";
import { inputIssueOf, setInputIssue, useInputHealth } from "./inputHealth";
import { useWorkbenchStore, type PaneMeta } from "../../store/workbenchStore";
import type { WorkloadSummary } from "../../generated/WorkloadSummary";
import type { SessionController } from "./sessionController";
import type { TerminalLike } from "./registry";

let root: Root | null = null;
let container: HTMLElement | null = null;

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
  } as TerminalLike;
}

/** TerminalView가 쓰는 seam(registry)만 갖춘 최소 컨트롤러. */
function fakeController(): SessionController {
  const registry = new TerminalRegistry({ createTerminal: fakeTerminal });
  // createDom을 주지 않는다 — 진짜 호스트에 붙이는 테스트라 registry의
  // 기본 factory(document.createElement)가 happy-dom 노드를 만들어야 한다.
  return { registry } as unknown as SessionController;
}

function pane(): PaneMeta {
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
  };
}

function workload(guard: WorkloadSummary["guard"]): WorkloadSummary[] {
  return [
    {
      workload_id: "workload-1",
      session_id: "session-1",
      mode: "shell",
      state: "RUNNING",
      priority: 1,
      title: "zsh",
      cwd: "/work",
      program: "/bin/zsh",
      reservation_bytes: "1073741824",
      cpu_slots: 1,
      enforcement: "observe",
      root_exited: false,
      cancel_requested: false,
      connection: "attached",
      relief: { kind: "NONE" },
      guard,
      guard_warning: null,
      protected: false,
    },
  ];
}

const suspendedGuard = {
  kind: "SUSPENDED",
  since_ms: "0",
  reason: "cpu_limit",
  manual: false,
  partial: false,
} as const;

async function renderPane(): Promise<HTMLElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  const controller = fakeController();
  await act(async () => {
    root?.render(
      createElement(
        ControllerContext.Provider,
        { value: controller },
        createElement(TerminalPane, { leafId: "leaf-1" }),
      ),
    );
  });
  return container;
}

/** workloads를 갈아끼운다 — 스토어 불변식대로 파생 인덱스도 함께 유지한다. */
function setWorkloads(list: WorkloadSummary[]): void {
  useWorkbenchStore.setState({
    workloads: list,
    workloadById: new Map(list.map((w) => [w.workload_id, w])),
    workloadBySession: new Map(
      list.filter((w) => w.session_id != null).map((w) => [w.session_id as string, w]),
    ),
  });
}

function setMirror(guard: WorkloadSummary["guard"]): void {
  act(() => {
    setWorkloads(workload(guard));
  });
}

afterEach(() => {
  if (root) {
    act(() => root?.unmount());
    root = null;
  }
  container?.remove();
  container = null;
  setInputIssue("session-1", null);
  setWorkloads([]);
  useWorkbenchStore.setState({ panes: {}, focusedLeafId: null });
  useInputHealth.setState({ issues: new Map() });
});

describe("InputHealthBadges — suspended 표시 걷기", () => {
  it("미러가 일시정지를 본 뒤 실행으로 풀리면 표시와 issue를 함께 걷는다", async () => {
    useWorkbenchStore.setState({ panes: { "leaf-1": pane() }, focusedLeafId: null });
    setWorkloads(workload(suspendedGuard));
    setInputIssue("session-1", "suspended");
    const html = await renderPane();
    // 정지 미러 + 재개 버튼이 보인다.
    expect(html.querySelector(".pane-guard")).not.toBeNull();
    expect(html.querySelector(".pane-guard-resume")).not.toBeNull();
    expect(inputIssueOf("session-1")).toBe("suspended");

    // 가드가 포커스로 재개했다 — 다음 스냅샷이 실행을 확인한다.
    setMirror({ kind: "NONE" });
    expect(inputIssueOf("session-1")).toBeNull();
    expect(html.querySelector(".pane-guard")).toBeNull();
  });

  it("미러가 일시정지를 본 적 없으면(오래된 Running 스냅샷) 방금 설정된 issue를 지우지 않는다", async () => {
    // 거절 응답이 스냅샷보다 먼저 도착한 상황: 미러는 아직 (오래된) Running.
    useWorkbenchStore.setState({ panes: { "leaf-1": pane() }, focusedLeafId: null });
    setWorkloads(workload({ kind: "NONE" }));
    const html = await renderPane();
    act(() => {
      setInputIssue("session-1", "suspended");
    });
    // 이후 오는 같은(Running) 스냅샷 갱신은 전이가 아니므로 걷지 않는다.
    setMirror({ kind: "NONE" });
    expect(inputIssueOf("session-1")).toBe("suspended");
    // 배지는 issue만으로도 켜져 있다 — 사용자는 정지를 알게 된다.
    expect(html.querySelector(".pane-guard")).not.toBeNull();
  });

  it("resume 버튼은 가드 워크로드 재개를 부른다", async () => {
    const controller = fakeController();
    const resume = vi.fn(() => Promise.resolve());
    (controller as unknown as { resumeWorkload: unknown }).resumeWorkload = resume;
    useWorkbenchStore.setState({ panes: { "leaf-1": pane() }, focusedLeafId: null });
    setWorkloads(workload(suspendedGuard));
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
    await act(async () => {
      root?.render(
        createElement(
          ControllerContext.Provider,
          { value: controller },
          createElement(TerminalPane, { leafId: "leaf-1" }),
        ),
      );
    });
    const button = container.querySelector<HTMLButtonElement>(".pane-guard-resume");
    expect(button).not.toBeNull();
    await act(async () => {
      button?.click();
    });
    expect(resume).toHaveBeenCalledWith("workload-1");
  });
});

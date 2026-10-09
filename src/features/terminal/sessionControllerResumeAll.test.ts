/**
 * 일시정지 작업 전체 재개(08 §5) — 단축키(⇧⌘R / Ctrl+Shift+R)·메뉴·팔레트가
 * 부르는 동작의 계약:
 * 1) 대상 — 가드가 SUSPENDED로 표시한 작업만. 정지 중이 아닌 작업은 건드리지 않는다.
 * 2) 결과 — 건별 toast 대신 한 번에 알린다(패널이 닫혀 있어도 눌렀는지 알 수 있게).
 *    일부 실패도 같은 자리에서 개수로 알린다.
 * 3) 조용한 no-op — 일시정지 중인 작업이 없으면 RPC도 toast도 없다.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { LaunchRequest } from "../../generated/LaunchRequest";
import { t } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { MockDaemonClient } from "../daemon/mockClient";
import { RpcClientError } from "../daemon/client";
import { SessionController } from "./sessionController";
import { TerminalRegistry, type TerminalLike } from "./registry";

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

function shellLaunch(requestId: string): LaunchRequest {
  return {
    request_id: requestId,
    profile_id: "shell",
    cwd: "/tmp",
    program: "/bin/zsh",
    argv: [],
    env_overrides: {},
    mode: "shell",
    executor: { kind: "local" },
    cols: 80,
    rows: 24,
    priority: 1,
    policy: {
      reservation_bytes: "0",
      cpu_slots: 1,
      enforcement: "observe",
      memory_max_bytes: null,
      cpu_max_cores: null,
      pids_max: null,
    },
  };
}

async function launch(client: MockDaemonClient, requestId: string): Promise<string> {
  const launched = await client.workloadLaunch(shellLaunch(requestId));
  return launched.workload_id;
}

/** 작업을 띄워 일시정지까지 마친 뒤, 그 스냅샷으로 스토어를 채운다(컨트롤러의 snapshot 경로와 같은 값). */
async function seedSuspended(count: number): Promise<{ client: MockDaemonClient; ids: string[] }> {
  const client = new MockDaemonClient({ resourceIntervalMs: 0 });
  const ids: string[] = [];
  for (let index = 0; index < count; index += 1) {
    ids.push(await launch(client, `req-${index}`));
  }
  for (const [index, id] of ids.entries()) {
    await client.workloadSuspend({ request_id: `suspend-${index}`, workload_id: id });
  }
  useWorkbenchStore.getState().applySnapshot(await client.systemSnapshot());
  return { client, ids };
}

beforeEach(() => {
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
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("resumeAllSuspended", () => {
  it("SUSPENDED만 재개하고 결과를 한 번에 알린다", async () => {
    const { client, ids } = await seedSuspended(2);
    const running = await launch(client, "req-running"); // 정지 중이 아닌 작업은 대상이 아니다
    const resume = vi.spyOn(client, "workloadResume");

    const controller = new SessionController({ client, registry: registry(), platform: "darwin" });
    await controller.resumeAllSuspended();

    expect(resume.mock.calls.map((call) => call[0].workload_id).sort()).toEqual([...ids].sort());
    expect(resume.mock.calls.map((call) => call[0].workload_id)).not.toContain(running);
    // 데몬(실제로는 workload.changed 이벤트)이 가드를 풀었다.
    const snapshot = await client.systemSnapshot();
    for (const id of ids) {
      expect(snapshot.workloads.find((w) => w.workload_id === id)?.guard).toEqual({ kind: "NONE" });
    }
    expect(useWorkbenchStore.getState().toast).toBe(t("terminal.workload.resumeAllDone", { n: 2 }));
    controller.dispose();
  });

  it("일시정지 중인 작업이 없으면 RPC도 toast도 없다", async () => {
    const client = new MockDaemonClient({ resourceIntervalMs: 0 });
    await launch(client, "req-running");
    useWorkbenchStore.getState().applySnapshot(await client.systemSnapshot());
    const resume = vi.spyOn(client, "workloadResume");

    const controller = new SessionController({ client, registry: registry(), platform: "darwin" });
    await controller.resumeAllSuspended();

    expect(resume).not.toHaveBeenCalled();
    expect(useWorkbenchStore.getState().toast).toBeNull();
    controller.dispose();
  });

  it("일부 실패는 같은 자리에서 개수로 알린다(건별 toast 아님)", async () => {
    const { client, ids } = await seedSuspended(2);
    vi.spyOn(client, "workloadResume").mockImplementation(async (params) => {
      if (params.workload_id === ids[0]) throw new RpcClientError("INVALID_STATE", "이미 끝난 작업입니다.");
      return MockDaemonClient.prototype.workloadResume.call(client, params);
    });

    const controller = new SessionController({ client, registry: registry(), platform: "darwin" });
    await controller.resumeAllSuspended();

    expect(useWorkbenchStore.getState().toast).toBe(
      t("terminal.workload.resumeAllPartial", { n: 1, failed: 1 }),
    );
    controller.dispose();
  });
});

/**
 * 이 컴퓨터가 받을 수 없는 크기의 예약을 데몬이 거절하지 않고 줄여 받았을 때
 * (effective_policy) 대화상자가 줄어든 값을 알린다 — happy-dom(*.dom.test.tsx).
 */

import { afterEach, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import type { DaemonClient, LaunchOutcome } from "../daemon/client";
import type { Capabilities } from "../../generated/Capabilities";
import type { LaunchRequest } from "../../generated/LaunchRequest";
import type { LaunchProfile } from "../profiles/types";
import { defaultProfilePolicy } from "../profiles/types";
import { t } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { formatGiB } from "../monitor/format";
import { ManagedRunDialog } from "./ManagedRunDialog";

let root: Root | null = null;
let container: HTMLElement | null = null;

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
  root = null;
  container = null;
});

const profile: LaunchProfile = {
  id: "p1",
  label: "agent",
  descriptor: {
    kind: "codex",
    program: "/usr/local/bin/agent",
    argv_prefix: [],
    detected_version: null,
    transport: "pty",
    capabilities: { turn_events: false, resume: false, concurrency_control: false },
  },
  cwd: "/work",
  policy: defaultProfilePolicy(),
  env: [],
  notes: "",
  interpreter: null,
};

const capabilities: Capabilities = {
  memory_limit_kind: { support: "supported" },
  cpu_quota: { support: "supported" },
  process_count_limit: { support: "supported" },
  tree_accounting: { support: "supported" },
  reattach: { support: "supported" },
  resume: { support: "unsupported", reason: "R1" },
  scheduling_yield: { support: "supported" },
  suspend_resume: { support: "supported" },
  platform: "macos",
  claude_provider_routing: true,
};

async function submitWith(fittedBytes: (asked: string) => string): Promise<LaunchRequest | null> {
  let sent: LaunchRequest | null = null;
  const client = {
    systemSnapshot: async () => ({ capabilities }),
    workloadLaunch: async (request: LaunchRequest): Promise<LaunchOutcome> => {
      sent = request;
      return {
        workload_id: "w-1",
        session_id: null,
        state: "QUEUED",
        effective_policy: { ...request.policy, reservation_bytes: fittedBytes(request.policy.reservation_bytes) },
        missing_capabilities: [],
      };
    },
  } as unknown as DaemonClient;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  await act(async () => {
    root?.render(<ManagedRunDialog client={client} platform="darwin" capabilities={capabilities} profiles={[profile]} />);
  });
  const form = container.querySelector("form.managed-run") as HTMLFormElement;
  await act(async () => {
    form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
  });
  return sent;
}

describe("관리 실행: 호스트에 맞춰 줄어든 예약", () => {
  it("줄었으면 대기 안내와 함께 줄어든 크기를 알린다", async () => {
    useWorkbenchStore.setState({ toast: null });
    const sent = await submitWith(asked => String(Number(asked) / 2));
    expect(sent).not.toBeNull();
    const fitted = Number(sent!.policy.reservation_bytes) / 2;
    expect(useWorkbenchStore.getState().toast).toBe(
      `${t("managed.queuedToast")} · ${t("managed.reservationFitted", { size: formatGiB(fitted) })}`,
    );
  });

  it("그대로면 대기 안내만 보인다", async () => {
    useWorkbenchStore.setState({ toast: null });
    await submitWith(asked => asked);
    expect(useWorkbenchStore.getState().toast).toBe(t("managed.queuedToast"));
  });
});

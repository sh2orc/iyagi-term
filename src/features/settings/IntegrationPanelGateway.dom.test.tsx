/**
 * 연동 패널 — 관측 게이트웨이·키 풀 카드(happy-dom, *.dom.test.tsx).
 *
 * 두 카드는 부모의 공유 `gateway.status` 폴 하나를 읽는다. "지원 안 함"은
 * 모르는 메서드 거절(METHOD_NOT_FOUND, 데몬의 `unknown method`)일 때만이고,
 * 다른 실패는 마지막 상태를 유지한 채 오류만 보인다. 정책을 바꾸면 데몬이 받아들인 뒤에만 선호에 저장한다.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { t, useI18nStore } from "../../i18n";
import { ControllerContext } from "../../app/controllerContext";
import { DEFAULT_PREFERENCES, usePreferences } from "../../store/preferences";
import { RpcClientError } from "../daemon/client";
import type { GatewayStatus } from "../../generated/GatewayStatus";
import type { GatewayPolicyParams } from "../../generated/GatewayPolicyParams";
import type { SessionController } from "../terminal/sessionController";
import { IntegrationPanel } from "./IntegrationPanel";

const pool = vi.hoisted(() => ({ list: vi.fn(), add: vi.fn() }));

vi.mock("../bridge/ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../bridge/ipc")>();
  // 다른 카드의 IPC는 조용히 실패시킨다 — 이 테스트는 게이트웨이·키 풀만 본다.
  return {
    ...actual,
    isTauri: () => true,
    tauriIpcAdapter: { invoke: async () => { throw new Error("no ipc in test"); } },
  };
});

vi.mock("../subscriptions/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../subscriptions/client")>();
  return { ...actual, listZaiKeyPool: pool.list, addZaiKeyPoolKey: pool.add };
});

let root: Root | null = null;
let container: HTMLElement | null = null;

function gatewayStatus(overrides: Partial<GatewayStatus> = {}): GatewayStatus {
  return {
    enabled: true,
    listening: true,
    port: 41234,
    blocked: false,
    totals: { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0, requests: 0, failures: 0 },
    sessions: [],
    pool: [],
    pool_strategy: "round_robin",
    ...overrides,
  };
}

async function flush(): Promise<void> {
  await act(async () => {
    for (let i = 0; i < 6; i++) await Promise.resolve();
  });
}

async function render(controller: Partial<SessionController>): Promise<HTMLElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  await act(async () => {
    root?.render(
      <ControllerContext.Provider value={controller as SessionController}>
        <IntegrationPanel />
      </ControllerContext.Provider>,
    );
  });
  await flush();
  return container as HTMLElement;
}

const card = (el: HTMLElement, id: string): HTMLElement =>
  el.querySelector<HTMLElement>(`[data-setting-id='${id}']`) as HTMLElement;

const buttonByText = (scope: HTMLElement, text: string): HTMLButtonElement | undefined =>
  [...scope.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent === text);

beforeEach(() => {
  useI18nStore.setState({ language: "ko" });
  pool.list.mockResolvedValue([]);
});

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
  root = null;
  container = null;
  pool.list.mockReset();
  pool.add.mockReset();
  usePreferences.setState({ ...DEFAULT_PREFERENCES });
  vi.restoreAllMocks();
});

describe("게이트웨이 카드", () => {
  it("METHOD_NOT_FOUND는 '지원 안 함'으로 접는다", async () => {
    const el = await render({
      gatewayStatus: vi.fn(async () => {
        throw new RpcClientError("METHOD_NOT_FOUND" as never, "gateway.status");
      }),
    });
    expect(card(el, "gatewayObservability").textContent).toContain(t("settings.gateway.daemonUnsupported"));
    // 구 데몬이면 키 풀도 쓸 수 없다 — 안내 + 전략 잠금.
    expect(card(el, "apiKeyPool").textContent).toContain(t("settings.keyPool.gatewayOff"));
    expect(card(el, "apiKeyPool").querySelector("select")?.disabled).toBe(true);
  });

  it("데몬 디스패처의 'unknown method' 거절도 '지원 안 함'으로 접는다", async () => {
    const el = await render({
      gatewayStatus: vi.fn(async () => {
        throw new RpcClientError("INVALID_ARGUMENT", 'unknown method "gateway.status"');
      }),
    });
    expect(card(el, "gatewayObservability").textContent).toContain(t("settings.gateway.daemonUnsupported"));
  });

  it("일시적 실패는 지원 안 함이 아니라 '확인하는 중' + 오류 문구로 보인다", async () => {
    const el = await render({
      gatewayStatus: vi.fn(async () => {
        throw new RpcClientError("DAEMON_UNAVAILABLE", "timed out");
      }),
    });
    const gateway = card(el, "gatewayObservability");
    expect(gateway.textContent).not.toContain(t("settings.gateway.daemonUnsupported"));
    expect(gateway.textContent).toContain(t("settings.gateway.checking"));
    expect(gateway.textContent).not.toContain(t("settings.usage.desktopOnly"));
    expect(gateway.textContent).toContain("timed out");
    // 첫 키 목록 조회가 끝나면 "확인하는 중"에 머물지 않는다.
    expect(card(el, "apiKeyPool").textContent).toContain(t("settings.keyPool.empty"));
  });

  it("라우팅이 꺼져 있으면 차단 버튼을 잠그고 안내와 불일치 경고를 보인다", async () => {
    usePreferences.setState({ gatewayObservability: true });
    const el = await render({ gatewayStatus: vi.fn(async () => gatewayStatus({ enabled: false, listening: false })) });
    const gateway = card(el, "gatewayObservability");
    expect(buttonByText(gateway, t("settings.gateway.block"))?.disabled).toBe(true);
    expect(gateway.textContent).toContain(t("settings.gateway.blockNeedsGateway"));
    expect(gateway.textContent).toContain(t("settings.gateway.policyMismatch"));
    const strategy = card(el, "apiKeyPool").querySelector("select");
    expect(strategy?.disabled).toBe(true);
    expect(card(el, "apiKeyPool").textContent).toContain(t("settings.keyPool.gatewayOff"));
  });

  it("스위치는 데몬이 받아들인 뒤에만 선호를 바꾸고, 실패하면 그대로 둔다", async () => {
    let current = gatewayStatus({ enabled: false, listening: false });
    const gatewaySetPolicy = vi.fn(async (params: GatewayPolicyParams) => {
      if (params.enabled === true && gatewaySetPolicy.mock.calls.length === 1) throw new Error("boom");
      current = { ...current, enabled: params.enabled ?? current.enabled, listening: params.enabled ?? current.listening };
      return { enabled: current.enabled, blocked: current.blocked, pool_strategy: current.pool_strategy };
    });
    const el = await render({ gatewayStatus: vi.fn(async () => current), gatewaySetPolicy });
    const toggle = card(el, "gatewayObservability").querySelector<HTMLInputElement>("input[role='switch']")!;

    await act(async () => { toggle.click(); });
    await flush();
    expect(usePreferences.getState().gatewayObservability).toBe(false);
    expect(card(el, "gatewayObservability").textContent).toContain("boom");

    await act(async () => { toggle.click(); });
    await flush();
    expect(usePreferences.getState().gatewayObservability).toBe(true);
    expect(card(el, "gatewayObservability").textContent).not.toContain(t("settings.gateway.policyMismatch"));
    expect(card(el, "apiKeyPool").querySelector("select")?.disabled).toBe(false);
  });

  it("정책 변경 전에 나간 늦은 폴 응답은 새 상태를 덮어쓰지 않는다", async () => {
    usePreferences.setState({ gatewayObservability: true });
    let release: ((status: GatewayStatus) => void) | null = null;
    let calls = 0;
    const gatewayStatusFn = vi.fn(() => {
      calls += 1;
      // 두 번째 조회(전략 변경 전 폴 흉내)는 붙잡아 두었다가 옛 상태로 늦게 돌려준다.
      if (calls === 2) return new Promise<GatewayStatus>((resolve) => { release = resolve; });
      return Promise.resolve(gatewayStatus({ pool_strategy: calls >= 3 ? "fill_first" : "round_robin" }));
    });
    const gatewaySetPolicy = vi.fn(async (params: GatewayPolicyParams) => ({
      enabled: true,
      blocked: false,
      pool_strategy: params.pool_strategy ?? "round_robin",
    }));
    const el = await render({ gatewayStatus: gatewayStatusFn, gatewaySetPolicy });
    // 수동 새로고침으로 두 번째(붙잡힌) 조회를 띄운다.
    await act(async () => { buttonByText(card(el, "gatewayObservability"), t("settings.gateway.refresh"))?.click(); });
    const select = card(el, "apiKeyPool").querySelector("select")!;
    await act(async () => {
      select.value = "fill_first";
      select.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await flush();
    expect(usePreferences.getState().gatewayPoolStrategy).toBe("fill_first");
    // 옛 응답이 이제야 도착한다 — 차단 상태로 바꿔 덮어쓰기 여부를 드러낸다.
    await act(async () => { release?.(gatewayStatus({ blocked: true })); });
    await flush();
    expect(card(el, "gatewayObservability").textContent).not.toContain(t("settings.gateway.blocked"));
  });
});

describe("게이트웨이 상태 폴 — 숨은 창", () => {
  let hidden = false;

  beforeEach(() => {
    hidden = false;
    Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
  });

  afterEach(() => {
    // 문서 원래의 hidden(프로토타입 getter)으로 되돌린다.
    delete (document as unknown as { hidden?: boolean }).hidden;
    vi.useRealTimers();
  });

  const setHidden = async (next: boolean): Promise<void> => {
    hidden = next;
    await act(async () => { document.dispatchEvent(new Event("visibilitychange")); });
    await flush();
  };

  it("창이 숨은 동안은 2초 폴을 건너뛰고, 다시 보이는 순간 곧바로 한 번 읽는다", async () => {
    vi.useFakeTimers();
    const gatewayStatusFn = vi.fn(async () => gatewayStatus());
    await render({ gatewayStatus: gatewayStatusFn });
    expect(gatewayStatusFn).toHaveBeenCalledTimes(1); // 마운트 직후 한 번

    await act(async () => { vi.advanceTimersByTime(2000); });
    await flush();
    expect(gatewayStatusFn).toHaveBeenCalledTimes(2); // 보이는 동안은 2초마다

    await setHidden(true);
    await act(async () => { vi.advanceTimersByTime(20_000); });
    await flush();
    expect(gatewayStatusFn).toHaveBeenCalledTimes(2); // 숨은 동안은 데몬을 깨우지 않는다

    await setHidden(false);
    expect(gatewayStatusFn).toHaveBeenCalledTimes(3); // 돌아오자마자 한 번
    await act(async () => { vi.advanceTimersByTime(2000); });
    await flush();
    expect(gatewayStatusFn).toHaveBeenCalledTimes(4); // 그리고 다시 주기대로
  });

  it("숨은 채로 열리면 첫 조회도 미루고, 보일 때 읽는다", async () => {
    vi.useFakeTimers();
    hidden = true;
    const gatewayStatusFn = vi.fn(async () => gatewayStatus());
    await render({ gatewayStatus: gatewayStatusFn });
    await act(async () => { vi.advanceTimersByTime(6000); });
    await flush();
    expect(gatewayStatusFn).not.toHaveBeenCalled();

    await setHidden(false);
    expect(gatewayStatusFn).toHaveBeenCalledTimes(1);
  });
});

describe("키 풀 카드", () => {
  it("브리지 오류 코드는 안내 문구로, 그 밖의 문구는 원문으로 보인다", async () => {
    pool.add.mockRejectedValueOnce({ code: "INVALID_ARGUMENT", message: "key_pool_full" });
    const el = await render({ gatewayStatus: vi.fn(async () => gatewayStatus()) });
    const keyPool = card(el, "apiKeyPool");
    const keyField = keyPool.querySelector<HTMLInputElement>("input[type='password']")!;
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
      setter.call(keyField, "abcdefgh-1234");
      keyField.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () => { buttonByText(keyPool, t("settings.keyPool.add"))?.click(); });
    await flush();
    expect(keyPool.textContent).toContain(t("settings.keyPool.error.full"));
    expect(keyPool.textContent).not.toContain("key_pool_full");
  });

  it("별명이 64바이트를 넘으면 추가를 잠그고 안내한다(한글은 글자당 3바이트)", async () => {
    const el = await render({ gatewayStatus: vi.fn(async () => gatewayStatus()) });
    const keyPool = card(el, "apiKeyPool");
    const labelField = keyPool.querySelector<HTMLInputElement>("input.key-pool-label")!;
    const keyField = keyPool.querySelector<HTMLInputElement>("input[type='password']")!;
    expect(labelField.maxLength).toBe(64);
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    await act(async () => {
      setter.call(keyField, "abcdefgh-1234");
      keyField.dispatchEvent(new Event("input", { bubbles: true }));
      setter.call(labelField, "가".repeat(22)); // 66바이트
      labelField.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(buttonByText(keyPool, t("settings.keyPool.add"))?.disabled).toBe(true);
    expect(keyPool.textContent).toContain(t("settings.keyPool.labelTooLong", { max: 64 }));
  });
});

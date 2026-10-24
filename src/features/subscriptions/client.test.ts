import { beforeEach, describe, expect, it, vi } from "vitest";

const calls: Array<{ command: string; args: unknown }> = [];

vi.mock("../bridge/ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../bridge/ipc")>();
  return {
    ...actual,
    isTauri: () => true,
    tauriIpcAdapter: {
      ...actual.tauriIpcAdapter,
      invoke: async (command: string, args?: unknown) => {
        calls.push({ command, args });
        return [];
      },
    },
  };
});

const { refreshSubscriptionUsage } = await import("./client");

describe("subscription usage refresh", () => {
  beforeEach(() => {
    calls.length = 0;
  });

  it("자동 갱신은 브리지 캐시를 그대로 쓴다(force 없음)", async () => {
    await refreshSubscriptionUsage();
    expect(calls).toEqual([{ command: "subscription_usage_refresh", args: undefined }]);
  });

  it("명시적 새로고침은 force로 캐시를 건너뛴다", async () => {
    await refreshSubscriptionUsage({ force: true });
    expect(calls).toEqual([{ command: "subscription_usage_refresh", args: { force: true } }]);
  });
});

import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ceilMinute, QuotaGauges } from "./SubscriptionUsageIndicator";
import { quotaGauges } from "./quotaGauges";
import { autoRefreshAllowed, SUBSCRIPTION_MIN_GAP_MS, SUBSCRIPTION_POLL_MS } from "./autoRefresh";
import type { ProviderUsage, UsageWindow } from "./types";

const quotaWindow = (id: string, remainingPercent: number, windowDurationMinutes: number | null): UsageWindow => ({
  id,
  label: id,
  usedPercent: 100 - remainingPercent,
  remainingPercent,
  windowDurationMinutes,
  resetsAt: null,
  used: null,
  limit: null,
});

const provider = (overrides: Partial<ProviderUsage> & Pick<ProviderUsage, "provider" | "displayName">): ProviderUsage => ({
  status: "ready",
  reasonCode: null,
  detail: null,
  plan: null,
  windows: [],
  observedAt: null,
  ...overrides,
});

describe("subscription quota gauges", () => {
  it("uses the Codex weekly window and other providers' shortest window, rounded and clamped to 0–100", () => {
    expect(quotaGauges([
      provider({ provider: "claude", displayName: "Claude", windows: [quotaWindow("week", 90, 10080), quotaWindow("5h", 71.6, 300)] }),
      provider({ provider: "codex", displayName: "Codex", windows: [quotaWindow("5h", 40, 300), quotaWindow("week", 104, 10080)] }),
      provider({ provider: "zai", displayName: "Z.ai", windows: [quotaWindow("5h", -2, 300)] }),
    ])).toEqual([
      { provider: "claude", name: "Claude", remaining: 72 },
      { provider: "codex", name: "Codex", remaining: 100 },
      { provider: "zai", name: "Z.ai", remaining: 0 },
    ]);
  });

  it.each([false, true])("selects Codex by seven-day duration regardless of window order (reversed: %s)", (reversed) => {
    const windows = [quotaWindow("0-primary", 80, 300), quotaWindow("0-secondary", 12.6, 10080)];
    expect(quotaGauges([
      provider({ provider: "codex", displayName: "Codex", windows: reversed ? windows.reverse() : windows }),
    ])).toEqual([{ provider: "codex", name: "Codex", remaining: 13 }]);
  });

  it("omits Codex when no seven-day window is available instead of substituting a different duration", () => {
    expect(quotaGauges([
      provider({ provider: "codex", displayName: "Codex", windows: [
        quotaWindow("5h", 80, 300),
        quotaWindow("7d", 50, null),
        quotaWindow("30d", 20, 43200),
      ] }),
    ])).toEqual([]);
  });

  it("omits providers that are not ready or have no windows", () => {
    expect(quotaGauges([
      provider({ provider: "claude", displayName: "Claude", status: "notConfigured", windows: [quotaWindow("5h", 50, 300)] }),
      provider({ provider: "codex", displayName: "Codex", windows: [] }),
    ])).toEqual([]);
  });

  it("renders a label, a bar filled to the remaining percent, and a right-aligned percent slot", () => {
    const html = renderToStaticMarkup(<QuotaGauges gauges={[{ provider: "claude", name: "Claude", remaining: 72 }]} />);
    expect(html).toBe(
      '<span class="strip-quota"><span class="strip-label">Claude</span>'
      + '<span class="strip-quota-bar" aria-hidden="true"><span class="strip-quota-fill" style="width:72%"></span></span>'
      + '<span class="strip-quota-value">72%</span></span>',
    );
  });

  it("marks a gauge low at 20% remaining or less", () => {
    const html = renderToStaticMarkup(<QuotaGauges gauges={[
      { provider: "codex", name: "Codex", remaining: 21 },
      { provider: "zai", name: "Z.ai", remaining: 20 },
    ]} />);
    expect(html).toContain('<span class="strip-quota"><span class="strip-label">Codex</span>');
    expect(html).toContain('<span class="strip-quota strip-quota-low"><span class="strip-label">Z.ai</span>');
  });
});


describe("reset time display", () => {
  it("rounds a reset deadline up to the next minute — providers roll windows at second boundaries like 16:59:59 and their dashboards show 17:00", () => {
    expect(ceilMinute(1_792_828_799)).toBe(1_792_828_800); // 2026-10-24T07:59:59Z = KST 16:59:59.99
    expect(ceilMinute(1_792_828_800)).toBe(1_792_828_800); // 이미 분 경계
  });
});

describe("Claude model-scoped quota", () => {
  it("shows Fable separately from the general Claude allowance", () => {
    const gauges = quotaGauges([provider({ provider: "claude", displayName: "Claude", windows: [
      quotaWindow("five_hour", 57, 300), quotaWindow("seven_day", 43, 10080),
      quotaWindow("model-scoped:Fable", 13, 10080),
    ] })]);
    expect(gauges).toEqual([
      { provider: "claude", name: "Claude", remaining: 57 },
      { provider: "claude", name: "Fable", remaining: 13 },
    ]);
    const html = renderToStaticMarkup(<QuotaGauges gauges={gauges} />);
    expect(html).toContain('strip-quota-low"><span class="strip-label">Fable');
    expect(html).toContain('width:13%');
  });

  it("never substitutes Fable for the general allowance or invents a missing Fable quota", () => {
    expect(quotaGauges([provider({ provider: "claude", displayName: "Claude", windows: [
      quotaWindow("model-scoped:Fable", 13, 10080),
    ] })])).toEqual([{ provider: "claude", name: "Fable", remaining: 13 }]);
    expect(quotaGauges([provider({ provider: "claude", displayName: "Claude", windows: [
      quotaWindow("five_hour", 57, 300),
    ] })])).toEqual([{ provider: "claude", name: "Claude", remaining: 57 }]);
  });
});

describe("구독 사용량 자동 갱신 판단(autoRefreshAllowed)", () => {
  const base = { now: 1_000_000, lastRefreshAt: null, hidden: false, pressure: "NORMAL" as const, minGapMs: 0 };

  it("자동 주기는 5분, 최소 간격은 1분이다(CLI 두 개를 매 분 띄우지 않는다)", () => {
    expect(SUBSCRIPTION_POLL_MS).toBe(5 * 60_000);
    expect(SUBSCRIPTION_MIN_GAP_MS).toBe(60_000);
  });

  it("보이는 창 + NORMAL이면 첫 갱신을 한다 — 압력을 아직 모르면(null) 막지 않는다", () => {
    expect(autoRefreshAllowed(base)).toBe(true);
    expect(autoRefreshAllowed({ ...base, pressure: null })).toBe(true);
  });

  it("창이 숨어 있으면 어떤 자동 갱신도 하지 않는다", () => {
    expect(autoRefreshAllowed({ ...base, hidden: true })).toBe(false);
    expect(autoRefreshAllowed({ ...base, hidden: true, pressure: null })).toBe(false);
  });

  it("메모리 압력이 NORMAL이 아니면 자동 갱신을 건너뛴다", () => {
    expect(autoRefreshAllowed({ ...base, pressure: "WARNING" })).toBe(false);
    expect(autoRefreshAllowed({ ...base, pressure: "CRITICAL" })).toBe(false);
  });

  it("창 복귀·주기 틱은 마지막 갱신(수동 포함) 뒤 1분이 지나야 한다", () => {
    const last = base.now - 30_000;
    expect(autoRefreshAllowed({ ...base, lastRefreshAt: last, minGapMs: SUBSCRIPTION_MIN_GAP_MS })).toBe(false);
    expect(autoRefreshAllowed({
      ...base,
      lastRefreshAt: base.now - SUBSCRIPTION_MIN_GAP_MS,
      minGapMs: SUBSCRIPTION_MIN_GAP_MS,
    })).toBe(true);
    // 간격 0(첫 갱신)은 직전 갱신이 있어도 막지 않는다.
    expect(autoRefreshAllowed({ ...base, lastRefreshAt: base.now, minGapMs: 0 })).toBe(true);
  });
});

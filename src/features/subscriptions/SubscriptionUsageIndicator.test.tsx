import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { QuotaGauges } from "./SubscriptionUsageIndicator";
import { quotaGauges } from "./quotaGauges";
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

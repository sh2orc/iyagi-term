import type { ProviderUsage, UsageWindow } from "./types";

const WEEK_MINUTES = 7 * 24 * 60;

export interface QuotaGauge {
  provider: ProviderUsage["provider"];
  name: string;
  /** 표시 대상 창의 잔여율(반올림, 0–100으로 제한). */
  remaining: number;
}

/** Codex는 7일 창, 나머지 공급자는 가장 짧은 창의 잔여율로 요약한다. */
export function quotaGauges(providers: ProviderUsage[]): QuotaGauge[] {
  return providers.flatMap((provider) => {
    if (provider.status !== "ready") return [];
    // 주간 데이터가 없을 때 단기 사용량을 7일 기준으로 대신 표시하지 않는다.
    const window = provider.provider === "codex"
      ? provider.windows.find((window) => window.windowDurationMinutes === WEEK_MINUTES)
      : primaryWindow(provider.windows.filter((window) => !window.id.startsWith("model-scoped:")));
    const gauges: QuotaGauge[] = window
      ? [{ provider: provider.provider, name: provider.displayName, remaining: Math.min(100, Math.max(0, Math.round(window.remainingPercent))) }]
      : [];
    if (provider.provider === "claude") {
      for (const scoped of provider.windows.filter((window) => window.id.startsWith("model-scoped:"))) {
        gauges.push({ provider: "claude", name: scoped.id.slice("model-scoped:".length),
          remaining: Math.min(100, Math.max(0, Math.round(scoped.remainingPercent))) });
      }
    }
    return gauges;
  });
}

function primaryWindow(windows: UsageWindow[]): UsageWindow | null {
  return [...windows].sort((a, b) => (a.windowDurationMinutes ?? Number.MAX_SAFE_INTEGER) - (b.windowDurationMinutes ?? Number.MAX_SAFE_INTEGER))[0] ?? null;
}

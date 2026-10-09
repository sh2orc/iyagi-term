export type ProviderId = "codex" | "claude" | "zai";
export type ProviderStatus = "ready" | "unavailable" | "notConfigured" | "error";

export interface UsageWindow {
  id: string;
  label: string;
  usedPercent: number;
  remainingPercent: number;
  windowDurationMinutes: number | null;
  resetsAt: number | null;
  used: number | null;
  limit: number | null;
}

export interface ProviderUsage {
  provider: ProviderId;
  displayName: string;
  status: ProviderStatus;
  reasonCode: string | null;
  detail: string | null;
  plan: string | null;
  windows: UsageWindow[];
  observedAt: number | null;
}

export interface ClaudeUsageStatus {
  path: string;
  active: boolean;
  proposed: string | null;
  command: string;
}

export interface ApiKeyStatus {
  configured: boolean;
}

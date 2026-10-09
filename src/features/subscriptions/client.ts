import { isTauri, tauriIpcAdapter } from "../bridge/ipc";
import type { ApiKeyStatus, ClaudeUsageStatus, ProviderUsage } from "./types";

export const SUBSCRIPTION_REFRESH_EVENT = "iyagi-subscription-refresh";

export async function refreshSubscriptionUsage(): Promise<ProviderUsage[]> {
  if (!isTauri()) return [];
  return tauriIpcAdapter.invoke<ProviderUsage[]>("subscription_usage_refresh");
}

export function refreshUsageSoon(): void {
  window.dispatchEvent(new Event(SUBSCRIPTION_REFRESH_EVENT));
}

export function getZaiKeyStatus(): Promise<ApiKeyStatus> {
  return tauriIpcAdapter.invoke<ApiKeyStatus>("zai_api_key_status");
}

export function saveZaiKey(apiKey: string): Promise<ApiKeyStatus> {
  return tauriIpcAdapter.invoke<ApiKeyStatus>("zai_api_key_set", { apiKey });
}

export function removeZaiKey(): Promise<ApiKeyStatus> {
  return tauriIpcAdapter.invoke<ApiKeyStatus>("zai_api_key_remove");
}

export function getClaudeUsageStatus(): Promise<ClaudeUsageStatus> {
  return tauriIpcAdapter.invoke<ClaudeUsageStatus>("claude_usage_status");
}

export function applyClaudeUsage(): Promise<ClaudeUsageStatus> {
  return tauriIpcAdapter.invoke<ClaudeUsageStatus>("claude_usage_apply");
}

export function removeClaudeUsage(): Promise<ClaudeUsageStatus> {
  return tauriIpcAdapter.invoke<ClaudeUsageStatus>("claude_usage_remove");
}

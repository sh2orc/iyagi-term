import { isTauri, tauriIpcAdapter } from "../bridge/ipc";
import type { ApiKeyStatus, ClaudeUsageStatus, ProviderUsage } from "./types";

export const SUBSCRIPTION_REFRESH_EVENT = "iyagi-subscription-refresh";

/**
 * 구독 사용량을 다시 읽는다. CLI로 읽는 행은 브리지가 짧게 캐시한다 —
 * `force`(사용자가 명시적으로 누른 새로고침, 설정 변경 직후)는 캐시를 건너뛴다.
 * 자동 주기 갱신은 캐시를 그대로 쓴다.
 */
export async function refreshSubscriptionUsage(options: { force?: boolean } = {}): Promise<ProviderUsage[]> {
  if (!isTauri()) return [];
  return options.force
    ? tauriIpcAdapter.invoke<ProviderUsage[]>("subscription_usage_refresh", { force: true })
    : tauriIpcAdapter.invoke<ProviderUsage[]>("subscription_usage_refresh");
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

/** 풀 키 요약(비밀 없음) — 09-key-pool-rotation. */
export interface PoolKeyView {
  id: string;
  label: string;
  created_epoch_ms: number;
}

export function listZaiKeyPool(): Promise<PoolKeyView[]> {
  return tauriIpcAdapter.invoke<PoolKeyView[]>("zai_key_pool_list");
}

export function addZaiKeyPoolKey(label: string, apiKey: string): Promise<PoolKeyView[]> {
  return tauriIpcAdapter.invoke<PoolKeyView[]>("zai_key_pool_add", { label, apiKey });
}

export function removeZaiKeyPoolKey(id: string): Promise<PoolKeyView[]> {
  return tauriIpcAdapter.invoke<PoolKeyView[]>("zai_key_pool_remove", { id });
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

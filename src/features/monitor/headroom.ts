/**
 * host 메모리 여유(headroom) 계산 — 상태 스트립과 대기열 drawer가 같은 값을
 * 보여 주도록 한곳에 둔다(03-resources §2 host 예비: max(2 GiB, 15%)).
 */

import type { HostSample } from "../../generated/HostSample";
import { parseU64 } from "./format";

export const HOST_RESERVE_MIN_BYTES = 2147483648;
export const HOST_RESERVE_PERCENT = 0.15;

/** host 예비 바이트 — 총량을 모르면 null. */
export function hostReserveBytes(totalBytes: number | null): number | null {
  if (totalBytes === null) return null;
  return Math.max(HOST_RESERVE_MIN_BYTES, Math.round(totalBytes * HOST_RESERVE_PERCENT));
}

/** 예비를 뺀 안전 여유(0 하한) — 측정값이 없으면 null(0으로 대체하지 않는다). */
export function safeAvailableBytes(host: HostSample | null): number | null {
  const total = parseU64(host?.physical_total_bytes.value ?? null);
  const available = parseU64(host?.physical_available_bytes.value ?? null);
  const reserve = hostReserveBytes(total);
  return available !== null && reserve !== null ? Math.max(0, available - reserve) : null;
}

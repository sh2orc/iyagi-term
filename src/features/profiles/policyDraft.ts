/**
 * 정책 편집 초안(draft) 변환 — UI는 GiB/정수로 편집하고 저장/실행 시
 * 계약 형식(ProfilePolicy: U64String 문자열 / LaunchPolicy)으로 바꾼다.
 * 기본값은 03 §8(observe + 2 GiB + cpu_slots 1 + cap 전부 null).
 */

import type { Enforcement } from "../../generated/Enforcement";
import type { ProfilePolicy } from "./types";
import { DEFAULT_RESERVATION_BYTES } from "./types";

/** UI가 편집하는 숫자 정책 상태(바이트). */
export interface NumericPolicy {
  enforcement: Enforcement;
  reservationBytes: number;
  cpuSlots: number;
  memoryMaxBytes: number | null;
  cpuMaxCores: number | null;
  pidsMax: number | null;
}

/** GiB 단위 편집 초안. */
export interface PolicyDraft {
  enforcement: Enforcement;
  reservationGiB: number;
  cpuSlots: number;
  memoryMaxGiB: number | null;
  cpuMaxCores: number | null;
  pidsMax: number | null;
}

/** 예약 하한 256 MiB — UI stepper 최소값. */
export const MIN_RESERVATION_GIB = 0.25;

export function gibToBytes(gib: number): number {
  return Math.round(gib * 1024 ** 3);
}

export function bytesToGiB(bytes: number): number {
  return Math.round((bytes / 1024 ** 3) * 100) / 100;
}

export function defaultPolicyDraft(): PolicyDraft {
  return {
    enforcement: "observe",
    reservationGiB: 2,
    cpuSlots: 1,
    memoryMaxGiB: null,
    cpuMaxCores: null,
    pidsMax: null,
  };
}

export function fromProfilePolicy(policy: ProfilePolicy | null): PolicyDraft {
  if (!policy) return defaultPolicyDraft();
  const reservation = Number(policy.reservation_bytes);
  const memory = policy.memory_max_bytes === null ? null : Number(policy.memory_max_bytes);
  return {
    enforcement: policy.enforcement,
    reservationGiB: Number.isFinite(reservation) && reservation > 0 ? bytesToGiB(reservation) : 2,
    cpuSlots: policy.cpu_slots,
    memoryMaxGiB: memory !== null && Number.isFinite(memory) && memory > 0 ? bytesToGiB(memory) : null,
    cpuMaxCores: policy.cpu_max_cores,
    pidsMax: policy.pids_max,
  };
}

export function draftToNumericPolicy(draft: PolicyDraft): NumericPolicy {
  return {
    enforcement: draft.enforcement,
    reservationBytes: gibToBytes(draft.reservationGiB),
    cpuSlots: draft.cpuSlots,
    memoryMaxBytes: draft.memoryMaxGiB === null ? null : gibToBytes(draft.memoryMaxGiB),
    cpuMaxCores: draft.cpuMaxCores,
    pidsMax: draft.pidsMax,
  };
}

export function draftToProfilePolicy(draft: PolicyDraft): ProfilePolicy {
  const numeric = draftToNumericPolicy(draft);
  return {
    enforcement: numeric.enforcement,
    reservation_bytes: String(numeric.reservationBytes || Number(DEFAULT_RESERVATION_BYTES)),
    cpu_slots: numeric.cpuSlots,
    memory_max_bytes: numeric.memoryMaxBytes === null ? null : String(numeric.memoryMaxBytes),
    cpu_max_cores: numeric.cpuMaxCores,
    pids_max: numeric.pidsMax,
  };
}

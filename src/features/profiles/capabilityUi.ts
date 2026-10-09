/**
 * 자원 정책 ↔ 실제 capability 대응 (03 §4·§8, 04 §7).
 *
 * 정책 편집기는 snapshot.capabilities의 실제 지원 상태를 그대로 보여 준다:
 * - prefer/require에서 어떤 상한이 미지원/권한 필요인지 이유와 함께 표시.
 * - require + 미지원(또는 권한 필요) 상한 요청은 실행 전에 차단하고
 *   CAPABILITY_UNAVAILABLE 사유를 설명한다(daemon은 이 코드로 거부한다).
 * - 상한(cap)은 "안전장치"다(03 §1) — 예약(reservation)이 기본 효과.
 */

import type { Capabilities } from "../../generated/Capabilities";
import type { Enforcement } from "../../generated/Enforcement";
import type { LimitCapability } from "../../generated/LimitCapability";
import type { LimitSupport } from "../../generated/LimitSupport";
import { t } from "../../i18n";

export type LimitKey = "memory_max_bytes" | "cpu_max_cores" | "pids_max";

/** 상한 라벨 — 접근 시점에 t()로 평가한다(모듈 로드 시 평가 금지). */
export const LIMIT_LABELS: Record<LimitKey, string> = {
  get memory_max_bytes(): string {
    return t("caps.limit.memory_max_bytes");
  },
  get cpu_max_cores(): string {
    return t("caps.limit.cpu_max_cores");
  },
  get pids_max(): string {
    return t("caps.limit.pids_max");
  },
};

/** LaunchPolicy의 cap 3종 → Capabilities 키 연결. */
export function limitCapabilityOf(capabilities: Capabilities, key: LimitKey): LimitCapability {
  switch (key) {
    case "memory_max_bytes":
      return capabilities.memory_limit_kind;
    case "cpu_max_cores":
      return capabilities.cpu_quota;
    case "pids_max":
      return capabilities.process_count_limit;
  }
}

export function limitSupportText(support: LimitSupport): string {
  switch (support) {
    case "supported":
      return t("caps.support.supported");
    case "unsupported":
      return t("caps.support.unsupported");
    case "permission_required":
      return t("caps.support.permission_required");
  }
}

/** 라벨/hint는 접근 시점에 t()로 평가한다(모듈 로드 시 평가 금지). */
export const ENFORCEMENT_OPTIONS: ReadonlyArray<{ value: Enforcement; label: string; hint: string }> = [
  {
    value: "observe",
    get label(): string {
      return t("policy.enforcement.observe.label");
    },
    get hint(): string {
      return t("policy.enforcement.observe.hint");
    },
  },
  {
    value: "prefer",
    get label(): string {
      return t("policy.enforcement.prefer.label");
    },
    get hint(): string {
      return t("policy.enforcement.prefer.hint");
    },
  },
  {
    value: "require",
    get label(): string {
      return t("policy.enforcement.require.label");
    },
    get hint(): string {
      return t("policy.enforcement.require.hint");
    },
  },
];

/** UI 숫자 정책(NumericPolicy)과 호환되는 상한 요청 형태. */
export interface PolicyCapsLike {
  enforcement: Enforcement;
  memoryMaxBytes: number | string | null;
  cpuMaxCores: number | null;
  pidsMax: number | null;
}

function isRequested(value: string | number | null | undefined): boolean {
  if (value === null || value === undefined) return false;
  if (typeof value === "string") return value.trim() !== "";
  return true;
}

export interface LimitReality {
  key: LimitKey;
  label: string;
  /** 사용자가 이 상한을 요청했는가(값 설정 여부). */
  requested: boolean;
  support: LimitSupport;
  reason: string | null;
  supportText: string;
  /** require에서 적용 불가 → 실행 차단. */
  blocking: boolean;
  /** 차단/경고 문구(화면 표시용). */
  notice: string | null;
}

/**
 * 각 상한의 실제 적용 현실. policy 값 + snapshot capabilities에서 생성.
 */
export function limitRealities(policy: PolicyCapsLike, capabilities: Capabilities): LimitReality[] {
  const entries: Array<{ key: LimitKey; requested: boolean }> = [
    { key: "memory_max_bytes", requested: isRequested(policy.memoryMaxBytes) },
    { key: "cpu_max_cores", requested: isRequested(policy.cpuMaxCores) },
    { key: "pids_max", requested: isRequested(policy.pidsMax) },
  ];
  return entries.map(({ key, requested }) => {
    const cap = limitCapabilityOf(capabilities, key);
    const support = cap.support;
    const reason = cap.reason ?? null;
    const label = LIMIT_LABELS[key];
    const supportText = limitSupportText(support);
    let blocking = false;
    let notice: string | null = null;
    if (requested) {
      const reasonText = reason ? t("caps.notice.reason", { reason }) : "";
      if (policy.enforcement === "require" && support !== "supported") {
        blocking = true;
        notice = t("caps.notice.require", { support: supportText, label }) + reasonText;
      } else if (policy.enforcement === "prefer" && support !== "supported") {
        notice = t("caps.notice.prefer", { label }) + reasonText;
      } else if (policy.enforcement === "observe") {
        // 관측만 모드에서는 상한을 적용하지 않는다(03 §1·§8).
        notice = t("caps.notice.observe", { label, support: supportText }) + reasonText;
      }
    }
    return { key, label, requested, support, reason, supportText, blocking, notice };
  });
}

export interface CapabilityGate {
  blocked: boolean;
  /** 차단 설명(CAPABILITY_UNAVAILABLE 안내). null when not blocked. */
  message: string | null;
  blockedLimits: LimitReality[];
}

/**
 * 실행 직전 capability 게이트. require + 요청 상한 미적용이면 차단한다.
 * 오직 실제 capabilities 데이터에서만 판정한다.
 */
export function capabilityGate(policy: PolicyCapsLike, capabilities: Capabilities): CapabilityGate {
  const realities = limitRealities(policy, capabilities);
  const blockedLimits = realities.filter((r) => r.blocking);
  if (blockedLimits.length === 0) return { blocked: false, message: null, blockedLimits: [] };
  const list = blockedLimits
    .map((r) => t("caps.gate.item", { label: r.label, support: r.supportText }) + (r.reason ? ` — ${r.reason}` : ""))
    .join("\n");
  return {
    blocked: true,
    message: [t("caps.gate.line1"), list, t("caps.gate.hint")].join("\n"),
    blockedLimits,
  };
}

/**
 * 상한 섹션 표지 — 03 §1: 상한은 안전장치이며 넘으면 성능 저하/OOM이 될 수 있다.
 * 값은 i18n 키(표시 시점에 t()로 변환 — 모듈 로드 시 평가 금지).
 */
export const CAPS_SECTION_LABEL = "caps.section.label";
export const CAPS_SECTION_NOTE = "caps.section.note";

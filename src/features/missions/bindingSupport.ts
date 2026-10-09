import type { Binding } from "../../generated/Binding";
import type { LocalEvidence } from "../../generated/LocalEvidence";
import type { Role } from "../../generated/Role";
import type { Support } from "../../generated/Support";
import type { TaskKind } from "../../generated/TaskKind";

const roleKinds: Record<Role, TaskKind> = {
  lead: "plan", researcher: "research", architect: "design", builder: "implement",
  test_author: "test_author", reviewer: "review", specialist: "consult",
  diagnostician: "diagnose", integrator: "integrate", documenter: "document",
};
/** 역할이 맡는 할 일 종류 — 역할만 손에 있는 화면이 필수 기능 집합을 고를 때 쓴다. */
export function roleTaskKind(role: Role): TaskKind {
  return roleKinds[role];
}
/** term_core::mission::capability::writes_workspace 사본. */
function writesWorkspace(kind: TaskKind): boolean {
  return ["implement", "test_author", "integrate", "document"].includes(kind);
}
export function bindingSupportsTask(binding: Binding, kind: TaskKind): boolean {
  if (!binding.enabled) return false;
  if (kind === "verify" || binding.runtime === "fake") return true;
  const c = binding.capabilities;
  return c.structured_result.supported && c.events.supported && c.cancel.supported
    && (writesWorkspace(kind) ? c.scoped_write.supported : c.read_only.supported);
}
export function bindingSupportsRole(binding: Binding, role: Role): boolean {
  return bindingSupportsTask(binding, roleKinds[role]);
}
export function capabilityBlocked(code: string | null): boolean {
  return code?.startsWith("capability_") ?? false;
}

/** registry가 실험적 동의로 연 기능에 붙이는 Support.reason_code(11 §3.1). */
export const EXPERIMENTAL_OPT_IN_REASON = "experimental_opt_in";
/** 출시 증거가 증명한 층 — 정확한 버전(null)과 같은 버전 라인(version_line). */
const SHIPPED_REASONS: Array<string | null> = [null, "version_line"];
/** 이 PC에서 측정·관측한 층(11 §3.1). */
const LOCAL_REASONS: Array<string | null> = ["local_probe", "observed_runs"];

/**
 * 판정이 보는 **필수 기능**(11 §8): structured_result·events·cancel과 접근 모드 하나
 * (쓰기 할 일이면 scoped_write, 아니면 read_only). steer/resume 같은 선택 기능은 보지 않는다.
 *
 * 할 일 종류가 없으면 접근 모드를 고를 수 없으므로 열린 쪽(supported)을 모두 본다 —
 * 어느 모드의 필수 집합에든 실험적 동의가 끼어 있으면 실험적 연결이다. 둘 다 닫혀 있으면
 * 둘 다 넣어 "필수 기능이 모두 supported"가 성립하지 않게 한다.
 */
function requiredSupports(capabilities: Binding["capabilities"], kind?: TaskKind): Support[] {
  const base = [capabilities.structured_result, capabilities.events, capabilities.cancel];
  if (kind) return [...base, writesWorkspace(kind) ? capabilities.scoped_write : capabilities.read_only];
  const open = [capabilities.read_only, capabilities.scoped_write].filter((support) => support?.supported);
  return [...base, ...(open.length > 0 ? open : [capabilities.read_only, capabilities.scoped_write])];
}

/**
 * 사용자 동의로 열린 필수 기능이 있는가(11 §8) — 결과·할 일·상세의 `실험적 연결` 배지가 쓴다.
 * 선택 기능이 실험적이라는 이유로는 배지를 붙이지 않는다. 역할 가능 여부
 * (bindingSupportsRole)는 reason_code가 아니라 `supported`만 본다.
 */
export function isExperimentalBinding(
  binding: Pick<Binding, "capabilities"> | null | undefined,
  kind?: TaskKind,
): boolean {
  if (!binding) return false;
  return requiredSupports(binding.capabilities, kind).some((support) => support?.reason_code === EXPERIMENTAL_OPT_IN_REASON);
}

/** 연결의 신뢰 칩 하나(11 §8) — 필수 기능이 어느 증거 층으로 열렸는지. */
export type BindingTrust = "shipped" | "local" | "experimental" | "unverified";

/**
 * - `shipped`: 필수 기능이 모두 출시 증거(reason `null`/`version_line`)로 열렸다.
 * - `local`: 모두 열렸고 그중 하나 이상이 이 PC의 증거(`local_probe`/`observed_runs`)이며 동의는 끼지 않았다.
 * - `experimental`: 필수 기능 중 하나라도 사용자 동의로 열렸다.
 * - `unverified`: 그 밖(닫힌 필수 기능이 있거나 알 수 없는 사유).
 */
export function bindingTrust(binding: Pick<Binding, "capabilities"> | null | undefined, kind?: TaskKind): BindingTrust {
  if (!binding) return "unverified";
  const supports = requiredSupports(binding.capabilities, kind);
  if (supports.some((support) => support?.reason_code === EXPERIMENTAL_OPT_IN_REASON)) return "experimental";
  if (!supports.every((support) => support?.supported)) return "unverified";
  if (supports.every((support) => SHIPPED_REASONS.includes(support.reason_code))) return "shipped";
  if (supports.some((support) => LOCAL_REASONS.includes(support.reason_code))) return "local";
  return "unverified";
}

/** 신뢰 칩 아래 한 줄로 보여 줄 근거(11 §8) — 없는 조각은 호출자가 뺀다. */
export interface LocalEvidenceSummary {
  /** 이 버전에서 자가 진단을 실행했는가(false면 protocolOk는 "안 함"이지 "실패"가 아니다). */
  probed: boolean;
  /** 어댑터가 쓰는 프로토콜·플래그를 이 설치본이 갖췄는가. */
  protocolOk: boolean;
  /** OS 경계 사례 통과/전체(진단하지 않은 OS·런타임이면 null). */
  sandboxPassed: number | null;
  sandboxTotal: number | null;
  /** 이 PC·이 버전·이 모델에서 성공한 실행 수(읽기 전용 + 쓰기). */
  runs: number;
  probedAt: string | null;
  /** 런타임 자신의 모델 목록에 이 연결의 모델이 있었는가(목록이 없으면 null). */
  modelListed: boolean | null;
}

/**
 * 데몬이 이 PC에서 측정한 증거(11 §3.3). 보여 줄 조각이 하나도 없으면(자가 진단도,
 * 성공 실행도 없으면) null — 빈 줄을 만들지 않는다.
 */
export function localEvidenceSummary(
  binding: { local_evidence?: LocalEvidence | null } | null | undefined,
): LocalEvidenceSummary | null {
  const evidence = binding?.local_evidence ?? null;
  if (!evidence) return null;
  const probe = evidence.probe ?? null;
  const runs = (evidence.runs?.succeeded_read_only ?? 0) + (evidence.runs?.succeeded_write ?? 0);
  if (!probe && runs === 0) return null;
  return {
    probed: probe !== null,
    protocolOk: probe?.protocol_ok ?? false,
    sandboxPassed: probe?.sandbox_cases_passed ?? null,
    sandboxTotal: probe?.sandbox_cases_total ?? null,
    runs,
    probedAt: evidence.probed_at ?? null,
    modelListed: probe?.model_listed ?? null,
  };
}

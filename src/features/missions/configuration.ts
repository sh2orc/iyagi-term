import type { Binding } from "../../generated/Binding";
import type { Policy } from "../../generated/Policy";
import type { Role } from "../../generated/Role";
import type { RuntimeKind } from "../../generated/RuntimeKind";
import defaults from "../../../docs/orchestration/defaults.json";
import { newRequestId } from "./viewUtils";

/** Daemon policy ceiling for one run's time limit; larger mission policies are rejected. */
export const RUN_TIME_LIMIT_CEILING_MS: number = defaults.policy_ceiling.run_time_limit_ms;

export function missionPolicy(bindingIds: string[] = []): Policy {
  return { max_parallel_runs: defaults.max_parallel_runs_per_mission, max_attempts_per_task: defaults.max_attempts_per_task,
    max_repair_cycles: defaults.max_repair_cycles, max_automatic_starts: defaults.max_automatic_mission_starts,
    active_time_limit_ms: String(defaults.mission_active_time_limit_ms), run_time_limit_ms: String(defaults.run_time_limit_ms),
    max_cost_usd_micros: null, unknown_cost: "allow_with_notice", allow_network: false, allow_automatic_plan_apply: true,
    allow_recovery_of_unsent: true, allowed_binding_ids: bindingIds, allowed_roles: ["lead", "builder", "reviewer", "integrator"],
    allowed_verification_ids: [], require_independent_review: true, require_enforced_verification: false };
}
/** 리뷰를 포함한 팀의 역할(빠른 설정·설정의 팀 저장이 만드는 네 역할). */
export const REVIEWED_TEAM_ROLES: readonly Role[] = ["lead", "builder", "reviewer", "integrator"];
/** Value of the "enter manually" option in model selects — a mode switch, never a model id. */
export const CUSTOM_MODEL_OPTION = "__manual__";

/**
 * 작업을 시작하기 전에 자동 실행이 가능한 연결을 확인할 역할(계약 B · 데몬 start 검사와 같다).
 * 리뷰 포함이면 lead·builder·reviewer, 리뷰 생략이면 lead·builder가 필수다.
 * integrator는 선택 역할이라 팀에 있을 때만 기능을 검사한다 — 없으면 통합 충돌 때
 * "Integrator에게 해결 요청" 선택지가 없을 뿐 시작은 막지 않는다.
 */
export function requiredMissionRoles(requireReview: boolean, teamRoles: readonly Role[]): Role[] {
  const roles: Role[] = requireReview ? ["lead", "builder", "reviewer"] : ["lead", "builder"];
  if (teamRoles.includes("integrator")) roles.push("integrator");
  return roles;
}

/**
 * 리뷰 포함/생략을 정책에 반영한다(계약 B). 생략이면 독립 리뷰를 끄고 허용 역할에서
 * reviewer를 뺀다 — 역할 연결에서 reviewer를 빼는 일은 missionRoleBindings가 맡는다.
 */
export function policyWithReviewMode(policy: Policy, requireReview: boolean): Policy {
  if (requireReview) return { ...policy, require_independent_review: true };
  return { ...policy, require_independent_review: false, allowed_roles: policy.allowed_roles.filter((role) => role !== "reviewer") };
}

/** 리뷰 생략이면 reviewer 역할 연결을 뺀다. */
export function missionRoleBindings<T extends { role: Role }>(bindings: readonly T[], requireReview: boolean): T[] {
  return requireReview ? [...bindings] : bindings.filter((binding) => binding.role !== "reviewer");
}

export function newBinding(runtime: RuntimeKind = "codex"): Binding {
  const unknown = { supported: false, reason_code: "not_checked" };
  return { id: newRequestId(), revision: "0", label: "", runtime, program: "", runtime_version: null, experimental_version: null, local_evidence: null,
    provider_id: runtime === "claude" ? "anthropic" : "openai", model_id: "", effort: null, auth_route: runtime === "opencode" ? "api_key" : "subscription",
    credential_ref: null, endpoint_ref: null, checked_at: null, enabled: true, estimated_run_cost_usd_micros: null,
    resource_policy: { reservation_bytes: "2147483648", cpu_slots: 1, enforcement: "observe", memory_max_bytes: null, cpu_max_cores: null, pids_max: null },
    capabilities: { structured_result: unknown, events: unknown, cancel: unknown, resume: unknown, steer: unknown, approval_reply: unknown,
      read_only: unknown, scoped_write: unknown, model_listing: unknown, usage: unknown, native_terminal_attach: unknown } };
}
/** Reject accidental key pastes before a binding crosses RPC. */
export function validConnectionRefs(binding: Pick<Binding, "credential_ref" | "endpoint_ref">): boolean {
  const uuid="[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}";
  return (binding.credential_ref===null || new RegExp(`^keyring:${uuid}$`).test(binding.credential_ref))
    && (binding.endpoint_ref===null || new RegExp(`^${uuid}$`).test(binding.endpoint_ref));
}
/**
 * Verification arguments as argv, never shell text: a JSON array of strings, or
 * whitespace-separated words where '…' and "…" group literal text (no escapes,
 * so Windows paths keep their backslashes). Unbalanced quotes are rejected.
 *
 * Input starting with `[` is JSON only when it parses (then it must be an array of
 * strings) or clearly tries to be a JSON array (`["…` or `[]`-like) — anything else,
 * such as `[slow] tests`, is split on whitespace like plain text.
 */
export function parseCommandArgs(text: string): string[] {
  const source=text.trim();
  if (!source) return [];
  if (source.startsWith("[")) {
    let parsed: unknown;
    let isJson=true;
    try { parsed=JSON.parse(source); } catch { isJson=false; }
    if (isJson) {
      if (!Array.isArray(parsed) || parsed.some(arg=>typeof arg!=="string")) throw new Error("invalid_args");
      return parsed as string[];
    }
    // A broken JSON array (e.g. `["test",`) is a typo, not literal words.
    if (/^\[\s*["\[\]]/.test(source)) throw new Error("invalid_args");
  }
  const args: string[]=[];
  let current="", started=false, quote: string | null=null;
  for (const char of source) {
    if (quote!==null) {
      if (char===quote) quote=null; else current+=char;
      continue;
    }
    if (char==="'" || char==='"') { quote=char; started=true; continue; }
    if (/\s/.test(char)) {
      if (started) { args.push(current); current=""; started=false; }
      continue;
    }
    current+=char; started=true;
  }
  if (quote!==null) throw new Error("invalid_args");
  if (started) args.push(current);
  return args;
}
/** Parse dollars without floating-point rounding or accepting NaN/negative caps. */
export function dollarsToMicros(text: string): string | null {
  if (!text.trim()) return null;
  const match=/^(\d+)(?:\.(\d{1,6}))?$/.exec(text.trim());
  if (!match) throw new Error("invalid_cost");
  const value=BigInt(match[1])*1_000_000n+BigInt((match[2] ?? "").padEnd(6,"0"));
  if (value<=0n || value>9_223_372_036_854_775_807n) throw new Error("invalid_cost");
  return value.toString();
}

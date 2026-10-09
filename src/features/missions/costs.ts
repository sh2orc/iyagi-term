import type { Run } from "../../generated/Run";
import type { Decision } from "../../generated/Decision";

export function isCostDecision(decision: Decision | null): boolean {
  return decision?.kind === "budget" && decision.options.some(o=>o.id === "stop_cost_mission");
}

export function microsToDollars(micros: bigint): string {
  return `${micros / 1_000_000n}.${String(micros % 1_000_000n).padStart(6, "0")}`;
}

/** Same reservation rule as the daemon: each immutable Run is counted once. */
export function summarizeUsage(runs: readonly Run[]) {
  let observed = 0n, estimated = 0n, unknownCost = 0, reportedRuns = 0;
  let input = 0n, output = 0n, unknownInput = 0, unknownOutput = 0;
  for (const run of runs) {
    if (!run.binding_snapshot) continue;
    const owned = !["succeeded", "failed", "cancelled"].includes(run.state);
    if (run.dispatch_state === "unsent" && !owned) continue;
    if (run.usage.input_tokens === null) unknownInput++; else input += BigInt(run.usage.input_tokens);
    if (run.usage.output_tokens === null) unknownOutput++; else output += BigInt(run.usage.output_tokens);
    const reported = run.usage.cost_usd_micros !== null && run.usage.cost_source !== "unknown"
      ? BigInt(run.usage.cost_usd_micros) : null;
    const reservation = run.binding_snapshot.estimated_run_cost_usd_micros != null
      ? BigInt(run.binding_snapshot.estimated_run_cost_usd_micros) : null;
    if (reported !== null) {
      if (run.usage.cost_source === "provider") { observed += reported; reportedRuns++; } else estimated += reported;
    }
    if (owned || reported === null) {
      if (reservation === null) unknownCost++;
      else if (reservation > (reported ?? 0n)) estimated += reservation - (reported ?? 0n);
    }
  }
  return { observed, estimated, unknownCost, reportedRuns, input, output, unknownInput, unknownOutput };
}

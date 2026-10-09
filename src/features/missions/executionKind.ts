import type { Task } from "../../generated/Task";
import type { Mission } from "../../generated/Mission";

export function taskRunsInPhase(task: Task, mission: Mission): boolean {
  if (isInternalIntegration(task)) return mission.phase === "integrating";
  if (task.kind === "verify") return mission.phase === "validating" && mission.candidate_id !== null;
  if (task.kind === "review") return mission.phase === "reviewing" && mission.candidate_id !== null;
  if (task.kind === "plan") return ["planning", "implementing"].includes(mission.phase);
  return mission.phase === "implementing";
}

export function isInternalIntegration(task: Task): boolean {
  return task.kind === "integrate" && (task.integration != null || (task.role === null && task.binding_id === null));
}

export function isDeterministicIntegration(task: Task): boolean {
  return isInternalIntegration(task) && !(task.integration && typeof task.integration.step === "object" && "resolving" in task.integration.step);
}

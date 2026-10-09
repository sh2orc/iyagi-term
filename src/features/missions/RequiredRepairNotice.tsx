import type { Mission } from "../../generated/Mission";
import type { Task } from "../../generated/Task";
import { useI18n } from "../../i18n";
import { useMissionStore, type MissionStoreState } from "./store";
import { StatusNotice } from "./StatusNotice";

export function requiredRepairOwner(s: Pick<MissionStoreState, "tasks" | "runs">, task: Task): Task | null {
  if (task.state !== "failed") return null;
  const latest = Object.values(s.runs).filter(r => r.task_id === task.id && r.mission_id === task.mission_id).sort((a, b) => b.attempt - a.attempt)[0];
  if (!latest) return null;
  return Object.values(s.tasks).find(candidate => candidate.mission_id === task.mission_id
    && candidate.kind === "plan" && candidate.role === "lead"
    && !["cancelled", "superseded"].includes(candidate.state)
    && candidate.failure_repair_run_ids?.includes(latest.id)) ?? null;
}

export function RequiredRepairNotice({ mission, task }: { mission: Mission; task: Task }): JSX.Element | null {
  const { t } = useI18n();
  const repair = useMissionStore(s => {
    if (task.failure_repair_run_ids?.length) return task;
    return requiredRepairOwner(s, task);
  });
  if (!repair || repair.kind !== "plan" || repair.role !== "lead"
    || repair.mission_id !== mission.id || ["cancelled", "superseded"].includes(repair.state)
    || ["completed", "cancelled", "failed"].includes(mission.state)) return null;
  const params = { cycle: repair.repair_cycle, limit: mission.policy.max_repair_cycles };
  const stopped = repair.state === "failed";
  return (
    <StatusNotice
      testId="required-repair-notice"
      line={t(stopped ? "missions.notice.requiredRepairStopped" : "missions.notice.requiredRepairWorking", params)}
      details={<p>{t(stopped ? "missions.requiredRepair.stopped" : "missions.requiredRepair.working", params)}</p>}
    />
  );
}

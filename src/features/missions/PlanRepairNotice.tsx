import type { Run } from "../../generated/Run";
import type { Task } from "../../generated/Task";
import { useI18n } from "../../i18n";
import { StatusNotice } from "./StatusNotice";

export function PlanRepairNotice({ task, run }: { task: Task; run: Run | null }): JSX.Element | null {
  const { t } = useI18n();
  if (task.kind !== "plan" || task.role !== "lead" || run?.task_id !== task.id
    || run.state !== "failed" || run.retry_evidence?.basis !== "plan_format_rejected") return null;
  const waiting = task.state === "blocked" && task.blocked_code === "plan_format_repair";
  if (!waiting && task.state !== "failed") return null;
  return (
    <StatusNotice
      testId="plan-repair-notice"
      line={t(waiting ? "missions.planRepair.short" : "missions.notice.planRepairStopped")}
      details={<p>{t(waiting ? "missions.planRepair.waiting" : "missions.planRepair.stopped")}</p>}
    />
  );
}

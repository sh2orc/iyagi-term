import type { Run } from "../../generated/Run";
import type { Task } from "../../generated/Task";
import { useI18n } from "../../i18n";
import { capabilityBlocked } from "./bindingSupport";
import { useArtifactText } from "./viewUtils";
import { StatusNotice } from "./StatusNotice";

export function CapabilityNotice({ task, run, onChangeModel, onOpenSettings }: {
  task: Task;
  run: Run | null;
  onChangeModel?: () => void;
  onOpenSettings?: () => void;
}): JSX.Element | null {
  const { t } = useI18n();
  const waiting = task.state === "blocked" && capabilityBlocked(task.blocked_code);
  const failed = task.state === "failed" && run?.failure_code === "CAPABILITY_UNSUPPORTED" && run.retry_evidence?.basis === "request_not_submitted";
  const failure = useArtifactText(failed ? run?.result_ref ?? null : null);
  const installationChanged = failure.text?.includes("changed after installation check") === true;
  if (!waiting && !failed) return null;
  const actions = onChangeModel || onOpenSettings ? (
    <>
      {onOpenSettings ? <button type="button" onClick={onOpenSettings} data-testid="capability-open-settings">{t("missions.create.openSettings")}</button> : null}
      {onChangeModel ? <button type="button" onClick={onChangeModel} data-testid="capability-change-model">{t("missions.cost.changeModel")}</button> : null}
    </>
  ) : undefined;
  return (
    <StatusNotice
      testId="capability-notice"
      line={t(waiting ? "missions.notice.capabilityWaiting" : installationChanged ? "missions.compatibility.installationChanged" : "missions.notice.capabilityChanged")}
      actions={actions}
      details={<>
        <p>{t(waiting ? "missions.compatibility.waiting" : "missions.compatibility.changed")}</p>
        {failed && failure.text ? <pre className="mission-capability-failure">{failure.text}</pre> : null}
      </>}
    />
  );
}

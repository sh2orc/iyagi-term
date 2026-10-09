import type { Run } from "../../generated/Run";
import { useI18n } from "../../i18n";
import { useMissionStore } from "./store";
import { StatusNotice } from "./StatusNotice";

export function RecoveryNotice({ run }: { run: Run | null }): JSX.Element | null {
  const { t } = useI18n();
  if (!run || !["unknown", "interrupted"].includes(run.state)) return null;
  const ended = run.reconciliation_ref !== null;
  return (
    <>
      <StatusNotice
        testId="recovery-notice"
        line={t(ended ? "missions.recovery.summary" : "missions.notice.recoveryUnconfirmed")}
        details={<p>{t(ended ? "missions.recovery.ended" : "missions.recovery.unconfirmed")}</p>}
      />
      <RecoveryCoverageNotice run={run} />
    </>
  );
}

export function RecoveryCoverageNotice({ run }: { run: Run | null }): JSX.Element | null {
  const { t } = useI18n();
  const exec = useMissionStore((s) => run?.exec_id ? s.execs[run.exec_id] : undefined);
  if (!run?.reconciliation_ref || exec?.run_id !== run.id || exec.mission_id !== run.mission_id || exec.group_kind !== "observed_tree") return null;
  return (
    <StatusNotice
      testId="recovery-coverage-notice"
      line={t("missions.notice.recoveryCoverage")}
      details={<p>{t("missions.recovery.observedCoverage")}</p>}
    />
  );
}

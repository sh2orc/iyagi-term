import type { Task } from "../../generated/Task";
import { useI18n } from "../../i18n";
import { StatusNotice } from "./StatusNotice";

/** 해제/재시도 시각(알 수 없으면 "확인할 수 없음"). */
function dispatchAfter(task: Task, language: string, unknown: string): string {
  const date = task.dispatch_after_unix_ms == null ? null : new Date(Number(task.dispatch_after_unix_ms));
  return date && Number.isFinite(date.getTime()) ? date.toLocaleString(language) : unknown;
}

export function RateLimitNotice({ task, onChangeModel }: { task: Task; onChangeModel?: () => void }): JSX.Element | null {
  const { t, language } = useI18n();
  if (task.state !== "blocked" || task.blocked_code !== "provider_rate_limited") return null;
  const reset = dispatchAfter(task, language, t("missions.quota.unknownReset"));
  return (
    <StatusNotice
      testId="rate-limit-notice"
      line={t("missions.notice.rateLimit", { reset })}
      actions={onChangeModel ? (
        <button type="button" onClick={onChangeModel} data-testid="rate-limit-change-model">{t("missions.cost.changeModel")}</button>
      ) : undefined}
      details={<p>{t("missions.quota.waiting", { reset })} {t("missions.quota.options")}</p>}
    />
  );
}

export function RetryNotice({ task, onChangeModel }: { task: Task; onChangeModel?: () => void }): JSX.Element | null {
  const { t, language } = useI18n();
  if (task.state !== "blocked" || task.blocked_code !== "transient_retry") return null;
  const after = dispatchAfter(task, language, t("missions.quota.unknownReset"));
  return (
    <StatusNotice
      testId="retry-notice"
      line={t("missions.notice.retry", { after })}
      actions={onChangeModel ? (
        <button type="button" onClick={onChangeModel} data-testid="retry-change-model">{t("missions.cost.changeModel")}</button>
      ) : undefined}
      details={<p>{t("missions.retry.waiting", { after })} {t("missions.retry.options")}</p>}
    />
  );
}

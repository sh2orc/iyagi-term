import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useI18n } from "../../i18n";
import { isTauri } from "../bridge/ipc";
import { refreshSubscriptionUsage, SUBSCRIPTION_REFRESH_EVENT } from "./client";
import type { ProviderUsage, UsageWindow } from "./types";
import { quotaGauges, type QuotaGauge } from "./quotaGauges";

const POLL_MS = 60_000;
/** 남은 비율이 이 값 이하이면 게이지를 빨갛게 — 상태 바 CPU/RAM/Disk의 80% 사용 기준과 같은 선. */
const LOW_REMAINING_PERCENT = 20;

/** 상태 바 표시: 공급자마다 라벨 · 막대 · 오른쪽 정렬 퍼센트. */
export function QuotaGauges({ gauges }: { gauges: QuotaGauge[] }): JSX.Element {
  return (
    <>
      {gauges.map((gauge) => (
        <span key={`${gauge.provider}:${gauge.name}`} className={`strip-quota${gauge.remaining <= LOW_REMAINING_PERCENT ? " strip-quota-low" : ""}`}>
          <span className="strip-label">{gauge.name}</span>
          <span className="strip-quota-bar" aria-hidden="true">
            <span className="strip-quota-fill" style={{ width: `${gauge.remaining}%` }} />
          </span>
          <span className="strip-quota-value">{gauge.remaining}%</span>
        </span>
      ))}
    </>
  );
}

export function SubscriptionUsageIndicator(): JSX.Element {
  const { t } = useI18n();
  const [providers, setProviders] = useState<ProviderUsage[]>([]);
  const [open, setOpen] = useState(false);
  const [loading, setLoading] = useState(false);
  const [failed, setFailed] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const refreshSequence = useRef(0);

  const refresh = useCallback(async () => {
    if (!isTauri()) return;
    const sequence = ++refreshSequence.current;
    setLoading(true);
    try {
      const result = await refreshSubscriptionUsage();
      if (sequence !== refreshSequence.current) return;
      setProviders(result);
      setFailed(false);
    } catch {
      if (sequence !== refreshSequence.current) return;
      setFailed(true);
    } finally {
      if (sequence === refreshSequence.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
    // 트레이에 숨어 있는 동안은 폴링하지 않는다 — 매 분 CLI를 띄우고 HTTP를
    // 치는 일은 보이는 상태 바를 위한 것이다. 다시 보이면 곧바로 갱신한다.
    const poll = () => {
      if (typeof document !== "undefined" && document.hidden) return;
      void refresh();
    };
    const timer = window.setInterval(poll, POLL_MS);
    const onRefresh = () => void refresh();
    window.addEventListener(SUBSCRIPTION_REFRESH_EVENT, onRefresh);
    document.addEventListener("visibilitychange", poll);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener(SUBSCRIPTION_REFRESH_EVENT, onRefresh);
      document.removeEventListener("visibilitychange", poll);
    };
  }, [refresh]);

  useEffect(() => {
    if (!open) return;
    const onPointer = (event: MouseEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onPointer);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const gauges = useMemo(() => quotaGauges(providers), [providers]);
  const unavailable = providers.filter((provider) => !gauges.some((gauge) => gauge.provider === provider.provider));
  const unavailableLabel = (provider: ProviderUsage) => t(
    provider.status === "notConfigured" || provider.reasonCode === "not_configured"
      ? "monitor.quota.needsSetup"
      : provider.status === "error" ? "monitor.quota.queryFailed" : "monitor.quota.noQuota",
  );
  const summary = failed ? t("monitor.quota.refreshFailed") : [
    ...gauges.map((gauge) => `${gauge.name} ${gauge.remaining}%`),
    ...unavailable.map((provider) => `${provider.displayName} ${unavailableLabel(provider)}`),
  ].join(" · ");

  return (
    <div className="strip-subscriptions" ref={root}>
      <button
        type="button"
        className="strip-subscription-button"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={summary || undefined}
        onClick={() => setOpen((value) => !value)}
      >
        {failed ? t("monitor.quota.queryFailed") : providers.length > 0 ? (
          <>
            <QuotaGauges gauges={gauges} />
            {unavailable.map((provider) => (
              <span key={provider.provider} className="strip-quota strip-quota-unavailable"
                title={reasonText(provider.reasonCode ?? "no_data", t)}>
                <span className="strip-label">{provider.displayName}</span>
                <span>{unavailableLabel(provider)}</span>
              </span>
            ))}
          </>
        ) : t(loading ? "monitor.quota.loading" : "monitor.quota.empty")}
      </button>
      {open ? (
        <section className="subscription-popover" role="dialog" aria-label={t("monitor.quota.title")}>
          <header>
            <strong>{t("monitor.quota.title")}</strong>
            <button type="button" disabled={loading} onClick={() => void refresh()}>
              {loading ? t("monitor.quota.loading") : t("monitor.quota.refresh")}
            </button>
          </header>
          {failed ? <p className="setting-error">{t("monitor.quota.refreshFailed")}</p> : null}
          {providers.length === 0 && !failed ? (
            <p className="muted">{t(!isTauri() ? "monitor.quota.desktopOnly" : loading ? "monitor.quota.loading" : "monitor.quota.reason.no_data")}</p>
          ) : null}
          {providers.map((provider) => (
            <article className="subscription-provider" key={provider.provider}>
              <div className="subscription-provider-title">
                <strong>{provider.displayName}</strong>
                {provider.plan ? <span>{provider.plan}</span> : null}
              </div>
              {provider.status === "ready" ? provider.windows.map((window) => (
                <QuotaWindow key={window.id} window={window} />
              )) : (
                <p className="muted">{reasonText(provider.reasonCode, t)}</p>
              )}
              {provider.observedAt ? (
                <small>{t("monitor.quota.updated", { time: formatTime(provider.observedAt) })}</small>
              ) : null}
            </article>
          ))}
        </section>
      ) : null}
    </div>
  );
}

function QuotaWindow({ window }: { window: UsageWindow }): JSX.Element {
  const { t } = useI18n();
  return (
    <div className="subscription-window">
      <div>
        <span>{window.label}</span>
        <strong>{t("monitor.quota.usedRemaining", {
          used: Math.round(window.usedPercent),
          remaining: Math.round(window.remainingPercent),
        })}</strong>
      </div>
      <progress max={100} value={window.remainingPercent} aria-label={`${window.label} ${Math.round(window.remainingPercent)}%`} />
      {window.used !== null && window.limit !== null ? (
        <small>{t("monitor.quota.count", { used: formatCount(window.used), limit: formatCount(window.limit) })}</small>
      ) : null}
      {window.resetsAt ? <small>{t("monitor.quota.resets", { time: formatTime(window.resetsAt) })}</small> : null}
    </div>
  );
}

function formatTime(epochSeconds: number): string {
  return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })
    .format(new Date(epochSeconds * 1000));
}

function formatCount(value: number): string {
  return new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(value);
}

type Translate = (key: string, params?: Record<string, string | number>) => string;

function reasonText(reason: string | null, t: Translate): string {
  const known = new Set([
    "cli_not_found", "subscription_login_required", "integration_required", "not_configured",
    "auth_failed", "no_data", "network_error", "timeout", "credential_store_error",
    "api_key_invalid", "provider_error", "response_invalid", "client_error", "cli_start_failed", "protocol_error",
  ]);
  return t(`monitor.quota.reason.${known.has(reason ?? "") ? reason : "unavailable"}`);
}

/**
 * ResourceStrip (04-ui.md §1): 하단 28px 상태바.
 * `CPU x.y / N cores | RAM a / b GiB · 압력 보통 | Disk R/W … | NET ↓ ↑`
 * 측정 불가는 "—" + 이유 tooltip(U15). host와 workload 메모리는 절대 합산하지
 * 않는다. 클릭하면 5분 그래프 drawer가 열린다.
 */

import { memo } from "react";
import { useI18n } from "../../i18n";
import type { HostSample } from "../../generated/HostSample";
import type { Metric } from "../../generated/Metric";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { queueWaitText } from "./statusStrings";
import {
  displayMetric,
  formatCoreNumber,
  formatGiB,
  parseU64,
  pressureText,
  UNAVAILABLE,
} from "./format";
import { safeAvailableBytes } from "./headroom";
import { RESOURCE_STRIP_PX } from "../terminal/splitTree";
import { SubscriptionUsageIndicator } from "../subscriptions/SubscriptionUsageIndicator";
import { DaemonOutdatedStripItem } from "../app/DaemonOutdatedBanner";
import type { DaemonClient } from "../daemon/client";

/** buildSegments/emptySegments로 넘기는 번역 함수(useI18n().t와 동일 형태). */
type TFunc = (key: string, params?: Record<string, string | number>) => string;

export const ResourceStrip = memo(function ResourceStrip({ client }: { client?: DaemonClient }): JSX.Element {
  const { t } = useI18n();
  const host = useWorkbenchStore((s) => s.host);
  const running = useWorkbenchStore((s) => s.workloads.filter((w) => w.mode === "managed" && (w.state === "RUNNING" || w.state === "STARTING")).length);
  const queued = useWorkbenchStore((s) => s.queue.length);
  // 08-pressure-relief §2: 지금 양보 중인 세션 수. CPU 세그먼트에 붙여
  // "압력 때문에 무엇이 일어나고 있는지"를 압력 문구 옆에서 바로 읽게 한다.
  const yielded = useWorkbenchStore(
    (s) => s.workloads.filter((w) => w.relief?.kind === "YIELDED").length,
  );
  // 08-pressure-relief §5: 가드가 일시정지한 작업 수. 정지된 백그라운드 탭은
  // 활동 점이 꺼져 조용한 셸과 구분되지 않는다 — 이 배지가 유일한 상시 단서다.
  const suspended = useWorkbenchStore(
    (s) => s.workloads.filter((w) => w.guard?.kind === "SUSPENDED").length,
  );
  // 대기 사유 상시 노출(W3-3): 그래프가 아니라 이 한 줄이 사용자가 자원
  // 관리를 체감하는 자리다 — QueueDrawer를 열지 않아도 이유가 보인다.
  const waitReason = useWorkbenchStore((s) => {
    if (s.queue.length === 0) return null;
    const workloads = s.workloads;
    const runningManaged = workloads.filter(
      (w) => w.mode === "managed" && (w.state === "RUNNING" || w.state === "STARTING"),
    ).length;
    const entry = s.queue[0];
    const need = s.workloadById.get(entry.workload_id);
    return queueWaitText(entry.wait_reason, {
      runningManaged,
      needBytes: need ? parseU64(need.reservation_bytes) : null,
      // QueueDrawer와 같은 예비 계산 — 스트립만 "여유 —"로 비지 않게.
      safeAvailableBytes: safeAvailableBytes(s.host),
    });
  });
  const setGraphDrawer = useWorkbenchStore((s) => s.setGraphDrawer);
  const toggleQueueDrawer = useWorkbenchStore((s) => s.toggleQueueDrawer);

  const segments = host ? buildSegments(host, t, yielded) : emptySegments(t);

  return (
    <footer className="resource-strip" style={{ height: RESOURCE_STRIP_PX }} role="contentinfo">
      <button
        type="button"
        className="strip-metrics"
        aria-label={t("monitor.strip.openGraph")}
        onClick={() => setGraphDrawer(true)}
      >
        {segments.map((seg) => (
          <span
            key={seg.label}
            className={`strip-segment${seg.utilization !== null && seg.utilization !== undefined && seg.utilization >= 0.8 ? " strip-segment-high" : ""}`}
            title={seg.title}
          >
            <span className="strip-label">{seg.label}</span> {seg.text}
            {seg.extra ? <span className="strip-extra"> {seg.extra}</span> : null}
          </span>
        ))}
      </button>
      <SubscriptionUsageIndicator />
      {client ? <DaemonOutdatedStripItem client={client} /> : null}
      <button
        type="button"
        className="strip-managed"
        aria-label={t("monitor.strip.openQueue")}
        onClick={() => toggleQueueDrawer()}
      >
        {t("monitor.strip.managed", { running, queued })}
        {suspended > 0 ? (
          <span className="strip-suspended" title={t("monitor.strip.suspendedTitle", { n: suspended })}>
            {" · "}
            {t("monitor.strip.suspended", { n: suspended })}
          </span>
        ) : null}
        {waitReason ? <span className="strip-wait-reason"> · {waitReason}</span> : null}
      </button>
    </footer>
  );
});

interface StripSegment {
  label: string;
  text: string;
  extra?: string;
  title?: string;
  utilization?: number | null;
}

function buildSegments(host: HostSample, t: TFunc, yielded = 0): StripSegment[] {
  const cpu = displayMetric(host.cpu_cores_used, (v) => formatCoreNumber(v));
  const total = metricNumber(host.physical_total_bytes);
  const available = metricNumber(host.physical_available_bytes);
  // Prefer the OS's own "used" (app + wired + compressed, ~Activity Monitor).
  // Fall back to total - available for daemons that don't report it; the two
  // differ on macOS, where total - available counts reclaimable cache as free.
  const usedReported = host.physical_used_bytes ? metricNumber(host.physical_used_bytes) : null;
  const ramUsed = usedReported ?? (total !== null && available !== null ? total - available : null);

  const net = host.interfaces.find((i) => !i.is_loopback) ?? null;
  const absent = absentMetric<number>(t("monitor.strip.noNetInterface"));
  const netRx = displayMetric(net ? net.rx_bytes_per_sec : absent, (v) => formatRate(v));
  const netTx = displayMetric(net ? net.tx_bytes_per_sec : absent, (v) => formatRate(v));

  const disk = host.disks[0] ?? null;
  const diskFree = disk ? displayMetric(disk.free_bytes, (v) => formatGiB(parseU64(v) ?? null)) : unavailable(t("monitor.strip.noDisk"));

  return [
    {
      label: "CPU",
      text: `${cpu.text} / ${t("monitor.unit.cores", { value: host.logical_cpu_count })}`,
      // RAM과 같은 자리·같은 문구로 CPU 압력(데몬 히스테리시스 판정)을 붙인다.
      // 값 자체가 아니라 판정이므로 근거는 tooltip 한 줄로만 설명한다.
      // 양보가 걸려 있으면 그 수를 같은 자리에 잇는다(08 §2).
      extra:
        t("monitor.strip.pressure", { level: pressureText(host.cpu_pressure ?? "NORMAL") }) +
        (yielded > 0 ? ` ${t("monitor.strip.yielded", { count: yielded })}` : ""),
      title: `${cpu.unavailableReason ?? `${cpu.source} · ${cpu.quality}`} · ${t("monitor.strip.cpuPressureBasis")}`,
      utilization: usageRatio(metricNumber(host.cpu_cores_used), host.logical_cpu_count),
    },
    {
      label: "RAM",
      text: `${ramUsed !== null ? formatGiB(ramUsed) : UNAVAILABLE} / ${formatGiB(total)}`,
      extra: t("monitor.strip.pressure", { level: pressureText(host.pressure) }),
      title: host.physical_available_bytes.reason ?? t("monitor.strip.ramBasis", { source: (host.physical_used_bytes ?? host.physical_available_bytes).source }),
      utilization: usageRatio(ramUsed, total),
    },
    {
      // HostSample 계약에 호스트 단위 디스크 속도가 없다 — "—"와 이유 tooltip.
      label: "Disk",
      text: t("monitor.strip.disk", { rw: UNAVAILABLE, free: diskFree.text }),
      title: diskFree.unavailableReason ?? t("monitor.strip.noDiskRate"),
      utilization: disk ? usedCapacityRatio(metricNumber(disk.free_bytes), metricNumber(disk.capacity_bytes)) : null,
    },
    {
      label: "NET",
      text: `↓ ${netRx.text} ↑ ${netTx.text}`,
      extra: net ? `· ${net.name}` : "",
      title: netRx.unavailableReason ?? netTx.unavailableReason ?? `${netRx.source} · ${netRx.quality}`,
    },
  ].map((segment) => ({
    ...segment,
    title: segment.utilization !== null && segment.utilization !== undefined
      ? `${t(segment.label === "Disk" ? "monitor.strip.diskUtilization" : "monitor.strip.utilization", { percent: Math.round(segment.utilization * 1000) / 10 })} · ${segment.title}`
      : segment.title,
  }));
}

function metricNumber(metric: Metric<string> | Metric<number>): number | null {
  if (metric.quality === "unavailable" || metric.value === null) return null;
  const value = typeof metric.value === "string" ? parseU64(metric.value) : metric.value;
  return value !== null && Number.isFinite(value) && value >= 0 ? value : null;
}

function usageRatio(used: number | null, total: number | null): number | null {
  if (used === null || total === null || !Number.isFinite(total) || total <= 0 || used < 0 || used > total) return null;
  return used / total;
}

function usedCapacityRatio(free: number | null, total: number | null): number | null {
  return usageRatio(free !== null && total !== null ? total - free : null, total);
}

function emptySegments(t: TFunc): StripSegment[] {
  return [
    { label: "CPU", text: UNAVAILABLE, title: t("monitor.strip.awaitingSample") },
    { label: "RAM", text: UNAVAILABLE, title: t("monitor.strip.awaitingSample") },
    { label: "Disk", text: UNAVAILABLE, title: t("monitor.strip.awaitingSample") },
    { label: "NET", text: UNAVAILABLE, title: t("monitor.strip.awaitingSample") },
  ];
}

function unavailable(reason: string): { text: string; unavailableReason: string | null; source: string; quality: string | null } {
  return { text: UNAVAILABLE, unavailableReason: reason, source: "—", quality: null };
}

function absentMetric<T>(reason: string): Metric<T> {
  return { value: null, source: "monitor", quality: "unavailable", reason };
}

function formatRate(bytesPerSec: number): string {
  return `${formatGiB(bytesPerSec)}/s`;
}

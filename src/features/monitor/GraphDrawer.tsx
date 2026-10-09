/**
 * GraphDrawer (04-ui.md §1·§11): monitor 클릭 시 5분 그래프 + 출처/품질.
 * 300-sample 고정 ring(graphRing.ts) — store 밖에 있다.
 */

import { useEffect, useState } from "react";
import { useI18n } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { GRAPH_SAMPLES, resourceHistory, type GraphMetricKey } from "./graphRing";
import { UNAVAILABLE } from "./format";

const ORDER: GraphMetricKey[] = ["cpuCores", "ramUsedGiB", "netRxBytesPerSec", "netTxBytesPerSec", "diskFreeGiB"];

export function GraphDrawer(): JSX.Element | null {
  const { t } = useI18n();
  const open = useWorkbenchStore((s) => s.graphDrawerOpen);
  const setOpen = useWorkbenchStore((s) => s.setGraphDrawer);
  const [, force] = useState(0);

  useEffect(() => {
    if (!open) return;
    const sub = resourceHistory.subscribe(() => force((n) => n + 1));
    return () => sub.dispose();
  }, [open]);

  if (!open) return null;
  const metrics = resourceHistory.metrics();

  return (
    <aside className="graph-drawer" aria-label={t("monitor.graph.aria")}>
      <header className="drawer-header">
        <span>{t("monitor.graph.header", { samples: GRAPH_SAMPLES })}</span>
        <button type="button" aria-label={t("monitor.graph.close")} onClick={() => setOpen(false)}>
          ×
        </button>
      </header>
      <div className="graph-grid">
        {ORDER.map((key) => {
          const ring = resourceHistory.rings[key];
          const meta = metrics?.[key] ?? null;
          const values = ring.toArray();
          return (
            <figure key={key} className="graph-cell">
              <figcaption>
                {meta?.label ?? key}
                <span className="graph-source" title={meta?.unavailableReason ?? meta?.source ?? undefined}>
                  {meta
                    ? meta.unavailableReason
                      ? `${UNAVAILABLE} — ${meta.unavailableReason}`
                      : `${meta.source} · ${meta.quality}`
                    : t("monitor.graph.awaitingSamples")}
                </span>
              </figcaption>
              <Sparkline values={values} />
            </figure>
          );
        })}
      </div>
    </aside>
  );
}

function Sparkline(props: { values: Array<number | null> }): JSX.Element {
  const { values } = props;
  if (values.length === 0) {
    return <div className="sparkline sparkline-empty">{UNAVAILABLE}</div>;
  }
  const numeric = values.filter((v): v is number => v !== null);
  if (numeric.length === 0) {
    return <div className="sparkline sparkline-empty">{UNAVAILABLE}</div>;
  }
  const max = Math.max(...numeric, 0.000001);
  const min = Math.min(...numeric, 0);
  const span = Math.max(max - min, 0.000001);
  const width = 100;
  const height = 28;
  let d = "";
  values.forEach((v, i) => {
    const x = (i / Math.max(values.length - 1, 1)) * width;
    const y = v === null ? null : height - ((v - min) / span) * height;
    if (y === null) return;
    d += `${d ? " L" : "M"}${x.toFixed(2)},${y.toFixed(2)}`;
  });
  return (
    <svg className="sparkline" viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" aria-hidden>
      <path d={d} fill="none" stroke="currentColor" strokeWidth="1" vectorEffect="non-scaling-stroke" />
    </svg>
  );
}

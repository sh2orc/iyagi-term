/**
 * In-memory 5-minute resource graphs: fixed 300-sample rings
 * (defaults.json graph_samples). Kept OUTSIDE the zustand store — the store
 * holds only the latest selected sample (04-ui.md §4).
 */

import type { HostSample } from "../../generated/HostSample";
import type { Metric } from "../../generated/Metric";
import { t } from "../../i18n";
import { parseU64 } from "./format";

export const GRAPH_SAMPLES = 300;

export class SampleRing {
  private readonly values: Array<number | null> = [];
  private readonly capacity: number;

  constructor(capacity = GRAPH_SAMPLES) {
    this.capacity = capacity;
  }

  push(value: number | null): void {
    this.values.push(value);
    if (this.values.length > this.capacity) this.values.shift();
  }

  toArray(): Array<number | null> {
    return [...this.values];
  }

  get length(): number {
    return this.values.length;
  }

  get last(): number | null {
    return this.values.length > 0 ? this.values[this.values.length - 1] : null;
  }
}

export type GraphMetricKey = "cpuCores" | "ramUsedGiB" | "netRxBytesPerSec" | "netTxBytesPerSec" | "diskFreeGiB";

export interface GraphMetricMeta {
  label: string;
  unit: string;
  source: string | null;
  quality: string | null;
  unavailableReason: string | null;
}

type Listener = () => void;

export class GraphCollector {
  readonly rings: Record<GraphMetricKey, SampleRing>;
  private readonly listeners = new Set<Listener>();
  private meta: Record<GraphMetricKey, GraphMetricMeta> | null = null;

  constructor(capacity = GRAPH_SAMPLES) {
    const make = () => new SampleRing(capacity);
    this.rings = {
      cpuCores: make(),
      ramUsedGiB: make(),
      netRxBytesPerSec: make(),
      netTxBytesPerSec: make(),
      diskFreeGiB: make(),
    };
  }

  push(sample: HostSample): void {
    const GiB = 1024 ** 3;
    const total = parseU64(sample.physical_total_bytes.value);
    const available = parseU64(sample.physical_available_bytes.value);
    const ramUsed = total !== null && available !== null ? (total - available) / GiB : null;

    const net = sample.interfaces.find((i) => !i.is_loopback);

    this.rings.cpuCores.push(sample.cpu_cores_used.value);
    this.rings.ramUsedGiB.push(ramUsed);
    this.rings.netRxBytesPerSec.push(net ? net.rx_bytes_per_sec.value : null);
    this.rings.netTxBytesPerSec.push(net ? net.tx_bytes_per_sec.value : null);
    const diskFreeBytes = sample.disks[0] ? parseU64(sample.disks[0].free_bytes.value) : null;
    this.rings.diskFreeGiB.push(diskFreeBytes !== null ? diskFreeBytes / GiB : null);

    this.meta = {
      cpuCores: metricMeta(t("monitor.graph.cpu"), sample.cpu_cores_used),
      ramUsedGiB: {
        label: t("monitor.graph.ram"),
        unit: "GiB",
        source: sample.physical_available_bytes.source,
        quality: sample.physical_available_bytes.quality,
        unavailableReason: available === null ? sample.physical_available_bytes.reason : null,
      },
      netRxBytesPerSec: net
        ? metricMeta(`NET ↓ ${net.name} (B/s)`, net.rx_bytes_per_sec)
        : unavailableMeta("NET ↓"),
      netTxBytesPerSec: net
        ? metricMeta(`NET ↑ ${net.name} (B/s)`, net.tx_bytes_per_sec)
        : unavailableMeta("NET ↑"),
      diskFreeGiB: sample.disks[0]
        ? metricMeta(t("monitor.graph.diskFreeMount", { mount: sample.disks[0].mount }), sample.disks[0].free_bytes)
        : unavailableMeta(t("monitor.graph.diskFree")),
    };
    for (const listener of this.listeners) listener();
  }

  metrics(): Record<GraphMetricKey, GraphMetricMeta> | null {
    return this.meta;
  }

  subscribe(listener: Listener): { dispose(): void } {
    this.listeners.add(listener);
    return { dispose: () => this.listeners.delete(listener) };
  }
}

function metricMeta(label: string, metric: Metric<number | string>): GraphMetricMeta {
  const unavailable = metric.value === null || metric.quality === "unavailable";
  return {
    label,
    unit: "",
    source: metric.source,
    quality: metric.quality,
    unavailableReason: unavailable ? (metric.reason ?? t("monitor.graph.cannotMeasure", { source: metric.source })) : null,
  };
}

function unavailableMeta(label: string): GraphMetricMeta {
  return {
    label,
    unit: "",
    source: null,
    quality: null,
    unavailableReason: t("monitor.graph.noMetrics"),
  };
}

/** App-wide singleton (module scope, outside React state). */
export const resourceHistory = new GraphCollector();

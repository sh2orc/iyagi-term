/**
 * 숫자 표시 규칙 (04-ui.md §1·§7, 03 §2).
 * - CPU core: 1.0 = 논리 코어 하나를 계속 사용한 수준 → "1코어"/"2.4코어".
 * - bytes: GiB 우선(소수 1자리, 0 생략), 1 GiB 미만은 MiB.
 * - 측정 불가는 0이 아니라 "—" + 이유 tooltip(U15) — silent 0 금지.
 */

import { t } from "../../i18n";
import type { Metric } from "../../generated/Metric";
import type { MetricQuality } from "../../generated/MetricQuality";
import type { PressureLevel } from "../../generated/PressureLevel";

const GiB = 1024 ** 3;
const MiB = 1024 ** 2;
const KiB = 1024;

export const UNAVAILABLE = "—";

function trim1(value: number): string {
  const rounded = Math.round(value * 10) / 10;
  return Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1);
}

/**
 * Numeric part of a core count, distinguishing three real states that a plain
 * 1-decimal round would collapse into a misleading "0":
 * - exactly 0 → "0" (measured idle: the process is alive but used no CPU)
 * - 0 < c < 0.1 → "<0.1" (alive and lightly loaded — e.g. an agent awaiting a
 *   model response spends almost all its time blocked on the network, so its
 *   averaged usage is a real but tiny fraction of a core, not zero)
 * - otherwise → 1-decimal (2.4)
 * A caller with a null value renders "—" (unavailable) before reaching here.
 */
function coreDigits(cores: number): string {
  if (cores <= 0) return "0";
  if (cores < 0.1) return "<0.1";
  return trim1(cores);
}

/** Core count display: 0.03 → "<0.1코어", 1 → "1코어", 2.4 → "2.4코어". */
export function formatCores(cores: number | null): string {
  if (cores === null || !Number.isFinite(cores)) return UNAVAILABLE;
  return t("monitor.unit.cores", { value: coreDigits(cores) });
}

/** Bare core number for `CPU 2.4 / 12 cores` style strings. */
export function formatCoreNumber(cores: number | null): string {
  if (cores === null || !Number.isFinite(cores)) return UNAVAILABLE;
  return coreDigits(cores);
}

/** GiB-first byte formatting: 2 GiB, 1.3 GiB, 512 MiB, 3 KiB. */
export function formatGiB(bytes: number | null): string {
  if (bytes === null || !Number.isFinite(bytes)) return UNAVAILABLE;
  if (bytes >= GiB) return `${trim1(bytes / GiB)} GiB`;
  if (bytes >= MiB) return `${trim1(bytes / MiB)} MiB`;
  if (bytes >= KiB) return `${trim1(bytes / KiB)} KiB`;
  return `${Math.round(bytes)} B`;
}

/** Rate formatting in MiB/s with GiB promotion. */
export function formatBytesPerSec(bytesPerSec: number | null): string {
  if (bytesPerSec === null || !Number.isFinite(bytesPerSec)) return UNAVAILABLE;
  return `${formatGiB(bytesPerSec)}/s`;
}

/** Parse a U64String (decimal string) to a number, null when invalid. */
export function parseU64(value: string | null | undefined): number | null {
  if (value === null || value === undefined || value === "") return null;
  const n = Number(value);
  return Number.isFinite(n) ? n : null;
}

export interface MetricDisplay {
  text: string;
  /** "—"인 이유 (tooltip용, U15). */
  unavailableReason: string | null;
  source: string;
  quality: MetricQuality | null;
}

/**
 * Format a Metric for display; unavailable metrics render "—" with their
 * reason instead of a silent 0.
 */
export function displayMetric<T extends number | string>(
  metric: Metric<T>,
  format: (value: T) => string,
): MetricDisplay {
  if (metric.value === null || metric.quality === "unavailable") {
    return {
      text: UNAVAILABLE,
      unavailableReason: metric.reason ?? t("monitor.metric.unavailableReason", { source: metric.source }),
      source: metric.source,
      quality: metric.quality,
    };
  }
  return { text: format(metric.value), unavailableReason: null, source: metric.source, quality: metric.quality };
}

const PRESSURE_KEY: Record<PressureLevel, string> = {
  NORMAL: "monitor.pressure.normal",
  WARNING: "monitor.pressure.warning",
  CRITICAL: "monitor.pressure.critical",
};

export function pressureText(level: PressureLevel): string {
  return t(PRESSURE_KEY[level]);
}

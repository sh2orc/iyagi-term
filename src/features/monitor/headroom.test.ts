import { describe, expect, it } from "vitest";
import type { HostSample } from "../../generated/HostSample";
import type { Metric } from "../../generated/Metric";
import { HOST_RESERVE_MIN_BYTES, hostReserveBytes, safeAvailableBytes } from "./headroom";

const metric = <T,>(value: T): Metric<T> => ({ value, source: "test", quality: "measured", reason: null });
/** 측정 실패(value null)는 quality unavailable + reason으로 표현한다. */
const bytes = (value: string | null): Metric<string> =>
  value === null
    ? { value: null, source: "test", quality: "unavailable", reason: "test" }
    : metric(value);
function host(total: string | null, available: string | null): HostSample {
  return {
    monotonic_ms: 0,
    logical_cpu_count: 8,
    cpu_cores_used: metric(1),
    physical_total_bytes: bytes(total),
    physical_available_bytes: bytes(available),
    swap_used_bytes: metric("0"),
    pressure: "NORMAL",
    cpu_pressure: "NORMAL",
    disks: [],
    interfaces: [],
  };
}

describe("safeAvailableBytes", () => {
  it("예비(max(2 GiB, 15%))를 뺀 여유를 돌려준다", () => {
    const total = 32 * 1024 ** 3;
    expect(hostReserveBytes(total)).toBe(Math.round(total * 0.15));
    expect(hostReserveBytes(8 * 1024 ** 3)).toBe(HOST_RESERVE_MIN_BYTES);
    expect(safeAvailableBytes(host(String(8 * 1024 ** 3), String(3 * 1024 ** 3)))).toBe(1024 ** 3);
  });

  it("측정값이 없으면 0이 아니라 null", () => {
    expect(safeAvailableBytes(null)).toBeNull();
    expect(safeAvailableBytes(host(null, "100"))).toBeNull();
    expect(safeAvailableBytes(host(String(8 * 1024 ** 3), String(1024 ** 3)))).toBe(0);
  });
});

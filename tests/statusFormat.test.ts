import { describe, expect, it } from "vitest";
import {
  UNAVAILABLE,
  displayMetric,
  formatBytesPerSec,
  formatCoreNumber,
  formatCores,
  formatGiB,
  parseU64,
  pressureText,
} from "../src/features/monitor/format";

const GiB = 1024 ** 3;
const MiB = 1024 ** 2;

describe("core formatting (1.0 = 1코어)", () => {
  it("renders whole cores without decimals", () => {
    expect(formatCores(1.0)).toBe("1코어");
    expect(formatCores(2)).toBe("2코어");
  });

  it("renders fractional cores with one decimal", () => {
    expect(formatCores(2.4)).toBe("2.4코어");
    expect(formatCores(0.5)).toBe("0.5코어");
  });

  it("null cores display as unavailable", () => {
    expect(formatCores(null)).toBe(UNAVAILABLE);
    expect(formatCoreNumber(null)).toBe(UNAVAILABLE);
    expect(formatCoreNumber(2.4)).toBe("2.4");
  });
});

describe("GiB formatting", () => {
  it("matches the spec examples", () => {
    expect(formatGiB(2 * GiB)).toBe("2 GiB");
    expect(formatGiB(Math.round(1.3 * GiB))).toBe("1.3 GiB");
  });

  it("falls back to MiB below 1 GiB and formats rates", () => {
    expect(formatGiB(512 * MiB)).toBe("512 MiB");
    expect(formatGiB(1024)).toBe("1 KiB");
    expect(formatGiB(null)).toBe(UNAVAILABLE);
    expect(formatBytesPerSec(2 * MiB)).toBe("2 MiB/s");
  });

  it("parses U64String decimals", () => {
    expect(parseU64("2147483648")).toBe(2147483648);
    expect(parseU64(null)).toBeNull();
    expect(parseU64("")).toBeNull();
    expect(parseU64("nope")).toBeNull();
  });
});

describe("Metric display — unavailable is never a silent 0 (U15)", () => {
  it("renders measured values with source/quality", () => {
    const display = displayMetric({ value: 2.4, source: "sysinfo", quality: "measured", reason: null }, (v) => `${v}코어`);
    expect(display.text).toBe("2.4코어");
    expect(display.unavailableReason).toBeNull();
    expect(display.source).toBe("sysinfo");
    expect(display.quality).toBe("measured");
  });

  it("renders null values as — with the collector's reason", () => {
    const display = displayMetric(
      { value: null, source: "cgroup.v2", quality: "unavailable", reason: "cgroup 미지원 플랫폼" },
      () => "never",
    );
    expect(display.text).toBe(UNAVAILABLE);
    expect(display.unavailableReason).toBe("cgroup 미지원 플랫폼");
  });

  it("quality=unavailable with a value still renders as —", () => {
    const display = displayMetric({ value: 0, source: "mock.net", quality: "unavailable", reason: null }, (v) => String(v));
    expect(display.text).toBe(UNAVAILABLE);
    expect(display.unavailableReason).toContain("mock.net");
  });
});

describe("pressure labels", () => {
  it("maps enum levels to Korean", () => {
    expect(pressureText("NORMAL")).toBe("보통");
    expect(pressureText("WARNING")).toBe("경고");
    expect(pressureText("CRITICAL")).toBe("위험");
  });
});

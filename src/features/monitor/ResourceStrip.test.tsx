import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { HostSample } from "../../generated/HostSample";
import type { Metric } from "../../generated/Metric";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { t } from "../../i18n";
import { ResourceStrip } from "./ResourceStrip";

// SSR normally reads Zustand's initial snapshot; select the test's current sample.
vi.mock("../../store/workbenchStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../store/workbenchStore")>();
  const store = actual.useWorkbenchStore;
  return { ...actual, useWorkbenchStore: Object.assign((selector: (state: ReturnType<typeof store.getState>) => unknown) => selector(store.getState()), store) };
});

const metric = <T,>(value: T): Metric<T> => ({ value, source: "test", quality: "measured", reason: null });
function sample(percent: number): HostSample {
  return {
    monotonic_ms: 1000,
    logical_cpu_count: 10,
    cpu_cores_used: metric(percent / 10),
    physical_total_bytes: metric("1000"),
    physical_available_bytes: metric(String(1000 - percent * 10)),
    swap_used_bytes: metric("0"),
    pressure: "NORMAL",
    cpu_pressure: "NORMAL",
    disks: [{ mount: "/", capacity_bytes: metric("1000"), free_bytes: metric(String(1000 - percent * 10)) }],
    // Rates alone cannot establish a network utilization percentage.
    interfaces: [{ name: "en0", is_loopback: false, rx_bytes_per_sec: metric(1e9), tx_bytes_per_sec: metric(1e9) }],
  };
}
function highLabels(host: HostSample | null): string[] {
  useWorkbenchStore.setState({ host });
  const html = renderToStaticMarkup(<ResourceStrip />);
  return [...html.matchAll(/class="strip-segment strip-segment-high"[^>]*><span class="strip-label">([^<]+)/g)].map(m => m[1]);
}
/** label → { title, extra } (SSR 마크업을 세그먼트 단위로 쪼갠다). */
function segment(host: HostSample, label: string): { title: string; extra: string } {
  useWorkbenchStore.setState({ host });
  const html = renderToStaticMarkup(<ResourceStrip />);
  const chunk = html
    .split('<span class="strip-segment')
    .slice(1)
    .find((part) => /<span class="strip-label">([^<]+)</.exec(part)?.[1] === label);
  if (!chunk) throw new Error(`segment ${label} not rendered`);
  return {
    title: /^[^"]*" title="([^"]*)"/.exec(chunk)?.[1] ?? "",
    extra: /<span class="strip-extra">(.*?)<\/span>/.exec(chunk)?.[1] ?? "",
  };
}
afterEach(() => useWorkbenchStore.setState({ host: null }));

describe("resource strip utilization colors", () => {
  it("turns CPU, RAM and disk red at 80% and restores normal color below it", () => {
    expect(highLabels(sample(79.9))).toEqual([]);
    expect(highLabels(sample(80))).toEqual(["CPU", "RAM", "Disk"]);
    expect(highLabels(sample(100))).toEqual(["CPU", "RAM", "Disk"]);
    expect(highLabels(sample(50))).toEqual([]);
  });
  it("does not flag missing or unavailable samples", () => {
    expect(highLabels(null)).toEqual([]);
    const host = sample(90);
    host.cpu_cores_used.quality = "unavailable";
    host.physical_available_bytes.quality = "unavailable";
    host.disks[0].capacity_bytes.quality = "unavailable";
    expect(highLabels(host)).toEqual([]);
  });
  it("does not derive percentages from zero capacities or inconsistent samples", () => {
    const host = sample(90);
    host.logical_cpu_count = 0;
    host.physical_total_bytes.value = "0";
    host.disks[0].free_bytes.value = "2000";
    expect(highLabels(host)).toEqual([]);
  });
});

// 08-pressure-relief §1: CPU 압력은 데몬의 히스테리시스 판정이라 값만으로는
// 읽을 수 없다 — RAM과 같은 자리에 문구로, 근거는 tooltip으로 보여 준다.
describe("resource strip CPU pressure", () => {
  it("shows the daemon's CPU pressure next to the cores, with its basis in the tooltip", () => {
    const host = sample(50);
    host.cpu_pressure = "WARNING";
    const cpu = segment(host, "CPU");
    expect(cpu.extra).toContain("압력 경고");
    expect(cpu.title).toContain(
      "CPU 압력: 사용 코어/전체 코어 비율 · 85% 경고 · 95% 위험 · 70% 이하 10초 지속 시 회복",
    );
    // 기존 출처 표기(measured/source)는 그대로 남는다.
    expect(cpu.title).toContain("test · measured");
    // RAM 압력(순간 분류)과는 서로 독립이다.
    expect(segment(host, "RAM").extra).toContain("압력 보통");
  });

  it("shows the normal label when the daemon reports NORMAL", () => {
    expect(segment(sample(50), "CPU").extra).toContain("압력 보통");
  });

  it("falls back to NORMAL when an older daemon omits cpu_pressure", () => {
    const { cpu_pressure: _omitted, ...withoutCpuPressure } = sample(50);
    expect(segment(withoutCpuPressure as HostSample, "CPU").extra).toContain("압력 보통");
  });
});

// RAM은 시스템 전체 물리 메모리(사용/전체)다. '사용'은 OS의 used_memory를
// 우선 쓰고(활성 모니터와 같음), 없는 옛 데몬만 total-available로 되돌린다.
describe("resource strip RAM basis", () => {
  it("shows the daemon-reported used figure over total - available", () => {
    const host = sample(20); // available=800 → total-available=200
    host.physical_used_bytes = metric("450"); // OS used differs from 200
    const ram = segment(host, "RAM");
    expect(ram.title).toContain("시스템 전체 물리 메모리");
    // 450/1000 = 45% 사용률 (200/1000이 아니라).
    expect(ram.title).toContain("사용률 45%");
  });

  it("falls back to total - available when an older daemon omits physical_used_bytes", () => {
    const { physical_used_bytes: _omitted, ...legacy } = sample(20);
    // available=800 → used 200 → 20%.
    expect(segment(legacy as HostSample, "RAM").title).toContain("사용률 20%");
  });
});

// 08-pressure-relief §2: 양보가 걸려 있으면 CPU 압력 문구 옆에 그 수를 잇는다.
describe("resource strip yielded count", () => {
  afterEach(() => useWorkbenchStore.setState({ workloads: [] }));

  function withWorkloads(relief: Array<{ kind: "NONE" } | { kind: "YIELDED" }>): string {
    useWorkbenchStore.setState({
      workloads: relief.map((r, index) => ({
        workload_id: `w${index}`,
        session_id: `s${index}`,
        mode: "shell",
        state: "RUNNING",
        priority: 1,
        title: "zsh",
        cwd: "/w",
        program: "/bin/zsh",
        reservation_bytes: "0",
        cpu_slots: 1,
        enforcement: "observe",
        root_exited: false,
        cancel_requested: false,
        connection: "attached",
        relief: r.kind === "YIELDED" ? { kind: "YIELDED", since_ms: "1", manual: false, partial: false } : { kind: "NONE" },
        protected: false,
      })) as unknown as ReturnType<typeof useWorkbenchStore.getState>["workloads"],
    });
    return segment(sample(50), "CPU").extra;
  }

  it("양보 중인 세션 수를 압력 문구 뒤에 적는다", () => {
    const extra = withWorkloads([{ kind: "YIELDED" }, { kind: "NONE" }, { kind: "YIELDED" }]);
    expect(extra).toContain("압력 보통");
    expect(extra).toContain("양보 2");
  });

  it("양보가 없으면 아무것도 덧붙이지 않는다", () => {
    expect(withWorkloads([{ kind: "NONE" }])).not.toContain("양보");
  });
});

// 08-pressure-relief §5: 가드가 일시정지한 작업 수 — 큐 서랍 토글(관리 N 실행
// · N 대기)에 이어 붙는다. 정지된 백그라운드 탭은 활동 점이 꺼지므로 이 배지가
// 사용자가 볼 수 있는 유일한 상시 단서다.
describe("resource strip suspended count", () => {
  afterEach(() => useWorkbenchStore.setState({ workloads: [] }));

  function htmlWithGuard(guards: Array<"NONE" | "SUSPENDED">): string {
    useWorkbenchStore.setState({
      workloads: guards.map((kind, index) => ({
        workload_id: `w${index}`,
        session_id: `s${index}`,
        mode: "shell",
        state: "RUNNING",
        priority: 1,
        title: "zsh",
        cwd: "/w",
        program: "/bin/zsh",
        reservation_bytes: "0",
        cpu_slots: 1,
        enforcement: "observe",
        root_exited: false,
        cancel_requested: false,
        connection: "attached",
        relief: { kind: "NONE" },
        protected: false,
        guard: kind === "SUSPENDED"
          ? { kind: "SUSPENDED", since_ms: "1", reason: "cpu_limit", manual: false, partial: false }
          : { kind: "NONE" },
      })) as unknown as ReturnType<typeof useWorkbenchStore.getState>["workloads"],
    });
    useWorkbenchStore.setState({ host: sample(50) });
    return renderToStaticMarkup(<ResourceStrip />);
  }

  it("가드가 정지한 작업 수를 큐 토글에 경고색으로 적는다", () => {
    const html = htmlWithGuard(["SUSPENDED", "NONE", "SUSPENDED"]);
    expect(html).toContain("strip-suspended");
    expect(html).toContain(t("monitor.strip.suspended", { n: 2 }));
    expect(html).toContain(t("monitor.strip.suspendedTitle", { n: 2 }));
  });

  it("정지가 없으면 배지를 그리지 않는다", () => {
    const html = htmlWithGuard(["NONE"]);
    expect(html).not.toContain("strip-suspended");
    // 구 데몬처럼 guard 필드 자체가 없어도 세지 않는다.
    const htmlLegacy = (() => {
      const workloads = [
        { workload_id: "w0", session_id: "s0", mode: "shell", state: "RUNNING" },
      ] as unknown as ReturnType<typeof useWorkbenchStore.getState>["workloads"];
      useWorkbenchStore.setState({ workloads });
      return renderToStaticMarkup(<ResourceStrip />);
    })();
    expect(htmlLegacy).not.toContain("strip-suspended");
  });
});

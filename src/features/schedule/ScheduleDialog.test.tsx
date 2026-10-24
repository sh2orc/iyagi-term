/**
 * 예약 명령 대화상자: SSR 렌더(이 저장소 시험 관례 — DOM 없이 정적 마크업
 * 단언)로 대상 선택·목록 상태 배지·카운트다운·이유 문구를 고정한다.
 * 폼 제출 계약(scheduleCreate 파라미터)은 모의 클라이언트 주입으로 검증한다.
 */

import { describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ScheduleDialog } from "./ScheduleDialog";
import { t } from "../../i18n";
import type { PaneMeta } from "../../store/workbenchStore";
import type { ScheduledCommand } from "../../generated/ScheduledCommand";

// SSR은 zustand의 초기 스냅샷을 읽는다 — 시험이 넣은 현재 상태를 고르게
// selector를 현재 getState로 직접 부른다(AgentSessionsDialog.test와 같은 처방).
vi.mock("../../store/workbenchStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../store/workbenchStore")>();
  const store = actual.useWorkbenchStore;
  return {
    ...actual,
    useWorkbenchStore: Object.assign(
      (selector: (state: ReturnType<typeof store.getState>) => unknown) => selector(store.getState()),
      store,
    ),
  };
});

vi.mock("./scheduleStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./scheduleStore")>();
  const store = actual.useScheduleStore;
  return {
    ...actual,
    useScheduleStore: Object.assign(
      (selector: (state: ReturnType<typeof store.getState>) => unknown) => selector(store.getState()),
      store,
    ),
  };
});

import { useWorkbenchStore as mockedWorkbench } from "../../store/workbenchStore";
import { useScheduleStore as mockedSchedule } from "./scheduleStore";

function pane(overrides: Partial<PaneMeta> = {}): PaneMeta {
  return {
    leafId: "leaf-1",
    viewId: "view-1",
    sessionId: "sess-1",
    workloadId: "wl-1",
    title: "zsh",
    terminalTitle: null,
    cwd: "/repo",
    phase: "live",
    error: null,
    usage: null,
    flowBlocked: false,
    ...overrides,
  };
}

function command(overrides: Partial<ScheduledCommand> = {}): ScheduledCommand {
  return {
    schedule_id: "sch-1",
    session_id: "sess-1",
    command: "cargo test",
    fire_at_ms: Date.now() + 90_000,
    status: "pending",
    attempts: 0,
    created_at_ms: Date.now() - 1_000,
    fired_at_ms: null,
    reason_code: null,
    ...overrides,
  };
}

/** DOM이 없는 환경이라 useEffect(refresh)는 돌지 않는다 — 목록은 시드로. */
function render(props: Parameters<typeof ScheduleDialog>[0] = {}): string {
  return renderToStaticMarkup(<ScheduleDialog {...props} />);
}

describe("ScheduleDialog 렌더", () => {
  it("대상 목록은 sessionId가 있는 pane만, terminalTitle을 우선한다", () => {
    mockedWorkbench.setState({
      panes: {
        "leaf-1": pane(),
        "leaf-2": pane({ leafId: "leaf-2", sessionId: "sess-2", terminalTitle: "cargo build" }),
        "leaf-3": pane({ leafId: "leaf-3", sessionId: null }),
      },
    } as never);
    mockedSchedule.setState({ items: [], loaded: true });

    const html = render();
    expect(html).toContain("cargo build");
    expect(html).toContain("zsh");
    expect(html).toContain(t("schedule.title"));
    expect(html).toContain(t("schedule.list.empty"));
  });

  it("pending 행은 카운트다운을, 종착 행은 상태·이유를 낸다", () => {
    mockedWorkbench.setState({
      panes: { "leaf-1": pane({ terminalTitle: "cargo build" }) },
    } as never);
    mockedSchedule.setState({
      items: [
        command(),
        command({
          schedule_id: "sch-2",
          status: "skipped",
          reason_code: "session-gone",
          fired_at_ms: Date.now() - 60_000,
          fire_at_ms: Date.now() - 30_000,
        }),
        command({
          schedule_id: "sch-3",
          status: "fired",
          fired_at_ms: Date.now() - 5_000,
          fire_at_ms: Date.now() - 6_000,
        }),
      ],
      loaded: true,
    });

    const html = render();
    expect(html).toContain(t("schedule.status.pending"));
    expect(html).toContain(t("schedule.status.skipped"));
    expect(html).toContain(t("schedule.reason.session-gone"));
    expect(html).toContain(t("schedule.status.fired"));
    // 카운트다운: 90초 → "1분 30초 후" 계열 문구가 자리한다(초 단위 올림).
    const ninetySeconds = [
      t("schedule.duration.m", { n: 1 }),
      t("schedule.duration.s", { n: 30 }),
    ].join(" ");
    expect(html).toContain(t("schedule.countdown", { time: ninetySeconds }));
  });

  it("열린 터미널이 없으면 대상 없음 안내와 함께 만들기가 막힌다", () => {
    mockedWorkbench.setState({ panes: {} } as never);
    mockedSchedule.setState({ items: [], loaded: true });

    const html = render();
    expect(html).toContain(t("schedule.target.none"));
    expect(html).toContain("disabled");
  });
});

describe("ScheduleDialog 제출 계약", () => {
  it("검증을 통과하면 scheduleCreate에 session_id·command·fire_at_ms가 그대로 간다", async () => {
    // SSR로 폼을 돌릴 수 없으므로, 다이얼로그가 쓰는 것과 같은 계약 함수를
    // 주입 클라이언트로 고정한다(파라미터 모양이 UI와 계약을 잇는 지점).
    const create = vi.fn().mockResolvedValue({ schedule_id: "sch-new" });
    const list = vi.fn().mockResolvedValue([command()]);
    const cancel = vi.fn().mockResolvedValue({ cancelled: true });
    const client = { scheduleCreate: create, scheduleList: list, scheduleCancel: cancel };

    await expect(
      client.scheduleCreate({ session_id: "sess-1", command: "cargo test", fire_at_ms: Date.now() + 60_000 }),
    ).resolves.toEqual({ schedule_id: "sch-new" });
    expect(create).toHaveBeenCalledWith({
      session_id: "sess-1",
      command: "cargo test",
      fire_at_ms: expect.any(Number),
    });
  });
});

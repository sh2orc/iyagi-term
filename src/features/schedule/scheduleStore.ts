/**
 * 예약 명령 스토어 + 이벤트 동기화.
 *
 * 진실 원본은 데몬이다 — 여기는 캐시뿐이고 아무것도 영속화하지 않는다.
 * `startScheduleSync`가 `schedule.fired`를 구독해 목록을 다시 가져오고
 * 토스트·알림 센터에 흘린다(창을 닫았다 열어도 데몬이 이미 실행한 결과는
 * 저널/스크롤백에 있다).
 */
import { create } from "zustand";
import type { DaemonClient } from "../daemon/client";
import type { ScheduledCommand } from "../../generated/ScheduledCommand";
import { flashToast } from "../../store/workbenchStore";
import { useNotificationStore } from "../notifications/notificationStore";
import { maybeDesktopNotify } from "../notifications/notificationStore";
import { t } from "../../i18n";
import { scheduleReasonText } from "./scheduleText";

/** 스토어가 쓰는 클라이언트 계약(시험 주입용 좁힘). */
export interface ScheduleClient extends Pick<DaemonClient, "scheduleList"> {}

interface ScheduleState {
  items: ScheduledCommand[];
  /** 첫 조회 성공 여부 — 구 데몬(메서드 없음)에서도 대화상자는 열린다. */
  loaded: boolean;
  refresh(client: ScheduleClient): Promise<void>;
}

export const useScheduleStore = create<ScheduleState>((set) => ({
  items: [],
  loaded: false,
  refresh: async (client) => {
    try {
      const items = await client.scheduleList();
      set({ items, loaded: true });
    } catch {
      // 구 데몬에는 schedule.* 메서드가 없다(INVALID_ARGUMENT 계열) —
      // 빈 목록으로 조용히 흡수한다(agentSessionList와 같은 예절).
      set({ items: [], loaded: true });
    }
  },
}));

/**
 * `schedule.fired` 구독(Workbench 효과에서 호출). 토스트는 결과 요약만,
 * 상세는 알림 센터 항목으로 — 클릭하면 대상 세션 pane으로 이동한다.
 * 구 데몬처럼 이벤트가 오지 않으면 아무 일도 하지 않는다.
 */
export function startScheduleSync(client: DaemonClient): () => void {
  void useScheduleStore.getState().refresh(client);
  const subscription = client.events.subscribe((event) => {
    if (event.kind !== "schedule.fired") return;
    const payload = event.payload;
    void useScheduleStore.getState().refresh(client);
    const command = payload.command;
    const reason = scheduleReasonText(payload.reason_code, t);
    const title =
      payload.outcome === "fired"
        ? t("schedule.toast.fired", { command })
        : payload.outcome === "failed"
          ? t("schedule.toast.failed", { command })
          : t("schedule.toast.skipped", { command, reason: reason ?? t("schedule.reason.unknown") });
    flashToast(title);
    useNotificationStore.getState().push({
      // 멱등 키: 같은 예약의 같은 결과는 두 번 쌓이지 않는다.
      id: `schedule:${payload.schedule_id}:${payload.outcome}`,
      kind: "schedule",
      title: command,
      detail: reason,
      at: new Date(payload.fired_at_ms).toISOString(),
      sessionId: payload.session_id,
      acknowledged: false,
    });
    maybeDesktopNotify(t("schedule.title"), title);
  });
  return () => subscription.dispose();
}

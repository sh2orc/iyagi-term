/**
 * 네트워크 끊김 자동 continue 이벤트 구독(Workbench 효과에서 연결).
 * 데몬이 감지·회복을 모두 처리하므로 여기는 안내뿐이다: armed는 토스트,
 * fired는 토스트 + 알림 센터(클릭하면 그 터미널로), disarmed는 사유 토스트.
 * 구 데몬에서는 이벤트가 오지 않아 아무 일도 하지 않는다.
 */
import type { DaemonClient, DaemonEvent } from "../daemon/client";
import { flashToast } from "../../store/workbenchStore";
import { maybeDesktopNotify, useNotificationStore } from "../notifications/notificationStore";
import { t } from "../../i18n";

/** disarmed 사유 문구 — 모르는 코드는 원문 그대로(데몬이 새 코드를 보내도). */
export function autoContinueReasonText(reason: string | null | undefined): string {
  if (reason == null || reason === "") return t("autoContinue.reason.unknown");
  switch (reason) {
    case "session-gone":
    case "guard-suspended":
    case "user-input":
    case "write-failed":
      return t(`autoContinue.reason.${reason}`);
    default:
      return reason;
  }
}

export function startAutoContinueSync(client: DaemonClient): () => void {
  const subscription = client.events.subscribe((event: DaemonEvent) => {
    if (event.kind !== "auto_continue.changed") return;
    const payload = event.payload;
    if (payload.state === "armed") {
      flashToast(t("autoContinue.armed"));
      return;
    }
    if (payload.state === "fired") {
      const title = t("autoContinue.fired");
      flashToast(title);
      useNotificationStore.getState().push({
        // 멱등 키: 같은 회복 건이 두 번 쌓이지 않는다.
        id: `auto-continue:${payload.session_id}:${payload.at_ms}`,
        kind: "auto-continue",
        title,
        detail: null,
        at: new Date(payload.at_ms).toISOString(),
        sessionId: payload.session_id,
        acknowledged: false,
      });
      maybeDesktopNotify(t("notifications.kind.autoContinue"), title);
      return;
    }
    // disarmed: 사용자가 볼 가치가 있는 사유만 조용히 알린다(세션 종료처럼
    // 자연스러운 해제는 굳이 말하지 않는다).
    if (payload.reason !== "session-gone") {
      flashToast(
        t("autoContinue.disarmed", { reason: autoContinueReasonText(payload.reason) }),
      );
    }
  });
  return () => subscription.dispose();
}

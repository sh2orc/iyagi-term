/**
 * 알림센터 벨 + 패널(W1-5 최소 버전).
 *
 * - 벨은 읽지 않은 개수만 보여 준다(조용한 신호). 패널을 열면 전부
 *   읽음 처리되고, 항목 클릭은 연결된 pane으로 이동한다(AI 작업 알림은 그 작업 탭을 연다).
 * - CLI hooks 등록은 앱이 사용자 설정을 직접 고치지 않는다(§2.1 스펙).
 *   패널 하단에 붙여넣기용 등록 조각을 제공한다.
 */

import { useI18n } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { interventionKindKey, selectUnreadCount, useNotificationStore, type NotificationItem } from "./notificationStore";
import { listLeaves } from "../terminal/splitTree";
import { openMissionTabAndSync } from "../missions/store";
import { missionNotificationKindKey } from "./missionNotifications";

export function NotificationBell(): JSX.Element | null {
  const { t } = useI18n();
  // 열림 상태는 스토어에 있다 — 메뉴 막대의 "알림"(04-ui §3-2)도 같은 패널을 연다.
  const open = useNotificationStore((s) => s.panelOpen);
  const setPanelOpen = useNotificationStore((s) => s.setPanelOpen);
  const items = useNotificationStore((s) => s.items);
  const unread = selectUnreadCount(items);
  if (items.length === 0 && !open) return null;

  return (
    <div className="notif">
      <button
        type="button"
        className="notif-bell icon-button"
        aria-label={t("notifications.bell")}
        aria-expanded={open}
        // 열면 전부 읽음 처리하고 데스크톱 알림 권한을 묻는다(setPanelOpen) — 벨 클릭은 명시적 상호작용이다.
        onClick={() => setPanelOpen(!open)}
      >
        {/* 벨 아이콘 — 이름은 aria-label이 유지한다(Workbench 상단 바 아이콘 규격과 같다). */}
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <path d="M18 8a6 6 0 0 0-12 0c0 7-3 9-3 9h18s-3-2-3-9" />
          <path d="M13.73 21a2 2 0 0 1-3.46 0" />
        </svg>
        {unread > 0 ? <span className="notif-count">{unread}</span> : null}
      </button>
      {open ? <NotificationPanel onClose={() => setPanelOpen(false)} /> : null}
    </div>
  );
}

function NotificationPanel(props: { onClose: () => void }): JSX.Element {
  const { t } = useI18n();
  const items = useNotificationStore((s) => s.items);
  const setActiveTab = useWorkbenchStore((s) => s.setActiveTab);
  const focusPane = useWorkbenchStore((s) => s.focusPane);

  // AI 작업 알림은 그 작업 탭을 연다(이미 열려 있으면 이동, 상한이면 store가 안내한다).
  const openItem = (item: NotificationItem): void => {
    if (item.kind === "mission" && item.missionId) {
      useNotificationStore.getState().acknowledge(item.id);
      openMissionTabAndSync(item.missionId, item.title);
      props.onClose();
      return;
    }
    focusSession(item.sessionId);
  };

  const focusSession = (sessionId: string | null): void => {
    if (!sessionId) return;
    const state = useWorkbenchStore.getState();
    const pane = Object.values(state.panes).find((p) => p.sessionId === sessionId);
    if (!pane) return;
    const tab = state.tabs.find((t) => t.kind === "terminal" && t.root && listLeaves(t.root).some((leaf) => leaf.id === pane.leafId));
    if (tab) setActiveTab(tab.id);
    focusPane(pane.leafId);
    props.onClose();
  };

  return (
    <div className="notif-panel" role="dialog" aria-label={t("notifications.panel")}>
      <div className="notif-header">
        <strong>{t("notifications.panel")}</strong>
        <button
          type="button"
          className="notif-clear"
          onClick={() => useNotificationStore.getState().clear()}
        >
          {t("notifications.clear")}
        </button>
      </div>
      {items.length === 0 ? (
        <p className="muted">{t("notifications.empty")}</p>
      ) : (
        <ul className="notif-list">
          {items.map((item) => (
            <li key={item.id} className={`notif-item${item.acknowledged ? "" : " unread"}`}>
              <button type="button" onClick={() => openItem(item)} title={item.detail ?? undefined}>
                <span className="notif-kind">
                  {t(
                    item.kind === "intervention"
                      ? interventionKindKey(item.interventionKind ?? "notification")
                      : item.kind === "mission"
                        ? missionNotificationKindKey(item.missionEvent ?? "decision")
                        : "notifications.kind.finished",
                  )}
                </span>
                <span className="notif-title">{item.title}</span>
                <span className="notif-at">{new Date(item.at).toLocaleTimeString()}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
      <p className="notif-hint muted">{t("notifications.hooksHint")}</p>
    </div>
  );
}

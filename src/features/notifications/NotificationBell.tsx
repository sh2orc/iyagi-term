/**
 * 알림센터 벨 + 패널(W1-5 최소 버전).
 *
 * - 벨은 읽지 않은 개수만 보여 준다(조용한 신호). 패널을 열면 전부
 *   읽음 처리되고, 항목 클릭은 연결된 pane으로 이동한다(AI 작업 알림은 그 작업 탭을 연다).
 * - 자원 가드의 자동 일시정지 알림은 그 자리에서 "재개"·"보호"를 고른다 — 작업이 아직
 *   정지돼 있을 때만 버튼을 보인다.
 * - CLI hooks 등록은 앱이 사용자 설정을 직접 고치지 않는다(§2.1 스펙).
 *   패널 하단에 붙여넣기용 등록 조각을 제공한다.
 */

import { useContext } from "react";
import { useI18n } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { ControllerContext } from "../../app/controllerContext";
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
                <span className="notif-kind">{t(notificationKindKey(item))}</span>
                <span className="notif-title">{item.title}</span>
                <span className="notif-at">{new Date(item.at).toLocaleTimeString()}</span>
              </button>
              {item.kind === "resource" && item.workloadId ? (
                <ResourceActions notificationId={item.id} workloadId={item.workloadId} />
              ) : null}
            </li>
          ))}
        </ul>
      )}
      <p className="notif-hint muted">{t("notifications.hooksHint")}</p>
    </div>
  );
}

/** 항목 종류 라벨 키. */
export function notificationKindKey(item: NotificationItem): string {
  switch (item.kind) {
    case "intervention":
      return interventionKindKey(item.interventionKind ?? "notification");
    case "mission":
      return missionNotificationKindKey(item.missionEvent ?? "decision");
    case "resource":
      return "notifications.kind.autoSuspended";
    default:
      return "notifications.kind.finished";
  }
}

/**
 * 자동 일시정지 알림의 동작: "재개"(workload.resume)·"보호"(자동 완화·정지에서
 * 빼고 재개). 작업이 아직 정지돼 있을 때만 보이고, 고르면 그 알림을 읽음으로 한다.
 */
function ResourceActions(props: { notificationId: string; workloadId: string }): JSX.Element | null {
  const { t } = useI18n();
  const controller = useContext(ControllerContext);
  const suspended = useWorkbenchStore((s) => s.workloadById.get(props.workloadId)?.guard?.kind === "SUSPENDED");
  if (!controller || !suspended) return null;
  const done = () => useNotificationStore.getState().acknowledge(props.notificationId);
  return (
    <div className="notif-actions">
      <button
        type="button"
        onClick={() => {
          done();
          void controller.resumeWorkload(props.workloadId);
        }}
      >
        {t("notifications.resource.resume")}
      </button>
      <button
        type="button"
        title={t("notifications.resource.protectTitle")}
        onClick={() => {
          done();
          void controller.protectWorkload(props.workloadId);
        }}
      >
        {t("notifications.resource.protect")}
      </button>
    </div>
  );
}

/**
 * 알림센터 스토어(SOTA_GAP_REVIEW W1-5, 최소 버전).
 *
 * - 항목은 유계(100개)이고 id로 멱등 dedup된다 — hook 재시도·이벤트
 *   재전송이 같은 알림을 두 번 쌓지 않는다.
 * - 조용한 신호 우선(§2.1): 여기 쌓이는 것 자체는 방해하지 않는다.
 *   소리는 없고, 데스크톱 알림은 "창이 숨겨져 있을 때 + 개입 요청"만.
 * - 터미널 출력·환경 변수는 절대 들어오지 않는다(제목·근거 문자열만).
 */

import { create } from "zustand";

export type NotificationKind = "intervention" | "workload-finished" | "mission";

export interface NotificationItem {
  /** 멱등 키(개입: report_id / 완료: workload id + 종료 상태). */
  id: string;
  kind: NotificationKind;
  title: string;
  detail: string | null;
  /** ISO 문자열(데몬 reported_at 또는 로컬 생성 시각). */
  at: string;
  /** 연결할 수 있는 세션(있으면). 클릭 시 해당 pane으로 이동한다. */
  sessionId: string | null;
  /** 개입 종류(permission/question/stop/notification) — 라벨용. */
  interventionKind?: string;
  /** AI 작업 알림(kind "mission")의 작업 id — 클릭하면 그 작업 탭을 연다. */
  missionId?: string;
  /** AI 작업 알림의 사건 종류(결정 필요·확정 대기·실패) — 라벨용. */
  missionEvent?: "decision" | "acceptance" | "failed";
  /** 사용자가 확인했는지(패널 열람·항목 클릭). */
  acknowledged: boolean;
}

const MAX_ITEMS = 100;

interface NotificationState {
  items: NotificationItem[];
  /** 상단 바 벨의 패널이 열려 있는가 — 벨과 메뉴 막대의 "알림"(04-ui §3-2)이 같은 패널을 연다. */
  panelOpen: boolean;
  /** 같은 id면 무시(멱등). 최신이 앞. */
  push(item: NotificationItem): boolean;
  acknowledge(id: string): void;
  acknowledgeAll(): void;
  clear(): void;
  /**
   * 패널 열기/닫기. 열면 전부 읽음 처리하고 데스크톱 알림 권한을 묻는다 — 벨·메뉴 클릭은 명시적
   * 상호작용이다(§2.1: 조용한 신호 우선, 요청 없이 알림을 시도하지 않는다).
   */
  setPanelOpen(open: boolean): void;
}

export const useNotificationStore = create<NotificationState>((set, get) => ({
  items: [],
  panelOpen: false,
  setPanelOpen: (open) => {
    if (open === get().panelOpen) return;
    set({ panelOpen: open });
    if (open) {
      get().acknowledgeAll();
      requestDesktopPermission();
    }
  },
  push: (item) => {
    if (get().items.some((existing) => existing.id === item.id)) return false;
    set((s) => ({ items: [item, ...s.items].slice(0, MAX_ITEMS) }));
    return true;
  },
  acknowledge: (id) =>
    set((s) => ({
      items: s.items.map((item) => (item.id === id ? { ...item, acknowledged: true } : item)),
    })),
  acknowledgeAll: () =>
    set((s) => ({ items: s.items.map((item) => ({ ...item, acknowledged: true })) })),
  clear: () => set({ items: [] }),
}));

export function selectUnreadCount(items: NotificationItem[]): number {
  return items.filter((item) => !item.acknowledged).length;
}

/** 개입 요청(kind별 문구용 라벨 — i18n 키를 반환한다). */
export function interventionKindKey(kind: string): string {
  switch (kind) {
    case "permission":
      return "notifications.kind.permission";
    case "question":
      return "notifications.kind.question";
    case "stop":
      return "notifications.kind.stop";
    default:
      return "notifications.kind.notification";
  }
}

/**
 * 데스크톱 알림 게이트(§2.1: 비포커스 + 개입 요청 조건부, 소리 없음).
 * 권한이 없거나 webview가 지원하지 않으면 조용히 건너뛴다.
 */
/** 벨 클릭 등 명시적 상호작용에서만 부른다 — 권한 프롬프트의 주체가
 * 사용자 행동이어야 하기 때문이다. 미지원 환경은 조용히 건너뛴다. */
export function requestDesktopPermission(): void {
  try {
    const ctor = (globalThis as { Notification?: typeof Notification }).Notification;
    if (!ctor || ctor.permission !== "default") return;
    void ctor.requestPermission().catch(() => undefined);
  } catch {
    // 권한 채널 실패는 알림센터 항목으로 충분하다.
  }
}

export function maybeDesktopNotify(title: string, body: string): void {
  try {
    if (typeof document !== "undefined" && document.hidden === false) return;
    const ctor = (globalThis as { Notification?: typeof Notification }).Notification;
    if (!ctor || ctor.permission !== "granted") return;
    new ctor(title, { body, silent: true });
  } catch {
    // 알림 채널 실패는 알림센터 항목으로 충분하다.
  }
}

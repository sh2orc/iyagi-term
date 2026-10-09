/**
 * 터미널 pane 컨텍스트 메뉴 모델(오른쪽 클릭).
 *
 * 무엇을 보일지만 정하는 순수 함수다 — store·controller를 모르고, 렌더링과
 * 키보드 조작은 app/ContextMenu, 실행은 pane 쪽 glue가 맡는다. 항목 id는
 * PaneMenuAction 그대로다.
 *
 * 구획: 링크(링크 위에서만) · 클립보드 · 보기(찾기·지우기·글꼴 크기) ·
 * 배치(분할·동시 입력) · 탭 재배치(분리·이동, 헤더 ⋯ 메뉴와 같은 항목) ·
 * 정보 복사(경로·재개 명령, 이 폴더에서 AI 작업) · 세션(다시 시작·닫기).
 * 빈 구획은 빼고 구획 사이에만 구분선을 넣는다.
 */

import type { ContextMenuEntry, ContextMenuItem, ContextMenuRow } from "../../app/contextMenuTypes";
import type { PanePhase } from "../monitor/statusStrings";
import type { MissionEntryState } from "../missions/capability";
import { t } from "../../i18n";
import { shortcutHint, type Platform, type ShortcutAction, type ShortcutOverrides } from "./shortcuts";
import { MAX_FONT_SIZE, MIN_FONT_SIZE } from "./zoom";

export type PaneMenuAction =
  | "open-link"
  | "copy-link"
  | "copy"
  | "paste"
  | "select-all"
  | "find"
  | "clear"
  | "zoom-out"
  | "zoom-reset"
  | "zoom-in"
  | "bg-color"
  | "bg-reset"
  | "split-row"
  | "split-column"
  | "broadcast"
  | "detach"
  | "move-to"
  | "copy-cwd"
  | "copy-resume"
  | "new-mission-here"
  | "restart"
  | "close";

export const PANE_MENU_ACTIONS: readonly PaneMenuAction[] = [
  "open-link",
  "copy-link",
  "copy",
  "paste",
  "select-all",
  "find",
  "clear",
  "zoom-out",
  "zoom-reset",
  "zoom-in",
  "bg-color",
  "bg-reset",
  "split-row",
  "split-column",
  "broadcast",
  "detach",
  "move-to",
  "copy-cwd",
  "copy-resume",
  "new-mission-here",
  "restart",
  "close",
];

const ACTION_SET: ReadonlySet<string> = new Set(PANE_MENU_ACTIONS);

/** 렌더러가 돌려준 id가 pane 동작인가(구분선·줄 id는 아니다). */
export function isPaneMenuAction(id: string): id is PaneMenuAction {
  return ACTION_SET.has(id);
}

/**
 * 오른쪽 클릭의 주인.
 *
 * 앱(TUI)이 마우스 보고를 켜 두어도 메뉴가 기본이다 — Claude Code·OpenCode는
 * 실행 내내 마우스 보고를 켜 두므로, "Shift를 눌러야 터미널 UI" 관례를 따르면
 * 에이전트 pane에서 메뉴가 사실상 사라진다. 그런 pane에서는 Shift+오른쪽
 * 클릭만 앱으로 보낸다. 마우스 보고가 꺼진 pane은 Shift와 무관하게 메뉴다.
 */
export function rightClickRouting(mouseTracking: boolean, shiftKey: boolean): "menu" | "app" {
  return mouseTracking && shiftKey ? "app" : "menu";
}

export interface PaneMenuContext {
  platform: Platform;
  overrides?: ShortcutOverrides;
  phase: PanePhase;
  hasSelection: boolean;
  /** 오른쪽 클릭 위치의 http/https 링크 — 없으면 null. */
  link: string | null;
  cwd: string | null;
  /** cwd가 속한 git 최상위 경로 — 없으면 null(새 AI 작업은 cwd를 쓴다). */
  project: string | null;
  /**
   * `이 폴더에서 AI 작업…`이 보일 모습(missionEntryState) — 프로덕션 빌드에서 프로토콜이
   * 없으면 숨기고, 쓸 수 없으면 사유(reasonKey)와 함께 흐리게 둔다.
   */
  missionEntry: Pick<MissionEntryState, "hidden" | "enabled" | "reasonKey">;
  broadcast: boolean;
  /** 이 탭에 pane을 더 둘 수 있는가(상한). 공간 부족은 실행 때 알린다. */
  canSplit: boolean;
  /** 이 pane이 같은 탭의 다른 pane과 함께 있는가 — 혼자면 이미 탭 하나를 쓰고 있다. */
  canDetach: boolean;
  /** 옮겨 갈 다른 터미널 탭이 있는가. */
  canMoveToTab: boolean;
  /** 이 pane의 현재 글꼴 크기. */
  fontSize: number;
  /** 사용자 기본 글꼴 크기(되돌리기 목표). */
  baseFontSize: number;
  /** 이 pane에 사용자가 지정한 배경색(없으면 null) — 초기화 항목 표시용. */
  backgroundColor: string | null;
  /** 에이전트 재개 명령(예: `claude --resume <id>`) — 없으면 null. */
  resumeCommand: string | null;
}

type Section = Array<ContextMenuItem | ContextMenuRow>;
type ItemExtra = Omit<ContextMenuItem, "kind" | "id" | "label">;

export function buildPaneMenu(ctx: PaneMenuContext): ContextMenuEntry[] {
  const hint = (action: ShortcutAction) => shortcutHint(action, ctx.platform, ctx.overrides);
  const item = (id: PaneMenuAction, key: string, extra: ItemExtra = {}): ContextMenuItem => ({
    kind: "item",
    id,
    label: t(`terminal.contextMenu.${key}`),
    ...extra,
  });

  const sections: Section[] = [];

  // 링크 위에서 연 메뉴는 링크가 가장 가까운 의도다 — 맨 위에 둔다.
  if (ctx.link) {
    sections.push([
      item("open-link", "openLink", { detail: ctx.link }),
      item("copy-link", "copyLink", { detail: ctx.link }),
    ]);
  }

  sections.push([
    item("copy", "copy", { shortcut: hint("copy"), disabled: !ctx.hasSelection }),
    // 재생·종료 중인 pane에 넣은 입력은 어디에도 가지 않는다.
    item("paste", "paste", { shortcut: hint("paste"), disabled: ctx.phase !== "live" }),
    item("select-all", "selectAll"),
  ]);

  const percent = ctx.baseFontSize > 0 ? Math.round((ctx.fontSize / ctx.baseFontSize) * 100) : 100;
  sections.push([
    item("find", "find", { shortcut: hint("search") }),
    // 시작·재생 중에 지우면 곧바로 재생 출력이 다시 채운다.
    item("clear", "clear", {
      disabled: ctx.phase === "starting" || ctx.phase === "replaying",
      detail: t("terminal.contextMenu.clearDetail"),
    }),
    {
      kind: "row",
      id: "font-size",
      label: t("terminal.contextMenu.fontSize", { percent }),
      items: [
        {
          kind: "item",
          id: "zoom-out",
          label: "−",
          ariaLabel: t("terminal.contextMenu.zoomOut"),
          shortcut: hint("zoom-out"),
          disabled: ctx.fontSize <= MIN_FONT_SIZE,
        },
        item("zoom-reset", "zoomReset", {
          shortcut: hint("zoom-reset"),
          disabled: ctx.fontSize === ctx.baseFontSize,
        }),
        {
          kind: "item",
          id: "zoom-in",
          label: "+",
          ariaLabel: t("terminal.contextMenu.zoomIn"),
          shortcut: hint("zoom-in"),
          disabled: ctx.fontSize >= MAX_FONT_SIZE,
        },
      ],
    },
    // 창마다 배경색(paneBackground.ts): 지정은 OS 색 고르기, 초기화는
    // 테마/에이전트 tint로 되돌린다.
    item("bg-color", "bgColor"),
    item("bg-reset", "bgReset", { disabled: !ctx.backgroundColor }),
  ]);

  sections.push([
    item("split-row", "splitRow", { shortcut: hint("split-row"), disabled: !ctx.canSplit }),
    item("split-column", "splitColumn", { shortcut: hint("split-column"), disabled: !ctx.canSplit }),
    item("broadcast", "broadcast", { shortcut: hint("broadcast-toggle"), checked: ctx.broadcast }),
  ]);

  // 탭 재배치(04-ui §2-5) — 헤더 ⋯ 메뉴와 같은 두 항목. 할 수 없는 자리도
  // 숨기지 않고 흐리게 남긴다(메뉴 모양은 상황에 따라 바뀌지 않는다).
  sections.push([
    item("detach", "detach", { disabled: !ctx.canDetach }),
    item("move-to", "moveTo", { disabled: !ctx.canMoveToTab }),
  ]);

  const info: Section = [];
  if (ctx.cwd) info.push(item("copy-cwd", "copyCwd", { detail: ctx.cwd }));
  if (ctx.resumeCommand) info.push(item("copy-resume", "copyResume", { detail: ctx.resumeCommand }));
  // 이 pane의 저장소로 새 AI 작업 대화상자를 연다. 경로가 없으면 채울 것이 없어 숨기고,
  // 프로덕션 빌드에서 데몬이 프로토콜을 선언하지 않았으면 숨긴다. 그 밖에 쓸 수 없으면
  // 흐리게 두고 툴팁에 사유를 싣는다(`+` 메뉴·팔레트와 같은 판정·사유 — 데몬이 더 새로우면 앱 업데이트).
  if (ctx.cwd && !ctx.missionEntry.hidden) {
    const { enabled, reasonKey } = ctx.missionEntry;
    info.push(
      item("new-mission-here", "newMissionHere", {
        disabled: !enabled,
        detail: enabled || !reasonKey ? ctx.project || ctx.cwd : t(reasonKey),
      }),
    );
  }
  sections.push(info);

  const session: Section = [];
  if (ctx.phase === "exited" || ctx.phase === "failed") session.push(item("restart", "restart"));
  session.push(item("close", "close", { shortcut: hint("close-pane"), danger: true }));
  sections.push(session);

  return joinSections(sections);
}

/** 빈 구획은 빼고, 구획 사이에만 구분선(sep-1, sep-2, …)을 넣는다. */
function joinSections(sections: Section[]): ContextMenuEntry[] {
  const entries: ContextMenuEntry[] = [];
  let separators = 0;
  for (const section of sections) {
    if (section.length === 0) continue;
    if (entries.length > 0) {
      separators += 1;
      entries.push({ kind: "separator", id: `sep-${separators}` });
    }
    entries.push(...section);
  }
  return entries;
}

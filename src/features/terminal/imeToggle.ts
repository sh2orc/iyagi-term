/**
 * Shift+Space 네이티브(CJK)↔라틴 강제 전환 — iyagi 고유 기능의 앱 쪽 절반.
 *
 * 웹뷰는 OS 입력기를 바꿀 수 없으니 Rust 커맨드(`ime_toggle`,
 * src-tauri/src/bridge/ime.rs)가 실제 전환을 맡고, 여기서는
 * - Shift+Space keydown을 xterm보다 먼저 가로채(PTY에 공백이 가지 않게),
 * - 설정(`imeToggle`: auto/on/off)과 OS 가용성(`ime_state`)으로 켜짐을 정하고,
 * - 전환 결과(실제 OS 상태)를 캐럿 옆 풍선으로 보여 준다.
 *
 * 대상은 한국어 고정이 아니라 사용자가 켜 둔 CJK 입력 소스(한국어·중국어·
 * 일본어)와 라틴 자판이다. 브라우저 미리보기(모의 데몬)에는 Tauri IPC가
 * 없으므로 항상 꺼진다. 모르는 상태는 추측하지 않는다: OS가 `native: null`을
 * 주면 "?"를 띄운다.
 */

import { isTauri, tauriIpcAdapter, type IpcAdapter } from "../bridge/ipc";
import { usePreferences, type ImeToggleMode } from "../../store/preferences";

/** 세션의 CJK 계열 — 풍선 글리프와 한국어 전용 경로(자모 복원·CapsLock 풍선) 게이팅. */
export type ImeFamily = "ko" | "zh" | "ja";

/** `bridge/ime.rs`의 `ImeState`(camelCase). */
export interface ImeState {
  available: boolean;
  native: boolean | null;
  family: ImeFamily | null;
  sourceId: string | null;
  reason: string | null;
}

export type ImeHudState = ImeState | { error: string };

type ChordEvent = Pick<
  KeyboardEvent,
  "type" | "code" | "shiftKey" | "ctrlKey" | "altKey" | "metaKey" | "isComposing" | "repeat"
>;

/** 정확히 Shift+Space(다른 수식키 없음, 조합 중 아님, 반복 아님)인 keydown. */
export function isImeToggleChord(event: ChordEvent): boolean {
  return (
    event.type === "keydown" &&
    event.code === "Space" &&
    event.shiftKey &&
    !event.ctrlKey &&
    !event.altKey &&
    !event.metaKey &&
    !event.isComposing &&
    !event.repeat
  );
}

export interface ImeToggleDeps {
  /** null = Tauri 밖(브라우저 미리보기): 전환 불가. */
  ipc: IpcAdapter | null;
  getMode: () => ImeToggleMode;
  showHud: (terminal: unknown, state: ImeHudState) => void;
}

export interface ImeToggle {
  /** 설정과 OS 가용성으로 지금 코드가 켜져 있는지. */
  enabled(): boolean;
  /**
   * keydown 처리. 코드를 소비했으면 true — 호출자는 xterm에 넘기지 않는다.
   * `onState`는 HUD와 같은 전환 결과(OS가 알려 준 실제 상태 또는 실패)를 받는다.
   */
  handleKeydown(
    event: KeyboardEvent,
    terminal: unknown,
    onState?: (state: ImeHudState) => void,
  ): boolean;
  /** OS 가용성·계열 재조회(auto 모드용). 실패는 "불가"로 캐시한다. */
  refresh(): Promise<ImeState | null>;
  /** 마지막으로 알려진 OS 가용성(null = 아직 모름). */
  availability(): boolean | null;
  /** 마지막으로 알려진 세션 계열(null = 모름 — 호출자는 한국어 레거시 경로). */
  family(): ImeFamily | null;
}

export function createImeToggle(deps: ImeToggleDeps): ImeToggle {
  let available: boolean | null = deps.ipc ? null : false;
  let family: ImeFamily | null = null;
  let refreshing: Promise<ImeState | null> | null = null;
  let inFlight = false;

  const refresh = (): Promise<ImeState | null> => {
    if (!deps.ipc) {
      available = false;
      return Promise.resolve(null);
    }
    if (refreshing) return refreshing;
    const ipc = deps.ipc;
    refreshing = ipc
      .invoke<ImeState>("ime_state")
      .then((state) => {
        available = state.available;
        family = state.family;
        return state;
      })
      .catch(() => {
        available = false;
        return null;
      })
      .finally(() => {
        refreshing = null;
      });
    return refreshing;
  };

  const enabled = (): boolean => {
    const mode = deps.getMode();
    if (mode === "off") return false;
    if (mode === "on") return deps.ipc !== null;
    if (available === null) {
      // auto인데 아직 모른다: 조회를 걸어 두고 이번 키는 그대로 둔다.
      void refresh();
      return false;
    }
    return available;
  };

  const handleKeydown = (
    event: KeyboardEvent,
    terminal: unknown,
    onState?: (state: ImeHudState) => void,
  ): boolean => {
    if (!isImeToggleChord(event) || !enabled()) return false;
    event.preventDefault();
    event.stopPropagation();
    if (inFlight || !deps.ipc) return true; // 단일 진행: 연타는 삼킨다
    inFlight = true;
    const report = (state: ImeHudState): void => {
      deps.showHud(terminal, state);
      try {
        onState?.(state);
      } catch {
        // 호출자 콜백의 실패가 전환 결과(HUD)를 실패로 뒤집지 않게 한다.
      }
    };
    deps.ipc
      .invoke<ImeState>("ime_toggle")
      .then((state) => {
        available = state.available;
        family = state.family;
        report(state);
      })
      .catch((error: unknown) => {
        const message =
          error && typeof error === "object" && "message" in error
            ? String((error as { message: unknown }).message)
            : String(error);
        report({ error: message });
      })
      .finally(() => {
        inFlight = false;
      });
    return true;
  };

  return {
    enabled,
    handleKeydown,
    refresh,
    availability: () => available,
    family: () => family,
  };
}

type TerminalInternals = {
  element?: HTMLElement | null;
  buffer?: { active?: { cursorX: number; cursorY: number } };
  _core?: {
    _renderService?: { dimensions?: { css?: { cell?: { width: number; height: number } } } };
  };
};

/** 계열별 네이티브 글리프 — macOS 입력 메뉴 표기 관행. 라틴은 항상 "A". */
const NATIVE_GLYPH: Record<ImeFamily, string> = { ko: "한", zh: "中", ja: "あ" };

/**
 * 캐럿 옆 전환 풍선 — xtermSetup의 CapsLock 풍선과 같은 CSS(.ime-mode-balloon)를
 * 쓴다. 실제 OS 상태를 그리며, 모르면 "?", 실패면 "전환 ✕"(툴팁에 이유).
 */
export function showImeBalloon(terminal: unknown, state: ImeHudState): void {
  if (typeof document === "undefined") return;
  const term = terminal as TerminalInternals;
  const parent = term.element?.querySelector<HTMLElement>(".xterm-helpers") ?? term.element ?? null;
  if (!parent) return;
  const failed = "error" in state ? state.error : state.available ? null : state.reason ?? "unavailable";
  const native = "error" in state ? null : state.native;
  // 계열을 모르면 한국어 글리프(기존 동작) — 다른 계열을 추측해 그리지 않는다.
  const glyph = ("error" in state ? null : state.family) ?? "ko";
  parent.querySelector(".ime-mode-balloon")?.remove();
  const balloon = document.createElement("div");
  balloon.className = `ime-mode-balloon ime-mode-${native === true ? glyph : "en"}`;
  balloon.textContent =
    failed !== null ? "전환 ✕" : native === true ? NATIVE_GLYPH[glyph] : native === false ? "A" : "?";
  balloon.title = failed ?? ("error" in state ? "" : state.reason ?? "");
  const cell = term._core?._renderService?.dimensions?.css?.cell;
  const cursor = term.buffer?.active;
  if (cell && cursor) {
    balloon.style.left = `${Math.max(cursor.cursorX * cell.width - 4, 0)}px`;
    balloon.style.top = `${cursor.cursorY * cell.height}px`;
  }
  balloon.addEventListener("animationend", () => balloon.remove());
  parent.appendChild(balloon);
}

let singleton: ImeToggle | null = null;

/** 앱 전역 인스턴스(설정 스토어 + Tauri IPC). 미리보기에선 항상 꺼짐. */
export function defaultImeToggle(): ImeToggle {
  if (!singleton) {
    singleton = createImeToggle({
      ipc: isTauri() ? tauriIpcAdapter : null,
      getMode: () => usePreferences.getState().imeToggle,
      showHud: showImeBalloon,
    });
    void singleton.refresh();
  }
  return singleton;
}

/** 시험용: 전역 인스턴스를 버린다. */
export function resetImeToggleForTests(): void {
  singleton = null;
}

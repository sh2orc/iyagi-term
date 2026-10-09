/**
 * 앱 종료 포트(Tauri ↔ 프론트).
 *
 * 데몬은 앱과 분리돼 있어 앱을 끝내도 터미널은 살아남는다. 그래서 종료는
 * "터미널을 두고 창만 끝낼지, 터미널도 함께 끝낼지"를 먼저 묻는 흐름이다
 * (src-tauri/src/quit.rs와 짝):
 *
 *  Rust `ExitRequested`(트레이 종료·마지막 창 파괴) ─emit→ `iyagi://quit-requested`
 *    → 프론트 `ack()`(워치독 해제) → 대화상자 → `exit()` 또는 `cancel()`.
 *  macOS 앱 메뉴 Quit(Cmd+Q)와 팔레트는 프론트에서 바로 같은 대화상자를
 *  띄운다(nativeMenu.ts가 메뉴 막대를 만들 때 Quit을 시스템 항목 대신 앱 항목으로 둔다).
 *
 * 브라우저 미리보기·node 시험에는 Tauri가 없으므로 포트는 주입식이다.
 */

import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { IpcAdapter } from "../bridge/ipc";
import { tauriIpcAdapter } from "../bridge/ipc";

/** src-tauri/src/quit.rs `QUIT_REQUESTED_EVENT`와 동일. */
export const QUIT_REQUESTED_EVENT = "iyagi://quit-requested";

export interface QuitPort {
  /** 종료 요청을 받았다(대화상자를 띄우거나 바로 결정한다) — Rust 워치독 해제. */
  ack(): Promise<void>;
  /** 최종 종료. 터미널 종료 여부는 호출 전에 데몬에 반영돼 있어야 한다. */
  exit(): Promise<void>;
  /** 사용자가 취소했다. */
  cancel(): Promise<void>;
  /**
   * 대화상자를 띄우기 전에 창을 보이게 한다(트레이에 숨어 있던 경우).
   * 묻지 않고 바로 끝나는 경로에서는 부르지 않아 창이 깜빡이지 않는다.
   */
  reveal(): Promise<void>;
}

export function tauriQuitPort(ipc: IpcAdapter = tauriIpcAdapter): QuitPort {
  const call = (command: string): Promise<void> =>
    ipc.invoke<void>(command).catch(() => undefined);
  return {
    ack: () => call("app_quit_ack"),
    exit: () => call("app_quit"),
    cancel: () => call("app_quit_cancel"),
    reveal: async () => {
      try {
        // theme.ts와 같은 이유로 동적 import — node 시험에서 창 모듈을 싣지 않는다.
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        const window = getCurrentWindow();
        await window.show();
        await window.setFocus();
      } catch {
        // 창이 없으면(브라우저) 대화상자만으로 충분하다.
      }
    },
  };
}

export type QuitListener = (event: string, handler: () => void) => Promise<UnlistenFn>;

/**
 * Rust가 보낸 종료 요청을 받는다. 반환 해제 함수는 언마운트에서 부른다
 * (StrictMode의 즉시 재마운트에서도 리스너가 두 번 남지 않게).
 */
export function listenQuitRequested(
  handler: () => void,
  listenImpl: QuitListener = (event, cb) => listen(event, () => cb()),
): Promise<UnlistenFn> {
  return listenImpl(QUIT_REQUESTED_EVENT, handler);
}

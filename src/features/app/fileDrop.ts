/**
 * 창에 파일을 끌어다 놓는 사건(Tauri `tauri://drag-drop`).
 *
 * `dragDropEnabled`가 켜진 창(기본값)에서는 웹뷰가 HTML5 dragover/drop을
 * 받지 못하고 네이티브 쪽이 경로와 좌표를 준다. 좌표는 물리 픽셀이다
 * (terminal/dropPaste.ts `toCssPoint`).
 *
 * quit.ts와 같은 이유로 @tauri-apps/api는 동적 import한다 — node 시험이
 * 창 모듈을 싣지 않게.
 */

export type FileDropEvent =
  | { kind: "over"; position: { x: number; y: number } }
  | { kind: "drop"; position: { x: number; y: number }; paths: string[] }
  | { kind: "leave" };

export type FileDropListener = (handler: (event: FileDropEvent) => void) => Promise<() => void>;

export const listenFileDrop: FileDropListener = async (handler) => {
  const { getCurrentWebview } = await import("@tauri-apps/api/webview");
  return getCurrentWebview().onDragDropEvent((event) => {
    const payload = event.payload;
    switch (payload.type) {
      // enter와 over는 같게 다룬다 — 둘 다 "지금 이 자리 위에 있다"는 말이다.
      case "enter":
      case "over":
        handler({ kind: "over", position: payload.position });
        break;
      case "drop":
        handler({ kind: "drop", position: payload.position, paths: payload.paths });
        break;
      default:
        handler({ kind: "leave" });
    }
  });
};

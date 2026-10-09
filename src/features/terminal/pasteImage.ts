/**
 * 클립보드 그림을 임시 파일로 떨구는 포트(Tauri ↔ 프론트).
 *
 * 웹뷰는 파일을 쓸 수 없으므로 앱 프로세스가 대신 쓴다(bridge/paste_image.rs).
 * 브라우저 미리보기·node 시험에는 Tauri가 없으므로 주입식이며, 포트가 없으면
 * 그림 붙여넣기는 조용히 건너뛰고 글 경로만 남는다.
 */

import type { IpcAdapter } from "../bridge/ipc";
import { tauriIpcAdapter } from "../bridge/ipc";
import { bytesToBase64, imageExtension } from "./clipboardImage";

export interface PasteImagePort {
  /** 임시 파일로 저장하고 절대 경로를 돌려준다. 실패는 reject. */
  save(bytes: Uint8Array, mime: string): Promise<string>;
}

export function tauriPasteImagePort(ipc: IpcAdapter = tauriIpcAdapter): PasteImagePort {
  return {
    save: async (bytes, mime) => {
      const ext = imageExtension(mime);
      // 형식 판정은 보내기 전에 끝낸다 — Rust 쪽 허용 목록과 어긋나면 거기서
      // 거절되지만, 여기서 걸러야 큰 base64를 헛되이 만들지 않는다.
      if (ext === null) throw new Error(`unsupported image type: ${mime}`);
      return ipc.invoke<string>("paste_save_image", { data: bytesToBase64(bytes), ext });
    },
  };
}

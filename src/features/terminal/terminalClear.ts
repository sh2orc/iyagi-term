/**
 * 화면 지우기(컨텍스트 메뉴): 이 창에서 지난 출력으로 스크롤해 돌아갈 수 없게
 * 스크롤백까지 지운다. 셸·프로그램에는 아무것도 보내지 않고 터미널 모드(마우스
 * 보고·괄호 붙여넣기·보조 화면)는 그대로 둔다 — reset()은 모드까지 지워 실행 중인
 * TUI를 망가뜨린다.
 *
 * - 일반 화면: xterm clear() — 스크롤백과 화면을 비우고 커서가 있던 줄(프롬프트)만
 *   맨 위에 남긴다.
 * - 보조 화면(vim·OpenCode 같은 전체 화면 앱): 그 화면은 앱이 곧 다시 그리므로 두고,
 *   뒤에 숨은 일반 화면의 스크롤백을 비운다 — 앱이 끝나도 지운 내용이 돌아오지
 *   않게. xterm에는 비활성 버퍼를 지우는 공개 API가 없어 내부 버퍼를 쓰고, 모양이
 *   다르면(버전 차이) 아무것도 하지 않는다.
 */

interface BufferInternals {
  clearAllMarkers?(): void;
  clear?(): void;
  fillViewportRows?(): void;
}

interface ClearableTerminal {
  clear?(): void;
  buffer?: { active?: { type?: string } };
  _core?: { _bufferService?: { buffers?: { normal?: BufferInternals } } };
}

export function clearTerminalHistory(terminal: unknown): void {
  const term = terminal as ClearableTerminal | null | undefined;
  if (!term) return;
  if (term.buffer?.active?.type !== "alternate") {
    term.clear?.();
    return;
  }
  const normal = term._core?._bufferService?.buffers?.normal;
  if (!normal?.clear || !normal.fillViewportRows) return;
  normal.clearAllMarkers?.();
  normal.clear();
  normal.fillViewportRows();
}

/**
 * 붙여넣기 정책 (02-runner.md §6, 04-ui.md §6):
 * - 1 MiB 초과 거절 + 파일 전달 안내.
 * - 여러 줄도 확인 없이 즉시 전달.
 * - bracketed-paste 규약을 유지한다.
 */

export const PASTE_MAX_BYTES = 1048576; // defaults.json paste_bytes

export type PasteDecision =
  | { kind: "allow"; bytes: number }
  | { kind: "reject"; bytes: number };

export function inspectPaste(text: string): PasteDecision {
  const bytes = new TextEncoder().encode(text).length;
  if (bytes > PASTE_MAX_BYTES) return { kind: "reject", bytes };
  return { kind: "allow", bytes };
}

const PASTE_END_MARK = /\x1b\[201~/g;
// 탭·CR·LF를 뺀 C0 제어 문자와 DEL. ESC가 포함되므로 클립보드에 실린
// 어떤 이스케이프 시퀀스도 셸에 "타이핑"되지 않는다.
const CONTROL_CHARS = /[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g;

/**
 * 붙여넣기 페이로드 정화: bracketed-paste 종료 마커(`ESC [201~`)가 본문에
 * 섞이면 괄호가 조기에 닫혀 뒤따르는 텍스트가 입력으로 실행된다 — 마커와
 * 제어 문자를 제거한다(개행·탭·CR은 유지).
 */
export function sanitizePasteText(text: string): string {
  return text.replace(PASTE_END_MARK, "").replace(CONTROL_CHARS, "");
}

/** bracketed paste로 감싼 전송 페이로드(멀티라인 붙여넣기용). */
export function bracketedPaste(text: string): string {
  return `\x1b[200~${sanitizePasteText(text)}\x1b[201~`;
}

/** 단일 라인 붙여넣기는 그대로(개행 문자 정규화 + 제어 문자 제거). */
export function plainPaste(text: string): string {
  return sanitizePasteText(text.replace(/\r\n?/g, "\r"));
}

/** xterm의 paste()와 같은 개행 정규화: LF·CRLF → CR(셸의 Enter 한 번). */
export function normalizePasteNewlines(text: string): string {
  return text.replace(/\r?\n/g, "\r");
}

/** 터미널(xterm 6 `modes`)이 bracketed paste(DECSET 2004)를 켰는가. */
export function bracketedPasteEnabled(terminal: unknown): boolean {
  const modes = (terminal as { modes?: { bracketedPasteMode?: unknown } } | null | undefined)?.modes;
  return modes?.bracketedPasteMode === true;
}

/**
 * 앱 경로 붙여넣기(Ctrl+Shift+V·팔레트) 페이로드 — xterm의 paste()와 같은
 * 규칙: 개행은 CR로, 괄호는 셸이 그 모드를 켰을 때만. cmd.exe·Windows
 * PowerShell 5는 DECSET 2004를 모르므로 마커를 문자 그대로 받고 CRLF는
 * Enter 두 번이 된다.
 */
export function preparePaste(text: string, bracketed: boolean): string {
  const body = normalizePasteNewlines(text);
  return bracketed ? bracketedPaste(body) : sanitizePasteText(body);
}

const PASTE_START = "\x1b[200~";
const PASTE_END = "\x1b[201~";

/** bracketed 페이로드면 본문을, 아니면 null(형제 pane 배분 시 모드 맞춤용). */
export function unwrapBracketedPaste(data: string): string | null {
  if (data.length < PASTE_START.length + PASTE_END.length) return null;
  if (!data.startsWith(PASTE_START) || !data.endsWith(PASTE_END)) return null;
  return data.slice(PASTE_START.length, data.length - PASTE_END.length);
}

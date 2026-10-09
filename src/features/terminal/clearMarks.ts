/**
 * 화면 지우기 지점(세션별).
 *
 * 저널 재생은 세션의 첫 출력부터 화면을 다시 그린다. 지운 뒤 다시 붙거나(재접속)
 * 앱을 다시 켜도 지운 내용이 돌아오지 않게, "몇 번 레코드까지 지웠는지"만 기억해
 * 재생이 그 레코드를 지나는 순간 똑같이 지운다. 출력 내용은 저장하지 않는다.
 * 세션 id는 다시 쓰이지 않으므로 가장 최근 것만 남긴다.
 */

const STORAGE_KEY = "iyagi.terminal-clear-marks.v1";
export const CLEAR_MARKS_RETAINED = 64;

type ClearMark = [sessionId: string, seq: number];

function readMarks(): ClearMark[] {
  try {
    const raw = globalThis.localStorage?.getItem(STORAGE_KEY);
    if (!raw) return [];
    const value: unknown = JSON.parse(raw);
    if (!Array.isArray(value)) return [];
    return value.filter(
      (entry): entry is ClearMark =>
        Array.isArray(entry) &&
        typeof entry[0] === "string" &&
        entry[0].length > 0 &&
        entry[0].length <= 128 &&
        Number.isSafeInteger(entry[1]) &&
        entry[1] > 0,
    );
  } catch {
    return [];
  }
}

function writeMarks(marks: ClearMark[]): void {
  try {
    globalThis.localStorage?.setItem(STORAGE_KEY, JSON.stringify(marks));
  } catch {
    // 저장소가 막혀도 이번 실행 동안은 pipeline이 지점을 기억한다.
  }
}

/** 이 세션을 마지막으로 지운 지점(레코드 seq). 없으면 null. */
export function loadClearMark(sessionId: string): number | null {
  const found = readMarks().find(([id]) => id === sessionId);
  return found ? found[1] : null;
}

/** 지운 지점을 기록한다(같은 세션은 덮어쓰고 가장 최근으로 옮긴다). */
export function saveClearMark(sessionId: string, seq: number): void {
  if (!Number.isSafeInteger(seq) || seq <= 0 || sessionId.length === 0) return;
  const marks = readMarks().filter(([id]) => id !== sessionId);
  marks.push([sessionId, seq]);
  writeMarks(marks.slice(-CLEAR_MARKS_RETAINED));
}

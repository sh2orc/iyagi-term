/**
 * 동기 입력(iTerm2의 broadcast input) 배분 규칙 — 04-ui.md §1.
 *
 * xterm의 onData로 나오는 것이 전부 사람이 친 키는 아니다. 프로그램이
 * 터미널에 물어본 것에 대한 응답(커서 위치 CPR, device attributes, status
 * report)과 마우스 추적 보고도 같은 경로로 나온다. 그것까지 다른 pane에
 * 뿌리면 묻지도 않은 응답이 남의 셸에 명령줄 쓰레기로 들어가고, 클릭 좌표가
 * 명령으로 실행될 수도 있다. vim·htop처럼 터미널에 자주 묻는 프로그램에서는
 * 이 잡음이 끊이지 않는다. 그래서 "보고로 보이는" 시퀀스는 배분에서 뺀다.
 *
 * 한계: 수식 F3(`CSI 1;5R`)과 커서 위치 응답(`CSI 24;80R`)은 바이트만 봐서는
 * 구분되지 않는다. 실제 터미널은 "내가 물어봤는가"라는 문맥으로 가르지만
 * 여기에는 그 문맥이 없다. 동기 입력 중 수식 기능키 하나를 놓치는 쪽이
 * 모든 pane에 CPR 잡음을 뿌리는 쪽보다 낫다고 보고 보고 쪽으로 판정한다.
 */

/** 키보드로는 나올 수 없는 시작 바이트(OSC·DCS·APC·PM·SOS 응답). */
const REPORT_INTRODUCERS = new Set(["]", "P", "_", "^", "X"]);

/** 응답 전용 CSI 최종 바이트: 커서 위치, device attributes, status, window ops. */
const REPORT_FINALS = new Set(["R", "c", "n", "t"]);

/**
 * 터미널이 스스로 만들어 낸 보고인가(= 다른 pane에 배분하면 안 되는가).
 * 사람이 친 키는 여기서 false여야 한다.
 */
export function isTerminalReport(data: string): boolean {
  if (data.length < 2 || data[0] !== "\x1b") return false;
  if (REPORT_INTRODUCERS.has(data[1])) return true;
  if (data[1] !== "[") return false; // SS3(`ESC O …`) 기능키 등은 사용자 입력
  // 마우스 추적: X10 `CSI M …`, SGR `CSI < … M|m`.
  if (data[2] === "M" || data[2] === "<") return true;
  return REPORT_FINALS.has(data[data.length - 1]);
}

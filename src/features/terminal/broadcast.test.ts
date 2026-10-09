/**
 * 동기 입력 배분 필터: 사람이 친 키는 지나가고, 터미널이 스스로 만든
 * 보고는 걸러진다.
 */

import { describe, expect, it } from "vitest";
import { isTerminalReport } from "./broadcast";

describe("isTerminalReport", () => {
  it.each([
    ["plain text", "ls -al"],
    ["한글 입력", "안녕하세요"],
    ["carriage return", "\r"],
    ["bare escape", "\x1b"],
    ["control byte", "\x03"],
    ["arrow key", "\x1b[A"],
    ["ctrl+arrow", "\x1b[1;5C"],
    ["home", "\x1b[H"],
    ["delete", "\x1b[3~"],
    ["SS3 function key", "\x1bOR"],
    ["bracketed paste", "\x1b[200~echo hi\x1b[201~"],
  ])("passes %s through", (_label, data) => {
    expect(isTerminalReport(data)).toBe(false);
  });

  it.each([
    ["cursor position report", "\x1b[24;80R"],
    ["device attributes", "\x1b[?1;2c"],
    ["device status", "\x1b[0n"],
    ["window ops report", "\x1b[8;24;80t"],
    ["SGR mouse press", "\x1b[<0;12;34M"],
    ["SGR mouse release", "\x1b[<0;12;34m"],
    ["X10 mouse", "\x1b[M !!"],
    ["OSC color reply", "\x1b]11;rgb:1e1e/1f1f/2424\x1b\\"],
    ["DCS reply", "\x1bP1$r0m\x1b\\"],
  ])("filters out %s", (_label, data) => {
    expect(isTerminalReport(data)).toBe(true);
  });
});

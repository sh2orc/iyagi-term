/**
 * 화면 지우기의 터미널 쪽: 일반 화면은 xterm clear()로 스크롤백까지, 보조 화면에서는
 * 앱 화면을 두고 뒤에 숨은 일반 화면 기록을 비운다. 내부 모양이 다르면 조용히 넘어간다.
 */

import { describe, expect, it } from "vitest";
import { clearTerminalHistory } from "./terminalClear";

function fakeTerminal(type: "normal" | "alternate" | undefined, withInternals = true) {
  const calls: string[] = [];
  const normal = {
    clearAllMarkers: () => calls.push("normal.clearAllMarkers"),
    clear: () => calls.push("normal.clear"),
    fillViewportRows: () => calls.push("normal.fillViewportRows"),
  };
  const terminal = {
    clear: () => calls.push("clear"),
    buffer: type ? { active: { type } } : undefined,
    _core: withInternals ? { _bufferService: { buffers: { normal } } } : undefined,
  };
  return { terminal, calls };
}

describe("clearTerminalHistory", () => {
  it("clears the normal screen and its scrollback with xterm clear()", () => {
    const { terminal, calls } = fakeTerminal("normal");
    clearTerminalHistory(terminal);
    expect(calls).toEqual(["clear"]);
  });

  it("treats a terminal without buffer info as the normal screen", () => {
    const { terminal, calls } = fakeTerminal(undefined);
    clearTerminalHistory(terminal);
    expect(calls).toEqual(["clear"]);
  });

  it("keeps a full-screen app's screen and wipes the normal screen hidden behind it", () => {
    const { terminal, calls } = fakeTerminal("alternate");
    clearTerminalHistory(terminal);
    expect(calls).toEqual(["normal.clearAllMarkers", "normal.clear", "normal.fillViewportRows"]);
  });

  it("does nothing in the alternate screen when the buffer internals are missing", () => {
    const { terminal, calls } = fakeTerminal("alternate", false);
    expect(() => clearTerminalHistory(terminal)).not.toThrow();
    expect(calls).toEqual([]);
  });

  it("ignores a missing terminal", () => {
    expect(() => clearTerminalHistory(null)).not.toThrow();
    expect(() => clearTerminalHistory(undefined)).not.toThrow();
  });
});

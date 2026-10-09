import { describe, expect, it } from "vitest";
import {
  bracketedPaste,
  bracketedPasteEnabled,
  normalizePasteNewlines,
  plainPaste,
  preparePaste,
  sanitizePasteText,
  unwrapBracketedPaste,
} from "./paste";

describe("paste sanitizing", () => {
  it("bracketed paste 본문에 섞인 종료 마커를 제거해 괄호가 조기에 닫히지 않는다", () => {
    const payload = bracketedPaste("echo hi\x1b[201~\rrm -rf ~\r");
    expect(payload.startsWith("\x1b[200~")).toBe(true);
    expect(payload.endsWith("\x1b[201~")).toBe(true);
    // 마커는 끝의 진짜 종료 하나뿐이다.
    expect(payload.split("\x1b[201~").length).toBe(2);
    expect(payload).toContain("echo hi\rrm -rf ~\r");
  });

  it("단일 라인 붙여넣기도 ESC·C0 제어 문자를 지우고 탭·CR은 남긴다", () => {
    expect(plainPaste("a\x1b[31mb\x00c\td\r\n")).toBe("a[31mbc\td\r");
    expect(sanitizePasteText("x\x7fy\n")).toBe("xy\n");
  });
});

describe("preparePaste — 대상 셸의 모드에 맞춘 페이로드(W3 Windows)", () => {
  it("개행은 xterm의 paste()처럼 CR 하나로 정규화한다(CRLF가 Enter 두 번이 되지 않게)", () => {
    expect(normalizePasteNewlines("a\r\nb\nc\r")).toBe("a\rb\rc\r");
  });

  it("bracketed 모드가 꺼진 셸(cmd·PowerShell 5)에는 마커 없이 보낸다", () => {
    expect(preparePaste("dir\r\ncd ..\n", false)).toBe("dir\rcd ..\r");
  });

  it("bracketed 모드가 켜진 셸에는 정규화한 본문을 감싸 보낸다", () => {
    expect(preparePaste("ls\r\npwd\n", true)).toBe("\x1b[200~ls\rpwd\r\x1b[201~");
  });

  it("터미널의 modes.bracketedPasteMode만 믿는다(없으면 꺼짐)", () => {
    expect(bracketedPasteEnabled({ modes: { bracketedPasteMode: true } })).toBe(true);
    expect(bracketedPasteEnabled({ modes: { bracketedPasteMode: false } })).toBe(false);
    expect(bracketedPasteEnabled({})).toBe(false);
    expect(bracketedPasteEnabled(null)).toBe(false);
  });

  it("unwrapBracketedPaste는 감싼 본문만 돌려주고 그 외는 null", () => {
    expect(unwrapBracketedPaste("\x1b[200~a\rb\x1b[201~")).toBe("a\rb");
    expect(unwrapBracketedPaste("a\rb")).toBeNull();
    expect(unwrapBracketedPaste("\x1b[200~")).toBeNull();
  });
});

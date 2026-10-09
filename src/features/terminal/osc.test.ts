/** OSC 파싱 순수 함수(W1-3/4) — 제목 정리와 cwd 추출의 계약. */
import { describe, expect, it } from "vitest";
import { acceptReportedCwd, cwdFromOsc7, sanitizeOscTitle } from "./osc";

describe("sanitizeOscTitle", () => {
  it("제어 문자를 없애고 트림한다", () => {
    expect(sanitizeOscTitle("  hello\x07 world ")).toBe("hello world");
  });
  it("빈 값·공백뿐 값은 null", () => {
    expect(sanitizeOscTitle("")).toBeNull();
    expect(sanitizeOscTitle("   ")).toBeNull();
    expect(sanitizeOscTitle("\x07\x1b")).toBeNull();
  });
  it("200자 상한", () => {
    expect(sanitizeOscTitle("x".repeat(300))?.length).toBe(200);
  });
});

describe("cwdFromOsc7", () => {
  it("file:// URL에서 경로를 percent-decoding해 돌려준다", () => {
    expect(cwdFromOsc7("file://myhost/Users/me/프로젝트")).toBe("/Users/me/프로젝트");
    expect(cwdFromOsc7("file:///home/user/repo")).toBe("/home/user/repo");
  });
  it("끝 슬래시를 정규화하고 루트는 /로", () => {
    expect(cwdFromOsc7("file:///home/user/")).toBe("/home/user");
    expect(cwdFromOsc7("file:///")).toBe("/");
  });
  it("Windows 드라이브 문자 경로의 앞 슬래시를 덜어낸다", () => {
    expect(cwdFromOsc7("file:///C:/Users/me/repo")).toBe("C:/Users/me/repo");
  });
  it("file:// 가 아니거나 깨진 URL은 null", () => {
    expect(cwdFromOsc7("http://example.com/x")).toBeNull();
    expect(cwdFromOsc7("not a url")).toBeNull();
  });
});

describe("acceptReportedCwd — 다음 pane의 시작 cwd로 써도 되는가(W3 Windows)", () => {
  it("Windows에서는 드라이브/UNC 절대 경로만 받고 구분자를 \\로 통일한다", () => {
    expect(acceptReportedCwd("C:/Users/me/repo", "windows")).toBe("C:\\Users\\me\\repo");
    expect(acceptReportedCwd("D:\\work", "windows")).toBe("D:\\work");
    expect(acceptReportedCwd("\\\\server\\share\\dir", "windows")).toBe("\\\\server\\share\\dir");
  });
  it("Windows에서 WSL·Git Bash의 POSIX 경로는 버린다(데몬이 거절해 pane이 실패하는 대신)", () => {
    expect(acceptReportedCwd("/home/u", "windows")).toBeNull();
    expect(acceptReportedCwd("/mnt/c/Users/u", "windows")).toBeNull();
    expect(acceptReportedCwd("", "windows")).toBeNull();
  });
  it("macOS/Linux에서는 /로 시작하는 경로만 받는다", () => {
    expect(acceptReportedCwd("/home/u", "linux")).toBe("/home/u");
    expect(acceptReportedCwd("/Users/u", "darwin")).toBe("/Users/u");
    expect(acceptReportedCwd("C:\\Users\\u", "darwin")).toBeNull();
    expect(acceptReportedCwd("relative/dir", "linux")).toBeNull();
  });
});

import { describe, expect, it } from "vitest";
import { abbreviateHome } from "./displayPath";

describe("abbreviateHome — 홈 아래 경로만 ~로 줄인다", () => {
  it("홈을 모르면 원문 그대로다", () => {
    expect(abbreviateHome("/Users/x/proj", null)).toBe("/Users/x/proj");
    expect(abbreviateHome("/Users/x/proj", undefined)).toBe("/Users/x/proj");
    expect(abbreviateHome("/Users/x/proj", "")).toBe("/Users/x/proj");
  });

  it("홈 자체는 ~, 그 아래는 ~/…", () => {
    expect(abbreviateHome("/Users/x", "/Users/x")).toBe("~");
    expect(abbreviateHome("/Users/x/proj/a", "/Users/x")).toBe("~/proj/a");
  });

  it("Tauri homeDir()의 끝 구분자를 무시한다", () => {
    expect(abbreviateHome("/Users/x/proj", "/Users/x/")).toBe("~/proj");
    expect(abbreviateHome("/Users/x/", "/Users/x/")).toBe("~/");
  });

  it("형제 경로·홈 밖 경로는 건드리지 않는다", () => {
    expect(abbreviateHome("/Users/xy/proj", "/Users/x")).toBe("/Users/xy/proj");
    expect(abbreviateHome("/opt/proj", "/Users/x")).toBe("/opt/proj");
    expect(abbreviateHome("/home/u/proj", "C:\\Users\\x")).toBe("/home/u/proj");
  });

  it("뿌리는 홈으로 보지 않는다 — 모든 경로가 ~가 되어 버린다", () => {
    expect(abbreviateHome("/opt/proj", "/")).toBe("/opt/proj");
    expect(abbreviateHome("C:\\proj", "C:\\")).toBe("C:\\proj");
  });

  it("Windows 경로는 대소문자·구분자 모양을 가리지 않고, 원문 구분자를 유지한다", () => {
    expect(abbreviateHome("C:\\Users\\x\\proj", "C:\\Users\\x\\")).toBe("~\\proj");
    expect(abbreviateHome("c:\\users\\x\\proj", "C:\\Users\\x")).toBe("~\\proj");
    expect(abbreviateHome("C:/Users/x/proj", "C:\\Users\\x")).toBe("~/proj");
    expect(abbreviateHome("C:\\Users\\xy\\proj", "C:\\Users\\x")).toBe("C:\\Users\\xy\\proj");
  });
});

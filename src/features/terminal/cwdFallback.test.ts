import { describe, expect, it } from "vitest";
import { ancestorDirs, filesystemRoot } from "./cwdFallback";

describe("ancestorDirs: 쓸 수 없는 시작 경로 대신 시도할 상위 경로", () => {
  it("가까운 것부터 올라가되 뿌리와 그 바로 아래는 넣지 않는다", () => {
    expect(ancestorDirs("/Users/me/work/wt/feature/src", "darwin")).toEqual([
      "/Users/me/work/wt/feature",
      "/Users/me/work/wt",
      "/Users/me/work",
      "/Users/me",
    ]);
    expect(ancestorDirs("/Volumes/USB/project", "darwin")).toEqual(["/Volumes/USB"]);
    expect(ancestorDirs("/tmp/x", "linux")).toEqual([]);
    expect(ancestorDirs("/", "linux")).toEqual([]);
  });

  it("끝 구분자·중복 구분자를 견디고, 절대 경로가 아니면 빈 목록이다", () => {
    expect(ancestorDirs("/home/me/a//b/", "linux")).toEqual(["/home/me/a", "/home/me"]);
    expect(ancestorDirs("relative/path", "linux")).toEqual([]);
    expect(ancestorDirs("C:\\Users\\me\\x", "darwin")).toEqual([]);
  });

  it("Windows 드라이브·UNC 경로는 그 뿌리 아래에서만 올라간다", () => {
    expect(ancestorDirs("C:\\Users\\me\\src\\app", "windows")).toEqual(["C:\\Users\\me\\src", "C:\\Users\\me"]);
    expect(ancestorDirs("D:/work/repo/sub", "windows")).toEqual(["D:\\work\\repo"]);
    expect(ancestorDirs("\\\\nas\\share\\team\\proj\\x", "windows")).toEqual(["\\\\nas\\share\\team\\proj"]);
    expect(ancestorDirs("/home/me/x", "windows")).toEqual([]);
  });

  it("filesystemRoot는 경로가 놓인 뿌리다", () => {
    expect(filesystemRoot("/Users/me", "darwin")).toBe("/");
    expect(filesystemRoot("C:\\Users\\me", "windows")).toBe("C:\\");
    expect(filesystemRoot("\\\\nas\\share\\x", "windows")).toBe("\\\\nas\\share");
    expect(filesystemRoot("~/x", "linux")).toBeNull();
  });

  it("Windows 확장 길이 경로(canonicalize 형태)는 접두를 둔 채 같은 규칙을 따른다", () => {
    expect(filesystemRoot("\\\\?\\C:\\Users\\me", "windows")).toBe("\\\\?\\C:\\");
    expect(filesystemRoot("\\\\?\\C:", "windows")).toBe("\\\\?\\C:\\");
    expect(ancestorDirs("\\\\?\\C:\\Users\\me\\src\\app", "windows"))
      .toEqual(["\\\\?\\C:\\Users\\me\\src", "\\\\?\\C:\\Users\\me"]);
    expect(filesystemRoot("\\\\?\\UNC\\nas\\share\\team", "windows")).toBe("\\\\?\\UNC\\nas\\share");
    expect(ancestorDirs("\\\\?\\UNC\\nas\\share\\team\\proj\\x", "windows"))
      .toEqual(["\\\\?\\UNC\\nas\\share\\team\\proj"]);
  });
});

/**
 * 파일 끌어다 놓기: 셸이 한 낱말로 읽는 경로 만들기, 물리 좌표 → pane 되짚기,
 * 끄는 동안의 대상 표시.
 */

import { describe, expect, it } from "vitest";
import { dropPayload, leafIdAtPoint, markDropTarget, quoteDropPath, toCssPoint } from "./dropPaste";

describe("quoteDropPath", () => {
  it("leaves a plain POSIX path bare and quotes anything a shell would reinterpret", () => {
    expect(quoteDropPath("/home/t/shot.png", "linux")).toBe("/home/t/shot.png");
    expect(quoteDropPath("/home/t/my shot.png", "linux")).toBe("'/home/t/my shot.png'");
    expect(quoteDropPath("/home/t/it's.png", "darwin")).toBe(String.raw`'/home/t/it'\''s.png'`);
    // 물결표는 앞에서 확장되므로 한 낱말이어도 감싼다.
    expect(quoteDropPath("/home/t/~draft.png", "linux")).toBe("'/home/t/~draft.png'");
  });

  it("quotes a Windows path only when a space or shell character is in it", () => {
    expect(quoteDropPath(String.raw`C:\tmp\shot.png`, "windows")).toBe(String.raw`C:\tmp\shot.png`);
    expect(quoteDropPath(String.raw`C:\my files\shot.png`, "windows")).toBe(
      String.raw`"C:\my files\shot.png"`,
    );
    // 겹따옴표는 Windows 파일 이름에 올 수 없다 — 섞여 들어오면 떼어 낸다.
    expect(quoteDropPath(String.raw`C:\tmp\a"b.png`, "windows")).toBe(String.raw`C:\tmp\ab.png`);
  });
});

describe("dropPayload", () => {
  it("joins every dropped path with a space and skips empty entries", () => {
    expect(dropPayload(["/a/one.png", "", "/a/two log.txt"], "linux")).toBe(
      "/a/one.png '/a/two log.txt'",
    );
  });

  it("is empty when nothing usable was dropped", () => {
    expect(dropPayload([], "linux")).toBe("");
    expect(dropPayload([""], "windows")).toBe("");
  });
});

describe("toCssPoint", () => {
  it("converts the native physical position to CSS pixels", () => {
    expect(toCssPoint({ x: 400, y: 200 }, 2)).toEqual({ x: 200, y: 100 });
  });

  it("treats a missing or nonsensical ratio as 1", () => {
    expect(toCssPoint({ x: 12, y: 8 }, 0)).toEqual({ x: 12, y: 8 });
    expect(toCssPoint({ x: 12, y: 8 }, Number.NaN)).toEqual({ x: 12, y: 8 });
  });
});

describe("leafIdAtPoint", () => {
  it("walks up from the element under the cursor to the owning pane", () => {
    const pane = document.createElement("section");
    pane.dataset.leafId = "leaf-7";
    // 터미널 안쪽 요소만 대상이다 — pane 안의 입력 칸 위에 떨어뜨린 파일은
    // 터미널이 아닌 그 칸의 몫이다(nativePaste.terminalLeafIdOf와 같은 판정).
    const xterm = document.createElement("div");
    xterm.className = "xterm";
    const inner = document.createElement("canvas");
    xterm.appendChild(inner);
    pane.appendChild(xterm);
    document.body.appendChild(pane);

    const doc = { elementFromPoint: () => inner } as unknown as Document;
    expect(leafIdAtPoint(10, 10, doc)).toBe("leaf-7");
  });

  it("is null outside any terminal pane", () => {
    const outside = { elementFromPoint: () => document.createElement("div") } as unknown as Document;
    expect(leafIdAtPoint(10, 10, outside)).toBeNull();
    const empty = { elementFromPoint: () => null } as unknown as Document;
    expect(leafIdAtPoint(10, 10, empty)).toBeNull();
  });
});

describe("markDropTarget", () => {
  it("marks only the addressed pane and clears every pane on null", () => {
    document.body.innerHTML = "";
    for (const id of ["leaf-a", "leaf-b"]) {
      const pane = document.createElement("section");
      pane.className = "terminal-pane";
      pane.dataset.leafId = id;
      document.body.appendChild(pane);
    }
    const panes = () =>
      Array.from(document.querySelectorAll<HTMLElement>("[data-leaf-id]")).map((pane) =>
        pane.classList.contains("drop-target"),
      );

    markDropTarget("leaf-b", document);
    expect(panes()).toEqual([false, true]);

    markDropTarget("leaf-a", document);
    expect(panes()).toEqual([true, false]);

    markDropTarget(null, document);
    expect(panes()).toEqual([false, false]);
  });
});

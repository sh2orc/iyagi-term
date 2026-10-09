/**
 * CLI hooks 연동 공용 판정 로직(§2.1 동의 계약) — Claude Code·Codex가
 * 공유하는 `classifyHooksStatus`/`manualSnippet`/`addedLines`를 검증한다.
 * `claudeHooksIntegration.test.ts`/`codexHooksIntegration.test.ts`는 각
 * 얇은 어댑터가 올바른 `cli`로 이 로직을 호출하는지만 확인한다.
 */

import { describe, expect, it } from "vitest";
import {
  addedLines,
  classifyHooksStatus,
  manualSnippet,
  type HooksStatus,
} from "./hooksIntegration";

const status = (overrides: Partial<HooksStatus> = {}): HooksStatus => ({
  path: "/home/user/.claude/settings.json",
  exists: true,
  current: "{}\n",
  proposed: null,
  managedEntries: 0,
  expectedEntries: 3,
  hookCommand: "iyagi-termd hook",
  ...overrides,
});

describe("classifyHooksStatus", () => {
  it("비-Tauri면 not-tauri로 unavailable", () => {
    const view = classifyHooksStatus(null, { kind: "not-tauri" });
    expect(view).toEqual({ kind: "unavailable", reason: "not-tauri" });
  });

  it("조회 오류면 메시지와 함께 unavailable", () => {
    const view = classifyHooksStatus(null, { kind: "error", message: "boom" });
    expect(view).toEqual({ kind: "unavailable", reason: "error", message: "boom" });
  });

  it("ready인데 상태가 없으면(아직 로딩 전) unavailable", () => {
    const view = classifyHooksStatus(null, { kind: "ready" });
    expect(view.kind).toBe("unavailable");
  });

  it("managedEntries 0이면 would-register", () => {
    const st = status({ managedEntries: 0, proposed: "…" });
    const view = classifyHooksStatus(st, { kind: "ready" });
    expect(view).toEqual({ kind: "would-register", status: st });
  });

  it("0 < managedEntries < expectedEntries면 partial(부분 등록)", () => {
    const st = status({ managedEntries: 1, expectedEntries: 3, proposed: "…" });
    const view = classifyHooksStatus(st, { kind: "ready" });
    expect(view).toEqual({ kind: "partial", status: st });
  });

  it("managedEntries === expectedEntries면 registered(완전 등록)", () => {
    const st = status({ managedEntries: 3, expectedEntries: 3, proposed: null });
    const view = classifyHooksStatus(st, { kind: "ready" });
    expect(view).toEqual({ kind: "registered", status: st });
  });

  it("managedEntries가 expectedEntries를 넘어도(방어적) registered로 본다", () => {
    const st = status({ managedEntries: 4, expectedEntries: 3 });
    const view = classifyHooksStatus(st, { kind: "ready" });
    expect(view.kind).toBe("registered");
  });
});

describe("manualSnippet", () => {
  it("claude는 Notification·SessionStart·SessionEnd 세 이벤트 모두를 담는다", () => {
    const snippet = manualSnippet("claude", "iyagi-termd hook");
    const parsed = JSON.parse(snippet);
    expect(Object.keys(parsed.hooks).sort()).toEqual([
      "Notification",
      "SessionEnd",
      "SessionStart",
    ]);
    for (const event of ["Notification", "SessionStart", "SessionEnd"]) {
      expect(parsed.hooks[event][0].hooks[0]).toEqual({
        type: "command",
        command: "iyagi-termd hook",
        timeout: 10,
      });
    }
  });

  it("codex는 SessionStart·SessionEnd만 담고 Notification은 없다", () => {
    const snippet = manualSnippet("codex", "iyagi-termd hook --agent codex");
    const parsed = JSON.parse(snippet);
    expect(Object.keys(parsed.hooks).sort()).toEqual(["SessionEnd", "SessionStart"]);
    expect(parsed.hooks.Notification).toBeUndefined();
    expect(parsed.hooks.SessionStart[0].hooks[0].command).toBe(
      "iyagi-termd hook --agent codex",
    );
  });

  it("따옴표가 든 명령을 이중 이스케이프 없이 원문 그대로 담는다", () => {
    const command = 'echo "hi" && iyagi-termd hook';
    const snippet = manualSnippet("claude", command);
    expect(() => JSON.parse(snippet)).not.toThrow();
    // 직접 `\"`로 바꿔 넘기면 JSON.stringify가 한 번 더 새겨 `\\"`가 되고,
    // 붙여넣은 사용자는 실행되지 않는 명령을 얻는다.
    expect(JSON.parse(snippet).hooks.Notification[0].hooks[0].command).toBe(command);
  });

  it("인용된 설치 경로도 원문 그대로 담는다", () => {
    const command = "'/Applications/IYAGI.app/Contents/MacOS/iyagi-termd' hook";
    expect(JSON.parse(manualSnippet("claude", command)).hooks.SessionStart[0].hooks[0].command).toBe(
      command,
    );
  });
});

describe("addedLines", () => {
  it("proposed가 없으면 빈 배열", () => {
    expect(addedLines("claude", null)).toEqual([]);
  });

  it("claude: 세 이벤트 키와 명령 줄만 추출한다", () => {
    const proposed = JSON.stringify(
      {
        model: "opus",
        hooks: {
          Notification: [{ hooks: [{ type: "command", command: "iyagi-termd hook", timeout: 10 }] }],
          SessionStart: [{ hooks: [{ type: "command", command: "iyagi-termd hook", timeout: 10 }] }],
          SessionEnd: [{ hooks: [{ type: "command", command: "iyagi-termd hook", timeout: 10 }] }],
        },
      },
      null,
      2,
    );
    const lines = addedLines("claude", proposed);
    expect(lines.some((l) => l.includes("Notification"))).toBe(true);
    expect(lines.some((l) => l.includes("SessionStart"))).toBe(true);
    expect(lines.some((l) => l.includes("SessionEnd"))).toBe(true);
    expect(lines.some((l) => l.includes("iyagi-termd hook"))).toBe(true);
    // 관계 없는 줄("model": "opus")은 제외된다.
    expect(lines.some((l) => l.includes("opus"))).toBe(false);
  });

  it("인용된 설치 경로 명령 줄도 추출한다(공백 있는 .app 경로)", () => {
    const proposed = JSON.stringify(
      {
        hooks: {
          SessionStart: [
            {
              hooks: [
                {
                  type: "command",
                  command: "'/Applications/IYAGI.app/Contents/MacOS/iyagi-termd' hook",
                  timeout: 10,
                },
              ],
            },
          ],
        },
      },
      null,
      2,
    );
    // 이벤트 키 줄이 아니라 command 줄이 잡혀야 한다(부분 문자열 필터는 놓쳤다).
    expect(addedLines("claude", proposed).some((l) => l.includes("IYAGI.app"))).toBe(true);
  });

  it("Windows .exe 인용 형태의 명령 줄도 추출한다", () => {
    const proposed = JSON.stringify(
      {
        hooks: {
          SessionEnd: [
            {
              hooks: [
                {
                  type: "command",
                  command: '"C:\\Program Files\\IYAGI\\iyagi-termd.exe" hook --agent codex',
                  timeout: 10,
                },
              ],
            },
          ],
        },
      },
      null,
      2,
    );
    expect(addedLines("codex", proposed).some((l) => l.includes("iyagi-termd.exe"))).toBe(true);
  });

  it("codex: SessionStart/SessionEnd만 추출하고 다른 이벤트 키는 무시한다", () => {
    const proposed = JSON.stringify(
      {
        hooks: {
          SessionStart: [{ hooks: [{ type: "command", command: "iyagi-termd hook --agent codex", timeout: 10 }] }],
          SessionEnd: [{ hooks: [{ type: "command", command: "iyagi-termd hook --agent codex", timeout: 10 }] }],
          Stop: [{ hooks: [{ type: "command", command: "notify-send done" }] }],
        },
      },
      null,
      2,
    );
    const lines = addedLines("codex", proposed);
    expect(lines.some((l) => l.includes("SessionStart"))).toBe(true);
    expect(lines.some((l) => l.includes("SessionEnd"))).toBe(true);
    expect(lines.some((l) => l.includes("notify-send"))).toBe(false);
  });
});

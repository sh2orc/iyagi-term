/**
 * 재개 인자의 안전성(04-ui.md §5).
 *
 * `claude --resume <id>` / `codex resume <id>`의 세션 id는 argv로 그대로
 * 들어간다 — 기록이 오염돼 `-`로 시작하는 값이 들어오면 그 CLI가 플래그로
 * 해석한다. 그런 기록은 "재개 가능"으로 취급하지 않고(resumeInfoFrom → null),
 * 인자도 만들지 않는다(resumeArgv → null)는 것을 고정한다.
 */

import { describe, expect, it } from "vitest";
import type { AgentSessionRecord } from "../../generated/AgentSessionRecord";
import { decodeResumeInfo, isSafeSessionId, resumeArgv, resumeCommand, resumeInfoFrom } from "./types";

function record(overrides: Partial<AgentSessionRecord> = {}): AgentSessionRecord {
  return {
    id: "rec-1",
    workload_id: "w-1",
    pty_session_id: "s-1",
    agent: "codex",
    agent_session_id: "agent-session-0001",
    cwd: "/work/iyagi",
    title: "iyagi",
    program: "/opt/bin/codex",
    source: "registry",
    first_seen_at: "2026-09-12T00:00:00Z",
    last_seen_at: "2026-09-12T01:00:00Z",
    ended_at: null,
    end_reason: null,
    active: false,
    ...overrides,
  };
}

describe("isSafeSessionId", () => {
  it.each([
    ["UUID", "0b3f6b1e-7c2a-4f51-9f5c-2f2a9d1b7e10"],
    ["영숫자", "abc123"],
    ["점·밑줄·콜론 섞임", "a.b_c:d-e"],
    ["한 글자", "a"],
    ["128자", "a".repeat(128)],
  ])("%s는 인자로 쓸 수 있다", (_name, id) => {
    expect(isSafeSessionId(id)).toBe(true);
  });

  it.each([
    ["긴 플래그", "--yolo"],
    ["짧은 플래그", "-x"],
    ["현재 디렉터리", "."],
    ["상위 디렉터리", ".."],
    ["숨은 파일 형태", ".hidden"],
    ["빈 값", ""],
    ["공백 포함", "abc def"],
    ["경로 구분자", "a/b"],
    ["셸 메타문자", "a;rm -rf /"],
    ["129자", "a".repeat(129)],
  ])("%s는 거절한다", (_name, id) => {
    expect(isSafeSessionId(id)).toBe(false);
  });
});

describe("resumeInfoFrom", () => {
  it("재개 인자가 확정된 에이전트 + 안전한 id면 정보를 만든다", () => {
    expect(resumeInfoFrom(record())?.agentSessionId).toBe("agent-session-0001");
  });

  it.each([["--yolo"], ["-x"], ["."], [".."]])(
    "id가 %s면 재개 대상이 아니다(목록은 버튼을 잠근다)",
    (id) => {
      expect(resumeInfoFrom(record({ agent_session_id: id }))).toBeNull();
    },
  );

  it("이어서 열기 인자가 없는 에이전트는 그대로 거절한다", () => {
    expect(resumeInfoFrom(record({ agent: "unknown-agent" }))).toBeNull();
  });
});

describe("resumeArgv", () => {
  it.each([["--yolo"], ["-x"], ["."], [".."]])("id가 %s면 인자를 만들지 않는다", (id) => {
    expect(resumeArgv("codex", id, false)).toBeNull();
    expect(resumeArgv("claude", id, true)).toBeNull();
    expect(resumeArgv("opencode", id, true)).toBeNull();
  });

  it("안전한 id는 CLI가 문서화한 순서대로 만든다", () => {
    expect(resumeArgv("codex", "sess-1", false)).toEqual(["resume", "sess-1"]);
    expect(resumeArgv("claude", "sess-1", false)).toEqual(["--resume", "sess-1"]);
  });

  it("OpenCode는 저장된 정확한 ID로 재개하며 권한 플래그를 추가하지 않는다", () => {
    const id = "ses_123abc";
    const info = resumeInfoFrom(record({ agent: "opencode", agent_session_id: id }));
    expect(decodeResumeInfo(info)).toEqual(info);
    expect(resumeCommand("opencode", id)).toBe(`opencode --session ${id}`);
    for (const autonomy of [false, true]) {
      expect(resumeArgv("opencode", id, autonomy)).toEqual(["--session", id]);
    }
  });
});

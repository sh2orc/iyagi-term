/**
 * 셸 명령(ccd/ccg) 설치 판정 — 카드가 어떤 화면을 그릴지 정하는 순수 로직.
 *
 * 여기서 잠그는 계약은 둘이다: ① 사용자가 직접 정의한 ccd/ccg가 있으면
 * 절대 "적용할 수 있는" 화면으로 가지 않는다(conflict), ② 지원 불가
 * 사유는 삼키지 않고 그대로 UI 문구 키까지 전달한다(unsupported).
 */

import { describe, expect, it } from "vitest";
import {
  addedLines,
  classifyShellProfiles,
  isConflictError,
  manualSnippet,
  rcBlock,
  FALLBACK_RC_PATH,
  FALLBACK_SCRIPT_PATH,
  type ShellProfileConflict,
  type ShellProfilesStatus,
} from "./shellProfiles";

const BLOCK = [
  "# >>> Iyagi claude profiles (ccd/ccg) >>>",
  "[ -r '/home/user/.iyagi/shell/iyagi.zsh' ] && source '/home/user/.iyagi/shell/iyagi.zsh'",
  "# <<< Iyagi claude profiles (ccd/ccg) <<<",
].join("\n");

const status = (overrides: Partial<ShellProfilesStatus> = {}): ShellProfilesStatus => ({
  supported: true,
  reason: null,
  shell: "/bin/zsh",
  rcPath: "/home/user/.zshrc",
  rcExists: true,
  installed: false,
  upToDate: false,
  scriptPath: "/home/user/.iyagi/shell/iyagi.zsh",
  scriptExists: false,
  daemonBinary: "/opt/iyagi/iyagi-termd",
  proposedBlock: BLOCK,
  scriptPreview: "ccd() { … }\nccg() { … }\n",
  conflicts: [],
  overriding: false,
  mainModel: "glm-5.3[1m]",
  ...overrides,
});

const conflict = (overrides: Partial<ShellProfileConflict> = {}): ShellProfileConflict => ({
  name: "ccd",
  file: "/home/user/.zshrc",
  line: 42,
  text: "alias ccd='cd ~/code'",
  replaceable: true,
  ...overrides,
});

describe("classifyShellProfiles", () => {
  it("비-Tauri면 not-tauri로 unavailable(수동 안내로 폴백한다)", () => {
    expect(classifyShellProfiles(null, { kind: "not-tauri" })).toEqual({
      kind: "unavailable",
      reason: "not-tauri",
    });
  });

  it("조회 오류면 메시지와 함께 unavailable", () => {
    expect(classifyShellProfiles(null, { kind: "error", message: "boom" })).toEqual({
      kind: "unavailable",
      reason: "error",
      message: "boom",
    });
  });

  it("ready인데 아직 상태가 없으면 unavailable/error", () => {
    const view = classifyShellProfiles(null, { kind: "ready" });
    expect(view.kind).toBe("unavailable");
    expect(view).toMatchObject({ reason: "error" });
  });

  it("supported=false면 사유를 그대로 실어 unsupported로 내린다", () => {
    const st = status({ supported: false, reason: "windows", shell: null });
    expect(classifyShellProfiles(st, { kind: "ready" })).toEqual({
      kind: "unavailable",
      reason: "unsupported",
      unsupported: "windows",
      status: st,
    });
  });

  it("zsh가 아닌 로그인 셸도 사유를 유지한다(문구가 셸 경로를 보여 준다)", () => {
    const st = status({ supported: false, reason: "shell_not_zsh", shell: "/bin/bash" });
    const view = classifyShellProfiles(st, { kind: "ready" });
    expect(view).toMatchObject({ kind: "unavailable", reason: "unsupported", unsupported: "shell_not_zsh" });
    expect(view.kind === "unavailable" ? view.status?.shell : null).toBe("/bin/bash");
  });

  it("데몬 경로를 못 찾은 경우도 지원 불가로 내려 적용 버튼을 열지 않는다", () => {
    const st = status({ supported: false, reason: "daemon_binary_missing", daemonBinary: null });
    expect(classifyShellProfiles(st, { kind: "ready" })).toMatchObject({
      kind: "unavailable",
      reason: "unsupported",
      unsupported: "daemon_binary_missing",
    });
  });

  it("사용자 정의 ccd/ccg가 있고 아직 설치 전이면 conflict(덮어쓰지 않는다)", () => {
    const conflicts = [conflict(), conflict({ name: "ccg", line: 43, text: "ccg() { echo hi }" })];
    const st = status({ conflicts });
    expect(classifyShellProfiles(st, { kind: "ready" })).toEqual({
      kind: "conflict",
      status: st,
      conflicts,
      replaceable: true,
    });
  });

  it(".zlogin처럼 가릴 수 없는 충돌이 하나라도 있으면 교체를 열지 않는다", () => {
    const conflicts = [conflict(), conflict({ name: "ccg", file: "/home/user/.zlogin", line: 1, replaceable: false })];
    const view = classifyShellProfiles(status({ conflicts }), { kind: "ready" });
    expect(view).toMatchObject({ kind: "conflict", replaceable: false });
  });

  it("교체로 우리 정의가 이기고 있으면(overriding) 충돌이 아니라 설치 상태로 그린다", () => {
    const st = status({ installed: true, upToDate: true, overriding: true, conflicts: [conflict()] });
    expect(classifyShellProfiles(st, { kind: "ready" })).toEqual({ kind: "installed", status: st });
    const stale = status({ installed: true, upToDate: false, overriding: true, conflicts: [conflict()] });
    expect(classifyShellProfiles(stale, { kind: "ready" })).toEqual({ kind: "outdated", status: stale });
  });

  it("설치 뒤 블록 아래에 생긴 정의는 우리를 덮으므로 다시 conflict로 보여 준다", () => {
    const st = status({ installed: true, upToDate: true, overriding: false, conflicts: [conflict()] });
    expect(classifyShellProfiles(st, { kind: "ready" })).toMatchObject({ kind: "conflict", replaceable: true });
  });

  it("설치돼 있고 최신이면 installed", () => {
    const st = status({ installed: true, upToDate: true, scriptExists: true, proposedBlock: null });
    expect(classifyShellProfiles(st, { kind: "ready" })).toEqual({ kind: "installed", status: st });
  });

  it("설치돼 있지만 낡았으면 outdated(다시 적용)", () => {
    const st = status({ installed: true, upToDate: false, scriptExists: true });
    expect(classifyShellProfiles(st, { kind: "ready" })).toEqual({ kind: "outdated", status: st });
  });

  it("지원되고 충돌도 설치도 없으면 would-install(미리보기 + 적용)", () => {
    const st = status();
    expect(classifyShellProfiles(st, { kind: "ready" })).toEqual({ kind: "would-install", status: st });
  });
});

describe("addedLines", () => {
  it("블록의 각 줄을 미리보기 줄로 돌려준다", () => {
    expect(addedLines(BLOCK)).toHaveLength(3);
    expect(addedLines(BLOCK)[1]).toContain("source");
  });

  it("블록이 없으면(이미 최신) 빈 목록", () => {
    expect(addedLines(null)).toEqual([]);
  });

  it("빈 줄은 미리보기에서 뺀다", () => {
    expect(addedLines("a\n\nb")).toEqual(["a", "b"]);
  });
});

describe("manualSnippet", () => {
  it("상태가 있으면 함수 파일 내용과 rc 블록을 경로 주석과 함께 잇는다", () => {
    const st = status();
    const snippet = manualSnippet(st);
    expect(snippet).toContain(`# ${st.scriptPath}`);
    expect(snippet).toContain("ccg() { … }");
    expect(snippet).toContain(`# ${st.rcPath}`);
    expect(snippet).toContain(BLOCK);
    // 함수 파일 → rc 블록 순서(붙여넣는 순서 그대로).
    expect(snippet.indexOf(`# ${st.scriptPath}`)).toBeLessThan(snippet.indexOf(`# ${st.rcPath}`));
  });

  it("이미 최신이라 proposedBlock이 없으면 표식 블록을 경로로 만들어 보여 준다", () => {
    const st = status({ installed: true, upToDate: true, proposedBlock: null });
    expect(manualSnippet(st)).toContain(rcBlock(st.scriptPath));
  });

  it("상태가 없으면(브라우저 dev) 기본 경로의 rc 블록만 보여 준다", () => {
    const snippet = manualSnippet(null);
    expect(snippet).toBe([`# ${FALLBACK_RC_PATH}`, rcBlock(FALLBACK_SCRIPT_PATH)].join("\n"));
    expect(snippet).toContain(FALLBACK_SCRIPT_PATH);
  });

  it("함수 파일 미리보기가 비어 있으면 rc 블록만 남긴다", () => {
    const st = status({ scriptPreview: "   \n" });
    expect(manualSnippet(st)).toBe([`# ${st.rcPath}`, BLOCK].join("\n"));
  });
});

describe("isConflictError", () => {
  it("실제 RpcError 모양(details.code)으로 충돌 거절을 알아본다", () => {
    // Tauri가 던지는 직렬화된 RpcError: 최상위 code는 ErrorCode(SCREAMING_SNAKE),
    // 진짜 사유는 details.code에 실려 온다(shell_profiles_rpc_error).
    expect(
      isConflictError({
        code: "INVALID_STATE",
        message: "shell_profiles_conflict: 1 conflicting ccd/ccg definition(s) already exist",
        retryable: false,
        details: { code: "shell_profiles_conflict", conflicts: [conflict()] },
      }),
    ).toBe(true);
  });

  it("교체로도 가릴 수 없다는 거절(unreplaceable)도 충돌로 알아본다", () => {
    expect(
      isConflictError({
        code: "INVALID_STATE",
        message: "shell_profiles_conflict_unreplaceable: 1 ccd/ccg definition(s) load after ~/.zshrc",
        retryable: false,
        details: { code: "shell_profiles_conflict_unreplaceable", conflicts: [conflict({ replaceable: false })] },
      }),
    ).toBe(true);
  });

  it("문구에만 코드가 실려 와도(문자열 오류) 알아본다", () => {
    expect(isConflictError(new Error("shell_profiles_conflict: ccd is user-defined"))).toBe(true);
  });

  it("다른 오류는 충돌로 보지 않는다", () => {
    expect(isConflictError(new Error("permission denied"))).toBe(false);
    expect(
      isConflictError({ code: "INVALID_STATE", message: "boom", retryable: false, details: { code: "io_error" } }),
    ).toBe(false);
    expect(isConflictError({ code: "io_error" })).toBe(false);
  });
});

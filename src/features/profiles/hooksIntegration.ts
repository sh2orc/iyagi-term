/**
 * CLI hooks 연동 상태(프론트 측 순수 로직 — Claude Code·Codex 공용).
 *
 * Tauri 명령(`claude_hooks_*`/`codex_hooks_*`)은 Rust `hooks_json`(두 CLI
 * 공용 JSON 조작)이 계산한 상태를 돌려준다. 이 모듈은 그 상태를 UI가 쓰는
 * 판정으로만 정리한다 — 파일 접근·JSON 조작은 전부 Rust(백업·동의 diff
 * 포함). `claudeHooksIntegration.ts`/`codexHooksIntegration.ts`는 이
 * 모듈을 `cli` 인자로 고정해 감싸는 얇은 어댑터다.
 *
 * 동의 계약(§2.1): `proposed`가 있는 동안은 "미리보기"만 보여 주고,
 * 사용자가 적용을 눌러야 `apply`를 부른다. 브라우저 dev(비-Tauri)에서는
 * 수동 안내(붙여넣기 조각)로 폴백한다.
 */

export type HookCli = "claude" | "codex";

export interface HooksStatus {
  path: string;
  exists: boolean;
  current: string | null;
  /** 적용 후 전체 내용 — 이미 모두 등록됐으면 null. */
  proposed: string | null;
  managedEntries: number;
  /** 완전히 등록됐을 때의 개수(Rust `HooksSpec`의 이벤트 수와 같다). */
  expectedEntries: number;
  hookCommand: string;
}

export type HooksAvailability =
  | { kind: "ready" }
  | { kind: "not-tauri" }
  | { kind: "error"; message: string };

/**
 * 현재 상태를 UI 판정으로 정리한다:
 * - "registered": 관리 대상 이벤트 전부 등록됨(managedEntries === expectedEntries).
 * - "partial": 일부만 등록됨(구버전에서 업그레이드했거나 수동 편집됨) —
 *   나머지를 마저 채우는 적용 버튼을 보여 줘야 한다.
 * - "would-register": 아직 하나도 등록 안 됨 — 적용 전 미리보기.
 * - "unavailable": 비-Tauri 환경이거나 상태 조회 실패.
 */
export type HooksView =
  | { kind: "registered"; status: HooksStatus }
  | { kind: "partial"; status: HooksStatus }
  | { kind: "would-register"; status: HooksStatus }
  | { kind: "unavailable"; reason: "not-tauri" | "error"; message?: string };

export function classifyHooksStatus(
  status: HooksStatus | null,
  availability: HooksAvailability,
): HooksView {
  if (availability.kind === "not-tauri") {
    return { kind: "unavailable", reason: "not-tauri" };
  }
  if (availability.kind === "error") {
    return { kind: "unavailable", reason: "error", message: availability.message };
  }
  if (!status) {
    return { kind: "unavailable", reason: "error", message: "no status" };
  }
  if (status.managedEntries <= 0) {
    return { kind: "would-register", status };
  }
  if (status.managedEntries < status.expectedEntries) {
    return { kind: "partial", status };
  }
  return { kind: "registered", status };
}

/**
 * CLI별 관리 이벤트 목록 — Rust 쪽(`claude_hooks::MANAGED_EVENTS` /
 * `codex_hooks::MANAGED_EVENTS`)과 반드시 같아야 한다. Claude만
 * `Notification`(승인 알림)을 더 갖는다; Codex는 세션 생명주기뿐이다.
 */
const MANAGED_EVENTS: Record<HookCli, readonly string[]> = {
  claude: ["Notification", "SessionStart", "SessionEnd"],
  codex: ["SessionStart", "SessionEnd"],
};

/** 우리가 넣는 hook 엔트리의 `timeout`(초) — Rust 쪽과 동일. */
const HOOK_TIMEOUT_SECS = 10;

/**
 * 우리 hook 명령을 알아보는 표시용 패턴. 실제 판정은 Rust
 * (`hooks_json::is_managed_command`)가 argv 수준으로 하고, 여기서는 "어느
 * 줄이 우리가 추가하는 줄인가"를 보여 주기만 한다 — 그래도 설치본처럼
 * 인용된 경로(`'…/iyagi-termd' hook`)와 Windows `iyagi-termd.exe hook`을
 * 놓치면 미리보기에서 정작 중요한 줄이 빠지므로, 프로그램 끝과 `hook`
 * 사이에 따옴표(JSON 문자열 안에서는 `\"`)가 끼어도 걸리게 둔다.
 */
const HOOK_COMMAND_PATTERN = /iyagi-termd(\.exe)?\\?["']?\s+hook\b/;

/**
 * 설정에 붙여넣을 수동 조각(비-Tauri 폴백 안내용) — 그 CLI의 이벤트 전부.
 * 이스케이프는 `JSON.stringify`가 한다(따옴표를 미리 고쳐 넘기면 결과가
 * `\\"`로 두 번 새겨져 붙여넣을 수 없는 명령이 된다).
 */
export function manualSnippet(cli: HookCli, hookCommand: string): string {
  const hooks: Record<string, unknown> = {};
  for (const event of MANAGED_EVENTS[cli]) {
    hooks[event] = [
      { hooks: [{ type: "command", command: hookCommand, timeout: HOOK_TIMEOUT_SECS }] },
    ];
  }
  return JSON.stringify({ hooks }, null, 2);
}

/** diff 대신 보여 줄 "바뀌는 부분" 발췌 — 전체 파일이 아닌 추가 라인들. */
export function addedLines(cli: HookCli, proposed: string | null): string[] {
  if (!proposed) return [];
  const eventPattern = new RegExp(`"(${MANAGED_EVENTS[cli].join("|")})"`);
  return proposed
    .split("\n")
    .filter((line) => HOOK_COMMAND_PATTERN.test(line) || eventPattern.test(line))
    .map((line) => line.trim());
}

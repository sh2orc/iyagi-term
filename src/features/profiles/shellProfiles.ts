/**
 * zsh 셸 명령(ccd/ccg) 설치 연동 — 프론트 측 순수 판정 + Tauri 명령 래퍼.
 *
 * `ccd`는 Claude Code를 Anthropic 인증 그대로, `ccg`는 설정에 저장한 Z.ai
 * 키로 GLM에 붙여 실행하는 zsh 함수다. 파일을 실제로 만들고 `~/.zshrc`를
 * 고치는 일은 전부 Rust(`shell_profiles_*` 명령)가 한다 — 이 모듈은 그
 * 상태를 UI 판정으로 정리하고, 명령을 부르는 얇은 래퍼만 갖는다.
 *
 * 동의 계약(§2.1): 상태 조회 → 추가될 줄 미리보기 → 사용자가 "적용"을
 * 눌러야 파일을 고친다. 브라우저 dev(비-Tauri)에서는 수동 안내(붙여넣기
 * 조각)로 폴백한다. 사용자가 직접 정의한 ccd/ccg가 있으면(conflicts)
 * 기본적으로 거절하고, 사용자가 "교체"를 고르면 그 줄은 지우지 않은 채
 * 우리 블록을 rc 맨 끝으로 옮겨 우리 정의가 이기게 한다(replaceExisting).
 */

import { tauriIpcAdapter } from "../bridge/ipc";

/** 설치할 수 없는 사유 — Rust `ShellProfilesStatus.reason`의 미러. */
export type ShellProfilesReason =
  | "windows"
  | "shell_not_zsh"
  | "daemon_binary_missing"
  | "no_home";

/** 사용자가 이미 정의해 둔 같은 이름의 함수·alias 한 건. */
export interface ShellProfileConflict {
  /** "ccd" | "ccg" — 확장 가능성이 있어 문자열로 둔다. */
  name: string;
  /** 충돌이 발견된 파일(대개 ~/.zshrc 또는 그 파일이 source하는 파일). */
  file: string;
  /** 1-based 줄 번호. */
  line: number;
  /** 그 줄의 원문(그대로 보여 준다 — 우리가 손대지 않는다는 증거). */
  text: string;
  /**
   * 우리 블록을 rc 끝에 두면 이 정의를 가릴 수 있는가. `.zshrc`보다 나중에
   * 읽히는 `.zlogin`의 정의는 거짓 — 교체로도 이길 수 없다.
   */
  replaceable: boolean;
}

/** `shell_profiles_*` 세 명령이 모두 돌려주는 상태(camelCase JSON). */
export interface ShellProfilesStatus {
  /** 이 기기에서 설치 가능한가(false면 reason이 사유). */
  supported: boolean;
  reason: ShellProfilesReason | null;
  /** 로그인 셸 경로(알 수 없으면 null). */
  shell: string | null;
  rcPath: string;
  rcExists: boolean;
  /** rc에 우리 블록이 들어 있는가. */
  installed: boolean;
  /** 설치된 스크립트가 현재 앱 버전·모델과 같은가(false면 다시 적용). */
  upToDate: boolean;
  /** 앱이 소유하는 함수 파일(rc는 이 파일을 source만 한다). */
  scriptPath: string;
  scriptExists: boolean;
  /** 함수가 호출할 데몬 실행 파일(없으면 reason이 daemon_binary_missing). */
  daemonBinary: string | null;
  /** rc에 추가될 표식 블록 — 이미 최신이면 null. */
  proposedBlock: string | null;
  /** 앱이 만들 함수 파일의 내용(접은 미리보기에 그대로 보여 준다). */
  scriptPreview: string;
  conflicts: ShellProfileConflict[];
  /**
   * 설치돼 있고 위 충돌이 모두 우리 블록보다 먼저 읽혀 지금 우리 함수가
   * 이기고 있는가(사용자가 "교체"를 골랐던 상태). 충돌이 없으면 false.
   */
  overriding: boolean;
  /** ccg가 쓸 주 모델(설정의 zaiMainModel을 그대로 넘긴 값). */
  mainModel: string;
}

export type ShellProfilesAvailability =
  | { kind: "ready" }
  | { kind: "not-tauri" }
  | { kind: "error"; message: string };

/**
 * 카드가 그리는 다섯 가지 상태:
 * - "would-install": 아직 설치 안 됨 — 추가될 줄 미리보기 + 적용.
 * - "outdated": 설치돼 있지만 내용이 낡음(앱 업데이트·모델 변경) — 다시 적용.
 * - "installed": 설치돼 있고 최신 — 제거만 보여 준다.
 * - "conflict": 사용자가 정의한 ccd/ccg가 있어 그냥 적용은 막는다. 모두
 *   가릴 수 있으면(replaceable) "교체"를 제안한다 — 사용자의 줄은 지우지
 *   않고 우리 블록을 rc 끝으로 옮긴다. 이미 교체해 둔 상태(overriding)는
 *   충돌이 아니라 installed/outdated로 그린다.
 * - "unavailable": 비-Tauri·조회 실패·이 기기에서 지원 불가(reason).
 */
export type ShellProfilesView =
  | { kind: "would-install"; status: ShellProfilesStatus }
  | { kind: "outdated"; status: ShellProfilesStatus }
  | { kind: "installed"; status: ShellProfilesStatus }
  | {
      kind: "conflict";
      status: ShellProfilesStatus;
      conflicts: ShellProfileConflict[];
      /** 모든 충돌을 블록 이동으로 가릴 수 있는가(교체 버튼을 열지). */
      replaceable: boolean;
    }
  | {
      kind: "unavailable";
      reason: "not-tauri" | "error" | "unsupported";
      /** reason === "error"일 때의 설명. */
      message?: string;
      /** reason === "unsupported"일 때 데몬이 준 사유(문구 키로 쓴다). */
      unsupported?: ShellProfilesReason | null;
      /** reason === "unsupported"일 때의 상태(셸 경로 등을 문구에 보여 준다). */
      status?: ShellProfilesStatus;
    };

export function classifyShellProfiles(
  status: ShellProfilesStatus | null,
  availability: ShellProfilesAvailability,
): ShellProfilesView {
  if (availability.kind === "not-tauri") {
    return { kind: "unavailable", reason: "not-tauri" };
  }
  if (availability.kind === "error") {
    return { kind: "unavailable", reason: "error", message: availability.message };
  }
  if (!status) {
    return { kind: "unavailable", reason: "error", message: "no status" };
  }
  if (!status.supported) {
    return { kind: "unavailable", reason: "unsupported", unsupported: status.reason, status };
  }
  // 사용자가 정의한 ccd/ccg는 묻지 않고 덮어쓰지 않는다. 이미 교체를 골라
  // 우리 정의가 이기고 있는 상태(overriding)만 설치 상태로 그린다 — 설치 뒤
  // 블록 아래에 새로 생긴 정의는 우리를 덮으므로 다시 충돌로 보여 준다.
  if (status.conflicts.length > 0 && !status.overriding) {
    return {
      kind: "conflict",
      status,
      conflicts: status.conflicts,
      replaceable: status.conflicts.every((conflict) => conflict.replaceable),
    };
  }
  if (status.installed && status.upToDate) {
    return { kind: "installed", status };
  }
  if (status.installed) {
    return { kind: "outdated", status };
  }
  return { kind: "would-install", status };
}

/**
 * 비-Tauri 폴백에서만 쓰는 기본값 — 실제 경로·블록은 언제나 데몬이 준다.
 * (브라우저 dev에는 상태 조회가 없어 보여 줄 것이 없기 때문이다.)
 * `<data_dir>`는 브라우저 dev가 알 수 없는 값이라 자리 표시로 그대로 남긴다
 * — 눈에 띄게 꺾쇠로 감싸 사용자가 실제 경로로 바꿔야 함을 드러낸다.
 * 마커·블록 형태는 `src-tauri/src/bridge/shell_profiles.rs`의
 * `BLOCK_BEGIN`/`BLOCK_END`/`render_block`과 글자 하나까지 맞춘다.
 */
export const FALLBACK_SCRIPT_PATH = "<data_dir>/config/shell/claude-profiles.zsh";
export const FALLBACK_RC_PATH = "~/.zshrc";
const BLOCK_BEGIN_MARKER = "# >>> Iyagi claude profiles (ccd/ccg) >>>";
const BLOCK_END_MARKER = "# <<< Iyagi claude profiles (ccd/ccg) <<<";

/** Rust `shell_quote`와 같은 POSIX 홑따옴표 인용(표시용). */
function shellQuote(value: string): string {
  return `'${value.replace(/'/g, "'\\''")}'`;
}

/** rc에 들어가는 3줄 표식 블록(표시용 폴백 — 실제 값은 proposedBlock). */
export function rcBlock(scriptPath: string): string {
  const quoted = shellQuote(scriptPath);
  return [
    BLOCK_BEGIN_MARKER,
    `[ -r ${quoted} ] && source ${quoted}`,
    BLOCK_END_MARKER,
  ].join("\n");
}

/**
 * 수동 안내 조각: 앱이 만들 함수 파일 내용 + rc에 붙일 표식 블록.
 * 상태가 없으면(브라우저 dev) 함수 본문은 데몬만 아는 값이라 생략하고,
 * rc 블록만 경로 기본값으로 보여 준다.
 */
export function manualSnippet(status: ShellProfilesStatus | null): string {
  const scriptPath = status?.scriptPath ?? FALLBACK_SCRIPT_PATH;
  const rcPath = status?.rcPath ?? FALLBACK_RC_PATH;
  const block = status?.proposedBlock ?? rcBlock(scriptPath);
  const script = status?.scriptPreview.trim() ? status.scriptPreview.trimEnd() : null;
  const sections = script === null
    ? [`# ${rcPath}`, block]
    : [`# ${scriptPath}`, script, "", `# ${rcPath}`, block];
  return sections.join("\n");
}

/** diff 대신 보여 줄 "추가되는 줄" — 빈 줄은 뺀다(hooks 카드와 같은 표기). */
export function addedLines(proposedBlock: string | null): string[] {
  if (!proposedBlock) return [];
  return proposedBlock
    .split("\n")
    .map((line) => line.trimEnd())
    .filter((line) => line.length > 0);
}

/**
 * 사용자 정의 ccd/ccg 때문에 적용이 거절됐을 때 데몬이 주는 코드. 교체로도
 * 가릴 수 없을 때는 `shell_profiles_conflict_unreplaceable` — 같은 접두사라
 * 두 경우 모두 충돌 목록을 다시 읽는다.
 */
export const CONFLICT_ERROR_CODE = "shell_profiles_conflict";

function isConflictCode(value: unknown): boolean {
  return typeof value === "string" && value.startsWith(CONFLICT_ERROR_CODE);
}

/** Tauri 오류에서 사람이 읽을 문구만 꺼낸다(다른 연동 카드와 같은 규칙). */
export function errorText(value: unknown): string {
  if (value instanceof Error) return value.message;
  if (typeof value === "object" && value !== null && "message" in value && typeof value.message === "string") {
    return value.message;
  }
  return String(value);
}

/**
 * 적용 거절이 "사용자 정의 함수와 충돌"인가 — 맞으면 충돌 목록을 다시 읽는다.
 * Tauri가 던지는 값은 직렬화된 `RpcError`다: 최상위 `code`는 SCREAMING_SNAKE
 * `ErrorCode`(예: `INVALID_STATE`)라 여기 코드와 같을 수 없고, 실제 사유는
 * `details.code`에 실려 온다(`shell_profiles_rpc_error`). 두 자리 모두 보고,
 * 그래도 못 찾으면 문구에서라도 찾는다.
 */
export function isConflictError(value: unknown): boolean {
  if (typeof value === "object" && value !== null) {
    const e = value as { code?: unknown; details?: { code?: unknown } | null };
    if (isConflictCode(e.code)) return true;
    if (isConflictCode(e.details?.code)) return true;
  }
  return errorText(value).includes(CONFLICT_ERROR_CODE);
}

// ------------------------------------------------------------ Tauri 명령

export function getShellProfilesStatus(mainModel?: string): Promise<ShellProfilesStatus> {
  return tauriIpcAdapter.invoke<ShellProfilesStatus>(
    "shell_profiles_status",
    mainModel === undefined ? undefined : { mainModel },
  );
}

/**
 * 적용. `replaceExisting`은 사용자가 "교체"를 눌렀을 때만(또는 이미 교체해 둔
 * 설치를 다시 적용할 때만) 참이다 — 그 외에는 충돌이 있으면 데몬이 거절한다.
 */
export function applyShellProfiles(
  mainModel: string,
  replaceExisting = false,
): Promise<ShellProfilesStatus> {
  return tauriIpcAdapter.invoke<ShellProfilesStatus>("shell_profiles_apply", {
    mainModel,
    replaceExisting,
  });
}

export function removeShellProfiles(mainModel?: string): Promise<ShellProfilesStatus> {
  return tauriIpcAdapter.invoke<ShellProfilesStatus>(
    "shell_profiles_remove",
    mainModel === undefined ? undefined : { mainModel },
  );
}

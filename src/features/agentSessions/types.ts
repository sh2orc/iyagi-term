/**
 * 기록된 에이전트 세션(01 §6 `agent_session.*`, 02 §8)을 UI 모양으로
 * 옮기는 순수 변환들 — 04-ui.md §5 "AI 에이전트 세션 이어서 열기".
 *
 * 실행 중 확인한 대화 식별자를 작업공간에 보존한다. 사용자가 연결하거나
 * 재실행할 때 resumeAgentSession 경로에서 이 식별자를 사용한다.
 */

import type { AgentSessionRecord } from "../../generated/AgentSessionRecord";
import { autonomyArgv } from "../workloads/autonomy";
import { t } from "../../i18n";

/** 이어서 열기 인자가 계약으로 확정된 에이전트만 재개할 수 있다. */
export type ResumableAgent = "claude" | "codex" | "opencode";

export function isResumableAgent(agent: string): agent is ResumableAgent {
  return agent === "claude" || agent === "codex" || agent === "opencode";
}

/** pane이 기억하는 "이 자리에서 이어서 열 수 있는 세션" 정보. */
export interface AgentResumeInfo {
  /** 데몬 기록의 식별자. 프로세스 관찰만으로 표시한 경우 아직 없을 수 있다. */
  recordId: string | null;
  agent: ResumableAgent;
  /** 각 CLI의 재개 옵션에 전달할 대화 ID. */
  agentSessionId: string;
  /** 기록된 작업 디렉터리 — 재개는 반드시 이 경로에서 시작한다. */
  cwd: string;
  title: string | null;
  /** 데몬이 관찰한 실행 파일 절대 경로(네이티브 바이너리일 때만). */
  program: string | null;
}

/**
 * 재개 인자로 그대로 들어갈 수 있는 세션 id인가.
 *
 * `claude --resume <id>` / `codex resume <id>`의 인자는 셸을 거치지 않고
 * argv로 전달되지만, `-`로 시작하는 값은 그 CLI가 플래그로 해석한다
 * (예: `--yolo`). 기록이 오염됐을 때 그것이 옵션 주입이 되지 않도록,
 * 첫 글자는 ASCII 영숫자로 못박고 나머지도 id에 쓰이는 문자만 허용한다.
 * 경로로 오해될 `.`/`..`는 첫 글자 규칙에서 이미 걸린다.
 */
const SAFE_SESSION_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]*$/;

export function isSafeSessionId(id: string): boolean {
  if (id.length < 1 || id.length > 128) return false;
  if (id === "." || id === "..") return false;
  return SAFE_SESSION_ID.test(id);
}

export function resumeInfoFrom(record: AgentSessionRecord): AgentResumeInfo | null {
  if (!isResumableAgent(record.agent)) return null;
  // 인자로 쓸 수 없는 id면 "재개 가능"이 아니다 — 목록은 잠근 버튼을 낸다.
  if (!isSafeSessionId(record.agent_session_id)) return null;
  return {
    recordId: record.id,
    agent: record.agent,
    agentSessionId: record.agent_session_id,
    cwd: record.cwd,
    title: record.title ?? null,
    program: record.program ?? null,
  };
}

/** 로컬 저장에는 검증된 대화 식별자와 재개 경로만 남긴다. */
export function decodeResumeInfo(value: unknown): AgentResumeInfo | null {
  if (!value || typeof value !== "object") return null;
  const info = value as Record<string, unknown>;
  const bounded = (v: unknown): v is string => typeof v === "string" && v.length > 0 && v.length <= 4096 && !v.includes("\0");
  if (typeof info.agent !== "string" || !isResumableAgent(info.agent) ||
    typeof info.agentSessionId !== "string" || !isSafeSessionId(info.agentSessionId) || !bounded(info.cwd)) return null;
  return {
    recordId: bounded(info.recordId) ? info.recordId : null,
    agent: info.agent,
    agentSessionId: info.agentSessionId,
    cwd: info.cwd,
    title: bounded(info.title) ? info.title : null,
    program: bounded(info.program) ? info.program : null,
  };
}

/** 표시용 축약 id — 전체 id는 툴팁에 그대로 남긴다. */
export function shortSessionId(id: string): string {
  return id.slice(0, 8);
}

/** 행/배지 표시 이름: 에이전트가 붙인 제목 → 없으면 짧은 id. */
export function sessionDisplayLabel(title: string | null | undefined, sessionId: string): string {
  return title && title.trim().length > 0 ? title : shortSessionId(sessionId);
}

/** CLI가 문서화한 재개 명령 — 툴팁에 원문 그대로 보여 준다. */
export function resumeCommand(agent: string, sessionId: string): string | null {
  if (agent === "claude") return `claude --resume ${sessionId}`;
  if (agent === "codex") return `codex resume ${sessionId}`;
  if (agent === "opencode") return `opencode --session ${sessionId}`;
  return null;
}

/**
 * pane의 재개 명령: 살아 있는 에이전트 세션의 것, 없으면 이어서 열 수 있는 기록의 것. 오른쪽 클릭
 * 메뉴와 메뉴 막대가 함께 쓴다(React가 없는 이 모듈에 둔다).
 */
export function paneResumeCommand(pane: {
  agent?: { agent: string; session_id?: string | null } | null;
  resume?: { agent: string; agentSessionId: string } | null;
}): string | null {
  if (pane.agent?.session_id) return resumeCommand(pane.agent.agent, pane.agent.session_id);
  if (pane.resume) return resumeCommand(pane.resume.agent, pane.resume.agentSessionId);
  return null;
}

/**
 * 재개 실행의 argv(프로그램 뒤 인자). codex는 하위 명령이 맨 앞에 와야
 * 하므로 자율 실행 플래그를 뒤에 붙이고, claude는 옵션이라 순서가 자유롭다.
 *
 * 인자로 쓸 수 없는 세션 id면 null이다 — 호출자는 아무것도 실행하지 않고
 * 문구만 남긴다(옵션 주입 방어선, isSafeSessionId).
 */
export function resumeArgv(
  agent: ResumableAgent,
  sessionId: string,
  autonomy: boolean,
): string[] | null {
  if (!isSafeSessionId(sessionId)) return null;
  // OpenCode의 기존 권한 설정을 그대로 사용한다.
  if (agent === "opencode") return ["--session", sessionId];
  return agent === "codex"
    ? ["resume", sessionId, ...autonomyArgv("codex", [], autonomy)]
    : autonomyArgv("claude", ["--resume", sessionId], autonomy);
}

const MINUTE_MS = 60_000;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;

/** 상대 시각(분/시간/일). 정확한 시각은 행 툴팁의 원문이 맡는다. */
export function relativeTimeText(iso: string, now: number = Date.now()): string {
  const at = Date.parse(iso);
  if (!Number.isFinite(at)) return iso;
  const elapsed = Math.max(0, now - at);
  if (elapsed < MINUTE_MS) return t("agentSessions.time.now");
  if (elapsed < HOUR_MS) return t("agentSessions.time.minutes", { n: Math.floor(elapsed / MINUTE_MS) });
  if (elapsed < DAY_MS) return t("agentSessions.time.hours", { n: Math.floor(elapsed / HOUR_MS) });
  return t("agentSessions.time.days", { n: Math.floor(elapsed / DAY_MS) });
}

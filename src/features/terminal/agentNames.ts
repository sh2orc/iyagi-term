/**
 * 자동 감지된 에이전트 id → 표시 이름. 서명 테이블(iyagi-termd
 * agent_watch)의 id와 대응한다 — 새 에이전트를 추가할 때 이쪽 문구도
 * 함께 늘린다(모르는 id는 그대로 표시해 깨지지 않게).
 */

import type { AgentStatus } from "../../generated/AgentStatus";
import { resumeCommand, sessionDisplayLabel } from "../agentSessions/types";
import { t } from "../../i18n";
import { isAgentModelProvisional } from "./agentModel";

const KNOWN_AGENTS = new Set(["claude", "codex", "opencode"]);

/** 서명 테이블이 아는 에이전트 id인가(claude/codex/opencode). */
export function isKnownAgent(id: string): boolean {
  return KNOWN_AGENTS.has(id);
}

/** 표시 이름과 색상에만 쓰는 공급자 id. 실행·복구용 에이전트 id는 유지한다. */
export function agentDisplayId(id: string, model?: string | null): string {
  return id === "claude" && model?.trim().toLowerCase().startsWith("glm") ? "zai" : id;
}

export function agentDisplayName(id: string, model?: string | null): string {
  if (agentDisplayId(id, model) === "zai") return "Z.ai";
  return KNOWN_AGENTS.has(id) ? t(`terminal.agent.${id}`) : id;
}

/**
 * 배지에 에이전트 이름 뒤로 붙는 세션 표시(04-ui §5): 에이전트가 붙인
 * 이름이 있으면 그대로, 없으면 세션 id 앞 8자. 세션 id를 아직 모르면
 * 아무것도 붙이지 않는다(추정하지 않는다).
 */
export function agentSessionBadgeLabel(
  status: Pick<AgentStatus, "session_id" | "session_name">,
): string | null {
  if (!status.session_id) return null;
  return sessionDisplayLabel(status.session_name, status.session_id);
}

/**
 * 배지 툴팁: 감지 문구 + (세션을 알면) 전체 세션 id와 그 CLI의 재개
 * 명령. 명령은 사용자가 터미널에서 직접 칠 수 있는 원문 그대로다.
 */
export function agentBadgeTooltip(status: AgentStatus): string {
  const lines = [t("terminal.agent.tooltip", { name: agentDisplayName(status.agent, status.model) })];
  if (status.session_id) {
    lines.push(t("terminal.agent.sessionId", { id: status.session_id }));
    const command = resumeCommand(status.agent, status.session_id);
    if (command) lines.push(t("terminal.agent.sessionResume", { command }));
  }
  // 모델·effort는 에이전트 기록 원문 그대로(배지에는 줄인 형태가 붙는다).
  if (status.model) lines.push(t("terminal.agent.model", { model: status.model }));
  if (status.effort) lines.push(t("terminal.agent.effort", { effort: status.effort }));
  if ((status.model || status.effort) && isAgentModelProvisional(status)) {
    lines.push(t("terminal.agent.modelProvisional"));
  }
  return lines.join("\n");
}

/**
 * 타이틀 앞 활동 마커의 상태. `working`은 응답 생성·도구/셸 실행 중,
 * `waiting`은 사용자 확인(권한·질문·대화상자) 대기, `idle`은 프롬프트 대기.
 */
export type AgentActivity = "working" | "waiting" | "idle";

/**
 * 에이전트가 스스로 보고한 상태 원문 → 활동 상태. Claude Code 레지스트리
 * (`~/.claude/sessions/<pid>.json`)의 `status`는 "busy" | "shell" | "idle" |
 * "waiting" 넷이다(2.1.x 바이너리 확인 — waiting은 `waitingFor`를 동반한다).
 * 모르는 값·없음은 null — 호출자가 출력 활동으로 대신 판정한다.
 */
export function agentActivityFromStatus(status: string | null | undefined): AgentActivity | null {
  switch (status) {
    case "busy":
    case "shell":
      return "working";
    case "waiting":
      return "waiting";
    case "idle":
      return "idle";
    default:
      return null;
  }
}

/**
 * pane의 에이전트 활동 상태. 자체 보고가 있으면 그것을 믿고, 없으면
 * (Codex·opencode는 밖으로 상태를 알리지 않는다) 최근 출력 활동으로
 * 대신한다 — TUI 스피너가 도는 동안은 출력이 흐르고, 입력 대기 중엔 멎는다.
 */
export function resolveAgentActivity(
  status: Pick<AgentStatus, "session_status">,
  outputActive: boolean,
): AgentActivity {
  return agentActivityFromStatus(status.session_status) ?? (outputActive ? "working" : "idle");
}

/** 마커 툴팁·aria 문구. idle은 마커가 없으므로 null. 탭과 pane 헤더가 같이 쓴다. */
export function agentActivityLabel(activity: AgentActivity, agentId: string): string | null {
  if (activity === "idle") return null;
  return t(activity === "working" ? "terminal.agent.working" : "terminal.agent.waiting", {
    name: agentDisplayName(agentId),
  });
}

/** 에이전트가 스스로 "작업 중"(busy·shell)이라고 보고했는가 — 추론하지 않는다. */
export function isAgentBusy(status: Pick<AgentStatus, "session_status">): boolean {
  return agentActivityFromStatus(status.session_status) === "working";
}

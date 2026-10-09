/**
 * 에이전트 배지·창 제목에 붙는 현재 모델·effort 표시.
 *
 * 값은 데몬 `model_watch`가 에이전트 자신의 기록(Claude 상태줄·transcript,
 * Codex config·rollout)에서 읽은 원문이다 — `/model`·`/effort`로 바꾸면 한
 * 틱(1초) 안에 따라간다. 여기서는 추측하지 않고 짧게 다듬기만 한다.
 */

import type { AgentStatus } from "../../generated/AgentStatus";

/** `claude-<family>-<major>[-<minor>][-YYYYMMDD][[1m]]` 형태의 모델 id. */
const CLAUDE_MODEL_ID = /^claude-([a-z]+)-(\d{1,2}(?:-\d{1,2})*)(?:-\d{8})?(\[1m\])?$/i;

/**
 * 좁은 헤더에 맞게 다듬는다: "Opus 5 (1M context)" → "Opus 5 (1M)",
 * "claude-sonnet-4-6" → "Sonnet 4.6", "opus[1m]" → "opus (1M)". 그 밖(Codex의
 * "gpt-5.6-sol" 등)은 원문 그대로.
 */
export function compactModelLabel(model: string): string {
  const trimmed = model.trim();
  const id = CLAUDE_MODEL_ID.exec(trimmed);
  if (id) {
    const family = id[1].charAt(0).toUpperCase() + id[1].slice(1).toLowerCase();
    return `${family} ${id[2].replace(/-/g, ".")}${id[3] ? " (1M)" : ""}`;
  }
  return trimmed.replace(/\s*\(1M context\)$/i, " (1M)").replace(/\[1m\]$/i, " (1M)");
}

/** "모델 · effort" — 둘 중 아는 것만. 둘 다 모르면 `null`. */
export function agentModelLabel(status: Pick<AgentStatus, "model" | "effort">): string | null {
  const model = status.model ? compactModelLabel(status.model) : "";
  const effort = status.effort?.trim() ?? "";
  if (model && effort) return `${model} · ${effort}`;
  return model || effort || null;
}

/** 세션 기록이 생기기 전의 잠정값(실행 인수·설정 기본값)인가. */
export function isAgentModelProvisional(status: Pick<AgentStatus, "model_source">): boolean {
  return status.model_source === "defaults";
}

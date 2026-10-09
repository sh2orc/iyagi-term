/**
 * 끝난 작업에서 새 작업 대화상자를 여는 흐름(05 §3·§9).
 *
 * - 완료: `이 결과로 후속 작업 만들기` — 이전 제목과 원래 목표를 인용한 초안.
 * - 실패·취소: `같은 목표로 새 작업` — 원래 목표 그대로.
 *
 * 목표 원문은 store에 두지 않으므로 goal artifact를 화면 수명 캐시로 다시 읽는다.
 * 읽지 못해도 대화상자는 연다(후속 초안은 제목만, 같은 목표는 빈 입력).
 */

import type { Mission } from "../../generated/Mission";
import { t } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { readArtifactText } from "./bodyCache";
import { getMissionClient } from "./clientAccess";
import { focusedRepositoryHint, openMissionCreate } from "./entry";

export type NewMissionSource = "follow-up" | "same-goal";

async function readGoalText(mission: Mission): Promise<string | null> {
  const client = getMissionClient();
  if (!client) return null;
  try {
    return await readArtifactText(client, mission.goal_ref.id, Number(mission.goal_ref.bytes));
  } catch {
    return null;
  }
}

/** 새 작업 대화상자에 넣을 목표 초안. */
export function newMissionGoalDraft(mission: Pick<Mission, "title">, source: NewMissionSource, goal: string | null): string | null {
  const original = goal?.trim() ? goal.trim() : null;
  if (source === "same-goal") return original;
  return original !== null
    ? t("missions.followUp.goalDraft", { title: mission.title, goal: original })
    : t("missions.followUp.goalDraftNoGoal", { title: mission.title });
}

/** 끝난 작업에서 새 작업 대화상자를 연다(같은 저장소를 먼저 채운다). */
export async function openNewMissionFrom(mission: Mission, source: NewMissionSource): Promise<void> {
  const goal = await readGoalText(mission);
  openMissionCreate(mission.repository_path || focusedRepositoryHint(useWorkbenchStore.getState()), {
    goal: newMissionGoalDraft(mission, source, goal),
    followUpOf: source === "follow-up" ? mission.id : null,
  });
}

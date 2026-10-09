/**
 * mutation 공용 흐름(01 §2 CAS · 05-ui §10).
 *
 * 화면이 들고 있는 mission prop은 한 박자 늦을 수 있다. 전송 직전 store의
 * 최신 mission으로 요청을 만들고, REVISION_CONFLICT면 한 번만 다시 동기화한
 * 뒤 최신 revision으로 다시 만든다. attempt는 호출될 때마다 새 request_id를
 * 담아야 한다(같은 id + 다른 expected_revision은 REQUEST_CONFLICT다).
 */

import type { Mission } from "../../generated/Mission";
import { useMissionStore } from "./store";
import { StaleActionError, isRevisionConflict } from "./errors";

function latestMission(missionId: string): Mission | null {
  return useMissionStore.getState().missions[missionId] ?? null;
}

/**
 * attempt(latest)를 보낸다. attempt가 null을 돌려주면(행동이 더 이상 유효하지
 * 않음) StaleActionError. 충돌이면 syncMission 후 1회만 재시도하고, 그래도
 * 충돌이면 원래 오류를 던진다.
 */
export async function mutateWithResync<T>(
  missionId: string,
  attempt: (mission: Mission) => Promise<T> | null,
): Promise<T> {
  const first = latestMission(missionId);
  const pending = first ? attempt(first) : null;
  if (pending === null) throw new StaleActionError();
  try {
    return await pending;
  } catch (error) {
    if (!isRevisionConflict(error)) throw error;
    await useMissionStore.getState().syncMission(missionId);
    const refreshed = latestMission(missionId);
    const retry = refreshed ? attempt(refreshed) : null;
    if (retry === null) throw new StaleActionError();
    try {
      return await retry;
    } catch (second) {
      if (isRevisionConflict(second)) throw error;
      throw second;
    }
  }
}

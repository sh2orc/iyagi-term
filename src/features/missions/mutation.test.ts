/**
 * mutateWithResync 계약: 최신 store mission으로 요청, REVISION_CONFLICT면
 * 재동기화 후 한 번만 재시도, 두 번째 충돌은 원래 오류, stale은 StaleActionError.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Mission } from "../../generated/Mission";
import { RpcClientError } from "../daemon/client";
import { StaleActionError } from "./errors";
import { mutateWithResync } from "./mutation";
import { resetMissionStoreForTests, useMissionStore } from "./store";

const originalSync = useMissionStore.getState().syncMission;

function seedMission(revision: string): Mission {
  const mission = { id: "mission-1", revision, title: "fixture" } as Mission;
  useMissionStore.setState((s) => ({ missions: { ...s.missions, [mission.id]: mission } }));
  return mission;
}

/** syncMission 대역: 호출되면 revision을 올린다(시험마다 복원한다). */
function installSync(nextRevision: string | null) {
  const sync = vi.fn(async (missionId: string) => {
    if (nextRevision === null) {
      useMissionStore.setState((s) => {
        const missions = { ...s.missions };
        delete missions[missionId];
        return { missions };
      });
      return;
    }
    useMissionStore.setState((s) => ({
      missions: { ...s.missions, [missionId]: { ...s.missions[missionId], revision: nextRevision } },
    }));
  });
  useMissionStore.setState({ syncMission: sync });
  return sync;
}

const conflict = () => new RpcClientError("REVISION_CONFLICT", "moved", false, { current_revision: "9" });

beforeEach(() => {
  resetMissionStoreForTests();
});

afterEach(() => {
  useMissionStore.setState({ syncMission: originalSync });
});

describe("mutateWithResync", () => {
  it("화면 prop이 아니라 store의 최신 mission으로 요청을 만든다", async () => {
    seedMission("7");
    const attempt = vi.fn(async (mission: Mission) => mission.revision);
    await expect(mutateWithResync("mission-1", attempt)).resolves.toBe("7");
    expect(attempt).toHaveBeenCalledTimes(1);
  });

  it("REVISION_CONFLICT면 재동기화하고 최신 revision으로 한 번만 다시 시도한다", async () => {
    seedMission("7");
    const sync = installSync("9");
    const revisions: string[] = [];
    const attempt = vi.fn((mission: Mission) => {
      revisions.push(mission.revision);
      return revisions.length === 1 ? Promise.reject(conflict()) : Promise.resolve("ok");
    });
    await expect(mutateWithResync("mission-1", attempt)).resolves.toBe("ok");
    expect(sync).toHaveBeenCalledTimes(1);
    expect(sync).toHaveBeenCalledWith("mission-1");
    expect(revisions).toEqual(["7", "9"]);
  });

  it("재시도도 충돌하면 원래 오류를 던지고 더 시도하지 않는다", async () => {
    seedMission("7");
    const sync = installSync("8");
    const first = conflict();
    const attempt = vi.fn()
      .mockImplementationOnce(() => Promise.reject(first))
      .mockImplementationOnce(() => Promise.reject(conflict()));
    await expect(mutateWithResync("mission-1", attempt)).rejects.toBe(first);
    expect(attempt).toHaveBeenCalledTimes(2);
    expect(sync).toHaveBeenCalledTimes(1);
  });

  it("재시도의 다른 오류는 그대로 던진다", async () => {
    seedMission("7");
    installSync("8");
    const denied = new RpcClientError("POLICY_DENIED", "no");
    const attempt = vi.fn()
      .mockImplementationOnce(() => Promise.reject(conflict()))
      .mockImplementationOnce(() => Promise.reject(denied));
    await expect(mutateWithResync("mission-1", attempt)).rejects.toBe(denied);
  });

  it("충돌이 아닌 오류는 재동기화 없이 그대로 던진다", async () => {
    seedMission("7");
    const sync = installSync("8");
    const busy = new RpcClientError("BUSY", "later", true);
    const attempt = vi.fn(() => Promise.reject(busy));
    await expect(mutateWithResync("mission-1", attempt)).rejects.toBe(busy);
    expect(attempt).toHaveBeenCalledTimes(1);
    expect(sync).not.toHaveBeenCalled();
  });

  it("attempt가 null이면 요청 없이 StaleActionError", async () => {
    seedMission("7");
    const sync = installSync("8");
    await expect(mutateWithResync("mission-1", () => null)).rejects.toBeInstanceOf(StaleActionError);
    expect(sync).not.toHaveBeenCalled();
  });

  it("store에 mission이 없으면 attempt를 부르지 않고 StaleActionError", async () => {
    const attempt = vi.fn(async () => "never");
    await expect(mutateWithResync("missing", attempt)).rejects.toBeInstanceOf(StaleActionError);
    expect(attempt).not.toHaveBeenCalled();
  });

  it("재동기화 뒤 행동이 무효(null)거나 mission이 사라지면 StaleActionError", async () => {
    seedMission("7");
    installSync("8");
    let calls = 0;
    await expect(mutateWithResync("mission-1", () => {
      calls += 1;
      return calls === 1 ? Promise.reject(conflict()) : null;
    })).rejects.toBeInstanceOf(StaleActionError);

    seedMission("7");
    installSync(null);
    const attempt = vi.fn(() => Promise.reject(conflict()));
    await expect(mutateWithResync("mission-1", attempt)).rejects.toBeInstanceOf(StaleActionError);
    expect(attempt).toHaveBeenCalledTimes(1);
  });
});

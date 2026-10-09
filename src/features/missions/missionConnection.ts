/**
 * mission 화면의 브리지·데몬 연결 상태(05 §10).
 *
 * 동기화 실패를 모두 "연결 끊김"으로 보이지 않도록 전송 계층 상태를 따로 본다.
 * 연결 상태를 알리는 공용 store/이벤트가 아직 없어서, 터미널 세션 제어기
 * (sessionController.checkTransport)와 같은 `transportStatus()`를 mission 화면이
 * 떠 있는 동안에만 주기적으로 읽는다. 재연결 자체는 세션 제어기와 RPC 재시도가
 * 맡고 여기서는 관찰만 한다 — 끊김→연결, 또는 연결 세대(generation)가 바뀌면
 * `reconnects`를 올려 화면이 snapshot을 다시 뽑게 한다.
 *
 * `transportStatus`가 없는 client(개발 mock·시험)는 상태를 모른다("unknown")
 * — 모르는 상태를 끊김으로 표시하지 않는다.
 */

import { useEffect } from "react";
import { create } from "zustand";
import { RpcClientError } from "../daemon/client";
import { getMissionClient } from "./clientAccess";

export type MissionConnectionStatus = "unknown" | "connected" | "disconnected";

export interface MissionConnectionState {
  status: MissionConnectionStatus;
  /** 마지막으로 본 연결 세대(realClient transportGeneration). */
  generation: number | null;
  /** 관찰한 재연결 횟수 — 값이 바뀌면 화면이 다시 동기화한다. */
  reconnects: number;
}

export const CONNECTION_POLL_MS = 3000;

export const useMissionConnectionStore = create<MissionConnectionState>(() => ({
  status: "unknown",
  generation: null,
  reconnects: 0,
}));

let users = 0;
let timer: ReturnType<typeof setInterval> | null = null;
let checking = false;

/** 연결 상태를 한 번 읽는다(동기화 오류가 연결 문제처럼 보일 때도 즉시 부른다). */
export async function checkMissionConnection(): Promise<void> {
  const client = getMissionClient();
  const statusFn = client?.transportStatus;
  if (!client || !statusFn || checking) return;
  checking = true;
  try {
    const status = await statusFn.call(client);
    applyConnection(status.controlAlive && status.dataAlive ? "connected" : "disconnected", status.generation);
  } catch {
    applyConnection("disconnected", null);
  } finally {
    checking = false;
  }
}

/** 관찰 결과 반영 — 끊김→연결 또는 세대 변경이면 재연결로 센다. */
export function applyConnection(status: MissionConnectionStatus, generation: number | null): void {
  const previous = useMissionConnectionStore.getState();
  const nextGeneration = generation ?? previous.generation;
  const recovered = previous.status === "disconnected" && status === "connected";
  const generationChanged =
    status === "connected" && generation !== null && previous.generation !== null && generation !== previous.generation;
  const reconnects = previous.reconnects + (recovered || generationChanged ? 1 : 0);
  if (previous.status === status && previous.generation === nextGeneration && previous.reconnects === reconnects) return;
  useMissionConnectionStore.setState({ status, generation: nextGeneration, reconnects });
}

/** mission 화면이 떠 있는 동안 연결 상태를 구독한다(여러 화면이 같은 주기를 공유). */
export function useMissionConnection(): { status: MissionConnectionStatus; reconnects: number } {
  useEffect(() => {
    users += 1;
    if (users === 1) {
      void checkMissionConnection();
      timer = setInterval(() => void checkMissionConnection(), CONNECTION_POLL_MS);
    }
    return () => {
      users -= 1;
      if (users === 0 && timer !== null) {
        clearInterval(timer);
        timer = null;
      }
    };
  }, []);
  const status = useMissionConnectionStore((s) => s.status);
  const reconnects = useMissionConnectionStore((s) => s.reconnects);
  return { status, reconnects };
}

/** dirty인 채 동기화가 실패했을 때 다시 뽑기까지의 첫 대기. */
export const SYNC_RETRY_BASE_MS = 1000;
/** 다시 뽑기 대기의 상한. */
export const SYNC_RETRY_MAX_MS = 30_000;

/**
 * 연속 실패 횟수(이미 다시 시도한 수) → 다음 재동기화까지의 대기: 1s → 2s → 4s … 최대 30s.
 * 성공하면 호출자가 횟수를 0으로 되돌린다.
 */
export function syncRetryDelayMs(failures: number): number {
  const exponent = Math.min(Math.max(0, Math.floor(failures)), 16);
  return Math.min(SYNC_RETRY_MAX_MS, SYNC_RETRY_BASE_MS * 2 ** exponent);
}

/** 동기화 오류 분류 — store는 `CODE: message` 문자열만 남긴다. */
export type SyncProblem = "none" | "not_found" | "transient" | "failed";

const TRANSIENT_SYNC_CODES = new Set(["BUSY", "SNAPSHOT_EXPIRED", "CURSOR_EXPIRED", "DAEMON_UNAVAILABLE"]);

export function parseSyncError(error: string): { code: string | null; message: string } {
  const match = /^([A-Z][A-Z0-9_]+): ([\s\S]*)$/.exec(error);
  return match ? { code: match[1], message: match[2] } : { code: null, message: error };
}

export function classifySyncError(error: string | null): SyncProblem {
  if (error === null) return "none";
  const { code, message } = parseSyncError(error);
  if (code === "NOT_FOUND") return "not_found";
  if ((code !== null && TRANSIENT_SYNC_CODES.has(code)) || /\btime(d)?\s?out\b|\btimeout\b/i.test(message)) {
    return "transient";
  }
  return "failed";
}

/** 동기화 오류 문자열 → missionError가 읽는 원인 객체. */
export function syncErrorCause(error: string): unknown {
  const { code, message } = parseSyncError(error);
  return code !== null ? new RpcClientError(code as RpcClientError["code"], message) : new Error(message);
}

/** 시험 전용 초기화. */
export function resetMissionConnectionForTests(): void {
  useMissionConnectionStore.setState({ status: "unknown", generation: null, reconnects: 0 });
}

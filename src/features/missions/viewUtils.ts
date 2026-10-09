/**
 * mission 화면 공용 소형 도구(O16) — artifact 본문 비동기 훅과 시각 포맷.
 */

import { useEffect, useState } from "react";
import type { ArtifactRef } from "../../generated/ArtifactRef";
import { getMissionClient } from "./clientAccess";
import { readArtifactText } from "./bodyCache";

export { isInternalIntegration, isDeterministicIntegration } from "./executionKind";

export interface ArtifactTextState {
  /** null = 아직 로드 중이거나 실패. 빈 문자열 = 실제 빈 본문. */
  text: string | null;
  error: boolean;
}

/**
 * artifact 본문을 화면 수명 캐시(bodyCache)로 읽는다. ref가 바뀌면 다시
 * 읽는다 — 캐시가 같은 artifact의 재읽기를 흡수한다(05 §4).
 */
export function useArtifactText(ref: ArtifactRef | null): ArtifactTextState {
  const artifactId = ref?.id ?? null;
  const [state, setState] = useState<ArtifactTextState>(() =>
    artifactId === null ? { text: "", error: false } : { text: null, error: false },
  );

  useEffect(() => {
    if (artifactId === null) {
      setState({ text: "", error: false });
      return;
    }
    let alive = true;
    const client = getMissionClient();
    if (!client) {
      setState({ text: null, error: true });
      return;
    }
    const byteLength = Number(ref?.bytes ?? 0);
    readArtifactText(client, artifactId, byteLength)
      .then((text) => {
        if (alive) setState({ text, error: false });
      })
      .catch(() => {
        if (alive) setState({ text: null, error: true });
      });
    return () => {
      alive = false;
    };
    // ref?.id만 감시한다 — bytes는 같은 artifact에서 변하지 않는다.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [artifactId]);

  return state;
}

/** ISO 시각 → 현지 HH:MM(보고 링크의 시간 표기). */
export function formatClock(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  const hh = String(date.getHours()).padStart(2, "0");
  const mm = String(date.getMinutes()).padStart(2, "0");
  return `${hh}:${mm}`;
}

/** Persisted elapsed milliseconds, without wall-clock extrapolation or Number rounding. */
export function formatElapsedTime(milliseconds: string): string {
  const seconds = BigInt(milliseconds) / 1000n;
  return `${seconds / 3600n}:${String(seconds / 60n % 60n).padStart(2, "0")}:${String(seconds % 60n).padStart(2, "0")}`;
}

/** 마지막 활동로부터 경과 분(내림, 최소 1). */
export function minutesSince(iso: string | null): number {
  if (!iso) return 0;
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return 0;
  return Math.max(1, Math.floor((Date.now() - then) / 60000));
}

/** 새 request_id — mutation 멱등 키(01 §2). */
export function newRequestId(): string {
  return typeof crypto !== "undefined" && typeof crypto.randomUUID === "function"
    ? crypto.randomUUID()
    : `req-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

/**
 * 사용자에게 보여줄 오류 문장(RpcClientError는 code: message 형태로).
 *
 * @deprecated 원문 `code: message`를 그대로 노출한다. 새 코드는 `errors.ts`의
 * `missionError(t, cause)` + `MissionErrorNotice`를 쓴다(원문은 `detail`로만).
 * 기존 호출부 호환을 위해 동작은 바꾸지 않는다.
 */
export function errorText(error: unknown): string {
  if (error instanceof Error && error.message) {
    const code = (error as { code?: unknown }).code;
    const prefix = typeof code === "string" ? `${code}: ` : "";
    return `${prefix}${error.message}`;
  }
  return String(error);
}

/** U64 wire string을 숫자로(토큰 합계 표기용 — 정밀도 손실은 표기에 무해). */
export function u64ToNumber(value: string | null | undefined): number | null {
  if (value === null || value === undefined) return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

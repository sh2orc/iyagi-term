/**
 * 저신뢰 텍스트 패턴 감지(W3-5, SOTA_GAP_REVIEW §2.1 fallback).
 *
 * 계약:
 * - **opt-in 배지 전용**이다. 데스크톱 알림·사운드와 결금 금지(토론 조건).
 * - hooks(1차 신호)가 없는 세션의 **live 출력에서만** 본다 — 저널 재생
 *   재발화 방지는 호출자(pipeline live 콜백 + 쿨다운)가 담당한다.
 * - 문구 의존 휴리스틱이라 거짓 양성이 있다. 영어·한글 프롬프트 형태만
 *   보고, 확정 상태로 표시하지 않는다("확인 필요" 라벨).
 */

export type InterventionPattern = "permission" | "question";

const PERMISSION_PATTERNS: readonly RegExp[] = [
  /do you want to (proceed|allow|continue)/i,
  /\b(permission|approve|allow)\b.*\?/i,
  /\b(y\/n|yes\/no)\b/i,
  /waiting for (your )?(approval|permission)/i,
  /\byour (permission|approval)\b/i,
  /승인|허용.*\?/,
];

const QUESTION_PATTERNS: readonly RegExp[] = [
  /waiting for (your )?(input|response|reply)/i,
  /\bselect an option\b/i,
  /답변을 기다/i,
];

/** 이 출력 조각에서 개입 문구를 찾는다. 여러 개면 permission 우선. */
export function detectInterventionPattern(text: string): InterventionPattern | null {
  if (text.length === 0 || text.length > 8192) return null;
  if (PERMISSION_PATTERNS.some((pattern) => pattern.test(text))) return "permission";
  if (QUESTION_PATTERNS.some((pattern) => pattern.test(text))) return "question";
  return null;
}

/** 같은 패턴 재발화 억제 창(밀리초) — 배지 스팸 방지. */
export const PATTERN_COOLDOWN_MS = 30_000;

/** 세션별 최근 감지 기록(쿨다운 판정). */
export class PatternCooldown {
  private readonly lastSeen = new Map<string, number>();

  /** 새로 알려야 하면 true. window는 시험 주입용. */
  shouldReport(sessionId: string, pattern: InterventionPattern, now: number): boolean {
    const key = `${sessionId}:${pattern}`;
    const last = this.lastSeen.get(key);
    if (last !== undefined && now - last < PATTERN_COOLDOWN_MS) return false;
    this.lastSeen.set(key, now);
    return true;
  }

  /** 세션 종료 등의 정리. */
  clear(sessionId: string): void {
    for (const key of [...this.lastSeen.keys()]) {
      if (key.startsWith(`${sessionId}:`)) this.lastSeen.delete(key);
    }
  }
}

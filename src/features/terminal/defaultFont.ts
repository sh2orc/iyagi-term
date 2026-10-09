/**
 * 기본 글꼴 결정 — 한글 셀 정렬(캐럿 오프셋) 개선.
 *
 * xterm은 셀 폭을 스택의 1차 글꼴(라틴) 메트릭으로 계산하고, 한글 음절은
 * unicode11 기준 2셀 상자에 시스템 폴백 글리프로 그린다. 한국어 코딩
 * 글꼴(D2Coding·나눔고딕 코딩)은 "한글 폭 = 라틴 폭 × 2"가 정확히
 * 맞물리지만, 폴백 조합(예: 라틴 0.6em + Apple SD Gothic Neo 0.865em)은
 * 2셀 상자(1.2em) 대비 글리프가 짧아 음절마다 빈 간극이 생긴다 — 커서
 * 블록이 글자에서 한 칸 가까이 떨어져 깜빡이는 것처럼 보인다.
 *
 * 그래서 설치돼 있으면 한국어 코딩 글꼴을 기본 스택 앞에 올리고, 아니면
 * 플랫폼 관례 라틴 스택으로 물러난다. 사용자가 설정에서 고른 fontFamily는
 * 언제나 이 기본값보다 우선한다(xtermSetup의 `prefs.fontFamily ??`).
 */

/** 한국어 코딩 글꼴 후보 — "한글 폭 = 라틴 폭 × 2"가 보장되는 스택.
 * Nanum Gothic Coding은 앱에 내장되므로(main.tsx 프리로드) 항상 사용
 * 가능하다 — 설치 없이도 한글 셀 정렬이 기본 보장된다. */
export const KOREAN_MONO_CANDIDATES: readonly string[] = ["D2Coding", "Nanum Gothic Coding"];

/** 후보가 없을 때의 라틴 기본 스택(Windows Consolas, macOS Menlo, Linux
 * DejaVu/Ubuntu/Noto Sans Mono — fontconfig의 generic monospace보다 메트릭이
 * 예측 가능하다). */
export const LATIN_FALLBACK_STACK =
  "Consolas, Menlo, 'DejaVu Sans Mono', 'Ubuntu Mono', 'Noto Sans Mono', 'Courier New', monospace";

/**
 * 사용 가능한 후보 중 첫 글꼴을 앞세운 기본 스택. 글꼴 가용성 검사는
 * 주입한다 — node 시험은 가짜 checker로, 브라우저는 document.fonts.check으로.
 */
export function pickDefaultFontFamily(
  isAvailable: (family: string) => boolean,
  candidates: readonly string[] = KOREAN_MONO_CANDIDATES,
): string {
  for (const family of candidates) {
    try {
      if (isAvailable(family)) return `"${family}", ${LATIN_FALLBACK_STACK}`;
    } catch {
      // 가용성 검사 실패는 후보 없음으로 취급한다.
    }
  }
  return LATIN_FALLBACK_STACK;
}

/** 한글·라틴이 섞인 측정 문자열 — 후보 글꼴이 있으면 폴백과 폭이 달라진다. */
const PROBE_TEXT = "mmmmmmmmmmlli 가나다라마바사";
const PROBE_GENERICS = ["monospace", "serif"] as const;

/**
 * 글꼴이 실제로 그려지는가. `document.fonts.check`는 설치 여부를 알려 주지
 * 않는다 — WKWebView(macOS)는 없는 이름에도 true를 돌려준다(실측:
 * `check('13px "NoSuchFontXYZ"') === true`). 그래서 설치되지 않은 D2Coding이
 * 늘 뽑혀 내장 Nanum Gothic Coding까지 내려가지 못하고, 한글이 시스템 폴백
 * 글리프로 그려져 음절마다 간극이 생겼다. 대신 후보를 앞세운 스택과 generic
 * 폴백만의 폭을 재어 비교한다: 후보가 없으면 두 스택은 같은 글꼴로 그려진다.
 * 측정이 안 되면(캔버스 없음) 없는 것으로 본다.
 */
export function isFontRendered(family: string, measure: (font: string) => number | null): boolean {
  for (const generic of PROBE_GENERICS) {
    const withFamily = measure(`13px "${family}", ${generic}`);
    const fallback = measure(`13px ${generic}`);
    if (withFamily === null || fallback === null) return false;
    if (withFamily !== fallback) return true;
  }
  return false;
}

/** 브라우저 전용 결정 경로(xtermSetup에서만 부른다). */
export function resolveDefaultFontFamily(): string {
  if (typeof document === "undefined") return LATIN_FALLBACK_STACK;
  const ctx = document.createElement("canvas").getContext("2d");
  if (!ctx) return LATIN_FALLBACK_STACK;
  const measure = (font: string): number => {
    ctx.font = font;
    return ctx.measureText(PROBE_TEXT).width;
  };
  return pickDefaultFontFamily((family) => isFontRendered(family, measure));
}

/** 기본 lineHeight — Menlo 13px 기준 행 높이 21px. Nanum Gothic Coding은
 * 잉크가 em 상자를 거의 채워 같은 px 행간이라도 더 붙어 보인다. iTerm2
 * 실측(같은 텍스트, 줄 피치/글자 잉크 ≈ 1.66)에 맞춰 1.1 → 1.4로 완화했다. */
export const BASE_LINE_HEIGHT = 1.4;
/**
 * 목표 행 높이(글꼴 크기 배수): Menlo의 글자 높이(13px에서 15px) × 1.1.
 * xterm의 행 높이는 "글자 높이 × lineHeight"라 글꼴 메트릭을 따른다 —
 * Nanum Gothic Coding은 글자 높이가 1.0em(13px에서 13px)이라 같은 1.1로는
 * 행이 14.3px로 약 13% 좁아져 줄이 딱 붙어 보였다(WKWebView 실측).
 */
const TARGET_ROW_EM = (15 / 13) * BASE_LINE_HEIGHT;
const LINE_HEIGHT_PROBE_PX = 100;

/** 글자 높이(em)에서 목표 행 높이를 맞추는 lineHeight. 더 높은 글꼴은 기본값을 지킨다. */
export function lineHeightForCharHeight(charHeightEm: number | null): number {
  if (charHeightEm === null || !Number.isFinite(charHeightEm) || charHeightEm <= 0) return BASE_LINE_HEIGHT;
  return Math.max(BASE_LINE_HEIGHT, Math.round((TARGET_ROW_EM / charHeightEm) * 100) / 100);
}

/**
 * 글꼴 스택의 글자 높이(em) — xterm이 셀을 재는 방식처럼 DOM span의 높이로
 * 잰다. 반올림 오차를 줄이려고 큰 크기에서 잰다. 잴 수 없으면 null.
 */
function measureCharHeightEm(fontFamily: string): number | null {
  if (typeof document === "undefined" || !document.body) return null;
  const span = document.createElement("span");
  span.style.cssText = `position:absolute;visibility:hidden;white-space:pre;line-height:normal;font-size:${LINE_HEIGHT_PROBE_PX}px`;
  span.style.fontFamily = fontFamily;
  span.textContent = "W";
  document.body.appendChild(span);
  try {
    const height = span.getBoundingClientRect().height;
    return height > 0 ? height / LINE_HEIGHT_PROBE_PX : null;
  } finally {
    span.remove();
  }
}

/** 이 글꼴 스택에 쓸 xterm lineHeight(브라우저 전용, 잴 수 없으면 기본값). */
export function lineHeightForFont(fontFamily: string): number {
  return lineHeightForCharHeight(measureCharHeightEm(fontFamily));
}

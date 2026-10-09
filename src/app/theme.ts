/**
 * 테마 적용(04-ui.md §4 "기본 theme dark/light 두 가지").
 *
 * 설정값은 system|dark|light 세 가지이고, 실제로 칠하는 값은 dark|light
 * 둘뿐이다. 해석된 값을 <html data-theme>에 찍으면 CSS가 팔레트를 고르고
 * (workbench.css), 같은 값을 xterm 인스턴스에도 전달한다(xtermSetup).
 *
 * 순수 함수(resolveTheme)와 DOM/matchMedia 접촉을 분리해 node 시험에서
 * 규칙만 검증할 수 있게 했다.
 */

import { usePreferences, type ThemePreference } from "../store/preferences";

export type ResolvedTheme = "dark" | "light";

const LIGHT_QUERY = "(prefers-color-scheme: light)";

/** system이면 OS 취향을 따르고, 명시 선택은 그대로 쓴다. */
export function resolveTheme(preference: ThemePreference, systemPrefersLight: boolean): ResolvedTheme {
  if (preference === "dark" || preference === "light") return preference;
  return systemPrefersLight ? "light" : "dark";
}

/** matchMedia가 없는 환경(node 시험, 구형 webview)은 dark로 본다. */
export function systemPrefersLight(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return false;
  try {
    return window.matchMedia(LIGHT_QUERY).matches;
  } catch {
    return false;
  }
}

export function applyThemeAttribute(theme: ResolvedTheme): void {
  if (typeof document === "undefined") return;
  document.documentElement.dataset.theme = theme;
}

/**
 * 네이티브 창(타이틀바와 창 버튼)도 해석된 테마를 따르게 한다. CSS의
 * color-scheme은 웹 콘텐츠에만 적용되므로 이 호출이 없으면 OS가 다크
 * 모드일 때 라이트 앱 위에 어두운 창 버튼이 남는다. 브라우저 프리뷰처럼
 * Tauri 밖에서는 조용히 넘어간다(node 시험에서 모듈 로드도 피하려고
 * 동적 import를 쓴다).
 */
async function syncNativeWindowTheme(theme: ResolvedTheme): Promise<void> {
  if (typeof window === "undefined") return;
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().setTheme(theme);
  } catch {
    // 창이 없거나(브라우저) 권한이 없으면 웹 테마만으로 진행한다.
  }
}

/**
 * 설정 변경과 OS 취향 변경 모두를 구독해 해석된 테마를 반영한다.
 * 반환값은 해제 함수다(Workbench effect cleanup).
 */
export function startThemeSync(onResolved: (theme: ResolvedTheme) => void): () => void {
  const emit = (): void => {
    const theme = resolveTheme(usePreferences.getState().theme, systemPrefersLight());
    applyThemeAttribute(theme);
    void syncNativeWindowTheme(theme);
    onResolved(theme);
  };
  emit();

  // 테마와 무관한 설정 변경(글꼴 입력 한 글자 등)에 네이티브 창 테마 IPC를
  // 다시 부르지 않는다.
  const unsubscribe = usePreferences.subscribe((state, prev) => {
    if (state.theme !== prev.theme) emit();
  });
  let media: MediaQueryList | null = null;
  if (typeof window !== "undefined" && typeof window.matchMedia === "function") {
    try {
      media = window.matchMedia(LIGHT_QUERY);
      media.addEventListener("change", emit);
    } catch {
      media = null;
    }
  }
  return () => {
    unsubscribe();
    media?.removeEventListener("change", emit);
  };
}

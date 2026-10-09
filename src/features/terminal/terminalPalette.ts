/**
 * 터미널 팔레트 단일 원본(04-ui §4 "기본 theme dark/light 두 가지").
 *
 * xtermSetup(브라우저 전용)이 아니라 여기 두는 이유: pane 배경색 계산
 * (paneBackground.ts)이 테마 배경을 알아야 하는데, node 시험이 xterm
 * 모듈을 import하지 않고도 순수 계산을 검증할 수 있어야 한다. xtermSetup은
 * 이 팔레트를 재수출해 기존 import 경로(Workbench 등)를 그대로 유지한다.
 *
 * pane 배경을 테마와 다른 명암으로 덮어쓸 때 글자색을 고르는 판단
 * (readablePaletteFor)도 여기 둔다 — 두 팔레트를 다 아는 자리다.
 */

export const DARK_THEME = {
  background: "#1e1f24",
  foreground: "#d7dae0",
  cursor: "#e8eaed",
  cursorAccent: "#1e1f24",
  selectionBackground: "#3b5ea8",
  black: "#1e1f24",
  red: "#e06c75",
  green: "#98c379",
  yellow: "#e5c07b",
  blue: "#61afef",
  magenta: "#c678dd",
  cyan: "#56b6c2",
  white: "#d7dae0",
};

export const LIGHT_THEME = {
  // --bg-panel(라이트)과 같은 값 — dark 테마가 터미널 배경을 --bg-panel로
  // 쓰는 것과 대칭이다(workbench.css 라이트 팔레트 주석 참조).
  background: "#f7f5f1",
  foreground: "#24292f",
  cursor: "#24292f",
  cursorAccent: "#f7f5f1",
  selectionBackground: "#9db8e0",
  black: "#24292f",
  red: "#b3424a",
  green: "#3f7f3f",
  yellow: "#8a6d1a",
  blue: "#2456b3",
  magenta: "#7a4ba3",
  cyan: "#1f7a85",
  white: "#57606a",
};

export type TerminalThemeName = "dark" | "light";

export type TerminalPalette = typeof DARK_THEME;

export function terminalTheme(name: TerminalThemeName): TerminalPalette {
  return name === "light" ? LIGHT_THEME : DARK_THEME;
}

/** 체감 밝기(ITU-R BT.601)가 이 아래면 "어두운 면"으로 본다. */
const DARK_COLOR_BRIGHTNESS = 128;

/** 체감 밝기 0..255. "#rrggbb"가 아니면 null(판단하지 않는다). */
export function colorBrightness(hex: string): number | null {
  if (!/^#[0-9a-fA-F]{6}$/.test(hex)) return null;
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
  return 0.299 * r + 0.587 * g + 0.114 * b;
}

/** 어두운 면인가("#rrggbb"가 아니면 null). */
export function isDarkColor(hex: string): boolean | null {
  const brightness = colorBrightness(hex);
  return brightness === null ? null : brightness < DARK_COLOR_BRIGHTNESS;
}

/**
 * 이 배경 위에서 글자가 읽히는 팔레트 — 지금 글자색으로 충분하면 null.
 *
 * pane 배경을 테마와 다른 명암으로 덮어쓸 때 쓴다(paneBackground.ts): 밝은
 * 테마에서도 에이전트 tint는 어두운 면이라, 밝은 테마의 검은 글자를 그대로
 * 두면 글자가 배경에 묻힌다. 반대로 어두운 테마에 사용자가 밝은 배경을
 * 고른 경우도 같은 이유로 뒤집는다.
 */
export function readablePaletteFor(
  background: string,
  foreground: string,
): TerminalPalette | null {
  const darkBackground = isDarkColor(background);
  const darkForeground = isDarkColor(foreground);
  if (darkBackground === null || darkForeground === null) return null;
  if (darkBackground !== darkForeground) return null;
  return darkBackground ? DARK_THEME : LIGHT_THEME;
}

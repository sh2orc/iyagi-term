/**
 * 설정 화면의 정보 구조(단일 원본).
 *
 * 그룹 목록·항목 목록을 데이터로 두면 좌측 내비게이션, 설정 검색, 각 행의
 * 라벨/설명이 모두 같은 정의를 읽는다. 새 설정을 넣는 절차는
 *   ① store/preferences.ts에 값 추가 → ② 여기 SETTINGS_ITEMS에 한 줄 추가
 *   → ③ 해당 패널에 <SettingRow item={...}>
 * 세 단계다. i18n 키가 실제로 존재하는지는 schema.test.ts가 강제한다.
 */

export type SettingsGroupId =
  | "missions"
  | "general"
  | "terminal"
  | "trackpad"
  | "integrations"
  | "run"
  | "profiles"
  | "shortcuts"
  | "compatibility";

export interface SettingsGroupDef {
  id: SettingsGroupId;
  /** settings.<id> / settings.<id>Hint 규칙을 그대로 쓴다. */
  titleKey: string;
  hintKey: string;
}

export interface SettingsItemDef {
  /** DOM data-setting-id — 검색 결과에서 그 행으로 이동할 때 쓴다. */
  id: string;
  group: SettingsGroupId;
  labelKey: string;
  descriptionKey: string;
  /** 라벨·설명에 없지만 사용자가 검색할 법한 낱말(한/영 모두). */
  keywords?: readonly string[];
}

const group = (id: SettingsGroupId): SettingsGroupDef => ({
  id,
  titleKey: `settings.${id}`,
  hintKey: `settings.${id}Hint`,
});

/** 좌측 내비게이션 순서 = 이 배열 순서. 자주 쓰는 것부터. */
export const SETTINGS_GROUPS: readonly SettingsGroupDef[] = [
  group("general"),
  group("terminal"),
  group("trackpad"),
  group("integrations"),
  group("missions"),
  group("run"),
  group("profiles"),
  group("shortcuts"),
  group("compatibility"),
];

const item = (
  id: string,
  itemGroup: SettingsGroupId,
  keywords?: readonly string[],
): SettingsItemDef => ({
  id,
  group: itemGroup,
  labelKey: `settings.item.${id}.label`,
  descriptionKey: `settings.item.${id}.description`,
  keywords,
});

export const SETTINGS_ITEMS: readonly SettingsItemDef[] = [
  // 순서 = 화면 순서(MissionSettings: 빠른 설정 → 사용법 → 모델 → 팀 → 검증). id는 그 요소의 data-setting-id다.
  item("missionQuickSetup", "missions", [
    "quick setup",
    "빠른 설정",
    "setup",
    "로그인",
    "login",
    "팀 만들기",
    "create team",
    "cli",
    "codex",
    "claude",
    "opencode",
    "AI 작업",
    "mission",
    "미션",
  ]),
  item("missionGuide", "missions", ["guide", "help", "사용법", "도움말", "how to", "AI 작업", "mission", "미션"]),
  item("missionModels", "missions", ["orchestration", "오케스트레이션", "모델", "model", "codex", "claude", "opencode", "z.ai", "zai", "glm"]),
  item("missionTeams", "missions", [
    "team",
    "팀",
    "lead",
    "builder",
    "reviewer",
    "integrator",
    "충돌 해결",
    "conflict",
    "agent",
    "에이전트",
  ]),
  item("missionVerification", "missions", ["verify", "검증", "test", "명령"]),
  item("language", "general", ["language", "언어", "korean", "english", "한국어"]),
  item("theme", "general", ["theme", "테마", "dark", "light", "다크", "라이트", "색"]),
  item("fontSize", "terminal", ["font", "글꼴", "폰트", "크기", "zoom", "확대"]),
  item("fontFamily", "terminal", ["font", "글꼴", "폰트", "family", "서체"]),
  item("cursorStyle", "terminal", ["cursor", "커서", "block", "underline", "bar", "커서 모양"]),
  item("hangulToggle", "terminal", ["한영", "한/영", "hangul", "korean", "ime", "shift", "space", "입력기", "전환", "한글"]),
  item("scrollback", "terminal", ["scrollback", "스크롤백", "기록", "줄"]),
  item("osc52", "terminal", ["osc52", "clipboard", "클립보드", "복사", "터미널에서 복사"]),
  item("gpuRenderer", "terminal", ["gpu", "webgl", "renderer", "렌더러", "박스", "테두리", "끊김"]),
  item("agentBackgrounds", "terminal", [
    "background",
    "배경",
    "색",
    "tint",
    "에이전트",
    "agent",
    "claude",
    "zai",
    "codex",
    "opencode",
    "구분",
  ]),
  item("agentBackgroundColors", "terminal", [
    "background",
    "배경",
    "색",
    "color",
    "picker",
    "tint",
    "에이전트",
    "agent",
    "claude",
    "zai",
    "codex",
    "opencode",
    "사용자 지정",
  ]),
  item("interventionTextBadge", "terminal", ["intervention", "개입", "승인", "badge", "패턴", "감지"]),
  item("defaultShell", "terminal", ["shell", "셸", "bash", "zsh", "powershell", "기본"]),
  item("customShell", "terminal", ["shell", "셸", "custom", "사용자", "추가"]),
  item("terminateOnClose", "terminal", ["close", "닫기", "종료", "terminate", "확인"]),
  item("quitBehavior", "terminal", ["quit", "exit", "종료", "앱 종료", "트레이", "tray", "백그라운드", "background"]),
  item("tabSwipe", "trackpad", [
    "trackpad",
    "트랙패드",
    "swipe",
    "스와이프",
    "gesture",
    "제스처",
    "two finger",
    "두 손가락",
    "tab",
    "탭 전환",
  ]),
  item("tabSwipeSensitivity", "trackpad", ["sensitivity", "감도", "threshold", "문턱", "거리"]),
  item("tabSwipeReverse", "trackpad", ["reverse", "반전", "방향", "natural scrolling", "자연스러운 스크롤"]),
  item("tabSwipeWrap", "trackpad", ["wrap", "순환", "끝", "처음", "마지막", "cycle"]),
  item("tabSwitchEffect", "trackpad", ["animation", "애니메이션", "전환", "효과", "transition", "슬라이드", "slide", "motion", "모션"]),
  item("claudeFullAutonomy", "integrations", ["claude", "자율", "권한", "autonomy", "permissions"]),
  item("codexFullAutonomy", "integrations", ["codex", "자율", "권한", "autonomy", "permissions"]),
  item("codexUsage", "integrations", ["codex", "subscription", "구독", "사용량", "quota"]),
  item("claudeUsage", "integrations", ["claude", "statusline", "구독", "사용량", "quota"]),
  item("zaiCodingPlan", "integrations", [
    "z.ai",
    "zai",
    "glm",
    "coding plan",
    "api key",
    "사용량",
    "route",
    "routing",
    "라우팅",
    "provider",
    "제공자",
    "claude",
    "model",
    "모델",
  ]),
  // 두 hook 카드. id는 IntegrationPanel의 `data-setting-id`와 같아야
  // 검색 결과에서 그 카드로 스크롤된다(HooksPanel의 settingId props).
  item("claudeHooks", "integrations", [
    "claude",
    "hook",
    "훅",
    "notification",
    "승인",
    "session",
    "세션",
    "resume",
    "재개",
    "settings.json",
  ]),
  item("codexHooks", "integrations", [
    "codex",
    "hook",
    "훅",
    "session",
    "세션",
    "resume",
    "재개",
    "hooks.json",
  ]),
  // 실행 프로필 패널 맨 아래의 ShellProfilesCard(data-setting-id와 같은 id).
  item("shellProfiles", "profiles", ["ccd", "ccg", "zsh", "zshrc", "alias", "shell", "셸", "함수", "z.ai", "glm"]),
];

export function itemsInGroup(id: SettingsGroupId): SettingsItemDef[] {
  return SETTINGS_ITEMS.filter((entry) => entry.group === id);
}

export function groupById(id: SettingsGroupId): SettingsGroupDef {
  const found = SETTINGS_GROUPS.find((entry) => entry.id === id);
  if (!found) throw new Error(`unknown settings group: ${id}`);
  return found;
}

export type Translate = (key: string) => string;

function normalize(value: string): string {
  return value.trim().toLowerCase();
}

function haystack(values: readonly string[]): string {
  return normalize(values.join(" "));
}

/** 항목 검색: 라벨·설명·키워드·소속 그룹 이름까지 본다. */
export function searchItems(query: string, t: Translate): SettingsItemDef[] {
  const needle = normalize(query);
  if (needle.length === 0) return [];
  return SETTINGS_ITEMS.filter((entry) =>
    haystack([
      entry.id,
      t(entry.labelKey),
      t(entry.descriptionKey),
      t(groupById(entry.group).titleKey),
      ...(entry.keywords ?? []),
    ]).includes(needle),
  );
}

/**
 * 그룹 검색: 그룹 자신의 이름·설명이 맞거나, 속한 항목 중 하나가 맞으면
 * 목록에 남긴다. 폼이 통째로 들어간 그룹(관리 실행·프로필·호환성)은
 * 항목 정의가 없으므로 이름·설명으로만 걸린다.
 */
export function searchGroups(query: string, t: Translate): SettingsGroupDef[] {
  const needle = normalize(query);
  if (needle.length === 0) return [...SETTINGS_GROUPS];
  const hit = new Set(searchItems(query, t).map((entry) => entry.group));
  return SETTINGS_GROUPS.filter(
    (entry) =>
      hit.has(entry.id) ||
      haystack([entry.id, t(entry.titleKey), t(entry.hintKey)]).includes(needle),
  );
}

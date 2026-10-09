/**
 * 두벌식(표준 2-set) 한글 오토마타 — IME가 개입하지 않은 자모 키를 음절로
 * 조립한다(순수 함수·node 시험 가능).
 *
 * 왜 필요한가: macOS WKWebView는 앱 시작 직후·입력 소스 전환 직후 한동안
 * 한글 입력 소스의 키를 IME에 넘기지 않고, 키보드 레이아웃 문자(`key="ㅇ"`)를
 * 실제 keyCode(68)와 함께 keydown으로 그대로 흘린다(실기 트레이스: 시작 후
 * 수 초~수십 초, 또는 CapsLock 뒤 첫 2~3키). xterm 6.0은 인쇄 가능한 단일
 * 문자 keydown을 그대로 PTY에 보내므로 셸에는 "ㅇㅏㄴㄴㅕㅇ" 같은 자모 열이
 * 찍힌다. 브리지는 이런 keydown을 가로채 이 오토마타로 음절을 만들고, 조합
 * 진행을 DEL+재쓰기(꼬리 편집)로 PTY에 즉시 반영한다 — IME가 없는 네이티브
 * 터미널이 하는 일과 같다.
 *
 * 규칙(표준 두벌식):
 *  - 초성 뒤 모음 → 중성. 중성 뒤 모음 → 복합 모음(ㅗ+ㅏ=ㅘ …)이면 결합,
 *    아니면 앞 음절 확정 후 새 음절(모음 단독).
 *  - 중성 뒤 자음 → 종성(ㄸ·ㅃ·ㅉ는 종성 불가 → 새 음절). 종성 뒤 자음 →
 *    복합 종성(ㄱ+ㅅ=ㄳ …)이면 결합, 아니면 앞 음절 확정 후 새 음절.
 *  - 종성 뒤 모음 → 받침 이동: (복합이면 뒷부분만) 종성이 새 음절의 초성이
 *    된다(안+ㅏ → 아나, 닭+ㅏ → 달가).
 *  - Backspace → 마지막 자모 키 하나를 되돌린다(복합도 한 단계씩: 닭→달→다).
 *  - 음절이 비면 Backspace는 오토마타 밖(xterm의 DEL)으로 넘긴다.
 *
 * 출력은 PTY에 보낼 꼬리 편집 문자열이다: 직전 조합 중 음절을 DEL로 걷어내고
 * (확정된 앞 음절이 바뀌었으면 그것도) 새 상태를 다시 쓴다.
 */

/** PTY backspace. */
const DEL = "\x7f";

const CHO = [
  "ㄱ", "ㄲ", "ㄴ", "ㄷ", "ㄸ", "ㄹ", "ㅁ", "ㅂ", "ㅃ", "ㅅ",
  "ㅆ", "ㅇ", "ㅈ", "ㅉ", "ㅊ", "ㅋ", "ㅌ", "ㅍ", "ㅎ",
] as const;
const JUNG = [
  "ㅏ", "ㅐ", "ㅑ", "ㅒ", "ㅓ", "ㅔ", "ㅕ", "ㅖ", "ㅗ", "ㅘ", "ㅙ",
  "ㅚ", "ㅛ", "ㅜ", "ㅝ", "ㅞ", "ㅟ", "ㅠ", "ㅡ", "ㅢ", "ㅣ",
] as const;
/** 종성표 — 인덱스 0은 "없음". */
const JONG = [
  "", "ㄱ", "ㄲ", "ㄳ", "ㄴ", "ㄵ", "ㄶ", "ㄷ", "ㄹ", "ㄺ", "ㄻ", "ㄼ", "ㄽ", "ㄾ",
  "ㄿ", "ㅀ", "ㅁ", "ㅂ", "ㅄ", "ㅅ", "ㅆ", "ㅇ", "ㅈ", "ㅊ", "ㅋ", "ㅌ", "ㅍ", "ㅎ",
] as const;

const COMPOUND_JUNG: Record<string, string> = {
  "ㅗㅏ": "ㅘ", "ㅗㅐ": "ㅙ", "ㅗㅣ": "ㅚ",
  "ㅜㅓ": "ㅝ", "ㅜㅔ": "ㅞ", "ㅜㅣ": "ㅟ",
  "ㅡㅣ": "ㅢ",
};
const COMPOUND_JONG: Record<string, string> = {
  "ㄱㅅ": "ㄳ",
  "ㄴㅈ": "ㄵ", "ㄴㅎ": "ㄶ",
  "ㄹㄱ": "ㄺ", "ㄹㅁ": "ㄻ", "ㄹㅂ": "ㄼ", "ㄹㅅ": "ㄽ", "ㄹㅌ": "ㄾ", "ㄹㅍ": "ㄿ", "ㄹㅎ": "ㅀ",
  "ㅂㅅ": "ㅄ",
};
/** 복합 종성 → [남는 종성, 이동하는 초성] (받침 이동용). */
const SPLIT_JONG: Record<string, [string, string]> = Object.fromEntries(
  Object.entries(COMPOUND_JONG).map(([pair, compound]) => [compound, [pair[0], pair[1]]]),
);
/** 종성이 될 수 없는 자음(두벌식). */
const NOT_JONG = new Set(["ㄸ", "ㅃ", "ㅉ"]);

const CHO_INDEX = new Map<string, number>(CHO.map((c, i) => [c, i]));
const JUNG_INDEX = new Map<string, number>(JUNG.map((v, i) => [v, i]));
const JONG_INDEX = new Map<string, number>(JONG.map((c, i) => [c, i]));

/** 호환 자모(U+3131–U+3163) 한 글자인가 — 키보드가 내는 자모의 범위. */
export function isCompatJamo(ch: string | undefined | null): ch is string {
  return typeof ch === "string" && ch.length === 1 && ch >= "ㄱ" && ch <= "ㅣ";
}

function isVowel(jamo: string): boolean {
  return JUNG_INDEX.has(jamo);
}

/**
 * 두벌식 자판의 라틴 키 → 자모. 한/영 전환 경주 동안 레이아웃 전환 전에
 * 도착한 라틴 keydown을 사용자의 의도(한글)대로 되살릴 때 쓴다.
 */
const LATIN_TO_JAMO: Record<string, string> = {
  q: "ㅂ", w: "ㅈ", e: "ㄷ", r: "ㄱ", t: "ㅅ", y: "ㅛ", u: "ㅕ", i: "ㅑ", o: "ㅐ", p: "ㅔ",
  a: "ㅁ", s: "ㄴ", d: "ㅇ", f: "ㄹ", g: "ㅎ", h: "ㅗ", j: "ㅓ", k: "ㅏ", l: "ㅣ",
  z: "ㅋ", x: "ㅌ", c: "ㅊ", v: "ㅍ", b: "ㅠ", n: "ㅜ", m: "ㅡ",
  Q: "ㅃ", W: "ㅉ", E: "ㄸ", R: "ㄲ", T: "ㅆ", O: "ㅒ", P: "ㅖ",
};
export function latinToJamo(ch: string): string | null {
  if (ch.length !== 1) return null;
  return LATIN_TO_JAMO[ch] ?? LATIN_TO_JAMO[ch.toLowerCase()] ?? null;
}

interface Syllable {
  cho: string | null;
  jung: string | null;
  jong: string | null;
  /** 이 음절에 받아들인 자모 키 순서 — Backspace가 한 단계씩 되돌린다. */
  history: string[];
}

function emptySyllable(): Syllable {
  return { cho: null, jung: null, jong: null, history: [] };
}

function composeSyllable(s: Syllable): string {
  if (s.cho !== null && s.jung !== null) {
    const cho = CHO_INDEX.get(s.cho) ?? 0;
    const jung = JUNG_INDEX.get(s.jung) ?? 0;
    const jong = s.jong === null ? 0 : (JONG_INDEX.get(s.jong) ?? 0);
    return String.fromCharCode(0xac00 + (cho * 21 + jung) * 28 + jong);
  }
  return s.cho ?? s.jung ?? "";
}

/**
 * 자모 하나를 현재 음절에 넣는다. 반환: 앞 음절이 확정되면 그 텍스트
 * (받침 이동으로 바뀐 형태일 수 있다), 아니면 null. `syllable`은 제자리에서
 * 갱신되며 확정 시 새 음절로 교체된다.
 */
function feedSyllable(state: { syllable: Syllable }, jamo: string): string | null {
  const s = state.syllable;
  const commitAndStart = (next: Syllable): string => {
    const committed = composeSyllable(s);
    state.syllable = next;
    return committed;
  };
  if (isVowel(jamo)) {
    if (s.jung === null) {
      // 빈 음절 또는 초성만 → 중성.
      s.jung = jamo;
      s.history.push(jamo);
      return null;
    }
    if (s.jong === null) {
      const compound = COMPOUND_JUNG[s.jung + jamo];
      if (compound) {
        s.jung = compound;
        s.history.push(jamo);
        return null;
      }
      return commitAndStart({ cho: null, jung: jamo, jong: null, history: [jamo] });
    }
    // 받침 이동: (복합 종성이면 뒷부분만) 새 음절의 초성이 된다.
    const split = SPLIT_JONG[s.jong];
    const moved = split ? split[1] : s.jong;
    s.jong = split ? split[0] : null;
    if (!s.history.length || s.history[s.history.length - 1] !== moved) {
      // 복합 종성 ㄲ/ㅆ처럼 한 키인 경우는 history 마지막이 그 자모다.
    }
    s.history.pop();
    return commitAndStart({ cho: moved, jung: jamo, jong: null, history: [moved, jamo] });
  }
  // 자음
  if (s.cho === null && s.jung === null) {
    s.cho = jamo;
    s.history.push(jamo);
    return null;
  }
  if (s.jung === null) {
    // 초성만 있는데 또 자음 → 앞 자음 확정, 새 초성.
    return commitAndStart({ cho: jamo, jung: null, jong: null, history: [jamo] });
  }
  if (s.cho === null) {
    // 모음 단독 음절 뒤 자음 → 새 음절(모음 단독은 종성을 받지 않는다).
    return commitAndStart({ cho: jamo, jung: null, jong: null, history: [jamo] });
  }
  if (s.jong === null) {
    if (NOT_JONG.has(jamo)) {
      return commitAndStart({ cho: jamo, jung: null, jong: null, history: [jamo] });
    }
    s.jong = jamo;
    s.history.push(jamo);
    return null;
  }
  const compound = COMPOUND_JONG[s.jong + jamo];
  if (compound) {
    s.jong = compound;
    s.history.push(jamo);
    return null;
  }
  return commitAndStart({ cho: jamo, jung: null, jong: null, history: [jamo] });
}

function charCount(text: string): number {
  return Array.from(text).length;
}

/**
 * 조합 상태기계 + PTY 꼬리 편집 출력. 현재 음절(미확정)의 텍스트는 이미 PTY에
 * 나가 있다(즉시 반영) — 조합이 바뀌면 그만큼 DEL 하고 다시 쓴다.
 */
export class HangulAutomaton {
  private state: { syllable: Syllable } = { syllable: emptySyllable() };

  /** 조합 중인(아직 확정되지 않은) 음절 텍스트 — 비어 있으면 조합 없음. */
  get current(): string {
    return composeSyllable(this.state.syllable);
  }

  /** 마지막으로 받아들인 자모(현재 음절 기준) — 늦은 IME 삽입의 중복 판정용. */
  get lastJamo(): string | null {
    const h = this.state.syllable.history;
    return h.length ? h[h.length - 1] : null;
  }

  /**
   * 자모 하나를 넣고 PTY에 보낼 꼬리 편집을 돌려준다.
   * 앞 음절이 그대로 확정되면 새 음절만 덧붙이고, 받침 이동으로 앞 음절이
   * 바뀌면 그것부터 다시 쓴다.
   */
  feed(jamo: string): string {
    const prev = this.current;
    const committed = feedSyllable(this.state, jamo);
    const next = this.current;
    if (committed === null) return DEL.repeat(charCount(prev)) + next;
    if (committed === prev) return next;
    return DEL.repeat(charCount(prev)) + committed + next;
  }

  /**
   * Backspace: 현재 음절의 마지막 자모를 되돌린다. 조합 중이 아니면 null
   * (호출자가 일반 Backspace로 처리한다).
   */
  backspace(): string | null {
    const s = this.state.syllable;
    if (s.history.length === 0) return null;
    const prev = this.current;
    const keys = s.history.slice(0, -1);
    const rebuilt: { syllable: Syllable } = { syllable: emptySyllable() };
    for (const k of keys) feedSyllable(rebuilt, k);
    this.state = rebuilt;
    return DEL.repeat(charCount(prev)) + this.current;
  }

  /** 현재 음절을 확정한다(텍스트는 이미 PTY에 있다 — 상태만 비운다). */
  commit(): void {
    this.state = { syllable: emptySyllable() };
  }
}

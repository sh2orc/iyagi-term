/**
 * macOS 메뉴 막대의 단축키 되울림 거르기(04-ui §3-2).
 *
 * WKWebView는 Cmd 조합을 먼저 웹 페이지에 보내고, 페이지가 preventDefault하지 않은 keydown을 앱
 * 메뉴에 다시 보낸다(키 등가물). 앱 단축키는 페이지(Workbench의 중앙 key handler)가 가드 — 모달·IME
 * 조합·key repeat·탭 이름 편집·설정/배치 편집 화면 — 와 함께 판정하므로, 페이지가 일부러 흘려보낸
 * 조합을 메뉴가 되받아 실행하면 가드가 뚫린다(예: 길게 누른 Cmd+D가 창을 계속 만든다).
 *
 * 그래서 Cmd/Ctrl이 섞인 keydown을 capture 단계에서 적어 두고, 앱 단축키와 같은 조합을 단 메뉴
 * 항목이 불리면 "페이지가 보고도 소비하지 않은 같은 조합"이 남아 있는지 본다. 있으면 되울림이다 —
 * 기록 하나를 쓰고 실행하지 않는다. defaultPrevented는 메뉴가 불린 뒤(= 디스패치가 끝난 뒤)에
 * 읽으므로 모든 리스너의 결정이 반영돼 있다. 마우스로 고른 항목은 기록이 없어 그대로 실행된다.
 *
 * 페이지는 물리 키(code)로, AppKit은 메뉴 조합을 글자로 맞춘다 — Dvorak·독일어 자판에서는 둘이
 * 다르다. 그래서 Cmd/Ctrl이 같고 물리 키나 글자 가운데 하나가 같으면 같은 조합으로 본다. 숫자·기호·
 * 기능 키는 자판에 따라 Shift를 눌러야 그 글자가 나오고(독일어 "="는 Shift+0) AppKit도 그런 조합에서는
 * Shift를 따지지 않으므로, Shift는 영문자 조합에서만 비교한다(⌘D와 ⇧⌘D는 다른 항목이다). 메뉴는
 * 페이지에 없는 키보드 동작을 새로 만들지 않는다(메뉴만 가진 조합 — 설정·종료 — 은 이 규칙 밖이다).
 */

import type { KeyBinding } from "../features/terminal/shortcuts";

/**
 * 되울림이 도착하기를 기다리는 시간 — 넘으면 그 keydown과 무관한 선택으로 본다. 웹뷰가 바쁘면 되울림이
 * 늦게 닿으므로 넉넉히 잡는다(그 사이 같은 항목을 마우스로 고르는 일은 드물다).
 */
export const KEY_ECHO_WINDOW_MS = 1000;

/** 기록 상한(길게 누름 반복을 담기에 충분하다). */
const MAX_RECORDS = 32;

export interface EchoKeyEvent {
  code: string;
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  readonly defaultPrevented: boolean;
}

export interface KeyEchoFilter {
  /** window keydown(capture)마다 부른다. */
  record(event: EchoKeyEvent): void;
  /** 이 조합의 메뉴 항목이 불렸다: 페이지가 흘려보낸 같은 조합의 되울림이면 true(기록 하나를 쓴다). */
  consumeEcho(binding: KeyBinding): boolean;
}

const SYMBOL_CHARACTERS: Readonly<Record<string, string>> = {
  Equal: "=",
  Minus: "-",
  Comma: ",",
  Period: ".",
  Slash: "/",
  Semicolon: ";",
  Quote: "'",
  Backquote: "`",
  Backslash: "\\",
  BracketLeft: "[",
  BracketRight: "]",
};

/** 메뉴 조합이 네이티브에서 맞춰지는 글자(muda가 물리 code를 이 글자로 등록한다). 글자가 없는 키는 null. */
export function acceleratorCharacter(code: string): string | null {
  const letter = /^Key([A-Z])$/.exec(code)?.[1];
  if (letter) return letter.toLowerCase();
  const digit = /^Digit([0-9])$/.exec(code)?.[1];
  if (digit) return digit;
  return SYMBOL_CHARACTERS[code] ?? null;
}

export function createKeyEchoFilter(now: () => number = defaultNow): KeyEchoFilter {
  let records: Array<{ event: EchoKeyEvent; at: number }> = [];
  const prune = (at: number) => {
    records = records.filter((entry) => at - entry.at <= KEY_ECHO_WINDOW_MS);
  };
  return {
    record(event) {
      // 메뉴 막대 조합은 Cmd(또는 Ctrl)를 쓴다 — 맨 글자 입력은 적지 않는다.
      if (!event.metaKey && !event.ctrlKey) return;
      const at = now();
      prune(at);
      records.push({ event, at });
      if (records.length > MAX_RECORDS) records.splice(0, records.length - MAX_RECORDS);
    },
    consumeEcho(binding) {
      prune(now());
      const character = acceleratorCharacter(binding.code);
      const compareShift = /^Key[A-Z]$/.test(binding.code);
      // 길게 누른 반복은 keydown마다 되울림이 하나씩 온다 — 가장 오래된 것부터 짝짓는다.
      const index = records.findIndex(
        ({ event }) =>
          !event.defaultPrevented &&
          !event.altKey &&
          event.ctrlKey === binding.ctrl &&
          event.metaKey === binding.meta &&
          (!compareShift || event.shiftKey === binding.shift) &&
          (event.code === binding.code || (character !== null && event.key.toLowerCase() === character)),
      );
      if (index < 0) return false;
      records.splice(index, 1);
      return true;
    },
  };
}

function defaultNow(): number {
  return typeof performance !== "undefined" ? performance.now() : Date.now();
}

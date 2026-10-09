/**
 * 메뉴 막대 단축키 되울림 거르기: 페이지가 보고도 소비하지 않은 Cmd 조합만 되울림으로 보고
 * keydown 하나에 한 번만 삼킨다. 페이지가 소비했거나 keydown이 없던 선택(마우스)은 실행한다.
 * 페이지는 물리 키로, 메뉴는 글자로 조합을 맞추므로 둘 중 하나만 같아도 같은 조합이다.
 */

import { describe, expect, it } from "vitest";
import type { KeyBinding } from "../features/terminal/shortcuts";
import { acceleratorCharacter, createKeyEchoFilter, KEY_ECHO_WINDOW_MS, type EchoKeyEvent } from "./menuKeyEcho";

const cmdD: KeyBinding = { code: "KeyD", ctrl: false, meta: true, shift: false };

function keydown(patch: Partial<EchoKeyEvent> = {}): EchoKeyEvent {
  return {
    code: "KeyD",
    key: "d",
    ctrlKey: false,
    metaKey: true,
    altKey: false,
    shiftKey: false,
    defaultPrevented: false,
    ...patch,
  };
}

describe("menu key echo filter", () => {
  it("treats a Cmd combo the page saw but did not consume as the menu's echo, once per keydown", () => {
    let now = 0;
    const filter = createKeyEchoFilter(() => now);
    filter.record(keydown());
    now = 20;
    expect(filter.consumeEcho(cmdD)).toBe(true);
    expect(filter.consumeEcho(cmdD)).toBe(false);
  });

  it("runs the item when the page consumed the keydown or no keydown preceded it (mouse)", () => {
    const filter = createKeyEchoFilter(() => 0);
    expect(filter.consumeEcho(cmdD)).toBe(false);
    filter.record(keydown({ defaultPrevented: true }));
    expect(filter.consumeEcho(cmdD)).toBe(false);
  });

  it("reads defaultPrevented when the menu fires, after every listener has decided", () => {
    const filter = createKeyEchoFilter(() => 0);
    const event = { ...keydown(), defaultPrevented: false };
    filter.record(event);
    // capture 단계에서 적은 뒤, 뒤따른 리스너(중앙 key handler)가 소비했다.
    event.defaultPrevented = true;
    expect(filter.consumeEcho(cmdD)).toBe(false);
  });

  it("requires the same Cmd/Ctrl, no Option, and the same key", () => {
    const filter = createKeyEchoFilter(() => 0);
    filter.record(keydown({ shiftKey: true, key: "D" }));
    filter.record(keydown({ altKey: true }));
    filter.record(keydown({ code: "KeyE", key: "e" }));
    filter.record(keydown({ metaKey: false, ctrlKey: true }));
    // 영문자는 Shift가 다른 조합을 만든다(⌘D ≠ ⇧⌘D).
    expect(filter.consumeEcho(cmdD)).toBe(false);
    expect(filter.consumeEcho({ ...cmdD, shift: true })).toBe(true);
    expect(filter.consumeEcho({ code: "KeyD", ctrl: true, meta: false, shift: false })).toBe(true);
  });

  it("also matches by the typed character, which is how AppKit matches menu shortcuts", () => {
    const filter = createKeyEchoFilter(() => 0);
    // Dvorak: D라고 적힌 키는 물리 KeyH다 — 페이지는 흘려보냈고 메뉴는 글자 "d"로 맞춘다.
    filter.record(keydown({ code: "KeyH", key: "d" }));
    expect(filter.consumeEcho(cmdD)).toBe(true);
    // 독일어 자판: "-"는 물리 Slash다.
    filter.record(keydown({ code: "Slash", key: "-" }));
    expect(filter.consumeEcho({ code: "Minus", ctrl: false, meta: true, shift: false })).toBe(true);
    // Shift가 붙은 대문자도 같은 글자다.
    filter.record(keydown({ code: "KeyH", key: "D", shiftKey: true }));
    expect(filter.consumeEcho({ ...cmdD, shift: true })).toBe(true);
    // 물리 키도 글자도 다르면 다른 조합이다.
    filter.record(keydown({ code: "KeyH", key: "h" }));
    expect(filter.consumeEcho(cmdD)).toBe(false);
  });

  it("ignores Shift for digits and symbols, which some layouts need Shift to type", () => {
    const filter = createKeyEchoFilter(() => 0);
    // 독일어 자판: "="는 Shift+0이다 — 메뉴의 ⌘=(글자 "=")가 그 조합으로 불린다.
    filter.record(keydown({ code: "Digit0", key: "=", shiftKey: true }));
    expect(filter.consumeEcho({ code: "Equal", ctrl: false, meta: true, shift: false })).toBe(true);
    // 프랑스어 자판: 숫자는 Shift를 눌러야 나온다.
    filter.record(keydown({ code: "Digit0", key: "0", shiftKey: true }));
    expect(filter.consumeEcho({ code: "Digit0", ctrl: false, meta: true, shift: false })).toBe(true);
    // Cmd/Ctrl은 여전히 같아야 한다.
    filter.record(keydown({ code: "Digit0", key: "0", metaKey: false, ctrlKey: true }));
    expect(filter.consumeEcho({ code: "Digit0", ctrl: false, meta: true, shift: false })).toBe(false);
  });

  it("knows the character each menu key code is registered as", () => {
    expect(acceleratorCharacter("KeyD")).toBe("d");
    expect(acceleratorCharacter("Digit0")).toBe("0");
    expect(acceleratorCharacter("Equal")).toBe("=");
    expect(acceleratorCharacter("BracketLeft")).toBe("[");
    expect(acceleratorCharacter("F5")).toBeNull();
  });

  it("forgets keydowns older than the echo window", () => {
    let now = 0;
    const filter = createKeyEchoFilter(() => now);
    filter.record(keydown());
    now = KEY_ECHO_WINDOW_MS + 1;
    expect(filter.consumeEcho(cmdD)).toBe(false);
  });

  it("absorbs every echo of a held key (key repeat), not just the last one", () => {
    let now = 0;
    const filter = createKeyEchoFilter(() => now);
    for (let i = 0; i < 3; i += 1) {
      filter.record(keydown());
      now += 30;
    }
    const results = [1, 2, 3, 4].map(() => filter.consumeEcho(cmdD));
    expect(results).toEqual([true, true, true, false]);
  });

  it("does not record plain typing", () => {
    const filter = createKeyEchoFilter(() => 0);
    filter.record(keydown({ metaKey: false }));
    expect(filter.consumeEcho({ code: "KeyD", ctrl: false, meta: false, shift: false })).toBe(false);
  });
});

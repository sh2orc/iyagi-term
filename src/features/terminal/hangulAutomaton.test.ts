import { describe, expect, it } from "vitest";
import { HangulAutomaton, isCompatJamo, latinToJamo } from "./hangulAutomaton";

const DEL = "\x7f";

/** PTY 바이트열에 DEL을 적용해 셸이 보여줄 줄을 복원한다. */
function shell(stream: string): string {
  const line: string[] = [];
  for (const ch of Array.from(stream)) {
    if (ch === DEL) line.pop();
    else line.push(ch);
  }
  return line.join("");
}

function type(keys: string, automaton = new HangulAutomaton()): { stream: string; automaton: HangulAutomaton } {
  let stream = "";
  for (const k of Array.from(keys)) {
    stream += k === "<" ? (automaton.backspace() ?? "") : automaton.feed(k);
  }
  return { stream, automaton };
}

describe("HangulAutomaton — 두벌식 조합", () => {
  it("자모 열을 음절로 조립한다(안녕하세요)", () => {
    const { stream } = type("ㅇㅏㄴㄴㅕㅇㅎㅏㅅㅔㅇㅛ");
    expect(shell(stream)).toBe("안녕하세요");
  });

  it("조합 진행은 꼬리 편집(DEL+재쓰기)로 즉시 반영되고 확정된 음절은 다시 쓰지 않는다", () => {
    const a = new HangulAutomaton();
    expect(a.feed("ㅇ")).toBe("ㅇ");
    expect(a.feed("ㅏ")).toBe(`${DEL}아`);
    expect(a.feed("ㄴ")).toBe(`${DEL}안`);
    expect(a.feed("ㄴ")).toBe("ㄴ"); // 안 확정(불변) + 새 음절 ㄴ
    expect(a.feed("ㅕ")).toBe(`${DEL}녀`);
    expect(a.feed("ㅇ")).toBe(`${DEL}녕`);
    expect(a.current).toBe("녕");
  });

  it("받침 이동: 종성이 다음 음절의 초성이 되면 앞 음절부터 다시 쓴다(안+ㅏ → 아나)", () => {
    const a = new HangulAutomaton();
    let s = a.feed("ㅇ") + a.feed("ㅏ") + a.feed("ㄴ");
    expect(shell(s)).toBe("안");
    const move = a.feed("ㅏ");
    expect(move).toBe(`${DEL}아나`);
    s += move;
    expect(shell(s)).toBe("아나");
  });

  it("복합 종성의 받침 이동은 뒷부분만 넘긴다(닭+ㅏ → 달가, 없+ㅓ → 업서)", () => {
    expect(shell(type("ㄷㅏㄹㄱㅏ").stream)).toBe("달가");
    expect(shell(type("ㅇㅓㅂㅅㅓ").stream)).toBe("업서");
  });

  it("복합 모음·복합 종성을 결합한다(왜·워·의·값·앉·닭)", () => {
    expect(shell(type("ㅇㅗㅐ").stream)).toBe("왜");
    expect(shell(type("ㅇㅜㅓ").stream)).toBe("워");
    expect(shell(type("ㅇㅡㅣ").stream)).toBe("의");
    expect(shell(type("ㄱㅏㅂㅅ").stream)).toBe("값");
    expect(shell(type("ㅇㅏㄴㅈ").stream)).toBe("앉");
    expect(shell(type("ㄷㅏㄹㄱ").stream)).toBe("닭");
  });

  it("결합할 수 없는 모음·자음은 앞 음절을 확정하고 새 음절을 연다", () => {
    expect(shell(type("ㅇㅏㅏ").stream)).toBe("아ㅏ"); // ㅏ+ㅏ 결합 불가
    expect(shell(type("ㄱㄱ").stream)).toBe("ㄱㄱ"); // 초성 뒤 자음
    expect(shell(type("ㅏㄱ").stream)).toBe("ㅏㄱ"); // 모음 단독 뒤 자음은 종성이 아니다
    expect(shell(type("ㅇㅏㄸ").stream)).toBe("아ㄸ"); // ㄸ은 종성 불가
    expect(shell(type("ㄱㅏㅂㅇ").stream)).toBe("갑ㅇ"); // ㅂ+ㅇ 복합 종성 없음
  });

  it("Shift 자모(ㄲㅆㅃㅉㄸ·ㅒㅖ)는 한 키로 초성·종성·중성이 된다", () => {
    expect(shell(type("ㄲㅏ").stream)).toBe("까");
    expect(shell(type("ㅇㅣㅆ").stream)).toBe("있");
    expect(shell(type("ㄱㅖ").stream)).toBe("계");
    expect(shell(type("ㅃㅏㅃ").stream)).toBe("빠ㅃ"); // ㅃ은 종성 불가
  });

  it("Backspace는 자모 한 단계씩 되돌리고, 복합도 한 단계다(닭→달→다→ㄷ→빈)", () => {
    const a = new HangulAutomaton();
    let s = type("ㄷㅏㄹㄱ", a).stream;
    expect(shell(s)).toBe("닭");
    s += a.backspace() ?? "";
    expect(shell(s)).toBe("달");
    s += a.backspace() ?? "";
    expect(shell(s)).toBe("다");
    s += a.backspace() ?? "";
    expect(shell(s)).toBe("ㄷ");
    s += a.backspace() ?? "";
    expect(shell(s)).toBe("");
    expect(a.current).toBe("");
    expect(a.backspace()).toBeNull(); // 조합 없음 → 호출자가 일반 Backspace로
  });

  it("Backspace 뒤 다시 조합할 수 있다(안녕 → 안ㄴ → 안녕)", () => {
    const { stream } = type("ㅇㅏㄴㄴㅕㅇ<<ㅕㅇ");
    expect(shell(stream)).toBe("안녕");
  });

  it("확정된 앞 음절은 Backspace로 되돌리지 않는다(IME와 동일)", () => {
    const a = new HangulAutomaton();
    const s = type("ㅇㅏㄴㄴ", a).stream; // 안 확정, ㄴ 조합 중
    expect(shell(s + (a.backspace() ?? ""))).toBe("안");
    expect(a.backspace()).toBeNull();
  });

  it("commit은 상태만 비우고 아무것도 보내지 않는다", () => {
    const a = new HangulAutomaton();
    a.feed("ㅎ");
    a.feed("ㅏ");
    expect(a.current).toBe("하");
    a.commit();
    expect(a.current).toBe("");
    expect(a.lastJamo).toBeNull();
    expect(a.feed("ㄴ")).toBe("ㄴ"); // 하에 종성으로 붙지 않는다
  });

  it("lastJamo는 현재 음절의 마지막 자모다", () => {
    const a = new HangulAutomaton();
    a.feed("ㅇ");
    expect(a.lastJamo).toBe("ㅇ");
    a.feed("ㅏ");
    expect(a.lastJamo).toBe("ㅏ");
  });
});

describe("자모 판정·자판 매핑", () => {
  it("isCompatJamo는 호환 자모 한 글자만 참", () => {
    expect(isCompatJamo("ㅇ")).toBe(true);
    expect(isCompatJamo("ㅣ")).toBe(true);
    expect(isCompatJamo("안")).toBe(false);
    expect(isCompatJamo("a")).toBe(false);
    expect(isCompatJamo("ㅇㅏ")).toBe(false);
    expect(isCompatJamo(undefined)).toBe(false);
  });

  it("latinToJamo는 두벌식 자판을 따른다(Shift 자모 포함)", () => {
    expect(latinToJamo("d")).toBe("ㅇ");
    expect(latinToJamo("k")).toBe("ㅏ");
    expect(latinToJamo("R")).toBe("ㄲ");
    expect(latinToJamo("P")).toBe("ㅖ");
    expect(latinToJamo("A")).toBe("ㅁ"); // Shift 자모가 없는 키는 소문자와 같다
    expect(latinToJamo("1")).toBeNull();
    expect(latinToJamo("dk")).toBeNull();
  });
});

import { describe, expect, it } from "vitest";
import { DEL, HOLD_IME_WAIT_MS, HOLD_WINDOW_MS, tailEdit, WebKitImeCore } from "./imeBridge";

describe("tailEdit — 꼬리 교체 diff", () => {
  it("첫 자소 삽입", () => {
    expect(tailEdit("", "ㅇ")).toEqual({ deletes: 0, insert: "ㅇ" });
  });
  it("조합(마지막 글자 교체)", () => {
    expect(tailEdit("ㅇ", "아")).toEqual({ deletes: 1, insert: "아" });
    expect(tailEdit("아", "안")).toEqual({ deletes: 1, insert: "안" });
  });
  it("확정 + 다음 자소", () => {
    expect(tailEdit("안", "안ㄴ")).toEqual({ deletes: 0, insert: "ㄴ" });
  });
  it("받침 이동(핫→하)", () => {
    expect(tailEdit("안녕핫", "안녕하")).toEqual({ deletes: 1, insert: "하" });
  });
  it("서로게이트 쌍은 한 글자로 센다", () => {
    expect(tailEdit("😀", "")).toEqual({ deletes: 1, insert: "" });
    expect(tailEdit("😀", "😀한")).toEqual({ deletes: 0, insert: "한" });
  });
});

type Ev =
  | ["beforeinput", string, string]
  | ["input", string, string, string?]
  | ["keydown", number, boolean?]
  | ["keypress", number]
  | ["keyup", number]
  | ["compositionstart"]
  | ["settle"];

/** 이벤트열 재생기 — emit/block/reset/schedule 카운트를 모은다. */
function replay(events: Ev[]): {
  emits: string[];
  blocks: number;
  resets: number;
  settles: number;
} {
  const core = new WebKitImeCore();
  const emits: string[] = [];
  let blocks = 0;
  let resets = 0;
  let settles = 0;
  const run = (action: ReturnType<WebKitImeCore["input"]>): void => {
    if (action.emit) emits.push(action.emit);
    if (action.block) blocks++;
    if (action.resetKeyDownSeen) resets++;
    if (action.scheduleSettle) settles++;
  };
  for (const ev of events) {
    switch (ev[0]) {
      case "beforeinput":
        if (core.beforeinput(ev[1], ev[2]).resetKeyDownSeen) resets++;
        break;
      case "input":
        run(core.input(ev[1], ev[2], ev[3]));
        break;
      case "keydown":
        run(core.keydown(ev[1], false, ev[2] ?? false));
        break;
      case "keypress":
        run(core.keypress(ev[1]));
        break;
      case "keyup":
        core.keyup(ev[1]);
        break;
      case "compositionstart":
        run(core.compositionstart());
        break;
      case "settle":
        run(core.settle());
        break;
    }
  }
  // run()에서 이미 카운트한 reset/settle과 beforeinput 분리 집계 정리
  return { emits, blocks, resets: resets, settles };
}

/** WebKit 순서 헬퍼: beforeinput(변경 전 값) → input(변경 후 값, data). */
function imeStep(type: string, before: string, after: string, kd = 229, data?: string): Ev[] {
  const inserted = data ?? Array.from(after).slice(Array.from(before).length).join("");
  return [["beforeinput", type, before] as Ev, ["input", type, after, inserted] as Ev, ["keydown", 229] as Ev, ["keyup", kd] as Ev];
}

describe("WebKitImeCore — 조합 확정 시점 일괄 전송", () => {
  it("느린 안: 첫 자소부터 보류 — 확정 시 DEL 없는 순수 추가", () => {
    const { emits } = replay([
      ...imeStep("insertText", "", "ㅇ", 68),
      ...imeStep("insertReplacementText", "ㅇ", "아", 75),
      ...imeStep("insertReplacementText", "아", "안", 83),
      ["beforeinput", "insertReplacementText", "안"] as Ev,
      ["input", "insertReplacementText", "안"] as Ev, // 값 변화 없음 = 확정 마커
    ]);
    expect(emits).toEqual(["안"]);
  });

  it("빠른 안녕: 음절마다 DEL 없이 순수 추가 — 재그리기 1회/음절", () => {
    const { emits } = replay([
      ...imeStep("insertText", "", "ㅇ", 68),
      ...imeStep("insertReplacementText", "ㅇ", "아", 75),
      ...imeStep("insertReplacementText", "아", "안", 83),
      // 확정 마커(값 불변)
      ["beforeinput", "insertReplacementText", "안"] as Ev,
      ["input", "insertReplacementText", "안"] as Ev,
      // ㄴ 시작 — 활성 후에는 자소도 보류(즉시 전송 없음)
      ...imeStep("insertText", "안", "안ㄴ", 83),
      ...imeStep("insertReplacementText", "안ㄴ", "안녀", 85),
      ...imeStep("insertReplacementText", "안녀", "안녕", 68),
      // 사용자가 멈춤 → 백스톱
      ["settle"] as Ev,
    ]);
    expect(emits).toEqual(["안", "녕"]);
  });

  it("실제 키가 소유한 insertText는 보류하지 않는다(이중 방지)", () => {
    const core = new WebKitImeCore();
    // 활성
    core.beforeinput("insertText", "");
    core.input("insertText", "ㅇ", "ㅇ");
    core.beforeinput("insertReplacementText", "ㅇ");
    core.input("insertReplacementText", "아");
    core.settle();
    // '.' keydown 대기 중 insertText → 실제 키 소유
    core.keydown(190, false);
    core.beforeinput("insertText", "아.");
    const action = core.input("insertText", "아.", ".");
    expect(action.emit).toBeUndefined();
    expect(action.block).toBe(true);
    expect(core.settle()).toEqual({}); // 보류 없음
  });

  it("pendingText: 보류 중 조합(preedit 표시분)만 노출한다", () => {
    const core = new WebKitImeCore();
    core.beforeinput("insertText", "");
    core.input("insertText", "ㅇ", "ㅇ");
    expect(core.pendingText).toBe("ㅇ"); // 첫 자소부터 preedit
    core.beforeinput("insertReplacementText", "ㅇ");
    core.input("insertReplacementText", "아");
    expect(core.pendingText).toBe("아");
    core.beforeinput("insertReplacementText", "아");
    core.input("insertReplacementText", "안");
    expect(core.pendingText).toBe("안");
    core.settle();
    expect(core.pendingText).toBe("");
    // 이어치기: 새 자소도 보류 → preedit에 보인다.
    core.beforeinput("insertText", "안");
    core.input("insertText", "안ㄴ", "ㄴ");
    expect(core.pendingText).toBe("ㄴ");
    core.beforeinput("insertReplacementText", "안ㄴ");
    core.input("insertReplacementText", "안녕");
    expect(core.pendingText).toBe("녕");
    core.settle();
    expect(core.pendingText).toBe("");
  });

  it("마커 없이 빠르게 쳐도 새 음절 시작이 앞 음절을 확정한다(하세요 뭉텅이 방지)", () => {
    const { emits } = replay([
      // 안 (마커 없음)
      ...imeStep("insertText", "", "ㅇ", 68),
      ...imeStep("insertReplacementText", "ㅇ", "아", 75),
      ...imeStep("insertReplacementText", "아", "안", 83),
      // ㄴ — 새 음절 시작 → 안 확정 + ㄴ 보류
      ...imeStep("insertText", "안", "안ㄴ", 83),
      ...imeStep("insertReplacementText", "안ㄴ", "안녀", 85),
      ...imeStep("insertReplacementText", "안녀", "안녕", 68),
      // ㅎ — 새 음절 시작 → 녕 확정 + ㅎ 보류
      ...imeStep("insertText", "안녕", "안녕ㅎ", 71),
      ...imeStep("insertReplacementText", "안녕ㅎ", "안녕하", 75),
      // Enter — 하 확정
      ["keydown", 13] as Ev,
    ]);
    expect(emits).toEqual(["안", "녕", "하"]);
  });

  it("한글 IME 영문 모드(ASCII)는 보류 없이 즉시 전송", () => {
    const core = new WebKitImeCore();
    core.beforeinput("insertText", "");
    const a = core.input("insertText", "a", "a");
    expect(a.emit).toBe("a");
    expect(core.pendingText).toBe("");
    core.beforeinput("insertText", "a");
    const b = core.input("insertText", "ab", "b");
    expect(b.emit).toBe("b");
  });

  it("compositionstart(영→한 전환 직후 발화)에도 보류분만 확정하고 계속 소유한다", () => {
    const core = new WebKitImeCore();
    core.beforeinput("insertText", "");
    core.input("insertText", "ㅇ", "ㅇ");
    core.beforeinput("insertReplacementText", "ㅇ");
    core.input("insertReplacementText", "안");
    expect(core.compositionstart().emit).toBe("안");
    // 물러나지 않는다 — 이후 조합도 여전히 보류·확정된다(전환 후 자모 유실 방지).
    core.beforeinput("insertText", "안");
    expect(core.input("insertText", "안ㄴ", "ㄴ")).toMatchObject({
      block: true,
      scheduleSettle: true,
    });
    core.beforeinput("insertReplacementText", "안ㄴ");
    core.input("insertReplacementText", "안녕");
    expect(core.settle()).toEqual({ emit: "녕" });
  });

  it("백스페이스(deleteContentBackward) 후 shellValue가 함께 줄어 취소 DEL이 나가지 않는다", () => {
    const core = new WebKitImeCore();
    // IME 영문 모드로 abc — 즉시 전송되며 shellValue가 따라감
    for (const [data, value] of [["a", "a"], ["b", "ab"], ["c", "abc"]] as Array<[string, string]>) {
      core.beforeinput("insertText", value.slice(0, -1));
      core.input("insertText", value, data);
    }
    // 백스페이스 3회 — deleteContentBackward로 textarea 축소
    // (beforeinput의 값은 아직 이전 상태)
    for (const [before, after] of [["abc", "ab"], ["ab", "a"], ["a", ""]] as Array<[string, string]>) {
      core.beforeinput("deleteContentBackward", before);
      core.input("deleteContentBackward", after);
    }
    // 안녕 입력 — 첫 flush가 순수 추가여야 한다(DEL 없음)
    core.beforeinput("insertText", "");
    core.input("insertText", "ㅇ", "ㅇ");
    core.beforeinput("insertReplacementText", "ㅇ");
    core.input("insertReplacementText", "아");
    core.beforeinput("insertReplacementText", "아");
    core.input("insertReplacementText", "안");
    core.beforeinput("insertText", "안");
    const flush = core.input("insertText", "안ㄴ", "ㄴ");
    expect(flush.emit).toBe("안");
    core.beforeinput("insertReplacementText", "안ㄴ");
    core.input("insertReplacementText", "안녀");
    core.beforeinput("insertReplacementText", "안녀");
    core.input("insertReplacementText", "안녕");
    expect(core.keydown(32, false).emit).toBe("녕"); // 스페이스 확정 — DEL 없음
  });

  it("확정 마커가 없어도 백스톱 타이머로 확정된다", () => {
    const core = new WebKitImeCore(5);
    core.beforeinput("insertText", "");
    core.input("insertText", "ㅇ", "ㅇ");
    core.beforeinput("insertReplacementText", "ㅇ");
    const held = core.input("insertReplacementText", "아");
    expect(held.scheduleSettle).toBe(true); // 보류 + 타이머 가동
    expect(core.settle()).toEqual({ emit: "아" });
  });

  it("실제 키 keydown은 xterm 처리 전에 조합을 확정한다(Enter 사례)", () => {
    const core = new WebKitImeCore();
    core.beforeinput("insertText", "");
    core.input("insertText", "ㅇ", "ㅇ");
    core.beforeinput("insertReplacementText", "ㅇ");
    core.input("insertReplacementText", "안");
    const action = core.keydown(13, false); // Enter
    expect(action.emit).toBe("안");
  });

  it("229 keydown은 조합 중이 아니면 항상 차단한다(xterm diff 타이머의 중복 전송 봉쇄)", () => {
    const core = new WebKitImeCore();
    // input 선행 없이 온 229도 차단한다 — insert 이벤트는 코어가 전부 소유하므로
    // xterm의 _handleAnyTextareaChanges는 다음 입력을 한 번 더 보낼 위험만 있다.
    expect(core.keydown(229, false).block).toBe(true);
    core.beforeinput("insertText", "");
    core.input("insertText", "ㅇ", "ㅇ");
    expect(core.keydown(229, false).block).toBe(true);
    core.keyup(68);
    core.beforeinput("insertReplacementText", "ㅇ");
    core.input("insertReplacementText", "아");
    expect(core.keydown(229, false).block).toBe(true);
    // 표준 조합 중(isComposing)의 229는 xterm CompositionHelper 몫 — 통과.
    expect(core.keydown(229, true).block).toBeUndefined();
  });

  it("IME가 먼저 넣은 insertText는 게이트를 리셋, 실제 키는 리셋 안 함", () => {
    const a = new WebKitImeCore();
    a.keydown(16, false); // Shift (수정키 — 소유 주장 안 함)
    expect(a.beforeinput("insertText", "").resetKeyDownSeen).toBe(true);
    const b = new WebKitImeCore();
    b.keydown(190, false); // '.' keydown이 대기
    expect(b.beforeinput("insertText", "x.").resetKeyDownSeen).toBeUndefined();
    b.keyup(190);
    expect(b.beforeinput("insertText", "x.").resetKeyDownSeen).toBe(true);
  });

  it("영→한 전환 경계: compositionstart 후에도 조합은 유실·유출 없이 확정된다", () => {
    const { emits } = replay([
      // 영문 모드 타이핑 후 전환 (값은 누적된다)
      ...imeStep("insertText", "", "a", 65),
      ["compositionstart"] as Ev, // WebKit의 전환 직후 단독 발화
      // 안녕 — 자모가 새지 않고 음절만
      ...imeStep("insertText", "a", "aㅇ", 68),
      ...imeStep("insertReplacementText", "aㅇ", "a아", 75),
      ...imeStep("insertReplacementText", "a아", "a안", 83),
      ...imeStep("insertText", "a안", "a안ㄴ", 83),
      ...imeStep("insertReplacementText", "a안ㄴ", "a안녀", 85),
      ...imeStep("insertReplacementText", "a안녀", "a안녕", 68),
      ["settle"] as Ev,
    ]);
    expect(emits).toEqual(["a", "안", "녕"]);
  });

  it("Enter로 textarea가 비워진 뒤 재동기화 후에도 정합 유지", () => {
    const core = new WebKitImeCore();
    core.beforeinput("insertText", "");
    core.input("insertText", "ㅇ", "ㅇ");
    core.beforeinput("insertReplacementText", "ㅇ");
    core.input("insertReplacementText", "안");
    core.keydown(13, false); // 확정 flush
    core.keyup(13);
    core.resync(""); // xterm이 CR로 비움
    core.beforeinput("insertText", "");
    core.input("insertText", "ㄴ", "ㄴ");
    core.beforeinput("insertReplacementText", "ㄴ");
    expect(core.input("insertReplacementText", "는")).toEqual({
      block: true,
      scheduleSettle: true,
    });
    expect(core.settle()).toEqual({ emit: "는" });
  });
});

describe("WebKitImeCore — 스페이스/NBSP 이중 전송 방지", () => {
  it("keydown 선행 Space도 브리지가 소유해 xterm과 경합하지 않는다", () => {
    const { emits } = replay([
      ...imeStep("insertText", "", "ㅇ", 68),
      ...imeStep("insertReplacementText", "ㅇ", "아", 75),
      ...imeStep("insertReplacementText", "아", "안", 83),
      // 스페이스: 브리지가 keydown/keypress를 차단하고 기본 textarea 삽입은 허용한다.
      ["keydown", 32] as Ev,
      ["keypress", 32] as Ev,
      ["keyup", 32] as Ev,
      ["beforeinput", "insertText", "안"] as Ev,
      ["input", "insertText", "안\xa0", "\xa0"] as Ev,
      // 다음 단어 — 상태가 어긋나지 않았다면 순수 추가만 나온다
      ...imeStep("insertText", "안 ", "안 ㄴ", 83, "ㄴ"),
      ...imeStep("insertReplacementText", "안 ㄴ", "안 녀", 85),
      ["settle"] as Ev,
    ]);
    expect(emits).toEqual(["안", " ", "녀"]);
  });

  it("수정키가 있는 Space는 가로채지 않아 터미널 단축키 의미를 보존한다", () => {
    const core = new WebKitImeCore();
    expect(core.keydown(32, false, true).block).toBeUndefined();
  });

  it("수정키 Space의 keyCode가 다음 일반 공백 input을 삼키지 않는다", () => {
    const core = new WebKitImeCore();
    core.keydown(32, false, true);
    core.keyup(32);
    core.beforeinput("insertText", "");
    expect(core.input("insertText", "\xa0", "\xa0").emit).toBe(" ");
  });

  it("첫 줄에서 '한글과 한글입력'을 매우 빠르게 이어 쳐도 공백이 보존된다", () => {
    const events: Ev[] = [
      ...imeStep("insertText", "", "ㅎ", 71),
      ...imeStep("insertReplacementText", "ㅎ", "하", 75),
      ...imeStep("insertReplacementText", "하", "한", 83),
      ...imeStep("insertText", "한", "한ㄱ", 82, "ㄱ"),
      ...imeStep("insertReplacementText", "한ㄱ", "한그", 77),
      ...imeStep("insertReplacementText", "한그", "한글", 70),
      ...imeStep("insertText", "한글", "한글ㄱ", 82, "ㄱ"),
      ...imeStep("insertReplacementText", "한글ㄱ", "한글과", 75),
      // 확정 마커 없이 곧바로 Space, 이어서 다음 한글 조합이 시작된다.
      ["keydown", 32],
      ["keypress", 32],
      ["beforeinput", "insertText", "한글과"],
      ["input", "insertText", "한글과\xa0", "\xa0"],
      ["keyup", 32],
      ...imeStep("insertText", "한글과 ", "한글과 ㅎ", 71, "ㅎ"),
      ...imeStep("insertReplacementText", "한글과 ㅎ", "한글과 한", 83),
      ...imeStep("insertText", "한글과 한", "한글과 한ㄱ", 82, "ㄱ"),
      ...imeStep("insertReplacementText", "한글과 한ㄱ", "한글과 한글", 70),
      ...imeStep("insertText", "한글과 한글", "한글과 한글ㅇ", 68, "ㅇ"),
      ...imeStep("insertReplacementText", "한글과 한글ㅇ", "한글과 한글이", 76),
      ...imeStep("insertReplacementText", "한글과 한글이", "한글과 한글입", 81),
      ...imeStep("insertText", "한글과 한글입", "한글과 한글입ㄹ", 70, "ㄹ"),
      ...imeStep("insertReplacementText", "한글과 한글입ㄹ", "한글과 한글입려", 85),
      ...imeStep("insertReplacementText", "한글과 한글입려", "한글과 한글입력", 82),
      ["settle"],
    ];

    expect(replay(events).emits.join("")).toBe("한글과 한글입력");
  });

  it("영문→한글→영문을 빠르게 전환해도 양쪽 공백이 모두 보존된다", () => {
    const { emits } = replay([
      // input 선행 영문 a와 뒤늦은 keydown/keypress.
      ["beforeinput", "insertText", ""],
      ["input", "insertText", "a", "a"],
      ["keydown", 65],
      ["keypress", 65],
      ["keyup", 65],
      // keydown 선행 공백.
      ["keydown", 32],
      ["keypress", 32],
      ["beforeinput", "insertText", "a"],
      ["input", "insertText", "a\xa0", "\xa0"],
      ["keyup", 32],
      ...imeStep("insertText", "a ", "a ㅎ", 71, "ㅎ"),
      ...imeStep("insertReplacementText", "a ㅎ", "a 한", 83),
      // input 선행 공백(한글 확정과 공백을 한 동작으로 전송).
      ["beforeinput", "insertText", "a 한"],
      ["input", "insertText", "a 한\xa0", "\xa0"],
      ["keydown", 229],
      ["keypress", 32],
      ["keyup", 32],
      ["beforeinput", "insertText", "a 한 "],
      ["input", "insertText", "a 한 b", "b"],
      ["keydown", 66],
      ["keypress", 66],
      ["keyup", 66],
    ]);

    expect(emits.join("")).toBe("a 한 b");
  });

  it("IME 커밋으로 input이 먼저 온 스페이스는 우리가 보내고 뒤따르는 keydown을 차단한다", () => {
    const core = new WebKitImeCore();
    core.beforeinput("insertText", "");
    core.input("insertText", "ㅇ", "ㅇ");
    core.beforeinput("insertReplacementText", "ㅇ");
    core.input("insertReplacementText", "안");
    // input 선행(WebKit IME 순서): 보류 안 확정 + 스페이스 즉시 전송
    core.beforeinput("insertText", "안");
    const space = core.input("insertText", "안\xa0", "\xa0");
    expect(space.emit).toBe("안 "); // flush(안) + 정규화된 스페이스 1개
    // 뒤늦은 스페이스 keydown — xterm이 ' '를 또 보내지 못하게 차단
    const kd = core.keydown(32, false);
    expect(kd.block).toBe(true);
    expect(kd.emit).toBeUndefined();
    // 이후 조합은 정합 유지
    core.keyup(32);
    core.beforeinput("insertText", "안 ");
    core.input("insertText", "안 ㄴ", "ㄴ");
    core.beforeinput("insertReplacementText", "안 ㄴ");
    core.input("insertReplacementText", "안 녀");
    expect(core.settle()).toEqual({ emit: "녀" });
  });

  it("우리가 보낸 키와 다른 keydown은 차단하지 않는다(오인 방지)", () => {
    const core = new WebKitImeCore();
    core.beforeinput("insertText", "");
    const a = core.input("insertText", "a", "a");
    expect(a.emit).toBe("a");
    expect(core.keydown(66, false).block).toBeUndefined(); // 'B' 키 — 통과
  });

  it("keydown 뒤 다른 input이 끼면 전송 우선권은 소진된다 — 같은 글자라도 스킵하지 않는다", () => {
    const core = new WebKitImeCore();
    core.keydown(65, false); // 'A' — xterm이 'a' 전송
    core.beforeinput("insertReplacementText", "");
    core.input("insertReplacementText", "아"); // 조합 input이 우선권 소진
    core.keyup(65);
    core.beforeinput("insertText", "아");
    const act = core.input("insertText", "아a", "a");
    expect(act.emit).toBe("아a"); // flush(아) + a — 스킵 없이 전송
  });

  it("실제 키 keydown 직후의 input은 우리가 다시 보내지 않는다(문자 이중 방지)", () => {
    const core = new WebKitImeCore();
    core.keydown(65, false); // 'A' keydown → xterm이 'a' 전송
    core.keyup(65);
    core.beforeinput("insertText", "");
    const act = core.input("insertText", "a", "a");
    expect(act.emit).toBeUndefined(); // 이미 xterm이 보냈다
    expect(act.block).toBe(true);
  });

  it("자체 전송 키의 keypress는 차단한다 — xterm은 인쇄문자를 keypress에서 보낸다", () => {
    const core = new WebKitImeCore();
    core.beforeinput("insertText", "");
    core.input("insertText", "a", "a"); // 자체 전송(IME input 선행)
    expect(core.keydown(65, false).block).toBe(true); // keydown 차단
    expect(core.keypress(65).block).toBe(true); // keypress도 차단(이중 봉쇄)
    expect(core.keypress(65).block).toBeUndefined(); // 소진되면 다시 안 막는다
  });

  it("textarea 기준점을 비워도 늦은 keydown/keypress의 이중 전송 차단은 유지한다", () => {
    const core = new WebKitImeCore();
    core.beforeinput("insertText", "");
    expect(core.input("insertText", "a", "a").emit).toBe("a");

    core.textAreaCleared();

    expect(core.keydown(65, false).block).toBe(true);
    expect(core.keypress(65).block).toBe(true);
    core.keyup(65);
    core.beforeinput("insertText", "");
    core.input("insertText", "ㅇ", "ㅇ");
    core.beforeinput("insertReplacementText", "ㅇ");
    core.input("insertReplacementText", "안", "안");
    expect(core.settle()).toEqual({ emit: "안" });
  });

  it("keydown이 keypress 짝과 다르면 억제는 소진된다 — 이후 같은 키 손실 없음", () => {
    const core = new WebKitImeCore();
    core.beforeinput("insertText", "");
    core.input("insertText", "a", "a"); // 자체 전송 'a'
    core.keydown(38, false); // 방향키 — 자체 전송 키와 불일치, keypress 없음
    core.keydown(65, false); // 나중의 진짜 'a' — keydown 억제는 이미 소진
    expect(core.keypress(65).block).toBeUndefined(); // keypress도 통과
  });

  it("229 keydown이 자체 전송 키를 소진한 경우에도 keypress 억제가 짝을 맞춘다", () => {
    const core = new WebKitImeCore();
    core.beforeinput("insertText", "");
    const sp = core.input("insertText", "안\xa0", "\xa0"); // 스페이스 자체 전송
    expect(sp.emit).toBe("안 ");
    expect(core.keydown(229, false).block).toBe(true); // 229 차단 경로
    expect(core.keypress(32).block).toBe(true); // 뒤따르는 스페이스 keypress 차단
  });

  it("한/영 전환 홀드: IME가 활성화되면 붙잡은 라틴을 버린다(dk 유출 방지)", () => {
    const core = new WebKitImeCore();
    core.armKoreanActivationHold();
    const d = core.keydown(68, false, false, "d");
    expect(d.block).toBe(true);
    expect(d.preventDefault).toBe(true);
    expect(d.emit).toBeUndefined();
    expect(core.keydown(75, false, false, "k").emit).toBeUndefined();
    // IME 활성 — 첫 자소가 조합으로 들어온다
    core.beforeinput("insertText", "");
    expect(core.input("insertText", "ㅇ", "ㅇ").emit).toBeUndefined();
    expect(core.settle()).toEqual({ emit: "ㅇ" }); // dk 없이 순수 조합
  });

  it("한/영 전환 홀드: IME가 켜지지 않으면 만료 시 라틴을 그대로 내보낸다", () => {
    const core = new WebKitImeCore();
    core.armKoreanActivationHold();
    core.keydown(68, false, false, "d");
    core.keydown(75, false, false, "k");
    expect(core.releaseHeldLatin()).toEqual({ emit: "dk" });
    expect(core.releaseHeldLatin()).toEqual({}); // 소진
    // 내보낸 라틴은 상태에 없다 — 이후 조합 diff가 dk를 지우지 않는다
    core.beforeinput("insertText", "");
    core.input("insertText", "ㅇ", "ㅇ");
    expect(core.settle()).toEqual({ emit: "ㅇ" });
  });

  it("한/영 전환 홀드 중 비인쇄 키는 붙잡은 라틴을 먼저 내보낸 뒤 처리된다", () => {
    const core = new WebKitImeCore();
    core.armKoreanActivationHold();
    core.keydown(68, false, false, "d");
    const enter = core.keydown(13, false, false, "Enter");
    expect(enter.emit).toBe("d"); // d 먼저, 이후 Enter는 xterm이 처리
  });

  it("한/영 전환 홀드는 스페이스·Tab·수정키를 붙잡지 않는다", () => {
    const core = new WebKitImeCore();
    core.armKoreanActivationHold();
    const space = core.keydown(32, false, false, " ");
    expect(space.preventDefault).toBeUndefined(); // 스페이스 캡처 경로 유지
    expect(core.keydown(9, false, false, "\t").preventDefault).toBeUndefined();
    expect(core.keydown(16, false, false, "Shift").preventDefault).toBeUndefined();
    expect(core.keydown(68, false, false, "d").preventDefault).toBe(true);
  });

  it("라틴 키 keyup이 롤오버로 늦어 pendingPrintableKey가 남아도 한글 조합은 앞 라틴을 먹지 않는다", () => {
    // hello안녕 → hell안녕 의 근본 원인: 비조합 글자('1', xterm이 전송)의
    // keyup이 다음 한글 조합보다 늦게 도착해 pendingPrintableKey가 남으면,
    // HEAD는 첫 한글 자소를 실제 키로 오인해 shellValue를 오염시키고 이어지는
    // 확정 diff의 DEL이 앞 '1'을 먹었다. 삽입 데이터가 한글이면 항상 조합
    // 소유로 보아 그 오인을 없앤다 — 브리지 emit에 파괴적 DEL이 없어야 한다.
    const core = new WebKitImeCore();
    const emits: string[] = [];
    const push = (a: ReturnType<WebKitImeCore["input"]>): void => {
      if (a.emit) emits.push(a.emit);
    };
    core.keydown(49, false); // '1' keydown → pendingPrintableKey=49
    core.beforeinput("insertText", "");
    push(core.input("insertText", "1", "1")); // xterm이 '1' 전송(브리지 emit 없음)
    // '1' keyup 아직(롤오버). 한글 조합 시작:
    core.beforeinput("insertText", "1");
    push(core.input("insertText", "1ㅇ", "ㅇ"));
    core.keydown(229, false);
    core.keyup(49); // 늦은 '1' keyup
    core.beforeinput("insertReplacementText", "1ㅇ");
    push(core.input("insertReplacementText", "1아"));
    core.keydown(229, false);
    core.keyup(75);
    core.beforeinput("insertReplacementText", "1아");
    push(core.input("insertReplacementText", "1안"));
    core.keydown(229, false);
    core.keyup(83);
    push(core.settle());
    const joined = emits.join("");
    expect(joined.includes(DEL)).toBe(false); // 파괴적 DEL 없음(앞 '1' 보존)
    expect(joined).toBe("안");
  });
});

describe("확정 뒤 Backspace 분해 — 되돌림 DEL 즉시 전송(하ㅎ·안아 겹침 회귀)", () => {
  // 실기(WKWebView) 순서: 백스톱(2초 유휴)이 음절을 셸로 보낸 뒤에도 WebKit IME는
  // 그 음절을 계속 조합 중이라, Backspace가 삭제가 아니라 자소 분해로 온다
  // (insertReplacementText가 keydown 229보다 먼저). 되돌린 글자의 DEL을 미루면
  // 화면에 확정된 요와 preedit ㅇ이 겹쳐 보이다가 다음 백스톱에서야 합쳐진다.
  type Core = WebKitImeCore;
  const step = (core: Core, emits: string[], type: string, before: string, after: string, data: string): void => {
    core.beforeinput(type, before);
    const action = core.input(type, after, data);
    if (action.emit) emits.push(action.emit);
  };

  it("유휴 백스톱이 보낸 요를 IME가 ㅇ으로 분해하면 DEL을 바로 보내고 ㅇ은 preedit에 남는다", () => {
    const core = new WebKitImeCore();
    const emits: string[] = [];
    step(core, emits, "insertText", "", "ㅇ", "ㅇ");
    step(core, emits, "insertReplacementText", "ㅇ", "요", "요");
    expect(core.settle()).toEqual({ emit: "요" }); // 백스톱 확정
    expect(core.pendingText).toBe("");

    step(core, emits, "insertReplacementText", "요", "ㅇ", "ㅇ"); // Backspace 분해
    expect(emits).toEqual([DEL]); // 화면의 요를 즉시 걷어낸다
    expect(core.pendingText).toBe("ㅇ"); // 분해된 자모는 preedit로만

    // 한 번 더 Backspace로 조합이 비면 WebKit은 값 불변 마커를 낸 뒤 실제
    // Backspace(keyCode 8)를 xterm에 넘긴다 — 마커는 보류분(ㅇ)을 내보내고
    // 뒤따르는 xterm의 DEL이 그것을 지워 화면은 깨끗해진다.
    step(core, emits, "insertReplacementText", "ㅇ", "ㅇ", "ㅇ");
    expect(emits).toEqual([DEL, "ㅇ"]);
    expect(core.pendingText).toBe("");
  });

  it("안 확정 뒤 분해→재조합→다음 음절은 DEL 한 번과 안만 보낸다(안아 겹침 없음)", () => {
    const core = new WebKitImeCore();
    const emits: string[] = [];
    step(core, emits, "insertText", "", "ㅇ", "ㅇ");
    step(core, emits, "insertReplacementText", "ㅇ", "아", "아");
    step(core, emits, "insertReplacementText", "아", "안", "안");
    const settled = core.settle();
    if (settled.emit) emits.push(settled.emit);
    expect(emits).toEqual(["안"]);

    step(core, emits, "insertReplacementText", "안", "아", "아"); // Backspace 분해
    expect(emits).toEqual(["안", DEL]);
    expect(core.pendingText).toBe("아");

    step(core, emits, "insertReplacementText", "아", "안", "안"); // ㄴ 다시
    expect(emits).toEqual(["안", DEL]); // 아직 preedit — 추가 DEL 없음
    step(core, emits, "insertText", "안", "안ㄴ", "ㄴ"); // 다음 음절 시작 → 안 확정
    expect(emits).toEqual(["안", DEL, "안"]);
    expect(core.pendingText).toBe("ㄴ");
  });

  it("보류 중(미전송) 음절의 분해는 DEL을 보내지 않는다", () => {
    const core = new WebKitImeCore();
    const emits: string[] = [];
    step(core, emits, "insertText", "", "ㅇ", "ㅇ");
    step(core, emits, "insertReplacementText", "ㅇ", "아", "아");
    step(core, emits, "insertReplacementText", "아", "안", "안");
    step(core, emits, "insertReplacementText", "안", "아", "아"); // 아직 셸에 없는 안 → DEL 불필요
    expect(emits).toEqual([]);
    expect(core.pendingText).toBe("아");
  });
});

describe("외부 textarea 비우기(xterm blur) 자가 복구 — DEL 폭주 회귀", () => {
  const step = (core: WebKitImeCore, emits: string[], type: string, before: string, after: string, data: string): void => {
    core.beforeinput(type, before);
    const action = core.input(type, after, data);
    if (action.emit) emits.push(action.emit);
  };

  it("beforeinput 값이 기준(textValue)과 다르면 기준점을 옮겨 다음 스페이스에 DEL을 쏟지 않는다", () => {
    // 실기: 안녕하세ㅇ(5자)가 textarea/shellValue에 남은 채 사용자가 클릭·패널
    // 전환 → xterm _handleTextAreaBlur가 textarea를 비움(입력 이벤트 없음) →
    // 영문 c c d(xterm keydown 전송) → 스페이스에서 emit="DEL×5 " → "cc만 남음".
    const core = new WebKitImeCore();
    const emits: string[] = [];
    step(core, emits, "insertText", "", "ㅇ", "ㅇ");
    step(core, emits, "insertReplacementText", "ㅇ", "안", "안");
    const settled = core.settle();
    if (settled.emit) emits.push(settled.emit);
    expect(emits).toEqual(["안"]);

    // xterm이 textarea를 비웠다 — 코어는 통보받지 못했다. 다음 입력(스페이스).
    step(core, emits, "insertText", "", "\xa0", "\xa0");
    expect(emits).toEqual(["안", " "]); // DEL 없음
    expect(core.pendingText).toBe("");
  });

  it("보류 조합이 있는 채로 비워지면 보류분은 버리고(diff 불가) 이후 입력만 보낸다", () => {
    const core = new WebKitImeCore();
    const emits: string[] = [];
    step(core, emits, "insertText", "", "ㅎ", "ㅎ");
    step(core, emits, "insertReplacementText", "ㅎ", "하", "하");
    expect(core.pendingText).toBe("하");
    step(core, emits, "insertText", "", "a", "a"); // 비워진 뒤 영문 a(IME 영문 모드 insertText)
    expect(emits).toEqual(["a"]); // 하를 DEL로 되감지도, 다시 보내지도 않는다
    expect(core.pendingText).toBe("");
  });
});

describe("입력 소스 홀드 창 — keyup 판정·자모 복원·투명 키 (07-korean-ime §3-1)", () => {
  const run = (_core: WebKitImeCore, action: ReturnType<WebKitImeCore["keydown"]>, out: string[]): void => {
    if (action.emit) out.push(action.emit);
  };

  it("`Unidentified`(keyCode 0)·수정키 keydown은 홀드 창을 해제하지 않는다(종전 회귀: kd 0이 즉시 해제)", () => {
    const core = new WebKitImeCore();
    core.armInputSourceHold();
    expect(core.keydown(0, false, false, "Unidentified")).toEqual({});
    expect(core.keydown(16, false, false, "Shift")).toEqual({});
    expect(core.holding).toBe(true);
    const d = core.keydown(68, false, false, "d");
    expect(d.block).toBe(true);
    expect(d.preventDefault).toBe(true);
  });

  it("보류 키의 keyup이 라틴이면 영문 확정 — 즉시 방출하고 창을 닫는다(지연 ≤ 키 누름 시간)", () => {
    const core = new WebKitImeCore();
    core.armInputSourceHold();
    core.keydown(72, false, false, "h");
    core.keydown(69, false, false, "e");
    expect(core.keyup(72, "h")).toEqual({ emit: "he" });
    expect(core.holding).toBe(false);
    // 창이 닫혔으니 다음 라틴은 xterm 몫(통과).
    expect(core.keydown(76, false, false, "l").block).toBeUndefined();
    expect(core.releaseHeldLatin()).toEqual({});
  });

  it("보류 키의 keyup이 자모면 레이아웃 전환 — IME 대기 타이머를 요청하고, 만료 시 자모로 복원한다", () => {
    // 실기(6893행 run): CapsLock → kd 68 "ㅇ"(우회) … 형태 A와, kd 83 "s" → ku "ㄴ" → IME 삽입 형태 B.
    const core = new WebKitImeCore();
    core.armInputSourceHold();
    core.keydown(68, false, false, "d");
    expect(core.keyup(68, "ㅇ")).toEqual({ holdTimerMs: HOLD_IME_WAIT_MS });
    core.keydown(75, false, false, "k");
    core.keyup(75, "ㅏ");
    // IME가 끝내 안 붙음 → 타이머 만료: dk가 아니라 아로 복원된다(꼬리 편집 열).
    expect(core.releaseHeldLatin()).toEqual({ emit: `ㅇ${DEL}아` });
    expect(core.automatonText).toBe("아");
    expect(core.holding).toBe(false);
  });

  it("IME 삽입이 오면 같은 키의 보류는 버리고 그 앞의 보류(IME가 삼킨 키)는 자모 복원한다", () => {
    // 형태 B: ㅇ·ㅏ는 우회/보류, ㄴ 키는 IME가 늦게 insertText "ㄴ"으로 처리.
    const core = new WebKitImeCore();
    const out: string[] = [];
    core.armInputSourceHold();
    run(core, core.keydown(68, false, false, "d"), out);
    core.keyup(68, "ㅇ");
    run(core, core.keydown(75, false, false, "k"), out);
    core.keyup(75, "ㅏ");
    run(core, core.keydown(83, false, false, "s"), out);
    core.keyup(83, "ㄴ");
    core.beforeinput("insertText", "");
    run(core, core.input("insertText", "ㄴ", "ㄴ"), out);
    // d·k → 아(오토마타, 즉시 전송), s는 IME의 ㄴ이므로 버림. ㄴ은 preedit 보류.
    expect(out.join("")).toBe(`ㅇ${DEL}아`);
    expect(core.pendingText).toBe("ㄴ");
    expect(core.holding).toBe(false);
    core.beforeinput("insertReplacementText", "ㄴ");
    core.input("insertReplacementText", "녀");
    expect(core.settle()).toEqual({ emit: "녀" });
  });

  it("타이머로 자모 복원한 직후 늦은 IME가 같은 자모를 넣으면 오토마타의 그 자모를 되돌린다(중복 방지)", () => {
    const core = new WebKitImeCore();
    const out: string[] = [];
    core.armInputSourceHold();
    core.keydown(68, false, false, "d");
    core.keyup(68, "ㅇ");
    run(core, core.releaseHeldLatin(), out); // 타이머 만료 → ㅇ 복원(전송됨)
    expect(out.join("")).toBe("ㅇ");
    // 그런데 IME가 뒤늦게 같은 ㅇ을 삽입한다 — 화면의 ㅇ을 걷어내고 IME 쪽을 따른다.
    core.beforeinput("insertText", "");
    run(core, core.input("insertText", "ㅇ", "ㅇ"), out);
    expect(out.join("")).toBe(`ㅇ${DEL}`);
    expect(core.pendingText).toBe("ㅇ");
    expect(core.automatonText).toBe("");
  });

  it("IME 우회 자모 keydown은 오토마타가 조립해 즉시 보내고 xterm에는 넘기지 않는다(안녕 → 안녕)", () => {
    const core = new WebKitImeCore();
    const out: string[] = [];
    for (const [code, jamo] of [[68, "ㅇ"], [75, "ㅏ"], [83, "ㄴ"], [83, "ㄴ"], [85, "ㅕ"], [68, "ㅇ"]] as Array<[number, string]>) {
      const a = core.keydown(code, false, false, jamo);
      expect(a.block).toBe(true);
      expect(a.preventDefault).toBe(true);
      run(core, a, out);
      core.keyup(code, jamo);
    }
    const shell: string[] = [];
    for (const ch of Array.from(out.join(""))) {
      if (ch === DEL) shell.pop();
      else shell.push(ch);
    }
    expect(shell.join("")).toBe("안녕");
    // Backspace는 오토마타가 자모 단위로 되돌린다(녕→녀), 조합이 비면 xterm에 넘긴다.
    expect(core.keydown(8, false, false, "Backspace")).toEqual({ emit: `${DEL}녀`, block: true, preventDefault: true });
    core.keydown(8, false, false, "Backspace");
    core.keydown(8, false, false, "Backspace");
    expect(core.automatonText).toBe("");
    expect(core.keydown(8, false, false, "Backspace").block).toBeUndefined(); // 안은 확정 — xterm DEL
  });

  it("우회 자모 뒤 IME 삽입이 시작되면 오토마타 음절을 확정하고 IME 조합을 이어 받는다", () => {
    const core = new WebKitImeCore();
    const out: string[] = [];
    core.armInputSourceHold();
    run(core, core.keydown(68, false, false, "ㅇ"), out);
    run(core, core.keydown(75, false, false, "ㅏ"), out);
    expect(core.automatonText).toBe("아");
    core.beforeinput("insertText", "");
    run(core, core.input("insertText", "ㄴ", "ㄴ"), out);
    expect(core.automatonText).toBe(""); // 확정 — 이미 셸에 있는 아는 그대로
    expect(core.pendingText).toBe("ㄴ");
    core.beforeinput("insertReplacementText", "ㄴ");
    core.input("insertReplacementText", "녕");
    run(core, core.settle(), out);
    const shell: string[] = [];
    for (const ch of Array.from(out.join(""))) {
      if (ch === DEL) shell.pop();
      else shell.push(ch);
    }
    expect(shell.join("")).toBe("아녕");
  });

  it("홀드 중 229 keydown은 남은 보류 키를 해소한 뒤 차단된다(IME가 삼킨 키 복원)", () => {
    const core = new WebKitImeCore();
    core.armInputSourceHold();
    core.keydown(82, false, false, "r");
    core.keyup(82, "ㄲ");
    const a = core.keydown(229, false, false, "r");
    expect(a.block).toBe(true);
    expect(a.emit).toBe("ㄲ"); // keyup이 알려준 자모(Shift 자모)로 복원
    expect(core.holding).toBe(false);
  });

  it("홀드 중 Backspace는 보류 키를 되돌리고 셸에는 아무것도 보내지 않는다", () => {
    const core = new WebKitImeCore();
    core.armInputSourceHold();
    core.keydown(72, false, false, "h");
    core.keydown(69, false, false, "e");
    expect(core.keydown(8, false, false, "Backspace")).toEqual({ block: true, preventDefault: true });
    expect(core.keyup(72, "h")).toEqual({ emit: "h" }); // e는 되돌려졌다
  });

  it("홀드 중 Space·Tab은 보류하지 않고 비인쇄 키는 보류를 먼저 해소한다", () => {
    const core = new WebKitImeCore();
    core.armInputSourceHold();
    expect(core.keydown(32, false, false, " ").preventDefault).toBeUndefined();
    expect(core.keydown(9, false, false, "Tab").preventDefault).toBeUndefined();
    core.keydown(68, false, false, "d");
    const enter = core.keydown(13, false, false, "Enter");
    expect(enter.emit).toBe("d");
    expect(core.holding).toBe(false);
  });

  it("resync는 홀드 창을 닫지 않는다(시작 직후 첫 Enter/blur가 창을 죽이면 시작 경주를 못 덮는다)", () => {
    const core = new WebKitImeCore();
    core.armInputSourceHold();
    core.resync("");
    expect(core.holding).toBe(true);
    core.keydown(68, false, false, "d");
    expect(core.keyup(68, "ㅇ")).toEqual({ holdTimerMs: HOLD_IME_WAIT_MS });
  });

  it("armInputSourceHold는 보류 조합을 먼저 확정하고 창 수명을 돌려준다", () => {
    const core = new WebKitImeCore();
    core.beforeinput("insertText", "");
    core.input("insertText", "ㅇ", "ㅇ");
    core.beforeinput("insertReplacementText", "ㅇ");
    core.input("insertReplacementText", "안");
    expect(core.armInputSourceHold()).toEqual({ emit: "안", holdTimerMs: HOLD_WINDOW_MS });
  });

  it("inputSourceToggled(Shift+Space 강제 전환)는 비인쇄 키처럼 보류 키를 먼저 해소하고 창을 다시 연다", () => {
    // 홀드 창에서 보류된 라틴은 전환 키보다 먼저 친 글자다 — 순서대로 먼저 나간다.
    const core = new WebKitImeCore();
    core.armInputSourceHold();
    core.keydown(76, false, false, "l");
    core.keydown(83, false, false, "s");
    expect(core.inputSourceToggled()).toEqual({ emit: "ls", holdTimerMs: HOLD_WINDOW_MS });
    expect(core.holding).toBe(true); // 전환 직후 첫 키를 가를 새 창
    // 보류 키의 keyup이 자모였다면(레이아웃이 이미 한글) 자모로 복원해 확정한다.
    const jamo = new WebKitImeCore();
    jamo.armInputSourceHold();
    jamo.keydown(71, false, false, "g");
    jamo.keyup(71, "ㅎ");
    expect(jamo.inputSourceToggled()).toEqual({ emit: "ㅎ", holdTimerMs: HOLD_WINDOW_MS });
    expect(jamo.automatonText).toBe(""); // 전환 경계에서 오토마타 음절은 확정된다
  });

  it("inputSourceToggled는 IME 조합을 확정하고, 오토마타 음절 뒤 자모가 앞 음절에 붙지 않게 한다", () => {
    const core = new WebKitImeCore();
    core.beforeinput("insertText", "");
    core.input("insertText", "ㅇ", "ㅇ");
    core.beforeinput("insertReplacementText", "ㅇ");
    core.input("insertReplacementText", "안");
    expect(core.inputSourceToggled()).toEqual({ emit: "안", holdTimerMs: HOLD_WINDOW_MS });

    const bypass = new WebKitImeCore();
    expect(bypass.keydown(71, false, false, "ㅎ").emit).toBe("ㅎ"); // IME 우회 자모 — 오토마타
    expect(bypass.keydown(75, false, false, "ㅏ").emit).toBe(`${DEL}하`);
    bypass.inputSourceToggled();
    // 전환 뒤 자모는 새 음절이다(하+ㄴ → 한이 아니라 하ㄴ).
    expect(bypass.keydown(83, false, false, "ㄴ").emit).toBe("ㄴ");
  });
});

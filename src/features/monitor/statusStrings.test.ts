/**
 * monitor 섹션 i18n 연결 검증:
 * - 사전 등록된 en 번역(translate 직접 단언).
 * - statusStrings/format이 저장 언어를 따라가는지(호출 시점 t() 평가).
 * - ko 원문은 04-ui.md §7 형식과 바이트 단위로 일치해야 한다
 *   (compatMatrix.test.tsx가 같은 문구를 단언한다).
 */

import { afterEach, describe, expect, it } from "vitest";
import { translate, useI18nStore } from "../../i18n";
import {
  observeOnlyText,
  paneLimitText,
  pasteTooLargeText,
  priorityText,
  queueWaitText,
  workloadStateText,
  isAbnormalExit,
  isCleanExit,
} from "./statusStrings";
import { formatCores, pressureText } from "./format";

afterEach(() => {
  useI18nStore.setState({ language: null });
});

describe("monitor 사전 en 번역", () => {
  it("큐 대기 문구가 파라미터를 치환해 영어로 나온다", () => {
    expect(translate("en", "monitor.queue.concurrency", { count: 2 })).toBe(
      "Queued: 2 managed workload(s) running.",
    );
    expect(translate("en", "monitor.queue.memoryHeadroom", { need: "1.5 GiB", safe: "512 MiB" })).toBe(
      "Queued: the new workload needs 1.5 GiB, but only 512 MiB is free excluding the safety margin.",
    );
  });

  it("압력·코어 단위·pane 상한 영어 문구", () => {
    expect(translate("en", "monitor.pressure.warning")).toBe("Warning");
    expect(translate("en", "monitor.unit.cores", { value: "2.4" })).toBe("2.4 cores");
    expect(translate("en", "monitor.pane.limit", { count: 8 })).toBe(
      "This tab has reached its limit of 8 panes",
    );
  });
});

describe("statusStrings/format의 언어 전환 반영", () => {
  it("저장 언어를 바꾸면 함수 출력이 함께 바뀐다", () => {
    // node 테스트 환경(navigator 없음) → 기본 언어 ko.
    expect(workloadStateText("RUNNING")).toBe("실행 중");
    expect(priorityText(0)).toBe("높음");
    expect(formatCores(2.4)).toBe("2.4코어");
    // 살아 있지만 소량(모델 대기 중 에이전트)은 0으로 뭉개지 않고 "<0.1"로 구분한다.
    expect(formatCores(0.03)).toBe("<0.1코어");
    expect(formatCores(0)).toBe("0코어");
    expect(formatCores(null)).toBe("—");
    expect(pressureText("CRITICAL")).toBe("위험");

    useI18nStore.setState({ language: "en" });
    expect(workloadStateText("RUNNING")).toBe("Running");
    expect(priorityText(0)).toBe("High");
    expect(formatCores(2.4)).toBe("2.4 cores");
    expect(pressureText("CRITICAL")).toBe("Critical");
    expect(paneLimitText(8)).toBe("This tab has reached its limit of 8 panes");
    expect(pasteTooLargeText(1048576)).toBe(
      "Paste rejected: it exceeds 1 MiB. Please pass it as a file instead.",
    );
  });

  it("ko 관측 문구는 compatMatrix 단언과 바이트 단위로 일치한다", () => {
    expect(observeOnlyText("darwin", "observe")).toBe(
      "관측만 가능: 이 macOS 실행에서는 메모리 강제 상한을 적용할 수 없습니다.",
    );
  });

  it("ko 대기 문구 형식(04-ui.md §7)을 보존한다", () => {
    expect(
      queueWaitText("WAIT_CONCURRENCY", { runningManaged: 3, needBytes: null, safeAvailableBytes: null }),
    ).toBe("대기: 관리 작업 3개가 실행 중입니다.");
  });
});

describe("isAbnormalExit — 이어서 열기 오버레이가 사유 줄을 붙일 종료인가", () => {
  it("정상 종료(코드 0)와 사용자 취소는 아니다", () => {
    expect(isAbnormalExit({ code: 0, reason: "process_exit", detail: null })).toBe(false);
    expect(isAbnormalExit({ code: null, reason: "cancelled", detail: null })).toBe(false);
  });

  it("0이 아닌 종료 코드·코드 없음(시그널)·OOM·저널 한도·미확인은 그렇다", () => {
    expect(isAbnormalExit({ code: 1, reason: "process_exit", detail: null })).toBe(true);
    expect(isAbnormalExit({ code: null, reason: "process_exit", detail: null })).toBe(true);
    expect(isAbnormalExit({ code: null, reason: "oom_kill", detail: "oom_kill 3" })).toBe(true);
    expect(isAbnormalExit({ code: null, reason: "journal_limit", detail: null })).toBe(true);
    expect(isAbnormalExit({ code: null, reason: "something_new", detail: null })).toBe(true);
  });
});

describe("isCleanExit — 오버레이 없이 창을 닫을 종료인가(04-ui §5-1)", () => {
  it("프로그램이 스스로 코드 0으로 끝난 경우만 그렇다", () => {
    expect(isCleanExit({ code: 0, reason: "process_exit", detail: null })).toBe(true);
  });

  it("취소·시그널·0이 아닌 코드·OOM·저널 한도·구 데몬의 미확인은 아니다", () => {
    expect(isCleanExit({ code: 0, reason: "cancelled", detail: null })).toBe(false);
    expect(isCleanExit({ code: 1, reason: "process_exit", detail: null })).toBe(false);
    expect(isCleanExit({ code: null, reason: "process_exit", detail: null })).toBe(false);
    expect(isCleanExit({ code: 0, reason: "oom_kill", detail: "oom_kill 1" })).toBe(false);
    expect(isCleanExit({ code: 0, reason: "journal_limit", detail: null })).toBe(false);
    expect(isCleanExit({ code: 0, reason: "unknown", detail: null })).toBe(false);
  });
});

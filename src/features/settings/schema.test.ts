/**
 * 설정 구조 계약.
 *
 * 이 화면의 실패 방식은 "정의는 늘었는데 문구가 없어 키가 그대로 보인다"와
 * "검색해도 안 나온다" 둘이라, 사전 존재 여부와 검색 동작을 함께 잠근다.
 */

import { describe, expect, it } from "vitest";
import { ko, en } from "../../i18n/sections/settings";
import {
  SETTINGS_GROUPS,
  SETTINGS_ITEMS,
  groupById,
  itemsInGroup,
  searchGroups,
  searchItems,
} from "./schema";

/** 실제 앱과 같은 해석 경로(ko 사전) — index 등록 없이 섹션을 직접 읽는다. */
const t = (key: string): string => ko[key as keyof typeof ko] ?? key;

describe("settings schema", () => {
  it("그룹 id가 유일하다", () => {
    const ids = SETTINGS_GROUPS.map((g) => g.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("항목 id가 유일하고 모두 실재하는 그룹에 속한다", () => {
    const ids = SETTINGS_ITEMS.map((i) => i.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const entry of SETTINGS_ITEMS) {
      expect(() => groupById(entry.group)).not.toThrow();
    }
  });

  it("모든 그룹·항목 문구가 ko/en 사전에 있다", () => {
    for (const group of SETTINGS_GROUPS) {
      for (const key of [group.titleKey, group.hintKey]) {
        expect(ko, `ko: ${key}`).toHaveProperty(key);
        expect(en, `en: ${key}`).toHaveProperty(key);
      }
    }
    for (const entry of SETTINGS_ITEMS) {
      for (const key of [entry.labelKey, entry.descriptionKey]) {
        expect(ko, `ko: ${key}`).toHaveProperty(key);
        expect(en, `en: ${key}`).toHaveProperty(key);
      }
    }
  });

  it("itemsInGroup은 정의 순서를 유지한다(패널 구조 분해가 이 순서를 쓴다)", () => {
    expect(itemsInGroup("general").map((i) => i.id)).toEqual(["language", "theme"]);
    expect(itemsInGroup("terminal").map((i) => i.id)).toEqual([
      "fontSize",
      "fontFamily",
      "cursorStyle",
      "hangulToggle",
      "scrollback",
      "osc52",
      "gpuRenderer",
      "agentBackgrounds",
      "agentBackgroundColors",
      "interventionTextBadge",
      "defaultShell",
      "customShell",
      "terminateOnClose",
      "quitBehavior",
    ]);
    expect(itemsInGroup("trackpad").map((i) => i.id)).toEqual([
      "tabSwipe",
      "tabSwipeSensitivity",
      "tabSwipeReverse",
      "tabSwipeWrap",
      "tabSwitchEffect",
    ]);
    expect(itemsInGroup("compatibility")).toEqual([]);
    expect(itemsInGroup("missions").map((i) => i.id)).toEqual([
      "missionQuickSetup",
      "missionGuide",
      "missionModels",
      "missionTeams",
      "missionVerification",
    ]);
  });

  it("AI 작업 빠른 설정·사용법·충돌 해결 담당을 한/영 낱말로 찾는다", () => {
    const ids = (query: string) => searchItems(query, t).map((i) => i.id);
    for (const query of ["빠른 설정", "quick setup", "로그인", "팀 만들기", "CLI"]) {
      expect(ids(query), query).toContain("missionQuickSetup");
    }
    for (const query of ["사용법", "도움말", "guide"]) {
      expect(ids(query), query).toContain("missionGuide");
    }
    expect(ids("충돌 해결")).toContain("missionTeams");
    // 그룹 설명은 "오케스트레이션" 대신 사용자가 하는 일을 말한다(검색어로는 계속 찾는다).
    expect(ko["settings.missionsHint"]).not.toContain("오케스트레이션");
    expect(ko["settings.item.missionModels.description"]).not.toContain("오케스트레이션");
    expect(ids("오케스트레이션")).toContain("missionModels");
    // Z.ai(GLM) 경로는 이 패널의 역할 피커에서만 고른다(빠른 설정은 Anthropic id만 준다).
    for (const query of ["zai", "z.ai", "glm", "GLM"]) {
      expect(ids(query), query).toContain("missionModels");
    }
  });
});

describe("설정 검색", () => {
  it("빈 질의는 모든 그룹을 그대로 보여 준다", () => {
    expect(searchGroups("   ", t)).toHaveLength(SETTINGS_GROUPS.length);
    expect(searchItems("   ", t)).toEqual([]);
  });

  it("라벨로 항목을 찾는다", () => {
    // "테마"는 agent 배경 설정의 설명 문구에도 등장한다 — 전문 검색이 정상.
    expect(searchItems("테마", t).map((i) => i.id)).toEqual(["theme", "agentBackgrounds", "agentBackgroundColors"]);
  });

  it("사전에 없는 낱말도 keywords로 잡는다(영문 검색·오탈자 대비)", () => {
    expect(searchItems("font", t).map((i) => i.id)).toContain("fontSize");
    expect(searchItems("dark", t).map((i) => i.id)).toContain("theme");
  });

  it("대소문자를 가리지 않는다", () => {
    expect(searchItems("FONT", t).map((i) => i.id)).toEqual(searchItems("font", t).map((i) => i.id));
  });

  it("항목이 맞으면 그 항목의 그룹이 목록에 남는다", () => {
    expect(searchGroups("글꼴", t).map((g) => g.id)).toEqual(["terminal"]);
  });

  it("트랙패드 제스처는 한글·영문 어느 쪽으로도 찾힌다", () => {
    expect(searchItems("스와이프", t).map((i) => i.id)).toContain("tabSwipe");
    expect(searchItems("swipe", t).map((i) => i.id)).toContain("tabSwipe");
    expect(searchGroups("트랙패드", t).map((g) => g.id)).toEqual(["trackpad"]);
  });

  it("항목 정의가 없는 그룹도 이름·설명으로 걸린다", () => {
    expect(searchGroups("호환성", t).map((g) => g.id)).toEqual(["compatibility"]);
  });

  it("아무것도 맞지 않으면 빈 목록이다", () => {
    expect(searchGroups("존재하지않는설정", t)).toEqual([]);
    expect(searchItems("존재하지않는설정", t)).toEqual([]);
  });
});

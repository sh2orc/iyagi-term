/** 패턴 감지(W3-5): 라이브 출력 휴리스틱과 쿨다운 dedup. */
import { describe, expect, it } from "vitest";
import {
  detectInterventionPattern,
  PatternCooldown,
  PATTERN_COOLDOWN_MS,
} from "./interventionPatterns";

describe("detectInterventionPattern", () => {
  it("승인 문구를 permission으로 분류한다", () => {
    expect(detectInterventionPattern("Do you want to proceed? (y/n)")).toBe("permission");
    expect(detectInterventionPattern("Claude needs your permission to use Bash")).toBe("permission");
    expect(detectInterventionPattern("waiting for your approval")).toBe("permission");
  });

  it("입력 대기 문구를 question으로, 일반 출력은 null로", () => {
    expect(detectInterventionPattern("waiting for your input")).toBe("question");
    expect(detectInterventionPattern("build succeeded in 3s")).toBeNull();
    expect(detectInterventionPattern("")).toBeNull();
    expect(detectInterventionPattern("x".repeat(9000))).toBeNull();
  });

  it("여러 패턴이 겹치면 permission이 우선한다", () => {
    expect(detectInterventionPattern("waiting for your approval (y/n)")).toBe("permission");
  });
});

describe("PatternCooldown — 재발화 억제", () => {
  it("같은 세션·패턴은 창 안에서 한 번만 알린다", () => {
    const cooldown = new PatternCooldown();
    expect(cooldown.shouldReport("s1", "permission", 1000)).toBe(true);
    expect(cooldown.shouldReport("s1", "permission", 1000 + PATTERN_COOLDOWN_MS - 1)).toBe(false);
    expect(cooldown.shouldReport("s1", "permission", 1000 + PATTERN_COOLDOWN_MS)).toBe(true);
    // 다른 세션·다른 패턴은 독립적이다.
    expect(cooldown.shouldReport("s2", "permission", 1000)).toBe(true);
    expect(cooldown.shouldReport("s1", "question", 1000)).toBe(true);
  });

  it("세션 정리(clear) 후에는 다시 알릴 수 있다", () => {
    const cooldown = new PatternCooldown();
    cooldown.shouldReport("s1", "permission", 1000);
    expect(cooldown.shouldReport("s1", "permission", 2000)).toBe(false);
    cooldown.clear("s1");
    expect(cooldown.shouldReport("s1", "permission", 3000)).toBe(true);
  });
});

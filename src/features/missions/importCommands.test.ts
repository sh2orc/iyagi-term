import { describe, expect, it } from "vitest";
import { branchSlug, candidateRefName, headRelation, importCommands, isOid, recommendedImport } from "./importCommands";

const BASE = "0".repeat(40);
const COMMIT = "a".repeat(40);

describe("importCommands", () => {
  it("데몬 candidate_ref 규칙과 같은 참조 이름", () => {
    expect(candidateRefName("m-1", "c-2")).toBe("refs/iyagi/missions/m-1/candidates/c-2");
  });

  it("ff-only 병합·새 브랜치·변경 보기 명령", () => {
    const commands = importCommands({ id: "m-1", title: "Add login form" }, { id: "c-2", base_oid: BASE, commit_oid: COMMIT });
    expect(commands).toEqual({
      refName: "refs/iyagi/missions/m-1/candidates/c-2",
      branchName: "ai/add-login-form",
      merge: `git merge --ff-only ${COMMIT}`,
      branch: `git switch -c ai/add-login-form ${COMMIT}`,
      diff: `git diff ${BASE} ${COMMIT}`,
    });
  });

  it("slug는 셸·git에 안전한 [a-z0-9-]만 남긴다", () => {
    expect(branchSlug("  Fix: API 호환성 (v2) / café  ", "m")).toBe("fix-api-v2-cafe");
    expect(branchSlug("로그인 기능", "0192F5A4-5b1e-7c4d")).toBe("mission-0192f5a4");
    expect(branchSlug("---", "---")).toBe("mission");
    const long = branchSlug("a".repeat(30) + " " + "b".repeat(30), "m");
    expect(long.length).toBeLessThanOrEqual(40);
    expect(long.endsWith("-")).toBe(false);
    expect(branchSlug("..lock", "m")).toBe("lock");
  });

  it("HEAD 관계와 추천 명령", () => {
    const candidate = { base_oid: BASE, commit_oid: COMMIT };
    expect(headRelation(null, candidate)).toBe("unknown");
    expect(headRelation(BASE, candidate)).toBe("at_base");
    expect(headRelation(COMMIT, candidate)).toBe("at_commit");
    expect(headRelation("f".repeat(40), candidate)).toBe("moved");
    expect(recommendedImport("at_base")).toBe("merge");
    expect(recommendedImport("moved")).toBe("branch");
    expect(recommendedImport("unknown")).toBe("branch");
    expect(isOid(COMMIT)).toBe(true);
    expect(isOid("HEAD")).toBe(false);
  });
});

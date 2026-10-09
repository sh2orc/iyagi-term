/**
 * 결과 가져오기 명령(순수 함수).
 *
 * 데몬 계약(읽기 전용 확인):
 * - 후보 커밋은 비공개 참조 `refs/iyagi/missions/<mission>/candidates/<candidate>`에
 *   고정된다(`workspace/git.rs` `candidate_ref`). 작업 공간 worktree는 저장소와
 *   객체·참조를 공유하므로 사용자 저장소에서 바로 커밋을 쓸 수 있다.
 * - capture 커밋의 부모는 작업 공간 HEAD = base(`commit_on_private_ref`), 통합
 *   후보는 base에서 출발한 통합 worktree의 HEAD(`integration.rs` 165·170) —
 *   어느 쪽이든 base의 후손이므로 HEAD가 base면 ff-only 병합이 된다.
 * - 변경이 없는 통합 후보(research-only)는 commit_oid == base_oid다.
 * - 사용자 브랜치는 데몬이 절대 움직이지 않는다(자동 반영 없음).
 *
 * 명령은 셸에 붙여 넣는 문자열이다. OID는 16진수, 브랜치 이름은 [a-z0-9-]만
 * 쓰므로 인용이 필요 없다.
 */

export type ImportCommandKind = "merge" | "branch" | "diff";

export interface ImportCommands {
  refName: string;
  branchName: string;
  merge: string;
  branch: string;
  diff: string;
}

export type HeadRelation = "at_base" | "at_commit" | "moved" | "unknown";

const OID_PATTERN = /^[0-9a-f]{40}([0-9a-f]{24})?$/;
const SLUG_MAX = 40;

export function candidateRefName(missionId: string, candidateId: string): string {
  return `refs/iyagi/missions/${missionId}/candidates/${candidateId}`;
}

/**
 * 미션 제목 → 브랜치에 안전한 slug. 영문·숫자만 남기고(발음 구별 기호 제거)
 * 나머지는 `-`로 잇는다. 남는 글자가 없으면(예: 한글 제목) 미션 id 앞 8자.
 */
export function branchSlug(title: string, missionId: string): string {
  const ascii = title
    .normalize("NFKD")
    .replace(/[̀-ͯ]/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  const cut = ascii.slice(0, SLUG_MAX).replace(/-+$/g, "");
  if (cut.length > 0) return cut;
  const idPart = missionId.toLowerCase().replace(/[^a-z0-9]/g, "").slice(0, 8);
  return idPart.length > 0 ? `mission-${idPart}` : "mission";
}

export function isOid(value: string | null | undefined): value is string {
  return typeof value === "string" && OID_PATTERN.test(value);
}

export function importCommands(
  mission: { id: string; title: string },
  candidate: { id: string; base_oid: string; commit_oid: string },
): ImportCommands {
  const branchName = `ai/${branchSlug(mission.title, mission.id)}`;
  return {
    refName: candidateRefName(mission.id, candidate.id),
    branchName,
    merge: `git merge --ff-only ${candidate.commit_oid}`,
    branch: `git switch -c ${branchName} ${candidate.commit_oid}`,
    diff: `git diff ${candidate.base_oid} ${candidate.commit_oid}`,
  };
}

/** 현재 저장소 HEAD와 후보의 관계. HEAD를 모르면 unknown. */
export function headRelation(headOid: string | null, candidate: { base_oid: string; commit_oid: string }): HeadRelation {
  if (!headOid) return "unknown";
  if (headOid === candidate.commit_oid) return "at_commit";
  if (headOid === candidate.base_oid) return "at_base";
  return "moved";
}

/** 추천 명령: HEAD가 base면 ff-only 병합, 그 밖에는 새 브랜치. */
export function recommendedImport(relation: HeadRelation): ImportCommandKind {
  return relation === "at_base" ? "merge" : "branch";
}

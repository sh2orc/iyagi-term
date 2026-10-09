/**
 * SystemProbe 계약 미러 (I11).
 *
 * 본체는 `src/features/bridge/systemProbe.ts`로 동시 구현 중이며 그 모양은
 * 아래와 정확히 같다. bridge 쪽이 완성되면 이 파일을
 * `export type { SystemProbe, CliCandidate } from "../bridge/systemProbe";`
 * 재내보내기로 교체해도 모든 이용처는 그대로 동작한다(이름이 seam이다).
 *
 * - listClis(): PATH에서 codex/claude/opencode 후보를 찾아 실행 파일 경로,
 *   symlink target, 설치 형태(npm shim 등)를 돌려준다(04 §5).
 * - queryVersion(program): 해당 executable의 "검증된" version query만
 *   2초 timeout / 8 KiB output cap으로 실행한다. 검증되지 않았으면
 *   null을 돌려준다 — UI는 절대 값을 지어내지 않는다.
 */

export type CliCandidateKind = "codex" | "claude" | "opencode" | "custom";

export interface CliCandidate {
  /** 발견된 실행 파일/셸 shim 경로(있는 그대로). */
  program: string;
  kind: CliCandidateKind;
  /** symlink/npkg shim이 가리키는 실제 대상(해석 실패 시 null). */
  resolvedTarget: string | null;
  /** 설치 형태 라벨(예: "npm", "native", "homebrew"). */
  installForm: string | null;
}

export interface SystemProbe {
  listClis(): Promise<CliCandidate[]>;
  queryVersion(program: string): Promise<string | null>;
}

/** mock 구현(프로필 폼 미리보기/테스트용) — 감지된 것이 없다면 빈 목록. */
export const mockSystemProbe: SystemProbe = {
  async listClis() {
    return [];
  },
  async queryVersion() {
    return null;
  },
};

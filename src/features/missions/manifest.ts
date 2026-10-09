/**
 * Candidate manifest 계약(데몬 → 화면).
 *
 * 데몬은 후보(Candidate.manifest_ref)에 두 가지 JSON 문서를 쓴다.
 *
 * - capture(작성자 후보, `workspace/capture.rs` `Manifest`):
 *   `{ "base_oid", "entries": [...] }`
 * - integration(통합 후보, `mission/workflow.rs` `manifest_document`):
 *   `{ "base_oid", "commit_oid", "tree_oid", "sources": [...], "entries": [...],
 *      "exclusion_decision_ids"?, "resolution_run_ids"? }`
 *
 * entry는 둘 다 `{ "path", "change": "added"|"modified"|"deleted", "bytes", "sha256" }`
 * 이고, 삭제 파일은 `bytes: 0`, `sha256: ""`로 기록된다(내용이 없다).
 *
 * `files[]`/`op` 형식은 데몬이 쓴 적이 없다 — 받아들이지 않는다(계약 위반을
 * 숨기지 않기 위해). 읽지 못한 문서는 빈 목록이 되고, 호출부는 Candidate의
 * OID로 요약을 보여 준다.
 */

export interface ManifestEntry {
  path: string;
  /** "added" | "modified" | "deleted" (알 수 없는 값은 원문 그대로). */
  change: string;
  /** 후보 커밋에서의 파일 크기. 삭제 파일이거나 값이 없으면 null. */
  bytes: number | null;
}

export interface CandidateManifest {
  baseOid: string | null;
  /** capture manifest에는 없다(null) — Candidate.commit_oid를 쓴다. */
  commitOid: string | null;
  entries: ManifestEntry[];
}

/** 결과 화면이 한 번에 나열하는 파일 수. 나머지는 "외 N개 파일". */
export const MANIFEST_VISIBLE_LIMIT = 20;

const EMPTY: CandidateManifest = { baseOid: null, commitOid: null, entries: [] };

function nonEmptyString(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function entryBytes(value: unknown, change: string): number | null {
  if (change === "deleted") return null;
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) return null;
  return value;
}

/** manifest artifact 본문(JSON)을 읽는다. 실패는 빈 결과(null OID, 빈 목록). */
export function parseCandidateManifest(text: string | null): CandidateManifest {
  if (!text) return { ...EMPTY, entries: [] };
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return { ...EMPTY, entries: [] };
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return { ...EMPTY, entries: [] };
  const record = parsed as { base_oid?: unknown; commit_oid?: unknown; entries?: unknown };
  const entries: ManifestEntry[] = [];
  if (Array.isArray(record.entries)) {
    for (const raw of record.entries) {
      if (typeof raw !== "object" || raw === null) continue;
      const item = raw as { path?: unknown; change?: unknown; bytes?: unknown };
      const path = nonEmptyString(item.path);
      if (path === null) continue;
      const change = nonEmptyString(item.change) ?? "modified";
      entries.push({ path, change, bytes: entryBytes(item.bytes, change) });
    }
  }
  return {
    baseOid: nonEmptyString(record.base_oid),
    commitOid: nonEmptyString(record.commit_oid),
    entries,
  };
}

/** change → i18n 키(알 수 없는 값은 null — 호출부가 원문을 보여 준다). */
export function manifestChangeLabelKey(change: string): string | null {
  switch (change) {
    case "added":
      return "missions.result.changeAdded";
    case "modified":
      return "missions.result.changeModified";
    case "deleted":
      return "missions.result.changeDeleted";
    default:
      return null;
  }
}

/** 파일 크기 표기: 512 B, 3.2 KiB, 1.5 MiB. */
export function formatManifestBytes(bytes: number): string {
  const trim = (value: number) => {
    const rounded = Math.round(value * 10) / 10;
    return Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1);
  };
  if (bytes >= 1024 * 1024 * 1024) return `${trim(bytes / (1024 * 1024 * 1024))} GiB`;
  if (bytes >= 1024 * 1024) return `${trim(bytes / (1024 * 1024))} MiB`;
  if (bytes >= 1024) return `${trim(bytes / 1024)} KiB`;
  return `${bytes} B`;
}

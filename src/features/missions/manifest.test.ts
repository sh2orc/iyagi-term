/**
 * Candidate manifest 계약 시험 — 데몬이 실제로 쓰는 두 형식.
 *
 * - capture: `serde_json::to_vec(&Manifest { base_oid, entries })`
 *   (crates/iyagi-termd/src/workspace/capture.rs)
 * - integration: `manifest_document(..)` + 선택 필드
 *   (crates/iyagi-termd/src/mission/workflow.rs)
 *
 * 삭제 파일은 데몬이 `bytes: 0, sha256: ""`로 기록한다 — 화면은 크기를 표시하지 않는다(null).
 */

import { describe, expect, it } from "vitest";
import { MANIFEST_VISIBLE_LIMIT, formatManifestBytes, manifestChangeLabelKey, parseCandidateManifest } from "./manifest";

const BASE = "3f786850e387550fdab836ed7e6dc881de23001b";
const COMMIT = "89e6c98d92887913cadf06b2adb97f26cde4849b";
const TREE = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const SHA = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/** serde가 struct 필드 순서대로 직렬화한 capture manifest(공백 없음). */
const CAPTURE_MANIFEST =
  `{"base_oid":"${BASE}","entries":[` +
  `{"path":"src/login.ts","change":"added","bytes":1834,"sha256":"${SHA}"},` +
  `{"path":"src/app.ts","change":"modified","bytes":20480,"sha256":"${SHA}"},` +
  `{"path":"src/legacy.ts","change":"deleted","bytes":0,"sha256":""}]}`;

/** serde_json::Value(json!) 문서 — preserve_order가 없어 키는 사전순으로 직렬화된다. */
const INTEGRATION_MANIFEST = JSON.stringify({
  base_oid: BASE,
  commit_oid: COMMIT,
  entries: [
    { bytes: 12, change: "modified", path: "README.md", sha256: SHA },
    { bytes: 0, change: "deleted", path: "docs/old.md", sha256: "" },
  ],
  exclusion_decision_ids: ["0192f5a4-5b1e-7c4d-9a51-3d2f0e6b7a10"],
  resolution_run_ids: [],
  sources: [
    {
      base_oid: BASE,
      candidate_id: "0192f5a4-5b1e-7c4d-9a51-3d2f0e6b7a11",
      commit_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      source_run_ids: ["0192f5a4-5b1e-7c4d-9a51-3d2f0e6b7a12"],
      tree_oid: TREE,
    },
  ],
  tree_oid: TREE,
});

describe("parseCandidateManifest — 데몬 계약", () => {
  it("capture manifest: base_oid + entries, commit_oid는 없다", () => {
    const manifest = parseCandidateManifest(CAPTURE_MANIFEST);
    expect(manifest.baseOid).toBe(BASE);
    expect(manifest.commitOid).toBeNull();
    expect(manifest.entries).toEqual([
      { path: "src/login.ts", change: "added", bytes: 1834 },
      { path: "src/app.ts", change: "modified", bytes: 20480 },
      { path: "src/legacy.ts", change: "deleted", bytes: null },
    ]);
  });

  it("integration manifest: commit/tree/sources와 선택 필드가 있어도 entries를 읽는다", () => {
    const manifest = parseCandidateManifest(INTEGRATION_MANIFEST);
    expect(manifest.baseOid).toBe(BASE);
    expect(manifest.commitOid).toBe(COMMIT);
    expect(manifest.entries).toEqual([
      { path: "README.md", change: "modified", bytes: 12 },
      { path: "docs/old.md", change: "deleted", bytes: null },
    ]);
  });

  it("변경 없는 통합 후보(research-only)는 빈 entries", () => {
    const manifest = parseCandidateManifest(
      JSON.stringify({ base_oid: BASE, commit_oid: BASE, entries: [], sources: [], tree_oid: TREE }),
    );
    expect(manifest).toEqual({ baseOid: BASE, commitOid: BASE, entries: [] });
  });

  it("files[]/op 형식은 데몬 계약이 아니므로 파일로 세지 않는다", () => {
    const manifest = parseCandidateManifest(JSON.stringify({ files: [{ path: "src/login.ts", op: "add" }] }));
    expect(manifest.entries).toEqual([]);
  });

  it("읽을 수 없는 본문·잘못된 항목은 안전하게 버린다", () => {
    for (const text of [null, "", "not json", "[]", "null", "42"]) {
      expect(parseCandidateManifest(text)).toEqual({ baseOid: null, commitOid: null, entries: [] });
    }
    const manifest = parseCandidateManifest(JSON.stringify({
      base_oid: "",
      entries: [null, 3, { change: "added" }, { path: "" }, { path: "a.txt", bytes: -1 }, { path: "b.txt", change: "renamed", bytes: 1.5 }],
    }));
    expect(manifest.baseOid).toBeNull();
    expect(manifest.entries).toEqual([
      { path: "a.txt", change: "modified", bytes: null },
      { path: "b.txt", change: "renamed", bytes: null },
    ]);
  });
});

describe("manifest 표시 도구", () => {
  it("change 라벨 키는 added/modified/deleted만 번역한다", () => {
    expect(manifestChangeLabelKey("added")).toBe("missions.result.changeAdded");
    expect(manifestChangeLabelKey("modified")).toBe("missions.result.changeModified");
    expect(manifestChangeLabelKey("deleted")).toBe("missions.result.changeDeleted");
    expect(manifestChangeLabelKey("renamed")).toBeNull();
  });

  it("크기 표기와 목록 한도", () => {
    expect(formatManifestBytes(0)).toBe("0 B");
    expect(formatManifestBytes(1023)).toBe("1023 B");
    expect(formatManifestBytes(1536)).toBe("1.5 KiB");
    expect(formatManifestBytes(20480)).toBe("20 KiB");
    expect(formatManifestBytes(3 * 1024 * 1024)).toBe("3 MiB");
    expect(MANIFEST_VISIBLE_LIMIT).toBe(20);
  });
});

/**
 * 작업 공간 정리 공용 도구(계약 D): 크기 표기, 정리 가능/막힘 판정, 이유 문구 폴백.
 */

import { describe, expect, it } from "vitest";
import { t } from "../../i18n";
import type { WorkspaceUsageEntry } from "../../generated/WorkspaceUsageEntry";
import {
  formatStorageSize,
  workspaceBlockedReasonKey,
  workspaceCleanable,
  workspaceCleanupBlocked,
  workspaceKeptReasonKey,
  workspaceUsageEntry,
} from "./workspaceUsage";

function entry(overrides: Partial<WorkspaceUsageEntry> = {}): WorkspaceUsageEntry {
  return { mission_id: "m-1", workspaces: 1, bytes: "2048", cleanable: true, blocked_reason: null, ...overrides };
}

describe("formatStorageSize", () => {
  it("B/KB/MB/GB로 줄이고 소수는 한 자리만 남긴다(U64 문자열도 받는다)", () => {
    expect(formatStorageSize(0)).toBe("0 B");
    expect(formatStorageSize(512)).toBe("512 B");
    expect(formatStorageSize(1024)).toBe("1 KB");
    expect(formatStorageSize(1536)).toBe("1.5 KB");
    expect(formatStorageSize("1048576")).toBe("1 MB");
    expect(formatStorageSize("5242880")).toBe("5 MB");
    expect(formatStorageSize(String(3 * 1024 ** 3))).toBe("3 GB");
    expect(formatStorageSize(1.25 * 1024 ** 3)).toBe("1.3 GB");
  });

  it("반올림으로 1024가 되면 다음 단위로 올리고, 읽을 수 없는 값은 0 B", () => {
    expect(formatStorageSize(1024 * 1024 - 1)).toBe("1 MB");
    expect(formatStorageSize("not-a-number")).toBe("0 B");
    expect(formatStorageSize(null)).toBe("0 B");
    expect(formatStorageSize(-5)).toBe("0 B");
  });
});

describe("정리 가능 판정", () => {
  it("cleanable이고 지울 것이 남아 있을 때만 정리 가능하다", () => {
    expect(workspaceCleanable(entry())).toBe(true);
    expect(workspaceCleanable(entry({ workspaces: 0, bytes: "0" }))).toBe(false);
    expect(workspaceCleanable(entry({ cleanable: false, blocked_reason: "mission_active" }))).toBe(false);
    expect(workspaceCleanable(null)).toBe(false);
  });

  it("막힌 항목은 이유가 있고 지울 것이 남아 있을 때만 막힘으로 본다", () => {
    expect(workspaceCleanupBlocked(entry({ cleanable: false, blocked_reason: "run_unreconciled" }))).toBe(true);
    expect(workspaceCleanupBlocked(entry({ cleanable: false, blocked_reason: null }))).toBe(false);
    expect(workspaceCleanupBlocked(entry({ cleanable: false, blocked_reason: "mission_active", workspaces: 0, bytes: "0" }))).toBe(false);
    expect(workspaceCleanupBlocked(entry())).toBe(false);
    expect(workspaceCleanupBlocked(null)).toBe(false);
  });

  it("두 판정은 서로 독립이다 — 정리 가능하지 않다는 결과가 막힘 판정의 입력 타입을 좁히지 않는다", () => {
    // 이전에는 둘 다 `entry is WorkspaceUsageEntry` 판별 함수라 앞의 false 분기에서 entry가 never로 좁혀졌다.
    const blocked: WorkspaceUsageEntry | null = entry({ cleanable: false, blocked_reason: "mission_active" });
    const cleanable = workspaceCleanable(blocked);
    const isBlocked = blocked !== null && !cleanable && workspaceCleanupBlocked(blocked);
    expect(cleanable).toBe(false);
    expect(isBlocked).toBe(true);
    expect(blocked !== null && !cleanable ? blocked.blocked_reason : null).toBe("mission_active");
  });

  it("사용량 결과에서 작업별 항목을 찾는다", () => {
    const usage = { missions: [entry({ mission_id: "a" }), entry({ mission_id: "b", bytes: "9" })], total_bytes: "2057" };
    expect(workspaceUsageEntry(usage, "b")?.bytes).toBe("9");
    expect(workspaceUsageEntry(usage, "c")).toBeNull();
    expect(workspaceUsageEntry(null, "a")).toBeNull();
  });
});

describe("이유 문구", () => {
  it("알려진 slug는 전용 문구, 모르는 slug는 원문 대신 일반 문구", () => {
    expect(workspaceBlockedReasonKey("mission_active")).toBe("missions.workspaceCleanup.blocked.missionActive");
    expect(workspaceBlockedReasonKey("run_unreconciled")).toBe("missions.workspaceCleanup.blocked.runUnreconciled");
    expect(workspaceBlockedReasonKey("brand_new")).toBe("missions.workspaceCleanup.blocked.other");
    expect(workspaceBlockedReasonKey(null)).toBe("missions.workspaceCleanup.blocked.other");
    expect(workspaceKeptReasonKey("dirty")).toBe("missions.workspaceCleanup.kept.dirty");
    expect(workspaceKeptReasonKey("run_active")).toBe("missions.workspaceCleanup.kept.inUse");
    expect(workspaceKeptReasonKey("deferred")).toBe("missions.workspaceCleanup.kept.deferred");
    expect(workspaceKeptReasonKey("quarantined")).toBe("missions.workspaceCleanup.kept.quarantined");
    expect(workspaceKeptReasonKey("brand_new")).toBe("missions.workspaceCleanup.kept.other");
  });
});

it("데몬이 보내는 남긴 항목 이유는 모두 전용 또는 일반 문구로 번역된다", () => {
  const slugs = ["not_daemon_owned", "run_active", "quarantined", "repository_unavailable", "unregistered_worktree", "dirty", "status_unavailable", "deferred", "remove_failed"];
  for (const slug of slugs) {
    const text = t(workspaceKeptReasonKey(slug));
    expect(text).not.toContain("missions.");
    expect(text).not.toContain(slug);
  }
  for (const slug of ["mission_active", "run_unreconciled"]) {
    expect(t(workspaceBlockedReasonKey(slug))).not.toContain("missions.");
  }
});

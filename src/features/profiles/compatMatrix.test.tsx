/**
 * 호환성 매트릭스: R1 기본 행(Windows/macOS/Linux, 출처 라벨),
 * 현재 실행 행 추가, macOS 관측만 문구(04 §7 원문), 미지원 셀 이유
 * tooltip 렌더링.
 */

import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";
import type { Capabilities } from "../../generated/Capabilities";
import { translate } from "../../i18n";
import { CompatibilityMatrix } from "./CompatibilityMatrix";
import { COMPAT_COLUMN_LABELS, LIVE_SOURCE, R1_DEFAULT_SOURCE, buildCompatRows } from "./compatMatrix";

const MACOS_OBSERVE_ONLY = "관측만 가능: 이 macOS 실행에서는 메모리 강제 상한을 적용할 수 없습니다.";

function liveCaps(overrides: Partial<Capabilities> = {}): Capabilities {
  return {
    memory_limit_kind: { support: "unsupported", reason: "mock executor는 OS 메모리 상한을 적용하지 않습니다" },
    cpu_quota: { support: "supported" },
    process_count_limit: { support: "permission_required", reason: "잡 권한 없음" },
    tree_accounting: { support: "unsupported", reason: "mock" },
    reattach: { support: "supported" },
    resume: { support: "unsupported", reason: "R1" },
    scheduling_yield: { support: "supported" },
    suspend_resume: { support: "supported" },
    platform: "mock",
    claude_provider_routing: false,
    ...overrides,
  };
}

describe("buildCompatRows — R1 기본표 (03 §5–§7)", () => {
  it("3플랫폼 행 + 지정한 열 순서 + 출처 라벨", () => {
    const rows = buildCompatRows(null);
    expect(rows.map((r) => r.platform)).toEqual(["Windows", "macOS", "Linux"]);
    expect(COMPAT_COLUMN_LABELS).toEqual([
      "compat.col.memory",
      "compat.col.cpu",
      "compat.col.pids",
      "compat.col.tree",
      "compat.col.reattach",
    ]);
    for (const row of rows) {
      expect(row.cells).toHaveLength(5);
      expect(row.source).toBe(R1_DEFAULT_SOURCE);
      for (const cell of row.cells) expect(cell.source).toBe(R1_DEFAULT_SOURCE);
    }
  });

  it("macOS 행은 메모리/CPU 미지원이고 관측만 문구(04 §7 원문)가 붙는다", () => {
    const mac = buildCompatRows(null)[1];
    expect(mac.cells[0]).toMatchObject({ text: "미지원", support: "unsupported" });
    expect(mac.cells[1].support).toBe("unsupported");
    expect(mac.notice).toBe(MACOS_OBSERVE_ONLY);
  });

  it("Linux는 조건부(cgroup 위임)로 표시된다", () => {
    const linux = buildCompatRows(null)[2];
    expect(linux.cells.slice(0, 4).every((c) => c.support === "conditional" && c.text === "조건부")).toBe(true);
    expect(linux.cells[0].reason).toContain("cgroup");
  });

  it("Windows 기본 행은 Job 기준 설명을 담는다", () => {
    const win = buildCompatRows(null)[0];
    expect(win.cells.every((c) => c.support === "supported")).toBe(true);
    expect(win.cells[0].reason).toContain("commit");
  });
});

describe("buildCompatRows — 현재 실행 행(snapshot.capabilities)", () => {
  it("capabilities를 넘기면 4번째 행으로 현재 실행 상태가 붙는다", () => {
    const rows = buildCompatRows(liveCaps({ platform: "darwin" }));
    expect(rows).toHaveLength(4);
    const live = rows[3];
    expect(live.platform).toBe("현재 실행 · macOS");
    expect(live.source).toBe(LIVE_SOURCE);
    expect(live.cells[0]).toMatchObject({ text: "미지원", support: "unsupported" });
    expect(live.cells[1].text).toBe("지원");
    expect(live.cells[2]).toMatchObject({ text: "권한 필요", support: "permission_required" });
    expect(live.notice).toBe(MACOS_OBSERVE_ONLY);
  });

  it("macOS가 아닌 플랫폼도 메모리 미지원이면 같은 형태의 관측만 안내를 낸다", () => {
    const rows = buildCompatRows(liveCaps({ platform: "windows" }));
    expect(rows[3].notice).toBe(
      "관측만 가능: 이 Windows 실행에서는 메모리 강제 상한을 적용할 수 없습니다.",
    );
  });

  it("메모리가 지원되면 관측만 안내가 없다", () => {
    const rows = buildCompatRows(liveCaps({ memory_limit_kind: { support: "supported" } }));
    expect(rows[3].notice).toBeNull();
  });
});

describe("CompatibilityMatrix 렌더링 (데이터 → 문자열)", () => {
  it("기본표만: 열 제목·플랫폼·macOS 관측만 문구 렌더", () => {
    const html = renderToString(<CompatibilityMatrix />);
    expect(html).toContain("플랫폼별 자원 적용 범위");
    expect(html).toContain("메모리 하한");
    expect(html).toContain("재접속");
    expect(html).toContain("Windows");
    expect(html).toContain("macOS");
    expect(html).toContain("Linux");
    expect(html).toContain(MACOS_OBSERVE_ONLY);
    expect(html).toContain("조건부");
  });

  it("현재 실행 행: 지원/미지원/권한 필요 + 이유 tooltip(title) 노출", () => {
    const html = renderToString(<CompatibilityMatrix capabilities={liveCaps()} />);
    expect(html).toContain("현재 실행 · mock");
    expect(html).toContain("지원");
    expect(html).toContain("미지원");
    expect(html).toContain("권한 필요");
    expect(html).toContain("title=\"mock executor는 OS 메모리 상한을 적용하지 않습니다 — 출처: 현재 실행 capabilities (snapshot)\"");
    expect(html).toContain("title=\"잡 권한 없음 — 출처: 현재 실행 capabilities (snapshot)\"");
  });
});

describe("en 번역 검증 (i18n 키 → 영어 문구)", () => {
  it("호환성 셀/출처 문구가 영어 사전으로 나온다", () => {
    expect(translate("en", "compat.cell.supported")).toBe("Supported");
    expect(translate("en", "compat.cell.permission_required")).toBe("Permission required");
    expect(translate("en", R1_DEFAULT_SOURCE)).toBe("R1 document defaults (03 §5–§7)");
    expect(translate("en", LIVE_SOURCE)).toBe("Current run capabilities (snapshot)");
    expect(translate("en", COMPAT_COLUMN_LABELS[0])).toBe("Memory floor");
    expect(translate("en", "compat.currentRun", { label: "macOS" })).toBe("Current run · macOS");
  });
});

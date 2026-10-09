/**
 * 호환성 매트릭스 데이터 (I11, 06 §1 — "Windows managed 지원 범위와
 * direct-shell 지원 범위를 UI 및 호환표에 구분").
 *
 * 행 = 플랫폼(Windows/macOS/Linux + 현재 실행), 열 = 메모리 하한 /
 * CPU quota / 프로세스 수 상한 / 트리 회계 / 재접속. cells는 전달받은
 * Capabilities(현재 실행) 또는 R1 문서 기본값(03 §5–§7)에서 만들고
 * 각 cell에 출처 라벨을 붙인다. macOS 관측만 문구는 04 §7 원문 그대로.
 */

import type { Capabilities } from "../../generated/Capabilities";
import type { LimitCapability } from "../../generated/LimitCapability";
import type { LimitSupport } from "../../generated/LimitSupport";
import { t } from "../../i18n";
import { observeOnlyText } from "../monitor/statusStrings";

/** 열 라벨 — 값은 i18n 키(렌더 시점에 t()로 변환). */
export const COMPAT_COLUMN_LABELS = [
  "compat.col.memory",
  "compat.col.cpu",
  "compat.col.pids",
  "compat.col.tree",
  "compat.col.reattach",
] as const;

export type CompatCellSupport = LimitSupport | "conditional";

export interface CompatCell {
  text: string;
  support: CompatCellSupport;
  /** tooltip용 이유(구현 디테일은 상세 진단에 두고 여기선 사용자 관점 설명). */
  reason: string | null;
  /** 이 cell의 출처(현재 실행/R1 문서 기본값) — i18n 키. */
  source: string;
}

export interface CompatRow {
  platform: string;
  cells: CompatCell[];
  /** 행 전체 안내(예: macOS 관측만 문구). */
  notice: string | null;
  source: string;
}

/** 출처 라벨 — 값은 i18n 키(표시 시점에 t()로 변환). */
export const R1_DEFAULT_SOURCE = "compat.source.r1";
export const LIVE_SOURCE = "compat.source.live";

function cell(support: CompatCellSupport, reason: string | null, source: string): CompatCell {
  const text = t(`compat.cell.${support}`);
  return { text, support, reason, source };
}

/** 문서 기본 행(03 §5 Linux, §6 Windows, §7 macOS). */
export function r1DefaultRows(): CompatRow[] {
  // 호출 시점에 평가(monitor statusStrings의 문구가 언어를 반영할 수 있게).
  const macOSObserveOnly = observeOnlyText("darwin", "observe");
  const linux = (reason: string): CompatCell => cell("conditional", reason, R1_DEFAULT_SOURCE);
  const win = (reason: string): CompatCell => cell("supported", reason, R1_DEFAULT_SOURCE);
  const no = (reason: string): CompatCell => cell("unsupported", reason, R1_DEFAULT_SOURCE);
  return [
    {
      platform: "Windows",
      cells: [
        win(t("compat.reason.win.memory")),
        win(t("compat.reason.win.cpu")),
        win(t("compat.reason.win.pids")),
        win(t("compat.reason.win.tree")),
        win(t("compat.reason.reattach")),
      ],
      notice: null,
      source: R1_DEFAULT_SOURCE,
    },
    {
      platform: "macOS",
      cells: [
        no(t("compat.reason.mac.memory")),
        no(t("compat.reason.mac.cpu")),
        no(t("compat.reason.mac.pids")),
        no(t("compat.reason.mac.tree")),
        win(t("compat.reason.reattach")),
      ],
      notice: macOSObserveOnly,
      source: R1_DEFAULT_SOURCE,
    },
    {
      platform: "Linux",
      cells: [
        linux(t("compat.reason.linux.memory")),
        linux(t("compat.reason.linux.cpu")),
        linux(t("compat.reason.linux.pids")),
        linux(t("compat.reason.linux.tree")),
        win(t("compat.reason.reattach")),
      ],
      notice: null,
      source: R1_DEFAULT_SOURCE,
    },
  ];
}

export function platformLabel(platform: string): string {
  const key = platform.toLowerCase();
  if (key === "darwin" || key === "macos") return "macOS";
  if (key === "windows" || key === "win32") return "Windows";
  if (key === "linux") return "Linux";
  return platform;
}

function liveCell(cap: LimitCapability): CompatCell {
  return cell(cap.support, cap.reason ?? null, LIVE_SOURCE);
}

/**
 * 표시할 행들을 만든다: R1 기본 3행 + (capabilities가 있으면) 현재 실행 행.
 * 현재 실행 행의 macOS 문구는 04 §7 원문으로 생성한다.
 */
export function buildCompatRows(capabilities: Capabilities | null): CompatRow[] {
  const rows = r1DefaultRows();
  if (!capabilities) return rows;
  const label = platformLabel(capabilities.platform);
  const memory = liveCell(capabilities.memory_limit_kind);
  const notice =
    memory.support !== "supported"
      ? (observeOnlyText(capabilities.platform, "observe") ??
        t("compat.observeOnlyFallback", { label }))
      : null;
  rows.push({
    platform: t("compat.currentRun", { label }),
    cells: [
      memory,
      liveCell(capabilities.cpu_quota),
      liveCell(capabilities.process_count_limit),
      liveCell(capabilities.tree_accounting),
      liveCell(capabilities.reattach),
    ],
    notice,
    source: LIVE_SOURCE,
  });
  return rows;
}

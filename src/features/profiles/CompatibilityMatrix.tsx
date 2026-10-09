/**
 * 호환성 매트릭스 표 (I11): R1 기본 3플랫폼 행 + 현재 실행 행.
 * 셀에는 지원/미지원/권한 필요/조건부와 출처가 있고, 이유는 tooltip으로
 * 노출한다. 관리 실행 대화상자(접힘 영역)와 설정 섹션에서 함께 쓴다.
 */

import { memo } from "react";
import type { Capabilities } from "../../generated/Capabilities";
import { useI18n } from "../../i18n";
import { COMPAT_COLUMN_LABELS, buildCompatRows } from "./compatMatrix";

export interface CompatibilityMatrixProps {
  /** 현재 실행 capabilities(null이면 R1 문서 기본 행만). */
  capabilities?: Capabilities | null;
  /** 표 제목(기본: "플랫폼별 자원 적용 범위"). */
  title?: string;
}

export const CompatibilityMatrix = memo(function CompatibilityMatrix(
  props: CompatibilityMatrixProps,
): JSX.Element {
  const { t } = useI18n();
  const rows = buildCompatRows(props.capabilities ?? null);
  const title = props.title ?? t("compat.title");
  return (
    <div className="compat-matrix" aria-label={title}>
      <table>
        <caption className="muted">{title}</caption>
        <thead>
          <tr>
            <th scope="col">{t("compat.platform")}</th>
            {COMPAT_COLUMN_LABELS.map((labelKey) => (
              <th key={labelKey} scope="col">
                {t(labelKey)}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={row.platform}>
              <th scope="row">{row.platform}</th>
              {row.cells.map((c, i) => {
                const sourceText = t("compat.source", { source: t(c.source) });
                return (
                  <td
                    key={i}
                    className={`compat-cell compat-${c.support}`}
                    title={[c.reason, sourceText].filter(Boolean).join(" — ") || sourceText}
                  >
                    {c.text}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
      {rows
        .filter((row) => row.notice !== null)
        .map((row) => (
          <p key={`${row.platform}-notice`} className="workload-notice" role="note">
            {row.notice}
          </p>
        ))}
    </div>
  );
});

/**
 * 상태 안내 한 줄(05-ui §6 헤더 notice 형식).
 *
 * 한 줄 상태 + 행동 버튼 + `자세히` 접기. 긴 설명은 접힌 곳에만 둔다.
 * 낭독 영역(role=status/aria-live)을 만들지 않는다 — 결정 알림은 배너가 맡는다.
 */

import type { ReactNode } from "react";
import { useI18n } from "../../i18n";
import "./decisions.css";

export interface StatusNoticeProps {
  testId: string;
  line: ReactNode;
  details?: ReactNode;
  actions?: ReactNode;
}

export function StatusNotice(props: StatusNoticeProps): JSX.Element {
  const { t } = useI18n();
  return (
    <div className="mission-status-notice" data-testid={props.testId}>
      <div className="mission-status-notice-row">
        <span className="mission-status-notice-line" data-testid={`${props.testId}-line`}>{props.line}</span>
        {props.actions ? <span className="mission-status-notice-actions">{props.actions}</span> : null}
      </div>
      {props.details ? (
        <details className="mission-status-notice-details">
          <summary>{t("missions.common.details")}</summary>
          <div className="mission-status-notice-body">{props.details}</div>
        </details>
      ) : null}
    </div>
  );
}

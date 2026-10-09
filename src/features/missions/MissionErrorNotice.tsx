/**
 * MissionErrorNotice — missionError() 결과 한 건을 보여 준다.
 *
 * - role="alert"는 이 컨테이너 한 곳에만 둔다(중첩 낭독 금지).
 * - 원인 문장 → 행동 버튼(있을 때) → `자세히`(원문 code: message).
 * - login은 외부 동작이 없어도 버튼이 로그인 방법 안내를 펼친다.
 */

import { useState } from "react";
import { useI18n } from "../../i18n";
import type { MissionErrorAction, MissionUiError } from "./errors";
import "./decisions.css";

const ACTION_LABEL: Record<MissionErrorAction, string> = {
  resync: "missions.error.action.resync",
  open_settings: "missions.error.action.openSettings",
  retry: "missions.error.action.retry",
  login: "missions.error.action.login",
  change_model: "missions.error.action.changeModel",
  open_list: "missions.error.action.openList",
};

export function MissionErrorNotice(props: {
  error: MissionUiError;
  onAction?: (action: MissionErrorAction) => void;
  onRetry?: () => void;
}): JSX.Element {
  const { t } = useI18n();
  const { error } = props;
  const [loginOpen, setLoginOpen] = useState(false);
  const action = error.action;
  const handled = action === "login"
    || (action === "retry" && (props.onRetry !== undefined || props.onAction !== undefined))
    || (action !== null && action !== "retry" && props.onAction !== undefined);

  const run = () => {
    if (action === null) return;
    if (action === "login") {
      setLoginOpen((open) => !open);
      props.onAction?.("login");
      return;
    }
    if (action === "retry" && props.onRetry) {
      props.onRetry();
      return;
    }
    props.onAction?.(action);
  };

  return (
    <div className="mission-error-notice" role="alert" data-testid="mission-error">
      <p className="mission-error-message" data-testid="mission-error-message">{error.message}</p>
      {action !== null && handled ? (
        <div className="mission-error-actions">
          <button
            type="button"
            onClick={run}
            aria-expanded={action === "login" ? loginOpen : undefined}
            data-testid="mission-error-action"
            data-action={action}
          >
            {t(ACTION_LABEL[action])}
          </button>
        </div>
      ) : null}
      {action === "login" && loginOpen ? (
        <p className="mission-error-login" data-testid="mission-error-login">{t("missions.error.loginHelp")}</p>
      ) : null}
      {error.detail ? (
        <details className="mission-error-detail">
          <summary>{t("missions.common.details")}</summary>
          <pre>{error.detail}</pre>
        </details>
      ) : null}
    </div>
  );
}

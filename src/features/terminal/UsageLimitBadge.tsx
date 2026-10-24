/**
 * 사용량 한도 카운트다운 배지(탭·pane 헤더 공용).
 *
 * - 시간은 공유 티커(useNow)를 쓴다 — pane마다 인터벌을 두면 창 수만큼
 *   타이머가 는다(lastOutput의 원칙). 구독하는 건 막힌 배지뿐이다.
 * - role="status"를 붙이지 않는다: 카운트다운은 계속 갱신되어 낭독기가
 *   매번 다시 읽는다(mission 배지와 같은 판단). title 툴팁으로 의미를 준다.
 * - 리셋이 지나도 짧은 유예 동안은 보이다가 스스로 사라진다(경계 흔들림
 *   방지). 리셋 시각을 모르면 "한도 초과" 정적 문구로 퇴화한다.
 */

import { useI18n } from "../../i18n";
import { formatDuration } from "../schedule/scheduleText";
import { usageLimitVisible } from "./agentUsage";
import { useNow } from "./lastOutput";

export function UsageLimitBadge(props: {
  /** 리셋 시각(unix ms). null이면 정적 변형, undefined면 아예 그리지 않는다. */
  resetsAtMs: number | null | undefined;
  variant: "tab" | "pane";
}): JSX.Element | null {
  const { t } = useI18n();
  const now = useNow();
  if (props.resetsAtMs === undefined) return null;
  if (!usageLimitVisible(props.resetsAtMs, now)) return null;
  const label =
    props.resetsAtMs === null
      ? t("app.usageLimitNoReset")
      : formatDuration(Math.max(0, props.resetsAtMs - now), t);
  const className =
    props.variant === "tab"
      ? "tab-badge tab-badge-usage-limit"
      : "pane-usage-limit";
  return (
    <span className={className} title={t("app.usageLimitBadge")}>
      ⏳ {label}
    </span>
  );
}

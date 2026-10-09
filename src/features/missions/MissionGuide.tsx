import { useI18n } from "../../i18n";

/** 펼치면 기본 흐름 다섯 줄만 보인다: 준비 → 시작 → 진행 → 확인 → 확정. */
export const MISSION_GUIDE_STEPS = ["configure", "goal", "work", "review", "accept"] as const;

/** `문제가 생겼을 때` 안에 접어 두는 문단(각 2문장 이내). 운영·내부 세부는 사용자 문서가 맡는다. */
export const MISSION_GUIDE_TROUBLE = [
  "repository",
  "plan",
  "messages",
  "controls",
  "time",
  "cost",
  "quota",
  "retry",
  "planRepair",
  "requiredRepair",
  "verification",
  "recovery",
] as const;

/**
 * Available beside creation and in settings, including before any mission exists.
 * `settingId` marks this block as a settings row so 설정 검색 can scroll to it.
 */
export function MissionGuide({ settingId }: { settingId?: string }): JSX.Element {
  const { t }=useI18n();
  return <details className="mission-guide" data-testid="mission-guide" data-setting-id={settingId}>
    <summary>{t("missions.guide.title")}</summary>
    <ol data-testid="mission-guide-steps">{MISSION_GUIDE_STEPS.map(step=><li key={step}>{t(`missions.guide.${step}`)}</li>)}</ol>
    <details className="mission-guide-trouble" data-testid="mission-guide-trouble">
      <summary>{t("missions.guide.troubleTitle")}</summary>
      {MISSION_GUIDE_TROUBLE.map(item=><p key={item} className="muted">{t(`missions.guide.${item}`)}</p>)}
    </details>
  </details>;
}

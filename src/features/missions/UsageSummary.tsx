import type { Run } from "../../generated/Run";
import { useI18n } from "../../i18n";
import { microsToDollars, summarizeUsage } from "./costs";

export function UsageSummary({runs}: {runs: readonly Run[]}): JSX.Element {
  const { t } = useI18n();
  const total = summarizeUsage(runs);
  const tokens = (known: bigint, unknown: number) => unknown
    ? t("missions.cost.partial", { known: known.toLocaleString(), count: unknown }) : known.toLocaleString();
  return <div data-testid="usage-summary">
    <p>{t("missions.result.runCount", { count: runs.length })} · {t("missions.result.usage", {
      input: tokens(total.input, total.unknownInput), output: tokens(total.output, total.unknownOutput),
      cost: total.reportedRuns ? t("missions.cost.reported", {amount: `$${microsToDollars(total.observed)}`}) : t("missions.cost.notReported"),
    })}</p>
    <p>{t("missions.cost.estimates", { amount: `$${microsToDollars(total.estimated)}`, count: total.unknownCost })}</p>
    <p className="muted">{t("missions.cost.admissionNote")}</p>
  </div>;
}

/**
 * 자원 정책 편집기 (03 §3·§8, 04 §1): enforcement 세그먼트(관측만/
 * 가능하면 적용/필수) + 현재 실행 capability 현실 표시 + 예약(GiB
 * stepper, 최소 256 MiB) + cpu_slots + 상한 3종("안전장치 (강제 아님)").
 *
 * snapshot.capabilities가 있으면 require/prefer에서 어떤 상한이
 * 미지원/권한 필요인지 이유와 함께 보여 준다. 게이트 판정은
 * ManagedRunDialog가 capabilityGate로 수행한다(버튼 비활성화).
 */

import { memo } from "react";
import type { Capabilities } from "../../generated/Capabilities";
import { useI18n } from "../../i18n";
import {
  CAPS_SECTION_LABEL,
  CAPS_SECTION_NOTE,
  ENFORCEMENT_OPTIONS,
  limitRealities,
} from "./capabilityUi";
import { MIN_RESERVATION_GIB, draftToNumericPolicy, type PolicyDraft } from "./policyDraft";
import { formatGiB } from "../monitor/format";

export interface PolicyEditorProps {
  draft: PolicyDraft;
  onChange: (patch: Partial<PolicyDraft>) => void;
  capabilities: Capabilities | null;
  idPrefix: string;
}

export const PolicyEditor = memo(function PolicyEditor(props: PolicyEditorProps): JSX.Element {
  const { draft, onChange, capabilities, idPrefix } = props;
  const { t } = useI18n();

  const stepReservation = (delta: number): void => {
    const next = Math.round((draft.reservationGiB + delta) * 100) / 100;
    onChange({ reservationGiB: Math.max(MIN_RESERVATION_GIB, next) });
  };

  const realities = capabilities ? limitRealities(draftToNumericPolicy(draft), capabilities) : null;

  return (
    <div className="policy-editor">
      <fieldset className="enforcement-segment">
        <legend>{t("policy.legend")}</legend>
        {ENFORCEMENT_OPTIONS.map((option) => (
          <label key={option.value} className={draft.enforcement === option.value ? "selected" : undefined}>
            <input
              type="radio"
              name={`${idPrefix}-enforcement`}
              value={option.value}
              checked={draft.enforcement === option.value}
              onChange={() => onChange({ enforcement: option.value })}
            />
            {option.label}
            <span className="muted enforcement-hint" title={option.hint}>
              ?
            </span>
          </label>
        ))}
      </fieldset>
      <p className="muted">
        {ENFORCEMENT_OPTIONS.find((o) => o.value === draft.enforcement)?.hint ?? ""}
      </p>

      {realities ? (
        <ul className="capability-reality" aria-label={t("caps.realities.aria")}>
          {realities.map((reality) => (
            <li key={reality.key} className={`limit-${reality.support}`}>
              <span title={reality.reason ?? undefined}>
                {reality.label}: {reality.supportText}
                {reality.requested && reality.support !== "supported" ? t("caps.realities.requestedSuffix") : ""}
              </span>
              {reality.notice ? <div className="workload-notice">{reality.notice}</div> : null}
            </li>
          ))}
        </ul>
      ) : (
        <p className="muted">{t("caps.realities.loading")}</p>
      )}

      <div className="policy-numbers">
        <label>
          {t("policy.reservation")}
          <span className="stepper">
            <button type="button" aria-label={t("policy.reservation.decrease")} onClick={() => stepReservation(-0.25)}>
              −
            </button>
            <input
              type="number"
              min={MIN_RESERVATION_GIB}
              step={0.25}
              value={draft.reservationGiB}
              onChange={(e) => onChange({ reservationGiB: Number(e.target.value) })}
              aria-label={t("policy.reservation.aria")}
            />
            <button type="button" aria-label={t("policy.reservation.increase")} onClick={() => stepReservation(0.25)}>
              +
            </button>
          </span>
          <span className="muted">
            {t("policy.reservation.note", { value: formatGiB(Math.round(draft.reservationGiB * 1024 ** 3)) })}
          </span>
        </label>
        <label>
          cpu_slots
          <input
            type="number"
            min={1}
            step={1}
            value={draft.cpuSlots}
            onChange={(e) => onChange({ cpuSlots: Number(e.target.value) })}
          />
        </label>
      </div>

      <fieldset className="caps-editor">
        <legend>{t(CAPS_SECTION_LABEL)}</legend>
        <p className="muted">{t(CAPS_SECTION_NOTE)}</p>
        <label>
          {t("policy.memoryMax")}
          <input
            type="number"
            min={0.25}
            step={0.25}
            value={draft.memoryMaxGiB ?? ""}
            placeholder={t("policy.unsetPlaceholder")}
            onChange={(e) => onChange({ memoryMaxGiB: e.target.value === "" ? null : Number(e.target.value) })}
          />
        </label>
        <label>
          {t("policy.cpuMax")}
          <input
            type="number"
            min={0.1}
            step={0.5}
            value={draft.cpuMaxCores ?? ""}
            placeholder={t("policy.unsetPlaceholder")}
            onChange={(e) => onChange({ cpuMaxCores: e.target.value === "" ? null : Number(e.target.value) })}
          />
        </label>
        <label>
          {t("policy.pidsMax")}
          <input
            type="number"
            min={1}
            step={1}
            value={draft.pidsMax ?? ""}
            placeholder={t("policy.unsetPlaceholder")}
            onChange={(e) => onChange({ pidsMax: e.target.value === "" ? null : Number(e.target.value) })}
          />
        </label>
      </fieldset>
    </div>
  );
});

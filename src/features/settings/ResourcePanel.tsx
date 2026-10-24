/**
 * 설정 → 자원 관리(08 §5 `resource_guard.*`, §2 `relief.auto_yield`).
 *
 * - 보이는 값은 데몬이 지금 쓰는 정책(스냅샷·`guard.set_policy` 응답)이다. "기본값"은
 *   데몬이 이 호스트의 코어·메모리에서 유도한 값(`guard_policy_default`)이고, 알리지
 *   않는 구 데몬이면 계약의 고정 기본값이다.
 * - 바꾸면 컨트롤러가 정책 전체를 보내고(`guard.set_policy`), 데몬이 받아들인 뒤에만
 *   바꾼 항목을 사용자 변경분(preferences)으로 저장한다 — 데몬 정책은 메모리에만 있어
 *   붙을 때마다 다시 적용된다(SessionController.applyResourcePolicy).
 * - 숫자는 데몬과 같은 범위로 먼저 검사해 그 자리에 사유를 보인다(보내지 않는다).
 *   입력은 로컬 초안이고 blur/Enter에 확정한다(한 글자마다 보내지 않는다).
 */

import { useContext, useEffect, useState } from "react";
import type { GuardPolicy } from "../../generated/GuardPolicy";
import { ControllerContext } from "../../app/controllerContext";
import { useI18n } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import {
  FALLBACK_GUARD_POLICY,
  GUARD_FIELDS,
  bytesToGib,
  gibToBytes,
  guardFieldError,
  msToSeconds,
  secondsToMs,
  type GuardField,
} from "../workloads/resourcePolicy";
import { SettingRow, SettingToggle } from "./controls";
import { itemsInGroup, type SettingsItemDef } from "./schema";

const items = itemsInGroup("resources");
const itemOf = (id: string): SettingsItemDef => items.find((entry) => entry.id === id)!;

type NumberField = Extract<GuardField, "cpu_cores_limit" | "rss_limit_bytes" | "sustain_ms" | "rss_sustain_ms">;

/** 숫자 항목의 표시 단위 변환(GiB·초·코어) — 저장 형식(바이트·밀리초)과 화면 형식 사이. */
const NUMBER_FIELDS: Record<NumberField, {
  item: SettingsItemDef;
  unitKey: string;
  step: number;
  show(policy: GuardPolicy): number | null;
  parse(value: number): GuardPolicy[NumberField] | null;
}> = {
  rss_limit_bytes: {
    item: itemOf("resourceRssLimit"),
    unitKey: "settings.item.resourceRssLimit.unit",
    step: 0.25,
    show: (policy) => bytesToGib(policy.rss_limit_bytes),
    parse: (value) => gibToBytes(value),
  },
  rss_sustain_ms: {
    item: itemOf("resourceRssSustain"),
    unitKey: "settings.resources.unit.seconds",
    step: 1,
    show: (policy) => msToSeconds(policy.rss_sustain_ms),
    parse: (value) => secondsToMs(value),
  },
  cpu_cores_limit: {
    item: itemOf("resourceCpuLimit"),
    unitKey: "settings.item.resourceCpuLimit.unit",
    step: 1,
    show: (policy) => policy.cpu_cores_limit,
    parse: (value) => value,
  },
  sustain_ms: {
    item: itemOf("resourceCpuSustain"),
    unitKey: "settings.resources.unit.seconds",
    step: 1,
    show: (policy) => msToSeconds(policy.sustain_ms),
    parse: (value) => secondsToMs(value),
  },
};

export function ResourcePanel(): JSX.Element {
  const { t } = useI18n();
  const controller = useContext(ControllerContext);
  const policy = useWorkbenchStore((s) => s.guardPolicy);
  const reportedDefaults = useWorkbenchStore((s) => s.guardPolicyDefault);
  const defaults = reportedDefaults ?? FALLBACK_GUARD_POLICY;
  const autoYield = useWorkbenchStore((s) => s.reliefPolicy.auto_yield);
  const yieldSupport = useWorkbenchStore((s) => s.schedulingYield?.support ?? null);
  const yieldReason = useWorkbenchStore((s) => s.schedulingYield?.reason ?? null);
  // 항목별 오류(검증 실패·데몬 거절) — 고치거나 다른 값이 적용되면 지운다.
  const [errors, setErrors] = useState<Partial<Record<GuardField | "auto_yield", string>>>({});
  const [busy, setBusy] = useState(false);
  const ready = policy !== null && controller !== null;

  const setError = (field: GuardField | "auto_yield", message: string | null): void =>
    setErrors((current) => {
      if ((current[field] ?? null) === message) return current;
      const next = { ...current };
      if (message === null) delete next[field];
      else next[field] = message;
      return next;
    });

  const failure = (value: unknown): string =>
    t("settings.resources.error.failed", { message: value instanceof Error ? value.message : String(value) });

  const apply = async (field: GuardField, patch: Partial<GuardPolicy>): Promise<void> => {
    if (!controller) return;
    setBusy(true);
    try {
      await controller.setGuardPolicy(patch);
      setError(field, null);
    } catch (value) {
      setError(field, failure(value));
    } finally {
      setBusy(false);
    }
  };

  const reset = async (fields: readonly GuardField[]): Promise<void> => {
    if (!controller) return;
    setBusy(true);
    try {
      await controller.resetGuardPolicy(fields);
      for (const field of fields) setError(field, null);
    } catch (value) {
      setError(fields[0], failure(value));
    } finally {
      setBusy(false);
    }
  };

  const applyAutoYield = async (value: boolean): Promise<void> => {
    if (!controller) return;
    setBusy(true);
    try {
      await controller.setReliefAutoYield(value);
      setError("auto_yield", null);
    } catch (error) {
      setError("auto_yield", failure(error));
    } finally {
      setBusy(false);
    }
  };

  const changed = (field: GuardField): boolean => policy !== null && policy[field] !== defaults[field];
  const anyChanged = GUARD_FIELDS.some(changed) || !autoYield;
  const yieldSupported = yieldSupport === null || yieldSupport === "supported";

  const toggleRow = (field: "auto_suspend" | "auto_resume", item: SettingsItemDef): JSX.Element => (
    <SettingRow
      item={item}
      changed={ready && changed(field)}
      onReset={() => void reset([field])}
    >
      {(id) => (
        <span className="setting-resource">
          <SettingToggle
            id={id}
            checked={policy?.[field] ?? defaults[field]}
            stateLabel={t((policy?.[field] ?? defaults[field]) ? "settings.on" : "settings.off")}
            onChange={(checked) => {
              if (ready && !busy) void apply(field, { [field]: checked });
            }}
          />
          {errors[field] ? <span className="setting-error" role="alert">{errors[field]}</span> : null}
        </span>
      )}
    </SettingRow>
  );

  const numberRow = (field: NumberField): JSX.Element => {
    const spec = NUMBER_FIELDS[field];
    const shown = policy ? spec.show(policy) : spec.show(defaults);
    return (
      <SettingRow item={spec.item} changed={ready && changed(field)} onReset={() => void reset([field])}>
        {(id) => (
          <span className="setting-resource">
            <PolicyNumber
              id={id}
              value={shown}
              step={spec.step}
              disabled={!ready}
              unit={t(spec.unitKey)}
              ariaLabel={t(spec.item.labelKey)}
              onCommit={(value) => {
                const next = spec.parse(value);
                const problem = next === null
                  ? "settings.resources.error.number"
                  : guardFieldError(field, next);
                if (problem) {
                  setError(field, t(problem));
                  return;
                }
                void apply(field, { [field]: next } as Partial<GuardPolicy>);
              }}
              onInvalid={() => setError(field, t("settings.resources.error.number"))}
            />
            {errors[field] ? <span className="setting-error" role="alert">{errors[field]}</span> : null}
          </span>
        )}
      </SettingRow>
    );
  };

  return (
    <>
      <p className="muted setting-resource-note">{t("settings.resources.note")}</p>
      {policy === null ? (
        <p className="muted" role="status" data-testid="resource-policy-unavailable">
          {t("settings.resources.unavailable")}
        </p>
      ) : null}
      {toggleRow("auto_suspend", itemOf("resourceAutoSuspend"))}
      {numberRow("rss_limit_bytes")}
      {numberRow("rss_sustain_ms")}
      {numberRow("cpu_cores_limit")}
      {numberRow("sustain_ms")}
      {toggleRow("auto_resume", itemOf("resourceAutoResume"))}
      <SettingRow
        item={itemOf("resourceAutoYield")}
        changed={controller !== null && !autoYield}
        onReset={() => void applyAutoYield(true)}
      >
        {(id) => (
          <span className="setting-resource">
            <SettingToggle
              id={id}
              checked={yieldSupported && autoYield}
              stateLabel={t(yieldSupported && autoYield ? "settings.on" : "settings.off")}
              onChange={(checked) => {
                if (controller && yieldSupported && !busy) void applyAutoYield(checked);
              }}
            />
            {!yieldSupported ? (
              <span className="muted">{yieldReason ?? t("settings.resources.yieldUnsupported")}</span>
            ) : null}
            {errors.auto_yield ? <span className="setting-error" role="alert">{errors.auto_yield}</span> : null}
          </span>
        )}
      </SettingRow>
      <div className="setting-resource-actions">
        <button
          type="button"
          disabled={!ready || busy || !anyChanged}
          onClick={() => {
            void reset(GUARD_FIELDS);
            if (!autoYield) void applyAutoYield(true);
          }}
        >
          {t("settings.resources.resetAll")}
        </button>
      </div>
    </>
  );
}

/**
 * 숫자 한 칸 — 로컬 초안을 blur/Enter에 확정한다. 확정이 거절되면(범위 밖) 초안을
 * 그대로 두어 사용자가 고칠 수 있게 하고, 받아들여지면 데몬 값으로 다시 맞춘다.
 */
function PolicyNumber(props: {
  id: string;
  value: number | null;
  step: number;
  unit: string;
  ariaLabel: string;
  disabled: boolean;
  onCommit: (value: number) => void;
  onInvalid: () => void;
}): JSX.Element {
  const [draft, setDraft] = useState(props.value === null ? "" : String(props.value));
  useEffect(() => {
    setDraft(props.value === null ? "" : String(props.value));
  }, [props.value]);
  const commit = (): void => {
    const raw = draft.trim();
    if (raw === "" || raw === String(props.value)) return;
    const parsed = Number(raw);
    if (!Number.isFinite(parsed)) {
      props.onInvalid();
      return;
    }
    props.onCommit(parsed);
  };
  return (
    <span className="setting-number">
      <input
        id={props.id}
        type="number"
        className="setting-number-input"
        step={props.step}
        value={draft}
        disabled={props.disabled}
        aria-label={props.ariaLabel}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={commit}
        onKeyDown={(event) => {
          if (event.key === "Enter") commit();
        }}
      />
      <span className="setting-unit">{props.unit}</span>
    </span>
  );
}

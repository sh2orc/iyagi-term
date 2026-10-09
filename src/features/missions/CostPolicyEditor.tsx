/**
 * 작업 한도 편집기.
 *
 * - PolicyLimitsEditor: 활성 시간 · 자동 시작 수 · 수정 사이클 · 할 일당 시도 ·
 *   병렬 수 · 비용 상한(+미확인 비용 정책). 현재 사용량/한도를 같이 보여 주고
 *   입력은 daemon policy_ceiling(defaults.json)으로 맞춘다.
 * - 저장은 mission.policy.update + mutateWithResync. 사용자가 건드린 필드만
 *   최신 정책 위에 덮어써 다른 곳의 동시 변경을 지우지 않는다.
 * - CostPolicyEditor: 기존 비용 전용 접이식 편집기(호환 export).
 */

import { useId, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import type { Mission } from "../../generated/Mission";
import type { UnknownCostPolicy } from "../../generated/UnknownCostPolicy";
import defaults from "../../../docs/orchestration/defaults.json";
import { useI18n } from "../../i18n";
import { getMissionClient } from "./clientAccess";
import { useMissionStore } from "./store";
import { dollarsToMicros } from "./configuration";
import { microsToDollars, summarizeUsage } from "./costs";
import { MissionErrorNotice, missionError, type MissionUiError } from "./errors";
import { mutateWithResync } from "./mutation";
import { selectLiveRunCount, selectMissionRuns } from "./selectors";
import { formatElapsedTime, newRequestId } from "./viewUtils";
import "./decisions.css";

const CEILING = defaults.policy_ceiling;
const MINUTE_MS = 60_000;
const HOUR_MS = 3_600_000;
const EDITABLE_STATES = ["running", "paused", "draft"];

type TimeUnit = "minutes" | "hours";
type LimitField = "active" | "starts" | "repair" | "attempts" | "parallel" | "cost" | "unknown";

const unitMs = (unit: TimeUnit): number => (unit === "hours" ? HOUR_MS : MINUTE_MS);

/** ms → 입력칸 문자열(해당 단위, 소수는 최대 2자리). */
function msToUnitText(ms: number, unit: TimeUnit): string {
  const value = ms / unitMs(unit);
  return Number.isInteger(value) ? String(value) : String(Math.round(value * 100) / 100);
}

/** 정수 한도를 [min, max]로 맞춘다. 정수가 아니면 null. */
export function clampLimit(text: string, min: number, max: number): { value: number; clamped: boolean } | null {
  const trimmed = text.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const parsed = Number(trimmed);
  const value = Math.min(max, Math.max(min, parsed));
  return { value, clamped: value !== parsed };
}

/** 분/시간 입력을 ms로 바꿔 [1분, ceiling]으로 맞춘다. 숫자가 아니면 null. */
export function clampActiveTime(text: string, unit: TimeUnit): { ms: number; clamped: boolean } | null {
  const trimmed = text.trim();
  if (!/^\d+(\.\d+)?$/.test(trimmed)) return null;
  const raw = Math.round(Number(trimmed) * unitMs(unit));
  const ms = Math.min(CEILING.active_time_limit_ms, Math.max(MINUTE_MS, raw));
  return { ms, clamped: ms !== raw };
}

export function PolicyLimitsEditor(props: { mission: Mission; onClose?: () => void; onSaved?: () => void }): JSX.Element {
  const { t } = useI18n();
  const titleId = useId();
  const stored = useMissionStore((s) => s.missions[props.mission.id]);
  const mission = stored ?? props.mission;
  const initial = useRef(mission.policy).current;
  const liveRuns = useMissionStore((s) => selectLiveRunCount(s, mission.id));
  const maxRepairCycle = useMissionStore((s) => {
    let max = 0;
    for (const task of Object.values(s.tasks)) if (task.mission_id === mission.id) max = Math.max(max, task.repair_cycle);
    return max;
  });
  const maxAttempt = useMissionStore((s) => {
    let max = 0;
    for (const task of Object.values(s.tasks)) if (task.mission_id === mission.id) max = Math.max(max, task.attempt_count);
    return max;
  });
  const runs = useMissionStore(useShallow((s) => selectMissionRuns(s, mission.id)));
  const usage = summarizeUsage(runs);

  const initialMs = Number(initial.active_time_limit_ms);
  const [unit, setUnit] = useState<TimeUnit>(() => (initialMs % HOUR_MS === 0 ? "hours" : "minutes"));
  const [activeText, setActiveText] = useState(() => msToUnitText(initialMs, initialMs % HOUR_MS === 0 ? "hours" : "minutes"));
  const [startsText, setStartsText] = useState(String(initial.max_automatic_starts));
  const [repairText, setRepairText] = useState(String(initial.max_repair_cycles));
  const [attemptsText, setAttemptsText] = useState(String(initial.max_attempts_per_task));
  const [parallelText, setParallelText] = useState(String(initial.max_parallel_runs));
  const [costText, setCostText] = useState(initial.max_cost_usd_micros === null ? "" : microsToDollars(BigInt(initial.max_cost_usd_micros)));
  const [unknown, setUnknown] = useState<UnknownCostPolicy>(initial.unknown_cost);
  const [touched, setTouched] = useState<Partial<Record<LimitField, boolean>>>({});
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState(false);
  const [clamped, setClamped] = useState(false);
  const [validation, setValidation] = useState<string | null>(null);
  const [error, setError] = useState<MissionUiError | null>(null);
  const editable = EDITABLE_STATES.includes(mission.state);
  const dirty = Object.values(touched).some(Boolean);

  const touch = (field: LimitField) => {
    setTouched((current) => (current[field] ? current : { ...current, [field]: true }));
    setSaved(false);
  };

  const changeUnit = (next: TimeUnit) => {
    const parsed = clampActiveTime(activeText, unit);
    if (parsed) setActiveText(msToUnitText(parsed.ms, next));
    setUnit(next);
  };

  const save = async () => {
    setValidation(null);
    setError(null);
    setSaved(false);
    setClamped(false);
    const active = clampActiveTime(activeText, unit);
    const starts = clampLimit(startsText, 1, CEILING.max_automatic_starts);
    const repair = clampLimit(repairText, 0, CEILING.max_repair_cycles);
    const attempts = clampLimit(attemptsText, 1, CEILING.max_attempts_per_task);
    const parallel = clampLimit(parallelText, 1, CEILING.max_parallel_runs);
    if (!active || !starts || !repair || !attempts || !parallel) {
      setValidation(t("missions.limits.invalidNumber"));
      return;
    }
    let cost: string | null;
    try {
      cost = dollarsToMicros(costText);
    } catch {
      setValidation(t("missions.create.costInvalid"));
      return;
    }
    if (active.clamped || starts.clamped || repair.clamped || attempts.clamped || parallel.clamped) {
      setActiveText(msToUnitText(active.ms, unit));
      setStartsText(String(starts.value));
      setRepairText(String(repair.value));
      setAttemptsText(String(attempts.value));
      setParallelText(String(parallel.value));
      setClamped(true);
    }
    const client = getMissionClient();
    if (!client) {
      setError(missionError(t, new Error(t("missions.sync.noClient"))));
      return;
    }
    setBusy(true);
    try {
      await mutateWithResync(mission.id, (latest) =>
        client.missionPolicyUpdate({
          request_id: newRequestId(),
          mission_id: latest.id,
          expected_revision: latest.revision,
          policy: {
            ...latest.policy,
            ...(touched.active ? { active_time_limit_ms: String(active.ms) } : {}),
            ...(touched.starts ? { max_automatic_starts: starts.value } : {}),
            ...(touched.repair ? { max_repair_cycles: repair.value } : {}),
            ...(touched.attempts ? { max_attempts_per_task: attempts.value } : {}),
            ...(touched.parallel ? { max_parallel_runs: parallel.value } : {}),
            ...(touched.cost ? { max_cost_usd_micros: cost } : {}),
            ...(touched.unknown ? { unknown_cost: unknown } : {}),
          },
          role_bindings: latest.role_bindings,
        }),
      );
      setSaved(true);
      setTouched({});
      props.onSaved?.();
    } catch (cause) {
      setError(missionError(t, cause));
    } finally {
      setBusy(false);
    }
  };

  const disabled = busy || !editable;
  const policy = mission.policy;
  const countRow = (
    field: LimitField,
    label: string,
    text: string,
    setText: (value: string) => void,
    min: number,
    max: number,
    usageText: string,
  ) => (
    <div className="mission-limits-row" data-testid={`limits-row-${field}`}>
      <label>
        {label}
        <input
          type="number"
          inputMode="numeric"
          min={min}
          max={max}
          step={1}
          value={text}
          disabled={disabled}
          data-testid={`limits-${field}`}
          onChange={(event) => {
            setText(event.target.value);
            touch(field);
          }}
          onBlur={() => {
            const next = clampLimit(text, min, max);
            if (next?.clamped) {
              setText(String(next.value));
              setClamped(true);
            }
          }}
        />
      </label>
      <span className="muted">{t("missions.limits.max", { max })}</span>
      <span className="mission-limits-usage">{usageText}</span>
    </div>
  );

  return (
    <section className="mission-limits-editor" aria-labelledby={titleId} data-testid="policy-limits-editor">
      <h4 id={titleId}>{t("missions.limits.title")}</h4>
      <div className="mission-limits-row" data-testid="limits-row-active">
        <label>
          {t("missions.limits.activeTime")}
          <input
            type="number"
            inputMode="decimal"
            min={0}
            step="any"
            value={activeText}
            disabled={disabled}
            data-testid="limits-active"
            onChange={(event) => {
              setActiveText(event.target.value);
              touch("active");
            }}
            onBlur={() => {
              const next = clampActiveTime(activeText, unit);
              if (next?.clamped) {
                setActiveText(msToUnitText(next.ms, unit));
                setClamped(true);
              }
            }}
          />
          <select
            value={unit}
            disabled={disabled}
            aria-label={t("missions.limits.activeTime")}
            data-testid="limits-active-unit"
            onChange={(event) => changeUnit(event.target.value as TimeUnit)}
          >
            <option value="minutes">{t("missions.limits.unit.minutes")}</option>
            <option value="hours">{t("missions.limits.unit.hours")}</option>
          </select>
        </label>
        <span className="muted">{t("missions.limits.max", { max: formatElapsedTime(String(CEILING.active_time_limit_ms)) })}</span>
        <span className="mission-limits-usage">
          {t("missions.limits.usage", { used: formatElapsedTime(mission.active_time_ms), limit: formatElapsedTime(policy.active_time_limit_ms) })}
        </span>
      </div>
      {countRow("starts", t("missions.limits.automaticStarts"), startsText, setStartsText, 1, CEILING.max_automatic_starts,
        t("missions.limits.usage", { used: mission.automatic_start_count, limit: policy.max_automatic_starts }))}
      {countRow("repair", t("missions.limits.repairCycles"), repairText, setRepairText, 0, CEILING.max_repair_cycles,
        t("missions.limits.usage", { used: maxRepairCycle, limit: policy.max_repair_cycles }))}
      {countRow("attempts", t("missions.limits.attempts"), attemptsText, setAttemptsText, 1, CEILING.max_attempts_per_task,
        t("missions.limits.usage", { used: maxAttempt, limit: policy.max_attempts_per_task }))}
      {countRow("parallel", t("missions.limits.parallel"), parallelText, setParallelText, 1, CEILING.max_parallel_runs,
        t("missions.limits.usageParallel", { used: liveRuns, limit: policy.max_parallel_runs }))}
      <div className="mission-limits-row" data-testid="limits-row-cost">
        <label>
          {t("missions.create.costCap")}
          <input
            className="mission-limits-cost"
            inputMode="decimal"
            value={costText}
            disabled={disabled}
            data-testid="limits-cost"
            onChange={(event) => {
              setCostText(event.target.value);
              touch("cost");
            }}
          />
        </label>
        <label>
          {t("missions.cost.unknownPolicy")}
          <select
            value={unknown}
            disabled={disabled}
            data-testid="limits-unknown-cost"
            onChange={(event) => {
              setUnknown(event.target.value as UnknownCostPolicy);
              touch("unknown");
            }}
          >
            <option value="block">{t("missions.cost.blockUnknown")}</option>
            <option value="allow_with_notice">{t("missions.cost.allowUnknown")}</option>
          </select>
        </label>
        <span className="mission-limits-usage">
          {t("missions.limits.usageCost", { observed: `$${microsToDollars(usage.observed)}`, estimated: `$${microsToDollars(usage.estimated)}` })}
          {" · "}
          {policy.max_cost_usd_micros === null
            ? t("missions.create.costCapNone")
            : t("missions.create.costCapSummary", { cap: microsToDollars(BigInt(policy.max_cost_usd_micros)) })}
        </span>
      </div>
      <details className="mission-decision-details">
        <summary>{t("missions.common.details")}</summary>
        <div>
          <p>{t("missions.time.definition")}</p>
          <p>{t("missions.cost.admissionNote")}</p>
        </div>
      </details>
      {!editable ? <p className="muted">{t("missions.limits.notEditable")}</p> : null}
      <div className="mission-limits-actions">
        <button type="button" className="primary" disabled={disabled || !dirty} onClick={() => void save()} data-testid="limits-save">
          {t("missions.limits.save")}
        </button>
        {props.onClose ? (
          <button type="button" onClick={props.onClose} data-testid="limits-close">
            {t("missions.common.close")}
          </button>
        ) : null}
      </div>
      {clamped ? <p className="muted" data-testid="limits-clamped">{t("missions.limits.clamped")}</p> : null}
      {validation ? <p className="mission-area-error" role="alert">{validation}</p> : null}
      {saved ? <p role="status" data-testid="limits-saved">{t("missions.limits.saved")}</p> : null}
      {error ? <MissionErrorNotice error={error} onRetry={() => void save()} /> : null}
    </section>
  );
}

/** 비용 정책만 바꾸는 접이식 편집기(기존 호출부 호환). */
export function CostPolicyEditor({ mission }: { mission: Mission }): JSX.Element {
  const { t } = useI18n();
  const [cap, setCap] = useState(mission.policy.max_cost_usd_micros === null ? "" : microsToDollars(BigInt(mission.policy.max_cost_usd_micros)));
  const [unknown, setUnknown] = useState(mission.policy.unknown_cost);
  const [busy, setBusy] = useState(false);
  const [validation, setValidation] = useState<string | null>(null);
  const [error, setError] = useState<MissionUiError | null>(null);
  const [saved, setSaved] = useState(false);
  const save = async () => {
    setValidation(null);
    setError(null);
    setSaved(false);
    let cost: string | null;
    try {
      cost = dollarsToMicros(cap);
    } catch {
      setValidation(t("missions.create.costInvalid"));
      return;
    }
    const client = getMissionClient();
    if (!client) {
      setError(missionError(t, new Error(t("missions.sync.noClient"))));
      return;
    }
    setBusy(true);
    try {
      await mutateWithResync(mission.id, (latest) =>
        client.missionPolicyUpdate({
          request_id: newRequestId(),
          mission_id: latest.id,
          expected_revision: latest.revision,
          policy: { ...latest.policy, max_cost_usd_micros: cost, unknown_cost: unknown },
          role_bindings: latest.role_bindings,
        }),
      );
      setSaved(true);
    } catch (cause) {
      setError(missionError(t, cause));
    } finally {
      setBusy(false);
    }
  };
  return (
    <details className="mission-cost-policy" data-testid="cost-policy-editor">
      <summary>{t("missions.cost.editPolicy")}</summary>
      <label>{t("missions.create.costCap")}<input inputMode="decimal" value={cap} disabled={busy} onChange={(e) => setCap(e.target.value)} /></label>
      <label>{t("missions.cost.unknownPolicy")}<select value={unknown} disabled={busy} onChange={(e) => setUnknown(e.target.value as typeof unknown)}>
        <option value="block">{t("missions.cost.blockUnknown")}</option><option value="allow_with_notice">{t("missions.cost.allowUnknown")}</option>
      </select></label>
      <p className="muted">{t("missions.cost.admissionNote")}</p>
      <button type="button" disabled={busy || !EDITABLE_STATES.includes(mission.state)} onClick={() => void save()}>{t("missions.cost.savePolicy")}</button>
      {saved ? <p role="status">{t("missions.cost.policySaved")}</p> : null}
      {validation ? <p className="mission-area-error" role="alert">{validation}</p> : null}
      {error ? <MissionErrorNotice error={error} onRetry={() => void save()} /> : null}
    </details>
  );
}

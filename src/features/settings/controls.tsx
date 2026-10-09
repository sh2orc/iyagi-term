/**
 * 설정 행의 표현 부품 — 라벨/설명/컨트롤/되돌리기 배치를 한곳에 모은다.
 *
 * 모든 행이 같은 골격을 쓰므로 새 설정을 추가할 때 마크업을 다시 짤 일이
 * 없고, data-setting-id 덕에 검색 결과에서 그 행으로 바로 이동한다.
 * 저장 버튼은 없다 — 값은 바꾸는 즉시 적용·저장된다.
 */

import { useEffect, useId, useState, type ReactNode } from "react";
import { useI18n } from "../../i18n";
import type { SettingsItemDef } from "./schema";

export interface SettingRowProps {
  item: SettingsItemDef;
  /** 컨트롤을 라벨과 연결할 id(없으면 자동 생성). */
  controlId?: string;
  /** 기본값과 다른가 — 참이면 "기본값으로" 버튼이 나온다. */
  changed?: boolean;
  onReset?: () => void;
  /** 폼처럼 넓은 컨트롤은 라벨 아래 전체 폭으로 놓는다. */
  layout?: "inline" | "stacked";
  children: (controlId: string) => ReactNode;
}

export function SettingRow(props: SettingRowProps): JSX.Element {
  const { t } = useI18n();
  const generated = useId();
  const controlId = props.controlId ?? generated;
  const label = t(props.item.labelKey);
  return (
    <div
      className={`setting-row setting-row-${props.layout ?? "inline"}`}
      data-setting-id={props.item.id}
    >
      <div className="setting-text">
        <label className="setting-label" htmlFor={controlId}>
          {label}
        </label>
        <p className="setting-description">{t(props.item.descriptionKey)}</p>
      </div>
      <div className="setting-control">
        {props.children(controlId)}
        {props.changed && props.onReset ? (
          <button
            type="button"
            className="setting-reset"
            aria-label={t("settings.resetItemAria").replace("{label}", label)}
            onClick={props.onReset}
          >
            {t("settings.resetItem")}
          </button>
        ) : null}
      </div>
    </div>
  );
}

export interface SettingOption<T extends string> {
  value: T;
  label: string;
}

export function SettingSelect<T extends string>(props: {
  id: string;
  value: T;
  options: readonly SettingOption<T>[];
  onChange: (value: T) => void;
  disabled?: boolean;
}): JSX.Element {
  return (
    <select
      id={props.id}
      className="setting-select"
      value={props.value}
      disabled={props.disabled}
      onChange={(event) => props.onChange(event.target.value as T)}
    >
      {props.options.map((option) => (
        <option key={option.value} value={option.value}>
          {option.label}
        </option>
      ))}
    </select>
  );
}

export function SettingToggle(props: {
  id: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
  /** 켬/끔 옆에 붙는 짧은 상태 문구. */
  stateLabel: string;
}): JSX.Element {
  return (
    <span className="setting-toggle">
      <input
        id={props.id}
        type="checkbox"
        checked={props.checked}
        onChange={(event) => props.onChange(event.target.checked)}
      />
      <span className="setting-toggle-state">{props.stateLabel}</span>
    </span>
  );
}

/** 슬라이더 + 숫자 입력 한 쌍 — 대충 맞추기도, 정확히 넣기도 되게. */
export function SettingNumber(props: {
  id: string;
  value: number;
  min: number;
  max: number;
  step?: number;
  unit?: string;
  ariaLabel: string;
  onChange: (value: number) => void;
}): JSX.Element {
  const commit = (raw: string): void => {
    const parsed = Number(raw);
    if (Number.isFinite(parsed)) props.onChange(parsed);
  };
  // 숫자 입력은 로컬 초안 — 한 글자마다 clamp되면 "12"가 9→92→24로 튄다.
  // blur/Enter에 확정하고 슬라이더는 즉시 반영한다.
  const [draft, setDraft] = useState(String(props.value));
  useEffect(() => {
    setDraft(String(props.value));
  }, [props.value]);
  const commitDraft = (): void => {
    if (draft.trim() !== "") commit(draft);
    // clamp 결과가 현재 값과 같으면 effect가 돌지 않으므로 직접 되돌린다.
    setDraft(String(props.value));
  };
  return (
    <span className="setting-number">
      <input
        id={props.id}
        type="range"
        min={props.min}
        max={props.max}
        step={props.step ?? 1}
        value={props.value}
        aria-label={props.ariaLabel}
        onChange={(event) => commit(event.target.value)}
      />
      <input
        type="number"
        className="setting-number-input"
        min={props.min}
        max={props.max}
        step={props.step ?? 1}
        value={draft}
        aria-label={props.ariaLabel}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={commitDraft}
        onKeyDown={(event) => {
          if (event.key === "Enter") commitDraft();
        }}
      />
      {props.unit ? <span className="setting-unit">{props.unit}</span> : null}
    </span>
  );
}

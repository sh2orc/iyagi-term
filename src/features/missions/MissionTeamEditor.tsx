/**
 * 팀 편집기(05-ui §5): 만든 뒤에도 역할별 모델 연결을 바꾼다.
 *
 * - 대상은 작업 정책이 허용한 역할(`policy.allowed_roles`)이다. lead·builder와,
 *   독립 리뷰가 켜져 있으면 reviewer는 뺄 수 없다(데몬 policy.update와 같은 규칙).
 *   integrator처럼 선택 역할은 `맡기지 않음`으로 뺀다.
 * - 목록은 활성 연결 전부다. 역할이 맡는 할 일 종류를 지원하지 않는 연결은 비활성 +
 *   이유를 보인다 — 임의 모델 대체는 하지 않는다(05 §3).
 * - 저장은 mission.policy.update다. 고른 연결을 `allowed_binding_ids`에 합집합으로
 *   더하기만 하고 빼지 않는다(실행 중인 Run이 쓰는 연결을 지우면 데몬이 거절한다).
 *   더 이상 활성이 아닌 fallback은 저장할 때 떨군다 — 데몬은 primary·fallback 모두
 *   활성 연결을 요구한다.
 * - 이미 보낸 할 일은 그대로 두고 다음 배정부터 새 연결을 쓴다.
 * - 저장은 mutateWithResync, 오류는 MissionErrorNotice.
 */

import { useEffect, useId, useState } from "react";
import type { Binding } from "../../generated/Binding";
import type { Mission } from "../../generated/Mission";
import type { Role } from "../../generated/Role";
import type { RoleBinding } from "../../generated/RoleBinding";
import { useI18n } from "../../i18n";
import { bindingSupportsRole } from "./bindingSupport";
import { getMissionClient } from "./clientAccess";
import { MissionErrorNotice, missionError, type MissionUiError } from "./errors";
import { roleLabel } from "./labels";
import { mutateWithResync } from "./mutation";
import { newRequestId } from "./viewUtils";
import "./decisions.css";

/**
 * 화면에 두는 역할 순서. 팀 템플릿이 쓰는 네 역할을 앞에 두고 나머지 역할을 뒤에
 * 둔다 — 정책이 허용했거나 이미 배정된 역할만 남기므로, 목록에서 빠진 역할이
 * 저장할 때 조용히 사라지는 일은 없다.
 */
const ROLE_ORDER: readonly Role[] = [
  "lead", "builder", "reviewer", "integrator",
  "researcher", "architect", "test_author", "specialist", "diagnostician", "documenter",
];
/** 데몬 policy.update가 받는 상태. */
const EDITABLE_STATES = ["draft", "running", "paused"];
/** 역할을 비울 수 없을 때 select가 쓰는 값. */
const NONE = "";

export interface MissionTeamEditorProps {
  mission: Mission;
  onClose?: () => void;
  onSaved?: () => void;
}

/** 역할 → 현재 primary 연결 ID(없으면 null). */
function currentPicks(mission: Mission): Record<string, string | null> {
  const picks: Record<string, string | null> = {};
  for (const role of ROLE_ORDER) {
    picks[role] = mission.role_bindings.find((binding) => binding.role === role)?.primary_binding_id ?? null;
  }
  return picks;
}

/** lead·builder는 언제나, reviewer는 독립 리뷰가 켜져 있을 때 필수다(데몬 start 검사와 같다). */
export function requiredTeamRole(mission: Mission, role: Role): boolean {
  if (role === "lead" || role === "builder") return true;
  return role === "reviewer" && mission.policy.require_independent_review;
}

export function MissionTeamEditor(props: MissionTeamEditorProps): JSX.Element {
  const { mission } = props;
  const { t } = useI18n();
  const titleId = useId();
  const [bindings, setBindings] = useState<Binding[] | null>(null);
  const [picks, setPicks] = useState<Record<string, string | null>>(() => currentPicks(mission));
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<MissionUiError | null>(null);
  const editable = EDITABLE_STATES.includes(mission.state);
  // 허용 역할 + 이미 배정된 역할(정책과 어긋난 기록도 저장에서 잃지 않는다).
  const roles = ROLE_ORDER.filter(
    (role) => mission.policy.allowed_roles.includes(role) || mission.role_bindings.some((binding) => binding.role === role),
  );

  useEffect(() => {
    let alive = true;
    const client = getMissionClient();
    if (!client) {
      setBindings([]);
      return;
    }
    client.bindingList().then(
      (result) => {
        if (alive) setBindings(result.bindings.filter((binding) => binding.enabled));
      },
      (cause) => {
        if (alive) {
          setBindings([]);
          setError(missionError(t, cause));
        }
      },
    );
    return () => {
      alive = false;
    };
    // 연결 목록은 편집기를 여는 동안 한 번만 읽는다(저장 뒤 재동기화는 mission이 맡는다).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mission.id]);

  const initial = currentPicks(mission);
  const dirty = roles.some((role) => picks[role] !== initial[role]);
  const missingRequired = roles.filter((role) => requiredTeamRole(mission, role) && !picks[role]);
  const disabled = busy || !editable || bindings === null;

  const save = async () => {
    const client = getMissionClient();
    if (!client) {
      setError(missionError(t, new Error(t("missions.sync.noClient"))));
      return;
    }
    const enabled = new Set((bindings ?? []).map((binding) => binding.id));
    const chosen: RoleBinding[] = [];
    for (const role of roles) {
      const primary = picks[role];
      if (!primary) continue;
      const previous = mission.role_bindings.find((binding) => binding.role === role);
      chosen.push({
        role,
        primary_binding_id: primary,
        // 사라졌거나 꺼진 fallback은 떨군다 — 데몬은 연결마다 활성을 요구한다.
        fallback_binding_ids: (previous?.fallback_binding_ids ?? []).filter(
          (id) => id !== primary && enabled.has(id),
        ),
      });
    }
    setBusy(true);
    setError(null);
    setSaved(false);
    try {
      await mutateWithResync(mission.id, (latest) => {
        // 허용 목록은 더하기만 한다: 실행 중인 Run이 쓰는 연결을 빼면 데몬이 거절한다.
        const allowed = new Set(latest.policy.allowed_binding_ids);
        for (const binding of chosen) {
          allowed.add(binding.primary_binding_id);
          for (const id of binding.fallback_binding_ids) allowed.add(id);
        }
        return client.missionPolicyUpdate({
          request_id: newRequestId(),
          mission_id: latest.id,
          expected_revision: latest.revision,
          policy: { ...latest.policy, allowed_binding_ids: [...allowed] },
          role_bindings: chosen,
        });
      });
      setSaved(true);
      props.onSaved?.();
    } catch (cause) {
      setError(missionError(t, cause));
    } finally {
      setBusy(false);
    }
  };

  const roleRow = (role: Role) => {
    const picked = picks[role];
    const known = (bindings ?? []).some((binding) => binding.id === picked);
    return (
      <div className="mission-team-row" key={role} data-testid={`team-row-${role}`}>
        <label>
          {roleLabel(t, role)}
          <select
            value={picked ?? NONE}
            disabled={disabled}
            data-testid={`team-pick-${role}`}
            onChange={(event) => {
              const value = event.target.value;
              setSaved(false);
              setPicks((previous) => ({ ...previous, [role]: value === NONE ? null : value }));
            }}
          >
            {requiredTeamRole(mission, role) ? (
              // 필수 역할은 비울 수 없다. 아직 비어 있으면 첫 연결이 고른 것처럼
              // 보이지 않도록 고를 수 없는 자리를 둔다.
              picked ? null : <option value={NONE} disabled>{t("missions.team.choose")}</option>
            ) : (
              <option value={NONE}>{t("missions.team.unassigned")}</option>
            )}
            {picked && !known ? (
              <option value={picked}>{t("missions.team.missingBinding")}</option>
            ) : null}
            {(bindings ?? []).map((binding) => {
              const supported = bindingSupportsRole(binding, role);
              const name = binding.label || binding.model_id;
              return (
                <option key={binding.id} value={binding.id} disabled={!supported}>
                  {supported
                    ? binding.label
                      ? `${binding.label} · ${binding.model_id}`
                      : binding.model_id
                    : t("missions.team.unsupportedOption", { model: name })}
                </option>
              );
            })}
          </select>
        </label>
        {requiredTeamRole(mission, role) ? (
          <span className="muted">{t("missions.team.required")}</span>
        ) : (
          <span className="muted">{t("missions.team.optional")}</span>
        )}
      </div>
    );
  };

  return (
    <section className="mission-team-editor" aria-labelledby={titleId} data-testid="mission-team-editor">
      <h4 id={titleId}>{t("missions.team.title")}</h4>
      <p className="muted">{t("missions.team.applies")}</p>
      {bindings === null ? <p className="muted">…</p> : roles.map(roleRow)}
      {bindings !== null && bindings.length === 0 ? (
        <p className="muted" data-testid="team-no-bindings">{t("missions.detail.noModel")}</p>
      ) : null}
      {!editable ? <p className="muted" data-testid="team-not-editable">{t("missions.team.notEditable")}</p> : null}
      {missingRequired.length > 0 ? (
        <p className="mission-area-error" role="alert" data-testid="team-missing-required">
          {t("missions.team.missingRequired", { roles: missingRequired.map((role) => roleLabel(t, role)).join(", ") })}
        </p>
      ) : null}
      <div className="mission-limits-actions">
        <button
          type="button"
          className="primary"
          disabled={disabled || !dirty || missingRequired.length > 0}
          onClick={() => void save()}
          data-testid="team-save"
        >
          {t("missions.team.save")}
        </button>
        {props.onClose ? (
          <button type="button" onClick={props.onClose} data-testid="team-close">
            {t("missions.common.close")}
          </button>
        ) : null}
      </div>
      {saved ? <p role="status" data-testid="team-saved">{t("missions.team.saved")}</p> : null}
      {error ? (
        // 목록을 읽지 못해 생긴 오류라면 저장 재시도를 권하지 않는다.
        <MissionErrorNotice error={error} onRetry={bindings && bindings.length > 0 ? () => void save() : undefined} />
      ) : null}
    </section>
  );
}

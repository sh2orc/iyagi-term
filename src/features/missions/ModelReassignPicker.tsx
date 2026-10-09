/**
 * 모델 재배정 선택기 — RunDetail과 DecisionPanel(실패 결정)이 같이 쓴다.
 *
 * - 목록: 활성 + 작업 정책이 허용한 모델 연결만. 이 할 일 종류를 지원하지 않는
 *   연결은 비활성 + 이유(툴팁). 동의는 연결당 한 번이라 버전이 달라졌다는 이유로
 *   막지 않는다(11 §3.4 — 버전이 바뀌면 설정에서 `지금 확인`을 다시 누른다).
 * - 고르면 확인 없이 바로 요청한다. action=retry는 모델 변경과 재시도 승인을
 *   한 CAS로, action=reassign은 배정만 저장한다(시작은 예약 검사가 한다).
 * - 선택기를 연 순간의 할 일 전제(상태·배정·실행·시도 수)를 기억해 두고, 전송과
 *   재동기화 뒤 재전송 모두 그 전제가 그대로일 때만 보낸다(아니면 StaleActionError).
 * - 전송은 mutateWithResync, 오류는 MissionErrorNotice.
 */

import { useEffect, useRef, useState } from "react";
import type { Binding } from "../../generated/Binding";
import type { Mission } from "../../generated/Mission";
import type { Task } from "../../generated/Task";
import { useI18n } from "../../i18n";
import { bindingSupportsTask } from "./bindingSupport";
import { getMissionClient } from "./clientAccess";
import { MissionErrorNotice, missionError, type MissionUiError } from "./errors";
import { mutateWithResync } from "./mutation";
import { selectTaskActionGuard, taskActionGuardHolds, type TaskActionGuard } from "./selectors";
import { useMissionStore } from "./store";
import { newRequestId } from "./viewUtils";

export type ReassignAction = "retry" | "reassign";

export interface ModelReassignPickerProps {
  mission: Mission;
  task: Task;
  action: ReassignAction;
  onClose: () => void;
  onDone?: () => void;
}

export function ModelReassignPicker(props: ModelReassignPickerProps): JSX.Element {
  const { t } = useI18n();
  const [bindings, setBindings] = useState<Binding[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<MissionUiError | null>(null);
  const allowed = props.mission.policy.allowed_binding_ids;
  // 선택기를 연 순간(할 일이 바뀌면 그 순간)의 전제 — 사용자는 이 상태를 보고 모델을 바꾸기로 했다.
  const guardRef = useRef<TaskActionGuard | null>(null);
  if (guardRef.current === null || guardRef.current.taskId !== props.task.id) {
    guardRef.current = selectTaskActionGuard(useMissionStore.getState(), props.task);
  }

  useEffect(() => {
    let alive = true;
    setBindings(null);
    const client = getMissionClient();
    if (!client) {
      setBindings([]);
      return;
    }
    client.bindingList().then(
      (result) => {
        if (alive) setBindings(result.bindings.filter((binding) => binding.enabled && allowed.includes(binding.id)));
      },
      () => {
        if (alive) setBindings([]);
      },
    );
    return () => {
      alive = false;
    };
    // allowed_binding_ids는 mission prop과 함께 바뀐다 — 목록 재조회는 mission/task 단위로 충분하다.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.mission.id, props.task.id]);

  const pick = async (binding: Binding) => {
    const client = getMissionClient();
    if (!client) {
      setError(missionError(t, new Error(t("missions.sync.noClient"))));
      return;
    }
    const guard = guardRef.current ?? selectTaskActionGuard(useMissionStore.getState(), props.task);
    setBusy(true);
    setError(null);
    try {
      await mutateWithResync(props.mission.id, (latest) => {
        // 할 일이 사라졌거나 연 순간과 상태·배정·실행·시도 수가 달라졌으면 요청하지 않는다.
        if (!taskActionGuardHolds(useMissionStore.getState(), guard)) return null;
        return client.missionTaskControl({
          request_id: newRequestId(),
          mission_id: latest.id,
          expected_revision: latest.revision,
          task_id: props.task.id,
          action: props.action,
          binding_id: binding.id,
        });
      });
      props.onDone?.();
      props.onClose();
    } catch (cause) {
      setError(missionError(t, cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mission-inline-confirm mission-reassign-picker" data-testid="reassign-picker">
      <p>{t("missions.detail.reassignPick")}</p>
      {bindings === null ? (
        <p className="muted">…</p>
      ) : bindings.length === 0 ? (
        <p className="muted">{t("missions.detail.noModel")}</p>
      ) : (
        bindings.map((binding) => {
          const supported = bindingSupportsTask(binding, props.task.kind);
          return (
            <button
              key={binding.id}
              type="button"
              disabled={busy || !supported}
              title={supported
                ? t(props.action === "retry" ? "missions.detail.reassignRetryTitle" : "missions.detail.reassignOnlyTitle", { model: binding.model_id })
                : t("missions.compatibility.unsupported")}
              onClick={() => void pick(binding)}
            >
              {binding.label} · {binding.model_id}
            </button>
          );
        })
      )}
      <button type="button" onClick={props.onClose}>
        {t("missions.common.cancel")}
      </button>
      {error ? <MissionErrorNotice error={error} /> : null}
    </div>
  );
}

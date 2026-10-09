/**
 * 멈춘 실행 사용자 확인 정리(계약 C `mission.run.attest_exited`).
 *
 * 종료 증거가 없어 실행 자리를 계속 잡는 불확실(unknown/interrupted) 실행에 한 줄 안내 +
 * "프로세스 종료를 직접 확인했습니다"를 둔다. 누르면 확인 방법(알려진 PID·프로세스 그룹, macOS/Linux `ps -p`·
 * 활동 모니터, Windows 작업 관리자)과 체크박스, "제공자 결과와 외부 영향은 확인되지 않은
 * 상태로 남습니다" 고지를 펼친다. 체크해야 전송할 수 있다.
 *
 * - 전송은 mutateWithResync(최신 revision, 충돌 시 1회 재동기화). 최신 store에서 실행이
 *   이미 정리됐으면 보내지 않는다(StaleActionError).
 * - 거절(`attestation_not_applicable` 등)은 missionError + MissionErrorNotice.
 * - 사용자 확인으로 정리된 실행은 RunAttestedNotice/RunAttestedLabel이 "직접 확인으로
 *   정리됨 · 외부 영향 미확인"으로 구분한다. 낭독 영역은 만들지 않는다.
 */

import { useEffect, useState } from "react";
import type { Mission } from "../../generated/Mission";
import type { Run } from "../../generated/Run";
import { useI18n } from "../../i18n";
import { getMissionClient } from "./clientAccess";
import { MissionErrorNotice, missionError, withSupportedActions, type MissionUiError } from "./errors";
import { mutateWithResync } from "./mutation";
import { PROCESS_ABSENT_ATTESTATION, execHasRecoverableGroup, runAwaitsExitAttestation, runUserAttested } from "./runReconciliation";
import { StatusNotice } from "./StatusNotice";
import { useMissionStore } from "./store";
import { newRequestId } from "./viewUtils";
import "./integration.css";

export function RunExitAttestation(props: { mission: Mission; run: Run | null }): JSX.Element | null {
  const { t } = useI18n();
  const run = props.run;
  const runId = run?.id ?? null;
  const exec = useMissionStore((s) => (run?.exec_id ? s.execs[run.exec_id] ?? null : null));
  const [open, setOpen] = useState(false);
  const [checked, setChecked] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<MissionUiError | null>(null);

  useEffect(() => {
    setOpen(false);
    setChecked(false);
    setError(null);
  }, [runId]);

  if (!run || !runAwaitsExitAttestation(run)) return null;

  // 같은 실행의 기록만 쓴다(다른 실행의 exec를 가리키면 PID를 보이지 않는다).
  const ownExec = exec !== null && exec.run_id === run.id && exec.mission_id === run.mission_id ? exec : null;
  const pid = ownExec?.identity?.pid ?? null;
  const group = ownExec?.group_reference ?? null;
  const titleId = `run-attest-title-${run.id}`;

  // 데몬이 프로세스 그룹을 되찾아 감시할 수 있는 실행은 직접 확인 정리 대상이 아니다(데몬이 거절).
  if (execHasRecoverableGroup(ownExec)) {
    return (
      <div className="mission-attest" data-testid="run-attest">
        <StatusNotice
          testId="run-attest-supervised"
          line={t("missions.attest.supervisedLine")}
          details={<p>{t("missions.attest.supervisedDetail")}</p>}
        />
      </div>
    );
  }

  const submit = async () => {
    if (!checked || busy) return;
    const client = getMissionClient();
    if (!client) {
      setError(missionError(t, new Error(t("missions.sync.noClient"))));
      return;
    }
    const targetRunId = run.id;
    setBusy(true);
    setError(null);
    try {
      await mutateWithResync(props.mission.id, (latest) => {
        const state = useMissionStore.getState();
        const current = state.runs[targetRunId];
        if (!current || current.mission_id !== latest.id || !runAwaitsExitAttestation(current)) return null;
        const latestExec = current.exec_id ? state.execs[current.exec_id] ?? null : null;
        if (latestExec && latestExec.run_id === current.id && execHasRecoverableGroup(latestExec)) return null;
        return client.missionRunAttestExited({
          request_id: newRequestId(),
          mission_id: latest.id,
          expected_revision: latest.revision,
          run_id: targetRunId,
          attestation: PROCESS_ABSENT_ATTESTATION,
        });
      });
      setOpen(false);
      setChecked(false);
    } catch (cause) {
      setError(missionError(t, cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mission-attest" data-testid="run-attest">
      <StatusNotice
        testId="run-attest-notice"
        line={t("missions.attest.line")}
        actions={
          open ? null : (
            <button
              type="button"
              onClick={() => {
                setError(null);
                setOpen(true);
              }}
              data-testid="run-attest-open"
            >
              {t("missions.attest.open")}
            </button>
          )
        }
      />
      {open ? (
        <div className="mission-attest-panel" role="group" aria-labelledby={titleId} data-testid="run-attest-panel">
          <h4 id={titleId}>{t("missions.attest.title")}</h4>
          <p>{t("missions.attest.howTo")}</p>
          {pid !== null ? (
            <p className="mission-attest-process" data-testid="run-attest-pid">{t("missions.attest.pid", { pid })}</p>
          ) : (
            <p className="muted" data-testid="run-attest-no-pid">{t("missions.attest.noPid")}</p>
          )}
          {group ? (
            <p className="mission-attest-process" data-testid="run-attest-group">{t("missions.attest.group", { group })}</p>
          ) : null}
          <ul className="mission-attest-hints">
            <li data-testid="run-attest-unix">
              {pid !== null ? t("missions.attest.unixHint") : t("missions.attest.unixHintNoPid")}
              {pid !== null ? (
                <code className="mission-attest-command" data-testid="run-attest-command">{`ps -p ${pid} -o pid,lstart,command`}</code>
              ) : null}
            </li>
            <li data-testid="run-attest-windows">{t("missions.attest.windowsHint")}</li>
          </ul>
          <label className="mission-attest-check">
            <input
              type="checkbox"
              checked={checked}
              disabled={busy}
              onChange={(event) => setChecked(event.target.checked)}
              data-testid="run-attest-check"
            />{" "}
            {t("missions.attest.check")}
          </label>
          <p className="mission-attest-unverified" data-testid="run-attest-unverified">{t("missions.attest.unverified")}</p>
          <div className="mission-attest-actions">
            <button
              type="button"
              className="primary"
              disabled={!checked || busy}
              onClick={() => void submit()}
              data-testid="run-attest-submit"
            >
              {busy ? t("missions.attest.submitting") : t("missions.attest.submit")}
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={() => {
                setOpen(false);
                setChecked(false);
              }}
              data-testid="run-attest-cancel"
            >
              {t("missions.common.cancel")}
            </button>
          </div>
        </div>
      ) : null}
      {error ? (
        <MissionErrorNotice
          error={withSupportedActions(error, ["resync", "retry"])}
          onRetry={() => void submit()}
          onAction={(action) => {
            if (action === "resync") void useMissionStore.getState().syncMission(props.mission.id);
          }}
        />
      ) : null}
    </div>
  );
}

/** RunDetail 헤더: 사용자 확인으로 정리된 실행 안내(데몬 관측 종료 안내 대신). */
export function RunAttestedNotice(): JSX.Element {
  const { t } = useI18n();
  return (
    <StatusNotice
      testId="run-attested-notice"
      line={t("missions.attest.label")}
      details={<p>{t("missions.attest.attestedDetail")}</p>}
    />
  );
}

/** 목록용 짧은 라벨 — 사용자 확인으로 정리된 실행에만 보인다. */
export function RunAttestedLabel(props: { run: Run | null }): JSX.Element | null {
  const { t } = useI18n();
  if (!runUserAttested(props.run)) return null;
  return (
    <span className="mission-attested-label" data-testid="run-attested-label">
      {t("missions.attest.label")}
    </span>
  );
}

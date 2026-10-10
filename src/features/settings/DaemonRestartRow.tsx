/**
 * 설정 → 일반: 백그라운드 실행 관리자(iyagi-termd) 재시작.
 *
 * 데몬은 앱보다 오래 산다(02-runner §1). 앱을 업데이트해도 옛 데몬 프로세스가
 * 그대로 돌면 고친 코드가 반영되지 않는다 — 앱을 다시 시작해도 새 앱이 옛
 * 데몬에 다시 붙을 뿐이다. 이 행이 그 데몬을 갈아 끼우는 유일한 UI 경로다.
 * 상태 줄은 `transportStatus().daemonOutdated`(디스크의 데몬 바이너리와의 빌드
 * 비교)를 보여 주고, 재시작은 확인을 거쳐 최신 데몬이어도 강제로 한다(멈춘
 * 데몬도 살릴 수 있게). 데몬의 PTY도 함께 끝나므로 확인 문구가 그것을 알린다.
 */

import { useCallback, useEffect, useState } from "react";
import type { DaemonClient } from "../daemon/client";
import { useI18n } from "../../i18n";
import { SettingRow } from "./controls";
import type { SettingsItemDef } from "./schema";

type Phase = "idle" | "confirm" | "restarting" | "done" | "failed";

export function DaemonRestartRow({ item, client }: { item: SettingsItemDef; client: DaemonClient }): JSX.Element {
  const { t } = useI18n();
  const [outdated, setOutdated] = useState<boolean | null>(null);
  const [phase, setPhase] = useState<Phase>("idle");

  const refresh = useCallback(async () => {
    try {
      const status = await client.transportStatus?.();
      setOutdated(status ? status.daemonOutdated : null);
    } catch {
      // 재연결 중이면 상태를 모른다 — 상태 줄을 비운다.
      setOutdated(null);
    }
  }, [client]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const restart = async (): Promise<void> => {
    setPhase("restarting");
    try {
      await client.restartDaemon?.({ force: true });
      setPhase("done");
    } catch {
      setPhase("failed");
    }
    // 시간 안에 끝나지 않았어도 새 데몬이 곧 올라올 수 있다 — 상태를 다시 맞춘다.
    void refresh();
  };

  const status =
    phase === "done"
      ? t("settings.item.daemonRestart.done")
      : outdated === true
        ? t("settings.item.daemonRestart.outdated")
        : outdated === false
          ? t("settings.item.daemonRestart.current")
          : null;

  return (
    <SettingRow item={item} layout="stacked">
      {(id) => (
        <div className="daemon-restart">
          {status ? (
            <p className="muted" role="status" data-testid="daemon-restart-status">
              {status}
            </p>
          ) : null}
          {phase === "confirm" ? (
            <>
              <p>{t("settings.item.daemonRestart.confirm")}</p>
              <div className="daemon-restart-actions">
                <button type="button" onClick={() => setPhase("idle")}>
                  {t("settings.cancel")}
                </button>
                <button id={id} type="button" onClick={() => void restart()} data-testid="daemon-restart-confirm">
                  {t("settings.item.daemonRestart.button")}
                </button>
              </div>
            </>
          ) : (
            <button
              id={id}
              type="button"
              disabled={phase === "restarting"}
              onClick={() => setPhase("confirm")}
              data-testid="daemon-restart"
            >
              {phase === "restarting" ? t("settings.item.daemonRestart.restarting") : t("settings.item.daemonRestart.button")}
            </button>
          )}
          {phase === "failed" ? <p className="setting-error">{t("settings.item.daemonRestart.failed")}</p> : null}
        </div>
      )}
    </SettingRow>
  );
}

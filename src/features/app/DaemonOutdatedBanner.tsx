/**
 * "데몬이 오래됨 — 재시작" 알림(review Finding 1).
 *
 * iyagi-termd 데몬은 앱보다 오래 산다(02-runner §1). 새로 빌드한 앱이 옛 데몬과
 * 이야기하면 새 RPC가 `unknown method`로 실패하거나 — 오늘처럼 — 고친 데몬
 * 코드가 반영되지 않은 채 옛 결함이 계속 돈다. 브리지는 연결 때 빌드 id를
 * 비교해(`HelloResult.daemon_version` vs 앱 빌드 id, 그리고 디스크의 데몬
 * 바이너리) `bridge_connection_status` → `client.transportStatus().daemonOutdated`
 * 로 알린다.
 *
 * 두 가지 표시를 제공한다:
 *  - `DaemonOutdatedStripItem`: 자원 스트립(아래쪽 고정 높이 띠) 안의 작은
 *    토막. 화면 배치를 흔들지 않고 클릭 한 번으로 재시작한다 — 기본 표시.
 *  - `DaemonOutdatedBanner`: 상단 배너(닫기 가능). 지금은 쓰지 않지만 계약을
 *    그대로 둔다.
 * 재시작은 옛 데몬을 물러나게 하고(`daemon.shutdown { stop_workloads: false }`)
 * 다시 연결해 새 데몬을 띄운다.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import type { DaemonClient } from "../daemon/client";
import { useI18n } from "../../i18n";

/** Re-check cadence for the outdated flag. The flag only changes on a
 *  (re)connect, so a relaxed poll is plenty. */
const POLL_MS = 4000;

export interface DaemonOutdatedState {
  /** 이 client가 상태 조회·재시작 seam을 갖는가(브라우저 mock은 없다). */
  available: boolean;
  outdated: boolean;
  restarting: boolean;
  failed: boolean;
  restart: () => Promise<void>;
}

/** 오래된 데몬 상태를 폴링하고 재시작을 제공한다(배너·스트립 항목 공용). */
export function useDaemonOutdated(client: DaemonClient): DaemonOutdatedState {
  const [outdated, setOutdated] = useState(false);
  const [restarting, setRestarting] = useState(false);
  const [failed, setFailed] = useState(false);
  const mounted = useRef(true);
  const available = Boolean(client.transportStatus && client.restartDaemon);

  const refresh = useCallback(async () => {
    const status = client.transportStatus;
    if (!status) return;
    try {
      const s = await status.call(client);
      if (mounted.current) setOutdated(s.daemonOutdated);
    } catch {
      // Mid-reconnect / disconnected: keep the last known state, poll again.
    }
  }, [client]);

  useEffect(() => {
    mounted.current = true;
    // No native transport seam (browser/dev mock omits these): never surface.
    if (!available) {
      return () => {
        mounted.current = false;
      };
    }
    void refresh();
    const timer = setInterval(() => void refresh(), POLL_MS);
    return () => {
      mounted.current = false;
      clearInterval(timer);
    };
  }, [client, refresh, available]);

  const restart = useCallback(async (): Promise<void> => {
    const restartDaemon = client.restartDaemon;
    if (!restartDaemon) return;
    setRestarting(true);
    setFailed(false);
    try {
      await restartDaemon.call(client);
      // A fresh daemon is now current — re-poll to clear the flag. If the
      // new daemon is not up yet, the interval poll clears it shortly.
      await refresh();
    } catch {
      if (mounted.current) setFailed(true);
    } finally {
      if (mounted.current) setRestarting(false);
    }
  }, [client, refresh]);

  return { available, outdated, restarting, failed, restart };
}

/**
 * 자원 스트립용 작은 항목: 데몬이 오래됐을 때만 "데몬 업데이트 · 재시작" 한
 * 토막을 띄운다. 스트립은 높이가 고정이라 화면 배치가 흔들리지 않는다.
 */
export function DaemonOutdatedStripItem({ client }: { client: DaemonClient }): JSX.Element | null {
  const { t } = useI18n();
  const { available, outdated, restarting, failed, restart } = useDaemonOutdated(client);
  if (!available || !outdated) return null;
  return (
    <button
      type="button"
      className={`strip-daemon${failed ? " strip-daemon-failed" : ""}`}
      onClick={() => void restart()}
      disabled={restarting}
      title={failed ? t("daemon.outdated.restartFailed") : t("daemon.outdated.stripTitle")}
      data-testid="daemon-outdated-strip"
    >
      {restarting ? t("daemon.outdated.restarting") : t("daemon.outdated.strip")}
    </button>
  );
}

export function DaemonOutdatedBanner({ client }: { client: DaemonClient }): JSX.Element | null {
  const { t } = useI18n();
  const { outdated, restarting, failed, restart } = useDaemonOutdated(client);
  const [dismissed, setDismissed] = useState(false);

  if (!outdated || dismissed) return null;

  return (
    <div className="daemon-outdated-banner" role="status" data-testid="daemon-outdated-banner">
      <span className="daemon-outdated-message">{t("daemon.outdated.message")}</span>
      {failed ? (
        <span className="daemon-outdated-error">{t("daemon.outdated.restartFailed")}</span>
      ) : null}
      <button
        type="button"
        className="daemon-outdated-restart"
        onClick={() => void restart()}
        disabled={restarting}
        data-testid="daemon-outdated-restart"
      >
        {restarting ? t("daemon.outdated.restarting") : t("daemon.outdated.restart")}
      </button>
      <button
        type="button"
        className="daemon-outdated-dismiss"
        aria-label={t("daemon.outdated.dismiss")}
        title={t("daemon.outdated.dismiss")}
        onClick={() => setDismissed(true)}
        data-testid="daemon-outdated-dismiss"
      >
        ×
      </button>
    </div>
  );
}

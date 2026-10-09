/**
 * agent-view 탭(05-ui §2·§6): 같은 run을 관찰하는 별도 보기 — 새 실행이
 * 아니다. MissionPage의 오른쪽 상세와 같은 RunDetail 컴포넌트를 골라 본다.
 * 작업 기록(또는 할 일)이 사라졌으면 무한 로딩 대신 탭 닫기를 권한다 — 단 이 보기가 연 뒤의
 * 동기화가 끝나기 전에는 판단하지 않는다(이전 동기화 결과로 "기록 없음"이 잠깐 비치지 않게).
 */

import { useEffect, useState } from "react";
import { useI18n } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { useMissionStore } from "./store";
import { useMissionUiStore } from "./uiStore";
import { RunDetail } from "./RunDetail";
import { classifySyncError } from "./missionConnection";
import { useArtifactText } from "./viewUtils";
import type { ArtifactRef } from "../../generated/ArtifactRef";

export function MissionAgentView(props: { missionId: string; taskId: string }): JSX.Element {
  const { t } = useI18n();
  const mission = useMissionStore((s) => s.missions[props.missionId] ?? null);
  const task = useMissionStore((s) => s.tasks[props.taskId] ?? null);
  const syncMission = useMissionStore((s) => s.syncMission);
  const syncStatus = useMissionStore((s) => s.sync[props.missionId] ?? null);
  const detailTab = useMissionUiStore((s) => s.perMission[props.missionId]?.detailTab ?? "activity");
  const patchUi = useMissionUiStore((s) => s.patchUi);

  /** 이 보기가 요청한 동기화가 한 번 끝났는가(그 전에는 로딩으로 보인다). */
  const [synced, setSynced] = useState(false);
  useEffect(() => {
    let alive = true;
    setSynced(false);
    void syncMission(props.missionId).finally(() => {
      if (alive) setSynced(true);
    });
    return () => {
      alive = false;
    };
  }, [props.missionId, syncMission]);

  const goal = useArtifactText(mission ? mission.goal_ref : (null as ArtifactRef | null));

  // mission 기록이 없거나, 이 보기의 snapshot을 다 받았는데(더 새 힌트도 없음) 이 할 일이 없으면
  // 더 기다릴 것이 없다. 동기화가 끝나기 전·진행 중에는 로딩으로 둔다.
  const notFound =
    synced &&
    syncStatus !== null &&
    !syncStatus.loading &&
    (classifySyncError(syncStatus.error) === "not_found" ||
      (mission !== null && task === null && syncStatus.error === null && !syncStatus.dirty && syncStatus.atSeq !== "0"));

  if (notFound) {
    const closeTab = () => {
      const workbench = useWorkbenchStore.getState();
      const tab = workbench.tabs.find(
        (candidate) => candidate.kind === "agent-view" && candidate.taskId === props.taskId,
      );
      if (tab) workbench.closeTab(tab.id);
    };
    return (
      <div className="mission-page mission-page-loading mission-not-found" data-testid="mission-not-found">
        <h2>{t("missions.notFound.title")}</h2>
        <p className="muted">{t("missions.notFound.body")}</p>
        <button type="button" onClick={closeTab} data-testid="not-found-close">
          {t("missions.notFound.closeTab")}
        </button>
      </div>
    );
  }

  if (!mission || !task) {
    return (
      <div className="mission-page mission-page-loading">
        <p className="muted">{t("missions.sync.loading")}</p>
      </div>
    );
  }

  return (
    <div className="mission-page mode-wide mission-agent-view" data-testid="mission-agent-view">
      <header className="mission-header">
        <div className="mission-header-title">
          <h2>{task.title}</h2>
          <span className="muted">· {goal.error ? "" : (goal.text ?? "").slice(0, 80)}</span>
        </div>
      </header>
      <RunDetail
        mission={mission}
        task={task}
        detailTab={detailTab}
        onDetailTabChange={(tab) => patchUi(props.missionId, { detailTab: tab })}
      />
    </div>
  );
}

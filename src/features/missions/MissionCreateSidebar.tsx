import { useI18n } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { MissionCreate } from "./MissionCreate";

/** A sibling of the terminal area: opening this never replaces or blocks it. */
export function MissionCreateSidebar(): JSX.Element | null {
  const request = useWorkbenchStore((s) => s.missionCreate);
  const close = useWorkbenchStore((s) => s.closeMissionCreate);
  const { t } = useI18n();
  if (!request) return null;
  return (
    <aside className="mission-create-sidebar" aria-label={t("missions.create.title")} data-testid="mission-create-sidebar">
      <MissionCreate
        key={`${request.repositoryPath ?? ""}\u0000${request.goal ?? ""}\u0000${request.followUpOf ?? ""}`}
        initialRepositoryPath={request.repositoryPath ?? null}
        initialGoal={request.goal ?? null}
        followUpOf={request.followUpOf ?? null}
        onClose={() => {
          // A previous asynchronous submission must not dismiss a newer draft.
          const current = useWorkbenchStore.getState().missionCreate;
          if (current?.repositoryPath === request.repositoryPath && current?.goal === request.goal && current?.followUpOf === request.followUpOf) close();
        }}
      />
    </aside>
  );
}

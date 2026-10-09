import { useI18n } from "../../i18n";

interface Change { path: string; diff: string; kind: { type: "add" | "delete" | "update"; move_path?: string | null } }
export interface FileChangeRequest { reason: string | null; grant_root: string | null; details_available: boolean; changes: Change[] | null }

export function approvalText(text: string | null | undefined): string | null | undefined {
  if (!text) return text;
  try {
    const value: unknown = JSON.parse(text);
    if (value && typeof value === "object" && "question" in value && typeof value.question === "string") return value.question;
  } catch { /* Older approvals can be plain text. */ }
  return text;
}

export function fileChangeRequest(text: string | null | undefined): FileChangeRequest | null {
  if (!text) return null;
  try {
    const value = JSON.parse(text);
    if (value?.type !== "file_change" || (value.reason !== null && typeof value.reason !== "string")
      || (value.grant_root !== null && typeof value.grant_root !== "string")) return null;
    const complete = value.details_available === true && Array.isArray(value.changes) && value.changes.length > 0
      && value.changes.every((c: Change) => c && typeof c.path === "string" && typeof c.diff === "string"
        && c.kind && ["add", "delete", "update"].includes(c.kind.type)
        && (c.kind.move_path == null || typeof c.kind.move_path === "string"));
    return { reason: value.reason, grant_root: value.grant_root, details_available: complete, changes: complete ? value.changes : null };
  } catch { return null; }
}

export function FileChangeApproval({ request }: { request: FileChangeRequest }): JSX.Element {
  const { t } = useI18n();
  return <div className="mission-file-approval" data-testid="file-change-approval">
    {request.reason ? <p>{request.reason}</p> : null}
    {request.grant_root != null ? <p className="mission-area-error">{t("missions.decision.grantRoot", { path: request.grant_root })}</p> : null}
    {!request.details_available ? <p className="mission-area-error">{t("missions.decision.fileDetailsMissing")}</p> : null}
    {request.changes?.map((change, index) => <div key={index}>
      <strong>{t(`missions.decision.file.${change.kind.type}`)}: <code>{change.path}</code></strong>
      {change.kind.move_path != null ? <p>{t("missions.decision.moveTo", { path: change.kind.move_path })}</p> : null}
      <pre>{change.diff}</pre>
    </div>)}
  </div>;
}

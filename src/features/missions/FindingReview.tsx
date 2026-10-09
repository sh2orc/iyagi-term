import { useRef, useState } from "react";
import type { Finding } from "../../generated/Finding";
import type { Mission } from "../../generated/Mission";
import type { ArtifactRef } from "../../generated/ArtifactRef";
import type { MissionFindingResolveParams } from "../../generated/MissionFindingResolveParams";
import { useI18n } from "../../i18n";
import { getMissionClient } from "./clientAccess";
import { MISSION_TEXT_MAX_BYTES, uploadTextArtifact } from "./artifactUpload";
import { errorText, newRequestId, useArtifactText } from "./viewUtils";
import { severityLabel } from "./labels";

type Props = { mission: Mission; finding: Finding; disabled: boolean; onRefresh: () => void };

export function FindingReview(props: Props): JSX.Element {
  const { t } = useI18n();
  const { mission, finding } = props;
  const evidence = useArtifactText(finding.evidence_ref);
  const resolution = useArtifactText(finding.resolution_ref);
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const current = useRef(props);
  current.current = props;
  const submitting = useRef(false);
  const pending = useRef<{ text: string; uploadId: string; reference?: ArtifactRef; params?: MissionFindingResolveParams } | null>(null);
  const actionable = !props.disabled && ["running", "paused"].includes(mission.state)
    && finding.mission_id === mission.id && finding.candidate_id === mission.candidate_id
    && finding.resolution === "open";
  const tooLarge = new TextEncoder().encode(reason.trim()).byteLength > MISSION_TEXT_MAX_BYTES;

  const dismiss = async () => {
    if (!actionable || !reason.trim() || tooLarge || submitting.current || saved) return;
    const client = getMissionClient();
    if (!client) { setError(t("missions.sync.noClient")); return; }
    submitting.current = true;
    setBusy(true);
    setError(null);
    const text = reason.trim();
    if (pending.current?.text !== text) pending.current = { text, uploadId: newRequestId() };
    const attempt = pending.current!;
    try {
      attempt.reference ??= await uploadTextArtifact(client, text, { missionId: mission.id, requestId: attempt.uploadId });
      const latest = current.current;
      if (latest.disabled || latest.mission.id !== mission.id || latest.mission.candidate_id !== finding.candidate_id
        || !["running", "paused"].includes(latest.mission.state) || latest.finding.resolution !== "open") {
        throw new Error(t("missions.finding.changed"));
      }
      // An uncertain receipt retries the exact same request. A confirmed CAS
      // rejection can use the refreshed revision on the next explicit click.
      attempt.params ??= { request_id: newRequestId(), mission_id: mission.id, expected_revision: latest.mission.revision,
        finding_id: finding.id, resolution: "dismissed", reason_ref: attempt.reference };
      await client.missionFindingResolve(attempt.params);
      setSaved(true);
      props.onRefresh();
    } catch (cause) {
      if (cause && typeof cause === "object" && "code" in cause && cause.code === "REVISION_CONFLICT") attempt.params = undefined;
      setError(t("missions.finding.saveError", { message: errorText(cause) }));
    } finally {
      submitting.current = false;
      setBusy(false);
    }
  };

  return <li className={`finding-${finding.resolution}`} data-testid="finding-review" data-anchor={`finding:${finding.id}`} tabIndex={-1}>
    <span className={`finding-severity severity-${finding.severity}`} data-testid="finding-severity">{severityLabel(t, finding.severity)}</span>
    {finding.path ? <span className="muted"> · {finding.path}{finding.line !== null ? `:${finding.line}` : ""}</span> : null}
    <pre className="mission-finding-body">{evidence.error ? t("missions.finding.bodyError") : evidence.text ?? "…"}</pre>
    {finding.resolution_ref ? <><strong>{t("missions.finding.reasonSaved")}</strong><pre className="mission-finding-body">{resolution.error ? t("missions.finding.bodyError") : resolution.text ?? "…"}</pre></> : null}
    {actionable && !saved ? <div className="mission-finding-form">
      <label htmlFor={`finding-reason-${finding.id}`}>{t("missions.finding.reason")}</label>
      <textarea id={`finding-reason-${finding.id}`} rows={3} value={reason} disabled={busy}
        onChange={event => setReason(event.target.value)} data-testid="finding-reason" />
      <p className="muted">{t("missions.finding.reasonHelp")}</p>
      {tooLarge ? <p role="alert">{t("missions.finding.tooLarge")}</p> : null}
      <button type="button" disabled={busy || !reason.trim() || tooLarge} onClick={() => void dismiss()} data-testid="finding-dismiss">
        {t("missions.finding.dismiss")}
      </button>
    </div> : null}
    {saved ? <p role="status" data-testid="finding-saved">{t("missions.finding.saved")}</p> : null}
    {error ? <p className="mission-area-error" role="alert">{error}</p> : null}
  </li>;
}

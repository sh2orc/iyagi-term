/**
 * 결과 가져오기(05-ui §9 보강): 후보 커밋을 사용자 저장소로 가져오는 방법.
 *
 * - 데몬은 사용자 브랜치를 움직이지 않는다 — 화면은 복사 가능한 명령만 준다.
 * - `repository.inspect`로 현재 HEAD를 읽어 base와 다르면 ff-only 대신 새
 *   브랜치를 권한다.
 * - "터미널에서 열기"는 저장소 경로에서 새 터미널 탭을 열고 명령을 클립보드에
 *   복사한다. 입력을 자동 실행하지 않는다.
 */

import { useContext, useEffect, useState } from "react";
import type { Candidate } from "../../generated/Candidate";
import type { Mission } from "../../generated/Mission";
import { useI18n } from "../../i18n";
import { ControllerContext } from "../../app/controllerContext";
import { getMissionClient } from "./clientAccess";
import {
  headRelation,
  importCommands,
  recommendedImport,
  type HeadRelation,
  type ImportCommandKind,
} from "./importCommands";

export interface RepositoryHeadState {
  headOid: string | null;
  loading: boolean;
  error: boolean;
}

/** 저장소 HEAD를 한 번 읽는다(경로가 바뀌거나 reloadKey가 바뀌면 다시). */
export function useRepositoryHead(path: string, reloadKey = 0): RepositoryHeadState {
  const [state, setState] = useState<RepositoryHeadState>({ headOid: null, loading: true, error: false });
  useEffect(() => {
    const client = getMissionClient();
    if (!client || !path) {
      setState({ headOid: null, loading: false, error: true });
      return;
    }
    let alive = true;
    setState((previous) => ({ ...previous, loading: true, error: false }));
    client
      .repositoryInspect({ path })
      .then((result) => {
        if (alive) setState({ headOid: result.head_oid || null, loading: false, error: false });
      })
      .catch(() => {
        if (alive) setState({ headOid: null, loading: false, error: true });
      });
    return () => {
      alive = false;
    };
  }, [path, reloadKey]);
  return state;
}

/** 클립보드 복사 — 실패하면 false(권한 거절·API 없음). */
export async function copyText(text: string): Promise<boolean> {
  try {
    if (typeof navigator === "undefined" || typeof navigator.clipboard?.writeText !== "function") return false;
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

function shortOid(oid: string): string {
  return oid.slice(0, 10);
}

type CopyStatus = { key: string; ok: boolean } | null;

function useCopy(): { status: CopyStatus; copy: (key: string, text: string) => Promise<boolean> } {
  const [status, setStatus] = useState<CopyStatus>(null);
  useEffect(() => {
    if (!status) return;
    const timer = setTimeout(() => setStatus(null), 2500);
    return () => clearTimeout(timer);
  }, [status]);
  const copy = async (key: string, text: string) => {
    const ok = await copyText(text);
    setStatus({ key, ok });
    return ok;
  };
  return { status, copy };
}

function CopyButton(props: { copyKey: string; text: string; status: CopyStatus; onCopy: (key: string, text: string) => void }): JSX.Element {
  const { t } = useI18n();
  const mine = props.status?.key === props.copyKey ? props.status : null;
  return (
    <>
      <button
        type="button"
        className="result-import-copy"
        onClick={() => props.onCopy(props.copyKey, props.text)}
        data-testid={`copy-${props.copyKey}`}
      >
        {t("missions.result.copy")}
      </button>
      {mine ? (
        <span className={mine.ok ? "result-import-copied" : "result-import-copy-failed"} role="status">
          {mine.ok ? t("missions.result.copied") : t("missions.result.copyFailed")}
        </span>
      ) : null}
    </>
  );
}

function HeadNotice(props: { relation: HeadRelation; head: RepositoryHeadState; noChanges: boolean }): JSX.Element | null {
  const { t } = useI18n();
  if (props.noChanges) {
    return <p className="muted" data-testid="import-no-changes">{t("missions.result.importNoChanges")}</p>;
  }
  if (props.relation === "moved") {
    return (
      <p className="result-import-warning" role="note" data-testid="import-head-mismatch">
        {t("missions.result.importHeadMismatch")}
      </p>
    );
  }
  if (props.relation === "at_commit") {
    return <p className="muted" data-testid="import-head-at-commit">{t("missions.result.importHeadAtCommit")}</p>;
  }
  if (props.relation === "unknown" && props.head.error) {
    return <p className="muted" data-testid="import-head-unknown">{t("missions.result.importHeadUnknown")}</p>;
  }
  return null;
}

/** 상단 고정 요약: 커밋 + 추천 명령 복사 + HEAD 경고. */
export function ResultImportSummary(props: {
  mission: Mission;
  candidate: Candidate;
  head: RepositoryHeadState;
  accepted: boolean;
  onShowDetails?: () => void;
}): JSX.Element {
  const { t } = useI18n();
  const { mission, candidate } = props;
  const commands = importCommands(mission, candidate);
  const relation = headRelation(props.head.headOid, candidate);
  const noChanges = candidate.commit_oid === candidate.base_oid;
  const kind = recommendedImport(relation);
  const { status, copy } = useCopy();
  return (
    <div className={`result-import-summary${props.accepted ? " emphasized" : ""}`} data-testid="import-summary">
      <p className="result-import-line">
        <strong>{t("missions.result.importTitle")}</strong>{" "}
        <span className="muted">
          {t("missions.result.importCommit")} <code>{shortOid(candidate.commit_oid)}</code>
        </span>
      </p>
      {noChanges ? null : (
        <p className="result-import-line">
          <code className="result-import-command" data-testid="import-summary-command">{commands[kind]}</code>{" "}
          <CopyButton copyKey={`summary-${kind}`} text={commands[kind]} status={status} onCopy={(key, text) => void copy(key, text)} />
        </p>
      )}
      <HeadNotice relation={relation} head={props.head} noChanges={noChanges} />
      {props.onShowDetails ? (
        <button type="button" className="link" onClick={props.onShowDetails} data-testid="import-show-details">
          {t("missions.result.importMore")}
        </button>
      ) : null}
    </div>
  );
}

/** 상세 섹션: OID·참조·명령 전체와 터미널 열기. */
export function ResultImport(props: {
  mission: Mission;
  candidate: Candidate;
  head: RepositoryHeadState;
  accepted: boolean;
}): JSX.Element {
  const { t } = useI18n();
  const controller = useContext(ControllerContext);
  const { mission, candidate } = props;
  const commands = importCommands(mission, candidate);
  const relation = headRelation(props.head.headOid, candidate);
  const noChanges = candidate.commit_oid === candidate.base_oid;
  const recommended = recommendedImport(relation);
  const [selected, setSelected] = useState<ImportCommandKind | null>(null);
  const chosen = selected ?? recommended;
  const { status, copy } = useCopy();
  const [terminalNote, setTerminalNote] = useState<string | null>(null);

  const rows: Array<{ kind: ImportCommandKind; label: string; help: string | null }> = [
    { kind: "merge", label: t("missions.result.importMerge"), help: t("missions.result.importMergeHelp") },
    { kind: "branch", label: t("missions.result.importBranch"), help: null },
    { kind: "diff", label: t("missions.result.importDiff"), help: null },
  ];

  const openInTerminal = async () => {
    // 클립보드 쓰기는 사용자 동작 안에서 먼저 한다(탭 전환 전에).
    const ok = await copyText(commands[chosen]);
    const message = ok ? t("missions.result.openTerminalCopied") : t("missions.result.copyFailed");
    if (controller) {
      const tabId = controller.newTab();
      controller.createFirstPane(tabId, mission.repository_path);
      controller.toast(message);
      setTerminalNote(null);
    } else {
      setTerminalNote(message);
    }
  };

  return (
    <div className={`result-import${props.accepted ? " emphasized" : ""}`} data-testid="result-import">
      <p className="muted">{t("missions.result.importHelp")}</p>
      <dl className="result-import-oids">
        <dt>{t("missions.result.importCommit")}</dt>
        <dd>
          <code data-testid="import-commit">{candidate.commit_oid}</code>{" "}
          <CopyButton copyKey="commit" text={candidate.commit_oid} status={status} onCopy={(key, text) => void copy(key, text)} />
        </dd>
        <dt>{t("missions.result.importBase")}</dt>
        <dd>
          <code data-testid="import-base">{candidate.base_oid}</code>{" "}
          <CopyButton copyKey="base" text={candidate.base_oid} status={status} onCopy={(key, text) => void copy(key, text)} />
        </dd>
        <dt>{t("missions.result.importRef")}</dt>
        <dd>
          <code data-testid="import-ref">{commands.refName}</code>{" "}
          <CopyButton copyKey="ref" text={commands.refName} status={status} onCopy={(key, text) => void copy(key, text)} />
        </dd>
      </dl>
      <HeadNotice relation={relation} head={props.head} noChanges={noChanges} />
      {noChanges ? null : (
        <>
          <ul className="result-import-commands">
            {rows.map((row) => (
              <li key={row.kind} data-testid={`import-command-${row.kind}`}>
                <label className="result-import-choice">
                  <input
                    type="radio"
                    name={`import-command-${candidate.id}`}
                    checked={chosen === row.kind}
                    onChange={() => setSelected(row.kind)}
                  />
                  {row.label}
                  {row.kind === recommended ? <span className="result-import-recommended">{t("missions.result.importRecommended")}</span> : null}
                </label>
                <div className="result-import-command-row">
                  <code className="result-import-command">{commands[row.kind]}</code>{" "}
                  <CopyButton copyKey={row.kind} text={commands[row.kind]} status={status} onCopy={(key, text) => void copy(key, text)} />
                </div>
                {row.help ? <p className="muted result-import-help">{row.help}</p> : null}
              </li>
            ))}
          </ul>
          <button type="button" onClick={() => void openInTerminal()} data-testid="import-open-terminal">
            {t("missions.result.openTerminal")}
          </button>
          {terminalNote ? <p role="status" data-testid="import-terminal-note">{terminalNote}</p> : null}
        </>
      )}
    </div>
  );
}

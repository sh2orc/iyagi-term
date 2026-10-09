/**
 * Modals: 명령 팔레트(Ctrl/Cmd+Shift+P), 관리 실행, 시작 터미널 선택,
 * 알림. modal이 열려 있으면 앱 단축키가 차단된다
 * (shortcuts.ts modalOpen).
 */

import { runOnTerminalPage } from "./layoutPage";
import { AboutDialog } from "./AboutDialog";
import { usePreferences } from "../store/preferences";
import { useMemo, useState } from "react";
import { useWorkbenchStore } from "../store/workbenchStore";
import { useController } from "./controllerContext";
import { pasteTooLargeText } from "../features/monitor/statusStrings";
import { PASTE_MAX_BYTES } from "../features/security/paste";
import { ManagedRunDialog as ManagedRunDialogFeature } from "../features/workloads/ManagedRunDialog";
import type { SessionSearchResult } from "../generated/SessionSearchResult";
import { ShellSelectDialog } from "../features/terminal/ShellSelectDialog";
import { AgentSessionsDialog } from "../features/agentSessions/AgentSessionsDialog";
import { AgentIcon } from "../features/terminal/AgentIcon";
import { workloadListAgent, workloadListTitle } from "../features/workloads/workloadTitle";
import { MoveTargetDialog } from "./MoveTargetDialog";
import { RegroupDialog } from "./RegroupDialog";
import { useShellProfileStore } from "../stores/shellProfileStore";
import { profileDisplayLabel } from "../features/terminal/shellProfiles";
import { useI18n } from "../i18n";
import type { Platform } from "../features/terminal/shortcuts";
import { focusedRepositoryHint, openMissionCreate } from "../features/missions/entry";
import { missionEntryState } from "../features/missions/capability";
import { openMissionList } from "../features/missions/entryPoints";
import { MissionList } from "../features/missions/MissionList";

/**
 * 팔레트 명령 하나. run의 반환 타입을 고정한다 — 배열 원소 형이 섞이면(비활성 항목) 추론이
 * 순환해 TS7023이 난다.
 */
export interface PaletteCommand {
  label: string;
  run: () => void;
  disabled?: boolean;
  disabledReason?: string | undefined;
  /** 라벨에 없지만 찾을 법한 낱말(한/영). */
  keywords?: readonly string[];
}

/** AI 작업 명령의 검색어 — "미션"·"agent"·"팀"으로 찾아도 나온다. */
export const MISSION_PALETTE_KEYWORDS: readonly string[] = ["mission", "미션", "agent", "에이전트", "team", "팀", "AI", "AI 작업"];

/** 팔레트 검색: 라벨 또는 검색어에 질의가 들어 있으면(대소문자 무시·앞뒤 공백 무시). */
export function paletteCommandMatches(command: Pick<PaletteCommand, "label" | "keywords">, query: string): boolean {
  const needle = query.trim().toLowerCase();
  if (needle.length === 0) return true;
  if (command.label.toLowerCase().includes(needle)) return true;
  return (command.keywords ?? []).some((keyword) => keyword.toLowerCase().includes(needle));
}

export function ModalHost(props: { platform: Platform }): JSX.Element | null {
  const modal = useWorkbenchStore((s) => s.modal);
  const closeModal = useWorkbenchStore((s) => s.closeModal);
  const controller = useController();
  if (!modal) return null;
  // 종료 확인은 Rust 쪽 대기 상태와 짝이라, 바깥 클릭으로 닫아도 취소를 알린다.
  const dismiss =
    modal.kind === "quit"
      ? () => { if (!controller.quitPending) controller.cancelQuit(); }
      : closeModal;
  return (
    <div className={`modal-backdrop${modal.kind === "quit" || modal.kind === "about" ? " quit-backdrop" : ""}`} role="presentation" onMouseDown={dismiss}>
      <div
        className={`modal${modal.kind === "quit" ? " quit-modal" : modal.kind === "about" ? " about-modal" : ""}`}
        role="dialog"
        aria-labelledby={modal.kind === "quit" ? "quit-title" : modal.kind === "about" ? "about-title" : modal.kind === "mission-list" ? "mission-list-title" : undefined}
        aria-describedby={modal.kind === "quit" ? "quit-description" : undefined}
        aria-modal="true"
        onMouseDown={(e) => e.stopPropagation()}
      >
        {modal.kind === "close-panes" ? <ClosePanesDialog {...modal} /> : null}
        {modal.kind === "about" ? <AboutDialog version={modal.version} onClose={closeModal} /> : null}
        {modal.kind === "quit" ? <QuitDialog sessions={modal.sessions} /> : null}
        {modal.kind === "palette" ? <Palette /> : null}
        {modal.kind === "search" ? <SearchDialog /> : null}
        {modal.kind === "managed-run" ? <ManagedRunDialogFeature /> : null}
        {modal.kind === "shell-select" ? <ShellSelectModal platform={props.platform} /> : null}
        {modal.kind === "agent-sessions" ? <AgentSessionsDialog /> : null}
        {modal.kind === "resume-agents" ? <ResumeAgentsDialog workloadIds={modal.workloadIds} /> : null}
        {/* 재배치(04-ui §2-5): pane 이동과 탭 합치기는 같은 "대상 탭 고르기" 화면이다. */}
        {modal.kind === "move-pane" || modal.kind === "merge-tab" ? <MoveTargetDialog modal={modal} /> : null}
        {modal.kind === "regroup" ? <RegroupDialog plan={modal.plan} /> : null}
        {modal.kind === "mission-list" ? <MissionList onClose={closeModal} /> : null}
        {modal.kind === "notice" ? (
          <NoticeDialog message={modal.kind === "notice" ? modal.message : ""} />
        ) : null}
      </div>
    </div>
  );
}

/** 시작 터미널 선택: 고르면 그 셸로 전체 창 pane 1개 + (체크 시) 기본 저장. */
function ShellSelectModal(props: { platform: Platform }): JSX.Element {
  const controller = useController();
  const { t } = useI18n();
  const closeModal = useWorkbenchStore((s) => s.closeModal);
  return (
    <ShellSelectDialog
      platform={props.platform}
      onPick={(profile, saveDefault) => {
        closeModal();
        if (saveDefault) useShellProfileStore.getState().setDefault(profile.id);
        controller.newTerminal({
          program: profile.program,
          argv: profile.argv,
          label: profileDisplayLabel(profile, t),
        });
      }}
    />
  );
}

function Palette(): JSX.Element {
  const { t } = useI18n();
  const controller = useController();
  const closeModal = useWorkbenchStore((s) => s.closeModal);
  const toggleQueueDrawer = useWorkbenchStore((s) => s.toggleQueueDrawer);
  const setGraphDrawer = useWorkbenchStore((s) => s.setGraphDrawer);
  const openModal = useWorkbenchStore((s) => s.openModal);
  const startTabRename = useWorkbenchStore((s) => s.startTabRename);
  const activeTabId = useWorkbenchStore((s) => s.activeTabId);
  const focusedLeafId = useWorkbenchStore((s) => s.focusedLeafId);
  const missionProtocol = useWorkbenchStore((s) => s.missionProtocol);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);

  // 데몬이 mission_protocol을 선언하지 않으면 AI 작업 명령은 비활성 + 사유(개발 빌드),
  // 프로덕션 빌드에서는 숨긴다. 데몬이 앱보다 새로우면 앱 업데이트를 안내한다.
  const missionEntry = missionEntryState(missionProtocol);
  const missionReason = missionEntry.reasonKey ? t(missionEntry.reasonKey) : undefined;

  const commands = useMemo<PaletteCommand[]>(
    () => [
      { label: t("palette.splitRow"), run: () => controller.dispatchShortcut("split-row") },
      { label: t("palette.splitColumn"), run: () => controller.dispatchShortcut("split-column") },
      { label: t("palette.closePane"), run: () => controller.dispatchShortcut("close-pane") },
      { label: t("palette.paste"), run: runOnTerminalPage(() => controller.dispatchShortcut("paste")) },
      { label: t("palette.search"), run: runOnTerminalPage(() => openModal({ kind: "search" })) },
      { label: t("palette.managedRun"), run: () => openModal({ kind: "managed-run" }) },
      { label: t("palette.agentSessions"), run: () => openModal({ kind: "agent-sessions" }) },
      { label: t("palette.resumeAll"), run: () => controller.dispatchShortcut("resume-all") },
      ...(missionEntry.hidden
        ? []
        : [
            {
              label: t("palette.missionNew"),
              // 팔레트를 연 자리(보고 있는 터미널)의 저장소를 채워 연다.
              run: () => openMissionCreate(focusedRepositoryHint(useWorkbenchStore.getState())),
              disabled: !missionEntry.enabled,
              disabledReason: missionEntry.enabled ? t("missions.newMission.hint") : missionReason,
              keywords: MISSION_PALETTE_KEYWORDS,
            },
            {
              label: t("palette.missionList"),
              run: () => openMissionList(),
              disabled: !missionEntry.enabled,
              disabledReason: missionEntry.enabled ? undefined : missionReason,
              keywords: MISSION_PALETTE_KEYWORDS,
            },
            {
              label: t("palette.missionSettings"),
              run: () => useWorkbenchStore.getState().openSettings("missions"),
              keywords: [...MISSION_PALETTE_KEYWORDS, "settings", "설정", "빠른 설정", "quick setup"],
            },
            {
              label: t("palette.missionGuide"),
              run: () => useWorkbenchStore.getState().openSettings("missions"),
              keywords: [...MISSION_PALETTE_KEYWORDS, "guide", "help", "사용법", "도움말"],
            },
          ]),
      { label: t("palette.openQueue"), run: runOnTerminalPage(() => toggleQueueDrawer(true)) },
      { label: t("palette.openGraph"), run: runOnTerminalPage(() => setGraphDrawer(true)) },
      { label: t("palette.newTab"), run: () => controller.newTab() },
      {
        label: t("palette.renameTab"),
        // 붙여넣기·찾기·탭 이름·패널은 터미널 화면의 일이다 — 배치 편집에서 고르면 먼저 돌아간다.
        run: runOnTerminalPage(() => {
          if (activeTabId) startTabRename(activeTabId);
        }),
      },
      // 재배치(04-ui §2-5) — 초점 창·활성 탭이 없으면 할 일이 없다(조용히 넘어간다).
      {
        label: t("palette.detachPane"),
        run: () => {
          if (focusedLeafId) controller.detachPaneToNewTab(focusedLeafId);
        },
      },
      {
        label: t("palette.movePane"),
        run: () => {
          if (focusedLeafId) openModal({ kind: "move-pane", leafId: focusedLeafId });
        },
      },
      {
        label: t("palette.moveTabLeft"),
        run: () => {
          if (activeTabId) controller.moveTab(activeTabId, -1);
        },
      },
      {
        label: t("palette.moveTabRight"),
        run: () => {
          if (activeTabId) controller.moveTab(activeTabId, 1);
        },
      },
      {
        label: t("palette.mergeTab"),
        run: () => {
          if (activeTabId) openModal({ kind: "merge-tab", tabId: activeTabId });
        },
      },
      { label: t("palette.regroup"), run: () => controller.regroupByProject() },
      { label: t("palette.layoutEditor"), run: () => useWorkbenchStore.getState().setPage("layout") },
      { label: t("palette.broadcast"), run: () => controller.toggleBroadcast() },
      { label: t("palette.memoryDiagnostics"), run: () => controller.copyMemoryDiagnostics() },
      { label: t("palette.quit"), run: () => controller.requestQuit() },
    ],
    // t는 언어가 바뀌면 새로 바인딩돼 라벨도 다시 계산된다.
    // eslint-disable-next-line react-hooks/exhaustive-deps -- missionEntry는 protocol에서 파생된다.
    [controller, openModal, toggleQueueDrawer, setGraphDrawer, startTabRename, activeTabId, focusedLeafId, t, missionProtocol],
  );

  const filtered = commands.filter((c) => paletteCommandMatches(c, query));
  const clamped = Math.min(selected, Math.max(filtered.length - 1, 0));
  const runCommand = (index: number): void => {
    const command = filtered[index];
    if (!command || command.disabled) return;
    closeModal();
    command.run();
  };

  return (
    <>
      <input
        className="palette-input"
        autoFocus
        placeholder={t("palette.placeholder")}
        value={query}
        aria-label={t("palette.aria")}
        role="combobox"
        aria-expanded="true"
        aria-controls="palette-listbox"
        aria-activedescendant={filtered[clamped] ? `palette-option-${clamped}` : undefined}
        onChange={(e) => {
          setQuery(e.target.value);
          setSelected(0);
        }}
        onKeyDown={(e) => {
          // 키보드 탐색(W2): ↑/↓ 선택, Enter는 첫 항목이 아니라 선택 항목.
          if (e.key === "ArrowDown") {
            e.preventDefault();
            setSelected(Math.min(clamped + 1, filtered.length - 1));
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setSelected(Math.max(clamped - 1, 0));
          } else if (e.key === "Enter") {
            runCommand(clamped);
          }
        }}
      />
      <ul className="palette-list" id="palette-listbox" role="listbox">
        {filtered.map((cmd, index) => (
          <li
            key={cmd.label}
            role="option"
            aria-selected={index === clamped}
            id={`palette-option-${index}`}
          >
            <button
              type="button"
              className={index === clamped ? "palette-selected" : undefined}
              disabled={cmd.disabled}
              title={cmd.disabledReason}
              onMouseEnter={() => setSelected(index)}
              onClick={() => runCommand(index)}
            >
              {cmd.label}
            </button>
          </li>
        ))}
      </ul>
    </>
  );
}

function SearchDialog(): JSX.Element {
  const { t } = useI18n();
  const controller = useController();
  const [query, setQuery] = useState("");
  const [status, setStatus] = useState<string | null>(null);
  const [scope, setScope] = useState<"scrollback" | "journal">("scrollback");
  const [journalResults, setJournalResults] = useState<SessionSearchResult | null>(null);
  const run = (direction: "next" | "previous") => {
    const found = controller.searchFocused(query, direction);
    setStatus(found ? t("search.found") : t("search.none"));
  };
  const runJournal = () => {
    void controller
      .searchJournals(query)
      .then((result) => {
        setJournalResults(result);
        setStatus(null);
      })
      .catch(() => setStatus(t("search.journal.failed")));
  };
  return (
    <div className="search-dialog">
      <h2>{t("search.title")}</h2>
      <p className="muted">{t("search.hint")}</p>
      <input
        autoFocus
        className="palette-input"
        value={query}
        placeholder={t("search.placeholder")}
        aria-label={t("search.aria")}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            if (scope === "journal") runJournal();
            else run(e.shiftKey ? "previous" : "next");
          }
        }}
      />
      <div className="search-scope" role="radiogroup" aria-label={t("search.scope.aria")}>
        <label>
          <input
            type="radio"
            name="search-scope"
            checked={scope === "scrollback"}
            onChange={() => {
              setScope("scrollback");
              setJournalResults(null);
            }}
          />
          {t("search.scope.scrollback")}
        </label>
        <label>
          <input
            type="radio"
            name="search-scope"
            checked={scope === "journal"}
            onChange={() => setScope("journal")}
          />
          {t("search.scope.journal")}
        </label>
      </div>
      <div className="modal-actions">
        {scope === "scrollback" ? (
          <>
            <button type="button" onClick={() => run("previous")}>
              {t("search.prev")}
            </button>
            <button type="button" onClick={() => run("next")}>
              {t("search.next")}
            </button>
          </>
        ) : (
          <button type="button" onClick={runJournal}>
            {t("search.journal.run")}
          </button>
        )}
      </div>
      {status ? <p className="muted" role="status">{status}</p> : null}
      {scope === "journal" && journalResults ? (
        <div className="search-journal-results">
          {journalResults.matches.length === 0 ? (
            <p className="muted">{t("search.none")}</p>
          ) : (
            <ul>
              {journalResults.matches.map((match) => (
                <li key={`${match.session_id}:${match.seq}`}>
                  <button
                    type="button"
                    onClick={() => controller.focusSessionPane(match.session_id)}
                    title={t("search.journal.goto")}
                  >
                    <span className="search-journal-session">{match.session_id.slice(0, 8)}</span>
                    <span className="search-journal-line">{match.line}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
          {journalResults.truncated ? (
            <p className="muted">{t("search.journal.truncated")}</p>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

function NoticeDialog(props: { message: string }): JSX.Element {
  const { t } = useI18n();
  const closeModal = useWorkbenchStore((s) => s.closeModal);
  return (
    <div className="notice-dialog" role="alert">
      <p>{props.message || pasteTooLargeText(PASTE_MAX_BYTES)}</p>
      <div className="modal-actions">
        <button type="button" onClick={closeModal}>
          {t("notice.confirm")}
        </button>
      </div>
    </div>
  );
}

/**
 * 앱 종료 확인(트레이 종료·앱 메뉴 Quit·팔레트): 터미널은 분리 데몬이 소유
 * 하므로 "유지하고 종료"와 "터미널도 종료"가 다른 결과다. "다시 묻지 않기"는
 * 고른 쪽을 설정(quitBehavior)에 남긴다 — 설정 > 터미널에서 되돌릴 수 있다.
 */
function QuitDialog({ sessions }: { sessions: number }): JSX.Element {
  const { t } = useI18n();
  const controller = useController();
  const [remember, setRemember] = useState(false);
  const [busy, setBusy] = useState<"keep" | "terminate" | null>(null);
  const decide = (terminate: boolean) => {
    if (busy) return;
    if (remember) usePreferences.getState().setQuitBehavior(terminate ? "terminate" : "keep");
    setBusy(terminate ? "terminate" : "keep");
    void controller.confirmQuit(terminate).finally(() => setBusy(null));
  };
  return (
    <div
      className="quit-dialog"
      aria-busy={busy !== null}
      onKeyDown={(e) => {
        if (e.key === "Escape" && !busy) controller.cancelQuit();
        if (e.key === "Tab") {
          const controls = Array.from(e.currentTarget.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled)"));
          const first = controls[0];
          const last = controls[controls.length - 1];
          if (e.shiftKey && document.activeElement === first) {
            e.preventDefault();
            last?.focus();
          } else if (!e.shiftKey && document.activeElement === last) {
            e.preventDefault();
            first?.focus();
          }
        }
      }}
    >
      <header className="quit-header">
        <span className="quit-icon" aria-hidden="true">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
            <rect x="3" y="4" width="18" height="16" rx="3" />
            <path d="m7 9 3 3-3 3m6 0h4" />
          </svg>
        </span>
        <span className="quit-count"><span aria-hidden="true" />{t("quit.running", { n: sessions })}</span>
      </header>
      <h2 id="quit-title">{t("quit.title")}</h2>
      <p id="quit-description" className="quit-description">{t("quit.description", { n: sessions })}</p>
      <div className="quit-choices">
        <button className="quit-choice" type="button" disabled={busy !== null} onClick={() => decide(false)}>
          <span className="quit-choice-copy">
            <strong>{t("quit.keep")}</strong>
            <span>{t("quit.keepHint")}</span>
          </span>
          <span className="quit-choice-arrow" aria-hidden="true">→</span>
        </button>
        <button className="quit-choice quit-choice-danger" type="button" disabled={busy !== null} onClick={() => decide(true)}>
          <span className="quit-choice-copy">
            <strong>{busy === "terminate" ? t("quit.terminating") : t("quit.terminate")}</strong>
            <span>{t("quit.terminateHint")}</span>
          </span>
          <span className="quit-choice-arrow" aria-hidden="true">{busy === "terminate" ? "…" : "→"}</span>
        </button>
      </div>
      <footer className="quit-footer">
        <label className="quit-remember">
          <input type="checkbox" checked={remember} disabled={busy !== null} onChange={(e) => setRemember(e.target.checked)} />
          <span>{t("quit.remember")}</span>
        </label>
        <button className="quit-cancel" type="button" autoFocus disabled={busy !== null} onClick={() => controller.cancelQuit()}>
          {t("quit.cancel")}
        </button>
      </footer>
    </div>
  );
}

/**
 * 끝난 에이전트 대화 일괄 재개 확인(대기열 "모두 재개"). 프로세스를 한 번에
 * 여러 개 띄우므로 개수와 대상 이름을 보이고 확인을 받는다 — 확인 없는
 * 자동 재실행은 하지 않는다(04-ui §7 "자동 재실행하지 않았습니다").
 */
function ResumeAgentsDialog({ workloadIds }: { workloadIds: string[] }): JSX.Element {
  const { t } = useI18n();
  const controller = useController();
  const workloadById = useWorkbenchStore((s) => s.workloadById);
  const panes = useWorkbenchStore((s) => s.panes);
  const workloadMemory = useWorkbenchStore((s) => s.workloadMemory);
  // 대화상자를 연 시점의 대상 그대로 — 여는 사이 목록이 바뀌어도 보인 것만 재개한다.
  const names = workloadIds.map((id) => {
    const workload = workloadById.get(id);
    return {
      id,
      title: workload ? workloadListTitle(workload, panes, workloadMemory) : id.slice(0, 8),
      agent: workload ? workloadListAgent(workload, workloadMemory) : null,
    };
  });
  const start = (): void => {
    useWorkbenchStore.getState().closeModal();
    void controller.resumeAgentWorkloads(workloadIds);
  };
  return (
    <div className="close-panes-dialog">
      <h2>{t("queue.resumeAll.title")}</h2>
      <p>{t("queue.resumeAll.description", { n: workloadIds.length })}</p>
      <ul className="resume-agents-list">
        {names.map((entry) => (
          <li key={entry.id}>
            {entry.agent ? <AgentIcon agent={entry.agent} /> : null}
            {entry.title}
          </li>
        ))}
      </ul>
      <div className="overlay-actions">
        <button type="button" autoFocus onClick={() => useWorkbenchStore.getState().closeModal()}>
          {t("terminal.close.cancel")}
        </button>
        <button type="button" onClick={start}>
          {t("queue.resumeAll.confirm")}
        </button>
      </div>
    </div>
  );
}

function ClosePanesDialog({
  leafIds,
  tabId,
  closeAllTabs = false,
}: {
  leafIds: string[];
  tabId?: string;
  closeAllTabs?: boolean;
}): JSX.Element {
  const { t } = useI18n();
  const controller = useController();
  const [remember, setRemember] = useState(false);
  const close = (terminate: boolean) => {
    if (!closeAllTabs && terminate && remember) usePreferences.getState().setTerminateOnClose(true);
    useWorkbenchStore.getState().closeModal();
    void controller.confirmClosePanes(leafIds, terminate, tabId, closeAllTabs);
  };
  return <div className="close-panes-dialog">
    <h2>{t(closeAllTabs ? "terminal.closeAll.title" : "terminal.close.title")}</h2>
    <p>{t(closeAllTabs ? "terminal.closeAll.description" : "terminal.close.description", { n: leafIds.length })}</p>
    {!closeAllTabs ? <label><input type="checkbox" checked={remember} onChange={e => setRemember(e.target.checked)} /> {t("terminal.close.remember")}</label> : null}
    <div className="overlay-actions">
      <button type="button" autoFocus onClick={() => useWorkbenchStore.getState().closeModal()}>{t("terminal.close.cancel")}</button>
      <button type="button" disabled={!closeAllTabs && remember} onClick={() => close(false)}>{t(closeAllTabs ? "terminal.closeAll.detach" : "terminal.close.detach")}</button>
      <button type="button" onClick={() => close(true)}>{t(closeAllTabs ? "terminal.closeAll.terminate" : "terminal.close.terminate")}</button>
    </div>
  </div>;
}

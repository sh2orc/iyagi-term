/**
 * QueueDrawer (04-ui.md §1·§7): 우측 drawer, 기본 닫힘.
 * - 대기열: wait_reason 문구(§7 그대로), effective priority, 순서.
 * - 관리 작업 목록: 상태, usage(cpu_cores, resident/accounted/committed을
 *   서로 다른 라벨로), root_exited badge, cancel(workload.cancel).
 * - "터미널 연결": 살아 있는 PTY는 attach, 종료된 Codex·Claude는 기록으로 재개.
 */

import { isFinishedWorkload } from "../../store/workloadState";
import { workloadListAgent, workloadListTitle } from "./workloadTitle";
import { AgentIcon } from "../terminal/AgentIcon";
import { memo } from "react";
import type { WorkloadSummary } from "../../generated/WorkloadSummary";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { rememberedWorkload } from "../../store/workloadMemory";
import { useController } from "../../app/controllerContext";
import { useI18n } from "../../i18n";
import {
  detachedRunningText,
  interruptedText,
  observeOnlyText,
  priorityText,
  exitReasonText,
  queueWaitText,
  workloadStateText,
} from "../monitor/statusStrings";
import { displayMetric, formatCores, formatGiB, parseU64 } from "../monitor/format";

/** defaults.json admission — 안전 여유 계산용. */
const HOST_RESERVE_MIN_BYTES = 2147483648;
const HOST_RESERVE_PERCENT = 0.15;

export const QueueDrawer = memo(function QueueDrawer(): JSX.Element | null {
  const { t } = useI18n();
  const open = useWorkbenchStore((s) => s.queueDrawerOpen);
  const toggle = useWorkbenchStore((s) => s.toggleQueueDrawer);
  const queue = useWorkbenchStore((s) => s.queue);
  const allWorkloads = useWorkbenchStore((s) => s.workloads);
  const workloads = allWorkloads.filter(w => !isFinishedWorkload(w.state));
  // 창을 닫은 터미널도 마지막 제목·마지막 에이전트로 부른다(workloadMemory).
  const workloadMemory = useWorkbenchStore((s) => s.workloadMemory);
  // 세션 요약 최소본(W4-3): 최근 종료 작업의 상태·종료 사유. 복구·재실행으로 새 작업이
  // 이어받은 작업은 관리 작업으로 옮겨 간 것이므로 여기서 뺀다.
  const finished = allWorkloads
    .filter(w => isFinishedWorkload(w.state) && !rememberedWorkload(workloadMemory, w.workload_id)?.recoveredBy)
    .slice(-10)
    .reverse();
  const panes = useWorkbenchStore((s) => s.panes);
  const host = useWorkbenchStore((s) => s.host);
  const controller = useController();

  if (!open) return null;

  const byId = new Map(workloads.map((w) => [w.workload_id, w]));
  const runningManaged = workloads.filter(
    (w) => w.mode === "managed" && (w.state === "RUNNING" || w.state === "STARTING"),
  ).length;
  // "모두 재개" 대상: 마지막으로 에이전트를 실행한 끝난 터미널. 기록이 없는
  // 일반 셸은 넣지 않는다 — 한 번에 새 셸을 여러 개 띄우지 않기 위해서다.
  const resumable = finished.filter((w) => workloadListAgent(w, workloadMemory) !== null);
  // "모두 연결" 대상: 개별 연결 버튼과 같은 조건(살아 있고 세션이 있으며 다른
  // 창이 붙지 않은 터미널). 앱 재시작 뒤 창 없는 실행을 한 번에 되돌린다.
  const attachable = workloads.filter(
    (w) =>
      (w.state === "RUNNING" || w.state === "STARTING") &&
      hasSession(w) &&
      !(w.session_id !== null && Object.values(panes).some((p) => p.sessionId === w.session_id)),
  );
  // 일시정지 모두 재개(08 §5)의 대상 — 단축키(⇧⌘R / Ctrl+Shift+R)와 같은 동작.
  const suspended = workloads.filter((w) => w.guard?.kind === "SUSPENDED");
  const totalBytes = parseU64(host?.physical_total_bytes.value ?? null);
  const availableBytes = parseU64(host?.physical_available_bytes.value ?? null);
  const reserve =
    totalBytes !== null ? Math.max(HOST_RESERVE_MIN_BYTES, Math.round(totalBytes * HOST_RESERVE_PERCENT)) : null;
  const safeAvailable =
    availableBytes !== null && reserve !== null ? Math.max(0, availableBytes - reserve) : null;

  return (
    <aside className="queue-drawer" aria-label={t("queue.aria.drawer")}>
      <header className="drawer-header">
        <span>{t("queue.title")}</span>
        <button type="button" aria-label={t("queue.close")} onClick={() => toggle(false)}>
          ×
        </button>
      </header>
      <ReliefPolicyToggle />
      <section className="queue-list" aria-label={t("queue.aria.list")}>
        <h3>{t("queue.count", { n: queue.length })}</h3>
        {queue.length === 0 ? <p className="muted">{t("queue.empty")}</p> : null}
        <ol>
          {queue.map((entry, index) => {
            const workload = byId.get(entry.workload_id) ?? null;
            const wait = queueWaitText(entry.wait_reason ?? workload?.queue_reason ?? null, {
              runningManaged,
              needBytes: parseU64(workload?.reservation_bytes ?? null),
              safeAvailableBytes: safeAvailable,
            });
            return (
              <li key={entry.workload_id} className="queue-entry">
                <div className="queue-entry-head">
                  <span className="queue-order">#{index + 1}</span>
                  <span className="queue-title">{workload ? workloadListTitle(workload, panes, workloadMemory) : entry.workload_id.slice(0, 8)}</span>
                  <span className="queue-priority" title={`priority ${entry.priority}`}>
                    {priorityText(entry.effective_priority)}
                  </span>
                </div>
                {wait ? <div className="queue-wait">{wait}</div> : <div className="queue-wait muted">{t("queue.pendingEval")}</div>}
                <div className="queue-entry-actions">
                  <button type="button" onClick={() => void controller.cancelWorkload(entry.workload_id)}>
                    {t("queue.cancel")}
                  </button>
                </div>
              </li>
            );
          })}
        </ol>
      </section>
      <section className="workload-list" aria-label={t("queue.aria.workloads")}>
        <div className="workload-list-head">
          <h3>{t("queue.workloadCount", { n: workloads.length })}</h3>
          <button
            type="button"
            title={t("queue.resumeAllSuspended.title")}
            disabled={suspended.length === 0}
            onClick={() => void controller.resumeAllSuspended()}
          >
            {t("queue.resumeAllSuspended", { n: suspended.length })}
          </button>
          <button
            type="button"
            title={t("queue.attachAll.title")}
            disabled={attachable.length === 0}
            onClick={() => {
              for (const workload of attachable) {
                void controller.attachWorkloadTerminal(workload.workload_id);
              }
            }}
          >
            {t("queue.attachAll", { n: attachable.length })}
          </button>
        </div>
        <ul>
          {workloads.map((w) => (
            <WorkloadRow key={w.workload_id} workload={w} />
          ))}
        </ul>
        {workloads.length === 0 ? <p className="muted">{t("queue.noWorkloads")}</p> : null}
      </section>
      {finished.length > 0 ? (
        <section className="finished-list" aria-label={t("queue.finished.section")}>
          <div className="workload-list-head">
            <h3>{t("queue.finished.section")}</h3>
            <button
              type="button"
              title={t("queue.resumeAll.title")}
              disabled={resumable.length === 0}
              onClick={() =>
                useWorkbenchStore.getState().openModal({
                  kind: "resume-agents",
                  workloadIds: resumable.map((w) => w.workload_id),
                })
              }
            >
              {t("queue.resumeAll", { n: resumable.length })}
            </button>
          </div>
          <ul>
            {finished.map((workload) => {
              const pane = Object.values(panes).find((p) => p.workloadId === workload.workload_id);
              const reason = pane?.exit ? exitReasonText(pane.exit) : null;
              // 마지막으로 실행한 에이전트가 있으면 이름 앞에 그 표시를 붙인다.
              const agent = workloadListAgent(workload, workloadMemory);
              return (
                <li key={workload.workload_id} className="finished-row">
                  <span className="workload-title" title={`${workload.program} · ${workload.cwd}`}>
                    {agent ? <AgentIcon agent={agent} /> : null}
                    {workloadListTitle(workload, panes, workloadMemory)}
                  </span>
                  <span className={`workload-state state-${workload.state.toLowerCase()}`}>
                    {workloadStateText(workload.state)}
                  </span>
                  {reason ? <span className="finished-reason">{reason}</span> : null}
                  <button
                    type="button"
                    onClick={() => void controller.attachWorkloadTerminal(workload.workload_id)}
                  >
                    {t("queue.attach")}
                  </button>
                </li>
              );
            })}
          </ul>
        </section>
      ) : null}
    </aside>
  );
});

/**
 * 자동 양보 정책 토글(08-pressure-relief §2). 끄면 새 양보만 멈춘다 — 이미
 * 양보된 세션은 압력이 풀리거나 사용자가 직접 해제할 때 돌아온다. 플랫폼이
 * 되돌릴 수 있는 양보를 지원하지 않으면(08 §0-4) 끄고 잠근 채 사유를 보여
 * 준다. 표시값은 데몬이 돌려준 정책이다(요청값이 아니다).
 */
function ReliefPolicyToggle(): JSX.Element {
  const { t } = useI18n();
  const controller = useController();
  const autoYield = useWorkbenchStore((s) => s.reliefPolicy.auto_yield);
  const support = useWorkbenchStore((s) => s.schedulingYield?.support ?? null);
  const reason = useWorkbenchStore((s) => s.schedulingYield?.reason ?? null);
  const supported = support === "supported";
  const title = supported ? t("queue.relief.autoYieldDetail") : reason ?? t("queue.relief.unsupported");
  return (
    <section className="relief-policy" aria-label={t("queue.relief.section")}>
      <label title={title}>
        <input
          type="checkbox"
          checked={supported && autoYield}
          disabled={!supported}
          onChange={(event) => void controller.setAutoYield(event.currentTarget.checked)}
        />
        {t("queue.relief.autoYield")}
      </label>
      {supported ? null : <p className="muted">{reason ?? t("queue.relief.unsupported")}</p>}
    </section>
  );
}

function WorkloadRow(props: { workload: WorkloadSummary }): JSX.Element {
  const { workload } = props;
  const { t } = useI18n();
  const controller = useController();
  const daemonPlatform = useWorkbenchStore((s) => s.daemonPlatform);
  // 붙은 터미널의 최신 제목(OSC 0/2)을 따른다 — 문자열 선택이라 제목이 바뀔 때만 다시 그린다.
  const title = useWorkbenchStore((s) => workloadListTitle(workload, s.panes, s.workloadMemory));
  // 이미 pane이 붙은 세션은 다시 연결할 수 없다(세션당 view 하나 — 컨트롤러도 거절).
  const alreadyAttached = useWorkbenchStore((s) =>
    workload.session_id !== null && Object.values(s.panes).some((p) => p.sessionId === workload.session_id),
  );
  const observeNotice = observeOnlyText(daemonPlatform ?? "", workload.enforcement);
  const detachedNotice = detachedRunningText(workload.connection, workload.state);
  const interrupted = interruptedText(workload.state);
  // 08 §5: 자원 가드 상태 — 일시정지 중이면 사유와 함께 재개 버튼을 낸다.
  const guard = workload.guard;
  const isSuspended = guard?.kind === "SUSPENDED";
  const suspendedNotice = isSuspended ? t(`queue.guard.reason.${(guard as { reason: string }).reason}`) : null;

  const sessionAttach = () => {
    // 살아 있는 세션만 pane으로 연결할 수 있다(queued는 session이 없다 — U14).
    void controller.attachWorkloadTerminal(workload.workload_id);
  };

  return (
    <li className={`workload-row state-${workload.state.toLowerCase()}`}>
      <div className="workload-head">
        <span className="workload-title" title={`${workload.program} · ${workload.cwd}`}>
          {title}
        </span>
        <span className="workload-state">{workloadStateText(workload.state)}</span>
        {workload.root_exited ? (
          <span className="badge badge-root-exited">{t("queue.rootExited")}</span>
        ) : null}
        {workload.cancel_requested ? <span className="badge">{t("queue.cancelRequested")}</span> : null}
      </div>
      <div className="workload-meta">
        <span>{priorityText(workload.priority)}</span>
        <span>{t("queue.reservation", { size: formatGiB(parseU64(workload.reservation_bytes) ?? null) })}</span>
        <span>cpu_slots {workload.cpu_slots}</span>
        <span>{workload.enforcement}</span>
      </div>
      {workload.usage ? <UsageLabels workload={workload} /> : null}
      {observeNotice ? <div className="workload-notice">{observeNotice}</div> : null}
      {detachedNotice ? <div className="workload-notice">{detachedNotice}</div> : null}
      {interrupted ? <div className="workload-notice notice-warn">{interrupted}</div> : null}
      {suspendedNotice ? <div className="workload-notice notice-warn">{suspendedNotice}</div> : null}
      <div className="workload-actions">
        {isSuspended ? (
          <button type="button" onClick={() => void controller.resumeWorkload(workload.workload_id)}>
            {t("queue.guard.resume")}
          </button>
        ) : null}
        {workload.state === "RUNNING" || workload.state === "STARTING" ? (
          <>
            <button type="button" onClick={sessionAttach} disabled={!hasSession(workload) || alreadyAttached}>
              {t("queue.attach")}
            </button>
            {isSuspended ? null : (
              <button type="button" onClick={() => void controller.suspendWorkload(workload.workload_id)}>
                {t("queue.guard.suspend")}
              </button>
            )}
            <button type="button" onClick={() => void controller.cancelWorkload(workload.workload_id)}>
              {t("queue.terminate")}
            </button>
          </>
        ) : null}
      </div>
    </li>
  );
}

function hasSession(workload: WorkloadSummary): boolean {
  return workload.connection === "attached" || workload.connection === "detached";
}

function UsageLabels(props: { workload: WorkloadSummary }): JSX.Element {
  const { usage } = props.workload;
  const { t } = useI18n();
  if (!usage) return <></>;
  const cpu = displayMetric(usage.cpu_cores, (v) => formatCores(v));
  const resident = displayMetric(usage.resident_bytes, (v) => formatGiB(parseU64(v) ?? null));
  const accounted = displayMetric(usage.accounted_bytes, (v) => formatGiB(parseU64(v) ?? null));
  const committed = displayMetric(usage.committed_bytes, (v) => formatGiB(parseU64(v) ?? null));
  const procs = displayMetric(usage.process_count, (v) => t("queue.processCount", { n: v }));
  return (
    <div className="workload-usage">
      <span title={cpu.unavailableReason ?? `${cpu.source} · ${cpu.quality}`}>CPU {cpu.text}</span>
      <span title={resident.unavailableReason ?? t("queue.titleResident")}>{t("queue.memRss")} {resident.text}</span>
      <span title={accounted.unavailableReason ?? t("queue.titleAccounted")}>
        {t("queue.cgroupAccounted")} {accounted.text}
      </span>
      <span title={committed.unavailableReason ?? t("queue.titleCommit")}>
        commit {committed.text}
      </span>
      <span title={procs.unavailableReason ?? undefined}>{t("queue.processes")} {procs.text}</span>
      <span className="muted">{usage.coverage}</span>
    </div>
  );
}

/**
 * monitor 섹션 — statusStrings/format(비-React, 호출 시점 t())와
 * ResourceStrip/GraphDrawer(컴포넌트 useI18n)의 사용자 노출 문구.
 * ko는 04-ui.md §7의 원문과 바이트 단위로 일치해야 한다
 * (compatMatrix.test.tsx가 같은 문구를 단언한다).
 */
import type { SectionMessages } from "../core";

export const ko = {
  // queueWaitText — 04-ui.md §7
  "monitor.queue.concurrency": "대기: 관리 작업 {count}개가 실행 중입니다.",
  "monitor.queue.memoryHeadroom": "대기: 새 작업에 {need}가 필요하지만 안전 여유를 제외하면 {safe}입니다.",
  "monitor.queue.telemetry": "대기: 자원 측정을 초기화하는 중입니다.",
  "monitor.queue.hostPressure": "대기: 호스트 메모리 압력이 높습니다. 회복되면 시작합니다.",
  "monitor.queue.cpuSlots": "대기: 사용 가능한 CPU 슬롯이 없습니다.",
  "monitor.queue.reservationBudget": "대기: 관리 실행 메모리 예산이 부족합니다.",
  "monitor.queue.unschedulable": "실행 불가: 요청 자원이 관리 예산보다 큽니다. 실행 조건을 조정해 주세요.",
  "monitor.queue.unknown": "대기: 사유를 확인하는 중입니다.",

  "monitor.observeOnly": "관측만 가능: 이 {platform} 실행에서는 메모리 강제 상한을 적용할 수 없습니다.",
  "monitor.detachedRunning": "화면 연결 해제됨 · 프로세스 실행 중",
  "monitor.journalPaused": "출력 읽기 일시 중지: 로그 상한 {limit}. 상한 늘리기 / 종료된 기록 관리",
  "monitor.interrupted": "실행 결과 확인 필요: 실행 관리자가 재시작되었습니다. 자동 재실행하지 않았습니다.",

  // workloadStateText
  "monitor.state.queued": "대기 중",
  "monitor.state.starting": "시작 중",
  "monitor.state.running": "실행 중",
  "monitor.state.stopping": "정지 중",
  "monitor.state.draining": "정리 중",
  "monitor.state.succeeded": "성공",
  "monitor.state.failed": "실패",
  "monitor.state.cancelled": "취소됨",
  "monitor.state.interrupted": "중단됨",

  // priorityText
  "monitor.priority.high": "높음",
  "monitor.priority.normal": "보통",
  "monitor.priority.low": "낮음",

  // panePhaseText
  "monitor.phase.starting": "시작 중",
  "monitor.phase.replaying": "화면 복원 중",
  "monitor.phase.live": "실행 중",
  "monitor.phase.failed": "실패",
  "monitor.phase.exited": "종료됨",
  "monitor.phase.detached": "연결 해제",

  "monitor.paste.tooLarge": "붙여넣기가 {max}를 초과해 거절되었습니다. 파일로 전달해 주세요.",
  "monitor.split.noSpace": "분할할 공간이 부족합니다",
  "monitor.pane.limit": "이 탭의 최대 창 수({count})에 도달했습니다",

  // format.ts — 단위·측정 불가 사유·압력
  "monitor.unit.cores": "{value}코어",
  "monitor.metric.unavailableReason": "{source}에서 측정하지 못했습니다",
  "monitor.pressure.normal": "보통",
  "monitor.pressure.warning": "경고",
  "monitor.pressure.critical": "위험",

  // ResourceStrip
  "monitor.strip.openGraph": "자원 그래프 열기(최근 5분)",
  "monitor.strip.openQueue": "작업 대기열 열기",
  // 터미널에서 자동 감지한 에이전트 요약(종류별 개수 · 작업 중 · 확인 대기).
  "monitor.strip.agentsTitle": "터미널에서 감지한 AI 에이전트 — 눌러 작업 대기열 열기",
  "monitor.strip.noAgents": "에이전트 없음",
  "monitor.strip.agentsWorking": "작업 중 {n}",
  "monitor.strip.agentsWaiting": "응답 대기 {n}",
  "monitor.strip.managed": "관리 {running} 실행 · {queued} 대기",
  "monitor.strip.pressure": "· 압력 {level}",
  // 압력 완화(08 §2): 지금 양보 중인 세션 수 — 압력 문구 바로 옆.
  "monitor.strip.yielded": "· 양보 {count}",
  // 자원 가드 일시정지(08 §5): 정지 개수 — 큐 서랍 토글에 붙는다.
  "monitor.strip.suspended": "일시정지 {n}",
  "monitor.strip.suspendedTitle": "자원 가드가 일시정지한 작업 {n}개 — 눌러 대기열을 열고 재개하세요",
  "monitor.strip.disk": "R/W {rw} · 여유 {free}",
  "monitor.strip.utilization": "사용률 {percent}%",
  "monitor.strip.diskUtilization": "저장 공간 사용률 {percent}%",
  "monitor.strip.ramBasis": "{source} · 시스템 전체 물리 메모리: 사용 / 전체. ‘사용’은 OS 기준(앱+wired+압축, 활성 모니터와 같음)이며, 재활용 가능한 캐시는 뺀 값이라 이 프로세스 하나의 메모리가 아닙니다.",
  // CPU 압력의 근거(08-pressure-relief §1.1) — tooltip 한 줄로만 노출한다.
  "monitor.strip.cpuPressureBasis": "CPU 압력: 사용 코어/전체 코어 비율 · 85% 경고 · 95% 위험 · 70% 이하 10초 지속 시 회복",
  "monitor.strip.noNetInterface": "집계 대상 네트워크 interface가 없습니다",
  "monitor.strip.noDisk": "디스크 정보 없음",
  "monitor.strip.noDiskRate": "호스트 디스크 R/W 속도는 계약에 없어 제공하지 않습니다",
  "monitor.strip.awaitingSample": "자원 샘플 대기 중",
  "monitor.quota.title": "AI 구독 사용량",
  "monitor.quota.empty": "AI 잔여량 —",
  "monitor.quota.refresh": "새로고침",
  "monitor.quota.loading": "조회 중…",
  "monitor.quota.needsSetup": "설정 필요",
  "monitor.quota.queryFailed": "조회 실패",
  "monitor.quota.noQuota": "확인 불가",
  "monitor.quota.refreshFailed": "사용량을 새로고침하지 못했습니다.",
  "monitor.quota.desktopOnly": "데스크톱 앱에서 사용량을 확인할 수 있습니다.",
  "monitor.quota.remaining": "{percent}% 남음",
  "monitor.quota.usedRemaining": "{used}% 사용 · {remaining}% 남음",
  "monitor.quota.count": "{used} / {limit} 사용",
  "monitor.quota.resets": "{time} 초기화",
  "monitor.quota.updated": "{time} 확인",
  "monitor.quota.reason.cli_not_found": "CLI를 찾지 못했습니다.",
  "monitor.quota.reason.subscription_login_required": "Codex 구독 계정 로그인이 필요합니다.",
  "monitor.quota.reason.integration_required": "설정에서 Claude 사용량 연동을 켜 주세요.",
  "monitor.quota.reason.not_configured": "설정에서 Z.ai API 키를 등록해 주세요.",
  "monitor.quota.reason.auth_failed": "API 키 인증에 실패했습니다.",
  "monitor.quota.reason.api_key_invalid": "저장된 API 키 형식을 확인해 주세요.",
  "monitor.quota.reason.provider_error": "공급자가 사용량 조회 요청을 거절했습니다.",
  "monitor.quota.reason.response_invalid": "사용량 응답을 읽지 못했습니다.",
  "monitor.quota.reason.client_error": "사용량 조회 연결을 초기화하지 못했습니다.",
  "monitor.quota.reason.cli_start_failed": "사용량 조회용 CLI를 시작하지 못했습니다.",
  "monitor.quota.reason.protocol_error": "사용량 조회용 CLI와 통신하지 못했습니다.",
  "monitor.quota.reason.no_data": "아직 사용량 데이터가 없습니다.",
  "monitor.quota.reason.network_error": "공급자에 연결하지 못했습니다.",
  "monitor.quota.reason.timeout": "조회 시간이 초과되었습니다.",
  "monitor.quota.reason.credential_store_error": "앱 전용 암호화 키 저장소를 읽지 못했습니다.",
  "monitor.quota.reason.unavailable": "현재 사용량을 확인할 수 없습니다.",

  // GraphDrawer
  "monitor.graph.aria": "자원 그래프 — 최근 5분",
  "monitor.graph.header": "최근 5분 ({samples} 샘플)",
  "monitor.graph.close": "그래프 닫기",
  "monitor.graph.awaitingSamples": "샘플 대기 중",
  "monitor.graph.cpu": "CPU (코어)",
  "monitor.graph.ram": "RAM 사용 (GiB)",
  "monitor.graph.diskFree": "Disk 여유 (GiB)",
  "monitor.graph.diskFreeMount": "Disk 여유 {mount} (GiB)",
  "monitor.graph.cannotMeasure": "{source} 측정 불가",
  "monitor.graph.noMetrics": "선택 가능한 측정 대상이 없습니다",

  // exitReasonText — 종료 사유 한 줄 (SOTA_GAP_REVIEW W1-1)
  "monitor.exit.processExit": "프로세스가 종료 코드 {code}로 끝났습니다",
  "monitor.exit.processExitNoCode": "프로세스가 종료되었습니다(코드 없음)",
  "monitor.exit.cancelled": "사용자 요청으로 취소되었습니다",
  "monitor.exit.journalLimit": "출력 저장이 먼저 멈췄습니다(전체 저널 예산 소진 또는 디스크 부족)",
  "monitor.exit.oomKill": "메모리 상한을 초과해 OS가 강제 종료했습니다",
  "monitor.exit.unknown": "종료 사유를 확인할 수 없습니다",
  "monitor.exit.detail": "근거: {detail}",
} satisfies Record<string, string>;

export const en: Record<keyof typeof ko, string> = {
  // queueWaitText — 04-ui.md §7
  "monitor.queue.concurrency": "Queued: {count} managed workload(s) running.",
  "monitor.queue.memoryHeadroom":
    "Queued: the new workload needs {need}, but only {safe} is free excluding the safety margin.",
  "monitor.queue.telemetry": "Queued: initializing resource telemetry.",
  "monitor.queue.hostPressure": "Queued: host memory pressure is high. It will start once the host recovers.",
  "monitor.queue.cpuSlots": "Queued: no CPU slots are available.",
  "monitor.queue.reservationBudget": "Queued: the managed-run memory budget is exhausted.",
  "monitor.queue.unschedulable":
    "Unschedulable: the requested resources exceed the managed budget. Please adjust the run requirements.",
  "monitor.queue.unknown": "Queued: determining the reason.",

  "monitor.observeOnly": "Observe only: a hard memory cap cannot be enforced on this {platform} host.",
  "monitor.detachedRunning": "Detached from view · process still running",
  "monitor.journalPaused": "Output reading paused: journal limit {limit}. Raise the limit / manage finished records",
  "monitor.interrupted": "Run result needs review: the run manager was restarted and did not re-run it automatically.",

  // workloadStateText
  "monitor.state.queued": "Queued",
  "monitor.state.starting": "Starting",
  "monitor.state.running": "Running",
  "monitor.state.stopping": "Stopping",
  "monitor.state.draining": "Draining",
  "monitor.state.succeeded": "Succeeded",
  "monitor.state.failed": "Failed",
  "monitor.state.cancelled": "Cancelled",
  "monitor.state.interrupted": "Interrupted",

  // priorityText
  "monitor.priority.high": "High",
  "monitor.priority.normal": "Normal",
  "monitor.priority.low": "Low",

  // panePhaseText
  "monitor.phase.starting": "Starting",
  "monitor.phase.replaying": "Restoring screen",
  "monitor.phase.live": "Live",
  "monitor.phase.failed": "Failed",
  "monitor.phase.exited": "Exited",
  "monitor.phase.detached": "Detached",

  "monitor.paste.tooLarge": "Paste rejected: it exceeds {max}. Please pass it as a file instead.",
  "monitor.split.noSpace": "Not enough space to split",
  "monitor.pane.limit": "This tab has reached its limit of {count} panes",

  // format.ts — 단위·측정 불가 사유·압력
  "monitor.unit.cores": "{value} cores",
  "monitor.metric.unavailableReason": "Failed to measure from {source}",
  "monitor.pressure.normal": "Normal",
  "monitor.pressure.warning": "Warning",
  "monitor.pressure.critical": "Critical",

  // ResourceStrip
  "monitor.strip.openGraph": "Open resource graphs (last 5 minutes)",
  "monitor.strip.openQueue": "Open workload queue",
  // Agents auto-detected in terminals (count per kind · working · waiting for you).
  "monitor.strip.agentsTitle": "AI agents detected in your terminals — click to open the workload queue",
  "monitor.strip.noAgents": "No agents",
  "monitor.strip.agentsWorking": "{n} working",
  "monitor.strip.agentsWaiting": "{n} waiting for you",
  "monitor.strip.managed": "Managed {running} running · {queued} queued",
  "monitor.strip.pressure": "· pressure {level}",
  "monitor.strip.yielded": "· yielding {count}",
  // Resource guard suspensions (08 §5): the count sits on the queue drawer toggle.
  "monitor.strip.suspended": "suspended {n}",
  "monitor.strip.suspendedTitle": "{n} workloads suspended by the resource guard — click to open the queue and resume them",
  "monitor.strip.disk": "R/W {rw} · free {free}",
  "monitor.strip.utilization": "Utilization {percent}%",
  "monitor.strip.diskUtilization": "Storage utilization {percent}%",
  "monitor.strip.ramBasis": "{source} · system-wide physical memory: used / total. ‘Used’ is the OS figure (app + wired + compressed, same as Activity Monitor), excluding reclaimable cache — not any single process's memory.",
  "monitor.strip.cpuPressureBasis": "CPU pressure: cores used / total · warning ≥85% · critical ≥95% · recovers when ≤70% for 10 s",
  "monitor.strip.noNetInterface": "No network interface available to aggregate",
  "monitor.strip.noDisk": "No disk information",
  "monitor.strip.noDiskRate": "Host disk R/W rates are not part of the contract, so they are not shown",
  "monitor.strip.awaitingSample": "Waiting for resource samples",
  "monitor.quota.title": "AI subscription usage",
  "monitor.quota.empty": "AI quota —",
  "monitor.quota.refresh": "Refresh",
  "monitor.quota.loading": "Loading…",
  "monitor.quota.needsSetup": "Setup needed",
  "monitor.quota.queryFailed": "Query failed",
  "monitor.quota.noQuota": "Unavailable",
  "monitor.quota.refreshFailed": "Could not refresh subscription usage.",
  "monitor.quota.desktopOnly": "Usage is available in the desktop app.",
  "monitor.quota.remaining": "{percent}% left",
  "monitor.quota.usedRemaining": "{used}% used · {remaining}% left",
  "monitor.quota.count": "{used} / {limit} used",
  "monitor.quota.resets": "Resets {time}",
  "monitor.quota.updated": "Checked {time}",
  "monitor.quota.reason.cli_not_found": "CLI was not found.",
  "monitor.quota.reason.subscription_login_required": "Sign in to Codex with a subscription account.",
  "monitor.quota.reason.integration_required": "Enable Claude usage integration in Settings.",
  "monitor.quota.reason.not_configured": "Add a Z.ai API key in Settings.",
  "monitor.quota.reason.auth_failed": "API key authentication failed.",
  "monitor.quota.reason.api_key_invalid": "Check the format of the saved API key.",
  "monitor.quota.reason.provider_error": "The provider rejected the usage request.",
  "monitor.quota.reason.response_invalid": "Could not read the usage response.",
  "monitor.quota.reason.client_error": "Could not initialize the usage connection.",
  "monitor.quota.reason.cli_start_failed": "Could not start the usage CLI.",
  "monitor.quota.reason.protocol_error": "Could not communicate with the usage CLI.",
  "monitor.quota.reason.no_data": "No usage data is available yet.",
  "monitor.quota.reason.network_error": "Could not reach the provider.",
  "monitor.quota.reason.timeout": "The usage query timed out.",
  "monitor.quota.reason.credential_store_error": "The app-local encrypted key store could not be read.",
  "monitor.quota.reason.unavailable": "Usage is currently unavailable.",

  // GraphDrawer
  "monitor.graph.aria": "Resource graphs — last 5 minutes",
  "monitor.graph.header": "Last 5 minutes ({samples} samples)",
  "monitor.graph.close": "Close graphs",
  "monitor.graph.awaitingSamples": "Waiting for samples",
  "monitor.graph.cpu": "CPU (cores)",
  "monitor.graph.ram": "RAM used (GiB)",
  "monitor.graph.diskFree": "Disk free (GiB)",
  "monitor.graph.diskFreeMount": "Disk free {mount} (GiB)",
  "monitor.graph.cannotMeasure": "Cannot measure {source}",
  "monitor.graph.noMetrics": "No metrics available to display",

  // exitReasonText — 종료 사유 한 줄 (SOTA_GAP_REVIEW W1-1)
  "monitor.exit.processExit": "Process exited with code {code}",
  "monitor.exit.processExitNoCode": "Process exited (no exit code)",
  "monitor.exit.cancelled": "Cancelled by user request",
  "monitor.exit.journalLimit": "Output recording stopped first (global journal budget exhausted or disk full)",
  "monitor.exit.oomKill": "The OS killed it for exceeding its memory limit",
  "monitor.exit.unknown": "Exit cause could not be determined",
  "monitor.exit.detail": "Evidence: {detail}",
};

export const monitorSection: SectionMessages = { ko, en };

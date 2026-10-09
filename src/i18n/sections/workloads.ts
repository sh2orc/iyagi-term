/**
 * workloads 섹션 — QueueDrawer · 관리 실행 대화상자 · launch 검증 문구.
 */
import type { SectionMessages } from "../core";

export const ko = {
  // QueueDrawer — drawer shell/대기열
  "queue.aria.drawer": "작업 대기열",
  "queue.title": "작업 대기열 · 관리 실행",
  "queue.close": "대기열 닫기",
  "queue.aria.list": "대기 중 작업",
  "queue.count": "대기열 ({n})",
  "queue.empty": "대기 중인 작업이 없습니다.",
  "queue.pendingEval": "평가 대기",
  "queue.cancel": "취소",

  // QueueDrawer — 압력 완화 정책(08-pressure-relief §2)
  "queue.relief.section": "압력 완화",
  "queue.relief.autoYield": "CPU 압력 시 백그라운드 세션 양보",
  "queue.guard.suspend": "일시정지",
  "queue.guard.resume": "재개",
  "queue.guard.reason.cpu_limit": "CPU 독점으로 일시정지됨(자원 가드)",
  "queue.guard.reason.memory_limit": "메모리 독점으로 일시정지됨(자원 가드)",
  "queue.guard.reason.host_memory_pressure": "시스템 메모리 부족으로 일시정지됨(자원 가드)",
  "queue.guard.reason.manual": "사용자가 일시정지했습니다",
  "queue.relief.autoYieldDetail": "CPU 압력이 높아지면 보고 있지 않은 세션의 우선순위를 낮추고, 압력이 풀리면 하나씩 되돌립니다. 끄면 새 양보만 멈춥니다.",
  "queue.relief.unsupported": "이 플랫폼에서는 양보를 적용하고 되돌릴 수 없어 자동 양보를 켤 수 없습니다.",

  // QueueDrawer — 관리 작업 목록
  "queue.aria.workloads": "관리 작업",
  "queue.workloadCount": "관리 작업 ({n})",
  "queue.noWorkloads": "등록된 작업이 없습니다.",
  "queue.rootExited": "root 종료됨 · 자손 실행 중",
  "queue.cancelRequested": "종료 요청됨",
  "queue.reservation": "예약 {size}",
  "queue.attach": "터미널 연결",
  "queue.attachAll": "모두 연결({n})",
  "queue.attachAll.title": "다른 창이 붙지 않은 살아 있는 터미널을 한 번에 다시 연결합니다",
  "queue.resumeAllSuspended": "일시정지 모두 재개({n})",
  "queue.resumeAllSuspended.title": "자원 가드가 일시정지한 작업을 한 번에 재개합니다",
  "queue.resumeAll": "모두 재개({n})",
  "queue.resumeAll.title": "끝난 에이전트 대화를 한 번에 이어서 엽니다. 기록이 없는 일반 터미널은 제외합니다",
  "queue.resumeAll.description": "에이전트 대화 {n}개를 이어서 엽니다. 대화마다 CLI 프로세스가 하나씩 새로 시작합니다.",
  "queue.resumeAll.confirm": "재개",
  "queue.resumeAll.done": "{n}개를 이어서 열었습니다.",
  "queue.resumeAll.partial": "{n}개를 이어서 열었고 {failed}개는 열지 못했습니다(기록이 없거나 경로가 사라졌습니다).",
  "queue.terminate": "작업 종료 후 닫기",

  // QueueDrawer — usage 라벨/tooltip
  "queue.memRss": "메모리(RSS)",
  "queue.cgroupAccounted": "cgroup 계상",
  "queue.titleResident": "resident(RSS) 관측값",
  "queue.titleAccounted": "Linux cgroup 계상값 — RSS와 다른 라벨",
  "queue.titleCommit": "Windows job commit — RSS와 다른 라벨",
  "queue.processes": "프로세스",
  "queue.processCount": "{n}개",

  // 관리 실행 대화상자 — 머리/프로필
  "managed.title": "관리 실행",
  "managed.noDaemon": "데몬 연결이 구성되지 않았습니다(setManagedRunDeps 필요).",
  "managed.profile": "프로필",
  "managed.profileAria": "실행 프로필",
  "managed.noProgram": "프로그램 미설정",
  "managed.noProfiles": "프로필 없음",

  // 관리 실행 대화상자 — direct shell 안내
  "managed.guide.title": "일반 실행 안내(direct shell)",
  "managed.guide.body":
    "일반 터미널에서 codex/claude/opencode를 직접 입력해도 사용할 수 있습니다. 단, 일반 셸의 자원 관측은 session 단위이므로 같은 셸의 다른 프로세스(dev server 등)가 포함될 수 있습니다. 관리 실행은 대상별 전용 PTY와 OS 그룹으로 더 명확한 attribution을 제공합니다. Windows에서 자동 발견된 .cmd/.bat/.ps1 shim은 일반 셸에서 실행하거나 프로필을 interpreter 형태로 등록하세요.",

  // 관리 실행 대화상자 — cwd/argv
  "managed.cwdLabel": "작업 디렉터리(절대 경로 — 존재 확인은 실행 시)",
  "managed.cwdAria": "작업 디렉터리",
  "managed.argvLegend": "인수",
  "managed.fixedPrefix": "프로필 고정 prefix(변경 불가)",
  "managed.extraArgs": "추가 인수(한 줄에 하나, 공백 포함 가능)",
  "managed.extraArgsAria": "추가 인수",
  "managed.argvBudget": "{count}/{maxCount}개 · {bytes}/{maxBytes}바이트",
  "managed.effectiveCommand": "실행 형태:",

  // 관리 실행 대화상자 — 우선순위/풋터/액션
  "managed.priority": "우선순위",
  "managed.priorityHigh": "높음(0)",
  "managed.priorityNormal": "보통(1)",
  "managed.priorityLow": "낮음(2)",
  "managed.queueNote":
    "대기 중에는 PTY/CLI를 미리 만들지 않습니다. 실행 후 큐 → 입장 허용 → 터미널 연결 순서다.",
  "managed.editProfile": "프로필 편집",
  "managed.compatMatrix": "호환성 매트릭스",
  "managed.cancel": "취소",
  "managed.submit": "실행 등록",
  "managed.queuedToast": "대기열에 등록했습니다 — 입장 허용 시 터미널에 연결합니다.",
  "managed.reservationFitted": "예약 메모리가 이 컴퓨터의 관리 실행 예산보다 커서 {size}로 줄여 등록했습니다",

  // 관리 실행 대화상자 — 오류/toast
  "managed.errorCapabilities": "capability 조회 실패",
  "managed.errorCapabilityUnavailable":
    "실행 불가: 요청한 상한을 이 실행에서 적용할 수 없습니다(CAPABILITY_UNAVAILABLE). {message}",
  "managed.errorLaunchFailed": "실행 등록 실패({code}): {message}",
  "managed.errorGeneric": "실행 등록에 실패했습니다.",
  "managed.claudeProviderRouted": "이 Claude Code 실행은 Z.ai Coding Plan(GLM)으로 라우팅됩니다 — 주 모델 {model}. 설정 → 연동에서 바꿀 수 있습니다.",

  // launchComposer — 검증 문구
  "launch.cwdRequired": "작업 디렉터리를 입력하세요(절대 경로).",
  "launch.reservationMin": "예약 메모리는 최소 256 MiB({minBytes}바이트)여야 합니다.",
  "launch.cpuSlotsMin": "cpu_slots는 1 이상의 정수여야 합니다.",
  "launch.claudeProviderEnvConflict": "프로필 환경 변수 {keys}은(는) Z.ai 라우팅과 충돌합니다 — 항목을 지우거나 설정 → Z.ai Coding Plan에서 라우팅을 끄세요.",
  "queue.finished.section": "최근 종료",

} satisfies Record<string, string>;

export const en: Record<keyof typeof ko, string> = {
  // QueueDrawer — drawer shell/queue
  "queue.aria.drawer": "Workload queue",
  "queue.title": "Workload queue · Managed runs",
  "queue.close": "Close queue",
  "queue.aria.list": "Queued workloads",
  "queue.count": "Queue ({n})",
  "queue.empty": "No workloads are waiting.",
  "queue.pendingEval": "Awaiting evaluation",
  "queue.cancel": "Cancel",

  // QueueDrawer — pressure relief policy (08-pressure-relief §2)
  "queue.relief.section": "Pressure relief",
  "queue.relief.autoYield": "Yield background sessions under CPU pressure",
  "queue.guard.suspend": "Suspend",
  "queue.guard.resume": "Resume",
  "queue.guard.reason.cpu_limit": "Suspended by the resource guard (CPU monopoly)",
  "queue.guard.reason.memory_limit": "Suspended by the resource guard (memory monopoly)",
  "queue.guard.reason.host_memory_pressure": "Suspended by the resource guard (system memory pressure)",
  "queue.guard.reason.manual": "Suspended by you",
  "queue.relief.autoYieldDetail": "Lowers the priority of sessions you are not watching while CPU pressure is high, then restores them one by one once it clears. Turning it off only stops new yields.",
  "queue.relief.unsupported": "This platform cannot apply and undo scheduling yield, so auto-yield stays off.",

  // QueueDrawer — managed workload list
  "queue.aria.workloads": "Managed workloads",
  "queue.workloadCount": "Managed workloads ({n})",
  "queue.noWorkloads": "No workloads registered.",
  "queue.rootExited": "Root exited · children still running",
  "queue.cancelRequested": "Termination requested",
  "queue.reservation": "Reserved {size}",
  "queue.attach": "Attach terminal",
  "queue.attachAll": "Connect all ({n})",
  "queue.attachAll.title": "Reattach every live terminal that no window is attached to, in one click",
  "queue.resumeAllSuspended": "Resume all suspended ({n})",
  "queue.resumeAllSuspended.title": "Resume every workload the resource guard suspended, in one click",
  "queue.resumeAll": "Resume all ({n})",
  "queue.resumeAll.title": "Reopen finished agent conversations in one click. Plain terminals without a record are skipped",
  "queue.resumeAll.description": "Reopens {n} agent conversations. Each one starts its own CLI process.",
  "queue.resumeAll.confirm": "Resume",
  "queue.resumeAll.done": "Reopened {n}.",
  "queue.resumeAll.partial": "Reopened {n}; {failed} could not be reopened (no record, or the directory is gone).",
  "queue.terminate": "Terminate workload",

  // QueueDrawer — usage labels/tooltips
  "queue.memRss": "Memory (RSS)",
  "queue.cgroupAccounted": "cgroup accounted",
  "queue.titleResident": "Observed resident (RSS) value",
  "queue.titleAccounted": "Linux cgroup accounting — a separate label from RSS",
  "queue.titleCommit": "Windows job commit — a separate label from RSS",
  "queue.processes": "Processes",
  "queue.processCount": "{n}",

  // Managed run dialog — header/profile
  "managed.title": "Managed run",
  "managed.noDaemon": "Daemon connection is not configured (setManagedRunDeps required).",
  "managed.profile": "Profile",
  "managed.profileAria": "Run profile",
  "managed.noProgram": "No program set",
  "managed.noProfiles": "No profiles",

  // Managed run dialog — direct shell guide
  "managed.guide.title": "Direct shell guide",
  "managed.guide.body":
    "You can also run codex/claude/opencode directly in a plain terminal. Note that resource accounting in a plain shell is session-scoped, so other processes in the same shell (such as a dev server) are included. Managed runs provide clearer attribution with a dedicated PTY and OS process group. On Windows, run auto-discovered .cmd/.bat/.ps1 shims in a plain shell, or register the profile in interpreter form.",

  // Managed run dialog — cwd/argv
  "managed.cwdLabel": "Working directory (absolute path — existence checked at launch)",
  "managed.cwdAria": "Working directory",
  "managed.argvLegend": "Arguments",
  "managed.fixedPrefix": "Profile-fixed prefix (locked)",
  "managed.extraArgs": "Additional arguments (one per line, spaces allowed)",
  "managed.extraArgsAria": "Additional arguments",
  "managed.argvBudget": "{count}/{maxCount} args · {bytes}/{maxBytes} bytes",
  "managed.effectiveCommand": "Effective command:",

  // Managed run dialog — priority/footer/actions
  "managed.priority": "Priority",
  "managed.priorityHigh": "High (0)",
  "managed.priorityNormal": "Normal (1)",
  "managed.priorityLow": "Low (2)",
  "managed.queueNote":
    "While queued, no PTY/CLI is created up front. After launch: queue → admission → terminal attach.",
  "managed.editProfile": "Edit profile",
  "managed.compatMatrix": "Compatibility matrix",
  "managed.cancel": "Cancel",
  "managed.submit": "Launch",
  "managed.queuedToast": "Added to the queue — the terminal will attach once the run is admitted.",
  "managed.reservationFitted": "The memory reservation was larger than this computer's managed budget — registered with {size}",

  // Managed run dialog — errors/toast
  "managed.errorCapabilities": "Failed to fetch capabilities",
  "managed.errorCapabilityUnavailable":
    "Cannot launch: the requested limits cannot be enforced in this session (CAPABILITY_UNAVAILABLE). {message}",
  "managed.errorLaunchFailed": "Launch registration failed ({code}): {message}",
  "managed.errorGeneric": "Failed to register the launch.",
  "managed.claudeProviderRouted": "This Claude Code run is routed through the Z.ai Coding Plan (GLM) — main model {model}. Change it under Settings → Integrations.",

  // launchComposer — validation messages
  "launch.cwdRequired": "Enter a working directory (absolute path).",
  "launch.reservationMin": "Reserved memory must be at least 256 MiB ({minBytes} bytes).",
  "launch.cpuSlotsMin": "cpu_slots must be an integer of 1 or greater.",
  "launch.claudeProviderEnvConflict": "Profile environment variable(s) {keys} conflict with Z.ai routing — remove them or turn routing off under Settings → Z.ai Coding Plan.",
  "queue.finished.section": "Recently finished",

};

export const workloadsSection: SectionMessages = { ko, en };

//! Journal retention release loop (SOTA_GAP_REVIEW W1-2, 최소 버전).
//!
//! 전역 저널 예산(2 GiB, `GlobalJournalBudget`)이 종료된 세션의 저널을
//! 영구히 점유하던 결함의 마감: `journal_retention_days`(기본 7일)보다
//! 오래 마지막으로 쓰인 저널 중 다음 조건을 모두 만족하는 것을 골라
//! 파일을 삭제하고 예산을 방출한다.
//!
//! - 소유 워크로드가 종료 상태(SUCCEEDED/FAILED/CANCELLED/INTERRUPTED)
//! - `pinned = 0` (사용자 고정 저널은 절대 지우지 않는다)
//! - actor가 아직 살아 있거나 뷰가 붙어 있는 세션이 아님(재생/재연결 중
//!   삭제 금지). 예전에는 "레지스트리에 있으면 무조건 건너뜀"이었는데,
//!   레지스트리가 삭제 경로 없이 자라기만 하던 시절 이 조건은 데몬 자신의
//!   세션 전부를 영구히 제외시켜 전역 예산이 한 번도 방출되지 않았다.
//!
//! 삭제 판정은 파일의 **마지막 쓰기 시각**(mtime)을 기준으로 한다 —
//! 세션 생성 시각이 아니라 실제 출력이 멈춘 시각이 보존 기간의 기준이다.
//! DB 행이 없거나 이미 'deleted'로 닫힌 세션의 남은 파일(고아 런)은 어떤
//! RPC로도 닿을 수 없으므로 같은 기준으로 지운다.
//!
//! **공간 압력.** 기동 시 전역 예산은 디스크에 남은 저널 바이트로 시드된다.
//! 이전 세대 데몬은 매번 0에서 시작해 상한을 넘겨 쌓았을 수 있으므로, 시드된
//! 예산이 처음부터 상한에 닿으면 새 저널의 헤더조차 예약되지 않아 모든 세션
//! 시작이 실패한다. 그래서 사용량이 높은 수위(상한의 90%)를 넘으면 보존
//! 기간과 무관하게 가장 오래된 것부터(고아 런 먼저, 그다음 종료·비고정·
//! 비활성 세션) 지워 낮은 수위(80%)까지 내린다. 세션을 받기 전 기동 경로에서
//! 한 번, 이후 이 스레드가 1분마다 확인한다.
//!
//! **방출 단위.** 예산 방출은 항상 이번에 실제로 지운 파일의 디스크
//! 바이트다. 시드는 파일 크기를 세고 writer는 쓴 만큼 예약하므로, DB의
//! `journal_bytes`(마지막 동기화 값)를 방출하면 단위가 섞여 예산이 조용히
//! 어긋난다. 이미 없는 파일에 대한 방출은 0이다.
//!
//! 디스크 여유 협상·정책 UI는 Wave 2가 맡는다.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use term_contracts::ids::SessionId;
use term_storage::SessionRecord;

use crate::state::DaemonState;

/// 스윕 주기(1시간)와 시작 지연(30초 — 시작 직후 다른 초기화와 경합하지
/// 않게 한다).
const TICK: Duration = Duration::from_secs(60 * 60);
const INITIAL_DELAY: Duration = Duration::from_secs(30);
/// 스윕 사이의 공간 압력 확인 주기. 수위 아래면 잠금 한 번으로 끝난다.
const PRESSURE_CHECK: Duration = Duration::from_secs(60);
/// 고정·활성 저널뿐이라 수위 아래로 내리지 못한 뒤의 재확인 주기. 그사이
/// 끝난 세션이 축출 대상이 될 수 있으므로 다음 스윕(1시간)까지 기다리지
/// 않는다 — 기다리면 그동안 새 저널 헤더 예약이 모두 JOURNAL_LIMIT로
/// 실패한다. 매분이 아니라 몇 분 간격인 것은 실패 경고 로그를 줄이기 위해서다.
const PRESSURE_STUCK_RECHECK: Duration = Duration::from_secs(5 * 60);
/// 전역 예산 사용량이 상한의 이 비율(%)을 넘으면 공간 압력 축출을 시작한다.
const PRESSURE_HIGH_PERCENT: u64 = 90;
/// 축출은 이 비율(%)까지 내린다 — 수위 바로 아래에서 멈춰 매분 다시 넘는
/// 진동을 막고, 새 세션이 쓸 여유를 남긴다.
const PRESSURE_LOW_PERCENT: u64 = 80;
/// 데이터 볼륨의 실제 남은 바이트 하한. 이보다 적으면 정기 스윕과 무관하게
/// 즉시 정리를 돈다(전체 다운 사고의 실제 방아쇠였던 디스크 가득 참 대응).
const DISK_HEADROOM_FLOOR_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Supervised loop body (`supervisor::spawn_supervised`).
pub fn run(state: Arc<DaemonState>) {
    // 종료 신호 수신기는 루프 밖에서 한 번만 만든다(재시작마다 새로 만든다).
    let shutdown = state.shutdown.subscribe();
    std::thread::sleep(INITIAL_DELAY);
    while !*shutdown.borrow() {
        sweep(&state);
        // 고정·활성 저널뿐이라 수위 아래로 내리지 못했으면 재확인 간격만
        // 늘린다(매분 같은 열거와 경고를 반복하지 않게). 멈추지는 않는다:
        // 그사이 끝난 세션은 축출 대상이 된다.
        let mut pressure_stuck = !relieve_space_pressure(&state);
        // 종료 신호를 TICK 전에 반응할 수 있게 1초씩 잘게 잔다.
        for second in 1..=TICK.as_secs() {
            if *shutdown.borrow() {
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
            let every = if pressure_stuck {
                PRESSURE_STUCK_RECHECK
            } else {
                PRESSURE_CHECK
            };
            if second.is_multiple_of(every.as_secs()) && second < TICK.as_secs() {
                pressure_stuck = !relieve_space_pressure(&state);
                relieve_disk_headroom(&state);
            }
        }
    }
}

/// 한 번의 정리 사이클. 개별 항목 실패는 건너뛰고 계속한다 — retention는
/// 스스로 데몬을 멈추지 않는다.
fn sweep(state: &Arc<DaemonState>) {
    let days = state.config.journal_retention_days();
    let Some(cutoff) = SystemTime::now().checked_sub(Duration::from_secs(u64::from(days) * 86_400))
    else {
        return;
    };
    let candidates = match state.storage.terminal_unpinned_sessions() {
        Ok(list) => list,
        Err(error) => {
            tracing::warn!(%error, "journal retention query failed");
            return;
        }
    };
    // 저널 디렉터리는 스윕마다 한 번만 열거한다(세션마다 열거하면
    // O(세션 × 파일)이다).
    let runs = journal_runs(&state.paths.journals_dir());
    let mut removed = 0usize;
    let mut released = 0u64;
    for record in &candidates {
        let run = runs.get(record.id.as_str());
        if keep_for_retention(run, cutoff) {
            continue;
        }
        if let Some(bytes) = evict_session(state, record, run) {
            removed += 1;
            released += bytes;
        }
    }
    // 고아 런도 같은 보존 기간을 따른다.
    for (id, run) in orphan_runs(state, &runs, &candidates) {
        if !keep_for_retention(Some(run), cutoff) {
            removed += 1;
            released += evict_orphan(state, id, run);
        }
    }
    if removed > 0 {
        tracing::info!(
            removed,
            released_bytes = released,
            "journal retention sweep done"
        );
    }
    // lifecycle_events도 같은 틱에 정리한다(W2): 최근 5,000건만 유지.
    if let Err(error) = state.storage.prune_lifecycle_events(LIFECYCLE_KEEP_RECENT) {
        tracing::warn!(%error, "lifecycle_events prune failed");
    }
    // 에이전트 세션 목록도 같은 틱에 정리한다(02 §8).
    match state
        .storage
        .prune_agent_sessions(AGENT_SESSION_MAX_AGE_DAYS, AGENT_SESSION_KEEP_AT_MOST)
    {
        Ok(0) => {}
        Ok(pruned) => tracing::info!(pruned, "agent sessions pruned"),
        Err(error) => tracing::warn!(%error, "agent_sessions prune failed"),
    }
    // 미션 오케스트레이션 테이블도 같은 틱에 정리한다.
    prune_orchestration(state);
}

/// 미션 오케스트레이션 테이블 정리: housekeeping 이벤트는 미션별 꼬리만,
/// dedupe 요청은 보존 기간만 남긴다. 개수가 많으면 한 번 VACUUM으로 파일
/// 크기까지 돌려준다(성공 1회).
fn prune_orchestration(state: &Arc<DaemonState>) {
    match state
        .storage
        .prune_mission_retention(MISSION_EVENT_TAIL, MISSION_REQUEST_MAX_AGE_DAYS)
    {
        Ok(pruned) => {
            let removed = pruned.housekeeping_events + pruned.requests;
            if removed > 0 {
                tracing::info!(
                    events = pruned.housekeeping_events,
                    requests = pruned.requests,
                    "orchestration retention prune done"
                );
            }
            if removed >= VACUUM_TRIGGER_ROWS && vacuum_once(state) {
                tracing::info!("orchestration tables vacuumed after heavy prune");
            }
        }
        Err(error) => tracing::warn!(%error, "orchestration retention prune failed"),
    }
}

/// 데이터 디렉터리가 속한 볼륨의 남은 바이트(최신 호스트 표본, 마운트
/// 접두어 일치). 디스크 용량은 10초 주기로 새로 고친다(sampler).
fn disk_free_bytes(state: &DaemonState) -> Option<u64> {
    let sample = state
        .host
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .0
        .clone()?;
    let target = state.paths.journals_dir();
    sample
        .disks
        .iter()
        .filter(|disk| target.starts_with(Path::new(&disk.mount)))
        .max_by_key(|disk| disk.mount.len())
        .and_then(|disk| disk.free_bytes.value.as_ref().map(|v| v.get()))
}

/// 실제 디스크 여유가 바닥 이하로 떨어지면 시간당 스윕을 기다리지 않고
/// 즉시 반응한다: 저널 연령 정리 + 오케스트레이션 정리를 돌리고 경고한다.
/// 살아 있는 세션은 절대 건드리지 않는다(삭제 판정은 [`sweep`]의 규칙:
/// 종료·비고정·비활성). 이 반응이 디스크를 다 채우지는 못해도 증가를
/// 멈추고 다음 스윕까지의 창을 벌어 준다.
pub(crate) fn relieve_disk_headroom(state: &Arc<DaemonState>) {
    let Some(free) = disk_free_bytes(state) else {
        return;
    };
    if free >= DISK_HEADROOM_FLOOR_BYTES {
        return;
    }
    tracing::warn!(
        free_bytes = free,
        floor_bytes = DISK_HEADROOM_FLOOR_BYTES,
        "disk headroom below floor — running an early retention pass"
    );
    sweep(state);
    prune_orchestration(state);
}

/// 데몬 수명당 한 번의 VACUUM. 읽기 풀과의 경합으로 BUSY가 나면 실패로
/// 돌려주고 다음 스윕이 다시 시도한다(플래그는 성공 시에만 세운다).
fn vacuum_once(state: &DaemonState) -> bool {
    if state.db_vacuumed.swap(true, Ordering::SeqCst) {
        return false;
    }
    state.storage.vacuum().is_ok()
}

/// 공간 압력 축출(모듈 문서 참고). 전역 예산 사용량이 높은 수위를 넘으면
/// 보존 기간과 무관하게 가장 오래된 것부터 지워 낮은 수위까지 내린다. 고정
/// 저널과 살아 있거나 누군가 보고 있는 세션은 절대 지우지 않는다. 호출이
/// 끝났을 때 사용량이 높은 수위 이하면 true.
pub fn relieve_space_pressure(state: &DaemonState) -> bool {
    let (used, limit) = budget_usage(state);
    let high_water = watermark(limit, PRESSURE_HIGH_PERCENT);
    if used <= high_water {
        return true;
    }
    let low_water = watermark(limit, PRESSURE_LOW_PERCENT);
    let runs = journal_runs(&state.paths.journals_dir());
    let candidates = match state.storage.terminal_unpinned_sessions() {
        Ok(list) => list,
        Err(error) => {
            // 고아 런은 행 조회로 따로 가려내므로 여전히 지울 수 있다.
            tracing::warn!(%error, "journal space pressure: candidate query failed");
            Vec::new()
        }
    };
    let mut victims = Vec::new();
    for (id, run) in orphan_runs(state, &runs, &candidates) {
        if run.readable {
            victims.push(Victim {
                last_write: run.last_write,
                target: VictimTarget::Orphan(id, run),
            });
        }
    }
    for record in &candidates {
        // 파일이 없는 세션은 공간을 돌려주지 않는다(회계 마감은 스윕 몫이다).
        // 메타데이터를 읽을 수 없는 런은 지우지 않는다(보수적).
        let Some(run) = runs.get(record.id.as_str()) else {
            continue;
        };
        if run.readable {
            victims.push(Victim {
                last_write: run.last_write,
                target: VictimTarget::Session(record, Some(run)),
            });
        }
    }
    sort_victims(&mut victims);
    let mut evicted = 0usize;
    let mut released = 0u64;
    for victim in &victims {
        if budget_usage(state).0 <= low_water {
            break;
        }
        let bytes = match victim.target {
            VictimTarget::Orphan(id, run) => Some(evict_orphan(state, id, run)),
            VictimTarget::Session(record, run) => evict_session(state, record, run),
        };
        if let Some(bytes) = bytes {
            evicted += 1;
            released += bytes;
        }
    }
    let (after, _) = budget_usage(state);
    tracing::info!(
        used_before = used,
        used_after = after,
        limit,
        evicted,
        released_bytes = released,
        "journal space pressure relief"
    );
    if after > high_water {
        tracing::warn!(
            used = after,
            limit,
            "journal budget stays above its high-water mark (pinned or live journals)"
        );
        return false;
    }
    true
}

/// 기동 시 전역 예산 시드: 이 모듈이 관리하는 저널 파일(`journal_runs`)의
/// 디스크 바이트 합. 시드와 방출이 같은 파일 집합, 같은 단위를 쓴다.
pub fn journal_bytes_on_disk(dir: &Path) -> u64 {
    journal_runs(dir).values().map(|run| run.bytes).sum()
}

/// lifecycle_events 유지 상한(W2).
const LIFECYCLE_KEEP_RECENT: i64 = 5_000;

/// 미션 housekeeping 이벤트(engine.time_checkpoint·engine.activity)를
/// 미션 head 기준 이만큼의 revision 안쪽(꼬리)만 남긴다. 그 밖의 오래된
/// housekeeping 행은 감사 가치가 없는 반복 갱신이라 매 스윕 지운다.
/// housekeeping이 아닌 이벤트는 개수·나이와 무관하게 절대 지우지 않는다.
/// 결정 대기 등으로 정체된 미션이 초당 한 번의 커밋을 무한히 쌓던 결함의
/// 저장층 마감(감쇠는 timing.rs의 IDLE_CHECKPOINT가 담당).
const MISSION_EVENT_TAIL: i64 = 200;

/// 미션 요청 멱등(dedupe) 행의 보존 기간(일). 이 창을 넘은 재생은 그냥
/// CAS 규칙 아래 재실행될 뿐이라 행은 죽은 무게다.
const MISSION_REQUEST_MAX_AGE_DAYS: u32 = 14;

/// 이보다 많은 행을 한 번의 정리에서 지웠으면(누적 기준) 파일 크기를
/// 되돌리기 위해 VACUUM을 한 번 시도한다. DELETE는 페이지를 재사용
/// 가능하게만 할 뿐 디스크를 돌려주지 않는다. VACUUM은 쓰기 연결을
/// 오래 잡으므로 데몬 수명당 성공 1회로 제한한다.
const VACUUM_TRIGGER_ROWS: u64 = 50_000;

/// 에이전트 세션 기록 보존 기간(일). 종료된 대화를 90일 뒤에 목록에서
/// 지운다 — 그보다 오래된 대화를 "이어서 열기"로 되살리는 일은 사실상
/// 없고, 그 기록은 에이전트 쪽에도 남아 있다(우리 행은 색인일 뿐이다).
/// 아직 살아 있는(ended_at IS NULL) 행은 나이와 무관하게 남는다.
const AGENT_SESSION_MAX_AGE_DAYS: u32 = 90;

/// 나이와 무관한 개수 상한. 목록은 한 화면에 쓰는 것이고 전체 응답은
/// 64 KiB 프레임 예산 안에 들어가야 하므로(01 §2), 무한히 쌓이게 두지
/// 않는다. `last_seen_at` 최신 1,000건만 남긴다. 밀려난 행이라도 **아직
/// 열려 있으면 지우지 않는다**(스토리지 `prune_agent_sessions` 참고).
const AGENT_SESSION_KEEP_AT_MOST: u32 = 1_000;

/// 이 세션을 스윕에서 건너뛸 것인가. `live`는 데몬 레지스트리에 항목이
/// 있을 때 `(actor 종료됨, 뷰가 붙어 있음)`이고, 없으면 `None`이다.
/// 건너뛰는 경우는 딱 둘 — actor가 아직 살아 있거나, 누군가 보고 있는 중.
fn skip_live_session(live: Option<(bool, bool)>) -> bool {
    match live {
        None => false,
        Some((actor_finalized, has_views)) => !actor_finalized || has_views,
    }
}

/// 보존 기간 때문에 이 런을 남겨야 하는가.
fn keep_for_retention(run: Option<&DiskRun>, cutoff: SystemTime) -> bool {
    match run {
        // 파일이 하나도 없다(외부 삭제 / 지난 스윕의 마킹 실패): 회계만 닫는다.
        None => false,
        // 메타데이터를 읽을 수 없으면 지우지 않는다(보수적).
        Some(run) if !run.readable => true,
        // 마지막 쓰기(런에서 가장 새 파일의 mtime)가 보존 기간 안이다.
        Some(run) => run.last_write.is_some_and(|time| time > cutoff),
    }
}

/// 세션 하나의 저널 런을 지우고 회계를 닫는다. 이번에 실제로 지운 파일의
/// 디스크 바이트를 방출하고, 런이 다 사라졌으면 그 값을 돌려준다. 살아
/// 있거나 누군가 보고 있거나 방금 다시 붙은 세션은 건드리지 않는다(`None`).
/// 일부 파일을 지우지 못하면 지운 만큼만 방출하고 행은 열어 둔다(`None`) —
/// 다음 스윕이 남은 파일을 다시 시도한다.
fn evict_session(
    state: &DaemonState,
    record: &SessionRecord,
    run: Option<&DiskRun>,
) -> Option<u64> {
    // 아직 살아 있는 actor의 세션이나 뷰가 붙어 있는 세션(재생·재연결
    // 중)은 절대 지우지 않는다. 종료했고 아무도 보고 있지 않은 세션은
    // 레지스트리에 남아 있어도 대상이다 — 그러지 않으면 데몬 자신의
    // 세션은 영원히 후보에서 빠지고 전역 예산이 방출되지 않는다.
    let live = state.session(&record.id).map(|session| {
        (
            session.actor_finalized.load(Ordering::Acquire),
            !session
                .views
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_empty(),
        )
    });
    if skip_live_session(live) {
        return None;
    }
    // 파일을 지우기 *전에* 보관 링·레지스트리에서 뺀다. 지운 뒤에
    // 빼면 그 사이 attach된 뷰가 파일 없는 항목에 붙어 20 ms마다 read
    // 실패를 반복한다. 아직 등록돼 있으면(방금 attach됨) 이번 스윕은
    // 건너뛴다 — 다음 스윕이 다시 본다.
    state.forget_session(&record.id);
    if state.session(&record.id).is_some() {
        return None;
    }
    // 롤링 저널은 파일 여러 개(활성 + 닫힌 세그먼트)다 — 전부 지운다. 이번
    // 데몬이 돌린 세션(레지스트리에 있던 것)은 열거 뒤에도 회전했을 수 있어
    // 다시 열거하고, 이전 세대의 세션은 열거해 둔 목록을 그대로 쓴다. 그래도
    // 남은 파일은 행이 'deleted'로 닫힌 뒤 고아 런으로 정리된다.
    let base = state.paths.journal(record.id.as_str());
    let files = match (live, run) {
        (Some(_), _) => term_pty::segments::journal_files(&base),
        (None, Some(run)) => run.files.clone(),
        (None, None) => Vec::new(),
    };
    let removal = remove_run_files(&files);
    // 디스크에서 사라진 바이트는 마킹 결과와 무관하게 지금 방출한다. 다음
    // 스윕은 그 파일을 찾지 못해 0을 방출하므로 같은 바이트가 두 번 나가지
    // 않는다(저널 하나당 정확히 한 번).
    release_journal_bytes(state, removal.bytes);
    if !removal.complete {
        return None;
    }
    // replay_status='deleted'로 닫아야 이 세션이 다음 스윕의 후보에서 빠진다.
    // 마킹이 실패하면 다음 스윕이 다시 시도한다 — 그때 방출할 바이트는 없다.
    if let Err(error) = state.storage.mark_journal_deleted(&record.id) {
        tracing::warn!(session = %record.id, %error, "journal retention: mark failed");
    }
    tracing::info!(
        session = %record.id,
        bytes = removal.bytes,
        files = files.len(),
        "journal released by retention"
    );
    Some(removal.bytes)
}

/// 고아 런(`orphan_runs`)의 파일을 지우고 실제로 지운 바이트를 방출한다.
fn evict_orphan(state: &DaemonState, id: &str, run: &DiskRun) -> u64 {
    let removal = remove_run_files(&run.files);
    release_journal_bytes(state, removal.bytes);
    tracing::info!(
        session = id,
        bytes = removal.bytes,
        complete = removal.complete,
        "orphan journal files removed"
    );
    removal.bytes
}

/// 디렉터리에는 있지만 되살릴 길이 없는 런: DB 행이 없거나, 행은 이미
/// 'deleted'로 닫혔는데 파일이 남은 세션. 어떤 RPC도 이 바이트에 닿지
/// 못하지만 기동 시드는 셌으므로, 지우지 않으면 예산을 영구히 점유한다.
/// 후보(`candidates`)나 레지스트리에 있는 세션, 고정된 행, 조회가 실패한
/// 런은 고아가 아니다. 세션 행은 발사 의도 트랜잭션에서 저널 파일보다 먼저
/// 만들어지고 지워지지 않으므로, 행이 없는 파일이 지금 쓰이는 저널일 수는
/// 없다.
fn orphan_runs<'a>(
    state: &DaemonState,
    runs: &'a BTreeMap<String, DiskRun>,
    candidates: &[SessionRecord],
) -> Vec<(&'a str, &'a DiskRun)> {
    let known: HashSet<&str> = candidates.iter().map(|r| r.id.as_str()).collect();
    let mut orphans = Vec::new();
    for (id, run) in runs {
        if known.contains(id.as_str()) {
            continue;
        }
        let Ok(session_id) = SessionId::parse(id) else {
            continue;
        };
        if state.session(&session_id).is_some() {
            continue;
        }
        let orphan = match state.storage.session(&session_id) {
            Ok(None) => true,
            Ok(Some(row)) => row.replay_status == "deleted" && !row.pinned,
            Err(_) => false,
        };
        if orphan {
            orphans.push((id.as_str(), run));
        }
    }
    orphans
}

/// 전역 저널 예산의 (사용량, 상한).
fn budget_usage(state: &DaemonState) -> (u64, u64) {
    let budget = state
        .journal_budget
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    (budget.used(), budget.limit())
}

/// 실제로 지운 디스크 바이트를 전역 예산에 돌려준다.
fn release_journal_bytes(state: &DaemonState, bytes: u64) {
    if bytes == 0 {
        return;
    }
    state
        .journal_budget
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .release(bytes);
}

/// 상한의 `percent`%(넘침 없이 내림).
fn watermark(limit: u64, percent: u64) -> u64 {
    limit / 100 * percent + limit % 100 * percent / 100
}

/// 한 세션의 저널 런(활성 파일 + 닫힌 세그먼트)을 디렉터리 열거로 본 모습.
#[derive(Debug)]
struct DiskRun {
    files: Vec<PathBuf>,
    /// 열거 시점의 파일 크기 합.
    bytes: u64,
    /// 런에서 가장 최근의 mtime(마지막 쓰기).
    last_write: Option<SystemTime>,
    /// 모든 항목의 메타데이터를 읽었다. 아니면 지우지 않는다(보수적).
    readable: bool,
}

impl DiskRun {
    fn new() -> Self {
        Self {
            files: Vec::new(),
            bytes: 0,
            last_write: None,
            readable: true,
        }
    }
}

/// 평평한 저널 디렉터리를 세션 id별 런으로 묶는다. 데몬이 만드는 이름만
/// 센다(`run_session_id`) — 그 밖의 파일은 저널이 아니므로 시드에도 축출에도
/// 들어가지 않는다. 디렉터리가 없거나 읽을 수 없으면 비어 있다.
fn journal_runs(dir: &Path) -> BTreeMap<String, DiskRun> {
    let mut runs: BTreeMap<String, DiskRun> = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return runs;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(id) = file_name.to_str().and_then(run_session_id) else {
            continue;
        };
        let meta = entry.metadata();
        if meta.as_ref().is_ok_and(|meta| !meta.is_file()) {
            continue;
        }
        let run = runs.entry(id.to_string()).or_insert_with(DiskRun::new);
        let Ok(meta) = meta else {
            run.readable = false;
            continue;
        };
        match meta.modified() {
            Ok(time) => run.last_write = run.last_write.max(Some(time)),
            Err(_) => run.readable = false,
        }
        run.files.push(entry.path());
        run.bytes += meta.len();
    }
    runs
}

/// 저널 파일 이름 → 세션 id. `<id>.mtj`(활성 파일)나 `<id>.mtj.<k>`(닫힌
/// 세그먼트, k는 숫자만 — `x.mtj.bak`은 아니다)이고, `<id>`가 데몬이 만드는
/// 정규형 UUID v4일 때만 id를 돌려준다.
fn run_session_id(name: &str) -> Option<&str> {
    let id = match name.split_once(".mtj.") {
        Some((id, index)) if is_segment_index(index) => id,
        _ => name.strip_suffix(".mtj")?,
    };
    let canonical = SessionId::parse(id).ok()?;
    (canonical.as_str() == id).then_some(id)
}

fn is_segment_index(suffix: &str) -> bool {
    !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
}

/// 저널 파일 삭제 결과.
#[derive(Debug)]
struct Removal {
    /// 이번 호출이 실제로 unlink한 파일들의 디스크 바이트.
    bytes: u64,
    /// 모든 파일이 사라졌다(지금 지웠거나 이미 없었다).
    complete: bool,
}

/// 파일을 하나씩 unlink하며 실제로 지운 바이트를 센다. 크기는 unlink 직전에
/// 잰다 — 예산은 디스크 바이트 단위다. 이미 없는 파일은 0이다(지운 쪽이 이미
/// 방출했다). 잴 수 없거나 일반 파일이 아니거나 지우지 못한 항목은 세지 않고
/// 미완료로 남긴다.
fn remove_run_files(files: &[PathBuf]) -> Removal {
    let mut removal = Removal {
        bytes: 0,
        complete: true,
    };
    for file in files {
        let len = match std::fs::symlink_metadata(file) {
            Ok(meta) if meta.is_file() => meta.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            _ => {
                removal.complete = false;
                continue;
            }
        };
        match std::fs::remove_file(file) {
            Ok(()) => removal.bytes += len,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(
                    path = %file.display(),
                    %error,
                    "journal retention: remove failed"
                );
                removal.complete = false;
            }
        }
    }
    removal
}

/// 공간 압력 축출 후보 하나.
struct Victim<'a> {
    last_write: Option<SystemTime>,
    target: VictimTarget<'a>,
}

#[derive(Clone, Copy)]
enum VictimTarget<'a> {
    /// 되살릴 길 없는 고아 런(`orphan_runs`).
    Orphan(&'a str, &'a DiskRun),
    /// 종료·비고정 세션. 살아 있는지는 지우기 직전에 다시 본다.
    Session(&'a SessionRecord, Option<&'a DiskRun>),
}

/// 축출 순서: 고아 런 먼저(어떤 RPC도 닿지 못한다), 그다음 마지막 쓰기가
/// 오래된 순.
fn sort_victims(victims: &mut [Victim<'_>]) {
    victims.sort_by_key(|victim| {
        let session = matches!(victim.target, VictimTarget::Session(..));
        (session, victim.last_write)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_constants_stay_sane() {
        // 시작 지연이 주기보다 길면 첫 스윕이 영영 미뤄진다.
        assert!(INITIAL_DELAY < TICK);
        assert!(TICK >= Duration::from_secs(60));
        // 공간 압력 확인은 스윕 사이에 여러 번 돈다. 막힌 뒤의 재확인도
        // 다음 스윕보다 먼저 온다.
        assert!(PRESSURE_CHECK < PRESSURE_STUCK_RECHECK);
        assert!(PRESSURE_STUCK_RECHECK < TICK);
        const { assert!(PRESSURE_LOW_PERCENT < PRESSURE_HIGH_PERCENT) };
        const { assert!(PRESSURE_HIGH_PERCENT < 100) };
    }

    /// 에이전트 세션 보존 상수는 계약 문서(02 §8)와 함께 움직인다.
    /// 나이 상한이 저널 보존 기본값(7일)보다 훨씬 길어야 한다 — 기록은
    /// 저널보다 가볍고(한 행), 복구 목록은 오래 남는 것이 쓸모 있다.
    #[test]
    fn agent_session_retention_constants_stay_sane() {
        assert_eq!(AGENT_SESSION_MAX_AGE_DAYS, 90);
        assert_eq!(AGENT_SESSION_KEEP_AT_MOST, 1_000);
        const { assert!(AGENT_SESSION_MAX_AGE_DAYS > 7) };
        // 개수 상한은 목록 응답 상한(200건)보다 넉넉해야 한다.
        use term_contracts::agent_session::limits;
        const { assert!(AGENT_SESSION_KEEP_AT_MOST > limits::LIST_MAX) };
    }

    /// L3: 종료됐고 뷰가 없는 세션은 레지스트리에 남아 있어도 스윕 대상이다.
    /// 예전 조건("레지스트리에 있으면 건너뜀")은 데몬 자신의 세션을 전부
    /// 제외시켜 전역 저널 예산(2 GiB)이 한 번도 방출되지 않게 만들었다.
    #[test]
    fn sweep_skips_only_live_or_watched_sessions() {
        // 레지스트리에 없다 — 이전 데몬이 남긴 저널: 대상.
        assert!(!skip_live_session(None));
        // 종료 + 뷰 없음: 대상(이것이 이번 수정의 핵심이다).
        assert!(!skip_live_session(Some((true, false))));
        // 종료했지만 누군가 재생 중: 건너뛴다.
        assert!(skip_live_session(Some((true, true))));
        // actor가 살아 있다: 뷰 유무와 무관하게 건너뛴다.
        assert!(skip_live_session(Some((false, false))));
        assert!(skip_live_session(Some((false, true))));
    }

    /// 수위는 넘침 없이 상한의 비율로 내림한다.
    #[test]
    fn watermarks_are_percentages_of_the_limit() {
        assert_eq!(watermark(1000, 90), 900);
        assert_eq!(watermark(99, 90), 89);
        assert_eq!(watermark(2 << 30, 90), 1_932_735_283);
        assert!(watermark(u64::MAX, 90) < u64::MAX);
    }

    /// 보존 기간 판정: 파일이 없는 런은 회계만 닫으러 지나가고, 읽을 수
    /// 없는 런은 남기며, 나머지는 마지막 쓰기로 가른다.
    #[test]
    fn retention_keeps_recent_and_unreadable_runs() {
        let at = |secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        let cutoff = at(1_000);
        let run = |last_write: SystemTime, readable: bool| DiskRun {
            files: Vec::new(),
            bytes: 0,
            last_write: Some(last_write),
            readable,
        };
        assert!(!keep_for_retention(None, cutoff));
        assert!(keep_for_retention(Some(&run(at(2_000), true)), cutoff));
        assert!(!keep_for_retention(Some(&run(at(500), true)), cutoff));
        assert!(keep_for_retention(Some(&run(at(500), false)), cutoff));
    }

    /// 시드와 축출은 데몬이 만드는 저널 이름만 센다: 정규형 UUID v4의
    /// `<id>.mtj`와 숫자 접미사 `<id>.mtj.<k>`.
    #[test]
    fn run_session_id_accepts_only_daemon_journal_names() {
        let id = SessionId::generate();
        let id = id.as_str();
        assert_eq!(run_session_id(&format!("{id}.mtj")), Some(id));
        assert_eq!(run_session_id(&format!("{id}.mtj.12")), Some(id));
        assert_eq!(run_session_id(&format!("{id}.mtj.bak")), None);
        assert_eq!(run_session_id(&format!("{id}.mtj.")), None);
        let upper = format!("{}.mtj", id.to_uppercase());
        assert_eq!(run_session_id(&upper), None);
        assert_eq!(run_session_id("notes.mtj"), None);
        // UUID이지만 v4가 아니다.
        let not_v4 = "00000000-0000-1000-8000-000000000000.mtj";
        assert_eq!(run_session_id(not_v4), None);
    }

    /// 디렉터리 열거는 세션 id별로 런을 묶고, 저널이 아닌 이름과 일반 파일이
    /// 아닌 항목은 세지 않는다. 시드(`journal_bytes_on_disk`)도 같은 집합이다.
    #[test]
    fn journal_runs_group_daemon_files_by_session() {
        let dir = tempfile::tempdir().unwrap();
        let id = SessionId::generate();
        let base = dir.path().join(format!("{id}.mtj"));
        std::fs::write(&base, [0u8; 20]).unwrap();
        std::fs::write(term_pty::segments::segment_path(&base, 3), [0u8; 7]).unwrap();
        std::fs::write(dir.path().join(format!("{id}.mtj.bak")), [0u8; 100]).unwrap();
        std::fs::write(dir.path().join("notes.mtj"), [0u8; 100]).unwrap();
        let other = SessionId::generate();
        let not_a_file = dir.path().join(format!("{other}.mtj"));
        std::fs::create_dir(&not_a_file).unwrap();

        let runs = journal_runs(dir.path());
        assert_eq!(runs.len(), 1);
        let run = &runs[id.as_str()];
        assert_eq!(run.bytes, 27);
        assert_eq!(run.files.len(), 2);
        assert!(run.readable && run.last_write.is_some());
        assert_eq!(journal_bytes_on_disk(dir.path()), 27);
        assert_eq!(journal_bytes_on_disk(&dir.path().join("missing")), 0);
    }

    /// 방출은 이번에 실제로 지운 디스크 바이트다. 이미 없는 파일은 0이고,
    /// 일반 파일이 아닌 항목은 지우지도 세지도 않고 미완료로 남긴다.
    #[test]
    fn remove_run_files_counts_only_what_it_unlinked() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("a.mtj");
        std::fs::write(&present, [0u8; 64]).unwrap();
        let absent = dir.path().join("a.mtj.1");
        let removal = remove_run_files(&[present.clone(), absent]);
        assert_eq!((removal.bytes, removal.complete), (64, true));
        assert!(!present.exists());

        let not_a_file = dir.path().join("b.mtj");
        std::fs::create_dir(&not_a_file).unwrap();
        let removal = remove_run_files(std::slice::from_ref(&not_a_file));
        assert_eq!((removal.bytes, removal.complete), (0, false));
        assert!(not_a_file.exists());
    }

    /// 공간 압력 축출 순서: 고아 런 먼저, 그다음 마지막 쓰기가 오래된 세션.
    #[test]
    fn pressure_victims_go_orphans_first_then_oldest_write() {
        let at = |secs| Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs));
        let record = |id: SessionId| SessionRecord {
            id,
            workload_id: term_contracts::ids::WorkloadId::generate(),
            initial_cols: 80,
            initial_rows: 24,
            journal_relative_path: String::new(),
            journal_limit_bytes: 0,
            journal_bytes: 0,
            last_seq: 0,
            replay_status: "available".into(),
            pinned: false,
            created_at: String::new(),
        };
        let old = record(SessionId::generate());
        let new = record(SessionId::generate());
        let orphan = DiskRun::new();
        let mut victims = vec![
            Victim {
                last_write: at(300),
                target: VictimTarget::Session(&new, None),
            },
            Victim {
                last_write: at(100),
                target: VictimTarget::Session(&old, None),
            },
            Victim {
                last_write: at(900),
                target: VictimTarget::Orphan("orphan", &orphan),
            },
        ];
        sort_victims(&mut victims);
        let order: Vec<&str> = victims
            .iter()
            .map(|victim| match victim.target {
                VictimTarget::Orphan(id, _) => id,
                VictimTarget::Session(session, _) => session.id.as_str(),
            })
            .collect();
        assert_eq!(order, ["orphan", old.id.as_str(), new.id.as_str()]);
    }
}

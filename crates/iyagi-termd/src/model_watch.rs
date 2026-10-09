//! 에이전트 **현재 모델·effort** 추적기 — `agent_watch` 틱(1초)마다 부른다.
//! 파서와 즉시성 실측표는 [`crate::agent_model`]에 있다.
//!
//! * Claude Code: 상태줄 캐시(`iyagi-termd claude-usage`가 세션 id별로 기록)와
//!   transcript 꼬리를 **수정 시각·크기가 바뀔 때만** 다시 읽는다. 둘 다
//!   `/model`·`/effort` 직후 수십 ms 안에 갱신되므로 반영 지연은 틱 하나다.
//! * Codex: 세션 기록(rollout)은 다음 턴에야 남는다. `/model` 선택은
//!   `config.toml`에 즉시 저장되므로, 그 파일 내용이 바뀌면 **마지막으로
//!   입력받은 Codex pane**(Codex pane이 하나뿐이면 그 pane)에 귀속한다. 다음 턴의
//!   rollout 기록이 더 새로우면 그 기록이 이긴다.
//! * 세션 기록이 아직 없으면 실행 인수·(pane 시작 시점의) 설정 기본값을
//!   잠정값(`Defaults`)으로 쓴다.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use term_contracts::ids::WorkloadId;
use term_contracts::snapshot::AgentModelSource;
use term_platform::proc_scan::ProcessBrief;

use crate::agent_model::{self, ClaudeModelEvent, LaunchModel, ModelObservation};
use crate::agent_session::{self, Resolved};
use crate::state::DaemonState;

/// 마지막 입력으로부터 이 시간 안의 Codex pane만 전역 설정 변경의 주인으로 본다.
const CODEX_CONFIG_ATTRIBUTION_WINDOW_MS: u64 = 120_000;
/// 입력 기록 보존 기간 — 기록이 많아지면 이보다 오래된 항목을 정리한다.
const INPUT_RECORD_TTL_MS: u64 = 10 * 60_000;
const INPUT_LOG_PRUNE_AT: usize = 256;
/// `config.toml` 읽기 상한.
const CODEX_CONFIG_READ_MAX: u64 = 1024 * 1024;
/// transcript를 세션 id로 폴백 검색할 때 훑는 프로젝트 디렉터리 상한.
const PROJECT_SCAN_MAX: usize = 4_000;
/// rollout을 세션 id로 찾을 때 훑는 날짜 디렉터리 상한(최근부터).
const ROLLOUT_DAY_SCAN_MAX: usize = 60;
/// 못 찾은 로그 경로를 다시 찾는 간격(틱).
const SEARCH_RETRY_TICKS: u32 = 5;

fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// 파일 변경 감지용 (수정 시각 ns, 크기). ns 정밀도라야 같은 밀리초 안에 같은
/// 길이로 다시 저장된 변경을 놓치지 않는다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    modified_ns: u128,
    len: u64,
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let modified_ns = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(FileStamp {
        modified_ns,
        len: meta.len(),
    })
}

// ---------------------------------------------------------------------------
// 입력 기록(Codex 전역 설정 변경의 pane 귀속 근거)

/// workload별 마지막 PTY 입력 시각(데몬 단조 ms).
pub struct InputLog(Mutex<Option<HashMap<WorkloadId, u64>>>);

impl InputLog {
    pub const fn new() -> Self {
        Self(Mutex::new(None))
    }

    pub fn note(&self, now_ms: u64, workload_id: &WorkloadId) {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let map = guard.get_or_insert_with(HashMap::new);
        if let Some(at) = map.get_mut(workload_id) {
            *at = now_ms;
            return;
        }
        if map.len() >= INPUT_LOG_PRUNE_AT {
            map.retain(|_, at| now_ms.saturating_sub(*at) <= INPUT_RECORD_TTL_MS);
        }
        map.insert(workload_id.clone(), now_ms);
    }

    pub fn forget(&self, workload_id: &WorkloadId) {
        if let Some(map) = self.0.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
            map.remove(workload_id);
        }
    }

    fn last(&self, workload_id: &WorkloadId) -> Option<u64> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()?
            .get(workload_id)
            .copied()
    }
}

impl Default for InputLog {
    fn default() -> Self {
        Self::new()
    }
}

static INPUT_LOG: InputLog = InputLog::new();

/// 세션 입력이 PTY로 전달될 때(`session.input`) 호출한다.
pub fn note_input(now_ms: u64, workload_id: &WorkloadId) {
    INPUT_LOG.note(now_ms, workload_id);
}

/// 감시에서 빠진 workload의 입력 기록을 지운다.
pub fn forget_input(workload_id: &WorkloadId) {
    INPUT_LOG.forget(workload_id);
}

// ---------------------------------------------------------------------------
// Codex config.toml 캐시

#[derive(Default)]
struct CodexConfigState {
    path: Option<PathBuf>,
    stamp: Option<FileStamp>,
    text: String,
    loaded: bool,
}

/// 마지막으로 읽은 `config.toml` — 내용 변경만 "변경"으로 센다.
pub struct CodexConfigCache(Mutex<Option<CodexConfigState>>);

impl CodexConfigCache {
    pub const fn new() -> Self {
        Self(Mutex::new(None))
    }

    /// 수정 시각·크기가 바뀌었으면 다시 읽는다. 반환: 직전 로드 대비 **내용**이
    /// 바뀌었는가(첫 로드·경로 변경은 기준선일 뿐 변경이 아니다).
    fn reload(&self, path: Option<&Path>) -> bool {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let state = guard.get_or_insert_with(CodexConfigState::default);
        let path_changed = state.path.as_deref() != path;
        let stamp = path.and_then(file_stamp);
        if state.loaded && !path_changed && state.stamp == stamp {
            return false;
        }
        let text = match (path, stamp) {
            (Some(path), Some(stamp)) if stamp.len <= CODEX_CONFIG_READ_MAX => {
                std::fs::read_to_string(path).unwrap_or_default()
            }
            _ => String::new(),
        };
        let changed = state.loaded && !path_changed && text != state.text;
        *state = CodexConfigState {
            path: path.map(Path::to_path_buf),
            stamp,
            text,
            loaded: true,
        };
        changed
    }

    fn observation(&self, profile: Option<&str>) -> ModelObservation {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(|state| agent_model::codex_config_model(&state.text, profile))
            .unwrap_or_default()
    }
}

impl Default for CodexConfigCache {
    fn default() -> Self {
        Self::new()
    }
}

static CODEX_CONFIG: CodexConfigCache = CodexConfigCache::new();

// ---------------------------------------------------------------------------
// 틱 문맥

/// 한 틱 동안 쓰는 경로와 공유 캐시.
pub struct ModelContext<'a> {
    pub data_dir: PathBuf,
    pub claude_home: Option<PathBuf>,
    pub codex_home: Option<PathBuf>,
    inputs: &'a InputLog,
    codex_config: &'a CodexConfigCache,
}

impl ModelContext<'static> {
    /// 데몬 실제 경로(`$CLAUDE_CONFIG_DIR`·`$CODEX_HOME` 반영)와 전역 캐시.
    pub fn current(state: &DaemonState) -> Self {
        Self {
            data_dir: state.paths.root().to_path_buf(),
            claude_home: agent_session::claude_home(),
            codex_home: agent_session::codex_home(),
            inputs: &INPUT_LOG,
            codex_config: &CODEX_CONFIG,
        }
    }
}

/// `config.toml` 내용이 이번 틱에 바뀌었으면 그 변경의 주인 pane.
/// 틱마다 한 번 부른다(Codex pane이 없어도 기준선을 따라가야 한다).
pub fn codex_config_target(
    ctx: &ModelContext<'_>,
    now_ms: u64,
    codex_workloads: &[WorkloadId],
) -> Option<WorkloadId> {
    let path = ctx.codex_home.as_ref().map(|home| home.join("config.toml"));
    if !ctx.codex_config.reload(path.as_deref()) {
        return None;
    }
    match codex_workloads {
        [] => None,
        [only] => Some(only.clone()),
        many => many
            .iter()
            .filter_map(|id| ctx.inputs.last(id).map(|at| (at, id)))
            .filter(|(at, _)| now_ms.saturating_sub(*at) <= CODEX_CONFIG_ATTRIBUTION_WINDOW_MS)
            .max_by_key(|(at, _)| *at)
            .map(|(_, id)| id.clone()),
    }
}

// ---------------------------------------------------------------------------
// (에이전트, pid)별 추적 상태

#[derive(Debug)]
struct Cached<T> {
    stamp: FileStamp,
    value: T,
}

/// 스탬프가 같으면 다시 읽지 않는다. 파일이 사라지면 캐시를 버린다.
fn refresh_cached<T>(slot: &mut Option<Cached<T>>, path: &Path, load: impl FnOnce(&Path) -> T) {
    match file_stamp(path) {
        None => *slot = None,
        Some(stamp) if slot.as_ref().is_some_and(|cached| cached.stamp == stamp) => {}
        Some(stamp) => {
            *slot = Some(Cached {
                stamp,
                value: load(path),
            })
        }
    }
}

/// 로그 파일 위치 — 못 찾으면 [`SEARCH_RETRY_TICKS`]마다 다시 찾는다.
#[derive(Debug, Default)]
struct LogLocator {
    path: Option<PathBuf>,
    last_search_tick: Option<u32>,
}

impl LogLocator {
    fn locate(&mut self, tick: u32, find: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
        let due = self
            .last_search_tick
            .is_none_or(|last| tick.wrapping_sub(last) >= SEARCH_RETRY_TICKS);
        if self.path.is_none() && due {
            self.last_search_tick = Some(tick);
            self.path = find();
        }
        self.path.clone()
    }
}

/// 한 (에이전트, pid)의 모델 추적 상태.
#[derive(Debug)]
pub struct ModelWatch {
    /// 이 프로세스를 처음 본 벽시계 ms — 잠정값의 기준 시각.
    started_wall_ms: u64,
    tick: u32,
    launch: Option<LaunchModel>,
    /// pane 시작 시점의 잠정값(실행 인수 > 설정 기본값). 한 번만 잡는다 —
    /// 나중에 바뀐 전역 설정은 이 pane의 기본값이 아니다.
    defaults: Option<ModelObservation>,
    session_id: Option<String>,
    statusline: Option<Cached<Option<(u64, ModelObservation)>>>,
    transcript_locator: LogLocator,
    transcript: Option<Cached<Vec<ClaudeModelEvent>>>,
    rollout_locator: LogLocator,
    rollout: Option<Cached<Option<(u64, ModelObservation)>>>,
    /// Codex `/model`의 즉시 저장값을 이 pane에 귀속한 것(벽시계 ms, 값).
    config_change: Option<(u64, ModelObservation)>,
    current: Option<(ModelObservation, AgentModelSource)>,
}

impl Default for ModelWatch {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelWatch {
    pub fn new() -> Self {
        Self::started_at(wall_ms())
    }

    fn started_at(started_wall_ms: u64) -> Self {
        Self {
            started_wall_ms,
            tick: 0,
            launch: None,
            defaults: None,
            session_id: None,
            statusline: None,
            transcript_locator: LogLocator::default(),
            transcript: None,
            rollout_locator: LogLocator::default(),
            rollout: None,
            config_change: None,
            current: None,
        }
    }

    /// 지금 아는 모델·effort와 출처. 한 번도 못 알아냈으면 `None`.
    pub fn current(&self) -> Option<&(ModelObservation, AgentModelSource)> {
        self.current.as_ref()
    }

    /// [`codex_config_target`]이 이 pane을 골랐을 때 — 바뀐 설정을 즉시 반영한다.
    pub fn apply_codex_config_change(&mut self, ctx: &ModelContext<'_>) {
        let profile = self
            .launch
            .as_ref()
            .and_then(|launch| launch.codex_profile.clone());
        let observation = ctx.codex_config.observation(profile.as_deref());
        if !observation.is_empty() {
            self.config_change = Some((wall_ms(), observation));
        }
    }
}

/// 한 틱의 갱신. `brief`는 에이전트 프로세스(실행 인수), `resolved`는 알아낸 세션.
pub fn refresh(
    ctx: &ModelContext<'_>,
    agent: &str,
    brief: Option<&ProcessBrief>,
    resolved: Option<&Resolved>,
    watch: &mut ModelWatch,
) {
    watch.tick = watch.tick.wrapping_add(1);
    if watch.launch.is_none() {
        if let Some(brief) = brief {
            watch.launch = Some(agent_model::launch_model(agent, &brief.cmd));
        }
    }
    if watch.defaults.is_none() {
        watch.defaults = Some(defaults_for(ctx, agent, watch.launch.as_ref()));
    }
    let session_id = resolved.map(|r| r.session_id.as_str());
    if session_id != watch.session_id.as_deref() {
        // 새 세션(재개·/clear 등) — 세션별 로그 캐시는 버리고 pane 수준 값은 둔다.
        watch.session_id = session_id.map(str::to_string);
        watch.statusline = None;
        watch.transcript = None;
        watch.transcript_locator = LogLocator::default();
        watch.rollout = None;
        watch.rollout_locator = LogLocator::default();
    }
    let next = match agent {
        "claude" => claude_current(ctx, resolved, watch),
        "codex" => codex_current(ctx, resolved, brief.map(|b| b.pid), watch),
        _ => None,
    };
    if next.is_some() {
        watch.current = next;
    }
}

/// 잠정값: 실행 인수가 설정 기본값(Codex만 — Claude의 settings.json 기본값은
/// 모델별 effort 때문에 믿을 수 없다)을 덮는다.
fn defaults_for(
    ctx: &ModelContext<'_>,
    agent: &str,
    launch: Option<&LaunchModel>,
) -> ModelObservation {
    let mut observation = match agent {
        "codex" => {
            let path = ctx.codex_home.as_ref().map(|home| home.join("config.toml"));
            ctx.codex_config.reload(path.as_deref());
            ctx.codex_config
                .observation(launch.and_then(|l| l.codex_profile.as_deref()))
        }
        _ => ModelObservation::default(),
    };
    if let Some(launch) = launch {
        if launch.observation.model.is_some() {
            observation.model = launch.observation.model.clone();
        }
        if launch.observation.effort.is_some() {
            observation.effort = launch.observation.effort.clone();
        }
    }
    observation
}

fn claude_current(
    ctx: &ModelContext<'_>,
    resolved: Option<&Resolved>,
    watch: &mut ModelWatch,
) -> Option<(ModelObservation, AgentModelSource)> {
    let defaults = watch.defaults.clone().filter(|o| !o.is_empty());
    let Some(resolved) = resolved else {
        return defaults.map(|o| (o, AgentModelSource::Defaults));
    };
    let session_id = resolved.session_id.as_str();

    let cache_path = agent_model::statusline_cache_path(&ctx.data_dir, session_id);
    refresh_cached(&mut watch.statusline, &cache_path, |path| {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| agent_model::parse_statusline_cache(&text))
    });

    let tick = watch.tick;
    let transcript = watch.transcript_locator.locate(tick, || {
        claude_transcript_path(
            ctx.claude_home.as_deref()?,
            session_id,
            resolved.cwd.as_deref(),
        )
    });
    if let Some(path) = transcript {
        refresh_cached(&mut watch.transcript, &path, |path| {
            agent_model::read_tail(path, agent_model::TAIL_READ_MAX)
                .map(|tail| agent_model::claude_transcript_events(&tail))
                .unwrap_or_default()
        });
    }

    // 기준: 상태줄 스냅숏과 잠정값 중 더 새로운 것. 그 뒤의 transcript 사건을 적용한다.
    let statusline = watch
        .statusline
        .as_ref()
        .and_then(|cached| cached.value.clone());
    let (base, base_source) = match (statusline, defaults) {
        (Some(line), Some(_)) if line.0 >= watch.started_wall_ms => {
            (Some(line), Some(AgentModelSource::StatusLine))
        }
        (Some(line), None) => (Some(line), Some(AgentModelSource::StatusLine)),
        (_, Some(defaults)) => (
            Some((watch.started_wall_ms, defaults)),
            Some(AgentModelSource::Defaults),
        ),
        (None, None) => (None, None),
    };
    let events: &[ClaudeModelEvent] = watch
        .transcript
        .as_ref()
        .map(|cached| cached.value.as_slice())
        .unwrap_or(&[]);
    let base_at = base.as_ref().map_or(0, |(at, _)| *at);
    let newer_event = events.iter().any(|event| event.at_ms > base_at);
    let folded = agent_model::fold_claude(base, events)?;
    let source = if newer_event {
        AgentModelSource::Transcript
    } else {
        base_source.unwrap_or(AgentModelSource::Transcript)
    };
    Some((folded, source))
}

/// `<claude_home>/projects/<slug(cwd)>/<session>.jsonl`. 프로젝트 디렉터리가 있는데
/// 파일만 없으면 아직 기록 전이다(다음에 다시). 디렉터리 이름 규칙이 다른 경우
/// (긴 경로 등)에만 세션 id로 폴백 검색한다.
fn claude_transcript_path(
    claude_home: &Path,
    session_id: &str,
    cwd: Option<&str>,
) -> Option<PathBuf> {
    let projects = claude_home.join("projects");
    let file_name = format!("{session_id}.jsonl");
    if let Some(cwd) = cwd {
        let dir = projects.join(agent_model::claude_project_slug(cwd));
        let expected = dir.join(&file_name);
        if expected.is_file() {
            return Some(expected);
        }
        if dir.is_dir() {
            return None;
        }
    }
    std::fs::read_dir(&projects)
        .ok()?
        .flatten()
        .take(PROJECT_SCAN_MAX)
        .map(|entry| entry.path().join(&file_name))
        .find(|path| path.is_file())
}

fn codex_current(
    ctx: &ModelContext<'_>,
    resolved: Option<&Resolved>,
    pid: Option<u32>,
    watch: &mut ModelWatch,
) -> Option<(ModelObservation, AgentModelSource)> {
    // (시각, 동률 우선순위, 값, 출처) — 가장 새로운 것이 이긴다.
    let mut candidates: Vec<(u64, u8, ModelObservation, AgentModelSource)> = Vec::with_capacity(3);
    if let Some(defaults) = watch.defaults.clone() {
        candidates.push((
            watch.started_wall_ms,
            0,
            defaults,
            AgentModelSource::Defaults,
        ));
    }
    if let Some((at, observation)) = watch.config_change.clone() {
        candidates.push((at, 1, observation, AgentModelSource::Config));
    }
    if let Some(resolved) = resolved {
        let session_id = resolved.session_id.as_str();
        let tick = watch.tick;
        let rollout = watch.rollout_locator.locate(tick, || {
            find_codex_rollout(ctx.codex_home.as_deref()?, session_id, pid)
        });
        if let Some(path) = rollout {
            refresh_cached(&mut watch.rollout, &path, |path| {
                agent_model::read_tail(path, agent_model::TAIL_READ_MAX)
                    .and_then(|tail| agent_model::codex_rollout_model(&tail))
            });
        }
        if let Some((at, observation)) = watch
            .rollout
            .as_ref()
            .and_then(|cached| cached.value.clone())
        {
            candidates.push((at, 2, observation, AgentModelSource::Rollout));
        }
    }
    candidates
        .into_iter()
        .filter(|(_, _, observation, _)| !observation.is_empty())
        .max_by_key(|(at, rank, _, _)| (*at, *rank))
        .map(|(_, _, observation, source)| (observation, source))
}

/// 세션 id의 rollout 파일. 열린 파일(구버전)을 먼저 보고, 없으면
/// `<codex_home>/sessions/YYYY/MM/DD/`를 최근 날짜부터 훑는다.
fn find_codex_rollout(codex_home: &Path, session_id: &str, pid: Option<u32>) -> Option<PathBuf> {
    let suffix = format!("{session_id}.jsonl");
    let is_match = |path: &Path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(&suffix))
    };
    if let Some(pid) = pid {
        if let Some(paths) = term_platform::proc_fds::open_file_paths(pid) {
            for path in &paths {
                let path: &Path = path.as_ref();
                if is_match(path) {
                    return Some(path.to_path_buf());
                }
            }
        }
    }
    let mut scanned = 0usize;
    for year in dirs_desc(&codex_home.join("sessions")) {
        for month in dirs_desc(&year) {
            for day in dirs_desc(&month) {
                scanned += 1;
                if scanned > ROLLOUT_DAY_SCAN_MAX {
                    return None;
                }
                let Ok(entries) = std::fs::read_dir(&day) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if is_match(&path) {
                        return Some(path);
                    }
                }
            }
        }
    }
    None
}

/// 하위 디렉터리를 이름 역순으로(날짜 디렉터리 = 최신 먼저).
fn dirs_desc(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs.reverse();
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;
    use term_contracts::agent_session::AgentSessionSource;

    const SESSION: &str = "6d0d2c32-3575-4e13-a11d-c9dad4cb85e1";

    struct Env {
        root: PathBuf,
        inputs: InputLog,
        config: CodexConfigCache,
    }

    impl Env {
        fn new(tag: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("model-watch-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Self {
                root,
                inputs: InputLog::new(),
                config: CodexConfigCache::new(),
            }
        }

        fn ctx(&self) -> ModelContext<'_> {
            ModelContext {
                data_dir: self.root.join("iyagi"),
                claude_home: Some(self.root.join("claude")),
                codex_home: Some(self.root.join("codex")),
                inputs: &self.inputs,
                codex_config: &self.config,
            }
        }

        fn write(&self, relative: &str, text: &str) {
            let path = self.root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn resolved(session_id: &str, cwd: &str) -> Resolved {
        Resolved {
            session_id: session_id.to_string(),
            name: None,
            status: None,
            cwd: Some(cwd.to_string()),
            title: None,
            source: AgentSessionSource::Registry,
        }
    }

    fn brief(pid: u32, cmd: &[&str]) -> ProcessBrief {
        ProcessBrief {
            pid,
            ppid: 1,
            name: cmd[0].to_string(),
            exe: None,
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn workload(n: u8) -> WorkloadId {
        serde_json::from_value(serde_json::json!(format!(
            "00000000-0000-4000-8000-0000000000{n:02}"
        )))
        .unwrap()
    }

    fn observed(watch: &ModelWatch) -> (Option<&str>, Option<&str>, Option<AgentModelSource>) {
        match watch.current() {
            Some((o, s)) => (o.model.as_deref(), o.effort.as_deref(), Some(*s)),
            None => (None, None, None),
        }
    }

    #[test]
    fn claude_launch_arguments_are_provisional_until_the_session_speaks() {
        let env = Env::new("claude-launch");
        let ctx = env.ctx();
        let mut watch = ModelWatch::started_at(1_000);
        let process = brief(7, &["claude", "--model", "sonnet", "--effort", "high"]);
        refresh(&ctx, "claude", Some(&process), None, &mut watch);
        assert_eq!(
            observed(&watch),
            (
                Some("sonnet"),
                Some("high"),
                Some(AgentModelSource::Defaults)
            )
        );
    }

    #[test]
    fn claude_statusline_and_transcript_reflect_model_and_effort_changes() {
        let env = Env::new("claude-live");
        let ctx = env.ctx();
        let cwd = "/work/proj";
        let session = resolved(SESSION, cwd);
        let mut watch = ModelWatch::started_at(1_000);

        // 시작 직후 상태줄(실측: 첫 실행) — Sonnet 5 · high.
        let cache = agent_model::statusline_cache_path(&ctx.data_dir, SESSION);
        let write_cache = |observation: ModelObservation, at: u64| {
            std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
            std::fs::write(
                &cache,
                agent_model::statusline_cache_value(&observation, at).to_string(),
            )
            .unwrap();
        };
        write_cache(
            ModelObservation {
                model: Some("Sonnet 5".into()),
                effort: Some("high".into()),
            },
            2_000,
        );
        refresh(&ctx, "claude", None, Some(&session), &mut watch);
        assert_eq!(
            observed(&watch),
            (
                Some("Sonnet 5"),
                Some("high"),
                Some(AgentModelSource::StatusLine)
            )
        );

        // `/effort xhigh` — transcript 로컬 명령 항목(상태줄보다 새로움). 프로젝트
        // 디렉터리는 transcript와 함께 생기므로 검색 재시도 간격을 넘겨 다시 찾는다.
        env.write(
            &format!("claude/projects/-work-proj/{SESSION}.jsonl"),
            &format!(
                "{}\n",
                r#"{"type":"user","timestamp":"2026-09-13T00:02:49.504Z","message":{"role":"user","content":"<local-command-stdout>Set effort level to xhigh (saved as your default for new sessions)</local-command-stdout>"}}"#
            ),
        );
        for _ in 0..SEARCH_RETRY_TICKS {
            refresh(&ctx, "claude", None, Some(&session), &mut watch);
        }
        assert_eq!(
            observed(&watch),
            (
                Some("Sonnet 5"),
                Some("xhigh"),
                Some(AgentModelSource::Transcript)
            )
        );

        // `/model opus[1m]` — 상태줄이 더 새로운 전체 스냅숏을 싣는다.
        write_cache(
            ModelObservation {
                model: Some("Opus 5 (1M context)".into()),
                effort: Some("xhigh".into()),
            },
            4_102_444_800_000,
        );
        refresh(&ctx, "claude", None, Some(&session), &mut watch);
        assert_eq!(
            observed(&watch),
            (
                Some("Opus 5 (1M context)"),
                Some("xhigh"),
                Some(AgentModelSource::StatusLine)
            )
        );
    }

    #[test]
    fn claude_transcript_fallback_finds_the_session_under_an_unexpected_project_dir() {
        let env = Env::new("claude-scan");
        env.write(
            &format!("claude/projects/-truncated-long-path-abc123/{SESSION}.jsonl"),
            &format!(
                "{}\n",
                r#"{"type":"user","timestamp":"2026-09-13T00:02:54.875Z","message":{"content":"<local-command-stdout>Set model to `Opus 5 (1M context)` and saved as your default for new sessions</local-command-stdout>"}}"#
            ),
        );
        let ctx = env.ctx();
        let mut watch = ModelWatch::started_at(1_000);
        refresh(
            &ctx,
            "claude",
            None,
            Some(&resolved(SESSION, "/some/very/long/path")),
            &mut watch,
        );
        assert_eq!(
            observed(&watch),
            (
                Some("Opus 5 (1M context)"),
                None,
                Some(AgentModelSource::Transcript)
            )
        );
    }

    #[test]
    fn codex_model_selection_is_attributed_to_the_pane_that_typed_it() {
        let env = Env::new("codex-config");
        env.write(
            "codex/config.toml",
            "model = \"gpt-5.6-sol\"\nmodel_reasoning_effort = \"high\"\n",
        );
        let ctx = env.ctx();
        let (a, b) = (workload(1), workload(2));
        let mut watch_a = ModelWatch::started_at(1_000);
        let mut watch_b = ModelWatch::started_at(1_000);
        let process = brief(9, &["codex"]);

        // 첫 틱: 기준선 로드(변경 아님) + 각 pane의 시작 시점 기본값.
        assert_eq!(
            codex_config_target(&ctx, 10_000, &[a.clone(), b.clone()]),
            None
        );
        refresh(&ctx, "codex", Some(&process), None, &mut watch_a);
        refresh(&ctx, "codex", Some(&process), None, &mut watch_b);
        assert_eq!(
            observed(&watch_a),
            (
                Some("gpt-5.6-sol"),
                Some("high"),
                Some(AgentModelSource::Defaults)
            )
        );

        // pane b에서 `/model` → config.toml 즉시 교체. 마지막 입력은 b.
        env.inputs.note(20_000, &a);
        env.inputs.note(29_000, &b);
        env.write(
            "codex/config.toml",
            "model = \"gpt-6-astra\"\nmodel_reasoning_effort = \"medium\"\n",
        );
        let target = codex_config_target(&ctx, 30_000, &[a.clone(), b.clone()]);
        assert_eq!(target, Some(b.clone()));
        watch_b.apply_codex_config_change(&ctx);
        refresh(&ctx, "codex", Some(&process), None, &mut watch_a);
        refresh(&ctx, "codex", Some(&process), None, &mut watch_b);
        assert_eq!(
            observed(&watch_b),
            (
                Some("gpt-6-astra"),
                Some("medium"),
                Some(AgentModelSource::Config)
            )
        );
        assert_eq!(
            observed(&watch_a),
            (
                Some("gpt-5.6-sol"),
                Some("high"),
                Some(AgentModelSource::Defaults)
            ),
            "다른 pane의 세션은 바뀌지 않았다"
        );

        // 다음 턴의 rollout 기록(더 새로움)이 권위 있는 값이다.
        env.write(
            &format!("codex/sessions/2026/09/13/rollout-2026-09-13T00-00-00-{SESSION}.jsonl"),
            &format!(
                "{}\n",
                r#"{"timestamp":"2100-01-01T00:00:00.000Z","type":"event_msg","payload":{"type":"thread_settings_applied","thread_settings":{"model":"gpt-5.5","reasoning_effort":"low"}}}"#
            ),
        );
        refresh(
            &ctx,
            "codex",
            Some(&process),
            Some(&resolved(SESSION, "/w")),
            &mut watch_b,
        );
        assert_eq!(
            observed(&watch_b),
            (
                Some("gpt-5.5"),
                Some("low"),
                Some(AgentModelSource::Rollout)
            )
        );
    }

    #[test]
    fn codex_config_change_without_a_recent_typist_is_not_guessed() {
        let env = Env::new("codex-stale");
        env.write("codex/config.toml", "model = \"gpt-5.6-sol\"\n");
        let ctx = env.ctx();
        let (a, b) = (workload(3), workload(4));
        assert_eq!(codex_config_target(&ctx, 0, &[a.clone(), b.clone()]), None);
        env.inputs.note(1_000, &a);
        // 길이가 다른 내용 — 파일 시스템 시각 정밀도와 무관하게 결정적이다.
        env.write("codex/config.toml", "model = \"gpt-6-astra-x\"\n");
        assert_eq!(
            codex_config_target(
                &ctx,
                1_000 + CODEX_CONFIG_ATTRIBUTION_WINDOW_MS + 1,
                &[a, b]
            ),
            None,
            "창 밖의 입력은 근거가 아니다 — 다음 턴 rollout을 기다린다"
        );
        // 내용이 같은 재저장은 변경이 아니다.
        std::thread::sleep(std::time::Duration::from_millis(5));
        env.write("codex/config.toml", "model = \"gpt-6-astra-x\"\n");
        assert_eq!(codex_config_target(&ctx, 0, &[workload(5)]), None);
    }

    #[test]
    fn input_log_keeps_the_latest_time_and_forgets_on_request() {
        let log = InputLog::new();
        let id = workload(6);
        log.note(5, &id);
        log.note(9, &id);
        assert_eq!(log.last(&id), Some(9));
        log.forget(&id);
        assert_eq!(log.last(&id), None);
    }
}

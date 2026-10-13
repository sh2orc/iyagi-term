//! 로컬 관측 게이트웨이 — 라우팅된 Claude Code pane의 업스트림 트래픽이
//! 데몬을 한 번 거쳐 가게 하는 루프백 프록시(패스스루).
//!
//! 하는 일은 세 가지뿐이다(프록시 자체는 아무것도 바꾸지 않는다):
//!
//! * **세션별 토큰 장부** — Anthropic Messages 응답의 `usage`
//!   (`message_start`/`message_delta` SSE, 비스트리밍 JSON)를 흘려보내며
//!   파싱해 세션·전체 사용량에 더한다. 응답 본문은 저장하지 않는다.
//! * **킬 스위치** — `gateway.set_policy { blocked: true }`면 모든 요청에
//!   403으로 답한다. 프로세스를 죽이는 기존 가드와 달리 *요청 단위* 차단이다.
//! * **비밀 노출면 축소** — 자식(Claude Code) 환경에는 실제 Z.ai 키 대신
//!   세션마다 발급되는 1회용 게이트웨이 토큰만 얹는다. 실제 키는 요청
//!   시점마다 `term_secrets`에서 다시 읽어 업스트림 헤더에만 쓴다(회전·
//!   삭제가 즉시 반영된다).
//!
//! 결정 구조:
//!
//! * 서버는 데몬 기동부터 **항상** 127.0.0.1의 임시 포트에 뜬다. `enabled`
//!   는 "새 라우팅을 게이트웨이로 감쌀까"만 결정한다 — 꺼진 동안에도 이미
//!   감싸인 세션의 요청은 계속 흘러야 한다(감다가 도중에 풀리면 토큰이
//!   안 맞아 401이 된다).
//! * 라우팅 결정은 **적용 시점**(셸: spawn 직전, 관리: gate 프레임 조립
//!   시점)에 내린다. 대기열에서 깨어난 관리 실행은 시작할 때 다시 묻는다
//!   — 데몬이 재시작해 포트가 바뀌어도 묵은 URL을 물고 있지 않는다.
//! * 프록시는 **범용 패스스루**다: `/healthz`와 인증·차단 검사를 제외하면
//!   모든 메서드·경로를 그대로 업스트림(Z.ai Anthropic 호환 엔드포인트)로
//!   넘긴다. 사용량 파싱은 `/v1/messages` 응답에만 붙는 관측 부가물이다.
//!
//! 보안:
//!
//! * 루프백 전용 바인드. 게이트웨이 토큰 없이는 그 어떤 요청도 업스트림으로
//!   가지 않는다(401). 토큰은 192비트 난수이고 로그·상태 응답에 절대 나오지
//!   않는다 — 상태는 워크로드/세션 id와 사용량만 싣는다.
//! * 업스트림 요청 헤더는 허용 목록으로 골라 실어 보낸다(호스트·쿠키·
//!   인증 헤더는 버리고 실제 키로 다시 쓴다). 로그에는 경로·상태·토큰
//!   수만 남긴다.
//! * 응답 본문은 파싱용으로 스캔만 하고 저장하지 않는다(저널·DB 무관).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpListener;
use zeroize::Zeroizing;

use term_contracts::gateway::{
    GatewayPolicy, GatewayPoolStrategy, GatewayStatus, GatewayUsage, GATEWAY_TOKEN_PREFIX,
};
use term_contracts::ids::{SessionId, WorkloadId};
use term_secrets::KeyPoolStore;

use crate::connections::{SecretRedactor, CLAUDE_ZAI_BASE_URL};

/// 인증을 게이트웨이 토큰으로 받아들이는 헤더. Claude Code는
/// `ANTHROPIC_AUTH_TOKEN` 경로에서 `Authorization: Bearer`를 쓰고
/// `ANTHROPIC_API_KEY` 경로에서 `x-api-key`를 쓴다(둘 다 허용).
const AUTH_HEADERS: [&str; 2] = ["authorization", "x-api-key"];

/// 요청 헤더 중 업스트림으로 실어 보내는 것(값은 손대지 않는다).
/// `authorization`/`x-api-key`는 여기 없다 — 실제 키로 다시 쓴다.
const FORWARD_REQUEST_HEADERS: [&str; 6] = [
    "content-type",
    "accept",
    "anthropic-version",
    "anthropic-beta",
    "user-agent",
    "x-app",
];

/// 업스트림 응답 헤더 중 클라이언트로 되돌려 주는 것. 재시도 힌트는
/// 429/529를 그대로 넘길 때 CLI가 자기 backoff 대신 제공자 값을 쓰게 한다.
const FORWARD_RESPONSE_HEADERS: [&str; 5] = [
    "content-type",
    "request-id",
    "retry-after",
    "retry-after-ms",
    "x-should-retry",
];

/// 업스트림 연결 수립 상한. 응답 대기·본문은 끊지 않는다(긴 생성은 정상이고
/// CLI가 `API_TIMEOUT_MS`로 스스로 끊는다) — 연결조차 안 되는 경우만 자른다.
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// 장부에 남기는 세션(게이트웨이 토큰) 상한. 초과하면 가장 오래 쉰 세션부터
/// 버린다 — 종료 훅(`forget_workload`)이 정리를 담당하지만, 비정상 종료로
/// 남는 항목이 데몬을 살아 있는 동안 무한히 자라지 않게 하는 보험이다.
const MAX_TRACKED_SESSIONS: usize = 256;

/// 요청 헤드(시작 줄 + 헤더)의 읽기 상한.
const MAX_HEAD_BYTES: usize = 16 * 1024;
/// 요청 본문 상한. 이미지 첨부를 담은 큰 요청도 여유 있게 들어온다.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
/// 비스트리밍 응답 본문의 버퍼 상한(SSE는 스캔만 하고 버퍼링하지 않는다).
const MAX_RESPONSE_BUFFER: usize = 8 * 1024 * 1024;
/// SSE 한 줄의 상한 — 델타 단위로 쪼개지는 실제 스트림보다 넉넉하다.
/// 넘치는 줄은 관측을 포기하고 흘려보내기만 한다.
const MAX_SSE_LINE: usize = 1024 * 1024;

/// 공유 상태. `DaemonState.gateway`로 살고, 서버 태스크와 런치 파이프라인이
/// 같은 인스턴스를 본다.
pub struct Gateway {
    /// 관측 라우팅 on/off(`gateway.set_policy`). 꺼져 있으면 새 라우팅을
    /// 감싸지 않는다(이미 감싸인 세션은 계속 흐른다).
    enabled: AtomicBool,
    /// 킬 스위치: true면 모든 요청을 403으로 차단한다.
    blocked: AtomicBool,
    /// 실제로 바인드된 포트(0 = 수신 불가, 라우팅 불가).
    port: AtomicU16,
    /// 게이트웨이 토큰 → 세션 장부 항목. 키(토큰)는 비밀이다 — 로그 금지.
    sessions: Mutex<HashMap<String, GwSession>>,
    /// 데몬 수명 전체의 누적 사용량(세션 정리와 무관하게 살아 있다).
    totals: Mutex<GatewayUsage>,
    /// 실제 키를 요청 시점에 다시 읽는 저장소 루트(`term_secrets`).
    secrets_root: PathBuf,
    /// 업스트림 기본 URL. Z.ai Anthropic 호환 엔드포인트(테스트가 바꾼다).
    upstream_base: String,
    /// 업스트림 호출용 비동기 클라이언트(스트리밍, 타임아웃 없음 —
    /// CLI가 `API_TIMEOUT_MS`로 스스로 끊는다).
    client: reqwest::Client,
    /// API 키 회전 엔진(09 §3). 상태(쿨다운·격리·커서)는 여기 산다.
    pool: std::sync::Mutex<crate::key_pool::KeyPool>,
    /// 풀 저장소 스냅숏 캐시(비밀 포함 — 로그 금지). 5초 TTL로 다시 읽는다.
    pool_cache: std::sync::Mutex<Option<CachedPool>>,
}

/// 풀 저장소 스냅숏 캐시. 비밀이 든 벡터를 들고 있으므로 drop 시 지워진다.
struct CachedPool {
    entries: Vec<PoolCacheEntry>,
    loaded_epoch_ms: u64,
}

/// 캐시 항목: id·라벨(상태 표면용) + 비밀(사본도 `Zeroizing` 드롭으로 지워진다).
#[derive(Clone)]
struct PoolCacheEntry {
    id: String,
    label: String,
    secret: Zeroizing<String>,
}

/// 풀 스냅숏 캐시 TTL — 앱에서 키를 추가·삭제해도 이 안에는 반영된다.
const POOL_CACHE_TTL_MS: u64 = 5_000;

/// 장부의 세션 항목. 토큰 문자열은 맵 키로만 존재한다.
struct GwSession {
    workload_id: WorkloadId,
    session_id: SessionId,
    usage: GatewayUsage,
    /// 이 세션이 마지막으로 쓴 모델(응답에서 관측, 전시 전용).
    model: Option<String>,
    /// 묶인 풀 키 id(친화성, 09 §3.2). 풀 경로가 아니면 None.
    key_id: Option<String>,
    started_epoch_ms: u64,
    last_seen_epoch_ms: u64,
}

/// 라우팅 적용 결과(셸 모드). 비밀인 토큰만 `Zeroizing`에 든다.
pub struct GatewayRoute {
    pub base_url: String,
    pub token: Zeroizing<String>,
}

impl Gateway {
    pub fn new(secrets_root: PathBuf) -> Gateway {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(UPSTREAM_CONNECT_TIMEOUT)
            .build()
            .unwrap_or_default();
        Gateway {
            enabled: AtomicBool::new(false),
            blocked: AtomicBool::new(false),
            port: AtomicU16::new(0),
            sessions: Mutex::new(HashMap::new()),
            totals: Mutex::new(GatewayUsage::default()),
            secrets_root,
            upstream_base: CLAUDE_ZAI_BASE_URL.to_string(),
            client,
            pool: std::sync::Mutex::new(crate::key_pool::KeyPool::new(
                GatewayPoolStrategy::RoundRobin,
            )),
            pool_cache: std::sync::Mutex::new(None),
        }
    }

    // -- 정책 ---------------------------------------------------------------

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub fn is_blocked(&self) -> bool {
        self.blocked.load(Ordering::Acquire)
    }

    /// 관측 라우팅이 가능한가: 켜져 있고 실제로 수신 중인가.
    pub fn is_routable(&self) -> bool {
        self.is_enabled() && self.port.load(Ordering::Acquire) != 0
    }

    pub fn set_policy(
        &self,
        enabled: Option<bool>,
        blocked: Option<bool>,
        pool_strategy: Option<GatewayPoolStrategy>,
    ) -> GatewayPolicy {
        if let Some(enabled) = enabled {
            self.enabled.store(enabled, Ordering::Release);
        }
        if let Some(blocked) = blocked {
            self.blocked.store(blocked, Ordering::Release);
        }
        if let Some(strategy) = pool_strategy {
            self.pool
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .set_strategy(strategy);
        }
        self.policy()
    }

    pub fn policy(&self) -> GatewayPolicy {
        GatewayPolicy {
            enabled: self.is_enabled(),
            blocked: self.is_blocked(),
            pool_strategy: self
                .pool
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .strategy(),
        }
    }

    // -- 수신 주소 -----------------------------------------------------------

    /// 서버가 바인드에 성공하면 알려 준다(0은 실패).
    pub fn set_listening(&self, port: u16) {
        self.port.store(port, Ordering::Release);
    }

    pub fn port(&self) -> u16 {
        self.port.load(Ordering::Acquire)
    }

    // -- 라우팅 발급 ---------------------------------------------------------

    /// 이 워크로드를 위한 게이트웨이 토큰을 발급한다. 같은 워크로드의 옛
    /// 토큰은 무효화한다(재발급·재시작 경로에서 항목이 두 배로 늘지 않게).
    /// 라우팅 불가면 `None`(호출자는 직접 라우팅을 유지한다).
    pub fn issue_route(
        &self,
        workload_id: &WorkloadId,
        session_id: &SessionId,
    ) -> Option<GatewayRoute> {
        if !self.is_routable() {
            return None;
        }
        let token = format!("{GATEWAY_TOKEN_PREFIX}{}", random_token());
        let now = epoch_ms();
        // 세션 발급 시점에 풀 키를 묶는다(진행 중 세션은 이 키를 유지한다).
        let key_id = {
            let snapshot = self.pool_snapshot_ids();
            let mut pool = self.pool.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(snapshot) = snapshot {
                pool.sync(&snapshot);
            }
            pool.bind_new(now)
        };
        {
            let mut sessions = self.lock_sessions();
            sessions.retain(|_, entry| entry.workload_id != *workload_id);
            sessions.insert(
                token.clone(),
                GwSession {
                    workload_id: workload_id.clone(),
                    session_id: session_id.clone(),
                    usage: GatewayUsage::default(),
                    model: None,
                    key_id,
                    started_epoch_ms: now,
                    last_seen_epoch_ms: now,
                },
            );
            evict_stale(&mut sessions);
        }
        Some(GatewayRoute {
            base_url: format!("http://127.0.0.1:{}", self.port()),
            token: Zeroizing::new(token),
        })
    }

    /// 셸 모드: 이미 해석된 라우팅을 게이트웨이 경유로 감싼다 — base URL을
    /// 로컬 게이트웨이로, 토큰을 1회용 게이트웨이 토큰으로 바꾸고 redactor도
    /// 그 토큰을 지우도록 다시 만든다(실제 키는 자식에 아예 없으므로 지울
    /// 것도 없다). 감쌀 수 없으면 아무것도 하지 않는다(직접 라우팅 유지).
    pub fn rewire(
        &self,
        resolved: &mut crate::claude_provider::ResolvedClaudeProvider,
        workload_id: &WorkloadId,
        session_id: &SessionId,
    ) -> bool {
        let Some(route) = self.issue_route(workload_id, session_id) else {
            return false;
        };
        resolved
            .env
            .insert("ANTHROPIC_BASE_URL".to_string(), route.base_url);
        resolved.token = route.token.clone();
        resolved.redactor = Arc::new(SecretRedactor::new([(*route.token).clone()]));
        true
    }

    /// 관리 모드 시작 시점: gate 프레임에 얹을 토큰을 게이트웨이 토큰으로
    /// 발급한다(`StartToken` 모양 그대로 — 호출자 코드가 갈라지지 않게).
    /// 발급하면 프레임 env의 base URL도 [`Self::frame_env`]가 게이트웨이로
    /// 다시 쓴다. 라우팅 불가면 `None`(호출자는 실제 키를 다시 읽는다).
    pub fn start_route(
        &self,
        workload_id: &WorkloadId,
        session_id: &SessionId,
    ) -> Option<crate::claude_provider::StartToken> {
        let route = self.issue_route(workload_id, session_id)?;
        Some(crate::claude_provider::StartToken {
            redactor: Arc::new(SecretRedactor::new([(*route.token).clone()])),
            token: route.token,
        })
    }

    /// 관리 실행의 gate 프레임 env에서 base URL을 확정한다. `via_gateway`
    /// 는 토큰 발급 판정([`Self::start_route`]의 결과)을 그대로 받는다 —
    /// 두 결정이 한 시점에 내려져 프레임의 URL과 토큰이 어긋나지 않는다.
    /// 게이트웨이 경로면 현재 포트를, 아니면 Z.ai 직접을 가리킨다(descriptor
    /// 에 남아 있던 값은 이 시점에 다시 쓴다).
    pub fn frame_env(
        &self,
        env: &mut std::collections::BTreeMap<String, String>,
        via_gateway: bool,
    ) {
        if via_gateway {
            env.insert(
                "ANTHROPIC_BASE_URL".to_string(),
                format!("http://127.0.0.1:{}", self.port()),
            );
        } else if env.contains_key("ANTHROPIC_BASE_URL") {
            env.insert(
                "ANTHROPIC_BASE_URL".to_string(),
                CLAUDE_ZAI_BASE_URL.to_string(),
            );
        }
    }

    /// 워크로드가 종료되면 장부 항목을 지운다(토큰도 함께 무효화된다).
    pub fn forget_workload(&self, workload_id: &WorkloadId) {
        self.lock_sessions()
            .retain(|_, entry| entry.workload_id != *workload_id);
    }

    // -- 상태 --------------------------------------------------------------

    pub fn status(&self) -> GatewayStatus {
        let sessions = self.lock_sessions();
        let mut list: Vec<_> = sessions
            .values()
            .map(|entry| term_contracts::gateway::GatewaySessionUsage {
                workload_id: entry.workload_id.clone(),
                session_id: entry.session_id.clone(),
                usage: entry.usage.clone(),
                model: entry.model.clone(),
                started_epoch_ms: entry.started_epoch_ms,
                last_seen_epoch_ms: entry.last_seen_epoch_ms,
            })
            .collect();
        list.sort_by(|a, b| a.workload_id.as_str().cmp(b.workload_id.as_str()));
        let (pool, pool_strategy) = {
            let snapshot = self.pool_snapshot_ids();
            let mut pool = self.pool.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(snapshot) = snapshot {
                pool.sync(&snapshot);
            }
            let view = pool.view(epoch_ms());
            let strategy = pool.strategy();
            (view, strategy)
        };
        GatewayStatus {
            enabled: self.is_enabled(),
            listening: self.port() != 0,
            port: self.port(),
            blocked: self.is_blocked(),
            totals: self
                .totals
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
            sessions: list,
            pool,
            pool_strategy,
        }
    }

    // -- 서버 내부 ----------------------------------------------------------

    fn lock_sessions(&self) -> std::sync::MutexGuard<'_, HashMap<String, GwSession>> {
        self.sessions.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// 인증: 헤더 값에서 토큰을 꺼내 등록된 세션인지 확인한다. `Bearer `
    /// 접두사는 붙여도 안 붙여도 받는다. 토큰 값은 로그에 절대 남기지 않는다.
    fn authenticate(&self, headers: &[(String, String)]) -> Option<String> {
        AUTH_HEADERS.iter().find_map(|name| {
            let raw = headers
                .iter()
                .rfind(|(key, _)| key == name)
                .map(|(_, value)| value.trim())?;
            let token = raw.strip_prefix("Bearer ").unwrap_or(raw);
            (!token.is_empty() && self.lock_sessions().contains_key(token))
                .then(|| token.to_string())
        })
    }

    /// 실제 Z.ai 키를 요청 시점에 읽는다(풀이 없을 때의 폴백 경로).
    fn single_key(&self) -> Option<Zeroizing<String>> {
        match term_secrets::LocalSecretStore::new(&self.secrets_root).read_zeroizing() {
            Ok(Some(key)) if !key.is_empty() => Some(key),
            _ => None,
        }
    }

    /// 풀 저장소 스냅숏(id·라벨 쌍) — 회전 엔진 `sync`용. 비밀은 버린다.
    /// 저장소를 읽지 못했으면 `None` — 호출자는 `sync`를 건너뛴다(빈 풀로
    /// 맞추면 격리·쿨다운 기록이 지워져 다음 정상 읽기에서 다시 맞는다).
    fn pool_snapshot_ids(&self) -> Option<Vec<(String, String)>> {
        Some(
            self.pool_entries_locked()?
                .into_iter()
                .map(|entry| (entry.id, entry.label))
                .collect(),
        )
    }

    /// TTL 이내의 캐시 또는 다시 읽은 풀 항목. 읽기에 실패하면 마지막으로
    /// 읽은 스냅숏을 그대로 쓰고(다음 호출이 다시 읽는다), 그것도 없으면
    /// `None`(단일 키 폴백이 이어받는다).
    fn pool_entries_locked(&self) -> Option<Vec<PoolCacheEntry>> {
        let now = epoch_ms();
        let mut cache = self.pool_cache.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(cached) = cache.as_ref() {
            if now.saturating_sub(cached.loaded_epoch_ms) < POOL_CACHE_TTL_MS {
                return Some(cached.entries.clone());
            }
        }
        match KeyPoolStore::new(&self.secrets_root).read_all_zeroizing() {
            Ok(read) => {
                let entries: Vec<PoolCacheEntry> = read
                    .into_iter()
                    .map(|(info, secret)| PoolCacheEntry {
                        id: info.id,
                        label: info.label,
                        secret,
                    })
                    .collect();
                *cache = Some(CachedPool {
                    entries: entries.clone(),
                    loaded_epoch_ms: now,
                });
                Some(entries)
            }
            Err(_) => {
                tracing::warn!("gateway key pool store unreadable; keeping last snapshot");
                cache.as_ref().map(|cached| cached.entries.clone())
            }
        }
    }

    /// 풀에 등록된 키가 하나라도 있는가(런치 시 단일 키 없이도 라우팅할 수
    /// 있는지 판단한다 — 쓸 수 있는 상태인지는 요청 시점에 따진다).
    pub fn has_pool_keys(&self) -> bool {
        self.pool_entries_locked()
            .is_some_and(|entries| !entries.is_empty())
    }

    /// 이 요청에 쓸 업스트림 키. 세션이 묶은 키를 존중하되(친화성) 쓸 수
    /// 없으면 갈아타고, 풀에 쓸 키가 없으면 단일 키로 폴백한다. `exclude`는
    /// 사전 스트림 페일오버에서 방금 실패한 키다.
    fn select_key(&self, bound: Option<&str>, exclude: Option<&str>) -> KeySelection {
        let now = epoch_ms();
        let entries = self.pool_entries_locked().unwrap_or_default();
        let mut exhausted = None;
        if !entries.is_empty() {
            let snapshot: Vec<(String, String)> = entries
                .iter()
                .map(|entry| (entry.id.clone(), entry.label.clone()))
                .collect();
            let mut pool = self.pool.lock().unwrap_or_else(|p| p.into_inner());
            pool.sync(&snapshot);
            if let Some(id) = pool.rebind(bound, exclude, now) {
                if let Some(entry) = entries.iter().find(|entry| entry.id == id) {
                    return KeySelection::Key {
                        id: Some(id),
                        secret: entry.secret.clone(),
                    };
                }
            }
            // 풀은 있는데 전원 사용 불가 — 단일 키라도 있으면 진행한다.
            exhausted = Some(pool.earliest_cooldown_remaining_ms(now));
        }
        match (self.single_key(), exhausted) {
            (Some(secret), _) => KeySelection::Key { id: None, secret },
            (None, Some(retry_after_ms)) => KeySelection::Exhausted { retry_after_ms },
            (None, None) => KeySelection::Missing,
        }
    }

    /// `failed` 대신 보낼 곳이 있는가 — 다른 풀 키나 단일 키. 없으면
    /// 페일오버하지 않고 받은 응답을 그대로 넘긴다(원래 429/401이 설정
    /// 오류 502로 바뀌지 않게).
    fn has_failover_key(&self, failed: &str) -> bool {
        self.pool
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .has_available_except(failed, epoch_ms())
            || self.single_key().is_some()
    }

    /// 세션에 묶인 풀 키 id(회전 결과를 다시 묶는 데 쓴다).
    fn session_key(&self, token: &str) -> Option<String> {
        self.lock_sessions()
            .get(token)
            .and_then(|entry| entry.key_id.clone())
    }

    /// 요청이 특정 풀 키로 나갔음을 기록한다(키별 요청 수·마지막 사용 시각).
    fn note_key_attempt(&self, key_id: Option<&str>) {
        if let Some(id) = key_id {
            self.pool
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record_attempt(id, epoch_ms());
            // 세션의 묶음도 따라간다(페일오버로 바뀐 경우).
        }
    }

    /// 페일오버로 키가 바뀌었으면 세션 묶음을 갈아탠다.
    fn rebind_session_key(&self, token: &str, key_id: Option<&str>) {
        if let Some(entry) = self.lock_sessions().get_mut(token) {
            entry.key_id = key_id.map(str::to_string);
        }
    }

    /// 사전 스트림 실패 결과를 회전 엔진에 기록한다.
    fn record_key_outcome(&self, key_id: Option<&str>, outcome: KeyOutcome, now_ms: u64) {
        let Some(id) = key_id else { return };
        let mut pool = self.pool.lock().unwrap_or_else(|p| p.into_inner());
        match outcome {
            KeyOutcome::Neutral => {}
            KeyOutcome::Success => pool.record_success(id),
            KeyOutcome::RateLimited { retry_after_ms } => {
                pool.record_rate_limited(id, retry_after_ms, now_ms)
            }
            KeyOutcome::Auth => pool.record_auth_failure(id),
            KeyOutcome::Transport => pool.record_transport_failure(id, now_ms),
        }
    }

    /// 응답 한 건을 장부에 기록한다. 세션 항목이 이미 정리됐으면 전체
    /// 누적에만 반영한다. `model`은 이 응답이 쓴 모델(관측된 경우).
    fn record(&self, token: &str, usage: &GatewayUsage, model: Option<&str>, ok: bool) {
        let now = epoch_ms();
        if let Some(entry) = self.lock_sessions().get_mut(token) {
            entry.usage.input_tokens += usage.input_tokens;
            entry.usage.output_tokens += usage.output_tokens;
            entry.usage.cache_read_tokens += usage.cache_read_tokens;
            entry.usage.cache_write_tokens += usage.cache_write_tokens;
            entry.usage.requests += 1;
            entry.usage.failures += u64::from(!ok);
            if model.is_some() {
                entry.model = model.map(str::to_string);
            }
            entry.last_seen_epoch_ms = now;
        }
        let totals = &mut *self.totals.lock().unwrap_or_else(|p| p.into_inner());
        totals.input_tokens += usage.input_tokens;
        totals.output_tokens += usage.output_tokens;
        totals.cache_read_tokens += usage.cache_read_tokens;
        totals.cache_write_tokens += usage.cache_write_tokens;
        totals.requests += 1;
        totals.failures += u64::from(!ok);
    }
}

// ---------------------------------------------------------------------------
// 서버

/// 게이트웨이 수신 루프. 바인드에 실패하면 경고만 남기고 돌아간다(게이트웨이
/// 없는 데몬 = 관측 없는 데몬 — 기능은 죽어도 데몬은 살아 있어야 한다).
pub async fn serve(gateway: Arc<Gateway>, mut shutdown: tokio::sync::watch::Receiver<bool>) {
    let listener = match TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(%error, "gateway listener bind failed; observability routing disabled");
            return;
        }
    };
    let port = listener
        .local_addr()
        .map(|addr| addr.port())
        .unwrap_or_default();
    gateway.set_listening(port);
    tracing::info!(port, "gateway listening on 127.0.0.1");
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let gateway = Arc::clone(&gateway);
                    tokio::spawn(async move {
                        handle_conn(gateway, stream).await;
                    });
                }
                Err(error) => {
                    // fd 고갈(EMFILE) 등은 즉시 다시 실패한다 — 바쁜 루프가
                    // 되지 않게 잠깐 쉰다.
                    tracing::debug!(%error, "gateway accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
        }
    }
}

/// 한 연결 = 한 요청(`Connection: close`로 응답한다). Claude Code는 요청마다
/// 새 연결을 만들므로 keep-alive 없이도 동작이 같다.
async fn handle_conn(gateway: Arc<Gateway>, stream: tokio::net::TcpStream) {
    let (mut reader, mut writer) = stream.into_split();
    // 헤드를 버퍼로 모은다(시작 줄 + 헤더, 상한 MAX_HEAD_BYTES).
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let head_len = loop {
        if let Some(end) = head_length(&buf) {
            break end;
        }
        if buf.len() > MAX_HEAD_BYTES {
            let _ = write_error(&mut writer, 431, "request head too large").await;
            return;
        }
        let mut chunk = [0u8; 4096];
        match tokio::time::timeout(Duration::from_secs(30), reader.read(&mut chunk)).await {
            Ok(Ok(0)) | Err(_) => return,
            Ok(Ok(n)) => buf.extend_from_slice(&chunk[..n]),
            Ok(Err(_)) => return,
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_len]).into_owned();
    let Some(ParsedHead {
        method,
        target,
        headers,
    }) = parse_head(&head)
    else {
        let _ = write_error(&mut writer, 400, "malformed request head").await;
        return;
    };

    // 로컬 건전성 확인용(토큰 불필요, 차단 대상 아님).
    if method == reqwest::Method::GET && target == "/healthz" {
        let body = br#"{"status":"ok"}"#;
        let _ = write_response(
            &mut writer,
            200,
            &[
                ("content-type", "application/json"),
                ("content-length", &body.len().to_string()),
            ],
            body,
        )
        .await;
        return;
    }

    // 인증: 등록된 게이트웨이 토큰이어야 한다.
    let Some(token) = gateway.authenticate(&headers) else {
        tracing::debug!(path = %target, "gateway request rejected: unknown session token");
        let _ = write_error(&mut writer, 401, "unknown or missing gateway session token").await;
        return;
    };
    // 킬 스위치.
    if gateway.is_blocked() {
        tracing::info!(path = %target, "gateway request blocked by policy");
        let _ = write_error(
            &mut writer,
            403,
            "blocked by IYAGI gateway policy (kill switch)",
        )
        .await;
        return;
    }

    // 경로: 업스트림 base URL 뒤에 그대로 붙으므로 `..` 세그먼트로 base
    // 경로 밖(같은 호스트의 다른 API)에 실제 키를 들고 나가지 못하게 한다.
    if !upstream_path_is_safe(&target) {
        let _ = write_error(&mut writer, 400, "request path is not allowed").await;
        return;
    }

    // 본문: Content-Length 필수(청크 요청은 411 — Claude Code는 고정 길이
    // JSON만 보낸다).
    let body = match read_body(&mut reader, &headers, &buf[head_len..]).await {
        Ok(body) => body,
        Err(code) => {
            let _ = write_error(&mut writer, code, "request body could not be read").await;
            return;
        }
    };

    // 업스트림 키 선택(풀 우선, 단일 키 폴백 — 09 §3.3) + 사전 스트림
    // 1회 페일오버. 시도는 최대 2회: 사전 스트림 401/429/402나 연결 수립
    // 실패이고 보낼 다른 키가 있으면, 결과를 회전 엔진에 기록하고 실패한
    // 키를 빼고 **한 번** 다시 보낸다. 5xx는 기록만 한다(요청이 처리됐을
    // 수 있다). SSE가 흐르기 시작한 뒤의 오류는 재시도하지 않는다.
    let body = bytes::Bytes::from(body);
    let session_bound = gateway.session_key(&token);
    let mut failed: Option<String> = None;
    let mut response: Option<(reqwest::Response, Option<String>)> = None;
    for attempt in 0..2 {
        let (key_id, key) = match gateway.select_key(session_bound.as_deref(), failed.as_deref()) {
            KeySelection::Key { id, secret } => (id, secret),
            KeySelection::Exhausted { retry_after_ms } => {
                let _ = write_pool_exhausted(&mut writer, retry_after_ms).await;
                return;
            }
            KeySelection::Missing => {
                let _ =
                    write_error(&mut writer, 502, "upstream credential is not configured").await;
                return;
            }
        };
        let url = format!("{}{}", gateway.upstream_base, target);
        let mut request = gateway.client.request(method.clone(), &url);
        for name in FORWARD_REQUEST_HEADERS {
            for (_, value) in headers.iter().filter(|(key, _)| key == name) {
                request = request.header(name, value.clone());
            }
        }
        if !headers.iter().any(|(key, _)| key == "content-type") {
            request = request.header("content-type", "application/json");
        }
        let request = request
            .header("authorization", format!("Bearer {}", key.as_str()))
            .header("x-api-key", key.as_str())
            .body(body.clone());
        let can_fail_over = |key_id: &Option<String>| {
            attempt == 0
                && key_id
                    .as_deref()
                    .is_some_and(|id| gateway.has_failover_key(id))
        };
        match request.send().await {
            Ok(upstream) => {
                let status = upstream.status().as_u16();
                // 키는 쓰였다 — 장부(요청 수)와 세션 친화성 갱신.
                gateway.note_key_attempt(key_id.as_deref());
                gateway.rebind_session_key(&token, key_id.as_deref());
                let outcome = key_outcome(status, &upstream);
                gateway.record_key_outcome(key_id.as_deref(), outcome, epoch_ms());
                let retryable =
                    matches!(outcome, KeyOutcome::Auth | KeyOutcome::RateLimited { .. });
                if retryable && can_fail_over(&key_id) {
                    tracing::info!(path = %target, status, "gateway pre-stream failure; failing over");
                    failed = key_id;
                    continue;
                }
                response = Some((upstream, key_id));
                break;
            }
            Err(error) => {
                tracing::warn!(path = %target, %error, "gateway upstream request failed");
                gateway.record_key_outcome(key_id.as_deref(), KeyOutcome::Transport, epoch_ms());
                // 연결 수립 실패만 다시 보낸다 — 그 밖의 오류는 본문이 이미
                // 업스트림에 닿았을 수 있다(두 번 실행·과금 위험).
                if error.is_connect() && can_fail_over(&key_id) {
                    failed = key_id;
                    continue;
                }
                let _ = write_error(&mut writer, 502, "upstream request failed").await;
                return;
            }
        }
    }
    let Some((response, used_key)) = response else {
        let _ = write_error(&mut writer, 502, "upstream request failed").await;
        return;
    };

    let status = response.status().as_u16();
    let forward: Vec<(String, String)> = FORWARD_RESPONSE_HEADERS
        .iter()
        .filter_map(|name| {
            response
                .headers()
                .get(*name)
                .and_then(|value| value.to_str().ok())
                .map(|value| ((*name).to_string(), value.to_string()))
        })
        .collect();
    let is_sse = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"));
    let counts_usage = target.split('?').next() == Some("/v1/messages");

    if is_sse {
        let mut scan = SseUsageScan::default();
        if write_stream_head(&mut writer, status, &forward)
            .await
            .is_err()
        {
            return;
        }
        let mut transport_ok = true;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    scan.feed(&bytes);
                    if write_chunk(&mut writer, &bytes).await.is_err() {
                        // 클라이언트가 떠났다 — 키 탓이 아니다.
                        transport_ok = false;
                        break;
                    }
                }
                Err(_) => {
                    // 업스트림이 중간에 끊겼다. 종결 청크를 쓰지 않고 닫아
                    // 클라이언트가 잘린 본문을 완결된 응답으로 믿지 않게
                    // 한다(네트워크 오류로 보고 스스로 재시도한다).
                    transport_ok = false;
                    gateway.record_key_outcome(
                        used_key.as_deref(),
                        KeyOutcome::Transport,
                        epoch_ms(),
                    );
                    break;
                }
            }
        }
        if transport_ok {
            let _ = writer.write_all(b"0\r\n\r\n").await;
            let _ = writer.flush().await;
        } else {
            let _ = writer.shutdown().await;
        }
        if counts_usage {
            gateway.record(
                &token,
                &scan.usage(),
                scan.model(),
                transport_ok && status < 400,
            );
        }
    } else {
        let mut payload: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        let mut relay_error = None;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) if payload.len() + bytes.len() <= MAX_RESPONSE_BUFFER => {
                    payload.extend_from_slice(&bytes);
                }
                Ok(_) => {
                    relay_error = Some("upstream response exceeded the gateway buffer");
                    break;
                }
                Err(_) => {
                    gateway.record_key_outcome(
                        used_key.as_deref(),
                        KeyOutcome::Transport,
                        epoch_ms(),
                    );
                    relay_error = Some("upstream response was interrupted");
                    break;
                }
            }
        }
        // 잘린 본문을 원래 상태 코드로 넘기지 않는다(중간이 빠진 JSON이
        // 맞는 content-length를 달고 나가면 클라이언트는 성공으로 믿는다).
        if let Some(message) = relay_error {
            tracing::warn!(path = %target, message, "gateway could not relay upstream body");
            let _ = write_error(&mut writer, 502, message).await;
            if counts_usage {
                gateway.record(&token, &GatewayUsage::default(), None, false);
            }
            return;
        }
        let mut usage = GatewayUsage::default();
        let mut model = None;
        if counts_usage {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&payload) {
                model = value
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                if let Some(u) = value.get("usage") {
                    usage.input_tokens = u
                        .get("input_tokens")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    usage.output_tokens = u
                        .get("output_tokens")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    usage.cache_read_tokens = u
                        .get("cache_read_input_tokens")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    usage.cache_write_tokens = u
                        .get("cache_creation_input_tokens")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                }
            }
        }
        let mut headers_out = forward;
        headers_out.push(("content-length".to_string(), payload.len().to_string()));
        let borrowed: Vec<(&str, &str)> = headers_out
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let wrote = write_response(&mut writer, status, &borrowed, &payload)
            .await
            .is_ok();
        if counts_usage {
            gateway.record(&token, &usage, model.as_deref(), wrote && status < 400);
        }
    }
}

/// 헤드(시작 줄 + 헤더)가 완결됐다면 그 길이. CRLF 우선, LF 전용도 받는다.
fn head_length(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|pos| pos + 4)
        .or_else(|| buf.windows(2).position(|w| w == b"\n\n").map(|pos| pos + 2))
}

/// 파싱된 요청 헤드: (메서드, 대상 경로+쿼리, 소문자 이름 헤더 목록).
struct ParsedHead {
    method: reqwest::Method,
    target: String,
    headers: Vec<(String, String)>,
}

fn parse_head(head: &str) -> Option<ParsedHead> {
    let mut lines = head.split("\r\n").flat_map(|line| line.split('\n'));
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    let method = reqwest::Method::from_bytes(method.as_bytes()).ok()?;
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    Some(ParsedHead {
        method,
        target: target.to_string(),
        headers,
    })
}

/// 요청 본문 읽기. Content-Length 필수, 상한 검사. 실패는 응답 상태 코드로.
async fn read_body(
    reader: &mut OwnedReadHalf,
    headers: &[(String, String)],
    prelude: &[u8],
) -> Result<Vec<u8>, u16> {
    if headers.iter().any(|(name, value)| {
        name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked")
    }) {
        return Err(411);
    }
    let length = headers
        .iter()
        .rfind(|(name, _)| name == "content-length")
        .map(|(_, value)| value.parse::<usize>());
    let length = match length {
        Some(length) => length.map_err(|_| 400u16)?,
        None => {
            return if prelude.is_empty() {
                Ok(Vec::new())
            } else {
                Err(411)
            }
        }
    };
    if length > MAX_BODY_BYTES {
        return Err(413);
    }
    let mut body = prelude.to_vec();
    if body.len() > length {
        return Err(400);
    }
    while body.len() < length {
        let mut chunk = [0u8; 16384];
        let want = (length - body.len()).min(chunk.len());
        let n = tokio::time::timeout(Duration::from_secs(120), reader.read(&mut chunk[..want]))
            .await
            .map_err(|_| 408u16)?
            .map_err(|_| 400u16)?;
        if n == 0 {
            return Err(400);
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Ok(body)
}

/// Anthropic 오류 모양의 짧은 응답(401/403/4xx/502). CLI가 그대로 보여준다.
async fn write_error(
    writer: &mut OwnedWriteHalf,
    status: u16,
    message: &str,
) -> std::io::Result<()> {
    write_error_with(writer, status, message, &[]).await
}

/// [`write_error`] + 추가 헤더(예: `retry-after`).
async fn write_error_with(
    writer: &mut OwnedWriteHalf,
    status: u16,
    message: &str,
    extra: &ResponseHeaders<'_>,
) -> std::io::Result<()> {
    let kind = match status {
        401 => "authentication_error",
        403 => "permission_error",
        413 | 431 => "request_too_large",
        429 => "rate_limit_error",
        _ => "api_error",
    };
    let body = serde_json::json!({
        "type": "error",
        "error": {"type": kind, "message": message}
    })
    .to_string();
    let length = body.len().to_string();
    let mut headers: Vec<(&str, &str)> = vec![
        ("content-type", "application/json"),
        ("content-length", &length),
    ];
    headers.extend_from_slice(extra);
    write_response(writer, status, &headers, body.as_bytes()).await
}

/// 응답 헤더 묶음(이름-값 순서쌍).
type ResponseHeaders<'a> = [(&'a str, &'a str)];

/// 단일 본문 응답(비스트리밍·오류 공용).
async fn write_response(
    writer: &mut OwnedWriteHalf,
    status: u16,
    headers: &ResponseHeaders<'_>,
    body: &[u8],
) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status} {}\r\n", reason_phrase(status));
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("connection: close\r\n\r\n");
    writer.write_all(head.as_bytes()).await?;
    writer.write_all(body).await?;
    writer.flush().await
}

/// 스트리밍(SSE) 응답의 시작 줄 — 본문은 청크 인코딩으로 이어진다.
async fn write_stream_head(
    writer: &mut OwnedWriteHalf,
    status: u16,
    headers: &[(String, String)],
) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status} {}\r\n", reason_phrase(status));
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("transfer-encoding: chunked\r\nconnection: close\r\n\r\n");
    writer.write_all(head.as_bytes()).await?;
    writer.flush().await
}

/// HTTP/1.1 청크 한 개.
async fn write_chunk(writer: &mut OwnedWriteHalf, bytes: &[u8]) -> std::io::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    writer
        .write_all(format!("{:x}\r\n", bytes.len()).as_bytes())
        .await?;
    writer.write_all(bytes).await?;
    writer.write_all(b"\r\n").await
}

/// 사전 스트림 단계의 키 결과(09 §3.3 표).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyOutcome {
    Success,
    RateLimited {
        retry_after_ms: Option<u64>,
    },
    Auth,
    Transport,
    /// 키와 무관한 결과(403·그 밖의 4xx — 요청 자체의 문제). 기록하지 않는다.
    Neutral,
}

/// 업스트림 상태 → 키 결과. 403은 격리하지 않는다: 키가 아니라 요청(허용
/// 안 된 모델·정책)의 문제인 경우가 많아, 요청 한 건이 풀 전체를 영구
/// 격리시킬 수 있다.
fn key_outcome(status: u16, response: &reqwest::Response) -> KeyOutcome {
    match status {
        200..=299 => KeyOutcome::Success,
        401 => KeyOutcome::Auth,
        429 | 402 => KeyOutcome::RateLimited {
            retry_after_ms: retry_after_ms(response),
        },
        500..=599 => KeyOutcome::Transport,
        _ => KeyOutcome::Neutral,
    }
}

/// `Retry-After`(초 단위 정수만)를 밀리초로. HTTP-DATE 형식은 무시한다.
fn retry_after_ms(response: &reqwest::Response) -> Option<u64> {
    response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|seconds| seconds.saturating_mul(1000))
}

/// 업스트림 경로로 넘겨도 되는가: `/`로 시작하고 `.`/`..` 세그먼트(퍼센트
/// 인코딩 포함)가 없어야 한다. 질의 문자열은 검사하지 않는다.
fn upstream_path_is_safe(target: &str) -> bool {
    let path = target.split('?').next().unwrap_or_default();
    path.starts_with('/')
        && path.split('/').all(|segment| {
            let decoded = segment.to_ascii_lowercase().replace("%2e", ".");
            decoded != "." && decoded != ".."
        })
}

/// 요청에 쓸 업스트림 키 선택 결과.
enum KeySelection {
    /// 풀 키(`id`) 또는 단일 키(`id = None`).
    Key {
        id: Option<String>,
        secret: Zeroizing<String>,
    },
    /// 풀은 있지만 전원 쿨다운·격리이고 단일 키도 없다. 쿨다운이면 가장
    /// 빨리 풀리는 시각까지 남은 시간.
    Exhausted { retry_after_ms: Option<u64> },
    /// 어떤 자격 증명도 없다.
    Missing,
}

/// 풀 전원 사용 불가. 쿨다운이면 429 + `Retry-After`(CLI가 그만큼 기다렸다
/// 다시 보낸다), 전원 격리면 401 — "설정 안 됨" 502로 뭉개지 않는다.
async fn write_pool_exhausted(
    writer: &mut OwnedWriteHalf,
    retry_after_ms: Option<u64>,
) -> std::io::Result<()> {
    match retry_after_ms {
        Some(ms) => {
            let seconds = ms.div_ceil(1000).max(1).to_string();
            write_error_with(
                writer,
                429,
                "all API keys in the pool are cooling down",
                &[("retry-after", &seconds)],
            )
            .await
        }
        None => write_error(
            writer,
            401,
            "all API keys in the pool were rejected by the provider; re-register them in Settings",
        )
        .await,
    }
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        529 => "Overloaded",
        _ => "Response",
    }
}

// ---------------------------------------------------------------------------
// SSE 사용량 스캔

/// SSE 데이터 줄(`data: {...}`)을 한 줄씩 보며 사용량을 모은다. 본문 자체는
/// 절대 저장하지 않는다 — 파싱에 필요한 값만 꺼낸다.
#[derive(Default)]
struct SseUsageScan {
    line: Vec<u8>,
    input_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    output_tokens: u64,
    model: Option<String>,
}

impl SseUsageScan {
    fn feed(&mut self, chunk: &[u8]) {
        for &byte in chunk {
            if byte == b'\n' {
                if self.line.last() == Some(&b'\r') {
                    self.line.pop();
                }
                let line = std::mem::take(&mut self.line);
                self.parse_line(&line);
            } else if self.line.len() < MAX_SSE_LINE {
                self.line.push(byte);
            }
        }
    }

    fn parse_line(&mut self, line: &[u8]) {
        let Some(payload) = line.strip_prefix(b"data:") else {
            return;
        };
        let payload = payload.strip_prefix(b" ").unwrap_or(payload);
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload) else {
            return;
        };
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("message_start") => {
                if self.model.is_none() {
                    self.model = value
                        .pointer("/message/model")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string);
                }
                let usage = value.pointer("/message/usage");
                self.input_tokens = usage
                    .and_then(|u| u.get("input_tokens"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
                    .max(self.input_tokens);
                self.cache_read_tokens = usage
                    .and_then(|u| u.get("cache_read_input_tokens"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
                    .max(self.cache_read_tokens);
                self.cache_write_tokens = usage
                    .and_then(|u| u.get("cache_creation_input_tokens"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
                    .max(self.cache_write_tokens);
            }
            // message_delta의 output_tokens는 누적값 — 마지막 것이 최종.
            Some("message_delta") => {
                if let Some(output) = value
                    .pointer("/usage/output_tokens")
                    .and_then(serde_json::Value::as_u64)
                {
                    self.output_tokens = output.max(self.output_tokens);
                }
            }
            _ => {}
        }
    }

    fn usage(&self) -> GatewayUsage {
        GatewayUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens,
            requests: 0,
            failures: 0,
        }
    }

    fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }
}

// ---------------------------------------------------------------------------
// 유틸

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 192비트 난수 토큰(base64url). 추측 불가능성이 전부라 형식은 자유다.
fn random_token() -> String {
    use base64::Engine as _;
    use rand::RngCore as _;
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// 세션 맵 상한 초과 시 가장 오래 쉰 항목부터 버긴다.
fn evict_stale(sessions: &mut HashMap<String, GwSession>) {
    while sessions.len() > MAX_TRACKED_SESSIONS {
        let Some(oldest) = sessions
            .iter()
            .min_by_key(|(_, entry)| entry.last_seen_epoch_ms)
            .map(|(token, _)| token.clone())
        else {
            break;
        };
        sessions.remove(&oldest);
    }
}

// ---------------------------------------------------------------------------
// 테스트

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write as _};
    use std::net::TcpListener as StdListener;
    use term_contracts::gateway::{PoolKeyStatus, PoolKeyViewState};

    const REAL_KEY: &str = "real-zai-key-0123456789abcdef";

    fn gateway_with_key(dir: &std::path::Path) -> Gateway {
        term_secrets::LocalSecretStore::new(dir)
            .write(REAL_KEY)
            .expect("write test key");
        let gateway = Gateway::new(dir.to_path_buf());
        gateway.enabled.store(true, Ordering::Release);
        gateway
    }

    fn ids() -> (WorkloadId, SessionId) {
        (WorkloadId::generate(), SessionId::generate())
    }

    /// 아무도 듣지 않는 업스트림(연결 거부). 닿으면 502가 된다.
    const CLOSED_UPSTREAM: &str = "http://127.0.0.1:1";

    /// 연결마다 준비된 응답을 하나씩 돌려주는 업스트림. 응답을 다 쓰면
    /// 리스너를 닫는다 — 그 뒤의 연결은 거부(게이트웨이 쪽 502)로 드러난다.
    /// 반환: (주소, 받은 요청 헤드 목록을 돌려주는 스레드).
    fn scripted_upstream(
        responses: Vec<&'static str>,
    ) -> (SocketAddr, std::thread::JoinHandle<Vec<String>>) {
        let upstream = StdListener::bind("127.0.0.1:0").unwrap();
        let addr = upstream.local_addr().unwrap();
        let thread = std::thread::spawn(move || {
            let mut heads = Vec::new();
            for response in responses {
                let (mut sock, _) = upstream.accept().unwrap();
                let mut buf = [0u8; 8192];
                let n = sock.read(&mut buf).unwrap();
                heads.push(String::from_utf8_lossy(&buf[..n]).into_owned());
                sock.write_all(response.as_bytes()).unwrap();
            }
            heads
        });
        (addr, thread)
    }

    /// 풀 키만 등록된(단일 키 없음) 게이트웨이를 띄운다. 반환: (게이트웨이,
    /// 포트, 서버 태스크, 임시 디렉터리).
    async fn pool_gateway(
        upstream: String,
        keys: &[(&str, &str)],
        strategy: GatewayPoolStrategy,
    ) -> (
        Arc<Gateway>,
        u16,
        tokio::task::JoinHandle<()>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyPoolStore::new(dir.path());
        for (label, key) in keys {
            store.add("zai", label, key).unwrap();
        }
        let mut raw = Gateway::new(dir.path().to_path_buf());
        raw.upstream_base = upstream;
        let gateway = Arc::new(raw);
        gateway.set_policy(Some(true), None, Some(strategy));
        let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = server.local_addr().unwrap().port();
        gateway.set_listening(port);
        let gw_server = Arc::clone(&gateway);
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = server.accept().await.unwrap();
                let gw = Arc::clone(&gw_server);
                tokio::spawn(async move {
                    handle_conn(gw, stream).await;
                });
            }
        });
        (gateway, port, task, dir)
    }

    /// 게이트웨이에 요청 한 건을 보내고 원시 응답을 통째로 읽는다.
    async fn roundtrip(port: u16, token: &str, path: &str) -> String {
        use tokio::io::AsyncWriteExt as _;
        let body = r#"{"model":"glm-5.3[1m]"}"#;
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
        let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        sock.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut sock, &mut response)
            .await
            .unwrap();
        String::from_utf8_lossy(&response).into_owned()
    }

    fn pool_state(gateway: &Gateway, label: &str) -> PoolKeyStatus {
        gateway
            .status()
            .pool
            .into_iter()
            .find(|key| key.label == label)
            .unwrap()
    }

    #[test]
    fn issue_route_requires_enabled_and_port() {
        let dir = tempfile::tempdir().unwrap();
        let gateway = Gateway::new(dir.path().to_path_buf());
        let (workload, session) = ids();
        assert!(gateway.issue_route(&workload, &session).is_none());
        gateway.enabled.store(true, Ordering::Release);
        assert!(gateway.issue_route(&workload, &session).is_none());
        gateway.set_listening(8080);
        assert!(gateway.issue_route(&workload, &session).is_some());
    }

    #[test]
    fn rewire_swaps_base_url_token_and_redactor() {
        let dir = tempfile::tempdir().unwrap();
        let gateway = gateway_with_key(dir.path());
        gateway.set_listening(47112);
        let (workload, session) = ids();
        let mut resolved = crate::claude_provider::ResolvedClaudeProvider {
            env: crate::claude_provider::zai_env("glm-5.3[1m]"),
            token: Zeroizing::new("real-zai-key".to_string()),
            env_remove: crate::claude_provider::routed_env_remove(),
            redactor: Arc::new(SecretRedactor::new(["real-zai-key".to_string()])),
            provider: term_contracts::launch::ClaudeProvider::ZaiCodingPlan {
                main_model: "glm-5.3[1m]".to_string(),
            },
        };
        assert!(gateway.rewire(&mut resolved, &workload, &session));
        assert_eq!(
            resolved.env.get("ANTHROPIC_BASE_URL").map(String::as_str),
            Some("http://127.0.0.1:47112")
        );
        let token = resolved.token.to_string();
        assert!(token.starts_with(GATEWAY_TOKEN_PREFIX));
        // 자식 출력에서 게이트웨이 토큰이 지워진다(실제 키는 자식에 없다).
        let mut line = format!("token={token}");
        resolved.redactor.redact_plain(&mut line);
        assert_eq!(line, "token=[redacted]");
        // 같은 워크로드 재발급은 옛 토큰을 무효화한다.
        let second = gateway.issue_route(&workload, &session).unwrap();
        assert_ne!(*second.token, *resolved.token);
        assert_eq!(gateway.lock_sessions().len(), 1);
    }

    #[test]
    fn frame_env_pins_the_current_decision() {
        let dir = tempfile::tempdir().unwrap();
        let gateway = gateway_with_key(dir.path());
        gateway.set_listening(47113);
        let mut env = crate::claude_provider::zai_env("glm-5.3[1m]");
        // 게이트웨이 토큰을 발급받은 프레임: 현재 포트로 확정.
        gateway.frame_env(&mut env, true);
        assert_eq!(
            env.get("ANTHROPIC_BASE_URL").map(String::as_str),
            Some("http://127.0.0.1:47113")
        );
        // 직접 라우팅 프레임: descriptor가 게이트웨이 URL을 물고 있어도
        // 직접 경로로 되돌린다.
        env.insert(
            "ANTHROPIC_BASE_URL".to_string(),
            "http://127.0.0.1:9999".to_string(),
        );
        gateway.frame_env(&mut env, false);
        assert_eq!(
            env.get("ANTHROPIC_BASE_URL").map(String::as_str),
            Some(CLAUDE_ZAI_BASE_URL)
        );
    }

    #[test]
    fn forget_workload_drops_the_session_entry() {
        let dir = tempfile::tempdir().unwrap();
        let gateway = gateway_with_key(dir.path());
        gateway.set_listening(8080);
        let (workload, session) = ids();
        gateway.issue_route(&workload, &session);
        assert_eq!(gateway.lock_sessions().len(), 1);
        gateway.forget_workload(&workload);
        assert_eq!(gateway.lock_sessions().len(), 0);
    }

    #[test]
    fn policy_toggles_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let gateway = Gateway::new(dir.path().to_path_buf());
        assert!(!gateway.is_routable());
        let policy = gateway.set_policy(Some(true), None, None);
        assert!(policy.enabled && !policy.blocked);
        let policy = gateway.set_policy(None, Some(true), None);
        assert!(policy.enabled && policy.blocked);
    }

    #[test]
    fn sse_scan_accumulates_usage_across_chunks() {
        let mut scan = SseUsageScan::default();
        scan.feed(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"glm-5.3");
        scan.feed(b"[1m]\",\"usage\":{\"input_tokens\":120,\"cache_read_input_tokens\":30,\"cache_creation_input_tokens\":7}}}\n\n");
        scan.feed(
            b"data: {\"type\":\"message_delta\",\"delta\":{},\"usage\":{\"output_tokens\":41}}\n",
        );
        scan.feed(
            b"data: {\"type\":\"message_delta\",\"delta\":{},\"usage\":{\"output_tokens\":87}}\n",
        );
        let usage = scan.usage();
        assert_eq!(usage.input_tokens, 120);
        assert_eq!(usage.cache_read_tokens, 30);
        assert_eq!(usage.cache_write_tokens, 7);
        // 누적 출력의 최종값.
        assert_eq!(usage.output_tokens, 87);
        assert_eq!(scan.model(), Some("glm-5.3[1m]"));
    }

    #[test]
    fn head_detection_and_parsing() {
        let head =
            "POST /v1/messages?beta=true HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer t\r\n\r\n";
        assert_eq!(head_length(head.as_bytes()), Some(head.len()));
        let parsed = parse_head(head).unwrap();
        assert_eq!(parsed.method, reqwest::Method::POST);
        assert_eq!(parsed.target, "/v1/messages?beta=true");
        assert_eq!(
            parsed
                .headers
                .iter()
                .rfind(|(n, _)| n == "authorization")
                .map(|(_, v)| v.as_str()),
            Some("Bearer t")
        );
        // LF 전용 헤드도 받는다(헤드 길이 28: 시작 줄 21 + `A: b` 5 + 빈 줄 2).
        assert_eq!(
            head_length(b"GET /healthz HTTP/1.1\nA: b\n\nrest"),
            Some(28)
        );
    }

    /// 업스트림(SSE 목업) ↔ 게이트웨이 ↔ 클라이언트 전 구간: 실제 키만
    /// 업스트림으로 가고, 게이트웨이 토큰은 새지 않으며, 사용량이 장부에
    /// 남는다.
    #[test]
    fn end_to_end_proxy_streams_usage_and_authenticates() {
        let upstream = StdListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let upstream_thread = std::thread::spawn(move || {
            let (mut sock, _) = upstream.accept().unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; 8192];
            loop {
                let n = sock.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                seen.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&seen).into_owned();
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let length: usize = text
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("content-length"))
                        .and_then(|l| l.split(':').nth(1))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    if seen.len() >= head_end + 4 + length {
                        break;
                    }
                }
            }
            let all = String::from_utf8_lossy(&seen).into_owned();
            let (head, body) = all.split_once("\r\n\r\n").unwrap();
            // 업스트림은 실제 키를 받아야 하고 게이트웨이 토큰은 없어야 한다.
            assert!(head.contains("Bearer real-zai-key"), "{head}");
            assert!(!head.contains(GATEWAY_TOKEN_PREFIX), "{head}");
            assert!(head.contains("anthropic-version: 2023-06-01"), "{head}");
            assert_eq!(body, "{\"model\":\"glm-5.3[1m]\",\"stream\":true}");
            let sse = concat!(
                "event: message_start\n",
                "data: {\"type\":\"message_start\",\"message\":{\"model\":\"glm-5.3[1m]\",\"usage\":{\"input_tokens\":11}}}\n\n",
                "event: message_delta\n",
                "data: {\"type\":\"message_delta\",\"delta\":{},\"usage\":{\"output_tokens\":5}}\n\n",
                "event: message_stop\n",
                "data: {\"type\":\"message_stop\"}\n\n",
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
                sse.len(),
                sse
            );
            sock.write_all(response.as_bytes()).unwrap();
        });

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let dir = tempfile::tempdir().unwrap();
            let mut raw = gateway_with_key(dir.path());
            raw.upstream_base = format!("http://{upstream_addr}/");
            let gateway = Arc::new(raw);
            let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = server.local_addr().unwrap().port();
            gateway.set_listening(port);
            let gw_server = Arc::clone(&gateway);
            let task = tokio::spawn(async move {
                loop {
                    let (stream, _) = server.accept().await.unwrap();
                    let gw = Arc::clone(&gw_server);
                    tokio::spawn(async move {
                        handle_conn(gw, stream).await;
                    });
                }
            });

            let (workload, session) = ids();
            let route = gateway.issue_route(&workload, &session).unwrap();
            let body = serde_json::json!({"model": "glm-5.3[1m]", "stream": true}).to_string();
            let request = format!(
                "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nanthropic-version: 2023-06-01\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                route.token.as_str(),
                body.len(),
                body
            );
            let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            use tokio::io::AsyncWriteExt as _;
            sock.write_all(request.as_bytes()).await.unwrap();
            let mut response = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut sock, &mut response)
                .await
                .unwrap();
            let text = String::from_utf8_lossy(&response);
            assert!(text.starts_with("HTTP/1.1 200"), "{text}");
            assert!(text.contains("text/event-stream"));
            // 청크 인코딩으로 SSE가 그대로 흘렀다.
            assert!(text.contains("message_start"), "{text}");
            task.abort();
            let status = gateway.status();
            assert_eq!(status.totals.input_tokens, 11);
            assert_eq!(status.totals.output_tokens, 5);
            assert_eq!(status.totals.requests, 1);
            assert_eq!(status.sessions.len(), 1);
            assert_eq!(status.sessions[0].usage.input_tokens, 11);
            assert_eq!(status.sessions[0].model.as_deref(), Some("glm-5.3[1m]"));
        });
        upstream_thread.join().unwrap();
    }

    /// 사전 스트림 1회 페일오버(09 §3.3): 첫 키가 429를 주면 같은 요청을
    /// 다음 키로 다시 보내고, 클라이언트는 200을 받는다. 첫 키는 쿨다운,
    /// 둘째 키의 장부에 요청이 남는다.
    #[test]
    fn pre_stream_failover_rotates_to_next_key() {
        const KEY_A: &str = "pool-key-aaaa1111";
        const KEY_B: &str = "pool-key-bbbb2222";

        let upstream = StdListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let upstream_thread = std::thread::spawn(move || {
            // 첫 연결: 키 A → 429(짧은 Retry-After).
            {
                let (mut sock, _) = upstream.accept().unwrap();
                let mut buf = [0u8; 8192];
                let n = sock.read(&mut buf).unwrap();
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                assert!(head.contains("Bearer pool-key-aaaa1111"), "{head}");
                sock.write_all(b"HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\nretry-after: 2\r\nconnection: close\r\ncontent-length: 0\r\n\r\n").unwrap();
            }
            // 둘째 연결: 키 B → 200 SSE(사용량 포함).
            {
                let (mut sock, _) = upstream.accept().unwrap();
                let mut buf = [0u8; 8192];
                let n = sock.read(&mut buf).unwrap();
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                assert!(head.contains("Bearer pool-key-bbbb2222"), "{head}");
                let sse = concat!(
                    "event: message_start\n",
                    "data: {\"type\":\"message_start\",\"message\":{\"model\":\"glm-5.3[1m]\",\"usage\":{\"input_tokens\":7}}}\n\n",
                    "event: message_delta\n",
                    "data: {\"type\":\"message_delta\",\"delta\":{},\"usage\":{\"output_tokens\":3}}\n\n",
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
                    sse.len(),
                    sse
                );
                sock.write_all(response.as_bytes()).unwrap();
            }
        });

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let dir = tempfile::tempdir().unwrap();
            // 풀에 키 두 개(단일 키 저장소는 비워 둔다 — 풀 경로 강제).
            let pool_store = KeyPoolStore::new(dir.path());
            pool_store.add("zai", "first", KEY_A).unwrap();
            pool_store.add("zai", "second", KEY_B).unwrap();
            let mut raw = Gateway::new(dir.path().to_path_buf());
            raw.upstream_base = format!("http://{upstream_addr}/");
            let gateway = Arc::new(raw);
            let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = server.local_addr().unwrap().port();
            gateway.set_listening(port);
            gateway.enabled.store(true, Ordering::Release);
            let gw_server = Arc::clone(&gateway);
            let task = tokio::spawn(async move {
                loop {
                    let (stream, _) = server.accept().await.unwrap();
                    let gw = Arc::clone(&gw_server);
                    tokio::spawn(async move {
                        handle_conn(gw, stream).await;
                    });
                }
            });

            let (workload, session) = ids();
            let route = gateway.issue_route(&workload, &session).unwrap();
            let body = serde_json::json!({"model": "glm-5.3[1m]", "stream": true}).to_string();
            let request = format!(
                "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                route.token.as_str(),
                body.len(),
                body
            );
            let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            use tokio::io::AsyncWriteExt as _;
            sock.write_all(request.as_bytes()).await.unwrap();
            let mut response = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut sock, &mut response)
                .await
                .unwrap();
            let text = String::from_utf8_lossy(&response);
            assert!(text.starts_with("HTTP/1.1 200"), "페일오버 후 200: {text}");
            assert!(text.contains("message_start"));
            task.abort();

            let status = gateway.status();
            // 클라이언트가 본 요청은 하나(429는 밖으로 새지 않았다).
            assert_eq!(status.totals.requests, 1);
            assert_eq!(status.totals.input_tokens, 7);
            assert_eq!(status.pool.len(), 2);
            let first = status.pool.iter().find(|k| k.label == "first").unwrap();
            let second = status.pool.iter().find(|k| k.label == "second").unwrap();
            assert_eq!(first.state, term_contracts::gateway::PoolKeyViewState::Cooling);
            assert!(first.cooldown_remaining_ms.unwrap() > 0);
            assert_eq!(second.state, term_contracts::gateway::PoolKeyViewState::Active);
            assert_eq!(second.requests, 1);
        });
        upstream_thread.join().unwrap();
    }

    /// 인증 실패(모르는 토큰) → 401 Anthropic 오류 모양, 장부 무변화.
    #[test]
    fn unknown_token_gets_401_and_never_reaches_upstream() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let dir = tempfile::tempdir().unwrap();
            let mut raw = gateway_with_key(dir.path());
            // 닫힌 포트 — 업스트림에 닿으면 401이 아니라 502가 된다.
            raw.upstream_base = CLOSED_UPSTREAM.to_string();
            let gateway = Arc::new(raw);
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            gateway.set_listening(port);
            let gw = Arc::clone(&gateway);
            let task = tokio::spawn(async move {
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    let gw = Arc::clone(&gw);
                    tokio::spawn(async move {
                        handle_conn(gw, stream).await;
                    });
                }
            });
            let request = "POST /v1/messages HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer iyagi-gw-bogus\r\ncontent-length: 2\r\n\r\n{}";
            let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            use tokio::io::AsyncWriteExt as _;
            sock.write_all(request.as_bytes()).await.unwrap();
            let mut response = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut sock, &mut response)
                .await
                .unwrap();
            let text = String::from_utf8_lossy(&response);
            assert!(text.starts_with("HTTP/1.1 401"), "{text}");
            assert!(text.contains("authentication_error"));
            assert_eq!(gateway.status().totals.requests, 0);
            task.abort();
        });
    }

    /// 킬 스위치 → 등록된 토큰이어도 403.
    #[test]
    fn kill_switch_blocks_registered_sessions() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let dir = tempfile::tempdir().unwrap();
            let mut raw = gateway_with_key(dir.path());
            // 닫힌 포트 — 업스트림에 닿으면 403이 아니라 502가 된다.
            raw.upstream_base = CLOSED_UPSTREAM.to_string();
            let gateway = Arc::new(raw);
            gateway.set_policy(None, Some(true), None);
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            gateway.set_listening(port);
            let gw = Arc::clone(&gateway);
            let task = tokio::spawn(async move {
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    let gw = Arc::clone(&gw);
                    tokio::spawn(async move {
                        handle_conn(gw, stream).await;
                    });
                }
            });
            let (workload, session) = ids();
            let route = gateway.issue_route(&workload, &session).unwrap();
            let request = format!(
                "POST /v1/messages HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\ncontent-length: 2\r\n\r\n{{}}",
                route.token.as_str()
            );
            let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            use tokio::io::AsyncWriteExt as _;
            sock.write_all(request.as_bytes()).await.unwrap();
            let mut response = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut sock, &mut response)
                .await
                .unwrap();
            let text = String::from_utf8_lossy(&response);
            assert!(text.starts_with("HTTP/1.1 403"), "{text}");
            assert!(text.contains("permission_error"));
            task.abort();
        });
    }

    /// 대체 키가 없으면 페일오버하지 않고 원래 429(+재시도 힌트)를 넘긴다.
    /// 그 뒤 쿨다운 동안은 업스트림에 가지 않고 429 + Retry-After를 만든다
    /// ("자격 증명 없음" 502로 뭉개지 않는다).
    #[test]
    fn lone_pool_key_passes_429_through_then_reports_cooldown() {
        let (addr, upstream) = scripted_upstream(vec![
            "HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\nretry-after: 30\r\ncontent-length: 2\r\n\r\n{}",
        ]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let (gateway, port, task, _dir) = pool_gateway(
                format!("http://{addr}"),
                &[("only", "pool-key-only-0001")],
                GatewayPoolStrategy::RoundRobin,
            )
            .await;
            let (workload, session) = ids();
            let route = gateway.issue_route(&workload, &session).unwrap();
            let first = roundtrip(port, &route.token, "/v1/messages").await;
            assert!(first.starts_with("HTTP/1.1 429"), "{first}");
            assert!(
                first.contains("retry-after: 30"),
                "재시도 힌트 전달: {first}"
            );
            let second = roundtrip(port, &route.token, "/v1/messages").await;
            assert!(second.starts_with("HTTP/1.1 429"), "{second}");
            assert!(second.contains("cooling down"), "{second}");
            assert!(second.contains("rate_limit_error"), "{second}");
            let retry: u64 = second
                .lines()
                .find_map(|line| line.strip_prefix("retry-after: "))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            assert!((1..=30).contains(&retry), "{retry}");
            task.abort();
        });
        assert_eq!(upstream.join().unwrap().len(), 1);
    }

    /// 403은 요청의 문제일 수 있다 — 격리도 페일오버도 하지 않고 그대로.
    #[test]
    fn forbidden_passes_through_without_quarantine() {
        let (addr, upstream) = scripted_upstream(vec![
            "HTTP/1.1 403 Forbidden\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}",
        ]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let (gateway, port, task, _dir) = pool_gateway(
                format!("http://{addr}"),
                &[("a", "pool-key-aaaa1111"), ("b", "pool-key-bbbb2222")],
                GatewayPoolStrategy::FillFirst,
            )
            .await;
            let (workload, session) = ids();
            let route = gateway.issue_route(&workload, &session).unwrap();
            let text = roundtrip(port, &route.token, "/v1/messages").await;
            assert!(text.starts_with("HTTP/1.1 403"), "{text}");
            assert_eq!(pool_state(&gateway, "a").state, PoolKeyViewState::Active);
            assert_eq!(pool_state(&gateway, "b").state, PoolKeyViewState::Unused);
            task.abort();
        });
        assert_eq!(upstream.join().unwrap().len(), 1, "페일오버 없음");
    }

    /// 5xx는 다시 보내지 않고(처리됐을 수 있다) 전송 실패로 센다 — 3회
    /// 연속이면 소프트 쿨다운으로 다음 요청은 다른 키로 간다.
    #[test]
    fn server_errors_escalate_to_soft_cooldown_without_failover() {
        const ERR: &str = "HTTP/1.1 503 Service Unavailable\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}";
        const OK: &str =
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}";
        let (addr, upstream) = scripted_upstream(vec![ERR, ERR, ERR, OK]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let (gateway, port, task, _dir) = pool_gateway(
                format!("http://{addr}"),
                &[("a", "pool-key-aaaa1111"), ("b", "pool-key-bbbb2222")],
                GatewayPoolStrategy::FillFirst,
            )
            .await;
            let (workload, session) = ids();
            let route = gateway.issue_route(&workload, &session).unwrap();
            for _ in 0..3 {
                let text = roundtrip(port, &route.token, "/v1/messages").await;
                assert!(text.starts_with("HTTP/1.1 503"), "{text}");
            }
            assert_eq!(pool_state(&gateway, "a").state, PoolKeyViewState::Cooling);
            let text = roundtrip(port, &route.token, "/v1/messages").await;
            assert!(text.starts_with("HTTP/1.1 200"), "{text}");
            task.abort();
        });
        let heads = upstream.join().unwrap();
        assert_eq!(heads.len(), 4);
        assert!(heads[..3]
            .iter()
            .all(|h| h.contains("Bearer pool-key-aaaa1111")));
        assert!(
            heads[3].contains("Bearer pool-key-bbbb2222"),
            "{}",
            heads[3]
        );
    }

    /// 업스트림 SSE가 중간에 끊기면 종결 청크 없이 닫는다(클라이언트가
    /// 잘린 응답을 완결로 믿지 않게) — 장부엔 실패로 남는다.
    #[test]
    fn upstream_cut_mid_stream_is_not_terminated_cleanly() {
        let (addr, upstream) = scripted_upstream(vec![concat!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n",
            "16\r\nevent: message_start\n\n\r\n",
            "40\r\ndata: {\"type\":"
        )]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let (gateway, port, task, _dir) = pool_gateway(
                format!("http://{addr}"),
                &[("a", "pool-key-aaaa1111")],
                GatewayPoolStrategy::RoundRobin,
            )
            .await;
            let (workload, session) = ids();
            let route = gateway.issue_route(&workload, &session).unwrap();
            let text = roundtrip(port, &route.token, "/v1/messages").await;
            assert!(text.starts_with("HTTP/1.1 200"), "{text}");
            assert!(text.contains("message_start"), "{text}");
            assert!(
                !text.ends_with("0\r\n\r\n"),
                "종결 청크가 없어야 한다: {text:?}"
            );
            assert_eq!(gateway.status().totals.failures, 1);
            task.abort();
        });
        upstream.join().unwrap();
    }

    /// `..` 세그먼트로 base 경로 밖을 가리키는 요청은 업스트림에 가지 않는다.
    #[test]
    fn dot_segment_paths_are_refused() {
        assert!(upstream_path_is_safe("/v1/messages?beta=true"));
        assert!(upstream_path_is_safe("/v1/messages/count_tokens"));
        assert!(!upstream_path_is_safe("/v1/../../paas/v4"));
        assert!(!upstream_path_is_safe("/v1/%2E%2e/x"));
        assert!(!upstream_path_is_safe("v1/messages"));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let (gateway, port, task, _dir) = pool_gateway(
                CLOSED_UPSTREAM.to_string(),
                &[("a", "pool-key-aaaa1111")],
                GatewayPoolStrategy::RoundRobin,
            )
            .await;
            let (workload, session) = ids();
            let route = gateway.issue_route(&workload, &session).unwrap();
            let text = roundtrip(port, &route.token, "/v1/../../x").await;
            assert!(text.starts_with("HTTP/1.1 400"), "{text}");
            task.abort();
        });
    }
}

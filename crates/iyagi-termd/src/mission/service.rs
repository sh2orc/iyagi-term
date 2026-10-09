//! Mission RPC service: request dedupe, revision CAS, domain validation,
//! transaction assembly, and snapshot/event reads (ticket O04).
//!
//! Error surface: [`MissionRpcError`] with structured details — the
//! dispatcher serializes it verbatim as the RPC error body so O1 codes and
//! `details.current_revision` survive every transport hop (01 §7).

use std::sync::Arc;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use term_contracts::ids::{ConnectionId, U64String};
use term_contracts::mission::error::{MissionErrorCode, MissionErrorDetails, MissionRpcError};
use term_contracts::mission::rpc::{
    methods, ArtifactBeginParams, ArtifactBeginResult, ArtifactCommitParams, ArtifactReadParams,
    ArtifactReadResult, ArtifactWriteParams, ArtifactWriteResult, BindingListResult,
    BindingProbeParams, BindingProbeResult, BindingSaveParams, BindingSaveResult,
    InstallationStatus, MissionControlAction, MissionControlParams, MissionCreateParams,
    MissionEventsParams, MissionEventsResult, MissionListParams, MissionListResult,
    MissionMessageParams, MissionRequestGetParams, MissionRequestGetResult, MissionRequestState,
    MissionSnapshotParams, ProbeModel, RuntimeDetectResult, TemplateListParams, TemplateListResult,
    TemplateSaveParams, TemplateSaveResult, VerificationListParams, VerificationListResult,
    VerificationSaveParams, VerificationSaveResult,
};
use term_contracts::mission::types::{
    BaseSnapshot, Binding, DecisionState, Entity, ExpectedOutput, Id, Mission, MissionEventType,
    MissionState, Phase, Role, RunState, RuntimeKind, Task, TaskContract, TaskKind, TaskState,
    VerificationCommand,
};
use term_contracts::mission::validation::{
    validate_artifact_ref, validate_git_oid, validate_policy, validate_requirements,
    validate_role_bindings, validate_title, MissionLimits, PolicyCeiling,
};
use term_storage::mission::types::{
    AppliedTransition, ApplyMissionTransition, ApplyMode, MissionListCursor, MissionStoreError,
};
use term_storage::Storage;

use super::artifacts::ArtifactStore;
use super::snapshot::SnapshotCache;

/// One dispatched mission RPC: the result value plus the post-commit hint.
pub struct Handled {
    pub result: Value,
    /// `(mission_id, latest_seq)` to broadcast as `mission.changed`.
    pub notify: Option<(Id, u64)>,
}

impl From<Value> for Handled {
    fn from(result: Value) -> Self {
        Handled {
            result,
            notify: None,
        }
    }
}

/// One mission mutation as assembled by a handler (fed into the single
/// storage transaction).
pub(super) struct ApplyPlan {
    pub(super) request_id: Id,
    pub(super) method: String,
    pub(super) params: Value,
    pub(super) mission_id: Id,
    pub(super) mode: ApplyMode,
    pub(super) event_type: MissionEventType,
    pub(super) upserts: Vec<Entity>,
    pub(super) deletes: Vec<term_contracts::mission::types::Change>,
    pub(super) outbox: Vec<term_storage::mission::types::OutboxIntent>,
    pub(super) adopt: Vec<Id>,
}

/// The daemon's no-inference self-check for one saved connection
/// (`binding.probe`, 11 §6). Injected so no suite spawns a CLI.
pub(super) type LocalProber = dyn Fn(
        &Binding,
        &str,
        &str,
        &crate::agent_runtime::detection::DetectionEnv,
        std::time::Duration,
    ) -> term_contracts::mission::types::LocalProbeReport
    + Send
    + Sync;

/// The cheap half of that self-check, run per `runtime.detect` row. `None`
/// means this runtime has nothing that can be checked without a connection.
pub(super) type CheapProber = dyn Fn(
        RuntimeKind,
        &str,
        &str,
        &crate::agent_runtime::detection::DetectionEnv,
        std::time::Duration,
    ) -> Option<term_contracts::mission::types::LocalProbeReport>
    + Send
    + Sync;

pub struct MissionService {
    pub(super) installation_probe: Box<super::binding_evidence::InstallationProbe>,
    pub(super) allow_unpinned_installation: bool,
    pub(super) capability_registry: Box<super::binding_evidence::CapabilityRegistry>,
    /// Local compatibility self-check for `binding.probe` (11 §6/§7).
    local_prober: Box<LocalProber>,
    /// Local compatibility self-check for `runtime.detect` rows (11 §6).
    cheap_prober: Box<CheapProber>,
    /// Whether `binding.probe` runs `local_prober` at all. Cleared by
    /// `with_binding_evidence`, whose fixtures state their own capabilities
    /// and must keep seeing "the self-check has not run for this version"
    /// (`local_evidence.probe == None`, 11 §7 DI).
    probe_locally: bool,
    /// PATH/home/config roots read by `runtime.detect` (injected for tests).
    detection_env: Box<dyn Fn() -> crate::agent_runtime::detection::DetectionEnv + Send + Sync>,
    /// Model-picker hints for `runtime.detect`/`binding.probe` (injected for
    /// tests so no suite spawns a CLI).
    model_lister: Box<super::binding_evidence::ModelLister>,
    pub(super) timing: std::sync::Mutex<super::timing::MissionClocks>,
    pub(super) monotonic: Box<dyn Fn() -> std::time::Instant + Send + Sync>,
    pub(super) wall_millis: Box<dyn Fn() -> u64 + Send + Sync>,
    pub(super) activity_guard: std::sync::Mutex<()>,
    pub(super) owner_daemon_id: Id,
    repository_guard: std::sync::Mutex<()>,
    pub(super) storage: Arc<Storage>,
    pub(super) dispatch_guard: std::sync::Mutex<()>,
    pub(super) dispatch_cursor: std::sync::atomic::AtomicUsize,
    pub(super) artifacts: ArtifactStore,
    snapshots: SnapshotCache,
    pub(super) limits: MissionLimits,
    pub(super) ceiling: PolicyCeiling,
    /// Bounded workspace size cache and cleanup replay (workspace.usage/cleanup).
    pub(super) workspace_housekeeping: super::workspace_cleanup::Housekeeping,
    /// Wall-clock now (injected for tests).
    now: Box<dyn Fn() -> String + Send + Sync>,
}

impl MissionService {
    pub fn new(storage: Arc<Storage>, artifacts: ArtifactStore) -> Self {
        MissionService {
            installation_probe: Box::new(crate::agent_runtime::installation::observe_expected),
            allow_unpinned_installation: false,
            capability_registry: Box::new(
                crate::agent_runtime::capability_evidence::capabilities_for_binding,
            ),
            local_prober: Box::new(crate::agent_runtime::local_probe::run),
            cheap_prober: Box::new(crate::agent_runtime::local_probe::run_cheap),
            probe_locally: true,
            detection_env: Box::new(crate::agent_runtime::detection::DetectionEnv::current),
            model_lister: Box::new(super::binding_evidence::list_models),
            timing: std::sync::Mutex::new(Default::default()),
            monotonic: Box::new(std::time::Instant::now),
            wall_millis: Box::new(crate::agent_runtime::rate_limits::unix_millis),
            activity_guard: std::sync::Mutex::new(()),
            owner_daemon_id: Id::generate(),
            repository_guard: std::sync::Mutex::new(()),
            storage,
            dispatch_guard: std::sync::Mutex::new(()),
            dispatch_cursor: std::sync::atomic::AtomicUsize::new(0),
            artifacts,
            snapshots: SnapshotCache::new(),
            limits: MissionLimits::load(),
            ceiling: PolicyCeiling::load(),
            workspace_housekeeping: Default::default(),
            now: Box::new(term_storage::time::now_iso8601),
        }
    }

    pub fn with_daemon_id(mut self, id: Id) -> Self {
        self.owner_daemon_id = id;
        self
    }

    /// Replace the process PATH/home/config roots `runtime.detect` searches.
    /// Embedder/test seam only; not reachable through RPC or environment.
    pub fn with_detection_env(
        mut self,
        env: impl Fn() -> crate::agent_runtime::detection::DetectionEnv + Send + Sync + 'static,
    ) -> Self {
        self.detection_env = Box::new(env);
        self
    }

    /// Replace the `binding.probe` self-check (11 §6) and enable it. Embedder
    /// and test seam only: the shipped daemon measures with
    /// `agent_runtime::local_probe::run`, and neither prober is reachable
    /// through RPC or environment.
    pub fn with_local_prober(
        mut self,
        prober: impl Fn(
                &Binding,
                &str,
                &str,
                &crate::agent_runtime::detection::DetectionEnv,
                std::time::Duration,
            ) -> term_contracts::mission::types::LocalProbeReport
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.local_prober = Box::new(prober);
        self.probe_locally = true;
        self
    }

    /// Replace the `runtime.detect` per-row self-check (11 §6). `None` is
    /// "not measured here", which leaves the row's grade on shipped evidence.
    pub fn with_cheap_prober(
        mut self,
        prober: impl Fn(
                RuntimeKind,
                &str,
                &str,
                &crate::agent_runtime::detection::DetectionEnv,
                std::time::Duration,
            ) -> Option<term_contracts::mission::types::LocalProbeReport>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.cheap_prober = Box::new(prober);
        self
    }

    /// Leave `local_evidence.probe` at "has not run for this version" so a
    /// suite whose capabilities come from an injected registry is not also
    /// judged by a measurement it never asked for (11 §7 DI).
    pub fn without_local_probe(mut self) -> Self {
        fn not_measured(
            _runtime: RuntimeKind,
            _program: &str,
            _model_id: &str,
            _env: &crate::agent_runtime::detection::DetectionEnv,
            _budget: std::time::Duration,
        ) -> Option<term_contracts::mission::types::LocalProbeReport> {
            None
        }
        self.probe_locally = false;
        self.cheap_prober = Box::new(not_measured);
        self
    }

    /// Replace the model-picker listing. Embedder/test seam only: these are
    /// hints for the model field, never evidence, so a suite can stub them
    /// without spawning a CLI.
    pub fn with_model_lister(
        mut self,
        lister: impl Fn(RuntimeKind, &str, Option<&str>) -> Vec<ProbeModel> + Send + Sync + 'static,
    ) -> Self {
        self.model_lister =
            Box::new(move |request| lister(request.runtime, request.program, request.provider_id));
        self
    }

    /// Provider reset timestamps are absolute UTC instants, including after restart.
    pub fn with_wall_clock_millis(
        mut self,
        clock: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Self {
        self.wall_millis = Box::new(clock);
        self
    }

    /// Inject a monotonic source before the service starts (deterministic tests).
    pub fn with_monotonic_clock(
        mut self,
        clock: impl Fn() -> std::time::Instant + Send + Sync + 'static,
    ) -> Self {
        self.monotonic = Box::new(clock);
        self
    }

    /// Drop cached snapshots when a control connection closes (01 §4: cache
    /// is connection-scoped).
    pub fn drop_connection(&self, conn: &ConnectionId) {
        self.snapshots.drop_connection(conn);
    }

    /// Entry point from dispatch. `conn` scopes staging uploads and the
    /// snapshot cache.
    pub fn handle(
        &self,
        conn: &ConnectionId,
        method: &str,
        params: &Value,
    ) -> Result<Handled, MissionRpcError> {
        // Replay before state-dependent validation. A committed create has
        // already adopted its goal, and a committed start is no longer draft.
        if matches!(
            method,
            methods::MISSION_CREATE
                | methods::MISSION_CONTROL
                | methods::MISSION_MESSAGE
                | methods::MISSION_TASK_CONTROL
                | methods::MISSION_DECISION_ANSWER
                | methods::MISSION_ACCEPT
                | methods::MISSION_PLAN_APPLY
                | methods::MISSION_POLICY_UPDATE
                | methods::MISSION_FINDING_RESOLVE
                | methods::MISSION_RUN_ATTEST_EXITED
        ) {
            // Match the canonical, typed payload used by the original
            // commit (including nullable fields omitted by a client).
            let canonical = match method {
                methods::MISSION_CREATE => {
                    serde_json::to_value(Self::parse::<MissionCreateParams>(params)?)
                }
                methods::MISSION_CONTROL => {
                    serde_json::to_value(Self::parse::<MissionControlParams>(params)?)
                }
                methods::MISSION_TASK_CONTROL => serde_json::to_value(Self::parse::<
                    term_contracts::mission::rpc::MissionTaskControlParams,
                >(params)?),
                methods::MISSION_DECISION_ANSWER => serde_json::to_value(Self::parse::<
                    term_contracts::mission::rpc::MissionDecisionAnswerParams,
                >(params)?),
                methods::MISSION_ACCEPT => serde_json::to_value(Self::parse::<
                    term_contracts::mission::rpc::MissionAcceptParams,
                >(params)?),
                methods::MISSION_PLAN_APPLY => serde_json::to_value(Self::parse::<
                    term_contracts::mission::rpc::MissionPlanApplyParams,
                >(params)?),
                methods::MISSION_POLICY_UPDATE => serde_json::to_value(Self::parse::<
                    term_contracts::mission::rpc::MissionPolicyUpdateParams,
                >(params)?),
                methods::MISSION_FINDING_RESOLVE => serde_json::to_value(Self::parse::<
                    term_contracts::mission::rpc::MissionFindingResolveParams,
                >(params)?),
                methods::MISSION_RUN_ATTEST_EXITED => {
                    serde_json::to_value(Self::parse::<
                        term_contracts::mission::rpc::MissionRunAttestExitedParams,
                    >(params)?)
                }
                _ => serde_json::to_value(Self::parse::<MissionMessageParams>(params)?),
            }
            .map_err(|error| MissionRpcError::new(MissionErrorCode::Internal, error.to_string()))?;
            if let Some(request_id) = params.get("request_id").and_then(Value::as_str) {
                let request_id = Id::parse(request_id).map_err(|e| {
                    MissionRpcError::new(MissionErrorCode::InvalidArgument, e.to_string())
                })?;
                if let Some(stored) = self
                    .storage
                    .mission_request(&request_id)
                    .map_err(Self::store_error)?
                {
                    if stored.method != method
                        || stored.fingerprint != Self::fingerprint(method, &canonical)
                    {
                        return Err(MissionRpcError::new(
                            MissionErrorCode::RequestConflict,
                            "request id already recorded with a different payload",
                        ));
                    }
                    return Ok(Handled {
                        result: serde_json::to_value(&stored.response).unwrap_or(Value::Null),
                        notify: None,
                    });
                }
            }
        }
        match method {
            methods::MISSION_CREATE => self.mission_create(conn, params),
            methods::MISSION_LIST => self.mission_list(params),
            methods::MISSION_SNAPSHOT => self.mission_snapshot(conn, params),
            methods::MISSION_EVENTS => self.mission_events(params),
            methods::MISSION_CONTROL => self.mission_control(params),
            methods::MISSION_MESSAGE => self.mission_message(params),
            methods::MISSION_REQUEST_GET => self.mission_request_get(params),
            methods::BINDING_LIST => self.binding_list(),
            methods::BINDING_SAVE => self.binding_save(params),
            methods::BINDING_PROBE => self.binding_probe(params),
            methods::RUNTIME_DETECT => self.runtime_detect(),
            methods::TEMPLATE_LIST => self.template_list(params),
            methods::TEMPLATE_SAVE => self.template_save(params),
            methods::VERIFICATION_LIST => self.verification_list(params),
            methods::VERIFICATION_SAVE => self.verification_save(params),
            methods::ARTIFACT_BEGIN => self.artifact_begin(conn, params),
            methods::ARTIFACT_WRITE => self.artifact_write(params),
            methods::ARTIFACT_COMMIT => self.artifact_commit(params),
            methods::ARTIFACT_READ => self.artifact_read(params),
            methods::MISSION_DECISION_ANSWER => self.decision_answer(params),
            methods::MISSION_TASK_CONTROL => self.task_control(params),
            methods::MISSION_ACCEPT => super::workflow::apply_accept(self, params),
            methods::MISSION_PLAN_APPLY => self.plan_apply(params),
            methods::REPOSITORY_INSPECT => self.repository_inspect(params),
            methods::MISSION_ACTIVITY => self.mission_activity(params),
            methods::MISSION_POLICY_UPDATE => self.policy_update(params),
            methods::MISSION_FINDING_RESOLVE => self.finding_resolve(params),
            methods::MISSION_RUN_ATTEST_EXITED => self.run_attest_exited(params),
            methods::WORKSPACE_USAGE => self.workspace_usage(params),
            methods::WORKSPACE_CLEANUP => self.workspace_cleanup(params),
            other => Err(MissionRpcError::new(
                MissionErrorCode::InvalidArgument,
                format!("unknown mission method {other:?}"),
            )),
        }
    }

    // ---- fingerprinting --------------------------------------------------

    /// SHA-256 over `method` + the canonical payload (request_id excluded).
    /// serde_json's Map is a BTreeMap by default, so `to_string` already
    /// emits object keys in sorted order — the canonical form (01 §2).
    pub(super) fn fingerprint(method: &str, params: &Value) -> String {
        let mut normalized = params.clone();
        if let Some(object) = normalized.as_object_mut() {
            object.remove("request_id");
        }
        let canonical = serde_json::to_string(&normalized).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(method.as_bytes());
        hasher.update(b"\n");
        hasher.update(canonical.as_bytes());
        let digest = hasher.finalize();
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub(super) fn now(&self) -> String {
        (self.now)()
    }

    pub(super) fn parse<T: serde::de::DeserializeOwned>(
        params: &Value,
    ) -> Result<T, MissionRpcError> {
        serde_json::from_value(params.clone()).map_err(|e| {
            MissionRpcError::new(
                MissionErrorCode::InvalidArgument,
                format!("params mismatch: {e}"),
            )
        })
    }

    pub(super) fn store_error(error: MissionStoreError) -> MissionRpcError {
        match error {
            MissionStoreError::RequestConflict(_) => {
                MissionRpcError::new(MissionErrorCode::RequestConflict, "request id already recorded with a different payload")
            }
            MissionStoreError::RevisionConflict {
                expected_revision,
                current_revision,
            } => MissionRpcError::with_details(
                MissionErrorCode::RevisionConflict,
                format!(
                    "mission revision is {current_revision}, not {expected_revision}; resync and re-apply"
                ),
                MissionErrorDetails {
                    current_revision: Some(U64String::new(current_revision).expect("fits SQLite bound")),
                    ..Default::default()
                },
            ),
            MissionStoreError::NotFound { what, id } => MissionRpcError::new(
                MissionErrorCode::NotFound,
                format!("{what} {id} not found"),
            ),
            MissionStoreError::InvalidState(message) => {
                MissionRpcError::new(MissionErrorCode::InvalidState, message)
            }
            MissionStoreError::InvalidArgument(message) => {
                MissionRpcError::new(MissionErrorCode::InvalidArgument, message)
            }
            MissionStoreError::Corrupt(message) => MissionRpcError::new(
                MissionErrorCode::Internal,
                format!("storage corruption: {message}"),
            ),
            MissionStoreError::WriterClosed => MissionRpcError::new(
                MissionErrorCode::StorageUnavailable,
                "storage writer closed",
            ),
            MissionStoreError::Sqlite(error) => MissionRpcError::new(
                MissionErrorCode::StorageUnavailable,
                format!("storage error: {error}"),
            ),
        }
    }

    pub(super) fn apply(&self, plan: ApplyPlan) -> Result<AppliedTransition, MissionRpcError> {
        let transition = ApplyMissionTransition {
            request_id: plan.request_id,
            method: plan.method.to_string(),
            fingerprint: Self::fingerprint(&plan.method, &plan.params),
            mission_id: plan.mission_id,
            mode: plan.mode,
            transaction_id: Id::generate(),
            event_type: plan.event_type,
            upserts: plan.upserts,
            deletes: plan.deletes,
            changes_ref: None,
            outbox: plan.outbox,
            outbox_updates: Vec::new(),
            adopt_staged_artifacts: plan.adopt,
            created_at: self.now(),
        };
        self.apply_timed_transition(transition)
            .map_err(Self::store_error)
    }

    pub(super) fn mutation_handled(applied: &AppliedTransition) -> Handled {
        let latest = applied.result.revision.get();
        Handled {
            result: serde_json::to_value(&applied.result).unwrap_or(Value::Null),
            notify: Some((applied.result.mission_id.clone(), latest)),
        }
    }

    pub(super) fn read_mission(&self, mission_id: &Id) -> Result<Mission, MissionRpcError> {
        let snapshot = self
            .storage
            .mission_snapshot(mission_id)
            .map_err(Self::store_error)?
            .ok_or_else(|| {
                MissionRpcError::new(
                    MissionErrorCode::NotFound,
                    format!("mission {mission_id} not found"),
                )
            })?;
        snapshot
            .entities
            .into_iter()
            .find_map(|entity| match entity {
                Entity::Mission(mission) => Some(*mission),
                _ => None,
            })
            .ok_or_else(|| {
                MissionRpcError::new(
                    MissionErrorCode::Internal,
                    "mission snapshot lacks its mission row",
                )
            })
    }

    // ---- mission lifecycle ------------------------------------------------

    fn mission_create(
        &self,
        conn: &ConnectionId,
        params: &Value,
    ) -> Result<Handled, MissionRpcError> {
        let params: MissionCreateParams = Self::parse(params)?;
        validate_title(&params.title, &self.limits)?;
        validate_git_oid(&params.expected_base_oid)?;
        validate_requirements(&params.requirements, &self.limits)?;
        validate_policy(&params.policy, &self.ceiling)?;
        validate_role_bindings(&params.role_bindings, &params.policy)?;
        validate_artifact_ref(&params.goal_ref)?;

        // Preserve draft creation for accessible directories, but normalize
        // valid Git input to its worktree root (including subdirectory input).
        let canonical = std::fs::canonicalize(&params.repository_path).map_err(|e| {
            Self::with_reason(
                MissionRpcError::new(
                    MissionErrorCode::InvalidArgument,
                    format!(
                        "repository path {:?} is not accessible: {e}",
                        params.repository_path
                    ),
                ),
                "path_not_accessible",
            )
        })?;
        let canonical = crate::workspace::repository_identity(&canonical)
            .map(|identity| identity.canonical_path)
            .unwrap_or(canonical);
        let canonical = canonical
            .to_str()
            .ok_or_else(|| {
                MissionRpcError::new(
                    MissionErrorCode::InvalidArgument,
                    "repository path is not valid UTF-8",
                )
            })?
            .to_string();
        let repository_id = self.register_repository(&canonical)?;
        if let Some(previous) = &params.follow_up_of {
            self.follow_up_base(
                previous,
                &repository_id,
                &canonical,
                &params.expected_base_oid,
            )?;
        }

        // The goal must be staged by this connection (creation adoption).
        self.artifacts
            .adopt_check(conn, &params.goal_ref.id)
            .map_err(|e| MissionRpcError::new(MissionErrorCode::InvalidArgument, e))?;
        self.artifacts
            .validate_reference(&params.goal_ref)
            .map_err(|(code, message)| MissionRpcError::new(code, message))?;
        if params.goal_ref.bytes.get() > self.limits.max_context_bytes as u64 {
            return Err(MissionRpcError::new(
                MissionErrorCode::ContextTooLarge,
                "goal exceeds the context byte limit",
            ));
        }

        let params_json = serde_json::to_value(&params).unwrap_or(Value::Null);
        let mission_id = Id::generate();
        let (base_oid, base_snapshot) = self.created_base(&params, &canonical, &mission_id)?;
        let repository_path = canonical.clone();
        let mission = Mission {
            id: mission_id.clone(),
            revision: U64String::new(1).expect("fits SQLite bound"),
            semantic_revision: None,
            state: MissionState::Draft,
            phase: Phase::Planning,
            title: params.title,
            repository_path: canonical,
            repository_id,
            base_oid,
            goal_ref: params.goal_ref.clone(),
            requirements: params.requirements,
            policy: params.policy,
            role_bindings: params.role_bindings,
            plan_revision: 0,
            candidate_id: None,
            open_decision_count: 0,
            active_time_ms: U64String::new(0).expect("fits SQLite bound"),
            automatic_start_count: 0,
            created_at: self.now(),
            updated_at: self.now(),
            archived_at: None,
            accepted_at: None,
            failure_code: None,
            follow_up_of: params.follow_up_of.clone(),
            base_snapshot: base_snapshot.clone(),
        };
        let created_id = mission_id.clone();
        let applied = self.apply(ApplyPlan {
            request_id: params.request_id,
            method: methods::MISSION_CREATE.to_string(),
            params: params_json,
            mission_id,
            mode: ApplyMode::Create,
            event_type: MissionEventType::Created,
            upserts: vec![Entity::Mission(Box::new(mission))],
            deletes: Vec::new(),
            outbox: Vec::new(),
            adopt: vec![params.goal_ref.id],
        })?;
        // A replayed request answers with the mission it created the first
        // time, which leaves this attempt's snapshot referenced by nothing.
        if base_snapshot.is_some() && applied.result.mission_id != created_id {
            let _ = crate::workspace::git::delete_private_refs(
                std::path::Path::new(&repository_path),
                &format!("refs/iyagi/missions/{created_id}/inputs"),
            );
        }
        Ok(Self::mutation_handled(&applied))
    }

    /// Base for a new mission: the accepted commit of a follow-up, the user's
    /// HEAD, or — when they asked to include uncommitted work — a private
    /// snapshot commit of the working tree on top of HEAD (04 §1). A clean
    /// tree records no snapshot and the mission stays an ordinary HEAD one.
    fn created_base(
        &self,
        params: &MissionCreateParams,
        repository_path: &str,
        mission_id: &Id,
    ) -> Result<(String, Option<BaseSnapshot>), MissionRpcError> {
        if params.include_uncommitted != Some(true) {
            return Ok((params.expected_base_oid.clone(), None));
        }
        if params.follow_up_of.is_some() {
            return Err(Self::with_reason(
                MissionRpcError::new(
                    MissionErrorCode::InvalidArgument,
                    "a follow-up starts from the previous accepted result, which uncommitted changes cannot extend",
                ),
                "follow_up_snapshot",
            ));
        }
        let repository = std::path::Path::new(repository_path);
        let identity =
            crate::workspace::repository_identity(repository).map_err(Self::git_user_error)?;
        if identity.head_oid != params.expected_base_oid {
            return Err(Self::with_reason(
                MissionRpcError::new(
                    MissionErrorCode::InvalidState,
                    "repository HEAD changed while the mission was being created; inspect the repository again",
                ),
                "base_changed",
            ));
        }
        let snapshot = self.snapshot_base(repository, mission_id, &identity.head_oid)?;
        Ok((
            snapshot.commit_oid.clone(),
            Self::recorded_snapshot(&snapshot),
        ))
    }

    /// Record the working tree as this mission's base. The private staging
    /// index sits beside the mission's other Git inputs so the same
    /// housekeeping clears it.
    fn snapshot_base(
        &self,
        repository: &std::path::Path,
        mission_id: &Id,
        head_oid: &str,
    ) -> Result<crate::workspace::WorkingTreeSnapshot, MissionRpcError> {
        let scratch = self
            .mission_dir(mission_id)
            .ok_or_else(|| {
                MissionRpcError::new(
                    MissionErrorCode::Internal,
                    "mission directory is unavailable",
                )
            })?
            .join("input-indexes");
        crate::workspace::snapshot_working_tree(repository, &scratch, mission_id.as_str(), head_oid)
            .map_err(Self::git_user_error)
    }

    /// A snapshot that folded nothing in is an ordinary HEAD base.
    fn recorded_snapshot(snapshot: &crate::workspace::WorkingTreeSnapshot) -> Option<BaseSnapshot> {
        snapshot.is_snapshot().then(|| BaseSnapshot {
            head_oid: snapshot.head_oid.clone(),
            entry_count: snapshot.entry_count,
        })
    }

    /// Reuse Git common-directory identity across linked worktrees. Old draft
    /// records without Git metadata are upgraded only after resolving their path.
    fn register_repository(&self, canonical: &str) -> Result<Id, MissionRpcError> {
        let _guard = self
            .repository_guard
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let identity = crate::workspace::repository_identity(std::path::Path::new(canonical)).ok();
        let common = identity
            .as_ref()
            .map(|i| {
                i.common_dir.to_str().ok_or_else(|| {
                    MissionRpcError::new(
                        MissionErrorCode::InvalidArgument,
                        "Git common directory is not valid UTF-8",
                    )
                })
            })
            .transpose()?;
        let invalid = |message: &str| MissionRpcError::new(MissionErrorCode::InvalidState, message);
        let parse_id = |value: &Value| {
            Id::parse(value["id"].as_str().unwrap_or(""))
                .map_err(|_| invalid("stored repository ID is invalid"))
        };
        let repos = self
            .storage
            .mission_configs("repository")
            .map_err(Self::store_error)?;
        let mut matches = Vec::new();
        for value in repos {
            let old_path = value["canonical_path"].as_str().unwrap_or("");
            let old_common = value["common_dir"].as_str();
            if old_path == canonical && old_common.is_some() && common != old_common {
                return Err(invalid(
                    "repository Git identity changed at its registered path",
                ));
            }
            let same = if let Some(common) = common {
                old_common == Some(common)
                    || (old_common.is_none()
                        && (old_path == canonical
                            || crate::workspace::repository_identity(std::path::Path::new(
                                old_path,
                            ))
                            .is_ok_and(|old| old.common_dir.to_str() == Some(common))))
            } else {
                old_path == canonical
            };
            if same {
                matches.push(value);
            }
        }
        if matches.len() > 1 {
            return Err(invalid("multiple legacy repository IDs resolve to the same Git common directory; reconcile their saved references first"));
        }
        let (mut document, expected_revision) = if let Some(value) = matches.pop() {
            let id = parse_id(&value)?;
            if common.is_none() {
                return Ok(id);
            }
            let format = identity
                .as_ref()
                .expect("common identity")
                .object_format
                .as_str();
            if value["common_dir"].as_str() == common {
                if value["object_format"] != format {
                    return Err(invalid("repository object format changed"));
                }
                return Ok(id);
            }
            let revision = value["revision"]
                .as_str()
                .and_then(|r| r.parse::<u64>().ok())
                .ok_or_else(|| invalid("stored repository revision is invalid"))?;
            (value, revision)
        } else {
            (
                json!({"id":Id::generate(), "canonical_path":canonical, "created_at":self.now(), "object_format":Value::Null}),
                0,
            )
        };
        if let Some(identity) = identity.as_ref() {
            document["common_dir"] = json!(common);
            document["object_format"] = json!(identity.object_format);
        }
        let fingerprint = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&document).expect("repository JSON"))
        );
        match self.storage.save_mission_config(
            Id::generate(),
            "repository.register",
            &fingerprint,
            "repository",
            expected_revision,
            document,
            self.now(),
        ) {
            Ok(saved) => parse_id(&saved.document),
            Err(error) => {
                // Another service/connection can win after our read. Storage
                // serializes uniqueness with the insert; reuse its committed ID.
                if let Some(common) = common {
                    let current = self
                        .storage
                        .mission_configs("repository")
                        .map_err(Self::store_error)?;
                    let winners: Vec<_> = current
                        .iter()
                        .filter(|v| v["common_dir"] == common)
                        .collect();
                    if winners.len() == 1
                        && winners[0]["object_format"] == identity.as_ref().unwrap().object_format
                    {
                        return parse_id(winners[0]);
                    }
                }
                Err(Self::store_error(error))
            }
        }
    }

    /// A follow-up starts from a completed mission's accepted candidate in the
    /// same repository. The commit must still be reachable through the
    /// daemon's private candidate ref (no user ref or checkout is consulted).
    fn follow_up_base(
        &self,
        previous_id: &Id,
        repository_id: &Id,
        repository_path: &str,
        expected_base_oid: &str,
    ) -> Result<(), MissionRpcError> {
        let not_accepted = |message: &str| {
            Self::with_reason(
                MissionRpcError::new(MissionErrorCode::InvalidArgument, message),
                "follow_up_not_accepted",
            )
        };
        let mismatch = |message: &str| {
            Self::with_reason(
                MissionRpcError::new(MissionErrorCode::InvalidArgument, message),
                "follow_up_base_mismatch",
            )
        };
        let previous = match super::workflow::load_entities(&self.storage, previous_id) {
            Ok(previous) => previous,
            Err(error) if error.code == MissionErrorCode::NotFound => {
                return Err(not_accepted("the previous mission does not exist"))
            }
            Err(error) => return Err(error),
        };
        let mission = &previous.mission;
        if mission.state != MissionState::Completed
            || mission.accepted_at.is_none()
            || &mission.repository_id != repository_id
        {
            return Err(not_accepted(
                "a follow-up requires a completed, accepted mission of the same repository",
            ));
        }
        let candidate = mission
            .candidate_id
            .as_ref()
            .and_then(|id| previous.candidates.iter().find(|c| &c.id == id))
            .ok_or_else(|| not_accepted("the previous mission has no accepted result"))?;
        if candidate.commit_oid != expected_base_oid {
            return Err(mismatch(
                "the base must be the previous mission's accepted result commit",
            ));
        }
        let repository = std::path::Path::new(repository_path);
        let reference =
            crate::workspace::git::candidate_ref(mission.id.as_str(), candidate.id.as_str());
        let reachable = crate::workspace::git::rev_parse(repository, &reference)
            .is_ok_and(|oid| oid == candidate.commit_oid)
            && crate::workspace::git::commit_tree_oid(repository, &candidate.commit_oid)
                .is_ok_and(|tree| tree == candidate.tree_oid);
        if !reachable {
            return Err(mismatch(
                "the accepted result commit is no longer reachable in this repository",
            ));
        }
        Ok(())
    }

    fn repository_inspect(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        use term_contracts::mission::rpc::{RepositoryInspectParams, RepositoryInspectResult};
        let params: RepositoryInspectParams = Self::parse(params)?;
        let identity = crate::workspace::repository_identity(std::path::Path::new(&params.path))
            .map_err(Self::git_user_error)?;
        let canonical = identity.canonical_path.to_str().ok_or_else(|| {
            MissionRpcError::new(
                MissionErrorCode::InvalidArgument,
                "repository path is not valid UTF-8",
            )
        })?;
        let dirty = crate::workspace::git::status_entries(&identity.canonical_path)
            .map_err(Self::git_user_error)?;
        let result = RepositoryInspectResult {
            repository_id: self.register_repository(canonical)?,
            canonical_path: canonical.into(),
            head_oid: identity.head_oid,
            clean: dirty.is_empty(),
            dirty_paths: dirty.into_iter().take(20).map(|(_, path)| path).collect(),
            verification_supported: super::verification_isolation::Isolation::require_supported()
                .is_ok(),
        };
        Ok(serde_json::to_value(result)
            .expect("repository result")
            .into())
    }

    /// Repository checks on user input (inspect/start) keep INVALID_ARGUMENT
    /// for unexpected Git failures, DIRTY_WORKTREE for dirty trees, and add a
    /// stable reason code; a missing Git executable is INVALID_STATE.
    fn git_user_error(error: crate::workspace::GitError) -> MissionRpcError {
        let mut mapped = super::workflow::git_error(error);
        if mapped.code == MissionErrorCode::Internal {
            mapped.code = MissionErrorCode::InvalidArgument;
            mapped.retryable = mapped.code.retryable();
        }
        mapped
    }

    /// Stable `details.reason_code` on an otherwise unchanged error.
    pub(super) fn with_reason(mut error: MissionRpcError, reason: &str) -> MissionRpcError {
        error.details.reason_code = Some(reason.to_string());
        error
    }

    fn mission_list(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        let params: MissionListParams = Self::parse(params)?;
        let cursor = params
            .cursor
            .as_deref()
            .map(SnapshotCursor::decode_repository)
            .transpose()?;
        let cursor = match cursor {
            Some(parts) => Some(MissionListCursor {
                updated_at: parts.0,
                id: Id::parse(&parts.1).map_err(|e| {
                    MissionRpcError::new(
                        MissionErrorCode::CursorExpired,
                        format!("list cursor: {e}"),
                    )
                })?,
            }),
            None => None,
        };
        let (missions, next) = self
            .storage
            .mission_list(cursor, params.limit, params.archived)
            .map_err(Self::store_error)?;
        let next_cursor =
            next.map(|cursor| SnapshotCursor(cursor.updated_at, cursor.id.to_string()).encode());
        let result = MissionListResult {
            items: missions,
            next_cursor,
        };
        Ok(serde_json::to_value(&result).unwrap_or(Value::Null).into())
    }

    fn mission_snapshot(
        &self,
        conn: &ConnectionId,
        params: &Value,
    ) -> Result<Handled, MissionRpcError> {
        let params: MissionSnapshotParams = Self::parse(params)?;
        let page = self
            .snapshots
            .page(&self.storage, conn, &params)
            .map_err(Self::store_error)?;
        Ok(serde_json::to_value(&page).unwrap_or(Value::Null).into())
    }

    fn mission_events(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        let params: MissionEventsParams = Self::parse(params)?;
        let (events, watermark) = self
            .storage
            .mission_events(&params.mission_id, params.after_seq.get(), params.limit)
            .map_err(Self::store_error)?;
        let next_after_seq = events
            .last()
            .map(|stored| stored.event.seq.get())
            .unwrap_or(params.after_seq.get());
        let result = MissionEventsResult {
            events: events.into_iter().map(|stored| stored.event).collect(),
            high_watermark: U64String::new(watermark).expect("fits SQLite bound"),
            next_after_seq: U64String::new(next_after_seq).expect("fits SQLite bound"),
        };
        Ok(serde_json::to_value(&result).unwrap_or(Value::Null).into())
    }

    fn mission_control(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        let params: MissionControlParams = Self::parse(params)?;
        let snapshot = super::workflow::load_entities(&self.storage, &params.mission_id)?;
        let mut mission = snapshot.mission;
        if !super::timing::revision_accepts(&mission, params.expected_revision.get()) {
            return Err(Self::store_error(MissionStoreError::RevisionConflict {
                expected_revision: params.expected_revision.get(),
                current_revision: mission.revision.get(),
            }));
        }
        if matches!(
            params.action,
            MissionControlAction::Archive | MissionControlAction::Unarchive
        ) {
            return self.archive(&params, mission, params.action);
        }
        use term_core::mission::reducer::{
            apply_control, ControlError, MissionAction, MissionRules,
        };
        let action = match params.action {
            MissionControlAction::Start => MissionAction::Start,
            MissionControlAction::Pause => MissionAction::Pause,
            MissionControlAction::Resume => MissionAction::Resume,
            MissionControlAction::Cancel => MissionAction::Cancel,
            _ => unreachable!(),
        };
        let live_count = snapshot
            .runs
            .iter()
            .filter(|run| {
                run.holds_execution_slot()
                    && !(action == MissionAction::Pause
                        && run.state == RunState::Prepared
                        && run.dispatch_state
                            == term_contracts::mission::types::RunDispatchState::Unsent)
            })
            .count();
        let rules = MissionRules {
            max_attempts_per_task: mission.policy.max_attempts_per_task,
            max_repair_cycles: mission.policy.max_repair_cycles,
            max_automatic_starts: mission.policy.max_automatic_starts as u64,
        };
        let (mut state, _) =
            apply_control(&mission, live_count, action, &rules).map_err(|error| match error {
                ControlError::NoBindings => Self::with_reason(
                    MissionRpcError::new(MissionErrorCode::InvalidArgument, error.to_string()),
                    "bindings_missing",
                ),
                ControlError::Illegal { .. } => Self::with_reason(
                    MissionRpcError::new(MissionErrorCode::InvalidState, error.to_string()),
                    "illegal_transition",
                ),
                _ => MissionRpcError::new(MissionErrorCode::InvalidState, error.to_string()),
            })?;
        let now = self.now();
        let mut upserts = Vec::new();
        let mut outbox = Vec::new();
        if action == MissionAction::Start {
            // Owned: the base may be re-recorded below, which mutates the
            // mission row this path would otherwise still borrow.
            let repository = std::path::PathBuf::from(&mission.repository_path);
            let identity =
                crate::workspace::repository_identity(&repository).map_err(Self::git_user_error)?;
            if self.register_repository(&mission.repository_path)? != mission.repository_id {
                return Err(Self::with_reason(
                    MissionRpcError::new(
                        MissionErrorCode::InvalidState,
                        "repository registration changed since this mission was created",
                    ),
                    "repository_changed",
                ));
            }
            let base_changed = |message: &str| {
                MissionRpcError::with_details(
                    MissionErrorCode::InvalidState,
                    message,
                    MissionErrorDetails {
                        reason_code: Some("base_changed".into()),
                        ..Default::default()
                    },
                )
            };
            if let Some(previous) = mission.follow_up_of.clone() {
                // The base is the accepted result commit, not the user's HEAD.
                // Workers, integration and verification build daemon-owned
                // worktrees from that commit and never read or write the
                // user's checkout, so its uncommitted state cannot change the
                // base; only reachability of the accepted commit matters.
                self.follow_up_base(
                    &previous,
                    &mission.repository_id,
                    &mission.repository_path,
                    &mission.base_oid,
                )
                .map_err(|mut error| {
                    error.code = MissionErrorCode::InvalidState;
                    error.retryable = error.code.retryable();
                    error
                })?;
            } else if let Some(recorded) = mission.base_snapshot.clone() {
                // The base is this user's working tree, recorded at creation.
                // The commit it sits on must still be HEAD; the tree itself is
                // re-recorded so the agents start from what the user sees now.
                // An unchanged tree yields the same commit OID, so the usual
                // case mints nothing new.
                if identity.head_oid != recorded.head_oid {
                    return Err(base_changed(
                        "repository HEAD changed since this mission was created; create a mission from the current base",
                    ));
                }
                let snapshot = self.snapshot_base(&repository, &mission.id, &identity.head_oid)?;
                mission.base_oid = snapshot.commit_oid.clone();
                mission.base_snapshot = Self::recorded_snapshot(&snapshot);
                if mission.base_snapshot.is_none() {
                    // The user committed or reverted everything in between.
                    // The base is plain HEAD again; no run has claimed an
                    // input ref yet, so the recorded snapshot can go.
                    let _ = crate::workspace::git::delete_private_refs(
                        &repository,
                        &format!("refs/iyagi/missions/{}/inputs", mission.id),
                    );
                }
            } else {
                crate::workspace::ensure_clean(&repository).map_err(Self::git_user_error)?;
                if identity.head_oid != mission.base_oid {
                    return Err(base_changed(
                        "repository HEAD changed since this mission was created; create a mission from the current base",
                    ));
                }
            }
            let lead = mission
                .role_bindings
                .iter()
                .find(|role| role.role == Role::Lead)
                .ok_or_else(|| {
                    Self::with_reason(
                        MissionRpcError::new(
                            MissionErrorCode::InvalidArgument,
                            "start requires a Lead role binding",
                        ),
                        "lead_missing",
                    )
                })?;
            if !mission
                .policy
                .allowed_binding_ids
                .contains(&lead.primary_binding_id)
            {
                return Err(Self::with_reason(
                    MissionRpcError::new(
                        MissionErrorCode::InvalidArgument,
                        "Lead binding is outside the mission allowlist",
                    ),
                    "lead_not_allowed",
                ));
            }
            // A mission always needs someone to change files. The Reviewer is
            // required only with independent review; the Integrator is optional
            // (deterministic integration uses no model, conflicts then offer
            // no integrator resolution).
            if !mission
                .role_bindings
                .iter()
                .any(|role| role.role == Role::Builder)
            {
                return Err(Self::with_reason(
                    MissionRpcError::new(
                        MissionErrorCode::InvalidArgument,
                        "start requires a Builder role binding",
                    ),
                    "builder_missing",
                ));
            }
            if mission.policy.require_independent_review
                && !mission
                    .role_bindings
                    .iter()
                    .any(|role| role.role == Role::Reviewer)
            {
                return Err(Self::with_reason(
                    MissionRpcError::new(
                        MissionErrorCode::InvalidArgument,
                        "independent review requires a Reviewer role binding; assign one or turn off review",
                    ),
                    "reviewer_missing",
                ));
            }
            let bindings: Vec<Binding> = self
                .storage
                .mission_bindings()
                .map_err(Self::store_error)?
                .iter()
                .map(|value| self.observed_binding(value))
                .collect::<Result<_, _>>()?;
            for role in &mission.role_bindings {
                if let Some(binding) = bindings
                    .iter()
                    .find(|binding| binding.id == role.primary_binding_id)
                {
                    self.require_binding_capability(
                        binding,
                        term_core::mission::capability::role_kind(role.role),
                    )?;
                }
                if !bindings.iter().any(|binding| {
                    binding.id == role.primary_binding_id
                        && binding.enabled
                        && !binding.model_id.trim().is_empty()
                }) {
                    return Err(Self::with_reason(
                        MissionRpcError::new(
                            MissionErrorCode::ModelUnavailable,
                            format!(
                                "the {:?} role needs an enabled binding with an explicit model",
                                role.role
                            ),
                        ),
                        "model_unavailable",
                    ));
                }
            }
            let ordinal = snapshot
                .tasks
                .iter()
                .map(|task| task.ordinal)
                .max()
                .map_or(0, |ordinal| ordinal + 1);
            upserts.push(Entity::Task(Box::new(Task {
                id: Id::generate(),
                mission_id: mission.id.clone(),
                title: "Plan the mission".into(),
                kind: TaskKind::Plan,
                role: Some(Role::Lead),
                state: TaskState::Ready,
                required: true,
                parent_task_id: None,
                depends_on: Vec::new(),
                contract: TaskContract {
                    objective_ref: mission.goal_ref.clone(),
                    requirement_ids: mission.requirements.iter().map(|r| r.id.clone()).collect(),
                    input_artifact_ids: Vec::new(),
                    allowed_paths: Vec::new(),
                    expected_outputs: vec![ExpectedOutput::Report],
                    verification_ids: mission.policy.allowed_verification_ids.clone(),
                    specialty: None,
                },
                binding_id: Some(lead.primary_binding_id.clone()),
                active_run_id: None,
                ordinal,
                attempt_count: 0,
                repair_cycle: 0,
                failure_repair_run_ids: vec![],
                integration: None,
                replacement_of: None,
                blocked_code: None,
                dispatch_after_unix_ms: None,
                workspace_id: None,
                created_at: now.clone(),
                updated_at: now.clone(),
            })));
        }
        if action == MissionAction::Cancel {
            let mut stopping = false;
            for run in snapshot
                .runs
                .iter()
                .filter(|run| run.holds_execution_slot())
            {
                let (next, intent) = super::outbox::prepare_cancel(run, &now);
                stopping |= next.holds_execution_slot();
                outbox.extend(intent);
                upserts.push(Entity::Run(Box::new(next)));
            }
            for mut task in snapshot
                .tasks
                .into_iter()
                .filter(|task| !task.state.is_terminal())
            {
                task.state = TaskState::Cancelled;
                task.updated_at = now.clone();
                if let Some(run_id) = &task.active_run_id {
                    if upserts.iter().any(|entity| matches!(entity, Entity::Run(run) if &run.id == run_id && run.state == RunState::Cancelled)) {
                        task.active_run_id = None;
                    }
                }
                upserts.push(Entity::Task(Box::new(task)));
            }
            for mut decision in snapshot
                .decisions
                .into_iter()
                .filter(|d| d.state == DecisionState::Open)
            {
                decision.state = DecisionState::Obsolete;
                upserts.push(Entity::Decision(Box::new(decision)));
            }
            mission.open_decision_count = 0;
            if !stopping {
                state = MissionState::Cancelled;
            }
        }
        mission.state = state;
        mission.revision =
            U64String::new(params.expected_revision.get() + 1).expect("fits SQLite bound");
        mission.updated_at = now;
        upserts.insert(0, Entity::Mission(Box::new(mission)));
        let event_type = MissionEventType::Changed;
        let params_json = serde_json::to_value(&params).unwrap_or(Value::Null);
        let applied = self.apply(ApplyPlan {
            request_id: params.request_id,
            method: methods::MISSION_CONTROL.to_string(),
            params: params_json,
            mission_id: params.mission_id.clone(),
            mode: ApplyMode::Mutate {
                expected_revision: params.expected_revision.get(),
            },
            event_type,
            upserts,
            deletes: Vec::new(),
            outbox,
            adopt: vec![],
        })?;
        Ok(Self::mutation_handled(&applied))
    }

    fn archive(
        &self,
        params: &MissionControlParams,
        mut mission: Mission,
        action: MissionControlAction,
    ) -> Result<Handled, MissionRpcError> {
        // archive = list classification, not deletion (01 §6): active
        // missions refuse; terminal missions toggle archived_at.
        let archiving = action == MissionControlAction::Archive;
        if archiving {
            if !matches!(
                mission.state,
                MissionState::Completed | MissionState::Failed | MissionState::Cancelled
            ) {
                return Err(MissionRpcError::new(
                    MissionErrorCode::InvalidState,
                    "only terminal missions can be archived",
                ));
            }
            if mission.archived_at.is_some() {
                return Err(MissionRpcError::new(
                    MissionErrorCode::InvalidState,
                    "mission is already archived",
                ));
            }
            mission.archived_at = Some(self.now());
        } else if mission.archived_at.is_none() {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidState,
                "mission is not archived",
            ));
        } else {
            mission.archived_at = None;
        }
        mission.revision =
            U64String::new(params.expected_revision.get() + 1).expect("fits SQLite bound");
        mission.updated_at = self.now();
        let event_type = if archiving {
            MissionEventType::Archived
        } else {
            MissionEventType::Changed
        };
        let params_json = serde_json::to_value(params).unwrap_or(Value::Null);
        let applied = self.apply(ApplyPlan {
            request_id: params.request_id.clone(),
            method: methods::MISSION_CONTROL.to_string(),
            params: params_json,
            mission_id: params.mission_id.clone(),
            mode: ApplyMode::Mutate {
                expected_revision: params.expected_revision.get(),
            },
            event_type,
            upserts: vec![Entity::Mission(Box::new(mission))],
            deletes: Vec::new(),
            outbox: Vec::new(),
            adopt: vec![],
        })?;
        Ok(Self::mutation_handled(&applied))
    }

    fn mission_message(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        let params: MissionMessageParams = Self::parse(params)?;
        let snapshot = super::workflow::load_entities(&self.storage, &params.mission_id)?;
        let mission = &snapshot.mission;
        if !super::timing::revision_accepts(mission, params.expected_revision.get()) {
            return Err(Self::store_error(MissionStoreError::RevisionConflict {
                expected_revision: params.expected_revision.get(),
                current_revision: mission.revision.get(),
            }));
        }
        if let Some(original_id) = &params.supersedes_message_id {
            self.validate_message_replacement(
                &snapshot,
                original_id,
                params.target_task_id.as_ref(),
            )?;
        }
        if matches!(
            mission.state,
            MissionState::Stopping
                | MissionState::Completed
                | MissionState::Failed
                | MissionState::Cancelled
        ) {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidState,
                "terminal or stopping missions accept no new messages; start a follow-up mission",
            ));
        }
        validate_artifact_ref(&params.body_ref)?;
        if params.body_ref.bytes.get() > self.limits.max_message_bytes as u64 {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidArgument,
                format!(
                    "message body exceeds {} bytes",
                    self.limits.max_message_bytes
                ),
            ));
        }
        if let Some(target) = &params.target_task_id {
            let task = snapshot
                .tasks
                .iter()
                .find(|t| &t.id == target)
                .ok_or_else(|| {
                    MissionRpcError::new(
                        MissionErrorCode::NotFound,
                        format!("task {target} not found in this mission"),
                    )
                })?;
            if task.state.is_terminal()
                || task.kind == TaskKind::Verify
                || task.is_deterministic_integration()
            {
                return Err(MissionRpcError::new(MissionErrorCode::InvalidState,
                    "this task cannot receive instructions; send a planning message to the Lead or start a follow-up mission"));
            }
        }
        let params_json = serde_json::to_value(&params).unwrap_or(Value::Null);
        let message = term_contracts::mission::types::Message {
            id: Id::generate(),
            mission_id: params.mission_id.clone(),
            target_task_id: params.target_task_id.clone(),
            role: term_contracts::mission::types::MessageRole::User,
            run_id: None,
            body_ref: params.body_ref.clone(),
            delivery: term_contracts::mission::types::MessageDelivery::Queued,
            supersedes_message_id: params.supersedes_message_id.clone(),
            created_at: self.now(),
        };
        let body = self.message_body(&params.mission_id, &message)?;
        if params.supersedes_message_id.is_some() && body.trim().is_empty() {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidArgument,
                "replacement instruction is empty",
            ));
        }
        let intent = super::messaging::route_intent(&message, None);
        let mut upserts = vec![Entity::Message(Box::new(message))];
        let mut next_mission = mission.clone();
        next_mission.revision =
            U64String::new(params.expected_revision.get() + 1).map_err(|_| {
                MissionRpcError::new(MissionErrorCode::InvalidArgument, "revision overflow")
            })?;
        next_mission.updated_at = self.now();
        if params.target_task_id.is_none() {
            if let Some(task) = self.message_lead_task(&snapshot)? {
                next_mission.phase = Phase::Planning;
                upserts.push(Entity::Task(Box::new(task)));
            }
        }
        upserts.push(Entity::Mission(Box::new(next_mission)));
        let applied = self.apply(ApplyPlan {
            request_id: params.request_id,
            method: methods::MISSION_MESSAGE.to_string(),
            params: params_json,
            mission_id: params.mission_id.clone(),
            mode: ApplyMode::Mutate {
                expected_revision: params.expected_revision.get(),
            },
            event_type: MissionEventType::Changed,
            upserts,
            deletes: Vec::new(),
            outbox: vec![intent],
            adopt: vec![],
        })?;
        let mut handled = Self::mutation_handled(&applied);
        handled.notify = Some((params.mission_id.clone(), applied.result.revision.get()));
        Ok(handled)
    }

    fn mission_request_get(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        let params: MissionRequestGetParams = Self::parse(params)?;
        let stored = self
            .storage
            .mission_request(&params.request_id)
            .map_err(Self::store_error)?;
        let result = match stored {
            None => MissionRequestGetResult {
                state: MissionRequestState::NotFound,
                result: None,
            },
            Some(stored) => {
                // A stored row with this id but a different fingerprint is a
                // conflict; the caller should not blind-retry (01 §2).
                MissionRequestGetResult {
                    state: MissionRequestState::Committed,
                    result: Some(stored.response),
                }
            }
        };
        Ok(serde_json::to_value(&result).unwrap_or(Value::Null).into())
    }

    // ---- bindings / templates / verification ------------------------------

    fn binding_list(&self) -> Result<Handled, MissionRpcError> {
        let values = self.storage.mission_bindings().map_err(Self::store_error)?;
        let mut bindings = Vec::with_capacity(values.len());
        for value in values {
            bindings.push(self.observed_binding(&value)?);
        }
        let result = BindingListResult { bindings };
        Ok(serde_json::to_value(&result).unwrap_or(Value::Null).into())
    }

    fn binding_save(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        let params: BindingSaveParams = Self::parse(params)?;
        if params
            .binding
            .estimated_run_cost_usd_micros
            .as_ref()
            .is_some_and(|v| v.get() == 0)
        {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidArgument,
                "estimated run cost must be positive or null",
            ));
        }
        if params
            .binding
            .credential_ref
            .as_deref()
            .is_some_and(|r| crate::connections::credential_id(r).is_err())
        {
            return Err(MissionRpcError::new(MissionErrorCode::InvalidArgument,
                "credential_ref must be an opaque keyring reference; API keys must be provisioned locally"));
        }
        // Consent is a user statement about this connection, never evidence.
        // The value is the version observed when it was given and is kept for
        // display only (11 §3.4); it is still validated as a version string so
        // an unbounded blob cannot be parked in the document.
        if params
            .binding
            .experimental_version
            .as_deref()
            .is_some_and(|version| {
                version.trim().is_empty()
                    || version.len() > 128
                    || version != version.trim()
                    || !term_contracts::mission::validation::has_no_control_chars(version)
                    || version.contains(['\n', '\r', '\t'])
            })
        {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidArgument,
                "experimental_version must be a non-empty CLI version string of at most 128 bytes, or null",
            ));
        }
        let params_json = serde_json::to_value(&params).unwrap_or(Value::Null);
        let prior = self
            .storage
            .mission_bindings()
            .map_err(Self::store_error)?
            .into_iter()
            .find(|value| {
                value.get("id").and_then(Value::as_str) == Some(params.binding.id.as_str())
            });
        let mut binding = params.binding.clone();
        // A consent carried unchanged onto another launch target is not a new
        // statement for it (binding_evidence::consent_carried_to_new_target).
        let carried = prior.as_ref().is_some_and(|prior| {
            super::binding_evidence::consent_carried_to_new_target(prior, &binding)
        });
        if carried {
            binding.experimental_version = None;
        }
        // Local evidence is daemon-measured (11 §2.4): whatever the client
        // sent is dropped, and the stored one is carried only onto the same
        // launch target.
        super::binding_evidence::carry_local_evidence(prior.as_ref(), &mut binding);
        let document = self.binding_document(binding, prior.as_ref());
        let saved = self
            .storage
            .save_mission_binding(
                params.request_id,
                methods::BINDING_SAVE,
                &Self::fingerprint(methods::BINDING_SAVE, &params_json),
                params.expected_revision.get(),
                document,
                self.now(),
            )
            .map_err(Self::store_error)?;
        let binding = self.observed_binding(&saved.document)?;
        let result = BindingSaveResult { binding };
        Ok(serde_json::to_value(&result).unwrap_or(Value::Null).into())
    }

    fn binding_probe(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        let params: BindingProbeParams = Self::parse(params)?;
        let values = self.storage.mission_bindings().map_err(Self::store_error)?;
        let binding = values
            .into_iter()
            .find(|value| {
                value.get("id").and_then(|v| v.as_str()) == Some(params.binding_id.as_str())
            })
            .ok_or_else(|| {
                MissionRpcError::new(
                    MissionErrorCode::NotFound,
                    format!("binding {} not found", params.binding_id),
                )
            })?;
        let binding: Binding = serde_json::from_value(binding).map_err(|e| {
            MissionRpcError::new(MissionErrorCode::Internal, format!("stored binding: {e}"))
        })?;
        use term_contracts::mission::types::RuntimeKind;
        let mut binding = binding;
        let mut executable = None;
        let installation = if binding.runtime == RuntimeKind::Fake {
            InstallationStatus::Verified
        } else {
            let observation = (self.installation_probe)(&binding.program, binding.runtime, None);
            binding.runtime_version = observation.as_ref().ok().map(|o| o.version.clone());
            executable = observation.as_ref().ok().and_then(|o| o.executable.clone());
            // 11 §7: a verified installation is also measured here. The
            // self-check runs no model and reads no token; it only proves what
            // this exact version does on this machine, and its report is the
            // binding's daemon-owned `local_evidence` from now on.
            if let (Ok(observed), true) = (&observation, self.probe_locally) {
                let env = (self.detection_env)();
                let report = (self.local_prober)(
                    &binding,
                    &binding.program,
                    &observed.version,
                    &env,
                    crate::agent_runtime::local_probe::BUDGET,
                );
                let probed_at = self.now();
                let prior = binding.local_evidence.take();
                binding.local_evidence = Some(super::binding_evidence::merged_local_probe(
                    prior,
                    std::env::consts::OS,
                    &observed.version,
                    &binding.model_id,
                    report,
                    probed_at,
                ));
            }
            binding.capabilities = (self.capability_registry)(
                &binding,
                std::env::consts::OS,
                binding.runtime_version.as_deref(),
            );
            match observation {
                Ok(_) => InstallationStatus::Verified,
                Err(failure) => failure.installation_status(),
            }
        };
        // Picker hints for the model field. Asked only once the CLI answered,
        // and never allowed to change the probe's outcome (a listing failure
        // is an empty list, not a probe failure).
        let models = if installation == InstallationStatus::Verified {
            let env = (self.detection_env)();
            (self.model_lister)(&super::binding_evidence::ModelListRequest {
                runtime: binding.runtime,
                program: &binding.program,
                provider_id: Some(&binding.provider_id),
                env: &env,
            })
        } else {
            Vec::new()
        };
        // Preserve the user's enabled setting. Installation is a separate fact.
        // Commit against the revision read before running the process so a
        // concurrent settings edit cannot be overwritten by an old probe.
        binding.checked_at = Some(self.now());
        let document = self.probe_document(&binding, executable);
        let saved = self
            .storage
            .save_mission_binding(
                Id::generate(),
                methods::BINDING_PROBE,
                &Self::fingerprint(methods::BINDING_PROBE, &document),
                binding.revision.get(),
                document,
                self.now(),
            )
            .map_err(Self::store_error)?;
        let binding = self.observed_binding(&saved.document)?;
        let result = BindingProbeResult {
            binding,
            models,
            installation,
        };
        Ok(serde_json::to_value(&result).unwrap_or(Value::Null).into())
    }

    /// Read-only first-run discovery: nothing is stored and no credential
    /// content is returned. Params are ignored like `binding.list`.
    fn runtime_detect(&self) -> Result<Handled, MissionRpcError> {
        use crate::agent_runtime::{capability_evidence, detection};
        use term_contracts::mission::rpc::DetectedRuntime;
        let env = (self.detection_env)();
        let installation_probe = &*self.installation_probe;
        let os = std::env::consts::OS;
        let facts = detection::detect_all(
            &env,
            &|program: &str, runtime: term_contracts::mission::types::RuntimeKind| {
                installation_probe(program, runtime, None)
            },
        );
        // The provider and model this row's one-click setup would save, picked
        // before the local probes so each probe can ask about that exact model.
        let plans: Vec<(&'static str, Option<&'static str>, String)> = facts
            .iter()
            .map(|facts| {
                let suggested_provider_id =
                    capability_evidence::evidence_provider_id(facts.runtime).unwrap_or_default();
                // Suggest the pinned model only where live evidence exists for
                // this exact version/architecture; otherwise it would override
                // the user's configured model for no verified benefit.
                let proven_model_id = capability_evidence::evidence_model_id(facts.runtime, os)
                    .filter(|_| {
                        facts.version.as_deref().is_some_and(|version| {
                            capability_evidence::capabilities_for(facts.runtime, os, Some(version))
                                .structured_result
                                .supported
                        })
                    });
                // Owned before it meets the `'static` pin: the configured id is
                // borrowed from this row's facts, the proven id is a registry constant.
                let model_id = proven_model_id
                    .map(str::to_owned)
                    .or_else(|| facts.configured_model_id.clone())
                    .unwrap_or_default();
                (suggested_provider_id, proven_model_id, model_id)
            })
            .collect();
        // 11 §6/§7: the cheap half of the local self-check, one row per
        // thread exactly like `detect_all` above, so this adds one CLI
        // handshake to the call's latency rather than the sum of all of them.
        // Only a verified installation is measured — there is nothing to ask
        // an executable that could not even report its version.
        let cheap_prober = &*self.cheap_prober;
        let detection_env = &env;
        let reports: Vec<Option<term_contracts::mission::types::LocalProbeReport>> =
            std::thread::scope(|scope| {
                let workers: Vec<_> = facts
                    .iter()
                    .zip(plans.iter())
                    .map(|(facts, plan)| {
                        let program = facts.program.clone()?;
                        if program.is_empty()
                            || facts.installation != InstallationStatus::Verified
                            || plan.2.trim().is_empty()
                        {
                            return None;
                        }
                        let (runtime, model_id) = (facts.runtime, plan.2.clone());
                        std::thread::Builder::new()
                            .name("runtime-local-probe".into())
                            .spawn_scoped(scope, move || {
                                cheap_prober(
                                    runtime,
                                    &program,
                                    &model_id,
                                    detection_env,
                                    crate::agent_runtime::local_probe::DETECT_BUDGET,
                                )
                            })
                            .ok()
                    })
                    .collect();
                workers
                    .into_iter()
                    // A probe that panicked or could not get a thread is
                    // "not measured here", never a failed detection.
                    .map(|worker| worker.and_then(|worker| worker.join().unwrap_or(None)))
                    .collect()
            });
        let mut runtimes: Vec<DetectedRuntime> = facts
            .into_iter()
            .zip(plans)
            .zip(reports)
            .map(
                |((facts, (suggested_provider_id, proven_model_id, model_id)), report)| {
                    // Same gate as mission start: registry capabilities for the
                    // exact binding the one-click setup would save, per role kind.
                    // Start also rejects an empty model, so no model means no role.
                    // Shipped evidence decides `verified`; the same binding with
                    // this connection's consent gives the experimental roles.
                    let mut shipped_claims = false;
                    let (verified_roles, experimental_roles): (Vec<Role>, Vec<Role>) =
                        match (facts.program.as_deref(), facts.version.as_deref()) {
                            (Some(program), Some(version)) if !model_id.trim().is_empty() => {
                                let mut binding = detection::candidate_binding(
                                    facts.runtime,
                                    program,
                                    suggested_provider_id,
                                    &model_id,
                                    version,
                                );
                                // The row is judged with the same daemon-owned
                                // local evidence the saved connection would carry
                                // (11 §7). Nothing here is stored.
                                binding.local_evidence = report.clone().map(|probe| {
                                    term_contracts::mission::types::LocalEvidence {
                                        os: os.to_owned(),
                                        version: version.to_owned(),
                                        model_id: model_id.clone(),
                                        // Not a saved measurement: no timestamp,
                                        // and no run history for a row that has
                                        // never been a connection.
                                        probed_at: None,
                                        probe: Some(probe),
                                        runs: Default::default(),
                                    }
                                });
                                let roles = |binding: &Binding| -> Vec<Role> {
                                    detection::SETUP_ROLES
                                        .into_iter()
                                        .filter(|role| {
                                            self.require_binding_capability(
                                                binding,
                                                term_core::mission::capability::role_kind(*role),
                                            )
                                            .is_ok()
                                        })
                                        .collect()
                                };
                                binding.capabilities =
                                    (self.capability_registry)(&binding, os, Some(version));
                                // Shipped-only projection: a claim this machine
                                // measured itself is `verified_locally`, not
                                // `verified` (11 §7).
                                shipped_claims = capability_evidence::claims_any(
                                    &capability_evidence::evidence_for_binding(
                                        &binding,
                                        os,
                                        Some(version),
                                    ),
                                );
                                let verified = roles(&binding);
                                binding.experimental_version = Some(version.to_owned());
                                binding.capabilities =
                                    (self.capability_registry)(&binding, os, Some(version));
                                (verified, roles(&binding))
                            }
                            _ => (Vec::new(), Vec::new()),
                        };
                    let grade = capability_evidence::compatibility_grade(
                        facts.runtime,
                        os,
                        facts.program.is_some()
                            && facts.installation != InstallationStatus::NotFound,
                        facts.version.as_deref(),
                        shipped_claims,
                        verified_roles.len() == detection::SETUP_ROLES.len(),
                        report.as_ref().map(|report| report.protocol_ok),
                    );
                    // A second provider route this CLI serves: Claude Code reads Z.ai Coding
                    // Plan candidates from its own transcripts (the ids that are neither
                    // `claude-*` nor an alias) — the mission equivalent of the `ccg` launch
                    // profile. Only a found CLI has routes worth listing.
                    let alt_models = match facts.runtime {
                        RuntimeKind::Claude
                            if facts.program.as_deref().is_some_and(|p| !p.is_empty()) =>
                        {
                            let candidates = crate::agent_runtime::model_catalog::claude_models(
                                env.claude_dir().as_deref(),
                                Some("zai-coding-plan"),
                            );
                            if candidates.is_empty() {
                                None
                            } else {
                                Some(vec![term_contracts::mission::rpc::RuntimeAltModels {
                                    provider_id: "zai-coding-plan".to_owned(),
                                    models: candidates,
                                }])
                            }
                        }
                        _ => None,
                    };
                    DetectedRuntime {
                        runtime: facts.runtime,
                        program: facts.program.unwrap_or_default(),
                        version: facts.version,
                        installation: facts.installation,
                        login: facts.login,
                        configured_model_id: facts.configured_model_id,
                        suggested_provider_id: suggested_provider_id.to_owned(),
                        proven_model_id: proven_model_id.map(str::to_owned),
                        // Filled below, once, off the detection pass.
                        models: Vec::new(),
                        alt_models,
                        verified_roles,
                        grade,
                        experimental_roles,
                    }
                },
            )
            .collect();
        self.attach_model_hints(&mut runtimes, &env);
        let result = RuntimeDetectResult { runtimes };
        Ok(serde_json::to_value(&result).unwrap_or(Value::Null).into())
    }

    /// Picker hints for the model field: what each installed CLI says it
    /// offers right now, so the field is not limited to the one evidence-pinned
    /// model plus whatever the config file happens to hold. Nothing here gates
    /// a role or changes a grade.
    ///
    /// Only the rows a one-click setup can actually turn into a connection are
    /// asked, with the provider that setup would save:
    /// * OpenCode is skipped. Its binding needs a stored `credential_ref` and
    ///   `endpoint_ref`, which a detection row has no way to produce — that is
    ///   why its `experimental_roles` are always empty and why `binding.probe`
    ///   judges it once the connection is saved. Its listing spawns the CLI for
    ///   about 1.6s against roughly 0.05s for the Codex app-server, and because
    ///   the rows are listed concurrently that one unusable hint would set the
    ///   latency of the whole call. `binding.probe` offers those candidates,
    ///   with the provider they belong to.
    /// * `provider_id` is the row's `suggested_provider_id`, the same value
    ///   `detection::candidate_binding` is given, so a row never offers a model
    ///   the binding it would save could not run.
    ///
    /// One listing per runtime, run concurrently: each spawns its own CLI, so
    /// serialising them would add up their timeouts on a call the settings
    /// screen waits on. A listing that panics or fails leaves that row's hints
    /// empty; detection itself never fails for it.
    fn attach_model_hints(
        &self,
        runtimes: &mut [term_contracts::mission::rpc::DetectedRuntime],
        env: &crate::agent_runtime::detection::DetectionEnv,
    ) {
        let lister = &*self.model_lister;
        // Targets are copied out first: the spawned listings borrow this
        // vector, so `runtimes` stays free to be written back into.
        let targets: Vec<Option<(RuntimeKind, String, String)>> = runtimes
            .iter()
            .map(|detected| {
                if detected.installation != InstallationStatus::Verified
                    || detected.runtime == RuntimeKind::Opencode
                {
                    return None;
                }
                Some((
                    detected.runtime,
                    detected.program.clone(),
                    detected.suggested_provider_id.clone(),
                ))
            })
            .collect();
        let listings: Vec<Option<Vec<ProbeModel>>> = std::thread::scope(|scope| {
            let handles: Vec<_> = targets
                .iter()
                .map(|target| {
                    target.as_ref().map(|(runtime, program, provider_id)| {
                        scope.spawn(move || {
                            lister(&super::binding_evidence::ModelListRequest {
                                runtime: *runtime,
                                program,
                                provider_id: Some(provider_id.as_str()),
                                env,
                            })
                        })
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.map(|handle| handle.join().unwrap_or_default()))
                .collect()
        });
        for (detected, listing) in runtimes.iter_mut().zip(listings) {
            detected.models = listing.unwrap_or_default();
        }
    }

    fn template_list(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        let params: TemplateListParams = Self::parse(params)?;
        let values = self
            .storage
            .mission_configs("template")
            .map_err(Self::store_error)?;
        let mut templates = Vec::new();
        for value in values {
            let matches_scope = match &params.repository_id {
                None => true,
                Some(wanted) => {
                    value.get("repository_id").and_then(|v| v.as_str()) == Some(wanted.as_str())
                }
            };
            if !matches_scope {
                continue;
            }
            templates.push(
                serde_json::from_value::<term_contracts::mission::types::TeamTemplate>(value)
                    .map_err(|e| {
                        MissionRpcError::new(
                            MissionErrorCode::Internal,
                            format!("stored template: {e}"),
                        )
                    })?,
            );
        }
        let result = TemplateListResult { templates };
        Ok(serde_json::to_value(&result).unwrap_or(Value::Null).into())
    }

    fn template_save(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        let params: TemplateSaveParams = Self::parse(params)?;
        validate_policy(&params.template.policy, &self.ceiling)?;
        validate_role_bindings(&params.template.role_bindings, &params.template.policy)?;
        let params_json = serde_json::to_value(&params).unwrap_or(Value::Null);
        let document = serde_json::to_value(&params.template).unwrap_or(Value::Null);
        let saved = self
            .storage
            .save_mission_config(
                params.request_id,
                methods::TEMPLATE_SAVE,
                &Self::fingerprint(methods::TEMPLATE_SAVE, &params_json),
                "template",
                params.expected_revision.get(),
                document,
                self.now(),
            )
            .map_err(Self::store_error)?;
        let template =
            serde_json::from_value::<term_contracts::mission::types::TeamTemplate>(saved.document)
                .map_err(|e| {
                    MissionRpcError::new(MissionErrorCode::Internal, format!("template save: {e}"))
                })?;
        let result = TemplateSaveResult { template };
        Ok(serde_json::to_value(&result).unwrap_or(Value::Null).into())
    }

    fn verification_list(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        let params: VerificationListParams = Self::parse(params)?;
        let values = self
            .storage
            .mission_configs("verification")
            .map_err(Self::store_error)?;
        let mut commands = Vec::new();
        for value in values {
            if value.get("repository_id").and_then(|v| v.as_str())
                != Some(params.repository_id.as_str())
            {
                continue;
            }
            commands.push(
                serde_json::from_value::<VerificationCommand>(value).map_err(|e| {
                    MissionRpcError::new(MissionErrorCode::Internal, format!("stored command: {e}"))
                })?,
            );
        }
        let result = VerificationListResult { commands };
        Ok(serde_json::to_value(&result).unwrap_or(Value::Null).into())
    }

    fn verification_save(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        let params: VerificationSaveParams = Self::parse(params)?;
        let command = &params.command;
        if command.title.is_empty() || command.title.len() > self.limits.max_title_bytes {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidArgument,
                "command title must be 1..=256 UTF-8 bytes",
            ));
        }
        if command.program.is_empty() || command.program.contains('\0') {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidArgument,
                "program must be a non-empty executable path",
            ));
        }
        if command.timeout_ms == 0 {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidArgument,
                "timeout_ms must be positive",
            ));
        }
        if command.cwd_relative.contains("..") || command.cwd_relative.starts_with('/') {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidArgument,
                "cwd_relative must be repository-relative",
            ));
        }
        let params_json = serde_json::to_value(&params).unwrap_or(Value::Null);
        let document = serde_json::to_value(command).unwrap_or(Value::Null);
        let saved = self
            .storage
            .save_mission_config(
                params.request_id,
                methods::VERIFICATION_SAVE,
                &Self::fingerprint(methods::VERIFICATION_SAVE, &params_json),
                "verification",
                params.expected_revision.get(),
                document,
                self.now(),
            )
            .map_err(Self::store_error)?;
        let command =
            serde_json::from_value::<VerificationCommand>(saved.document).map_err(|e| {
                MissionRpcError::new(MissionErrorCode::Internal, format!("command save: {e}"))
            })?;
        let result = VerificationSaveResult { command };
        Ok(serde_json::to_value(&result).unwrap_or(Value::Null).into())
    }

    // ---- artifact transfer (O05) ------------------------------------------

    fn artifact_begin(
        &self,
        conn: &ConnectionId,
        params: &Value,
    ) -> Result<Handled, MissionRpcError> {
        let params: ArtifactBeginParams = Self::parse(params)?;
        let (upload_id, chunk_bytes) = self.artifacts.begin(conn, &params).map_err(|message| {
            MissionRpcError::new(
                match message.as_str() {
                    "bytes exceed the artifact limit" => MissionErrorCode::ArtifactLimit,
                    _ => MissionErrorCode::InvalidArgument,
                },
                message,
            )
        })?;
        let result = ArtifactBeginResult {
            upload_id,
            chunk_bytes,
        };
        Ok(serde_json::to_value(&result).unwrap_or(Value::Null).into())
    }

    fn artifact_write(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        let params: ArtifactWriteParams = Self::parse(params)?;
        let next_offset = self
            .artifacts
            .write(&params)
            .map_err(|(code, message)| MissionRpcError::new(code, message))?;
        let result = ArtifactWriteResult {
            next_offset: U64String::new(next_offset).expect("fits SQLite bound"),
        };
        Ok(serde_json::to_value(&result).unwrap_or(Value::Null).into())
    }

    fn artifact_commit(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        let params: ArtifactCommitParams = Self::parse(params)?;
        let reference = self
            .artifacts
            .commit(&params)
            .map_err(|(code, message)| MissionRpcError::new(code, message))?;
        Ok(serde_json::to_value(&reference)
            .unwrap_or(Value::Null)
            .into())
    }

    fn artifact_read(&self, params: &Value) -> Result<Handled, MissionRpcError> {
        let params: ArtifactReadParams = Self::parse(params)?;
        if params.max_bytes as u64 > 4096 {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidArgument,
                "max_bytes must be <= 4096",
            ));
        }
        let result: ArtifactReadResult = self
            .artifacts
            .read(&params)
            .map_err(|(code, message)| MissionRpcError::new(code, message))?;
        Ok(serde_json::to_value(&result).unwrap_or(Value::Null).into())
    }
}

/// Opaque cursor helper: `(updated_at, id)` tuples travel base64url-encoded.
struct SnapshotCursor(String, String);

impl SnapshotCursor {
    fn encode(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("{}\t{}", self.0, self.1))
    }

    fn decode_repository(value: &str) -> Result<(String, String), MissionRpcError> {
        use base64::Engine;
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|e| {
                MissionRpcError::new(MissionErrorCode::CursorExpired, format!("cursor: {e}"))
            })?;
        let text = String::from_utf8(decoded).map_err(|_| {
            MissionRpcError::new(MissionErrorCode::CursorExpired, "cursor is not UTF-8")
        })?;
        let (updated_at, id) = text.split_once('\t').ok_or_else(|| {
            MissionRpcError::new(MissionErrorCode::CursorExpired, "cursor shape mismatch")
        })?;
        Ok((updated_at.to_string(), id.to_string()))
    }
}

/// Every method owned by the mission service (dispatch gate).
pub fn is_mission_method(method: &str) -> bool {
    matches!(
        method,
        methods::MISSION_CREATE
            | methods::REPOSITORY_INSPECT
            | methods::MISSION_LIST
            | methods::MISSION_SNAPSHOT
            | methods::MISSION_EVENTS
            | methods::MISSION_CONTROL
            | methods::MISSION_ACCEPT
            | methods::MISSION_MESSAGE
            | methods::MISSION_PLAN_APPLY
            | methods::MISSION_TASK_CONTROL
            | methods::MISSION_POLICY_UPDATE
            | methods::MISSION_FINDING_RESOLVE
            | methods::MISSION_DECISION_ANSWER
            | methods::MISSION_REQUEST_GET
            | methods::MISSION_ACTIVITY
            | methods::BINDING_LIST
            | methods::BINDING_SAVE
            | methods::BINDING_PROBE
            | methods::RUNTIME_DETECT
            | methods::TEMPLATE_LIST
            | methods::TEMPLATE_SAVE
            | methods::VERIFICATION_LIST
            | methods::VERIFICATION_SAVE
            | methods::ARTIFACT_BEGIN
            | methods::ARTIFACT_WRITE
            | methods::ARTIFACT_COMMIT
            | methods::ARTIFACT_READ
            | methods::MISSION_RUN_ATTEST_EXITED
            | methods::WORKSPACE_USAGE
            | methods::WORKSPACE_CLEANUP
    )
}

//! Daemon-owned installation observations, stored atomically with a binding.
//! Wire Binding fields are projections, never evidence supplied by a client.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use term_contracts::mission::{types::*, MissionErrorCode, MissionRpcError};

use super::service::MissionService;
use crate::agent_runtime::installation;
use crate::agent_runtime::installation_identity::ExecutableIdentity;

const OBSERVATION: &str = "_installation_observation";

pub(super) type InstallationProbe = dyn Fn(
        &str,
        RuntimeKind,
        Option<&ExecutableIdentity>,
    ) -> Result<installation::Installation, installation::ProbeFailure>
    + Send
    + Sync;
pub(super) type CapabilityRegistry =
    dyn Fn(&Binding, &str, Option<&str>) -> RuntimeCapabilities + Send + Sync;

/// What `runtime.detect` and `binding.probe` know when they ask a runtime
/// which models it offers. `provider_id` narrows a listing that spans
/// providers: `binding.probe` passes the saved binding's provider, detection
/// passes the one its one-click setup would save.
pub(super) struct ModelListRequest<'a> {
    pub(super) runtime: RuntimeKind,
    /// Absolute program path; empty when no CLI was found.
    pub(super) program: &'a str,
    /// `None` only from a caller that cannot name the provider yet. Runtimes
    /// whose ids are meaningless without one (OpenCode) answer with nothing,
    /// and a runtime asked for a provider other than its own answers with
    /// nothing either: that binding could not launch the pick anyway.
    pub(super) provider_id: Option<&'a str>,
    /// The same PATH/home/config roots detection reads, so a sandboxed test
    /// env applies here too.
    pub(super) env: &'a crate::agent_runtime::detection::DetectionEnv,
}

/// Picker hints only — the models a user may pick from, never evidence.
/// Injected so no suite spawns a CLI, and so a failure here can never fail
/// the call that asked (every implementation returns an empty list instead).
pub(super) type ModelLister =
    dyn Fn(&ModelListRequest<'_>) -> Vec<term_contracts::mission::rpc::ProbeModel> + Send + Sync;

/// Ask the installed CLI. Codex speaks `model/list` over its app-server, and
/// is asked only when the named provider is its own; OpenCode prints
/// `provider/model` lines, and is asked only when the provider to keep is
/// known at all; Claude Code has no listing at all, so its hints come from
/// the aliases it resolves plus the model ids its own transcripts show in use,
/// narrowed to the connection's provider (the same CLI routes to non-Anthropic
/// backends, and a transcript does not say which one answered).
///
/// Every branch therefore reads `provider_id` before it answers, because every
/// candidate is offered to a binding that names one provider: a candidate that
/// binding cannot launch is worse than no candidate, since it is refused only
/// at start, long after the user chose it in the picker. What a *missing*
/// provider means differs per runtime and is stated at the branch.
pub(super) fn list_models(
    request: &ModelListRequest<'_>,
) -> Vec<term_contracts::mission::rpc::ProbeModel> {
    use crate::agent_runtime::{capability_evidence, codex, model_catalog, opencode};
    if request.runtime != RuntimeKind::Claude && request.program.is_empty() {
        return Vec::new();
    }
    match request.runtime {
        RuntimeKind::Codex => {
            // Codex has exactly one provider, so unlike OpenCode below a
            // missing one is not ambiguous: `None` (a caller that cannot name
            // the provider yet) still gets the catalog, and only a provider
            // that is named and is not Codex's own is refused. That binding is
            // one `capability_evidence::capabilities_for_binding` leaves
            // `unclaimed()` — it can never launch — so filling its picker only
            // moves the refusal to the moment the user presses start. The
            // evidence provider is asked for rather than spelled out here, so
            // the guard cannot drift from the registry it defers to.
            if request.provider_id.is_some_and(|provider_id| {
                capability_evidence::evidence_provider_id(RuntimeKind::Codex) != Some(provider_id)
            }) {
                return Vec::new();
            }
            // The app-server needs a working directory but never touches it
            // here: this session stops before `thread/start`, so no task,
            // workspace, or credential is involved.
            codex::list_models(
                std::path::Path::new(request.program),
                &std::env::temp_dir(),
                CODEX_LIST_TIMEOUT,
            )
            .unwrap_or_default()
        }
        RuntimeKind::Opencode => {
            // `opencode models` prints `provider/model` and a binding stores
            // the two halves separately, so the parser keeps only the model.
            // Without a provider to filter by, rows that are not the same
            // model collapse into one: `openai/gpt-6-astra` and
            // `opencode/gpt-6-astra` both arrive as `gpt-6-astra`, an id that
            // no longer says who serves it. A candidate nobody can resolve is
            // worse than no candidate, so this returns before spawning a CLI.
            let Some(provider_id) = request.provider_id else {
                return Vec::new();
            };
            opencode::models::list(request.program, Some(provider_id))
        }
        RuntimeKind::Claude => {
            model_catalog::claude_models(request.env.claude_dir().as_deref(), request.provider_id)
        }
        RuntimeKind::Fake => Vec::new(),
    }
}

/// Spawning the app-server and completing `initialize` costs more than a
/// `--version` capture, so this sits above the installation probe's budget
/// while still bounding an RPC a user is waiting on.
const CODEX_LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    format: u32,
    target_sha256: String,
    os: String,
    version: Option<String>,
    checked_at: String,
    #[serde(default)]
    executable: Option<ExecutableIdentity>,
    #[serde(default)]
    verified_at_revision: Option<term_contracts::ids::U64String>,
}

fn target(binding: &Binding) -> String {
    let bytes = serde_json::to_vec(&(
        &binding.id,
        binding.runtime,
        &binding.program,
        &binding.provider_id,
        &binding.model_id,
        &binding.effort,
        binding.auth_route,
        &binding.credential_ref,
        &binding.endpoint_ref,
    ))
    .expect("binding identity serializes");
    format!("{:x}", Sha256::digest(bytes))
}

fn observation(document: &Value, binding: &Binding, allow_unpinned: bool) -> Option<Observation> {
    let observed: Observation = serde_json::from_value(document.get(OBSERVATION)?.clone()).ok()?;
    ((observed.format == 2 || (allow_unpinned && observed.format == 1))
        && (observed
            .verified_at_revision
            .as_ref()
            .is_some_and(|revision| {
                revision.get() > 0 && revision.get() <= binding.revision.get()
            })
            || (allow_unpinned && observed.format == 1))
        && observed.target_sha256 == target(binding)
        && observed.os == std::env::consts::OS
        && observed
            .executable
            .as_ref()
            .is_none_or(ExecutableIdentity::is_valid)
        && (observed.version.is_none() || observed.executable.is_some() || allow_unpinned)
        && !observed.checked_at.is_empty())
    .then_some(observed)
}

/// Merge one `binding.probe` self-check into the binding's daemon-owned local
/// evidence (11 §7). The run counters describe one OS, one CLI version and one
/// model, so they survive only while all three still hold: a re-probe of the
/// same installation replaces the report and keeps them, a model change keeps
/// the (model-independent) report and drops them, and another version starts
/// over.
pub(super) fn merged_local_probe(
    prior: Option<LocalEvidence>,
    os: &str,
    version: &str,
    model_id: &str,
    probe: LocalProbeReport,
    probed_at: Timestamp,
) -> LocalEvidence {
    let runs = match &prior {
        Some(prior) if prior.os == os && prior.version == version && prior.model_id == model_id => {
            prior.runs.clone()
        }
        _ => LocalRunEvidence::default(),
    };
    LocalEvidence {
        os: os.to_owned(),
        version: version.to_owned(),
        model_id: model_id.to_owned(),
        probed_at: Some(probed_at),
        probe: Some(probe),
        runs,
    }
}

/// `binding.save` never accepts local evidence from a client (11 §2.4/§7).
/// The stored measurement is carried only when the save keeps the same launch
/// target — the same fields a consent is tied to. Only the model may differ:
/// the probe is a property of the CLI, the run counters are a property of the
/// model, so the first is kept and the second restarts.
pub(super) fn carry_local_evidence(prior: Option<&Value>, binding: &mut Binding) {
    binding.local_evidence = None;
    let Some(prior) = prior else {
        return;
    };
    let Ok(stored) = serde_json::from_value::<Binding>(prior.clone()) else {
        return;
    };
    let Some(mut evidence) = stored.local_evidence else {
        return;
    };
    if stored.runtime != binding.runtime
        || stored.program != binding.program
        || stored.provider_id != binding.provider_id
        || stored.auth_route != binding.auth_route
        || stored.credential_ref != binding.credential_ref
        || stored.endpoint_ref != binding.endpoint_ref
    {
        return;
    }
    if evidence.model_id != binding.model_id {
        evidence.model_id.clone_from(&binding.model_id);
        evidence.runs = LocalRunEvidence::default();
    }
    binding.local_evidence = Some(evidence);
}

/// A prepared Run carries the consent of its binding snapshot, but consent is
/// the user's current statement: it reaches a launch only while the current
/// binding document still holds one. A later withdrawal (null) or a deleted
/// binding clears it; the version the consent names is display-only.
fn consent_withdrawn(snapshot: &Binding, current: Option<&Value>) -> bool {
    if snapshot
        .experimental_version
        .as_deref()
        .is_none_or(|accepted| accepted.trim().is_empty())
    {
        return false;
    }
    // 11 §3.4: consent is a per-connection statement. Only a withdrawal (null,
    // omitted) or a deleted binding revokes it; a consent restated for a newer
    // observed version is still consent for this connection.
    !current
        .and_then(|document| document.get("experimental_version"))
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
}

/// A consent names one CLI version for one launch target. A `binding.save`
/// that moves the binding to another runtime, executable, provider, auth
/// route, credential or endpoint while only carrying the stored consent along
/// (same value) does not restate it. A different version in the same save is
/// a new statement for the new target.
pub(super) fn consent_carried_to_new_target(prior: &Value, binding: &Binding) -> bool {
    let Some(accepted) = binding.experimental_version.as_deref() else {
        return false;
    };
    let Ok(stored) = serde_json::from_value::<Binding>(prior.clone()) else {
        return false;
    };
    stored.experimental_version.as_deref() == Some(accepted)
        && (stored.runtime != binding.runtime
            || stored.program != binding.program
            || stored.provider_id != binding.provider_id
            || stored.auth_route != binding.auth_route
            || stored.credential_ref != binding.credential_ref
            || stored.endpoint_ref != binding.endpoint_ref)
}

/// The Run was prepared while this connection had experimental consent and
/// the current document no longer does (11 §3.4 keeps the withdrawal, not the
/// version equality). Kept apart from the generic capability slug so the UI
/// can offer to accept again rather than send the user to "check installation".
fn consent_withdrawn_error(missing: &str) -> MissionRpcError {
    MissionRpcError::with_details(
        MissionErrorCode::CapabilityUnsupported,
        format!(
            "experimental consent was withdrawn for this connection; {missing} is unverified \
             without it. Accept experimental use again or choose a compatible connection"
        ),
        term_contracts::mission::MissionErrorDetails {
            reason_code: Some("experimental_consent_withdrawn".into()),
            ..Default::default()
        },
    )
}

impl MissionService {
    pub(super) fn missing_binding_capability(
        binding: &Binding,
        kind: TaskKind,
    ) -> Option<&'static str> {
        // Production rejects Fake in its adapter factory. Deterministic tests
        // deliberately inject their own adapter and capability scenarios.
        if binding.runtime == RuntimeKind::Fake {
            return None;
        }
        term_core::mission::capability::missing(&binding.capabilities, kind)
    }

    pub(super) fn require_binding_capability(
        &self,
        binding: &Binding,
        kind: TaskKind,
    ) -> Result<(), MissionRpcError> {
        if let Some(missing) = Self::missing_binding_capability(binding, kind) {
            // 11 §3.4: consent is per connection, not per CLI version, so a
            // capability that is still missing here is simply unproven — a
            // newer CLI is answered by "check installation", which re-runs the
            // local self-check, not by asking for consent again.
            return Err(MissionRpcError::with_details(MissionErrorCode::CapabilityUnsupported,
                format!("the selected connection has no verified {missing} support for this task; check installation or choose a compatible connection"),
                term_contracts::mission::MissionErrorDetails { reason_code: Some(format!("capability_{missing}")), ..Default::default() }));
        }
        Ok(())
    }

    pub(super) fn reconcile_capability_blocks(&self) -> Result<(), MissionRpcError> {
        let bindings = self
            .storage
            .mission_bindings()
            .map_err(Self::store_error)?
            .iter()
            .map(|value| self.observed_binding(value))
            .collect::<Result<Vec<_>, _>>()?;
        let mut cursor = None;
        loop {
            let (missions, next) = self
                .storage
                .mission_list(cursor, 50, false)
                .map_err(Self::store_error)?;
            for mission in missions {
                if !matches!(
                    mission.state,
                    MissionState::Running | MissionState::Paused | MissionState::Pausing
                ) {
                    continue;
                }
                let snapshot = super::workflow::load_entities(&self.storage, &mission.id)?;
                let mut upserts = vec![];
                for task in &snapshot.tasks {
                    if task.state != TaskState::Blocked
                        || task.active_run_id.is_some()
                        || !task
                            .blocked_code
                            .as_deref()
                            .is_some_and(|code| code.starts_with("capability_"))
                    {
                        continue;
                    }
                    let Some(binding) = bindings.iter().find(|b| {
                        Some(&b.id) == task.execution_binding_id()
                            && b.enabled
                            && mission.policy.allowed_binding_ids.contains(&b.id)
                    }) else {
                        continue;
                    };
                    if Self::missing_binding_capability(binding, task.kind).is_none() {
                        let mut task = task.clone();
                        task.state = TaskState::Ready;
                        task.blocked_code = None;
                        task.updated_at = term_storage::time::now_iso8601();
                        upserts.push(Entity::Task(Box::new(task)));
                    }
                }
                if !upserts.is_empty() {
                    self.commit_actor(
                        snapshot.mission,
                        "engine.capability_reconcile",
                        upserts,
                        vec![],
                    )?;
                }
            }
            match next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        Ok(())
    }

    /// Runs on the start worker, never on the mission actor or DB writer.
    /// A mismatch fails before any provider start/prompt and never changes an
    /// immutable Run to silently accept a different CLI version.
    pub(super) fn validate_runtime_before_start(
        &self,
        start: &crate::agent_runtime::RunStart,
    ) -> Result<(), MissionRpcError> {
        if start.binding.runtime == RuntimeKind::Fake {
            return Ok(());
        }
        let mut binding = start.binding.clone();
        let current = self.current_binding_document(&binding.id)?;
        let observed = self.current_observation(current.as_ref(), &binding);
        let observed = observed.ok_or_else(|| {
            MissionRpcError::new(
                MissionErrorCode::CapabilityUnsupported,
                "CLI file identity is unverified; check installation again",
            )
        })?;
        binding.capabilities =
            self.launch_capabilities(&binding, current.as_ref(), observed.version.as_deref());
        let kind = match start.workspace_access {
            crate::agent_runtime::WorkspaceAccess::ReadOnly => TaskKind::Research,
            crate::agent_runtime::WorkspaceAccess::Write => TaskKind::Implement,
        };
        self.require_launch_capability(&binding, current.as_ref(), kind)?;
        let actual =
            (self.installation_probe)(&binding.program, binding.runtime, observed.executable.as_ref()).map_err(|_| {
                MissionRpcError::new(MissionErrorCode::CapabilityUnsupported,
                "CLI file identity or installation could not be verified before launch; check installation again")
            })?;
        if binding.runtime_version.as_deref() != Some(actual.version.as_str())
            || observed.executable != actual.executable
        {
            // A changed CLI is answered by another installation check, which
            // is also the local self-check for the new version (11 §7); no
            // consent is asked for again.
            return Err(MissionRpcError::new(
                MissionErrorCode::CapabilityUnsupported,
                format!(
                    "CLI file or version changed after installation check (checked: {}, installed: {}); check installation again",
                    binding.runtime_version.as_deref().unwrap_or("unknown"),
                    actual.version,
                ),
            ));
        }
        Ok(())
    }

    /// Dependency injection for a daemon embedder's local evidence source.
    /// The shipped daemon uses the built-in registry and bounded CLI probe;
    /// neither dependency is configurable through RPC or environment variables.
    pub fn with_binding_evidence(
        mut self,
        probe: impl Fn(&str, RuntimeKind) -> Result<String, installation::ProbeFailure>
            + Send
            + Sync
            + 'static,
        registry: impl Fn(&Binding, &str, Option<&str>) -> RuntimeCapabilities + Send + Sync + 'static,
    ) -> Self {
        self.installation_probe = Box::new(move |program, runtime, _| {
            probe(program, runtime).map(|version| installation::Installation {
                version,
                executable: None,
            })
        });
        self.allow_unpinned_installation = true;
        self.capability_registry = Box::new(registry);
        // An injected registry states the capabilities itself, so measuring
        // the fixture would only add a second, contradictory source. The
        // binding keeps `local_evidence.probe == None` — "the self-check has
        // not run for this version" (11 §7 DI). A suite that wants a fixed
        // report adds `with_local_prober`/`with_cheap_prober` after this.
        self.without_local_probe()
    }

    pub(super) fn observed_binding(&self, document: &Value) -> Result<Binding, MissionRpcError> {
        let mut binding: Binding = serde_json::from_value(document.clone()).map_err(|_| {
            MissionRpcError::new(MissionErrorCode::Internal, "invalid stored binding")
        })?;
        // Fake runs are available only to explicitly injected test adapters.
        // They never enter a production provider through this runtime kind.
        if binding.runtime == RuntimeKind::Fake {
            return Ok(binding);
        }
        let observed = observation(document, &binding, self.allow_unpinned_installation);
        self.project_observation(&mut binding, observed);
        Ok(binding)
    }

    fn project_observation(&self, binding: &mut Binding, observed: Option<Observation>) {
        binding.runtime_version = observed.as_ref().and_then(|o| o.version.clone());
        binding.checked_at = observed.map(|o| o.checked_at);
        binding.capabilities = (self.capability_registry)(
            binding,
            std::env::consts::OS,
            binding.runtime_version.as_deref(),
        );
    }

    /// Drop all client evidence; only copy a matching observation from storage.
    pub(super) fn binding_document(&self, mut binding: Binding, prior: Option<&Value>) -> Value {
        if binding.runtime == RuntimeKind::Fake {
            return serde_json::to_value(binding).expect("binding serializes");
        }
        let observed =
            prior.and_then(|value| observation(value, &binding, self.allow_unpinned_installation));
        self.project_observation(&mut binding, observed.clone());
        let mut document = serde_json::to_value(binding).expect("binding serializes");
        if let Some(observed) = observed {
            document[OBSERVATION] = serde_json::to_value(observed).expect("observation serializes");
        }
        document
    }

    pub(super) fn probe_document(
        &self,
        binding: &Binding,
        executable: Option<ExecutableIdentity>,
    ) -> Value {
        let mut document = serde_json::to_value(binding).expect("binding serializes");
        if binding.runtime != RuntimeKind::Fake {
            document[OBSERVATION] = serde_json::to_value(Observation {
                format: 2,
                target_sha256: target(binding),
                os: std::env::consts::OS.into(),
                version: binding.runtime_version.clone(),
                checked_at: binding
                    .checked_at
                    .clone()
                    .expect("probe records its timestamp"),
                executable,
                verified_at_revision: binding.revision.get().checked_add(1).map(|revision| {
                    term_contracts::ids::U64String::parse(&revision.to_string())
                        .expect("u64 revision")
                }),
            })
            .expect("observation serializes");
        }
        document
    }

    /// The capability projection of a prepared Run's binding without the
    /// requirement check (tests inspect it; launches use `launch_binding`).
    #[cfg(test)]
    pub(super) fn execution_binding(&self, binding: Binding) -> Result<Binding, MissionRpcError> {
        if binding.runtime == RuntimeKind::Fake {
            return Ok(binding);
        }
        let current = self.current_binding_document(&binding.id)?;
        Ok(self.project_launch(binding, current.as_ref()))
    }

    /// A prepared historical Run is not itself a trusted capability registry.
    /// Preserve its selected model and installation snapshot, but require the
    /// matching daemon observation before carrying support into a new process,
    /// and apply its experimental consent only while the current binding
    /// document still holds it. A missing capability whose consent the current
    /// document withdrew is `experimental_consent_withdrawn`.
    pub(super) fn launch_binding(
        &self,
        binding: Binding,
        kind: TaskKind,
    ) -> Result<Binding, MissionRpcError> {
        if binding.runtime == RuntimeKind::Fake {
            return Ok(binding);
        }
        let current = self.current_binding_document(&binding.id)?;
        let binding = self.project_launch(binding, current.as_ref());
        self.require_launch_capability(&binding, current.as_ref(), kind)?;
        Ok(binding)
    }

    fn project_launch(&self, mut binding: Binding, current: Option<&Value>) -> Binding {
        let observed = self.current_observation(current, &binding);
        binding.capabilities = self.launch_capabilities(
            &binding,
            current,
            observed.as_ref().and_then(|o| o.version.as_deref()),
        );
        binding
    }

    fn launch_capabilities(
        &self,
        binding: &Binding,
        current: Option<&Value>,
        version: Option<&str>,
    ) -> RuntimeCapabilities {
        if consent_withdrawn(binding, current) {
            let mut without_consent = binding.clone();
            without_consent.experimental_version = None;
            return (self.capability_registry)(&without_consent, std::env::consts::OS, version);
        }
        (self.capability_registry)(binding, std::env::consts::OS, version)
    }

    fn require_launch_capability(
        &self,
        binding: &Binding,
        current: Option<&Value>,
        kind: TaskKind,
    ) -> Result<(), MissionRpcError> {
        if consent_withdrawn(binding, current) {
            if let Some(missing) = Self::missing_binding_capability(binding, kind) {
                return Err(consent_withdrawn_error(missing));
            }
        }
        self.require_binding_capability(binding, kind)
    }

    pub(super) fn current_binding_document(
        &self,
        id: &Id,
    ) -> Result<Option<Value>, MissionRpcError> {
        Ok(self
            .storage
            .mission_bindings()
            .map_err(Self::store_error)?
            .into_iter()
            .find(|value| value.get("id").and_then(Value::as_str) == Some(id.as_str())))
    }

    fn current_observation(
        &self,
        current: Option<&Value>,
        binding: &Binding,
    ) -> Option<Observation> {
        current
            .and_then(|document| observation(document, binding, self.allow_unpinned_installation))
            .filter(|o| {
                o.version == binding.runtime_version
                    && binding.checked_at.as_ref() == Some(&o.checked_at)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agent_runtime::{capability_evidence, fake::fake_binding},
        mission::artifacts::ArtifactStore,
    };
    use std::sync::Arc;

    #[test]
    #[cfg(unix)]
    fn a_codex_listing_for_another_provider_never_spawns_the_cli() {
        use crate::agent_runtime::detection::DetectionEnv;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("codex");
        // A real, answering app-server, and a program path that is not empty:
        // the unrelated "no CLI found" guard cannot be what returns below. It
        // records that it ran before writing a line, so a received catalog
        // proves the marker is already on disk. `cat` only holds stdin open
        // until `close()` drops it.
        std::fs::write(
            &program,
            r#"#!/bin/sh
touch "$0.listed"
printf '{"id":1,"result":{"userAgent":"fixture"}}\n'
printf '{"id":2,"result":{"data":[{"model":"gpt-fixture","defaultReasoningEffort":"medium"}],"nextCursor":null}}\n'
cat > /dev/null
"#,
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let env = DetectionEnv::default();
        let marker = program.with_extension("listed");
        let list = |provider_id: Option<&str>| -> Vec<String> {
            list_models(&ModelListRequest {
                runtime: RuntimeKind::Codex,
                program: program.to_str().unwrap(),
                provider_id,
                env: &env,
            })
            .into_iter()
            .map(|model| model.id)
            .collect()
        };
        assert!(
            list(Some("custom")).is_empty(),
            "a binding on another provider is left unclaimed, so none of these \
             models could ever launch through it"
        );
        assert!(
            !marker.exists(),
            "the provider guard returns before the CLI is spawned"
        );
        // Same non-empty program, only the provider differs: the listing runs.
        let own = capability_evidence::evidence_provider_id(RuntimeKind::Codex);
        assert!(
            own.is_some(),
            "Codex has an evidence provider to compare to"
        );
        assert_eq!(list(own), ["gpt-fixture"]);
        assert!(marker.exists(), "the runtime's own provider is still asked");
        std::fs::remove_file(&marker).unwrap();
        // Unlike the OpenCode listing, an unnamed provider is not ambiguous
        // here: Codex serves exactly one, so an id without it still names a
        // model.
        assert_eq!(list(None), ["gpt-fixture"]);
        assert!(
            marker.exists(),
            "a caller that cannot name the provider yet still gets the catalog"
        );
    }

    #[test]
    #[cfg(unix)]
    fn an_opencode_listing_without_a_provider_never_spawns_the_cli() {
        use crate::agent_runtime::detection::DetectionEnv;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("opencode");
        // A real, answering CLI, and a program path that is not empty: the
        // unrelated "no CLI found" guard cannot be what returns below. Two
        // providers print the same model name, which is exactly the collision
        // an unfiltered listing would hand the picker as one id.
        std::fs::write(
            &program,
            "#!/bin/sh\ntouch \"$0.listed\"\n\
             printf 'openai/gpt-6-astra\\nopencode/gpt-6-astra\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let env = DetectionEnv::default();
        assert!(
            list_models(&ModelListRequest {
                runtime: RuntimeKind::Opencode,
                program: program.to_str().unwrap(),
                provider_id: None,
                env: &env,
            })
            .is_empty(),
            "a model id stripped of its provider is not a selectable candidate"
        );
        assert!(
            !program.with_extension("listed").exists(),
            "the provider guard returns before the CLI is spawned"
        );
        // Same non-empty program, only the provider differs: the listing runs.
        let listed: Vec<String> = list_models(&ModelListRequest {
            runtime: RuntimeKind::Opencode,
            program: program.to_str().unwrap(),
            provider_id: Some("opencode"),
            env: &env,
        })
        .into_iter()
        .map(|model| model.id)
        .collect();
        assert_eq!(listed, ["gpt-6-astra"]);
        assert!(program.with_extension("listed").exists());
    }

    #[test]
    #[cfg(unix)]
    fn persisted_entrypoint_identity_rejects_same_version_changes_after_reopening_storage() {
        use crate::agent_runtime::{RunStart, WorkspaceAccess};
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("cli");
        std::fs::write(&program, "#!/bin/sh\nprintf 'codex-cli 0.154.0\\n'\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let make_service = || {
            let storage =
                Arc::new(term_storage::Storage::open(dir.path().join("state.db")).unwrap());
            let mut service = MissionService::new(
                storage.clone(),
                ArtifactStore::new(storage, dir.path().join("artifacts")),
            );
            // Only capability coverage is synthetic. File reads, version child,
            // private observation validation and SQLite are production paths.
            service.capability_registry = Box::new(|_, _, version| {
                if version == Some("0.154.0") {
                    fake_binding().capabilities
                } else {
                    capability_evidence::unclaimed()
                }
            });
            service
        };
        let service = make_service();
        let mut binding = fake_binding();
        binding.runtime = RuntimeKind::Codex;
        binding.program = program.to_str().unwrap().into();
        binding.runtime_version = None;
        binding.checked_at = None;
        let connection = term_contracts::ids::ConnectionId::generate();
        service.handle(&connection, "binding.save", &serde_json::json!({"request_id":Id::generate(),"expected_revision":"0","binding":binding})).unwrap();
        let response = service
            .handle(
                &connection,
                "binding.probe",
                &serde_json::json!({"binding_id":binding.id}),
            )
            .unwrap()
            .result;
        assert_eq!(response["installation"], "verified");
        assert!(response["binding"].get(OBSERVATION).is_none());
        let binding: Binding = serde_json::from_value(response["binding"].clone()).unwrap();
        let saved = service.storage.mission_bindings().unwrap()[0].clone();
        assert_eq!(saved[OBSERVATION]["format"], 2);
        assert_eq!(
            saved[OBSERVATION]["executable"]["canonical_path"],
            program.canonicalize().unwrap().to_str().unwrap()
        );
        for malformed in [
            Value::Null,
            serde_json::json!({"canonical_path":"/cli","bytes":1,"sha256":"invalid"}),
        ] {
            let mut document = saved.clone();
            document[OBSERVATION]["executable"] = malformed;
            assert!(service
                .observed_binding(&document)
                .unwrap()
                .runtime_version
                .is_none());
        }
        let mut legacy = saved.clone();
        legacy[OBSERVATION]["format"] = serde_json::json!(1);
        legacy[OBSERVATION]
            .as_object_mut()
            .unwrap()
            .remove("executable");
        assert!(service
            .observed_binding(&legacy)
            .unwrap()
            .checked_at
            .is_none());
        let start = RunStart {
            task_kind: Some(TaskKind::Research),
            mission_id: Id::generate(),
            owner_daemon_id: Id::generate(),
            workspace_access: WorkspaceAccess::ReadOnly,
            allow_network: false,
            run_id: Id::generate(),
            fencing_token: 1,
            binding: binding.clone(),
            context_path: dir.path().join("context"),
            workspace: Some(dir.path().into()),
            prompt_stdin: "test".into(),
        };
        service.validate_runtime_before_start(&start).unwrap();
        drop(service);
        let reopened = make_service();
        reopened.validate_runtime_before_start(&start).unwrap();
        std::fs::write(
            &program,
            "#!/bin/sh\ntouch \"$0.executed\"; printf 'codex-cli 0.154.0\\n'\n",
        )
        .unwrap();
        assert_eq!(
            reopened
                .validate_runtime_before_start(&start)
                .unwrap_err()
                .code,
            MissionErrorCode::CapabilityUnsupported
        );
        assert!(
            !program.with_extension("executed").exists(),
            "replacement must not even execute --version"
        );
        assert_eq!(
            reopened.storage.mission_bindings().unwrap()[0],
            saved,
            "preflight cannot overwrite the prior observation"
        );
        assert_eq!(
            start.binding, binding,
            "historical launch binding is preserved"
        );
        let fresh = reopened
            .handle(
                &connection,
                "binding.probe",
                &serde_json::json!({"binding_id":binding.id}),
            )
            .unwrap()
            .result;
        assert_eq!(fresh["installation"], "verified");
        assert!(
            reopened.validate_runtime_before_start(&start).is_err(),
            "an old Run cannot inherit a new file observation"
        );
        // Even if wall-clock timestamps collide, a later probe cannot supply
        // a different file identity to an older Run's binding revision.
        let fresh_binding: Binding = serde_json::from_value(fresh["binding"].clone()).unwrap();
        let mut same_time = reopened.storage.mission_bindings().unwrap()[0].clone();
        same_time["checked_at"] = serde_json::json!(binding.checked_at);
        same_time[OBSERVATION]["checked_at"] = serde_json::json!(binding.checked_at);
        let same_time = reopened
            .storage
            .save_mission_binding(
                Id::generate(),
                "same-time-fixture",
                &"a".repeat(64),
                fresh_binding.revision.get(),
                same_time,
                binding.checked_at.clone().unwrap(),
            )
            .unwrap();
        assert!(reopened.validate_runtime_before_start(&start).is_err());
        let mut new_start = start;
        new_start.binding = reopened.observed_binding(&same_time.document).unwrap();
        reopened.validate_runtime_before_start(&new_start).unwrap();
    }

    #[test]
    fn prepared_runs_require_the_same_server_observation_and_current_registry() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(term_storage::Storage::open(dir.path().join("state.db")).unwrap());
        let service = MissionService::new(
            storage.clone(),
            ArtifactStore::new(storage.clone(), dir.path().join("artifacts")),
        )
        .with_binding_evidence(
            |_, _| Ok("fixture-v1".into()),
            |_, _, version| {
                if version == Some("fixture-v1") {
                    fake_binding().capabilities
                } else {
                    capability_evidence::unclaimed()
                }
            },
        );
        let mut binding = fake_binding();
        binding.runtime = RuntimeKind::Codex;
        binding.runtime_version = Some("fixture-v1".into());
        binding.checked_at = Some("2026-09-16T00:00:00Z".into());
        binding.revision = term_contracts::ids::U64String::parse("0").unwrap();
        // A Run with claimed support alone is not authoritative.
        assert!(
            !service
                .execution_binding(binding.clone())
                .unwrap()
                .capabilities
                .events
                .supported
        );
        let saved = storage
            .save_mission_binding(
                Id::generate(),
                "binding.probe",
                &"c".repeat(64),
                0,
                service.probe_document(&binding, None),
                "2026-09-16T00:00:00Z".into(),
            )
            .unwrap();
        let binding = service.observed_binding(&saved.document).unwrap();
        assert!(
            service
                .execution_binding(binding.clone())
                .unwrap()
                .capabilities
                .events
                .supported
        );
        for key in ["runtime_version", "checked_at", "model_id", "program"] {
            let mut changed = serde_json::to_value(&binding).unwrap();
            changed[key] = serde_json::json!("changed");
            let changed: Binding = serde_json::from_value(changed).unwrap();
            assert!(
                !service
                    .execution_binding(changed)
                    .unwrap()
                    .capabilities
                    .events
                    .supported,
                "{key}"
            );
        }
        // Server registry revocation takes effect without rewriting history.
        let revoked = MissionService::new(
            storage.clone(),
            ArtifactStore::new(storage.clone(), dir.path().join("artifacts")),
        )
        .with_binding_evidence(
            |_, _| Ok("fixture-v1".into()),
            |_, _, _| capability_evidence::unclaimed(),
        );
        assert!(
            !revoked
                .observed_binding(&saved.document)
                .unwrap()
                .capabilities
                .events
                .supported
        );
        assert!(
            !revoked
                .execution_binding(binding)
                .unwrap()
                .capabilities
                .events
                .supported
        );
        assert_eq!(storage.mission_bindings().unwrap()[0], saved.document);
    }

    /// 11 §3.4: consent is per connection, so the registry no longer compares
    /// it with the observed version — only a withdrawal (or a consent that
    /// belongs to a different statement) closes a Run's prepared support.
    #[test]
    fn prepared_run_consent_applies_only_while_the_current_binding_still_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(term_storage::Storage::open(dir.path().join("state.db")).unwrap());
        let service = MissionService::new(
            storage.clone(),
            ArtifactStore::new(storage.clone(), dir.path().join("artifacts")),
        )
        .with_binding_evidence(
            |_, _| Ok("fixture-v1".into()),
            // No evidence at all: support exists only through this
            // connection's consent, whatever version is installed.
            |binding, _, version| {
                if version.is_some() && binding.experimental_version.is_some() {
                    fake_binding().capabilities
                } else {
                    capability_evidence::unclaimed()
                }
            },
        );
        let mut binding = fake_binding();
        binding.runtime = RuntimeKind::Codex;
        binding.runtime_version = Some("fixture-v1".into());
        binding.checked_at = Some("2026-09-16T00:00:00Z".into());
        binding.revision = term_contracts::ids::U64String::parse("0").unwrap();
        binding.experimental_version = Some("fixture-v1".into());
        let saved = storage
            .save_mission_binding(
                Id::generate(),
                "binding.probe",
                &"d".repeat(64),
                0,
                service.probe_document(&binding, None),
                "2026-09-16T00:00:00Z".into(),
            )
            .unwrap();
        // The snapshot a Run was prepared with while the consent was current.
        let frozen = service.observed_binding(&saved.document).unwrap();
        service
            .launch_binding(frozen.clone(), TaskKind::Implement)
            .unwrap();
        let mut revision = saved.revision;
        for (consent, allowed) in [
            // Withdrawn: the prepared Run loses the support consent gave it.
            (None, false),
            // A consent restated for another observed version (a CLI update
            // accepted again) is still consent for this connection (11 §3.4).
            (Some("fixture-v2"), true),
            (Some("fixture-v1"), true),
        ] {
            let mut document = storage.mission_bindings().unwrap()[0].clone();
            document["experimental_version"] = serde_json::json!(consent);
            revision = storage
                .save_mission_binding(
                    Id::generate(),
                    "binding.save",
                    &"e".repeat(64),
                    revision,
                    document,
                    "2026-09-16T00:00:01Z".into(),
                )
                .unwrap()
                .revision;
            let launch = service.launch_binding(frozen.clone(), TaskKind::Implement);
            if allowed {
                launch.unwrap();
            } else {
                let error = launch.unwrap_err();
                assert_eq!(error.code, MissionErrorCode::CapabilityUnsupported);
                assert_eq!(
                    error.details.reason_code.as_deref(),
                    Some("experimental_consent_withdrawn"),
                    "{consent:?}"
                );
                assert!(
                    !error.message.contains("CLI version"),
                    "a CLI update is not what the user has to answer here"
                );
            }
        }
    }

    #[test]
    fn a_consent_carried_onto_another_launch_target_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(term_storage::Storage::open(dir.path().join("state.db")).unwrap());
        let service = MissionService::new(
            storage.clone(),
            ArtifactStore::new(storage, dir.path().join("artifacts")),
        );
        let connection = term_contracts::ids::ConnectionId::generate();
        let save = |binding: &Binding, expected: &term_contracts::ids::U64String| -> Binding {
            let result = service
                .handle(
                    &connection,
                    "binding.save",
                    &serde_json::json!({"request_id":Id::generate(),"expected_revision":expected,"binding":binding}),
                )
                .unwrap()
                .result;
            serde_json::from_value(result["binding"].clone()).unwrap()
        };
        let mut binding = fake_binding();
        binding.experimental_version = Some("1.0.0".into());
        let created = save(&binding, &term_contracts::ids::U64String::new(0).unwrap());
        assert_eq!(created.experimental_version.as_deref(), Some("1.0.0"));
        // Same target: the consent stays.
        let mut relabeled = created.clone();
        relabeled.label = "Renamed".into();
        let relabeled = save(&relabeled, &created.revision);
        assert_eq!(relabeled.experimental_version.as_deref(), Some("1.0.0"));
        // Another executable with the stored consent carried along: cleared.
        let mut moved = relabeled.clone();
        moved.program = "other-fixture".into();
        let moved = save(&moved, &relabeled.revision);
        assert_eq!(moved.experimental_version, None);
        // Another executable with a newly stated version: kept.
        let mut restated = moved.clone();
        restated.program = "third-fixture".into();
        restated.experimental_version = Some("1.1.0".into());
        let restated = save(&restated, &moved.revision);
        assert_eq!(restated.experimental_version.as_deref(), Some("1.1.0"));
        // Another auth route with that consent carried along: cleared.
        let mut rerouted = restated.clone();
        rerouted.auth_route = AuthRoute::Subscription;
        let rerouted = save(&rerouted, &restated.revision);
        assert_eq!(rerouted.experimental_version, None);
        let stored = &service.storage.mission_bindings().unwrap()[0];
        assert!(
            stored.get("experimental_version").is_none(),
            "a null consent is omitted from the stored document"
        );
    }

    fn report(protocol_ok: bool) -> LocalProbeReport {
        LocalProbeReport {
            protocol_ok,
            sandbox_cases_passed: Some(12),
            sandbox_cases_total: Some(12),
            model_listed: Some(true),
            failures: vec![],
        }
    }

    fn measured(version: &str, model_id: &str, read_only_runs: u32) -> LocalEvidence {
        LocalEvidence {
            os: std::env::consts::OS.into(),
            version: version.into(),
            model_id: model_id.into(),
            probed_at: Some("2026-09-19T00:00:00Z".into()),
            probe: Some(report(true)),
            runs: LocalRunEvidence {
                succeeded_read_only: read_only_runs,
                last_at: Some("2026-09-19T00:00:01Z".into()),
                ..Default::default()
            },
        }
    }

    /// 11 §7: one `binding.probe` measurement replaces the previous one, and
    /// the Run counters beside it survive only while the installation they
    /// were collected on is still the one being measured.
    #[test]
    fn a_probe_keeps_run_counters_only_for_the_same_os_version_and_model() {
        let os = std::env::consts::OS;
        let prior = measured("0.154.0", "gpt-fixture", 4);
        let same = merged_local_probe(
            Some(prior.clone()),
            os,
            "0.154.0",
            "gpt-fixture",
            report(false),
            "2026-09-19T01:00:00Z".into(),
        );
        assert_eq!(
            same.probe,
            Some(report(false)),
            "the newer measurement wins"
        );
        assert_eq!(same.probed_at.as_deref(), Some("2026-09-19T01:00:00Z"));
        assert_eq!(same.runs, prior.runs, "observed Runs are not re-measured");

        for (version, model_id) in [("0.154.0", "gpt-other"), ("0.155.0", "gpt-fixture")] {
            let reset = merged_local_probe(
                Some(prior.clone()),
                os,
                version,
                model_id,
                report(true),
                "2026-09-19T01:00:00Z".into(),
            );
            assert_eq!(reset.version, version);
            assert_eq!(reset.model_id, model_id);
            assert_eq!(
                reset.runs,
                LocalRunEvidence::default(),
                "{version}/{model_id} has no observed Runs of its own"
            );
        }
        let foreign = merged_local_probe(
            Some(LocalEvidence {
                os: "another-os".into(),
                ..prior.clone()
            }),
            os,
            "0.154.0",
            "gpt-fixture",
            report(true),
            "2026-09-19T01:00:00Z".into(),
        );
        assert_eq!(foreign.runs, LocalRunEvidence::default());
    }

    /// 11 §2.4/§7: `binding.save` never accepts a measurement from a client,
    /// and carries the stored one only onto the same launch target.
    #[test]
    fn a_save_drops_client_local_evidence_and_carries_the_stored_one_per_target() {
        let mut stored = fake_binding();
        stored.runtime = RuntimeKind::Codex;
        stored.program = "/usr/local/bin/codex".into();
        stored.provider_id = "openai".into();
        stored.model_id = "gpt-fixture".into();
        stored.local_evidence = Some(measured("0.154.0", "gpt-fixture", 4));
        let prior = serde_json::to_value(&stored).unwrap();

        // Whatever the client sent is replaced by what the daemon measured.
        let mut incoming = stored.clone();
        incoming.label = "Renamed".into();
        incoming.local_evidence = Some(measured("99.0.0", "forged", 9999));
        carry_local_evidence(Some(&prior), &mut incoming);
        assert_eq!(incoming.local_evidence, stored.local_evidence);

        // Only the model changed: the CLI was not re-measured, so its report
        // still stands, but the counters belonged to the other model.
        let mut remodelled = stored.clone();
        remodelled.model_id = "gpt-other".into();
        remodelled.local_evidence = None;
        carry_local_evidence(Some(&prior), &mut remodelled);
        let carried = remodelled.local_evidence.expect("the report is carried");
        assert_eq!(carried.probe, Some(report(true)));
        assert_eq!(carried.model_id, "gpt-other");
        assert_eq!(carried.runs, LocalRunEvidence::default());

        // Every other launch-target field makes it another installation.
        let targets: [fn(&mut Binding); 6] = [
            |b| b.runtime = RuntimeKind::Claude,
            |b| b.program = "/opt/codex".into(),
            |b| b.provider_id = "other".into(),
            |b| b.auth_route = AuthRoute::ApiKey,
            |b| b.credential_ref = Some("keyring:other".into()),
            |b| b.endpoint_ref = Some(Id::generate()),
        ];
        for change in targets {
            let mut moved = stored.clone();
            change(&mut moved);
            carry_local_evidence(Some(&prior), &mut moved);
            assert!(moved.local_evidence.is_none());
        }

        // A first save has nothing to carry, whatever the client claims.
        let mut fresh = stored.clone();
        carry_local_evidence(None, &mut fresh);
        assert!(fresh.local_evidence.is_none());
    }
}

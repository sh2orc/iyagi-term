//! Pure validation helpers for O1 wire inputs (02 ticket: UUID, UTF-8 byte
//! budgets, path shapes, enum/variant pairing, null policy). Everything here
//! is side-effect free; the daemon calls these before any storage mutation.

use serde::{Deserialize, Serialize};

use super::error::{MissionErrorCode, MissionRpcError};
use super::types::{
    ArtifactRef, ExpectedOutput, Policy, ProviderResult, Requirement, Role, Task, TaskKind,
};
use crate::ids::U64String;

/// O1 default limits — typed mirror of `docs/orchestration/defaults.json`
/// (loaded at test time from the same file, keeping a single source of
/// truth). Values are targets, not measurements.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionLimits {
    pub max_title_bytes: usize,
    pub max_requirement_text_bytes: usize,
    pub max_requirements: usize,
    pub max_tasks_per_mission: usize,
    pub max_plan_revisions: u32,
    pub max_delegation_depth: u32,
    pub max_specialist_requests_per_task: u32,
    pub max_artifact_bytes: u64,
    pub max_artifact_bytes_per_mission: u64,
    pub max_context_bytes: usize,
    /// Per-file bound for pinned `.iyagi/roles/<role>.md` text (10 §2).
    pub max_role_instruction_bytes: usize,
    pub max_message_bytes: usize,
    pub max_provider_session_id_bytes: usize,
    pub artifact_chunk_bytes: usize,
    pub max_missions_active: usize,
}

impl MissionLimits {
    /// Load from `docs/orchestration/defaults.json`; falls back to the inline
    /// copy (kept in sync by the parity test against the asset).
    pub fn load() -> MissionLimits {
        let asset = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../docs/orchestration/defaults.json");
        std::fs::read_to_string(&asset)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_else(Self::inline)
    }

    fn inline() -> MissionLimits {
        MissionLimits {
            max_title_bytes: 256,
            max_requirement_text_bytes: 2048,
            max_requirements: 32,
            max_tasks_per_mission: 256,
            max_plan_revisions: 20,
            max_delegation_depth: 3,
            max_specialist_requests_per_task: 3,
            max_artifact_bytes: 67_108_864,
            max_artifact_bytes_per_mission: 536_870_912,
            max_context_bytes: 262_144,
            max_role_instruction_bytes: 32_768,
            max_message_bytes: 262_144,
            max_provider_session_id_bytes: 512,
            artifact_chunk_bytes: 4096,
            max_missions_active: 8,
        }
    }
}

/// Policy ceilings a user may raise values up to (defaults.json
/// `policy_ceiling`); global caps are never policy-expandable.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PolicyCeiling {
    pub max_parallel_runs: u32,
    pub max_attempts_per_task: u32,
    pub max_repair_cycles: u32,
    pub max_automatic_starts: u32,
    pub active_time_limit_ms: u64,
    pub run_time_limit_ms: u64,
}

impl PolicyCeiling {
    pub fn load() -> PolicyCeiling {
        let asset = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../docs/orchestration/defaults.json");
        if let Some(v) = std::fs::read_to_string(&asset)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|v| v.get("policy_ceiling").cloned())
            .and_then(|v| serde_json::from_value(v).ok())
        {
            return v;
        }
        PolicyCeiling {
            max_parallel_runs: 8,
            max_attempts_per_task: 12,
            max_repair_cycles: 8,
            max_automatic_starts: 512,
            active_time_limit_ms: 86_400_000,
            run_time_limit_ms: 7_200_000,
        }
    }
}

pub type ValidationResult<T> = Result<T, MissionRpcError>;

fn invalid(message: impl Into<String>) -> MissionRpcError {
    MissionRpcError::new(MissionErrorCode::InvalidArgument, message)
}

// ---- primitive validators ---------------------------------------------------

pub fn validate_sha256(value: &str) -> ValidationResult<()> {
    let ok = value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if ok {
        Ok(())
    } else {
        Err(invalid("sha256 must be 64 lowercase hex characters"))
    }
}

pub fn validate_git_oid(value: &str) -> ValidationResult<()> {
    let hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    let ok = (value.len() == 40 || value.len() == 64) && value.bytes().all(hex);
    if ok {
        Ok(())
    } else {
        Err(invalid(
            "git oid must be 40 or 64 lowercase hex characters (repository object format)",
        ))
    }
}

/// Display strings that survive logs and titles: valid UTF-8 (guaranteed by
/// String), no C0/C1 control characters except tab/newline/CR.
pub fn has_no_control_chars(value: &str) -> bool {
    value
        .chars()
        .all(|c| c == '\t' || c == '\n' || c == '\r' || !c.is_control())
}

pub fn validate_title(title: &str, limits: &MissionLimits) -> ValidationResult<()> {
    if title.is_empty() {
        return Err(invalid("title must not be empty"));
    }
    if title.len() > limits.max_title_bytes {
        return Err(MissionRpcError::new(
            MissionErrorCode::InvalidArgument,
            format!(
                "title exceeds {} UTF-8 bytes ({} bytes); truncate before sending",
                limits.max_title_bytes,
                title.len()
            ),
        ));
    }
    if !has_no_control_chars(title) {
        return Err(invalid("title must not contain control characters"));
    }
    Ok(())
}

/// Repository-relative writable path: no absolute prefix, no `..`, no NUL,
/// no `.git` access. A trailing `/` marks a directory prefix.
pub fn validate_allowed_path(path: &str) -> ValidationResult<()> {
    if path.is_empty() {
        return Err(invalid("allowed path must not be empty"));
    }
    if path.contains('\0') {
        return Err(invalid("allowed path must not contain NUL"));
    }
    if path.starts_with('/') || path.starts_with('\\') || path.contains(':') {
        return Err(invalid(format!(
            "allowed path {path:?} must be repository-relative"
        )));
    }
    for component in path.trim_end_matches('/').split('/') {
        if component.is_empty() {
            return Err(invalid(format!("allowed path {path:?} has empty segment")));
        }
        if component == "." || component == ".." {
            return Err(invalid(format!(
                "allowed path {path:?} must not traverse with {component:?}"
            )));
        }
    }
    let head = path.split('/').next().unwrap_or_default();
    if head == ".git" {
        return Err(invalid("allowed path must not touch .git"));
    }
    Ok(())
}

pub fn validate_provider_session_id(value: &str, limits: &MissionLimits) -> ValidationResult<()> {
    if value.len() > limits.max_provider_session_id_bytes {
        return Err(invalid(format!(
            "provider session id exceeds {} bytes",
            limits.max_provider_session_id_bytes
        )));
    }
    if !has_no_control_chars(value) {
        return Err(invalid(
            "provider session id must not contain control characters",
        ));
    }
    Ok(())
}

/// Provider-local task key: ASCII `[a-z][a-z0-9_-]{0,63}`.
pub fn validate_local_key(key: &str) -> ValidationResult<()> {
    let bytes = key.as_bytes();
    let shape = !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-');
    if shape {
        Ok(())
    } else {
        Err(invalid(format!(
            "local_key {key:?} must match [a-z][a-z0-9_-]{{0,63}}"
        )))
    }
}

// ---- composite validators ---------------------------------------------------

pub fn validate_artifact_ref(reference: &ArtifactRef) -> ValidationResult<()> {
    validate_sha256(&reference.sha256)?;
    if reference.media_type.is_empty() || reference.media_type.len() > 256 {
        return Err(invalid("media_type must be 1..=256 characters"));
    }
    if !has_no_control_chars(&reference.media_type) {
        return Err(invalid("media_type must not contain control characters"));
    }
    Ok(())
}

pub fn validate_requirements(
    requirements: &[Requirement],
    limits: &MissionLimits,
) -> ValidationResult<()> {
    if requirements.is_empty() {
        return Err(invalid("mission requires at least one requirement"));
    }
    if requirements.len() > limits.max_requirements {
        return Err(invalid(format!(
            "requirements exceed {} entries",
            limits.max_requirements
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for requirement in requirements {
        if !seen.insert(requirement.id.as_str()) {
            return Err(invalid("requirement ids must be unique"));
        }
        if requirement.text.is_empty() {
            return Err(invalid("requirement text must not be empty"));
        }
        if requirement.text.len() > limits.max_requirement_text_bytes {
            return Err(invalid(format!(
                "requirement {} text exceeds {} UTF-8 bytes",
                requirement.id, limits.max_requirement_text_bytes
            )));
        }
    }
    Ok(())
}

/// 09 §4: policy ints have floor 1 (repair cycles may be 0), ceilings come
/// from defaults, duplicate allowlist entries are rejected outright, and
/// cost caps are null or strictly positive.
pub fn validate_policy(policy: &Policy, ceiling: &PolicyCeiling) -> ValidationResult<()> {
    if policy.max_parallel_runs < 1 || policy.max_parallel_runs > ceiling.max_parallel_runs {
        return Err(invalid(format!(
            "max_parallel_runs must be 1..={}",
            ceiling.max_parallel_runs
        )));
    }
    if policy.max_attempts_per_task < 1
        || policy.max_attempts_per_task > ceiling.max_attempts_per_task
    {
        return Err(invalid(format!(
            "max_attempts_per_task must be 1..={}",
            ceiling.max_attempts_per_task
        )));
    }
    if policy.max_repair_cycles > ceiling.max_repair_cycles {
        return Err(invalid(format!(
            "max_repair_cycles must be 0..={}",
            ceiling.max_repair_cycles
        )));
    }
    if policy.max_automatic_starts < 1 || policy.max_automatic_starts > ceiling.max_automatic_starts
    {
        return Err(invalid(format!(
            "max_automatic_starts must be 1..={}",
            ceiling.max_automatic_starts
        )));
    }
    if policy.active_time_limit_ms.get() < 1
        || policy.active_time_limit_ms.get() > ceiling.active_time_limit_ms
    {
        return Err(invalid("active_time_limit_ms out of policy ceiling"));
    }
    if policy.run_time_limit_ms.get() < 1
        || policy.run_time_limit_ms.get() > ceiling.run_time_limit_ms
    {
        return Err(invalid("run_time_limit_ms out of policy ceiling"));
    }
    if let Some(cost) = &policy.max_cost_usd_micros {
        if cost.get() == 0 {
            return Err(invalid("max_cost_usd_micros must be positive or null"));
        }
    }
    let mut bindings = std::collections::HashSet::new();
    if policy
        .allowed_binding_ids
        .iter()
        .any(|id| !bindings.insert(id.as_str()))
    {
        return Err(invalid("allowed_binding_ids must not contain duplicates"));
    }
    let mut roles = std::collections::HashSet::new();
    if policy.allowed_roles.iter().any(|role| !roles.insert(*role)) {
        return Err(invalid("allowed_roles must not contain duplicates"));
    }
    let mut verifications = std::collections::HashSet::new();
    if policy
        .allowed_verification_ids
        .iter()
        .any(|id| !verifications.insert(id.as_str()))
    {
        return Err(invalid(
            "allowed_verification_ids must not contain duplicates",
        ));
    }
    Ok(())
}

/// Role-binding allowlist closure: primary and fallbacks must all be inside
/// the policy allowlist; fallback order is preserved as given.
pub fn validate_role_bindings(
    role_bindings: &[super::types::RoleBinding],
    policy: &Policy,
) -> ValidationResult<()> {
    let allowed = |id: &super::types::Id| policy.allowed_binding_ids.contains(id);
    for binding in role_bindings {
        if !policy.allowed_roles.contains(&binding.role) {
            return Err(invalid(format!(
                "role {:?} is not in allowed_roles",
                binding.role
            )));
        }
        if !allowed(&binding.primary_binding_id) {
            return Err(invalid(format!(
                "primary binding {} of role {:?} is not in allowed_binding_ids",
                binding.primary_binding_id, binding.role
            )));
        }
        let mut seen = std::collections::HashSet::new();
        for fallback in &binding.fallback_binding_ids {
            if !seen.insert(fallback.as_str()) {
                return Err(invalid("fallback_binding_ids must not contain duplicates"));
            }
            if !allowed(fallback) {
                return Err(invalid(format!(
                    "fallback binding {fallback} is not in allowed_binding_ids"
                )));
            }
        }
    }
    Ok(())
}

/// Result-variant/task-kind pairing (02 §5): plan→plan, review→review,
/// writer kinds→patch, research/design/consult/diagnose→report;
/// question/blocked are allowed for every AI task; verify tasks take no
/// model result at all.
pub fn result_variant_matches_task(kind: TaskKind, result: &ProviderResult) -> bool {
    match result {
        ProviderResult::Plan { .. } => kind == TaskKind::Plan,
        ProviderResult::Review { .. } => kind == TaskKind::Review,
        ProviderResult::Patch { .. } => {
            matches!(
                kind,
                TaskKind::Implement | TaskKind::TestAuthor | TaskKind::Document
            )
        }
        ProviderResult::Report { .. } => matches!(
            kind,
            TaskKind::Research | TaskKind::Design | TaskKind::Consult | TaskKind::Diagnose
        ),
        ProviderResult::Question { .. } | ProviderResult::Blocked { .. } => true,
    }
}

/// Default role for a task kind per the 00 role table (used to check
/// provider-proposed role/kind pairs).
pub fn default_role_for_kind(kind: TaskKind) -> Option<Role> {
    match kind {
        TaskKind::Plan => Some(Role::Lead),
        TaskKind::Research => Some(Role::Researcher),
        TaskKind::Design => Some(Role::Architect),
        TaskKind::Implement => Some(Role::Builder),
        TaskKind::TestAuthor => Some(Role::TestAuthor),
        TaskKind::Review => Some(Role::Reviewer),
        TaskKind::Consult => Some(Role::Specialist),
        TaskKind::Diagnose => Some(Role::Diagnostician),
        TaskKind::Integrate => Some(Role::Integrator),
        TaskKind::Document => Some(Role::Documenter),
        TaskKind::Verify => None,
    }
}

/// Task-kind/role pairing rule (02 §5): verify has role=null/binding=null,
/// every other kind maps to its 00-table role.
pub fn role_matches_kind(kind: TaskKind, role: Option<Role>) -> bool {
    match kind {
        TaskKind::Verify => role.is_none(),
        _ => role == default_role_for_kind(kind),
    }
}

/// Expected-output pairing: writer kinds promise patches; plan/review
/// promise their own outputs (02 §5 mapping used by plan validation).
pub fn expected_outputs_for_kind(kind: TaskKind) -> &'static [ExpectedOutput] {
    match kind {
        TaskKind::Implement | TaskKind::TestAuthor | TaskKind::Document => &[ExpectedOutput::Patch],
        TaskKind::Plan => &[ExpectedOutput::Report],
        TaskKind::Review => &[ExpectedOutput::Review],
        TaskKind::Integrate => &[ExpectedOutput::Patch],
        _ => &[ExpectedOutput::Report],
    }
}

/// Validate a stored task's contract shape (paths, outputs).
pub fn validate_task_contract(contract: &super::types::TaskContract) -> ValidationResult<()> {
    validate_artifact_ref(&contract.objective_ref)?;
    for path in &contract.allowed_paths {
        validate_allowed_path(path)?;
    }
    if contract.expected_outputs.is_empty() {
        return Err(invalid("task contract needs at least one expected output"));
    }
    Ok(())
}

/// Mission-scope check helper: every id must belong to the mission's known
/// set (cross-mission references are E24).
pub fn ensure_all_in_scope<'a, I: IntoIterator<Item = &'a super::types::Id>>(
    ids: I,
    mission_tasks: &'a [Task],
) -> ValidationResult<()> {
    let known: std::collections::HashSet<&str> =
        mission_tasks.iter().map(|t| t.id.as_str()).collect();
    for id in ids {
        if !known.contains(id.as_str()) {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidArgument,
                format!("task {id} references an entity outside this mission"),
            ));
        }
    }
    Ok(())
}

/// Null policy: usage/cost nulls stay null — helper for builders.
pub fn unknown_usage() -> super::types::Usage {
    super::types::Usage {
        input_tokens: None,
        output_tokens: None,
        cost_usd_micros: None,
        cost_source: super::types::UsageCostSource::Unknown,
    }
}

/// U64 helper for validators that need to compare wire numbers.
pub fn u64_of(value: &U64String) -> u64 {
    value.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha_and_oid_shapes() {
        assert!(validate_sha256(&"a".repeat(64)).is_ok());
        assert!(validate_sha256(&"A".repeat(64)).is_err());
        assert!(validate_sha256(&"a".repeat(63)).is_err());
        assert!(validate_git_oid(&"f".repeat(40)).is_ok());
        assert!(validate_git_oid(&"f".repeat(64)).is_ok());
        assert!(validate_git_oid(&"f".repeat(41)).is_err());
    }

    #[test]
    fn title_budget_is_utf8_bytes() {
        let limits = MissionLimits::load();
        assert!(validate_title("로그인 기능", &limits).is_ok());
        let long = "한".repeat(limits.max_title_bytes); // 3 bytes per char
        assert!(validate_title(&long, &limits).is_err());
    }

    #[test]
    fn allowed_paths_reject_traversal_and_git() {
        assert!(validate_allowed_path("src/api/").is_ok());
        assert!(validate_allowed_path("README.md").is_ok());
        assert!(validate_allowed_path("../outside").is_err());
        assert!(validate_allowed_path("/abs").is_err());
        assert!(validate_allowed_path(".git/config").is_err());
        assert!(validate_allowed_path("src/\0x").is_err());
        assert!(validate_allowed_path("C:\\x").is_err());
    }

    #[test]
    fn local_key_shape() {
        assert!(validate_local_key("api").is_ok());
        assert!(validate_local_key("a-b_2").is_ok());
        assert!(validate_local_key("Api").is_err());
        assert!(validate_local_key("2api").is_err());
        assert!(validate_local_key(&"a".repeat(65)).is_err());
        assert!(validate_local_key("").is_err());
    }

    #[test]
    fn policy_validation_rejects_duplicates_and_zero_costs() {
        let ceiling = PolicyCeiling::load();
        let mut policy = crate::mission::types::Policy {
            max_parallel_runs: 4,
            max_attempts_per_task: 3,
            max_repair_cycles: 3,
            max_automatic_starts: 64,
            active_time_limit_ms: U64String::new(14_400_000).unwrap(),
            run_time_limit_ms: U64String::new(2_700_000).unwrap(),
            max_cost_usd_micros: None,
            unknown_cost: crate::mission::types::UnknownCostPolicy::AllowWithNotice,
            allow_network: false,
            allow_automatic_plan_apply: true,
            allow_recovery_of_unsent: true,
            allowed_binding_ids: Vec::new(),
            allowed_roles: Vec::new(),
            allowed_verification_ids: Vec::new(),
            require_independent_review: true,
            require_enforced_verification: false,
        };
        assert!(validate_policy(&policy, &ceiling).is_ok());
        let a = crate::mission::types::Id::generate();
        policy.allowed_binding_ids = vec![a.clone(), a];
        assert!(validate_policy(&policy, &ceiling).is_err());
        policy.allowed_binding_ids = Vec::new();
        policy.max_cost_usd_micros = Some(U64String::new(0).unwrap());
        assert!(validate_policy(&policy, &ceiling).is_err());
        policy.max_cost_usd_micros = None;
        policy.max_parallel_runs = ceiling.max_parallel_runs + 1;
        assert!(validate_policy(&policy, &ceiling).is_err());
        policy.max_parallel_runs = 4;
        policy.max_repair_cycles = 0; // allowed: disables auto-repair loops
        assert!(validate_policy(&policy, &ceiling).is_ok());
    }

    #[test]
    fn result_variant_pairing() {
        use crate::mission::types::ProviderResult as R;
        let question = R::Question {
            question_text: "q".into(),
            options: Vec::new(),
        };
        assert!(result_variant_matches_task(TaskKind::Implement, &question));
        assert!(!result_variant_matches_task(
            TaskKind::Implement,
            &R::Plan {
                based_on_plan_revision: 0,
                tasks: Vec::new(),
                retire_task_ids: Vec::new(),
                rationale_text: String::new(),
            }
        ));
        assert!(result_variant_matches_task(
            TaskKind::Review,
            &R::Review {
                candidate_id: crate::mission::types::Id::generate(),
                report_text: String::new(),
                findings: Vec::new(),
            }
        ));
    }

    #[test]
    fn role_kind_pairing() {
        assert!(role_matches_kind(TaskKind::Verify, None));
        assert!(!role_matches_kind(TaskKind::Verify, Some(Role::Builder)));
        assert!(role_matches_kind(TaskKind::Implement, Some(Role::Builder)));
        assert!(!role_matches_kind(
            TaskKind::Implement,
            Some(Role::Reviewer)
        ));
    }
}

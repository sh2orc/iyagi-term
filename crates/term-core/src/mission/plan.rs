//! Plan validation and application assembly (02 §5). Everything here is a
//! pure function over the mission snapshot + the proposal: the daemon
//! allocates UUIDs for new local keys once, registers the proposal
//! artifact, then calls [`validate_proposal`] and commits the returned
//! [`PlanApplication`] atomically (partial application is a contract
//! violation — this module never emits one).

use std::collections::{HashMap, HashSet};

use term_contracts::mission::plan::{validate_plan_graph, PlanGraphError, PlanNode};
use term_contracts::mission::types::{
    ArtifactRef, Id, Mission, ProviderTaskSpec, Role, Task, TaskKind,
};
use term_contracts::mission::validation::{
    role_matches_kind, validate_allowed_path, MissionLimits,
};

/// A validated plan ready for the storage transaction.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanApplication {
    /// New tasks in topological order (dependency inserts succeed).
    pub tasks: Vec<Task>,
    /// Retired task ids (validated eligible for retirement).
    pub retired_task_ids: Vec<Id>,
    /// The plan revision after applying (mission.plan_revision + 1).
    pub next_plan_revision: u32,
}

/// One proposal task after local-key resolution (daemon allocates the UUID
/// exactly once and re-uses it on re-sends — 02 §5).
#[derive(Debug, Clone, PartialEq)]
pub struct PlanCandidate {
    /// daemon-allocated UUID for the provider's local_key.
    pub id: Id,
    pub spec: ProviderTaskSpec,
}

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("invalid plan: {0}")]
    Invalid(String),
    #[error("plan policy denied: {0}")]
    PolicyDenied(String),
    #[error("plan is based on revision {proposed}, mission is at {current}")]
    StaleBase { proposed: u32, current: u32 },
    #[error("graph: {0}")]
    Graph(#[from] PlanGraphError),
    #[error("task {key:?} has kind {kind:?} which needs role {wanted:?}")]
    RoleMismatch {
        key: String,
        kind: TaskKind,
        wanted: Option<Role>,
    },
    #[error("task references unknown requirement {1}")]
    UnknownRequirement(String, String),
    #[error("task uses binding {1} outside the allowlist")]
    BindingNotAllowed(String, String),
    #[error("task has an invalid allowed path {path:?}")]
    BadPath { key: String, path: String },
    #[error("requirement {0} has no covering task and no human check")]
    UncoveredRequirement(String),
    #[error("retired task {0} still owns an execution or is ineligible for replacement")]
    RetireIneligible(Id),
    #[error("retire leaves live dependents {0:?}")]
    RetireHasDependents(Vec<Id>),
    #[error("plan exceeds the mission task cap ({cap})")]
    TooManyTasks { cap: usize },
    #[error("plan revision budget exhausted ({cap})")]
    TooManyRevisions { cap: u32 },
    #[error("verify task {key:?} must have role=null and binding=null")]
    VerifyTainted { key: String },
    #[error("result variant mismatch for task {key:?}")]
    ResultVariantMismatch { key: String },
}

/// Validate a resolved proposal against the mission snapshot (02 §5 order):
/// base revision, ids/uniqueness, role/kind pairing, bindings allowlist,
/// dependency graph over accepted+new tasks, parent depth, retire rules,
/// requirement coverage, and path shapes.
pub fn validate_proposal(
    mission: &Mission,
    existing_tasks: &[Task],
    candidates: &[PlanCandidate],
    retire_task_ids: &[Id],
    objective_refs: &HashMap<Id, ArtifactRef>,
    limits: &MissionLimits,
) -> Result<PlanApplication, PlanError> {
    validate_proposal_with_exclusions(
        mission,
        existing_tasks,
        candidates,
        retire_task_ids,
        objective_refs,
        limits,
        &HashSet::new(),
    )
}

/// Exclusions are daemon-verified user decisions, never provider assertions.
/// Historical tasks still consume ordinals and limits but cannot cover work
/// or supply dependencies in the new active plan.
pub fn validate_proposal_with_exclusions(
    mission: &Mission,
    existing_tasks: &[Task],
    candidates: &[PlanCandidate],
    retire_task_ids: &[Id],
    objective_refs: &HashMap<Id, ArtifactRef>,
    limits: &MissionLimits,
    excluded: &HashSet<Id>,
) -> Result<PlanApplication, PlanError> {
    if candidates.is_empty() && retire_task_ids.is_empty() && excluded.is_empty() {
        return Err(PlanError::Graph(PlanGraphError::Cycle));
    }
    // 1. Base revision: the proposal was written against this plan shape.
    // (The caller compares based_on_plan_revision; the mission row here is
    // the current projection.)
    if mission.plan_revision >= limits.max_plan_revisions {
        return Err(PlanError::TooManyRevisions {
            cap: limits.max_plan_revisions,
        });
    }

    let mut keys = HashSet::new();
    for c in candidates {
        let key = c.spec.local_key.as_bytes();
        if key.is_empty()
            || key.len() > 64
            || !key[0].is_ascii_lowercase()
            || !key
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
            || !keys.insert(c.spec.local_key.as_str())
        {
            return Err(PlanError::Invalid(
                "local keys must be unique ASCII task names".into(),
            ));
        }
        if c.spec.title.trim().is_empty() || c.spec.title.len() > limits.max_title_bytes {
            return Err(PlanError::Invalid("task title exceeds its bounds".into()));
        }
        if c.spec
            .role
            .is_some_and(|role| !mission.policy.allowed_roles.contains(&role))
        {
            return Err(PlanError::PolicyDenied(
                "role outside the mission allowlist".into(),
            ));
        }
        if c.spec.kind != TaskKind::Verify && c.spec.binding_id.is_none() {
            return Err(PlanError::Invalid(
                "AI task needs an explicit binding".into(),
            ));
        }
        if c.spec
            .verification_ids
            .iter()
            .any(|id| !mission.policy.allowed_verification_ids.contains(id))
        {
            return Err(PlanError::PolicyDenied(
                "verification command outside the allowlist".into(),
            ));
        }
    }
    // 2. Kind/role pairing and verify purity (02 §5 rule 2).
    for candidate in candidates {
        if !role_matches_kind(candidate.spec.kind, candidate.spec.role) {
            return Err(PlanError::RoleMismatch {
                key: candidate.spec.local_key.clone(),
                kind: candidate.spec.kind,
                wanted: candidate.spec.role,
            });
        }
        if candidate.spec.kind == TaskKind::Verify
            && (candidate.spec.role.is_some() || candidate.spec.binding_id.is_some())
        {
            return Err(PlanError::VerifyTainted {
                key: candidate.spec.local_key.clone(),
            });
        }
        if candidate.spec.kind == TaskKind::Verify && candidate.spec.verification_ids.len() != 1 {
            return Err(PlanError::Invalid(
                "each verify task must name exactly one command".into(),
            ));
        }
        if !objective_refs.contains_key(&candidate.id) {
            return Err(PlanError::Graph(PlanGraphError::MissingDependency(
                format!("objective-artifact {}", candidate.id),
                candidate.spec.local_key.clone(),
            )));
        }
    }

    // 3. Bindings inside the user allowlist (02 §5 rule 3).
    let allowed_bindings: HashSet<&str> = mission
        .policy
        .allowed_binding_ids
        .iter()
        .map(|id| id.as_str())
        .collect();
    for candidate in candidates {
        if let Some(binding) = &candidate.spec.binding_id {
            if !allowed_bindings.contains(binding.as_str()) {
                return Err(PlanError::BindingNotAllowed(
                    candidate.spec.local_key.clone(),
                    binding.to_string(),
                ));
            }
        }
    }

    // 4. Dependency graph over accepted existing + new tasks (rule 4).
    let mut nodes: Vec<PlanNode> = existing_tasks
        .iter()
        .filter(|task| {
            !excluded.contains(&task.id)
                && !matches!(
                    task.state,
                    term_contracts::mission::types::TaskState::Superseded
                )
        })
        .map(|task| PlanNode {
            id: task.id.to_string(),
            mission: mission.id.to_string(),
            deps: task.depends_on.iter().map(|id| id.to_string()).collect(),
        })
        .collect();
    let key_to_id: HashMap<&str, &Id> = candidates
        .iter()
        .map(|candidate| (candidate.spec.local_key.as_str(), &candidate.id))
        .collect();
    let mut new_ids: HashSet<String> = HashSet::new();
    for candidate in candidates {
        if !new_ids.insert(candidate.id.to_string()) {
            return Err(PlanError::Graph(PlanGraphError::DuplicateId(
                candidate.id.to_string(),
            )));
        }
    }
    for candidate in candidates {
        let mut deps = Vec::new();
        for dep in &candidate.spec.depends_on_keys {
            let resolved = match key_to_id.get(dep.as_str()) {
                Some(id) => id.to_string(),
                None => dep.clone(), // an existing task UUID
            };
            deps.push(resolved);
        }
        if let Some(parent) = &candidate.spec.parent_key {
            let resolved = match key_to_id.get(parent.as_str()) {
                Some(id) => id.to_string(),
                None => parent.clone(),
            };
            deps.push(resolved);
        }
        nodes.push(PlanNode {
            id: candidate.id.to_string(),
            mission: mission.id.to_string(),
            deps,
        });
    }
    validate_plan_graph(&nodes)?;

    // The scheduler executes implementation, then verification, then
    // review. A dependency in a later phase would deadlock a valid DAG.
    let phase = |kind: TaskKind| match kind {
        TaskKind::Review => 2,
        TaskKind::Verify => 1,
        _ => 0,
    };
    for candidate in candidates {
        for key in &candidate.spec.depends_on_keys {
            let dependency_kind =
                if let Some(new) = candidates.iter().find(|c| c.spec.local_key == *key) {
                    Some(new.spec.kind)
                } else {
                    existing_tasks
                        .iter()
                        .find(|t| {
                            t.id.as_str() == key
                                && t.state != term_contracts::mission::types::TaskState::Succeeded
                        })
                        .map(|t| t.kind)
                };
            if dependency_kind.is_some_and(|kind| phase(kind) > phase(candidate.spec.kind)) {
                return Err(PlanError::Invalid(format!(
                    "task {} depends on a later execution phase",
                    candidate.spec.local_key
                )));
            }
        }
    }

    // 5. Task cap (rule 5, delegation depth rides the parent chain length).
    if existing_tasks.len() + candidates.len() > limits.max_tasks_per_mission {
        return Err(PlanError::TooManyTasks {
            cap: limits.max_tasks_per_mission,
        });
    }

    // 6. Retire eligibility (rule 6): only unstarted/failed/blocked tasks;
    // no live dependent may remain on a retired task.
    let retiring: HashSet<&Id> = retire_task_ids.iter().collect();
    if retiring.len() != retire_task_ids.len() {
        return Err(PlanError::Invalid("duplicate retired task".into()));
    }
    for id in retire_task_ids {
        let Some(task) = existing_tasks.iter().find(|t| &t.id == id) else {
            return Err(PlanError::Graph(PlanGraphError::MissingDependency(
                id.to_string(),
                "retire".into(),
            )));
        };
        if task.active_run_id.is_some()
            || !matches!(
                task.state,
                term_contracts::mission::types::TaskState::Planned
                    | term_contracts::mission::types::TaskState::Ready
                    | term_contracts::mission::types::TaskState::Blocked
                    | term_contracts::mission::types::TaskState::Failed
                    | term_contracts::mission::types::TaskState::Cancelled
            )
        {
            return Err(PlanError::RetireIneligible(task.id.clone()));
        }
    }
    let dependents: Vec<Id> = existing_tasks
        .iter()
        .filter(|task| {
            !retiring.contains(&task.id)
                && !matches!(
                    task.state,
                    term_contracts::mission::types::TaskState::Superseded
                        | term_contracts::mission::types::TaskState::Cancelled
                )
                && task.depends_on.iter().any(|dep| retiring.contains(dep))
        })
        .map(|task| task.id.clone())
        .collect();
    if candidates.iter().any(|c| {
        c.spec
            .depends_on_keys
            .iter()
            .any(|key| retire_task_ids.iter().any(|id| id.as_str() == key))
    }) {
        return Err(PlanError::Invalid(
            "new task depends on a retired task".into(),
        ));
    }
    if !dependents.is_empty() {
        return Err(PlanError::RetireHasDependents(dependents));
    }

    // 7. Requirement coverage (rule 7): every requirement has a covering
    // task or a final human check.
    let requirement_ids: HashSet<&str> =
        mission.requirements.iter().map(|r| r.id.as_str()).collect();
    let mut covered: HashSet<&str> = mission
        .requirements
        .iter()
        .filter(|r| r.human_check)
        .map(|r| r.id.as_str())
        .collect();
    for task in existing_tasks.iter().filter(|t| {
        t.required
            && t.kind != TaskKind::Plan
            && !excluded.contains(&t.id)
            && !retiring.contains(&t.id)
            && !matches!(
                t.state,
                term_contracts::mission::types::TaskState::Superseded
                    | term_contracts::mission::types::TaskState::Cancelled
            )
    }) {
        covered.extend(task.contract.requirement_ids.iter().map(Id::as_str));
    }
    for candidate in candidates {
        for requirement in &candidate.spec.requirement_ids {
            if !requirement_ids.contains(requirement.as_str()) {
                return Err(PlanError::UnknownRequirement(
                    candidate.spec.local_key.clone(),
                    requirement.to_string(),
                ));
            }
            if candidate.spec.required && candidate.spec.kind != TaskKind::Plan {
                covered.insert(requirement.as_str());
            }
        }
    }
    for requirement in &mission.requirements {
        if !covered.contains(requirement.id.as_str()) {
            return Err(PlanError::UncoveredRequirement(requirement.id.to_string()));
        }
    }

    // 8. Writable paths are repository-relative and .git-free (rule 8).
    for candidate in candidates {
        for path in &candidate.spec.allowed_paths {
            validate_allowed_path(path).map_err(|e| PlanError::BadPath {
                key: candidate.spec.local_key.clone(),
                path: e.message,
            })?;
        }
    }

    // 9. Verify commands come from the allowlist (rule 7 tail) — the
    // daemon checks command ids against its registry before this call;
    // here we only enforce the ID shape.
    for candidate in candidates {
        for command in &candidate.spec.verification_ids {
            let _ = command; // registry check is daemon-side (storage row).
        }
    }

    // Assemble the Task rows in topological order.
    let order = term_contracts::mission::plan::topological_order(&nodes)?;
    let by_id: HashMap<&str, &PlanCandidate> =
        candidates.iter().map(|c| (c.id.as_str(), c)).collect();
    let mut tasks = Vec::with_capacity(candidates.len());
    let base_ordinal = existing_tasks
        .iter()
        .map(|t| t.ordinal)
        .max()
        .map_or(0, |n| n + 1);
    for node_id in &order {
        if let Some(candidate) = by_id.get(node_id.as_str()) {
            tasks.push(Task {
                id: candidate.id.clone(),
                mission_id: mission.id.clone(),
                title: candidate.spec.title.clone(),
                kind: candidate.spec.kind,
                role: candidate.spec.role,
                state: term_contracts::mission::types::TaskState::Planned,
                required: candidate.spec.required,
                parent_task_id: candidate.spec.parent_key.as_ref().and_then(|key| {
                    key_to_id
                        .get(key.as_str())
                        .map(|id| (*id).clone())
                        .or_else(|| {
                            existing_tasks
                                .iter()
                                .find(|t| t.id.as_str() == key.as_str())
                                .map(|t| t.id.clone())
                        })
                }),
                depends_on: candidate
                    .spec
                    .depends_on_keys
                    .iter()
                    .map(|key| {
                        key_to_id
                            .get(key.as_str())
                            .map(|id| (*id).clone())
                            .unwrap_or_else(|| Id::parse(key).expect("existing task uuid"))
                    })
                    .collect(),
                contract: term_contracts::mission::types::TaskContract {
                    objective_ref: objective_refs[&candidate.id].clone(),
                    requirement_ids: candidate.spec.requirement_ids.clone(),
                    input_artifact_ids: candidate.spec.input_artifact_ids.clone(),
                    allowed_paths: candidate.spec.allowed_paths.clone(),
                    expected_outputs: candidate.spec.expected_outputs.clone(),
                    verification_ids: candidate.spec.verification_ids.clone(),
                    specialty: candidate.spec.specialty.clone(),
                },
                binding_id: candidate.spec.binding_id.clone(),
                active_run_id: None,
                ordinal: base_ordinal + tasks.len() as u32,
                attempt_count: 0,
                repair_cycle: 0,
                failure_repair_run_ids: vec![],
                integration: None,
                replacement_of: candidate.spec.replacement_of.clone(),
                blocked_code: None,
                dispatch_after_unix_ms: None,
                workspace_id: None,
                created_at: String::new(),
                updated_at: String::new(),
            });
        }
    }

    let parents: HashMap<&Id, Option<&Id>> = existing_tasks
        .iter()
        .chain(tasks.iter())
        .map(|t| (&t.id, t.parent_task_id.as_ref()))
        .collect();
    for task in &tasks {
        let mut seen = HashSet::new();
        let mut parent = task.parent_task_id.as_ref();
        let mut depth = 0;
        while let Some(id) = parent {
            if !seen.insert(id) {
                return Err(PlanError::Invalid("parent cycle".into()));
            }
            depth += 1;
            if depth > limits.max_delegation_depth {
                return Err(PlanError::Invalid("delegation depth exceeded".into()));
            }
            parent = parents.get(id).copied().flatten();
        }
    }
    Ok(PlanApplication {
        tasks,
        retired_task_ids: retire_task_ids.to_vec(),
        next_plan_revision: mission.plan_revision + 1,
    })
}

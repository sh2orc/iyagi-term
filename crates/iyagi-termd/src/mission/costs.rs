//! Cost admission and reversible, task-scoped budget decisions.
use super::{
    service::MissionService,
    workflow::{self, MissionEntities},
};
use std::collections::{HashMap, HashSet};
use term_contracts::mission::{types::*, MissionErrorCode, MissionRpcError};
use term_core::mission::budget::{cost_admission, summarize_cost, CostBlock};

pub(super) const STOP_COST_MISSION: &str = "stop_cost_mission";
pub(super) fn is_cost_decision(decision: &Decision) -> bool {
    decision.kind == DecisionKind::Budget
        && decision.options.iter().any(|o| o.id == STOP_COST_MISSION)
}
fn cost_blocked(task: &Task) -> bool {
    task.state == TaskState::Blocked
        && task.active_run_id.is_none()
        && matches!(
            task.blocked_code.as_deref(),
            Some("cost_limit" | "cost_unknown")
        )
}

impl MissionService {
    fn cost_decision(
        &self,
        snapshot: &MissionEntities,
        task: &Task,
        reason: CostBlock,
    ) -> Result<Decision, MissionRpcError> {
        let total = summarize_cost(&snapshot.runs);
        let question = workflow::store_artifact(&self.artifacts, &snapshot.mission.id, "text/plain", format!(
            "Task '{}' is waiting for cost admission ({}). Observed provider cost: {} USD micros; reserved or estimated cost: {} USD micros; unpriced runs: {}. Set a per-run estimate on the model connection, adjust the mission cost/unknown-cost policy, or stop the mission. Estimates cannot enforce a provider hard cap. Independent affordable tasks can continue.",
            task.title, reason.code(), total.observed_micros, total.estimated_micros, total.unknown_runs).as_bytes())?;
        let (_, decision) = super::engine::new_decision(
            &snapshot.mission,
            DecisionKind::Budget,
            question,
            vec![DecisionOption {
                id: STOP_COST_MISSION.into(),
                label: "Stop mission".into(),
            }],
            vec![task.id.clone()],
            false,
            None,
        );
        Ok(decision)
    }

    pub(super) fn block_cost(
        &self,
        snapshot: &MissionEntities,
        task: &Task,
        reason: CostBlock,
    ) -> Result<(), MissionRpcError> {
        let mut mission = snapshot.mission.clone();
        let mut next = task.clone();
        next.state = TaskState::Blocked;
        next.blocked_code = Some(reason.code().into());
        next.updated_at = term_storage::time::now_iso8601();
        let decision = self.cost_decision(snapshot, task, reason)?;
        mission.open_decision_count += 1;
        self.commit_actor(
            mission,
            "engine.cost_block",
            vec![
                Entity::Task(Box::new(next)),
                Entity::Decision(Box::new(decision)),
            ],
            vec![],
        )
    }

    /// Re-read saved bindings and reservations so completed runs, updated
    /// estimates, or a policy change can release only the relevant cost hold.
    pub(super) fn reconcile_cost_blocks(&self) -> Result<(), MissionRpcError> {
        let bindings: HashMap<Id, Binding> = self
            .storage
            .mission_bindings()
            .map_err(Self::store_error)?
            .into_iter()
            .map(serde_json::from_value::<Binding>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| {
                MissionRpcError::new(MissionErrorCode::Internal, "stored binding is invalid")
            })?
            .into_iter()
            .map(|b| (b.id.clone(), b))
            .collect();
        let mut cursor = None;
        loop {
            let (missions, next) = self
                .storage
                .mission_list(cursor, 50, false)
                .map_err(Self::store_error)?;
            for mission in missions {
                let snapshot = workflow::load_entities(&self.storage, &mission.id)?;
                let result = self.reconcile_cost_snapshot(&snapshot, &bindings);
                if let Err(error) = result {
                    if error.code != MissionErrorCode::RevisionConflict {
                        return Err(error);
                    }
                }
            }
            if next.is_none() {
                break;
            }
            cursor = next;
        }
        Ok(())
    }

    fn reconcile_cost_snapshot(
        &self,
        snapshot: &MissionEntities,
        bindings: &HashMap<Id, Binding>,
    ) -> Result<(), MissionRpcError> {
        let mut mission = snapshot.mission.clone();
        let actionable = matches!(
            mission.state,
            MissionState::Running | MissionState::Pausing | MissionState::Paused
        );
        let mut upserts = vec![];
        let mut retained = HashSet::new();
        let mut resolved = HashSet::new();
        let mut reasons = HashMap::new();
        if actionable {
            for task in snapshot.tasks.iter().filter(|task| cost_blocked(task)) {
                let Some(binding) = task
                    .binding_id
                    .as_ref()
                    .and_then(|id| bindings.get(id))
                    .filter(|b| b.enabled && mission.policy.allowed_binding_ids.contains(&b.id))
                else {
                    continue;
                };
                match cost_admission(&mission.policy, &snapshot.runs, Some(binding)) {
                    Ok(()) => {
                        let mut next = task.clone();
                        next.state = TaskState::Ready;
                        next.blocked_code = None;
                        next.updated_at = term_storage::time::now_iso8601();
                        upserts.push(Entity::Task(Box::new(next)));
                        resolved.insert(task.id.clone());
                    }
                    Err(reason) => {
                        reasons.insert(task.id.clone(), reason);
                    }
                }
            }
        }
        for decision in snapshot
            .decisions
            .iter()
            .filter(|d| d.state == DecisionState::Open && is_cost_decision(d))
        {
            let task = snapshot
                .tasks
                .iter()
                .find(|t| decision.affected_task_ids == [t.id.clone()]);
            let valid = actionable
                && task.is_some_and(|t| {
                    cost_blocked(t)
                        && !resolved.contains(&t.id)
                        && reasons
                            .get(&t.id)
                            .is_none_or(|r| t.blocked_code.as_deref() == Some(r.code()))
                })
                && decision.plan_revision == mission.plan_revision
                && decision.candidate_id == mission.candidate_id;
            if valid && retained.insert(task.expect("valid task").id.clone()) {
                continue;
            }
            let mut old = decision.clone();
            old.state = DecisionState::Obsolete;
            mission.open_decision_count = mission.open_decision_count.saturating_sub(1);
            upserts.push(Entity::Decision(Box::new(old)));
        }
        for (id, reason) in reasons {
            if retained.contains(&id) {
                continue;
            }
            let task = snapshot
                .tasks
                .iter()
                .find(|t| t.id == id)
                .expect("cost task");
            let mut next = task.clone();
            next.blocked_code = Some(reason.code().into());
            next.updated_at = term_storage::time::now_iso8601();
            upserts.push(Entity::Task(Box::new(next)));
            upserts.push(Entity::Decision(Box::new(
                self.cost_decision(snapshot, task, reason)?,
            )));
            mission.open_decision_count += 1;
        }
        if !upserts.is_empty() {
            self.commit_actor(mission, "engine.cost_reconcile", upserts, vec![])?;
        }
        Ok(())
    }
}

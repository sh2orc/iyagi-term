//! Provider reset holds are durable and task-scoped, without consuming attempts.
use super::{service::MissionService, workflow};
use std::collections::HashMap;
use term_contracts::{
    ids::U64String,
    mission::{types::*, MissionErrorCode, MissionRpcError},
};
use term_core::mission::rate_limits::reset_deadline;

pub(super) const RATE_LIMITED: &str = "provider_rate_limited";

impl MissionService {
    pub(super) fn rate_limit_deadline(
        &self,
        binding: &Binding,
    ) -> Result<Option<u64>, MissionRpcError> {
        let now = (self.wall_millis)();
        let runs = self
            .storage
            .mission_rate_limit_runs(now)
            .map_err(Self::store_error)?;
        Ok(reset_deadline(binding, &runs, now))
    }

    pub(super) fn hold_for_rate_limit(
        &self,
        snapshot: &workflow::MissionEntities,
        task: &Task,
        until: u64,
    ) -> Result<(), MissionRpcError> {
        let mut task = task.clone();
        task.state = TaskState::Blocked;
        task.blocked_code = Some(RATE_LIMITED.into());
        task.dispatch_after_unix_ms = Some(U64String::new(until).expect("validated reset"));
        task.updated_at = term_storage::time::now_iso8601();
        self.commit_actor(
            snapshot.mission.clone(),
            "engine.rate_limit_hold",
            vec![Entity::Task(Box::new(task))],
            vec![],
        )
    }

    // Called under the dispatch guard. A provider observation and a new Run
    // reservation cannot commit on opposite sides of a stale admission check.
    pub(super) fn reconcile_rate_limits(&self) -> Result<(), MissionRpcError> {
        let now = (self.wall_millis)();
        let runs = self
            .storage
            .mission_rate_limit_runs(now)
            .map_err(Self::store_error)?;
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
                if !matches!(
                    mission.state,
                    MissionState::Running | MissionState::Paused | MissionState::Pausing
                ) {
                    continue;
                }
                let snapshot = workflow::load_entities(&self.storage, &mission.id)?;
                if !matches!(
                    snapshot.mission.state,
                    MissionState::Running | MissionState::Paused | MissionState::Pausing
                ) {
                    continue;
                }
                let mut upserts = vec![];
                for task in &snapshot.tasks {
                    let held = task.state == TaskState::Blocked
                        && task.blocked_code.as_deref() == Some(RATE_LIMITED);
                    if task.active_run_id.is_some() || !(held || task.state == TaskState::Ready) {
                        continue;
                    }
                    let until = task
                        .execution_binding_id()
                        .and_then(|id| bindings.get(id))
                        .and_then(|b| reset_deadline(b, &runs, now));
                    let mut next = task.clone();
                    match until {
                        Some(until) => {
                            next.state = TaskState::Blocked;
                            next.blocked_code = Some(RATE_LIMITED.into());
                            next.dispatch_after_unix_ms =
                                Some(U64String::new(until).expect("validated reset"));
                        }
                        None if held => {
                            next.state = TaskState::Ready;
                            next.blocked_code = None;
                            next.dispatch_after_unix_ms = None;
                        }
                        None => continue,
                    }
                    if &next != task {
                        next.updated_at = term_storage::time::now_iso8601();
                        upserts.push(Entity::Task(Box::new(next)));
                    }
                }
                if !upserts.is_empty() {
                    if let Err(error) = self.commit_actor(
                        snapshot.mission,
                        "engine.rate_limit_reconcile",
                        upserts,
                        vec![],
                    ) {
                        if error.code != MissionErrorCode::RevisionConflict {
                            return Err(error);
                        }
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
}

//! Persist a delay for a proved, unsubmitted request before creating a new Run.
use super::{failures, service::MissionService, workflow::MissionEntities};
use term_contracts::{
    ids::U64String,
    mission::{types::*, MissionErrorCode, MissionRpcError},
};

pub(super) const RETRY_WAIT: &str = "transient_retry";

impl MissionService {
    pub(super) fn reconcile_transient_retry_snapshot(
        &self,
        snapshot: &MissionEntities,
    ) -> Result<bool, MissionRpcError> {
        if !matches!(
            snapshot.mission.state,
            MissionState::Running | MissionState::Pausing | MissionState::Paused
        ) {
            return Ok(false);
        }
        let mut mission = snapshot.mission.clone();
        let mut changes = Vec::new();
        let now = (self.wall_millis)();
        for task in &snapshot.tasks {
            let pending = task.state == TaskState::Blocked
                && task.blocked_code.as_deref() == Some(RETRY_WAIT);
            if task.state != TaskState::Failed && !pending {
                continue;
            }
            // A terminal Run alone cannot release an unfinished/missing Exec.
            let Some(run) = failures::ended_failure(snapshot, task) else {
                continue;
            };
            let deadline = term_core::mission::retry::retry_deadline(run);
            let prepared = deadline.map(|_| self.prepare_task_retry(snapshot, task, None));
            let mut next = match prepared {
                Some(Ok(next)) => next,
                Some(Err(error))
                    if !matches!(
                        error.code,
                        MissionErrorCode::PolicyDenied
                            | MissionErrorCode::BudgetExceeded
                            | MissionErrorCode::ModelUnavailable
                    ) =>
                {
                    return Err(error)
                }
                _ if pending => {
                    let mut next = task.clone();
                    next.state = TaskState::Failed;
                    next.blocked_code = Some(format!("{:?}", run.failure_code));
                    next.dispatch_after_unix_ms = None;
                    next
                }
                _ => continue,
            };
            if let Some(deadline) = deadline.filter(|_| next.state == TaskState::Ready) {
                if !pending || now < deadline || mission.state != MissionState::Running {
                    next.state = TaskState::Blocked;
                    next.blocked_code = Some(RETRY_WAIT.into());
                    next.dispatch_after_unix_ms =
                        Some(U64String::new(deadline).expect("validated retry deadline"));
                }
            }
            // Preparing a retry stamps updated_at; compare substantive state
            // first so an idle delay produces no event/revision churn.
            next.updated_at = task.updated_at.clone();
            if &next != task {
                next.updated_at = term_storage::time::now_iso8601();
                changes.extend(failures::obsolete_failure_decisions(
                    &mut mission,
                    &snapshot.decisions,
                    &task.id,
                ));
                changes.push(Entity::Task(Box::new(next)));
            }
        }
        if changes.is_empty() {
            return Ok(false);
        }
        self.commit_actor(mission, "engine.transient_retry", changes, vec![])?;
        Ok(true)
    }
}

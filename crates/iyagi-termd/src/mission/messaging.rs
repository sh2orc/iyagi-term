//! A message is first queued for routing. A separate immutable intent binds
//! delivery to one Run and fence; routing acknowledgement is not delivery.
use serde_json::json;
use term_contracts::mission::{types::*, MissionErrorCode, MissionRpcError};
use term_storage::mission::types::{
    OutboxIntent, OutboxOperation, OutboxState, OutboxUpdate, StoredOutbox,
};

use super::{
    service::MissionService,
    workflow::{self, MissionEntities},
};

pub(super) fn route_intent(message: &Message, excluded_run: Option<&Id>) -> OutboxIntent {
    let id = Id::generate();
    OutboxIntent {
        dedupe_key: format!("{}/{}/route/{}", message.mission_id, message.id, id),
        id,
        mission_id: message.mission_id.clone(),
        run_id: None,
        operation: OutboxOperation::Message,
        // Routing has no Run owner; its initial version still obeys the
        // storage contract's positive fence invariant.
        fencing_token: 1,
        payload: json!({"mode":"route","message_id":message.id,"excluded_run_id":excluded_run}),
        created_at: term_storage::time::now_iso8601(),
    }
}

fn bound_intent(message: &Message, run: &Run, mode: &str) -> OutboxIntent {
    OutboxIntent {
        id: Id::generate(),
        mission_id: message.mission_id.clone(),
        run_id: Some(run.id.clone()),
        operation: OutboxOperation::Message,
        dedupe_key: format!("{}/{}/{}/{mode}", message.mission_id, message.id, run.id),
        fencing_token: run.fencing_token.get(),
        payload: json!({"mode":mode,"message_id":message.id}),
        created_at: term_storage::time::now_iso8601(),
    }
}

fn update(intent: &StoredOutbox, from: OutboxState, to: OutboxState) -> OutboxUpdate {
    OutboxUpdate {
        id: intent.id.clone(),
        expected_state: from,
        state: to,
        fencing_token: intent.fencing_token,
    }
}

#[derive(Default)]
pub(super) struct MessageEffects {
    pub upserts: Vec<Entity>,
    pub intents: Vec<OutboxIntent>,
    pub updates: Vec<OutboxUpdate>,
}

impl MessageEffects {
    fn bind(&mut self, route: &StoredOutbox, message: &Message, run: &Run, mode: &str) {
        let bound = bound_intent(message, run, mode);
        // Both routing transitions and the new immutable Run binding commit
        // together. The Message remains queued until the provider accepts it.
        self.updates
            .push(update(route, OutboxState::Prepared, OutboxState::Sending));
        self.updates.push(update(
            route,
            OutboxState::Sending,
            OutboxState::Acknowledged,
        ));
        if mode == "context" {
            self.updates.push(OutboxUpdate {
                id: bound.id.clone(),
                expected_state: OutboxState::Prepared,
                state: OutboxState::Sending,
                fencing_token: bound.fencing_token,
            });
        }
        self.intents.push(bound);
        let mut message = message.clone();
        message.run_id = Some(run.id.clone());
        self.upserts.push(Entity::Message(Box::new(message)));
    }
}

impl MissionService {
    pub(super) fn validate_message_replacement(
        &self,
        snapshot: &MissionEntities,
        original_id: &Id,
        target: Option<&Id>,
    ) -> Result<(), MissionRpcError> {
        let messages = self.messages(&snapshot.mission.id)?;
        let original = messages
            .iter()
            .find(|message| &message.id == original_id)
            .ok_or_else(|| {
                MissionRpcError::new(
                    MissionErrorCode::NotFound,
                    "original message is not in this mission",
                )
            })?;
        if original.role != MessageRole::User
            || !matches!(
                original.delivery,
                MessageDelivery::Unknown | MessageDelivery::Rejected
            )
            || original.target_task_id.as_ref() != target
            || snapshot
                .decisions
                .iter()
                .any(|decision| decision.answer_message_id.as_ref() == Some(original_id))
        {
            return Err(MissionRpcError::new(MissionErrorCode::InvalidState,
                "only an unknown or rejected user instruction can be replaced for the same recipient; decision answers cannot be resent"));
        }
        if messages
            .iter()
            .any(|message| message.supersedes_message_id.as_ref() == Some(original_id))
        {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidState,
                "this message already has a replacement",
            ));
        }
        if self
            .storage
            .mission_outbox()
            .map_err(Self::store_error)?
            .iter()
            .any(|intent| {
                intent.mission_id == snapshot.mission.id
                    && intent.payload["message_id"].as_str() == Some(original_id.as_str())
                    && matches!(intent.state, OutboxState::Prepared | OutboxState::Sending)
            })
        {
            return Err(MissionRpcError::new(
                MissionErrorCode::InvalidState,
                "the original delivery still has a pending intent",
            ));
        }
        // The original Message, Run, and unknown receipt remain immutable.
        // A new intent is created only by the explicit mission.message request.
        Ok(())
    }

    pub(super) fn messages(&self, mission_id: &Id) -> Result<Vec<Message>, MissionRpcError> {
        Ok(self
            .storage
            .mission_snapshot(mission_id)
            .map_err(Self::store_error)?
            .map(|s| {
                s.entities
                    .into_iter()
                    .filter_map(|e| match e {
                        Entity::Message(m) => Some(*m),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub(super) fn message_body(
        &self,
        mission_id: &Id,
        message: &Message,
    ) -> Result<String, MissionRpcError> {
        let bytes = self
            .artifacts
            .read_mission_body(mission_id, &message.body_ref, self.limits.max_message_bytes)
            .map_err(|(code, message)| MissionRpcError::new(code, message))?;
        String::from_utf8(bytes).map_err(|_| {
            MissionRpcError::new(
                MissionErrorCode::InvalidArgument,
                "message must contain UTF-8 text",
            )
        })
    }

    pub(super) fn message_lead_task(
        &self,
        snapshot: &MissionEntities,
    ) -> Result<Option<Task>, MissionRpcError> {
        if snapshot.mission.state != MissionState::Running
            || snapshot
                .tasks
                .iter()
                .filter(|t| t.role == Some(Role::Lead))
                .max_by_key(|t| t.ordinal)
                .is_some_and(|t| t.state == TaskState::Failed)
            || snapshot.tasks.iter().any(|t| {
                t.role == Some(Role::Lead)
                    && (matches!(
                        t.state,
                        TaskState::Planned
                            | TaskState::Ready
                            | TaskState::Blocked
                            | TaskState::Running
                            | TaskState::AwaitingInput
                            | TaskState::AwaitingReview
                    ) || t.active_run_id.is_some())
            })
            || snapshot
                .decisions
                .iter()
                .any(|d| d.state == DecisionState::Open && d.blocking)
        {
            return Ok(None);
        }
        if snapshot.tasks.len() >= self.limits.max_tasks_per_mission
            || snapshot.mission.plan_revision >= self.limits.max_plan_revisions
        {
            return Err(MissionRpcError::new(
                MissionErrorCode::PlanLimit,
                "message replanning would exceed the task limit",
            ));
        }
        let binding = snapshot
            .mission
            .role_bindings
            .iter()
            .find(|r| r.role == Role::Lead)
            .map(|r| r.primary_binding_id.clone())
            .ok_or_else(|| {
                MissionRpcError::new(
                    MissionErrorCode::ModelUnavailable,
                    "Lead binding is missing",
                )
            })?;
        let mut task = super::pipeline::task(
            &snapshot.mission,
            &snapshot.tasks,
            TaskKind::Plan,
            Some(Role::Lead),
            "Update the mission plan from queued instructions".into(),
            snapshot.mission.goal_ref.clone(),
            Some(binding),
        );
        task.contract.verification_ids = snapshot.mission.policy.allowed_verification_ids.clone();
        task.state = TaskState::Ready;
        Ok(Some(task))
    }

    /// Queued/Delivered rows are context; uncertain or rejected requests are
    /// never silently replayed. An in-flight steer belongs only to its Run.
    pub(super) fn context_messages(
        &self,
        mission_id: &Id,
        task: &Task,
    ) -> Result<Vec<Message>, MissionRpcError> {
        let mut messages: Vec<_> = self
            .messages(mission_id)?
            .into_iter()
            .filter(|m| {
                (m.target_task_id.as_ref() == Some(&task.id)
                    || (m.target_task_id.is_none() && task.role == Some(Role::Lead)))
                    && (m.delivery == MessageDelivery::Delivered
                        || (m.delivery == MessageDelivery::Queued && m.run_id.is_none()))
            })
            .collect();
        messages
            .sort_by(|a, b| (&a.created_at, a.id.as_str()).cmp(&(&b.created_at, b.id.as_str())));
        Ok(messages)
    }

    pub(super) fn bind_context_messages(
        &self,
        run: &Run,
        messages: &[Message],
    ) -> Result<MessageEffects, MissionRpcError> {
        let mut effects = MessageEffects::default();
        for route in self
            .storage
            .mission_outbox()
            .map_err(Self::store_error)?
            .iter()
            .filter(|i| {
                i.mission_id == run.mission_id
                    && i.operation == OutboxOperation::Message
                    && i.state == OutboxState::Prepared
                    && i.payload["mode"] == "route"
            })
        {
            if let Some(message) = messages.iter().find(|m| {
                m.delivery == MessageDelivery::Queued
                    && route.payload["message_id"].as_str() == Some(m.id.as_str())
            }) {
                effects.bind(route, message, run, "context");
            }
        }
        Ok(effects)
    }

    pub(super) fn route_messages(&self) -> Result<(), MissionRpcError> {
        let intents = self.storage.mission_outbox().map_err(Self::store_error)?;
        for route in intents.iter().filter(|i| {
            i.operation == OutboxOperation::Message
                && i.state == OutboxState::Prepared
                && i.payload["mode"] == "route"
        }) {
            let snapshot = workflow::load_entities(&self.storage, &route.mission_id)?;
            let Some(message) = self.messages(&route.mission_id)?.into_iter().find(|m| {
                route.payload["message_id"].as_str() == Some(m.id.as_str())
                    && m.delivery == MessageDelivery::Queued
                    && m.run_id.is_none()
            }) else {
                continue;
            };
            if matches!(
                snapshot.mission.state,
                MissionState::Stopping
                    | MissionState::Completed
                    | MissionState::Cancelled
                    | MissionState::Failed
            ) || snapshot
                .tasks
                .iter()
                .any(|t| message.target_task_id.as_ref() == Some(&t.id) && t.state.is_terminal())
            {
                let mut rejected = message;
                rejected.delivery = MessageDelivery::Rejected;
                self.commit_actor(
                    snapshot.mission,
                    "engine.message_closed",
                    vec![Entity::Message(Box::new(rejected))],
                    vec![update(route, OutboxState::Prepared, OutboxState::Failed)],
                )?;
                continue;
            }
            if snapshot.mission.state != MissionState::Running {
                continue;
            }
            if message.target_task_id.is_none() {
                let lead = match self.message_lead_task(&snapshot) {
                    Ok(task) => task,
                    Err(error)
                        if matches!(
                            error.code,
                            MissionErrorCode::PlanLimit | MissionErrorCode::ModelUnavailable
                        ) =>
                    {
                        let reference = workflow::store_artifact(
                            &self.artifacts,
                            &snapshot.mission.id,
                            "text/plain",
                            error.message.as_bytes(),
                        )?;
                        let (_, decision) = super::engine::new_decision(
                            &snapshot.mission,
                            DecisionKind::Budget,
                            reference,
                            vec![DecisionOption {
                                id: "stop_mission".into(),
                                label: "Stop mission and use a follow-up mission".into(),
                            }],
                            vec![],
                            true,
                            None,
                        );
                        let mut mission = snapshot.mission.clone();
                        mission.open_decision_count += 1;
                        self.commit_actor(
                            mission,
                            "engine.message_plan_blocked",
                            vec![Entity::Decision(Box::new(decision))],
                            vec![],
                        )?;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if let Some(task) = lead {
                    let mut mission = snapshot.mission.clone();
                    mission.phase = Phase::Planning;
                    self.commit_actor(
                        mission,
                        "engine.message_plan",
                        vec![Entity::Task(Box::new(task))],
                        vec![],
                    )?;
                    continue;
                }
            }
            let target = snapshot
                .tasks
                .iter()
                .filter(|t| {
                    message
                        .target_task_id
                        .as_ref()
                        .map_or(t.role == Some(Role::Lead), |id| &t.id == id)
                        && !t.state.is_terminal()
                })
                .max_by_key(|t| t.ordinal);
            let Some(run) = target.and_then(|t| {
                snapshot
                    .runs
                    .iter()
                    .find(|r| t.active_run_id.as_ref() == Some(&r.id))
            }) else {
                continue;
            };
            if !matches!(run.state, RunState::Running | RunState::AwaitingInput)
                || run.dispatch_state != RunDispatchState::Acknowledged
                || !run
                    .binding_snapshot
                    .as_ref()
                    .is_some_and(|b| b.capabilities.steer.supported)
                || route.payload["excluded_run_id"].as_str() == Some(run.id.as_str())
            {
                continue;
            }
            let mut effects = MessageEffects::default();
            effects.bind(route, &message, run, "steer");
            self.commit_actor_effects(
                snapshot.mission,
                "engine.route_message",
                effects.upserts,
                effects.intents,
                effects.updates,
            )?;
        }
        Ok(())
    }

    pub(super) fn context_delivery_effects(
        &self,
        run: &Run,
        delivered: bool,
    ) -> Result<MessageEffects, MissionRpcError> {
        let mut effects = MessageEffects::default();
        let messages = self.messages(&run.mission_id)?;
        for intent in self
            .storage
            .mission_outbox()
            .map_err(Self::store_error)?
            .iter()
            .filter(|i| {
                i.run_id.as_ref() == Some(&run.id)
                    && i.operation == OutboxOperation::Message
                    && i.payload["mode"] == "context"
                    && i.state == OutboxState::Sending
                    && i.fencing_token == run.fencing_token.get()
            })
        {
            effects.updates.push(update(
                intent,
                OutboxState::Sending,
                if delivered {
                    OutboxState::Acknowledged
                } else {
                    OutboxState::Unknown
                },
            ));
            if let Some(message) = messages.iter().find(|m| {
                intent.payload["message_id"].as_str() == Some(m.id.as_str())
                    && m.run_id.as_ref() == Some(&run.id)
            }) {
                let mut next = message.clone();
                next.delivery = if delivered {
                    MessageDelivery::Delivered
                } else {
                    MessageDelivery::Unknown
                };
                effects.upserts.push(Entity::Message(Box::new(next)));
            }
        }
        Ok(effects)
    }

    pub(super) fn defer_message(&self, intent: &StoredOutbox) -> Result<(), MissionRpcError> {
        self.record_delivery(
            intent,
            &crate::agent_runtime::DeliveryReceipt::Queued {
                reason: crate::agent_runtime::QueuedReason::NextRun,
            },
        )
    }
}

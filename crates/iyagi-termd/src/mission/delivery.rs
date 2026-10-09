//! Adapter writes/ack waits run outside the actor. One delivery per Run is
//! in flight; persisted claims precede IO and receipts survive projection CAS.
use super::*;
use crate::agent_runtime::QueuedReason;

impl MissionActor {
    pub(super) fn collect_deliveries(&mut self) {
        let done: Vec<_> = self
            .delivering
            .iter()
            .filter(|(_, w)| w.handle.is_finished())
            .map(|(id, _)| id.clone())
            .collect();
        for id in done {
            let worker = self.delivering.remove(&id).expect("finished delivery");
            let receipt = worker.handle.join().unwrap_or(DeliveryReceipt::Unknown {
                reason: "delivery worker ended without a receipt",
            });
            self.deliveries.insert(id, (worker.intent, receipt));
        }
    }

    pub(super) fn dispatch_deliveries(&mut self) -> Result<(), MissionRpcError> {
        for mut intent in self
            .service
            .storage
            .mission_outbox()
            .map_err(MissionService::store_error)?
        {
            if self.delivering.len() >= 8 {
                break;
            }
            let message =
                intent.operation == OutboxOperation::Message && intent.payload["mode"] == "steer";
            if intent.state != OutboxState::Prepared
                || !(message || intent.operation == OutboxOperation::Answer)
            {
                continue;
            }
            let Some(run_id) = intent.run_id.clone() else {
                continue;
            };
            let snapshot = workflow::load_entities(&self.service.storage, &intent.mission_id)?;
            if message
                && matches!(
                    snapshot.mission.state,
                    MissionState::Stopping
                        | MissionState::Completed
                        | MissionState::Cancelled
                        | MissionState::Failed
                )
            {
                self.service.record_delivery(
                    &intent,
                    &DeliveryReceipt::Rejected {
                        reason: "mission stopped before message delivery",
                    },
                )?;
                continue;
            }
            let run = snapshot
                .runs
                .iter()
                .find(|r| r.id == run_id && r.fencing_token.get() == intent.fencing_token);
            if message
                && (run.is_none()
                    || run.is_some_and(|r| {
                        r.state.is_terminal()
                            || snapshot
                                .tasks
                                .iter()
                                .any(|t| t.id == r.task_id && t.state.is_terminal())
                    }))
            {
                self.service.defer_message(&intent)?;
                continue;
            }
            let Some(run) =
                run.filter(|r| matches!(r.state, RunState::Running | RunState::AwaitingInput))
            else {
                continue;
            };
            // Paused/pausing missions save instructions until resumed. A
            // stopping mission never sends a newly prepared instruction.
            if message && snapshot.mission.state != MissionState::Running {
                continue;
            }
            if self
                .delivering
                .values()
                .any(|w| w.intent.run_id.as_ref() == Some(&run_id))
                || self
                    .deliveries
                    .values()
                    .any(|(i, _)| i.run_id.as_ref() == Some(&run_id))
            {
                continue;
            }
            let Some(live) = self.live.get(&run_id).filter(|l| {
                l.token == intent.fencing_token
                    && l.starting.is_none()
                    && l.interrupted.is_none()
                    && !l.closed
            }) else {
                continue;
            };
            let (request, body) = if message {
                let Some(row) = self
                    .service
                    .messages(&intent.mission_id)?
                    .into_iter()
                    .find(|m| {
                        intent.payload["message_id"].as_str() == Some(m.id.as_str())
                            && m.run_id.as_ref() == Some(&run_id)
                            && m.delivery == MessageDelivery::Queued
                    })
                else {
                    continue;
                };
                if !run
                    .binding_snapshot
                    .as_ref()
                    .is_some_and(|b| b.capabilities.steer.supported)
                {
                    self.service.defer_message(&intent)?;
                    continue;
                }
                (None, self.service.message_body(&intent.mission_id, &row)?)
            } else {
                let request = intent.payload["provider_request_id"]
                    .as_str()
                    .ok_or_else(|| {
                        MissionRpcError::new(
                            MissionErrorCode::Internal,
                            "approval intent has no request ID",
                        )
                    })?
                    .to_owned();
                let answer = intent.payload["answer"]
                    .as_str()
                    .ok_or_else(|| {
                        MissionRpcError::new(
                            MissionErrorCode::Internal,
                            "approval intent has no answer",
                        )
                    })?
                    .to_owned();
                (Some(request), answer)
            };
            let adapter = live.adapter.clone();
            self.service.commit_actor(
                snapshot.mission,
                "engine.claim_delivery",
                vec![],
                vec![OutboxUpdate {
                    id: intent.id.clone(),
                    expected_state: OutboxState::Prepared,
                    state: OutboxState::Sending,
                    fencing_token: intent.fencing_token,
                }],
            )?;
            intent.state = OutboxState::Sending;
            let worker = std::thread::Builder::new()
                .name(format!("mission-delivery-{}", intent.id))
                .spawn(move || {
                    if let Some(request) = request {
                        adapter.answer(&run_id, &request, &body)
                    } else {
                        adapter.send_message(&run_id, &body)
                    }
                });
            match worker {
                Ok(handle) => {
                    self.delivering
                        .insert(intent.id.clone(), DeliveryWorker { intent, handle });
                }
                Err(_) => {
                    let receipt = if message {
                        DeliveryReceipt::Queued {
                            reason: QueuedReason::NextRun,
                        }
                    } else {
                        DeliveryReceipt::Rejected {
                            reason: "approval delivery worker could not start",
                        }
                    };
                    self.deliveries.insert(intent.id.clone(), (intent, receipt));
                }
            }
        }
        Ok(())
    }
}

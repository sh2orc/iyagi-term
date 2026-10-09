//! What this machine learned from Runs that actually finished (11 §7).
//!
//! A shipped fixture proves a CLI version the author had; a Run proves the one
//! the user has. Every terminal Run therefore leaves one counter on its
//! binding's daemon-owned `local_evidence`, and `capability_evidence` promotes
//! a capability once enough of them agree.
//!
//! This is observation, not state: it runs **after** the actor's transaction
//! has committed, never inside it, and a failure here is logged and dropped —
//! a Run's outcome is never re-decided by whether its evidence could be
//! stored.
use term_contracts::mission::rpc::methods;
use term_contracts::mission::types::{
    Binding, Id, LocalEvidence, LocalRunEvidence, Run, RuntimeKind,
};
use term_contracts::mission::MissionErrorCode;
use term_contracts::mission::MissionRpcError;

use super::service::MissionService;

/// The one fact a finished Run adds. Deliberately coarse: the counters answer
/// "does this CLI/model pair do the thing at all", not "how well".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RunOutcome {
    /// Succeeded on a task that never writes the workspace.
    SucceededReadOnly,
    /// Succeeded on a task that writes the workspace (the scoped-write path).
    SucceededWrite,
    /// The adapter confirmed a cancellation this daemon asked for.
    Cancelled,
    /// The CLI answered, but not in the contract's shape.
    InvalidResult,
}

/// One binding observation, taken from the Run's immutable binding snapshot so
/// a later edit of the connection cannot retarget it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RunEvidenceUpdate {
    pub(super) binding_id: Id,
    /// The version observed when the Run was prepared. `None` means the Run
    /// was prepared without a verified installation, which proves nothing
    /// about any version.
    pub(super) version: Option<String>,
    pub(super) model_id: String,
    pub(super) outcome: RunOutcome,
}

impl RunEvidenceUpdate {
    /// `None` when there is nothing to learn: a deterministic Run has no
    /// binding, and the Fake runtime is a test fixture, never a CLI.
    pub(super) fn for_run(run: &Run, outcome: RunOutcome) -> Option<Self> {
        let binding = run.binding_snapshot.as_ref()?;
        if binding.runtime == RuntimeKind::Fake {
            return None;
        }
        Some(RunEvidenceUpdate {
            binding_id: binding.id.clone(),
            version: binding.runtime_version.clone(),
            model_id: binding.model_id.clone(),
            outcome,
        })
    }
}

impl MissionService {
    /// Best-effort, post-commit. Never called from inside a mission
    /// transaction, and it only ever writes `local_evidence` — no Run, Task or
    /// Mission field is reachable from here.
    pub(super) fn record_run_evidence(&self, update: &RunEvidenceUpdate) {
        let Some(version) = update.version.as_deref() else {
            return;
        };
        // One retry: the only expected loser here is a concurrent
        // `binding.save`/`binding.probe` on the same document, and re-reading
        // it is enough to apply the same counter to the newer revision.
        for attempt in 0..2 {
            match self.apply_run_evidence(update, version) {
                Ok(()) => return,
                Err(error) if error.code == MissionErrorCode::RevisionConflict && attempt == 0 => {}
                Err(error) => {
                    tracing::warn!(
                        code = ?error.code,
                        binding = %update.binding_id,
                        "local run evidence was not recorded"
                    );
                    return;
                }
            }
        }
    }

    fn apply_run_evidence(
        &self,
        update: &RunEvidenceUpdate,
        version: &str,
    ) -> Result<(), MissionRpcError> {
        let Some(document) = self.current_binding_document(&update.binding_id)? else {
            // The connection was deleted while the Run finished.
            return Ok(());
        };
        let binding: Binding = serde_json::from_value(document.clone()).map_err(|_| {
            MissionRpcError::new(MissionErrorCode::Internal, "invalid stored binding")
        })?;
        if binding.runtime == RuntimeKind::Fake {
            return Ok(());
        }
        let os = std::env::consts::OS;
        // The counters describe one OS, one version and one model. Anything
        // else starts a fresh record rather than pooling unrelated runs — and
        // a probe belongs to the version it measured, so it is never carried
        // onto a different one.
        let mut evidence = match binding.local_evidence {
            Some(prior) if prior.os == os && prior.version == version => {
                if prior.model_id == update.model_id {
                    prior
                } else {
                    LocalEvidence {
                        model_id: update.model_id.clone(),
                        runs: LocalRunEvidence::default(),
                        ..prior
                    }
                }
            }
            _ => LocalEvidence {
                os: os.to_owned(),
                version: version.to_owned(),
                model_id: update.model_id.clone(),
                probed_at: None,
                probe: None,
                runs: LocalRunEvidence::default(),
            },
        };
        let runs = &mut evidence.runs;
        match update.outcome {
            RunOutcome::SucceededReadOnly => {
                runs.succeeded_read_only = runs.succeeded_read_only.saturating_add(1);
            }
            RunOutcome::SucceededWrite => {
                runs.succeeded_write = runs.succeeded_write.saturating_add(1);
            }
            RunOutcome::Cancelled => runs.cancelled = runs.cancelled.saturating_add(1),
            RunOutcome::InvalidResult => {
                runs.invalid_result = runs.invalid_result.saturating_add(1);
            }
        }
        let now = self.now();
        runs.last_at = Some(now.clone());
        // Written into the stored document rather than a re-serialized
        // `Binding` so the private installation observation beside it (and any
        // field a newer daemon wrote) survives this save untouched.
        let mut document = document;
        document["local_evidence"] =
            serde_json::to_value(&evidence).expect("local evidence serializes");
        self.storage
            .save_mission_binding(
                Id::generate(),
                methods::BINDING_RUN_EVIDENCE,
                &Self::fingerprint(methods::BINDING_RUN_EVIDENCE, &document),
                binding.revision.get(),
                document,
                now,
            )
            .map_err(Self::store_error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::fake::fake_binding;
    use crate::mission::artifacts::ArtifactStore;
    use std::sync::Arc;
    use term_contracts::mission::types::LocalProbeReport;

    /// A saved connection plus the service that owns it. Real SQLite, like the
    /// other mission suites — the CAS this module relies on is the thing under
    /// test.
    fn rig(binding: &Binding) -> (tempfile::TempDir, MissionService) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(term_storage::Storage::open(dir.path().join("state.db")).unwrap());
        let service = MissionService::new(
            storage.clone(),
            ArtifactStore::new(storage.clone(), dir.path().join("artifacts")),
        );
        storage
            .save_mission_binding(
                Id::generate(),
                "fixture",
                &"a".repeat(64),
                0,
                serde_json::to_value(binding).unwrap(),
                "2026-09-19T00:00:00Z".into(),
            )
            .unwrap();
        (dir, service)
    }

    fn codex_binding() -> Binding {
        let mut binding = fake_binding();
        binding.runtime = RuntimeKind::Codex;
        binding.provider_id = "openai".into();
        binding.model_id = "gpt-fixture".into();
        binding.runtime_version = Some("0.154.0".into());
        binding.revision = term_contracts::ids::U64String::new(0).unwrap();
        binding.local_evidence = None;
        binding
    }

    fn update(binding: &Binding, outcome: RunOutcome) -> RunEvidenceUpdate {
        RunEvidenceUpdate {
            binding_id: binding.id.clone(),
            version: binding.runtime_version.clone(),
            model_id: binding.model_id.clone(),
            outcome,
        }
    }

    fn stored(service: &MissionService, id: &Id) -> Binding {
        serde_json::from_value(
            service
                .current_binding_document(id)
                .unwrap()
                .expect("binding still stored"),
        )
        .unwrap()
    }

    #[test]
    fn every_outcome_lands_on_its_own_counter_and_stamps_the_observation() {
        let binding = codex_binding();
        let (_dir, service) = rig(&binding);
        for outcome in [
            RunOutcome::SucceededReadOnly,
            RunOutcome::SucceededReadOnly,
            RunOutcome::SucceededWrite,
            RunOutcome::Cancelled,
            RunOutcome::InvalidResult,
        ] {
            service.record_run_evidence(&update(&binding, outcome));
        }
        let evidence = stored(&service, &binding.id).local_evidence.unwrap();
        assert_eq!(evidence.os, std::env::consts::OS);
        assert_eq!(evidence.version, "0.154.0");
        assert_eq!(evidence.model_id, "gpt-fixture");
        assert_eq!(evidence.runs.succeeded_read_only, 2);
        assert_eq!(evidence.runs.succeeded_write, 1);
        assert_eq!(evidence.runs.cancelled, 1);
        assert_eq!(evidence.runs.invalid_result, 1);
        assert!(evidence.runs.last_at.is_some());
        assert!(
            evidence.probe.is_none() && evidence.probed_at.is_none(),
            "a Run observes; it does not claim to have run the self-check"
        );
    }

    #[test]
    fn a_probe_for_this_version_keeps_its_report_while_another_version_starts_over() {
        let mut binding = codex_binding();
        binding.local_evidence = Some(LocalEvidence {
            os: std::env::consts::OS.into(),
            version: "0.154.0".into(),
            model_id: "gpt-fixture".into(),
            probed_at: Some("2026-09-19T00:00:00Z".into()),
            probe: Some(LocalProbeReport {
                protocol_ok: true,
                sandbox_cases_passed: Some(12),
                sandbox_cases_total: Some(12),
                model_listed: Some(true),
                failures: vec![],
            }),
            runs: LocalRunEvidence {
                succeeded_read_only: 2,
                ..Default::default()
            },
        });
        let (_dir, service) = rig(&binding);
        service.record_run_evidence(&update(&binding, RunOutcome::SucceededWrite));
        let evidence = stored(&service, &binding.id).local_evidence.unwrap();
        assert!(evidence.probe.is_some(), "the measurement still stands");
        assert_eq!(evidence.runs.succeeded_read_only, 2);
        assert_eq!(evidence.runs.succeeded_write, 1);

        // The user updated the CLI: nothing measured or observed for the old
        // version carries over to the new one.
        let mut newer = update(&binding, RunOutcome::SucceededReadOnly);
        newer.version = Some("0.155.0".into());
        service.record_run_evidence(&newer);
        let evidence = stored(&service, &binding.id).local_evidence.unwrap();
        assert_eq!(evidence.version, "0.155.0");
        assert!(evidence.probe.is_none());
        assert_eq!(evidence.runs.succeeded_read_only, 1);
        assert_eq!(evidence.runs.succeeded_write, 0);
    }

    #[test]
    fn another_model_on_the_same_installation_restarts_the_counters_only() {
        let mut binding = codex_binding();
        binding.local_evidence = Some(LocalEvidence {
            os: std::env::consts::OS.into(),
            version: "0.154.0".into(),
            model_id: "gpt-fixture".into(),
            probed_at: Some("2026-09-19T00:00:00Z".into()),
            probe: Some(LocalProbeReport {
                protocol_ok: true,
                sandbox_cases_passed: None,
                sandbox_cases_total: None,
                model_listed: None,
                failures: vec![],
            }),
            runs: LocalRunEvidence {
                succeeded_read_only: 3,
                succeeded_write: 3,
                ..Default::default()
            },
        });
        let (_dir, service) = rig(&binding);
        let mut other = update(&binding, RunOutcome::SucceededReadOnly);
        other.model_id = "gpt-other".into();
        service.record_run_evidence(&other);
        let evidence = stored(&service, &binding.id).local_evidence.unwrap();
        assert_eq!(evidence.model_id, "gpt-other");
        assert_eq!(evidence.runs.succeeded_read_only, 1);
        assert_eq!(
            evidence.runs.succeeded_write, 0,
            "another model's successes are not this model's"
        );
        assert!(
            evidence.probe.is_some(),
            "the protocol self-check is a property of the CLI, not of the model"
        );
    }

    #[test]
    fn a_run_without_a_version_or_on_the_fake_runtime_records_nothing() {
        let binding = codex_binding();
        let (_dir, service) = rig(&binding);
        let mut unversioned = update(&binding, RunOutcome::SucceededWrite);
        unversioned.version = None;
        service.record_run_evidence(&unversioned);
        assert!(stored(&service, &binding.id).local_evidence.is_none());

        let fake = fake_binding();
        let (_fake_dir, fake_service) = rig(&fake);
        fake_service.record_run_evidence(&RunEvidenceUpdate {
            binding_id: fake.id.clone(),
            version: Some("fixture-v1".into()),
            model_id: fake.model_id.clone(),
            outcome: RunOutcome::SucceededWrite,
        });
        assert!(stored(&fake_service, &fake.id).local_evidence.is_none());
        assert!(
            RunEvidenceUpdate::for_run(
                &finished_run(Some(fake.clone())),
                RunOutcome::SucceededReadOnly
            )
            .is_none(),
            "the fixture runtime is never a compatibility observation"
        );
        assert!(
            RunEvidenceUpdate::for_run(&finished_run(None), RunOutcome::SucceededReadOnly)
                .is_none(),
            "a deterministic Run has no CLI to learn about"
        );
        let observed =
            RunEvidenceUpdate::for_run(&finished_run(Some(binding.clone())), RunOutcome::Cancelled)
                .expect("a CLI-backed Run is an observation");
        assert_eq!(observed, update(&binding, RunOutcome::Cancelled));
    }

    #[test]
    fn a_deleted_connection_is_dropped_and_a_concurrent_save_keeps_the_counter() {
        let binding = codex_binding();
        let (_dir, service) = rig(&binding);
        // Deleted, or never this daemon's, while the Run was finishing:
        // nothing to record and nothing to fail.
        let mut gone = update(&binding, RunOutcome::Cancelled);
        gone.binding_id = Id::generate();
        service.record_run_evidence(&gone);
        assert!(stored(&service, &binding.id).local_evidence.is_none());

        // Another writer moved the connection on between two Runs. The
        // observation is always applied to the revision that is current when
        // it is written, so a settings edit neither loses a counter nor wins
        // one back.
        service.record_run_evidence(&update(&binding, RunOutcome::Cancelled));
        let document = service
            .current_binding_document(&binding.id)
            .unwrap()
            .unwrap();
        let revision: u64 = document["revision"].as_str().unwrap().parse().unwrap();
        let bumped = service
            .storage
            .save_mission_binding(
                Id::generate(),
                "fixture-concurrent",
                &"b".repeat(64),
                revision,
                document,
                "2026-09-19T00:00:01Z".into(),
            )
            .unwrap();
        assert_eq!(bumped.revision, revision + 1);
        service.record_run_evidence(&update(&binding, RunOutcome::Cancelled));
        let after = stored(&service, &binding.id);
        assert_eq!(after.local_evidence.unwrap().runs.cancelled, 2);
        assert_eq!(after.revision.get(), revision + 2);
    }

    fn finished_run(binding: Option<Binding>) -> Run {
        use term_contracts::ids::U64String;
        use term_contracts::mission::types::{ArtifactRef, RunDispatchState, RunState};
        Run {
            id: Id::generate(),
            mission_id: Id::generate(),
            task_id: Id::generate(),
            attempt: 1,
            state: RunState::Succeeded,
            requested_model: binding.as_ref().map(|b| b.model_id.clone()),
            binding_snapshot: binding,
            observed_model: None,
            provider_session_id: None,
            provider_turn_id: None,
            exec_id: None,
            pty_session_id: None,
            workspace_id: None,
            fencing_token: U64String::new(1).unwrap(),
            dispatch_state: RunDispatchState::Acknowledged,
            context_ref: ArtifactRef {
                id: Id::generate(),
                sha256: "0".repeat(64),
                bytes: U64String::new(0).unwrap(),
                media_type: "text/plain".into(),
            },
            result_ref: None,
            usage: term_contracts::mission::validation::unknown_usage(),
            last_activity_at: None,
            active_time_ms: U64String::new(0).unwrap(),
            started_at: None,
            ended_at: None,
            failure_code: None,
            reconciliation_ref: None,
            reconciliation_kind: None,
            rate_limit: None,
            retry_evidence: None,
        }
    }
}

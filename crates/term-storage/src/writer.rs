//! Single serialized writer: one thread owns the write connection; every
//! write travels as a [`WriterCommand`] over an mpsc channel and returns its
//! result on a per-command reply channel (spec `01-contracts.md` §7).

use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;

use rusqlite::Connection;
use term_contracts::agent_session::AgentSessionRecord;
use term_contracts::ids::{SessionId, WorkloadId};
use term_contracts::snapshot::QueueReason;
use term_contracts::state::WorkloadState;
use term_contracts::workload::ProcessOwnership;

use crate::error::StorageResult;
use crate::mission::types::{AppliedTransition, MissionStoreResult};
use crate::ops;
use crate::types::{AgentSessionUpsert, LaunchIntent, LaunchIntentOutcome};

pub(crate) type ReplySender<T> = Sender<StorageResult<T>>;

#[derive(Debug)]
pub(crate) enum Flag {
    CancelRequested,
    RootExited,
}

pub(crate) enum WriterCommand {
    RecordLaunchIntent {
        intent: Box<LaunchIntent>,
        reply: ReplySender<LaunchIntentOutcome>,
    },
    MissionApply {
        transition: Box<crate::mission::types::ApplyMissionTransition>,
        reply: Sender<MissionStoreResult<AppliedTransition>>,
    },
    MissionPruneRetention {
        event_tail_per_mission: i64,
        request_retention_days: u32,
        reply: Sender<MissionStoreResult<crate::mission::ops::PrunedRows>>,
    },
    Vacuum {
        reply: ReplySender<()>,
    },
    MissionSaveBinding {
        request_id: term_contracts::mission::types::Id,
        method: String,
        fingerprint: String,
        expected_revision: u64,
        document: serde_json::Value,
        created_at: String,
        reply: Sender<crate::mission::ops::SavedConfigResult>,
    },
    MissionSaveConfig {
        request_id: term_contracts::mission::types::Id,
        method: String,
        fingerprint: String,
        kind: String,
        expected_revision: u64,
        document: serde_json::Value,
        created_at: String,
        reply: Sender<crate::mission::ops::SavedConfigResult>,
    },
    MissionArtifactUpload {
        op: crate::mission::artifacts::UploadOp,
        reply: Sender<crate::mission::types::MissionStoreResult<()>>,
    },
    MissionArtifactCommit {
        upload_id: term_contracts::mission::types::Id,
        artifact: crate::mission::artifacts::ArtifactRow,
        reply: Sender<
            crate::mission::types::MissionStoreResult<crate::mission::artifacts::ArtifactRow>,
        >,
    },
    MissionOutboxState {
        id: term_contracts::mission::types::Id,
        state: crate::mission::types::OutboxState,
        updated_at: String,
        reply: Sender<MissionStoreResult<()>>,
    },
    Transition {
        workload_id: WorkloadId,
        to: WorkloadState,
        exit_code: Option<i32>,
        reason_code: Option<String>,
        reply: ReplySender<()>,
    },
    SaveGroupIdentity {
        ownership: ProcessOwnership,
        reply: ReplySender<()>,
    },
    SetQueueReason {
        workload_id: WorkloadId,
        reason: Option<QueueReason>,
        reply: ReplySender<()>,
    },
    SetFlag {
        workload_id: WorkloadId,
        flag: Flag,
        value: bool,
        reply: ReplySender<()>,
    },
    UpdateSessionProgress {
        session_id: SessionId,
        last_seq: u64,
        journal_bytes: u64,
        reply: ReplySender<()>,
    },
    MarkJournalDeleted {
        session_id: SessionId,
        reply: ReplySender<()>,
    },
    PruneLifecycleEvents {
        keep_recent: i64,
        reply: ReplySender<()>,
    },
    UpsertAgentSession {
        upsert: Box<AgentSessionUpsert>,
        reply: ReplySender<AgentSessionRecord>,
    },
    EndAgentSessionsForWorkload {
        workload_id: WorkloadId,
        reason: String,
        reply: ReplySender<usize>,
    },
    EndAgentSession {
        workload_id: WorkloadId,
        agent: String,
        agent_session_id: String,
        reason: String,
        reply: ReplySender<bool>,
    },
    ForgetAgentSession {
        id: String,
        reply: ReplySender<bool>,
    },
    PruneAgentSessions {
        max_age_days: u32,
        keep_at_most: u32,
        reply: ReplySender<usize>,
    },
    Shutdown {
        reply: Sender<()>,
    },
}

pub(crate) fn spawn(conn: Connection, commands: Receiver<WriterCommand>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("term-storage-writer".into())
        .spawn(move || writer_loop(conn, commands))
        .expect("storage writer thread spawns")
}

fn writer_loop(mut conn: Connection, commands: Receiver<WriterCommand>) {
    while let Ok(command) = commands.recv() {
        match command {
            WriterCommand::RecordLaunchIntent { intent, reply } => {
                let _ = reply.send(ops::record_launch_intent(&mut conn, &intent));
            }
            WriterCommand::MissionApply { transition, reply } => {
                let _ = reply.send(crate::mission::ops::apply(&mut conn, &transition));
            }
            WriterCommand::MissionPruneRetention {
                event_tail_per_mission,
                request_retention_days,
                reply,
            } => {
                let _ = reply.send(crate::mission::ops::prune_retention(
                    &mut conn,
                    event_tail_per_mission,
                    request_retention_days,
                ));
            }
            WriterCommand::Vacuum { reply } => {
                // VACUUM refuses to run inside a transaction; between commands
                // the writer connection sits in autocommit, which is what it
                // needs. A concurrent reader snapshot surfaces as an error the
                // caller can retry on the next sweep.
                let _ = reply.send(
                    conn.execute("VACUUM", [])
                        .map(|_| ())
                        .map_err(crate::StorageError::from),
                );
            }
            WriterCommand::MissionSaveBinding {
                request_id,
                method,
                fingerprint,
                expected_revision,
                document,
                created_at,
                reply,
            } => {
                let _ = reply.send(crate::mission::ops::save_binding(
                    &mut conn,
                    &request_id,
                    &method,
                    &fingerprint,
                    expected_revision,
                    document,
                    &created_at,
                ));
            }
            WriterCommand::MissionSaveConfig {
                request_id,
                method,
                fingerprint,
                kind,
                expected_revision,
                document,
                created_at,
                reply,
            } => {
                let _ = reply.send(crate::mission::ops::save_config(
                    &mut conn,
                    &crate::mission::ops::SaveConfig {
                        request_id: &request_id,
                        method: &method,
                        fingerprint: &fingerprint,
                        kind: &kind,
                        expected_revision,
                        created_at: &created_at,
                    },
                    document,
                ));
            }
            WriterCommand::MissionArtifactUpload { op, reply } => {
                let _ = reply.send(crate::mission::artifacts::apply_upload_op(&mut conn, &op));
            }
            WriterCommand::MissionArtifactCommit {
                upload_id,
                artifact,
                reply,
            } => {
                let _ = reply.send(crate::mission::artifacts::commit_upload(
                    &mut conn, &upload_id, &artifact,
                ));
            }
            WriterCommand::MissionOutboxState {
                id,
                state,
                updated_at,
                reply,
            } => {
                let _ = reply.send(crate::mission::queries::set_outbox_state(
                    &mut conn,
                    &id,
                    state,
                    &updated_at,
                ));
            }
            WriterCommand::Transition {
                workload_id,
                to,
                exit_code,
                reason_code,
                reply,
            } => {
                let _ = reply.send(ops::transition_workload(
                    &mut conn,
                    &workload_id,
                    to,
                    exit_code,
                    reason_code.as_deref(),
                ));
            }
            WriterCommand::SaveGroupIdentity { ownership, reply } => {
                let _ = reply.send(ops::save_group_identity(&mut conn, &ownership));
            }
            WriterCommand::SetQueueReason {
                workload_id,
                reason,
                reply,
            } => {
                let _ = reply.send(ops::set_queue_reason(&mut conn, &workload_id, reason));
            }
            WriterCommand::SetFlag {
                workload_id,
                flag,
                value,
                reply,
            } => {
                let column = match flag {
                    Flag::CancelRequested => "cancel_requested",
                    Flag::RootExited => "root_exited",
                };
                let _ = reply.send(ops::set_bool_flag(&mut conn, &workload_id, column, value));
            }
            WriterCommand::UpdateSessionProgress {
                session_id,
                last_seq,
                journal_bytes,
                reply,
            } => {
                let _ = reply.send(ops::update_session_progress(
                    &mut conn,
                    &session_id,
                    last_seq,
                    journal_bytes,
                ));
            }
            WriterCommand::MarkJournalDeleted { session_id, reply } => {
                let _ = reply.send(ops::mark_journal_deleted(&mut conn, &session_id));
            }
            WriterCommand::PruneLifecycleEvents { keep_recent, reply } => {
                let _ = reply.send(ops::prune_lifecycle_events(&mut conn, keep_recent));
            }
            WriterCommand::UpsertAgentSession { upsert, reply } => {
                let _ = reply.send(ops::upsert_agent_session(&mut conn, &upsert));
            }
            WriterCommand::EndAgentSessionsForWorkload {
                workload_id,
                reason,
                reply,
            } => {
                let _ = reply.send(ops::end_agent_sessions_for_workload(
                    &mut conn,
                    &workload_id,
                    &reason,
                ));
            }
            WriterCommand::EndAgentSession {
                workload_id,
                agent,
                agent_session_id,
                reason,
                reply,
            } => {
                let _ = reply.send(ops::end_agent_session(
                    &mut conn,
                    &workload_id,
                    &agent,
                    &agent_session_id,
                    &reason,
                ));
            }
            WriterCommand::ForgetAgentSession { id, reply } => {
                let _ = reply.send(ops::forget_agent_session(&mut conn, &id));
            }
            WriterCommand::PruneAgentSessions {
                max_age_days,
                keep_at_most,
                reply,
            } => {
                let _ = reply.send(ops::prune_agent_sessions(
                    &mut conn,
                    max_age_days,
                    keep_at_most,
                ));
            }
            WriterCommand::Shutdown { reply } => {
                let _ = reply.send(());
                break;
            }
        }
    }
}

//! Monotonic elapsed time, committed with the state transition it measures.
//! Anchors are process-local: restart never infers elapsed time from UTC.
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use term_contracts::{
    ids::U64String,
    mission::{rpc::methods, types::*, MissionRpcError},
};
use term_storage::mission::types::{
    AppliedTransition, ApplyMissionTransition, ApplyMode, MissionStoreError,
};

use super::{service::MissionService, workflow};

const CHECKPOINT: Duration = Duration::from_secs(1);
/// A quiet mission — no run clock and no meaningful commit for this window —
/// persists its wall clock at [`IDLE_CHECKPOINT`] instead of [`CHECKPOINT`].
/// The in-memory anchor keeps `effective_active_time` exact for admission, so
/// only crash persistence granularity relaxes (bounded by the idle interval).
const MEANINGFUL_WINDOW: Duration = Duration::from_secs(60);
const IDLE_CHECKPOINT: Duration = Duration::from_secs(60);

/// Engine commits that only move elapsed time or activity timestamps. They
/// still advance `revision`/event seq, but never `semantic_revision`.
pub(super) const TIME_CHECKPOINT_METHOD: &str = "engine.time_checkpoint";
pub(super) const ACTIVITY_METHOD: &str = "engine.activity";

pub(super) fn is_housekeeping(method: &str) -> bool {
    matches!(method, TIME_CHECKPOINT_METHOD | ACTIVITY_METHOD)
}

/// User RPC mutations whose expected_revision may predate housekeeping-only
/// commits. Engine/actor commits keep their exact CAS.
fn tolerates_housekeeping(method: &str) -> bool {
    matches!(
        method,
        methods::MISSION_CONTROL
            | methods::MISSION_MESSAGE
            | methods::MISSION_TASK_CONTROL
            | methods::MISSION_DECISION_ANSWER
            | methods::MISSION_ACCEPT
            | methods::MISSION_PLAN_APPLY
            | methods::MISSION_POLICY_UPDATE
            | methods::MISSION_FINDING_RESOLVE
            | methods::MISSION_RUN_ATTEST_EXITED
    )
}

/// Last revision with a meaningful change. Legacy documents have none
/// recorded, so every earlier revision counts as meaningful.
pub(super) fn semantic_revision(mission: &Mission) -> u64 {
    mission
        .semantic_revision
        .as_ref()
        .map_or(mission.revision.get(), U64String::get)
        .min(mission.revision.get())
}

/// A client's expected revision is current when only housekeeping commits
/// happened after it: `semantic_revision <= expected <= revision`.
pub(super) fn revision_accepts(mission: &Mission, expected: u64) -> bool {
    semantic_revision(mission) <= expected && expected <= mission.revision.get()
}

/// Rebase a user mutation built on an older housekeeping-only revision onto
/// the stored one. Housekeeping fields never move backwards.
fn rebase_housekeeping(
    upserts: &mut [Entity],
    stored: &Mission,
    stored_runs: &HashMap<Id, &Run>,
) -> Result<(), MissionStoreError> {
    let next = U64String::new(stored.revision.get().saturating_add(1))
        .map_err(|e| MissionStoreError::InvalidArgument(e.to_string()))?;
    for entity in upserts {
        match entity {
            Entity::Mission(mission) => {
                mission.revision = next.clone();
                mission.active_time_ms = mission
                    .active_time_ms
                    .clone()
                    .max(stored.active_time_ms.clone());
            }
            Entity::Run(run) => {
                if let Some(old) = stored_runs
                    .get(&run.id)
                    .filter(|old| old.fencing_token == run.fencing_token)
                {
                    run.active_time_ms = run.active_time_ms.clone().max(old.active_time_ms.clone());
                    run.last_activity_at = run
                        .last_activity_at
                        .clone()
                        .max(old.last_activity_at.clone());
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[derive(Clone)]
struct Counter {
    anchor: Instant,
    base: u64,
}

impl Counter {
    fn value(&self, now: Instant) -> U64String {
        // Keep the original anchor across checkpoints, retaining sub-ms time.
        let elapsed = now.saturating_duration_since(self.anchor).as_millis();
        U64String::new((self.base as u128 + elapsed).min(i64::MAX as u128) as u64)
            .expect("clamped SQLite integer")
    }
}

#[derive(Clone)]
struct MissionClock {
    mission: Option<Counter>,
    runs: HashMap<Id, (U64String, Counter)>,
    persisted_at: Instant,
    /// Last non-housekeeping commit. A quiet mission (no run clocks, nothing
    /// meaningful for [`MEANINGFUL_WINDOW`]) decays to [`IDLE_CHECKPOINT`] so a
    /// decision-blocked or otherwise stuck mission stops rewriting its rows
    /// once a second. Restart re-anchors to "now": the grace window keeps the
    /// first minute at the fast cadence before decaying.
    last_meaningful_at: Option<Instant>,
}

#[derive(Default)]
pub(super) struct MissionClocks(HashMap<Id, MissionClock>);

/// Persistence cadence for one mission clock. Run clocks need second
/// granularity (their budget is per execution), and a mission that just made
/// a meaningful change may be about to make another; a mission that has been
/// quiet for [`MEANINGFUL_WINDOW`] with nothing executing only needs its wall
/// clock persisted once per [`IDLE_CHECKPOINT`] — the churn of a stuck,
/// decision-blocked mission drops from one commit/second to one per minute.
fn checkpoint_interval(clock: &MissionClock, now: Instant) -> Duration {
    if !clock.runs.is_empty() {
        return CHECKPOINT;
    }
    match clock
        .last_meaningful_at
        .map(|t| now.saturating_duration_since(t))
    {
        Some(quiet_for) if quiet_for >= MEANINGFUL_WINDOW => IDLE_CHECKPOINT,
        _ => CHECKPOINT,
    }
}

/// Waiting for the user's acceptance is not execution time: no dispatch is
/// possible in that phase, so it never consumes the active-time budget.
fn mission_active(mission: &Mission) -> bool {
    matches!(mission.state, MissionState::Running | MissionState::Pausing)
        && mission.phase != Phase::AwaitingAcceptance
}

/// `AwaitingInput` is the run's half of the same rule: the provider stopped and
/// is waiting for a person to answer a blocking decision, so nothing of ours is
/// executing. Charging that to the budget bills the user for their own reading
/// time, and — because the clock is what makes the run dirty every
/// [`CHECKPOINT`] — it also rewrites the run and its mission once a second for
/// as long as the question stays unanswered, with a revision, an event row and
/// a client broadcast each time.
fn run_active(run: &Run) -> bool {
    run.started_at.is_some()
        && run.ended_at.is_none()
        && matches!(
            run.state,
            RunState::Starting | RunState::Running | RunState::Stopping
        )
}

impl MissionClock {
    fn observe(mission: Option<&Mission>, now: Instant) -> Self {
        Self {
            mission: mission.filter(|m| mission_active(m)).map(|m| Counter {
                anchor: now,
                base: m.active_time_ms.get(),
            }),
            // Existing Runs belong to a previous owner. Only a committed start
            // in this service creates a Run clock; Unknown stays unestimated.
            runs: HashMap::new(),
            persisted_at: now,
            last_meaningful_at: Some(now),
        }
    }
}

impl MissionService {
    /// The single production storage boundary. The lock orders timing samples
    /// with concurrent RPC/actor/verifier commits, not with external work.
    pub(super) fn apply_timed_transition(
        &self,
        mut transition: ApplyMissionTransition,
    ) -> Result<AppliedTransition, MissionStoreError> {
        let mut clocks = self.timing.lock().unwrap_or_else(|p| p.into_inner());
        let snapshot = match transition.mode {
            ApplyMode::Create => None,
            ApplyMode::Mutate { .. } => Some(
                self.storage
                    .mission_snapshot(&transition.mission_id)?
                    .ok_or_else(|| MissionStoreError::NotFound {
                        what: "mission",
                        id: transition.mission_id.to_string(),
                    })?,
            ),
        };
        let current: Vec<Entity> = snapshot
            .as_ref()
            .map(|s| s.entities.clone())
            .unwrap_or_default();
        let old_mission = current.iter().find_map(|entity| match entity {
            Entity::Mission(m) => Some(m.as_ref()),
            _ => None,
        });
        let now = (self.monotonic)();
        let mut next_clock = clocks
            .0
            .get(&transition.mission_id)
            .cloned()
            .unwrap_or_else(|| MissionClock::observe(old_mission, now));
        let old_runs: HashMap<_, _> = current
            .iter()
            .filter_map(|entity| match entity {
                Entity::Run(run) => Some((run.id.clone(), run.as_ref())),
                _ => None,
            })
            .collect();
        next_clock.runs.retain(|id, (token, _)| {
            old_runs
                .get(id)
                .is_some_and(|run| run.fencing_token == *token && run_active(run))
        });

        // A user mutation may name a revision older than the stored one when
        // only housekeeping commits happened since. Every commit serializes on
        // this lock, so rebasing onto the stored revision keeps the storage CAS
        // exact; a meaningful commit in between still conflicts.
        let mut rebased_from = None;
        if let (ApplyMode::Mutate { expected_revision }, Some(old)) = (transition.mode, old_mission)
        {
            if expected_revision != old.revision.get()
                && tolerates_housekeeping(&transition.method)
                && revision_accepts(old, expected_revision)
            {
                rebase_housekeeping(&mut transition.upserts, old, &old_runs)?;
                transition.mode = ApplyMode::Mutate {
                    expected_revision: old.revision.get(),
                };
                rebased_from = Some(expected_revision);
            }
        }

        // Some engine mutations previously omitted the mission projection.
        // Every event must carry its revision and elapsed-time projection.
        if !transition
            .upserts
            .iter()
            .any(|e| matches!(e, Entity::Mission(_)))
        {
            if let Some(old) = old_mission {
                let mut mission = old.clone();
                mission.revision = U64String::new(old.revision.get().saturating_add(1))
                    .map_err(|e| MissionStoreError::InvalidArgument(e.to_string()))?;
                mission.updated_at = transition.created_at.clone();
                transition
                    .upserts
                    .insert(0, Entity::Mission(Box::new(mission)));
            }
        }
        for (id, (token, counter)) in &next_clock.runs {
            let Some(old) = old_runs
                .get(id)
                .filter(|r| r.fencing_token == *token && run_active(r))
            else {
                continue;
            };
            if !transition
                .upserts
                .iter()
                .any(|e| matches!(e, Entity::Run(r) if &r.id == id))
            {
                let elapsed = counter.value(now).max(old.active_time_ms.clone());
                if elapsed != old.active_time_ms {
                    let mut run = (*old).clone();
                    run.active_time_ms = elapsed;
                    transition.upserts.push(Entity::Run(Box::new(run)));
                }
            }
        }
        for entity in &mut transition.upserts {
            match entity {
                Entity::Mission(mission) => {
                    if let Some(counter) = &next_clock.mission {
                        mission.active_time_ms =
                            counter.value(now).max(mission.active_time_ms.clone());
                    }
                    if !mission_active(mission) {
                        next_clock.mission = None;
                    } else if next_clock.mission.is_none() {
                        next_clock.mission = Some(Counter {
                            anchor: now,
                            base: mission.active_time_ms.get(),
                        });
                    }
                }
                Entity::Run(run) => {
                    if let Some((_, counter)) = next_clock.runs.get(&run.id) {
                        run.active_time_ms = counter.value(now).max(run.active_time_ms.clone());
                    }
                    let same_owner = next_clock
                        .runs
                        .get(&run.id)
                        .is_some_and(|(token, _)| *token == run.fencing_token);
                    if !run_active(run) || !same_owner {
                        next_clock.runs.remove(&run.id);
                    }
                    let new_start = run_active(run)
                        && old_runs
                            .get(&run.id)
                            .is_none_or(|old| old.started_at.is_none());
                    if new_start {
                        next_clock.runs.entry(run.id.clone()).or_insert_with(|| {
                            (
                                run.fencing_token.clone(),
                                Counter {
                                    anchor: now,
                                    base: run.active_time_ms.get(),
                                },
                            )
                        });
                    }
                }
                _ => {}
            }
        }
        // Housekeeping keeps the last meaningful revision; everything else
        // (including create) marks its own revision as meaningful.
        let committed_revision = match transition.mode {
            ApplyMode::Create => 1,
            ApplyMode::Mutate { expected_revision } => expected_revision.saturating_add(1),
        };
        let semantic = match old_mission {
            Some(old) if is_housekeeping(&transition.method) => semantic_revision(old),
            _ => committed_revision,
        };
        let semantic = U64String::new(semantic)
            .map_err(|e| MissionStoreError::InvalidArgument(e.to_string()))?;
        for entity in &mut transition.upserts {
            if let Entity::Mission(mission) = entity {
                mission.semantic_revision = Some(semantic.clone());
            }
        }
        let id = transition.mission_id.clone();
        let housekeeping = is_housekeeping(&transition.method);
        let result = self
            .storage
            .apply_mission_transition(transition)
            .map_err(|error| match (error, rebased_from) {
                (
                    MissionStoreError::RevisionConflict {
                        current_revision, ..
                    },
                    Some(expected),
                ) => MissionStoreError::RevisionConflict {
                    expected_revision: expected,
                    current_revision,
                },
                (error, _) => error,
            })?;
        if !result.replayed {
            next_clock.persisted_at = now;
            if !housekeeping {
                next_clock.last_meaningful_at = Some(now);
            }
            if next_clock.mission.is_none() && next_clock.runs.is_empty() {
                clocks.0.remove(&id);
            } else {
                clocks.0.insert(id, next_clock);
            }
        }
        Ok(result)
    }

    /// Admission uses the current monotonic total even between checkpoints.
    pub(super) fn effective_active_time(&self, mission: &Mission) -> U64String {
        let clocks = self.timing.lock().unwrap_or_else(|p| p.into_inner());
        clocks
            .0
            .get(&mission.id)
            .and_then(|clock| clock.mission.as_ref())
            .map(|counter| {
                counter
                    .value((self.monotonic)())
                    .max(mission.active_time_ms.clone())
            })
            .unwrap_or_else(|| mission.active_time_ms.clone())
    }

    /// Persist quiet missions too. CAS/storage failures retain the anchors;
    /// the next pass accounts the whole interval exactly once.
    pub(super) fn checkpoint_time(&self) -> Result<(), MissionRpcError> {
        self.persist_time(false)
    }

    pub(super) fn flush_time(&self) -> Result<(), MissionRpcError> {
        self.persist_time(true)
    }

    fn persist_time(&self, force: bool) -> Result<(), MissionRpcError> {
        let mut cursor = None;
        loop {
            let (missions, next) = self
                .storage
                .mission_list(cursor, 50, false)
                .map_err(Self::store_error)?;
            for mission in missions {
                let due = {
                    let mut clocks = self.timing.lock().unwrap_or_else(|p| p.into_inner());
                    let now = (self.monotonic)();
                    let current = self.read_mission(&mission.id)?;
                    if mission_active(&current) {
                        clocks
                            .0
                            .entry(current.id.clone())
                            .or_insert_with(|| MissionClock::observe(Some(&current), now));
                    }
                    clocks.0.get(&current.id).is_some_and(|clock| {
                        let interval = checkpoint_interval(clock, now);
                        let elapsed = now.saturating_duration_since(clock.persisted_at);
                        elapsed >= interval || (force && elapsed >= Duration::from_millis(1))
                    })
                };
                if due {
                    let current = workflow::load_entities(&self.storage, &mission.id)?.mission;
                    match self.commit_actor(current, TIME_CHECKPOINT_METHOD, vec![], vec![]) {
                        Err(e)
                            if e.code
                                == term_contracts::mission::MissionErrorCode::RevisionConflict => {}
                        other => other?,
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

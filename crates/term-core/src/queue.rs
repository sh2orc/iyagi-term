//! Bounded managed workload queue (spec `03-resources.md` §3).
//!
//! * capacity `limits.queued_workloads` (64) — the 65th enqueue is
//!   `QUEUE_FULL`;
//! * ordering: effective priority → queued_at → workload id (string
//!   compare) — all deterministic;
//! * aging: one priority level up per full `timing_ms.priority_aging`
//!   interval (5 min), but never above NORMAL(1) — 낮은 우선순위(2)가 오래
//!   기다리면 보통 요청과 같은 줄에 서지만, 높은 우선순위(0)를 앞지르지는
//!   않는다(P2-1). 원래 0인 요청은 그대로 0이다;
//! * head-of-line bypass: when the head waits on resources, the first entry
//!   the current conditions admit may start; skipped heads record their wait
//!   reason for the UI. 큰 요청이 [`HOL_BYPASS_LIMIT_MS`] 넘게 자원(메모리
//!   여유·CPU 슬롯·예약 예산)을 기다렸으면 더는 추월시키지 않는다 — 작은
//!   요청이 끝없이 앞질러 큰 요청이 굶는 것을 막는다.
//!
//! `pick_next` does **not** reserve anything: it hands the winner to the
//! caller, which pairs it with [`crate::reservation::ReservationLedger`].

use std::cmp::Ordering;
use std::sync::{Mutex, MutexGuard};

use term_contracts::defaults::Defaults;
use term_contracts::ids::{RequestId, WorkloadId};
use term_contracts::launch::Priority;
use term_contracts::snapshot::{QueueEntry, QueueReason};

use crate::clock::Clock;
use crate::error::CoreError;

/// 자원을 기다리는 머리 항목을 이보다 오래 추월시키지 않는다.
pub const HOL_BYPASS_LIMIT_MS: u64 = 10 * 60_000;

/// 에이징이 올려 줄 수 있는 가장 높은 우선순위(NORMAL).
pub const AGING_CEILING: u8 = 1;

/// Queue sizing from defaults: 64 entries, one aging step per 5 min.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueConfig {
    /// `limits.queued_workloads` (default 64).
    pub max_entries: u32,
    /// `timing_ms.priority_aging` (default 300_000).
    pub aging_step_ms: u64,
}

impl QueueConfig {
    pub fn from_defaults(defaults: &Defaults) -> Self {
        Self {
            max_entries: defaults.limits.queued_workloads,
            aging_step_ms: defaults.timing_ms.priority_aging,
        }
    }
}

/// Effective priority after aging: `steps = elapsed / aging_step_ms` full
/// intervals, one level up per step, but never above [`AGING_CEILING`]
/// (an original 0 stays 0). An `aging_step_ms` of 0 disables aging (no step
/// is ever complete).
pub fn effective_priority(
    priority: Priority,
    queued_at_ms: u64,
    now_ms: u64,
    aging_step_ms: u64,
) -> Priority {
    if aging_step_ms == 0 {
        return priority;
    }
    let elapsed = now_ms.saturating_sub(queued_at_ms);
    let steps = u8::try_from(elapsed / aging_step_ms).unwrap_or(u8::MAX);
    let ceiling = priority.0.min(AGING_CEILING);
    Priority(priority.0.saturating_sub(steps).max(ceiling))
}

/// 머리 항목이 이 사유로 오래 기다리면 뒤 항목의 추월을 막는다 — 요청
/// 크기에 따라 갈리는 자원 대기만 해당한다(압력·텔레메트리·동시 실행 수는
/// 모두에게 같아서 추월로 굶을 일이 없다).
fn size_dependent_wait(reason: QueueReason) -> bool {
    matches!(
        reason,
        QueueReason::WaitMemoryHeadroom
            | QueueReason::WaitCpuSlots
            | QueueReason::WaitReservationBudget
    )
}

#[derive(Debug, Clone)]
struct QueuedWorkload {
    workload_id: WorkloadId,
    request_id: RequestId,
    priority: Priority,
    queued_at_ms: u64,
    /// Last wait reason a scheduler pass recorded for this entry.
    wait_reason: Option<QueueReason>,
}

/// Read-only view handed to the admission closure during [`WorkloadQueue::pick_next`].
pub struct QueueCandidate<'a> {
    pub workload_id: &'a WorkloadId,
    pub request_id: &'a RequestId,
    /// Original priority at enqueue time.
    pub priority: Priority,
    /// Priority after aging, what the ordering actually uses.
    pub effective_priority: Priority,
    /// Monotonic enqueue time.
    pub queued_at_ms: u64,
    /// `now - queued_at` in monotonic milliseconds.
    pub waited_ms: u64,
}

/// A head entry the scheduler evaluated and bypassed, with its wait reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedEntry {
    pub workload_id: WorkloadId,
    pub request_id: RequestId,
    pub reason: QueueReason,
}

/// One scheduler pass over the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickOutcome {
    /// The highest-ordered entry the closure admitted, removed from the
    /// queue (`wait_reason = ADMIT`).
    pub picked: Option<QueueEntry>,
    /// Entries ahead of it that were bypassed, in queue order.
    pub skipped: Vec<SkippedEntry>,
}

pub struct WorkloadQueue<C: Clock> {
    config: QueueConfig,
    clock: C,
    state: Mutex<QueueState>,
}

#[derive(Default)]
struct QueueState {
    entries: Vec<QueuedWorkload>,
}

fn queue_ordering(
    a_eff: Priority,
    a_at: u64,
    a_id: &str,
    b_eff: Priority,
    b_at: u64,
    b_id: &str,
) -> Ordering {
    a_eff
        .0
        .cmp(&b_eff.0)
        .then_with(|| a_at.cmp(&b_at))
        .then_with(|| a_id.cmp(b_id))
}

impl<C: Clock> WorkloadQueue<C> {
    pub fn new(config: QueueConfig, clock: C) -> Self {
        Self {
            config,
            clock,
            state: Mutex::new(QueueState::default()),
        }
    }

    pub fn config(&self) -> &QueueConfig {
        &self.config
    }

    fn lock(&self) -> MutexGuard<'_, QueueState> {
        // Queue mutations are single Vec ops; recover a poisoned lock rather
        // than cascading one panicked owner into daemon-wide deadlock.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Enqueue at the tail. Fails with `QUEUE_FULL` at capacity and rejects a
    /// workload id that is already queued.
    pub fn enqueue(
        &self,
        workload_id: WorkloadId,
        request_id: RequestId,
        priority: Priority,
    ) -> Result<(), CoreError> {
        let mut state = self.lock();
        if state.entries.len() >= self.config.max_entries as usize {
            return Err(CoreError::QueueFull {
                len: state.entries.len(),
                capacity: self.config.max_entries,
            });
        }
        if state.entries.iter().any(|e| e.workload_id == workload_id) {
            return Err(CoreError::DuplicateQueueEntry { workload_id });
        }
        state.entries.push(QueuedWorkload {
            workload_id,
            request_id,
            priority,
            queued_at_ms: self.clock.now_ms(),
            wait_reason: None,
        });
        Ok(())
    }

    /// Remove a queued workload (cancel/terminate request). Returns whether
    /// an entry was actually removed.
    pub fn cancel(&self, workload_id: &WorkloadId) -> bool {
        let mut state = self.lock();
        let before = state.entries.len();
        state.entries.retain(|e| &e.workload_id != workload_id);
        before != state.entries.len()
    }

    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().entries.is_empty()
    }

    /// Current queue in scheduler order (effective priority → queued_at →
    /// id), each entry carrying its aging-adjusted priority and the wait
    /// reason recorded by the last scheduler pass.
    pub fn snapshot(&self) -> Vec<QueueEntry> {
        let now = self.clock.now_ms();
        let state = self.lock();
        let mut out: Vec<QueueEntry> = state
            .entries
            .iter()
            .map(|e| QueueEntry {
                workload_id: e.workload_id.clone(),
                request_id: e.request_id.clone(),
                priority: e.priority,
                effective_priority: effective_priority(
                    e.priority,
                    e.queued_at_ms,
                    now,
                    self.config.aging_step_ms,
                ),
                queued_at_ms: e.queued_at_ms,
                wait_reason: e.wait_reason,
            })
            .collect();
        out.sort_unstable_by(|a, b| {
            queue_ordering(
                a.effective_priority,
                a.queued_at_ms,
                a.workload_id.as_str(),
                b.effective_priority,
                b.queued_at_ms,
                b.workload_id.as_str(),
            )
        });
        out
    }

    /// Head-of-line bypass pass (spec `03-resources.md` §3). Visits entries
    /// in queue order, returns the first the closure reports `ADMIT` for
    /// (removed from the queue), and records each bypassed head's wait
    /// reason — stored on the entry (visible in [`Self::snapshot`]) and
    /// returned for the UI. When nothing is admissible every entry keeps its
    /// latest reason and `picked` is `None`.
    ///
    /// The closure must not call back into this queue (it would deadlock on
    /// the same-thread lock); it only evaluates admission.
    pub fn pick_next<F>(&self, mut decide: F) -> PickOutcome
    where
        F: FnMut(&QueueCandidate<'_>) -> QueueReason,
    {
        let now = self.clock.now_ms();
        let mut state = self.lock();
        let mut ranked: Vec<(Priority, u64, usize)> = state
            .entries
            .iter()
            .enumerate()
            .map(|(index, e)| {
                (
                    effective_priority(e.priority, e.queued_at_ms, now, self.config.aging_step_ms),
                    e.queued_at_ms,
                    index,
                )
            })
            .collect();
        ranked.sort_unstable_by(|a, b| {
            queue_ordering(
                a.0,
                a.1,
                state.entries[a.2].workload_id.as_str(),
                b.0,
                b.1,
                state.entries[b.2].workload_id.as_str(),
            )
        });

        let mut skipped: Vec<SkippedEntry> = Vec::new();
        let mut picked: Option<QueueEntry> = None;
        // 오래 굶은 머리 항목이 있으면 그 뒤는 평가하지 않고 같은 사유로 기다린다.
        let mut held_by: Option<QueueReason> = None;
        for &(effective, queued_at, index) in &ranked {
            let entry = &state.entries[index];
            if let Some(reason) = held_by {
                let workload_id = entry.workload_id.clone();
                let request_id = entry.request_id.clone();
                state.entries[index].wait_reason = Some(reason);
                skipped.push(SkippedEntry {
                    workload_id,
                    request_id,
                    reason,
                });
                continue;
            }
            let candidate = QueueCandidate {
                workload_id: &entry.workload_id,
                request_id: &entry.request_id,
                priority: entry.priority,
                effective_priority: effective,
                queued_at_ms: queued_at,
                waited_ms: now.saturating_sub(queued_at),
            };
            match decide(&candidate) {
                QueueReason::Admit => {
                    let removed = state.entries.remove(index);
                    picked = Some(QueueEntry {
                        workload_id: removed.workload_id,
                        request_id: removed.request_id,
                        priority: removed.priority,
                        effective_priority: effective,
                        queued_at_ms: removed.queued_at_ms,
                        wait_reason: Some(QueueReason::Admit),
                    });
                    break;
                }
                reason => {
                    let workload_id = entry.workload_id.clone();
                    let request_id = entry.request_id.clone();
                    state.entries[index].wait_reason = Some(reason);
                    skipped.push(SkippedEntry {
                        workload_id,
                        request_id,
                        reason,
                    });
                    if size_dependent_wait(reason)
                        && now.saturating_sub(queued_at) >= HOL_BYPASS_LIMIT_MS
                    {
                        held_by = Some(reason);
                    }
                }
            }
        }
        PickOutcome { picked, skipped }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use term_contracts::defaults::load_spec_defaults;

    fn qid(n: u8) -> WorkloadId {
        WorkloadId::parse(&format!("00000000-0000-4000-8000-0000000000{n:02x}")).expect("v4")
    }

    fn rid(n: u8) -> RequestId {
        RequestId::parse(&format!("10000000-0000-4000-8000-0000000000{n:02x}")).expect("v4")
    }

    fn config() -> QueueConfig {
        QueueConfig::from_defaults(&load_spec_defaults().expect("spec defaults"))
    }

    fn queue(clock: &FakeClock) -> WorkloadQueue<FakeClock> {
        WorkloadQueue::new(config(), clock.clone())
    }

    fn ids(snapshot: &[QueueEntry]) -> Vec<WorkloadId> {
        snapshot.iter().map(|e| e.workload_id.clone()).collect()
    }

    #[test]
    fn from_defaults_pins_capacity_and_aging() {
        let cfg = config();
        assert_eq!(cfg.max_entries, 64);
        assert_eq!(cfg.aging_step_ms, 300_000);
    }

    #[test]
    fn enqueue_fills_to_capacity_then_reports_queue_full() {
        let clock = FakeClock::new();
        let q = queue(&clock);
        for n in 1..=64u8 {
            q.enqueue(qid(n), rid(n), Priority(2))
                .expect("within capacity");
        }
        assert_eq!(q.len(), 64);
        let err = q.enqueue(qid(65), rid(65), Priority(0)).unwrap_err();
        assert_eq!(
            err,
            CoreError::QueueFull {
                len: 64,
                capacity: 64
            }
        );
        assert_eq!(q.len(), 64);
        // Cancelling frees a slot again.
        assert!(q.cancel(&qid(64)));
        q.enqueue(qid(65), rid(65), Priority(0))
            .expect("freed slot");
    }

    #[test]
    fn duplicate_workload_id_is_rejected() {
        let clock = FakeClock::new();
        let q = queue(&clock);
        q.enqueue(qid(1), rid(1), Priority(1)).expect("enqueued");
        let err = q.enqueue(qid(1), rid(9), Priority(0)).unwrap_err();
        assert_eq!(
            err,
            CoreError::DuplicateQueueEntry {
                workload_id: qid(1)
            }
        );
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn order_is_effective_priority_then_queued_at_then_id() {
        let clock = FakeClock::new();
        let q = queue(&clock);
        // Same instant: pure priority order (0 = highest first).
        q.enqueue(qid(1), rid(1), Priority(2)).unwrap();
        q.enqueue(qid(2), rid(2), Priority(0)).unwrap();
        q.enqueue(qid(3), rid(3), Priority(1)).unwrap();
        assert_eq!(ids(&q.snapshot()), vec![qid(2), qid(3), qid(1)]);
        // Same priority: earlier queued_at wins...
        clock.advance(1_000);
        q.enqueue(qid(4), rid(4), Priority(2)).unwrap();
        assert_eq!(
            ids(&q.snapshot()),
            vec![qid(2), qid(3), qid(1), qid(4)],
            "older entry first within equal priority"
        );
        // ...and a lexicographic id tiebreak when both coincide.
        let tie = FakeClock::new();
        let tied = queue(&tie);
        tied.enqueue(qid(9), rid(9), Priority(1)).unwrap();
        tied.enqueue(qid(2), rid(2), Priority(1)).unwrap();
        assert_eq!(ids(&tied.snapshot()), vec![qid(2), qid(9)]);
    }

    #[test]
    fn aging_rises_one_level_per_full_5min_and_stops_at_normal() {
        let clock = FakeClock::new();
        let q = queue(&clock);
        q.enqueue(qid(1), rid(1), Priority(2)).unwrap();
        let eff = || q.snapshot()[0].effective_priority;

        assert_eq!(eff(), Priority(2));
        clock.advance(299_999);
        assert_eq!(eff(), Priority(2), "incomplete interval does not age");
        clock.advance(1); // exactly 5 min
        assert_eq!(eff(), Priority(1), "one level per full interval");
        clock.advance(3_600_000);
        assert_eq!(
            eff(),
            Priority(1),
            "aging stops at NORMAL — never jumps a high-priority request"
        );
        assert_eq!(
            q.snapshot()[0].priority,
            Priority(2),
            "original priority kept"
        );
        // 원래 높은(0) 요청은 그대로 0, 보통(1)은 그대로 1.
        assert_eq!(
            effective_priority(Priority(0), 0, 3_600_000, 300_000),
            Priority(0)
        );
        assert_eq!(
            effective_priority(Priority(1), 0, 3_600_000, 300_000),
            Priority(1)
        );
    }

    /// 자원(크기)을 기다리는 머리 항목이 오래 굶으면 뒤 항목의 추월을 막는다.
    #[test]
    fn a_long_starved_head_stops_the_bypass() {
        let clock = FakeClock::new();
        let q = queue(&clock);
        q.enqueue(qid(1), rid(1), Priority(1)).unwrap(); // 큰 요청
        clock.advance(1_000);
        q.enqueue(qid(2), rid(2), Priority(1)).unwrap(); // 작은 요청
        let big = qid(1);
        let decide = |c: &QueueCandidate<'_>| {
            if c.workload_id == &big {
                QueueReason::WaitMemoryHeadroom
            } else {
                QueueReason::Admit
            }
        };
        // 아직은 추월된다.
        assert_eq!(
            q.pick_next(decide).picked.expect("bypass").workload_id,
            qid(2)
        );
        q.enqueue(qid(3), rid(3), Priority(1)).unwrap();
        // 머리가 상한 넘게 기다렸다: 더는 추월시키지 않고 같은 사유로 기다린다.
        clock.advance(HOL_BYPASS_LIMIT_MS);
        let outcome = q.pick_next(decide);
        assert!(outcome.picked.is_none(), "{outcome:?}");
        assert_eq!(outcome.skipped.len(), 2);
        assert!(outcome
            .skipped
            .iter()
            .all(|s| s.reason == QueueReason::WaitMemoryHeadroom));
        // 크기와 무관한 대기(압력)는 추월을 막지 않는다.
        let pressure = |c: &QueueCandidate<'_>| {
            if c.workload_id == &big {
                QueueReason::WaitHostPressure
            } else {
                QueueReason::Admit
            }
        };
        assert_eq!(
            q.pick_next(pressure).picked.expect("bypass").workload_id,
            qid(3)
        );
    }

    #[test]
    fn head_of_line_bypass_picks_first_admissible_and_records_reasons() {
        let clock = FakeClock::new();
        let q = queue(&clock);
        q.enqueue(qid(1), rid(1), Priority(0)).unwrap();
        q.enqueue(qid(2), rid(2), Priority(1)).unwrap();
        q.enqueue(qid(3), rid(3), Priority(2)).unwrap();

        let blocked = qid(1);
        let outcome = q.pick_next(|c| {
            if c.workload_id == &blocked {
                QueueReason::WaitMemoryHeadroom
            } else {
                QueueReason::Admit
            }
        });

        let picked = outcome.picked.expect("mid entry is admissible");
        assert_eq!(picked.workload_id, qid(2));
        assert_eq!(picked.wait_reason, Some(QueueReason::Admit));
        assert_eq!(picked.priority, Priority(1));
        assert_eq!(
            outcome.skipped,
            vec![SkippedEntry {
                workload_id: qid(1),
                request_id: rid(1),
                reason: QueueReason::WaitMemoryHeadroom,
            }]
        );
        // The reason persists for the UI; entries behind the pick were not
        // evaluated and carry no reason.
        let snap = q.snapshot();
        assert_eq!(ids(&snap), vec![qid(1), qid(3)]);
        assert_eq!(snap[0].wait_reason, Some(QueueReason::WaitMemoryHeadroom));
        assert_eq!(snap[1].wait_reason, None);
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn nothing_admissible_records_every_reason() {
        let clock = FakeClock::new();
        let q = queue(&clock);
        q.enqueue(qid(1), rid(1), Priority(0)).unwrap();
        q.enqueue(qid(2), rid(2), Priority(1)).unwrap();
        let outcome = q.pick_next(|c| {
            if c.workload_id == &qid(1) {
                QueueReason::WaitHostPressure
            } else {
                QueueReason::WaitTelemetry
            }
        });
        assert!(outcome.picked.is_none());
        assert_eq!(outcome.skipped.len(), 2);
        assert_eq!(outcome.skipped[0].reason, QueueReason::WaitHostPressure);
        assert_eq!(outcome.skipped[1].reason, QueueReason::WaitTelemetry);
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn blocked_high_priority_still_lets_lower_entries_start() {
        let clock = FakeClock::new();
        let q = queue(&clock);
        q.enqueue(qid(1), rid(1), Priority(0)).unwrap();
        q.enqueue(qid(2), rid(2), Priority(2)).unwrap();
        let high = qid(1);
        let outcome = q.pick_next(|c| {
            if c.workload_id == &high {
                QueueReason::WaitHostPressure
            } else {
                QueueReason::Admit
            }
        });
        assert_eq!(outcome.picked.expect("bypass works").workload_id, qid(2));
        assert_eq!(outcome.skipped[0].reason, QueueReason::WaitHostPressure);
    }

    #[test]
    fn aging_lifting_an_old_low_priority_above_new_arrivals() {
        // High priority is permanently blocked. A low-priority entry ages to
        // effective 1 after 5 min and then outranks a freshly queued middle
        // priority (older queued_at wins within the same level); before that,
        // the middle entry wins the bypass order.
        let clock = FakeClock::new();
        let q = queue(&clock);
        let high = qid(1);
        q.enqueue(qid(1), rid(1), Priority(0)).unwrap();
        q.enqueue(qid(3), rid(3), Priority(2)).unwrap();
        // Fresh middle arrival before any aging: order is high, mid, low.
        q.enqueue(qid(2), rid(2), Priority(1)).unwrap();
        let first = q.pick_next(|c| {
            if c.workload_id == &high {
                QueueReason::WaitHostPressure
            } else {
                QueueReason::Admit
            }
        });
        assert_eq!(
            first.picked.expect("mid wins pre-aging").workload_id,
            qid(2),
            "middle priority outranks the un-aged low entry"
        );

        // Now age the low entry past one interval.
        clock.advance(300_000);
        q.enqueue(qid(4), rid(4), Priority(1)).unwrap();
        let second = q.pick_next(|c| {
            if c.workload_id == &high {
                QueueReason::WaitHostPressure
            } else {
                QueueReason::Admit
            }
        });
        assert_eq!(
            second.picked.expect("starvation relief").workload_id,
            qid(3),
            "aged low entry (effective 1, older) now outranks the fresh priority 1"
        );
        assert_eq!(second.skipped[0].workload_id, qid(1));
        // The permanently blocked head keeps its reason for the UI.
        assert_eq!(
            q.snapshot()[0].wait_reason,
            Some(QueueReason::WaitHostPressure)
        );
    }

    #[test]
    fn cancel_removes_by_id_and_pick_on_empty_queue_is_none() {
        let clock = FakeClock::new();
        let q = queue(&clock);
        let outcome = q.pick_next(|_| QueueReason::Admit);
        assert!(outcome.picked.is_none());
        assert!(outcome.skipped.is_empty());

        q.enqueue(qid(1), rid(1), Priority(0)).unwrap();
        q.enqueue(qid(2), rid(2), Priority(0)).unwrap();
        assert!(q.cancel(&qid(1)));
        assert!(!q.cancel(&qid(1)), "second cancel of the same id");
        assert_eq!(q.len(), 1);
        let outcome = q.pick_next(|_| QueueReason::Admit);
        assert_eq!(outcome.picked.unwrap().workload_id, qid(2));
        assert!(q.is_empty());
    }

    #[test]
    fn candidate_exposes_wait_time_and_aging_state() {
        let clock = FakeClock::new();
        let q = queue(&clock);
        q.enqueue(qid(1), rid(1), Priority(2)).unwrap();
        clock.advance(345_000);
        let seen: Mutex<Vec<(u8, u8, u64)>> = Mutex::new(Vec::new());
        q.pick_next(|c| {
            seen.lock()
                .unwrap()
                .push((c.priority.0, c.effective_priority.0, c.waited_ms));
            QueueReason::Admit
        });
        let seen = seen.into_inner().unwrap();
        assert_eq!(
            seen,
            vec![(2, 1, 345_000)],
            "priority 2, aged to 1 after one full interval, 345 s waited"
        );
    }
}

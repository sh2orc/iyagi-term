//! Output flow control: per-view credits, global output budgets, and the
//! ACK transmission ledger (spec `02-runner.md` §4).
//!
//! The data path is `PTY reader -> journal -> data IPC -> xterm.write ->
//! ACK`, and every unacknowledged byte is accounted twice:
//!
//! * raw bytes pending across all sessions share an 8 MiB budget
//!   ([`GlobalOutputBudget`] raw side) — the sum of all views' unacked raw
//!   bytes plus in-flight chunks;
//! * transient duplicates (base64/IPC copies and journal-side retained
//!   copies) share a separate 24 MiB transport budget. Each retained copy of
//!   a record costs one reservation at its own size; the transport copy is
//!   the base64 encoding, the journal copy is the raw payload
//!   (see [`GlobalOutputBudget::base64_len`]).
//!
//! Per view, sending stops once 256 KiB of raw bytes are unacked (high
//! watermark) and resumes at or below 64 KiB (low watermark), so a slow
//! consumer blocks only its own view while the journal keeps accepting PTY
//! output. ACKs advance per epoch: duplicates and old-epoch ACKs are
//! ignored, future/unsent seqs are protocol errors.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use term_contracts::session::limits::{
    ACK_COALESCE_BYTES, ACK_COALESCE_MS, OUTPUT_CHUNK_BYTES, OUTPUT_HIGH_BYTES, OUTPUT_LOW_BYTES,
    OUTPUT_RAW_GLOBAL_BYTES, TRANSPORT_GLOBAL_BYTES,
};
use term_contracts::ViewId;

/// Per-view high watermark (`output_high_bytes`, 256 KiB).
pub const VIEW_HIGH_WATERMARK: u64 = OUTPUT_HIGH_BYTES as u64;
/// Per-view low watermark (`output_low_bytes`, 64 KiB).
pub const VIEW_LOW_WATERMARK: u64 = OUTPUT_LOW_BYTES as u64;

/// Flow-control errors.
#[derive(Debug, thiserror::Error)]
pub enum FlowError {
    /// Malformed ACK sequence: future/unsent seq, send-stream gap, or seq 0.
    #[error("flow protocol violation: {detail}")]
    ProtocolError { detail: String },
    #[error("global raw output budget exhausted ({limit} bytes)")]
    RawBudgetExhausted { limit: u64 },
    #[error("global transport budget exhausted ({limit} bytes)")]
    TransportBudgetExhausted { limit: u64 },
    #[error("no flow state attached for view {0}")]
    UnknownView(ViewId),
}

/// Global budgets bounding unacked raw bytes (8 MiB default) and transient
/// transport copies (24 MiB default) across all sessions.
///
/// A reservation is taken for every retained copy of a record and released
/// when that copy is freed (ACK for transport sends, deletion for journal
/// retention). These are accounting budgets, not RSS measurements.
#[derive(Debug)]
pub struct GlobalOutputBudget {
    raw_limit: u64,
    raw_used: u64,
    transport_limit: u64,
    transport_used: u64,
}

impl Default for GlobalOutputBudget {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalOutputBudget {
    pub fn new() -> Self {
        Self {
            raw_limit: OUTPUT_RAW_GLOBAL_BYTES as u64,
            raw_used: 0,
            transport_limit: TRANSPORT_GLOBAL_BYTES as u64,
            transport_used: 0,
        }
    }

    /// Shared handle with the default limits.
    pub fn shared() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self::new()))
    }

    pub fn with_limits(raw_limit: u64, transport_limit: u64) -> Self {
        Self {
            raw_limit,
            raw_used: 0,
            transport_limit,
            transport_used: 0,
        }
    }

    /// Shared handle with custom limits.
    pub fn shared_with_limits(raw_limit: u64, transport_limit: u64) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self::with_limits(raw_limit, transport_limit)))
    }

    /// Size of a base64 transport copy of `raw_len` bytes (standard alphabet,
    /// no line wrapping): `ceil(raw_len / 3) * 4`.
    pub fn base64_len(raw_len: u64) -> u64 {
        raw_len.div_ceil(3) * 4
    }

    pub fn raw_limit(&self) -> u64 {
        self.raw_limit
    }

    pub fn raw_used(&self) -> u64 {
        self.raw_used
    }

    pub fn transport_limit(&self) -> u64 {
        self.transport_limit
    }

    pub fn transport_used(&self) -> u64 {
        self.transport_used
    }

    /// Whether `bytes` more raw bytes fit under the raw budget.
    pub fn raw_available(&self, bytes: u64) -> bool {
        self.raw_used
            .checked_add(bytes)
            .is_some_and(|next| next <= self.raw_limit)
    }

    /// Whether `bytes` more transport bytes fit under the transport budget.
    pub fn transport_available(&self, bytes: u64) -> bool {
        self.transport_used
            .checked_add(bytes)
            .is_some_and(|next| next <= self.transport_limit)
    }

    pub fn reserve_raw(&mut self, bytes: u64) -> Result<(), FlowError> {
        match self.raw_used.checked_add(bytes) {
            Some(next) if next <= self.raw_limit => {
                self.raw_used = next;
                Ok(())
            }
            _ => Err(FlowError::RawBudgetExhausted {
                limit: self.raw_limit,
            }),
        }
    }

    pub fn release_raw(&mut self, bytes: u64) {
        self.raw_used = self.raw_used.saturating_sub(bytes);
    }

    pub fn reserve_transport(&mut self, bytes: u64) -> Result<(), FlowError> {
        match self.transport_used.checked_add(bytes) {
            Some(next) if next <= self.transport_limit => {
                self.transport_used = next;
                Ok(())
            }
            _ => Err(FlowError::TransportBudgetExhausted {
                limit: self.transport_limit,
            }),
        }
    }

    pub fn release_transport(&mut self, bytes: u64) {
        self.transport_used = self.transport_used.saturating_sub(bytes);
    }
}

/// Per-view unacked-raw-byte credit with hysteresis: block at the high
/// watermark (256 KiB), resume at or below the low watermark (64 KiB).
#[derive(Debug, Clone, Copy)]
pub struct ViewCredit {
    high: u64,
    low: u64,
    unacked: u64,
    blocked: bool,
}

impl Default for ViewCredit {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewCredit {
    pub fn new() -> Self {
        Self::with_watermarks(VIEW_HIGH_WATERMARK, VIEW_LOW_WATERMARK)
    }

    /// Custom watermarks (`low < high` expected for meaningful hysteresis).
    pub fn with_watermarks(high: u64, low: u64) -> Self {
        debug_assert!(low < high, "low watermark must be below the high watermark");
        Self {
            high,
            low,
            unacked: 0,
            blocked: false,
        }
    }

    pub fn unacked_bytes(&self) -> u64 {
        self.unacked
    }

    pub fn is_blocked(&self) -> bool {
        self.blocked
    }

    /// Account `raw_len` newly sent (unacked) bytes. Returns true exactly
    /// once, on the open -> blocked transition.
    pub fn record_sent(&mut self, raw_len: u64) -> bool {
        self.unacked = self.unacked.saturating_add(raw_len);
        if !self.blocked && self.unacked >= self.high {
            self.blocked = true;
            return true;
        }
        false
    }

    /// Release acknowledged bytes. Returns true exactly once, on the
    /// blocked -> open transition (unacked dropped to `low` or below).
    pub fn release(&mut self, raw_len: u64) -> bool {
        self.unacked = self.unacked.saturating_sub(raw_len);
        if self.blocked && self.unacked <= self.low {
            self.blocked = false;
            return true;
        }
        false
    }
}

/// A record handed to a view's transport (one journal-ordered frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SentRecord {
    pub seq: u64,
    /// Raw payload size (0 for resize frames).
    pub raw_len: u32,
}

/// Successful contiguous ACK advance returned by [`AckLedger::on_ack`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckAdvance {
    pub through_seq: u64,
    pub released_bytes: u64,
    /// Transport budget to give back: the sum of each released record's
    /// base64 copy size (per-record sums never equal `base64_len(sum)`, so
    /// release must mirror reserve record by record).
    pub released_transport_bytes: u64,
}

/// Per-session+epoch transmission ledger: seq -> raw_len for records sent
/// but not yet acknowledged (`02-runner.md` §4).
///
/// The send stream must be FIFO-contiguous in seq (the data path never skips
/// records within an epoch), which makes duplicate/future detection exact.
#[derive(Debug)]
pub struct AckLedger {
    epoch: String,
    has_sends: bool,
    last_sent_seq: u64,
    acked_through: u64,
    unacked: VecDeque<(u64, u32)>,
    unacked_bytes: u64,
}

impl AckLedger {
    pub fn new(epoch: impl Into<String>) -> Self {
        Self {
            epoch: epoch.into(),
            has_sends: false,
            last_sent_seq: 0,
            acked_through: 0,
            unacked: VecDeque::new(),
            unacked_bytes: 0,
        }
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// Rotate to a fresh epoch (every attach/owner change) and clear the
    /// ledger. Callers must release the old epoch's budget reservations.
    pub fn new_epoch(&mut self, epoch: impl Into<String>) {
        *self = Self::new(epoch);
    }

    pub fn has_sends(&self) -> bool {
        self.has_sends
    }

    pub fn last_sent_seq(&self) -> u64 {
        self.last_sent_seq
    }

    pub fn acked_through(&self) -> u64 {
        self.acked_through
    }

    /// 스냅샷 재개(`AttachParams.resume_from_seq`)로 처음부터 보내지 않는
    /// 뷰의 밑받침: UI는 이미 `seq`까지의 화면을 갖고 있어 그 자리를 ACK한다.
    /// 실제로 보낸 기록은 없으므로 `acked_through`만 올려 두면 그 ACK는
    /// 중복으로 무시된다("beyond the last sent seq" 오류로 튕기지 않는다).
    /// 첫 전송(`record_sent`) 전에만 적용된다.
    pub fn seed_acked_through(&mut self, seq: u64) {
        if !self.has_sends && seq > self.acked_through {
            self.acked_through = seq;
        }
    }

    pub fn pending_bytes(&self) -> u64 {
        self.unacked_bytes
    }

    pub fn pending_records(&self) -> usize {
        self.unacked.len()
    }

    /// Record a send. The first send of an epoch may start at any seq >= 1
    /// (replay resumes at `replay_from_seq`); subsequent sends must continue
    /// the stream with no gap.
    pub fn record_sent(&mut self, seq: u64, raw_len: u32) -> Result<(), FlowError> {
        if seq == 0 {
            return Err(FlowError::ProtocolError {
                detail: "journal seqs start at 1, got 0".into(),
            });
        }
        if self.has_sends {
            let expected =
                self.last_sent_seq
                    .checked_add(1)
                    .ok_or_else(|| FlowError::ProtocolError {
                        detail: "send seq overflowed u64".into(),
                    })?;
            if seq != expected {
                return Err(FlowError::ProtocolError {
                    detail: format!("send stream gap: expected seq {expected}, sent {seq}"),
                });
            }
        } else {
            self.has_sends = true;
        }
        self.unacked_bytes = self
            .unacked_bytes
            .checked_add(raw_len as u64)
            .ok_or_else(|| FlowError::ProtocolError {
                detail: "ledger byte accounting overflowed u64".into(),
            })?;
        self.unacked.push_back((seq, raw_len));
        self.last_sent_seq = seq;
        Ok(())
    }

    /// Apply an ACK (`through_seq` = last consecutively processed seq in
    /// this epoch).
    ///
    /// * `Ok(None)` — duplicate/lower ACK, ignored;
    /// * `Err(ProtocolError)` — future or never-sent seq;
    /// * `Ok(Some(advance))` — contiguous advance, with the raw bytes
    ///   released from the pending window.
    pub fn on_ack(&mut self, through_seq: u64) -> Result<Option<AckAdvance>, FlowError> {
        if through_seq <= self.acked_through {
            return Ok(None); // duplicate/lower
        }
        if through_seq > self.last_sent_seq {
            return Err(FlowError::ProtocolError {
                detail: format!(
                    "ACK through_seq {through_seq} is beyond the last sent seq {}",
                    self.last_sent_seq
                ),
            });
        }
        let mut released = 0u64;
        let mut released_transport = 0u64;
        while let Some(&(seq, raw_len)) = self.unacked.front() {
            if seq > through_seq {
                break;
            }
            self.unacked.pop_front();
            released = released.saturating_add(raw_len as u64);
            released_transport =
                released_transport.saturating_add(GlobalOutputBudget::base64_len(raw_len as u64));
        }
        self.unacked_bytes = self.unacked_bytes.saturating_sub(released);
        self.acked_through = through_seq;
        Ok(Some(AckAdvance {
            through_seq,
            released_bytes: released,
            released_transport_bytes: released_transport,
        }))
    }
}

/// Consumer-side ACK batching: send an accumulated ACK when 64 KiB has been
/// processed since the last ACK, or when 16 ms elapsed with something
/// pending (`ack_coalesce` in defaults.json).
#[derive(Debug, Clone, Copy)]
pub struct AckCoalescer {
    interval_ms: u64,
    bytes_threshold: u64,
    last_sent_ms: u64,
}

impl AckCoalescer {
    /// `clock_start_ms` anchors the 16 ms timer until the first ACK.
    pub fn new(clock_start_ms: u64) -> Self {
        Self::with_policy(clock_start_ms, ACK_COALESCE_MS, ACK_COALESCE_BYTES as u64)
    }

    pub fn with_policy(clock_start_ms: u64, interval_ms: u64, bytes_threshold: u64) -> Self {
        Self {
            interval_ms,
            bytes_threshold,
            last_sent_ms: clock_start_ms,
        }
    }

    /// Whether an accumulated ACK for `bytes_since` raw bytes is due at
    /// `now_ms`. Zero pending bytes are never due.
    pub fn coalesce_due(&self, now_ms: u64, bytes_since: u64) -> bool {
        if bytes_since >= self.bytes_threshold {
            return true;
        }
        if bytes_since == 0 {
            return false;
        }
        now_ms.saturating_sub(self.last_sent_ms) >= self.interval_ms
    }

    /// Record that the ACK was sent at `now_ms`.
    pub fn mark_sent(&mut self, now_ms: u64) {
        self.last_sent_ms = now_ms;
    }
}

/// One-shot blocked/unblocked signal so callers emit `session.flow_blocked`
/// events once per transition, not per record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowTransition {
    Blocked { view: ViewId },
    Unblocked { view: ViewId },
}

/// Result of applying an ACK to flow state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AckOutcome {
    /// Duplicate/lower ACK, or one for an unknown view or an older epoch.
    Ignored,
    /// Contiguous advance: credits released and views that crossed the low
    /// watermark resumed.
    Advanced {
        through_seq: u64,
        released_bytes: u64,
        unblocked_views: Vec<ViewId>,
    },
}

/// Per-view flow state: one epoch (every attach rotates it) with its ledger
/// and credit, plus the outstanding transport reservation total (mirrors the
/// ledger's unacked entries record by record).
struct ViewFlow {
    epoch: String,
    ledger: AckLedger,
    credit: ViewCredit,
    pending_transport: u64,
}

/// Session-side flow controller wiring credits, the ACK ledger, and the
/// shared global budgets together.
///
/// [`FlowController::record_sent`] reserves one raw reservation plus one
/// transport (base64-sized) reservation per sent record; both are released
/// by the ACK covering that record. Callers retaining an additional copy
/// (e.g. the journal-side raw copy) must reserve it separately via
/// [`GlobalOutputBudget::reserve_transport`] / `reserve_raw`.
pub struct FlowController {
    views: HashMap<ViewId, ViewFlow>,
    budget: Arc<Mutex<GlobalOutputBudget>>,
}

impl Default for FlowController {
    fn default() -> Self {
        Self::new()
    }
}

impl FlowController {
    /// Controller with its own default budgets; production callers share one
    /// budget across all sessions via [`FlowController::with_budget`].
    pub fn new() -> Self {
        Self::with_budget(GlobalOutputBudget::shared())
    }

    pub fn with_budget(budget: Arc<Mutex<GlobalOutputBudget>>) -> Self {
        Self {
            views: HashMap::new(),
            budget,
        }
    }

    /// Shared budget handle (for cross-session accounting/tests).
    pub fn budget(&self) -> Arc<Mutex<GlobalOutputBudget>> {
        Arc::clone(&self.budget)
    }

    /// Attach a view with a fresh epoch. Re-attaching an existing view
    /// rotates the epoch (the old ledger is cleared and its outstanding
    /// budget reservations released, mirroring the dropped connection).
    pub fn attach_view(&mut self, view: ViewId, epoch: impl Into<String>) {
        if let Some(old) = self.views.remove(&view) {
            self.release_view_budget(&old);
        }
        let epoch = epoch.into();
        let ledger = AckLedger::new(epoch.clone());
        self.views.insert(
            view,
            ViewFlow {
                epoch,
                ledger,
                credit: ViewCredit::new(),
                pending_transport: 0,
            },
        );
    }

    /// Detach a view, releasing its outstanding reservations. Returns
    /// whether the view was attached.
    pub fn detach_view(&mut self, view: &ViewId) -> bool {
        match self.views.remove(view) {
            Some(flow) => {
                self.release_view_budget(&flow);
                true
            }
            None => false,
        }
    }

    /// Whether a record may be sent to `view`: the view must be below its
    /// high watermark and both global budgets must have room for a worst
    /// case output chunk. Unknown views cannot send.
    pub fn can_send(&self, view: &ViewId) -> bool {
        let Some(flow) = self.views.get(view) else {
            return false;
        };
        if flow.credit.is_blocked() {
            return false;
        }
        let budget = self.lock_budget();
        budget.raw_available(OUTPUT_CHUNK_BYTES as u64)
            && budget.transport_available(GlobalOutputBudget::base64_len(OUTPUT_CHUNK_BYTES as u64))
    }

    /// Account a record sent to `view`: reserves global raw + transport
    /// budget, then extends the ledger and the view credit. Returns the
    /// blocked transition when this send crossed the high watermark.
    /// On error nothing is reserved or recorded.
    pub fn record_sent(
        &mut self,
        view: &ViewId,
        record: SentRecord,
    ) -> Result<Option<FlowTransition>, FlowError> {
        {
            let flow = self
                .views
                .get(view)
                .ok_or_else(|| FlowError::UnknownView(view.clone()))?;
            // Pre-validate send-stream contiguity before touching budgets.
            if flow.ledger.has_sends() {
                let expected = flow.ledger.last_sent_seq().checked_add(1).ok_or_else(|| {
                    FlowError::ProtocolError {
                        detail: "send seq overflowed u64".into(),
                    }
                })?;
                if record.seq != expected {
                    return Err(FlowError::ProtocolError {
                        detail: format!(
                            "send stream gap: expected seq {expected}, sent {}",
                            record.seq
                        ),
                    });
                }
            }
        }
        let raw = record.raw_len as u64;
        let transport = GlobalOutputBudget::base64_len(raw);
        if raw > 0 {
            let mut budget = self.lock_budget();
            budget.reserve_raw(raw)?;
            if let Err(error) = budget.reserve_transport(transport) {
                budget.release_raw(raw);
                return Err(error);
            }
        }
        let flow = self
            .views
            .get_mut(view)
            .ok_or_else(|| FlowError::UnknownView(view.clone()))?;
        if let Err(error) = flow.ledger.record_sent(record.seq, record.raw_len) {
            if raw > 0 {
                let mut budget = self.lock_budget();
                budget.release_raw(raw);
                budget.release_transport(transport);
            }
            return Err(error);
        }
        flow.pending_transport = flow.pending_transport.saturating_add(transport);
        if flow.credit.record_sent(raw) {
            return Ok(Some(FlowTransition::Blocked { view: view.clone() }));
        }
        Ok(None)
    }

    /// Apply a view's ACK: duplicates and old-epoch ACKs are ignored, future
    /// or unsent seqs are protocol errors, and a contiguous advance releases
    /// the raw and transport reservations of every covered record.
    pub fn on_ack(
        &mut self,
        view: &ViewId,
        epoch: &str,
        through_seq: u64,
    ) -> Result<AckOutcome, FlowError> {
        let advance = {
            let Some(flow) = self.views.get_mut(view) else {
                return Ok(AckOutcome::Ignored);
            };
            if flow.epoch != epoch {
                return Ok(AckOutcome::Ignored); // older epoch (or stale view)
            }
            flow.ledger.on_ack(through_seq)?
        };
        let Some(advance) = advance else {
            return Ok(AckOutcome::Ignored);
        };
        if advance.released_bytes > 0 {
            let mut budget = self.lock_budget();
            budget.release_raw(advance.released_bytes);
            budget.release_transport(advance.released_transport_bytes);
        }
        let mut unblocked_views = Vec::new();
        if let Some(flow) = self.views.get_mut(view) {
            flow.pending_transport = flow
                .pending_transport
                .saturating_sub(advance.released_transport_bytes);
            if flow.credit.release(advance.released_bytes) {
                unblocked_views.push(view.clone());
            }
        }
        Ok(AckOutcome::Advanced {
            through_seq: advance.through_seq,
            released_bytes: advance.released_bytes,
            unblocked_views,
        })
    }

    /// Unacked raw bytes for the view (from its transmission ledger credit).
    pub fn unacked_bytes(&self, view: &ViewId) -> Option<u64> {
        self.views.get(view).map(|flow| flow.credit.unacked_bytes())
    }

    /// Whether the view is currently blocked by its high watermark.
    pub fn view_blocked(&self, view: &ViewId) -> bool {
        self.views
            .get(view)
            .is_some_and(|flow| flow.credit.is_blocked())
    }

    /// Current epoch of the view, if attached.
    pub fn view_epoch(&self, view: &ViewId) -> Option<String> {
        self.views.get(view).map(|flow| flow.epoch.clone())
    }

    /// 이 뷰의 현재 epoch 원장이 실제로 보낸 마지막 seq — 한 번도 보내지
    /// 않았거나 붙어 있지 않으면 `None`. 펌프가 배달 커서를 원장에 맞추는 데
    /// 쓴다: 원장이 커서보다 앞서면 그 레코드는 이미 큐에 들어간 것이다.
    pub fn sent_through(&self, view: &ViewId) -> Option<u64> {
        self.views
            .get(view)
            .filter(|flow| flow.ledger.has_sends())
            .map(|flow| flow.ledger.last_sent_seq())
    }

    /// [`AckLedger::seed_acked_through`]의 (view, epoch) 지정 버전 — attach
    /// 직후 재개 지점(`resume_from_seq - 1`)을 밑받침한다. epoch이 다르면
    /// 조용히 무시한다.
    pub fn seed_view_acked(&mut self, view: &ViewId, epoch: &str, through_seq: u64) {
        if let Some(flow) = self.views.get_mut(view) {
            if flow.epoch == epoch {
                flow.ledger.seed_acked_through(through_seq);
            }
        }
    }

    /// Release a detached/rotated view's outstanding reservations (its
    /// transport copies die with the connection; the raw bytes stop pending
    /// because no consumer will ever ACK them).
    fn release_view_budget(&self, flow: &ViewFlow) {
        let pending_raw = flow.credit.unacked_bytes();
        if pending_raw > 0 || flow.pending_transport > 0 {
            let mut budget = self.lock_budget();
            budget.release_raw(pending_raw);
            budget.release_transport(flow.pending_transport);
        }
    }

    fn lock_budget(&self) -> MutexGuard<'_, GlobalOutputBudget> {
        // Counter-only state: recover from poisoning instead of failing the data path.
        self.budget
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_credit_hysteresis_at_exact_watermarks() {
        let mut credit = ViewCredit::new();
        assert!(!credit.record_sent(VIEW_HIGH_WATERMARK - 1));
        assert!(!credit.is_blocked());
        assert!(credit.record_sent(1)); // exactly 256 KiB -> block
        assert_eq!(credit.unacked_bytes(), VIEW_HIGH_WATERMARK);
        assert!(credit.is_blocked());
        assert!(!credit.release(1)); // 256 KiB - 1: still inside hysteresis
                                     // Release down to low + 1: hysteresis keeps it blocked.
        assert!(!credit.release(VIEW_HIGH_WATERMARK - VIEW_LOW_WATERMARK - 2));
        assert_eq!(credit.unacked_bytes(), VIEW_LOW_WATERMARK + 1);
        assert!(credit.is_blocked());
        assert!(credit.release(1)); // exactly 64 KiB -> resume
        assert!(!credit.is_blocked());
    }

    /// 스냅샷 재개 뷰의 원장: 재개 지점까지를 이미 ACK된 것으로 밑받침하면,
    /// UI가 보낸 적 없는 그 seq 자리를 ACK해도 오류가 아니라 중복 무시가
    /// 된다. 첫 실제 전송은 재개 지점 다음 seq에서 자유롭게 시작한다.
    #[test]
    fn seeded_ledger_takes_the_resume_ack_as_duplicate() {
        let mut ledger = AckLedger::new("e1");
        ledger.seed_acked_through(5000);
        // 보낸 적 없는 재개 지점 ACK — 예전이라면 "beyond the last sent seq".
        assert_eq!(ledger.on_ack(5000).unwrap(), None);
        // 첫 전송은 5001부터(재생 시작점).
        ledger.record_sent(5001, 10).unwrap();
        ledger.record_sent(5002, 20).unwrap();
        let advance = ledger.on_ack(5002).unwrap().unwrap();
        assert_eq!(advance.through_seq, 5002);
        assert_eq!(advance.released_bytes, 30);
        // 밑받침은 첫 전송 뒤에는 적용되지 않는다(되돌리지 않는다).
        ledger.seed_acked_through(9000);
        assert!(matches!(
            ledger.on_ack(9000),
            Err(FlowError::ProtocolError { .. })
        ));
    }

    /// 펌프의 커서 정렬 기준: 현재 epoch 원장이 실제로 보낸 마지막 seq.
    /// 보낸 것이 없거나(첫 전송 전) 새 epoch으로 다시 붙으면 `None`이다.
    #[test]
    fn sent_through_follows_the_current_epoch_ledger() {
        let mut flow =
            FlowController::with_budget(Arc::new(Mutex::new(GlobalOutputBudget::default())));
        let view = ViewId::generate();
        assert_eq!(flow.sent_through(&view), None);
        flow.attach_view(view.clone(), "e1");
        assert_eq!(flow.sent_through(&view), None);
        flow.record_sent(&view, SentRecord { seq: 7, raw_len: 3 })
            .unwrap();
        flow.record_sent(&view, SentRecord { seq: 8, raw_len: 3 })
            .unwrap();
        assert_eq!(flow.sent_through(&view), Some(8));
        // 같은 seq를 다시 넣으면 원장이 거부한다 — 펌프는 이 값으로 커서를 맞춰 피한다.
        assert!(matches!(
            flow.record_sent(&view, SentRecord { seq: 8, raw_len: 3 }),
            Err(FlowError::ProtocolError { .. })
        ));
        flow.attach_view(view.clone(), "e2");
        assert_eq!(flow.sent_through(&view), None);
    }

    /// FlowController의 (view, epoch) 지정 밑받침 — epoch이 다르면 무시된다.
    #[test]
    fn seed_view_acked_ignores_other_epochs() {
        let mut flow =
            FlowController::with_budget(Arc::new(Mutex::new(GlobalOutputBudget::default())));
        flow.attach_view(ViewId::generate(), "e1");
        let view = ViewId::generate();
        flow.attach_view(view.clone(), "e1");
        flow.seed_view_acked(&view, "e0", 100); // 다른 epoch — 조용히 무시
        flow.seed_view_acked(&view, "e1", 100);
        flow.seed_view_acked(&view, "e1", 50); // 낮은 값은 덮지 않는다
                                               // 여전히 첫 전송 전이므로 원장은 비어 있고, ACK 100은 중복 무시.
        assert_eq!(
            flow.views
                .get_mut(&view)
                .unwrap()
                .ledger
                .on_ack(100)
                .unwrap(),
            None
        );
    }

    #[test]
    fn ack_ledger_duplicate_future_and_rotation() {
        let mut ledger = AckLedger::new("e1");
        // Future ACK before any send.
        assert!(matches!(
            ledger.on_ack(1),
            Err(FlowError::ProtocolError { .. })
        ));
        ledger.record_sent(1, 100).unwrap();
        ledger.record_sent(2, 200).unwrap();
        // Send-stream gap.
        assert!(matches!(
            ledger.record_sent(4, 1),
            Err(FlowError::ProtocolError { .. })
        ));
        // Future/unsent ACK.
        assert!(matches!(
            ledger.on_ack(3),
            Err(FlowError::ProtocolError { .. })
        ));
        let advance = ledger.on_ack(2).unwrap().unwrap();
        assert_eq!(
            advance,
            AckAdvance {
                through_seq: 2,
                released_bytes: 300,
                released_transport_bytes: GlobalOutputBudget::base64_len(100)
                    + GlobalOutputBudget::base64_len(200),
            }
        );
        assert_eq!(ledger.pending_bytes(), 0);
        // Duplicate/lower ACK ignored.
        assert_eq!(ledger.on_ack(2).unwrap(), None);
        assert_eq!(ledger.on_ack(1).unwrap(), None);
        // Rotation clears.
        ledger.new_epoch("e2");
        assert_eq!(ledger.epoch(), "e2");
        assert!(!ledger.has_sends());
        assert_eq!(ledger.pending_bytes(), 0);
        assert_eq!(ledger.pending_records(), 0);
    }

    #[test]
    fn ack_ledger_replay_epochs_start_above_one() {
        let mut ledger = AckLedger::new("e1");
        ledger.record_sent(5, 10).unwrap();
        ledger.record_sent(6, 20).unwrap();
        let advance = ledger.on_ack(6).unwrap().unwrap();
        assert_eq!(advance.through_seq, 6);
        assert_eq!(advance.released_bytes, 30);
        assert_eq!(
            advance.released_transport_bytes,
            GlobalOutputBudget::base64_len(10) + GlobalOutputBudget::base64_len(20)
        );
    }

    #[test]
    fn base64_len_matches_encoder_output_size() {
        assert_eq!(GlobalOutputBudget::base64_len(0), 0);
        assert_eq!(GlobalOutputBudget::base64_len(1), 4);
        assert_eq!(GlobalOutputBudget::base64_len(2), 4);
        assert_eq!(GlobalOutputBudget::base64_len(3), 4);
        assert_eq!(GlobalOutputBudget::base64_len(4), 8);
        assert_eq!(
            GlobalOutputBudget::base64_len(OUTPUT_CHUNK_BYTES as u64),
            21_848
        );
    }

    #[test]
    fn budget_reserve_release_with_clear_errors() {
        let mut budget = GlobalOutputBudget::with_limits(1_000, 1_000);
        budget.reserve_raw(600).unwrap();
        budget.reserve_transport(600).unwrap();
        assert!(matches!(
            budget.reserve_raw(401),
            Err(FlowError::RawBudgetExhausted { limit: 1_000 })
        ));
        assert!(matches!(
            budget.reserve_transport(401),
            Err(FlowError::TransportBudgetExhausted { limit: 1_000 })
        ));
        budget.release_raw(600);
        budget.release_transport(600);
        assert!(budget.raw_available(1_000));
        assert!(budget.transport_available(1_000));
        // Overflow counts as exhausted, never wraps.
        assert!(!budget.raw_available(u64::MAX));
        assert!(matches!(
            budget.reserve_raw(u64::MAX),
            Err(FlowError::RawBudgetExhausted { .. })
        ));
    }
}

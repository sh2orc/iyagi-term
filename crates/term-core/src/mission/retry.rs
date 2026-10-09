//! A transient code alone is not evidence that replaying a task is safe.
use term_contracts::mission::{types::*, MissionErrorCode};

/// Two automatic retries at 2s/10s plus 0..20% jitter. Run identity supplies
/// a stable seed, so persistence retries and restarts never move the deadline.
pub fn retry_deadline(run: &Run) -> Option<u64> {
    if run.state != RunState::Failed
        || run.ended_at.is_none()
        || run.dispatch_state == RunDispatchState::Acknowledged
        || !matches!(
            run.failure_code,
            Some(MissionErrorCode::ProviderUnavailable | MissionErrorCode::ProviderRateLimited)
        )
    {
        return None;
    }
    let base = match run.attempt {
        1 => 2_000u64,
        2 => 10_000,
        _ => return None,
    };
    let RetryEvidence::RequestNotSubmitted {
        observed_at_unix_ms,
        retry_after_unix_ms,
    } = run.retry_evidence.as_ref()?
    else {
        return None;
    };
    let seed = run
        .id
        .as_str()
        .bytes()
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    let deadline = observed_at_unix_ms
        .get()
        .checked_add(base + seed % (base / 5 + 1))?;
    let deadline = deadline.max(retry_after_unix_ms.as_ref().map_or(0, |v| v.get()));
    let deadline = deadline.max(
        run.rate_limit
            .as_ref()
            .filter(|v| super::rate_limits::valid_observation(v))
            .map_or(0, |v| v.resets_at_unix_ms.get()),
    );
    (deadline <= i64::MAX as u64).then_some(deadline)
}

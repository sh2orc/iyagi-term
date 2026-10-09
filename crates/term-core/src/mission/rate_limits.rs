//! Known provider reset windows, scoped to the exact configured connection.
use term_contracts::mission::types::{Binding, RateLimitObservation, Run};

pub fn valid_observation(value: &RateLimitObservation) -> bool {
    value.observed_at_unix_ms.get() < value.resets_at_unix_ms.get()
        && value.resets_at_unix_ms.get() <= 253_402_300_799_999 // UTC year 9999
}

pub fn same_connection(a: &Binding, b: &Binding) -> bool {
    a.id == b.id
        && a.runtime == b.runtime
        && a.program == b.program
        && a.provider_id == b.provider_id
        && a.model_id == b.model_id
        && a.auth_route == b.auth_route
        && a.credential_ref == b.credential_ref
        && a.endpoint_ref == b.endpoint_ref
}

/// Include archived and terminal runs: archiving a mission does not reset quotas.
/// A label, estimate, or capability probe edit does not change account identity.
pub fn reset_deadline(binding: &Binding, observations: &[Run], now_ms: u64) -> Option<u64> {
    observations
        .iter()
        .filter_map(|run| {
            let previous = run.binding_snapshot.as_ref()?;
            let value = run.rate_limit.as_ref()?;
            (same_connection(binding, previous)
                && valid_observation(value)
                && value.observed_at_unix_ms.get() <= now_ms
                && value.resets_at_unix_ms.get() > now_ms)
                .then_some(value.resets_at_unix_ms.get())
        })
        .max()
}

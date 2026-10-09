//! Supervised resident threads.
//!
//! A panic or an early return in a daemon-critical loop (telemetry,
//! scheduler, retention, …) used to end that loop silently: telemetry
//! staleness then blocked every launch with WAIT_TELEMETRY forever, and a
//! dead scheduler never advanced the queue while RPCs kept accepting
//! changes. Supervised loops catch their own panics and restart with
//! exponential backoff; a clean `return` counts as an exit only while
//! shutdown was requested — anything else is a bug and also restarts.
//!
//! The supervisor thread itself is spawned with `panic!` on failure: a
//! startup-time thread-exhaustion is fatal by the same contract as before,
//! just concentrated in one place instead of scattered `.expect` sites.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::state::DaemonState;

const RESTART_MIN: Duration = Duration::from_secs(1);
const RESTART_MAX: Duration = Duration::from_secs(60);
/// A loop that ran at least this long before dying is considered healthy —
/// its next restart begins at the minimum backoff, not the accumulated one.
const HEALTHY_RUN: Duration = Duration::from_secs(60);

/// Spawn `body` as a supervised resident loop. `body` must be re-runnable
/// (plain `Fn` over the shared state; per-run resources are created inside).
pub fn spawn_supervised<F>(state: &Arc<DaemonState>, name: &str, body: F)
where
    F: Fn(Arc<DaemonState>) + Send + Sync + 'static,
{
    let state = Arc::clone(state);
    let owned_name = name.to_string();
    thread::Builder::new()
        .name(format!("sup:{owned_name}"))
        .spawn(move || {
            let shutdown = state.shutdown.subscribe();
            let mut backoff = RESTART_MIN;
            loop {
                if *shutdown.borrow() {
                    return;
                }
                let started = Instant::now();
                let attempt = {
                    let state = Arc::clone(&state);
                    let body = &body;
                    move || body(state)
                };
                match std::panic::catch_unwind(AssertUnwindSafe(attempt)) {
                    Ok(()) => {
                        if *shutdown.borrow() {
                            return;
                        }
                        tracing::warn!(
                            thread = %owned_name,
                            "resident loop returned without shutdown — restarting"
                        );
                    }
                    Err(payload) => {
                        tracing::error!(
                            thread = %owned_name,
                            panic = %panic_message(&payload),
                            "resident loop panicked — restarting with backoff"
                        );
                    }
                }
                if started.elapsed() >= HEALTHY_RUN {
                    backoff = RESTART_MIN;
                }
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(RESTART_MAX);
            }
        })
        .unwrap_or_else(|error| panic!("spawn supervised loop {name}: {error}"));
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "unknown panic payload".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_message_downcasts_common_payloads() {
        let text: Box<dyn std::any::Any + Send> = Box::new("boom");
        assert_eq!(panic_message(&text), "boom");
        let text: Box<dyn std::any::Any + Send> = Box::new("boom".to_string());
        assert_eq!(panic_message(&text), "boom");
        let number: Box<dyn std::any::Any + Send> = Box::new(42u8);
        assert_eq!(panic_message(&number), "unknown panic payload");
    }
}

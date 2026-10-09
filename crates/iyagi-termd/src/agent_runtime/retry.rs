//! Evidence produced only before an adapter submits the task, never inferred
//! from a provider's error text or a missing process after a disconnect.
use super::AdapterEvent;
use std::io;
use term_contracts::mission::{types::Id, MissionErrorCode};

#[derive(Debug)]
struct BeforeSubmission(io::Error);
impl std::fmt::Display for BeforeSubmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for BeforeSubmission {}

pub(crate) fn mark_before_submission(error: io::Error) -> io::Error {
    if matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::Interrupted
            | io::ErrorKind::AddrInUse
    ) {
        io::Error::new(error.kind(), BeforeSubmission(error))
    } else {
        error
    }
}

pub(crate) fn has_submission_proof(error: &io::Error) -> bool {
    error.get_ref().is_some_and(|e| e.is::<BeforeSubmission>())
}

pub(crate) fn failure(
    run_id: Id,
    fencing_token: u64,
    code: MissionErrorCode,
    message: String,
) -> AdapterEvent {
    AdapterEvent::FailedBeforeSubmission {
        run_id,
        fencing_token,
        code,
        message,
        observed_at_unix_ms: super::rate_limits::unix_millis(),
        retry_after_unix_ms: None,
    }
}

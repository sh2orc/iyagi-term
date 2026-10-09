//! Session seq/journal byte caps: at i64::MAX the next update is an explicit
//! error and the stored value never wraps (spec §1). Backwards sequences are
//! rejected.

mod common;

use common::{launch_intent, open};
use term_storage::StorageError;

#[test]
fn seq_cap_errors_without_wrapping() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    let storage = open(&path).unwrap();

    let intent = launch_intent("seq-cap");
    storage.record_launch_intent(intent.clone()).unwrap();

    // Progress to just below the cap, then to the cap.
    storage
        .update_session_progress(&intent.session_id, i64::MAX as u64 - 1, 1000)
        .unwrap();
    storage
        .update_session_progress(&intent.session_id, i64::MAX as u64, 1000)
        .unwrap();

    // One past the SQLite bound: explicit error, value unchanged.
    let err = storage
        .update_session_progress(&intent.session_id, i64::MAX as u64 + 1, 1000)
        .unwrap_err();
    assert!(
        matches!(err, StorageError::SeqOverflow { attempted, .. } if attempted == i64::MAX as u64 + 1),
        "{err:?}"
    );
    let sessions = storage.sessions().unwrap();
    assert_eq!(sessions[0].last_seq, i64::MAX as u64);
    assert_eq!(sessions[0].journal_bytes, 1000);

    // The very same value is an idempotent no-op, not an error.
    storage
        .update_session_progress(&intent.session_id, i64::MAX as u64, 1000)
        .unwrap();
}

#[test]
fn journal_bytes_cap_errors_too() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    let storage = open(&path).unwrap();

    let intent = launch_intent("bytes-cap");
    storage.record_launch_intent(intent.clone()).unwrap();

    let err = storage
        .update_session_progress(&intent.session_id, 5, i64::MAX as u64 + 1)
        .unwrap_err();
    assert!(
        matches!(err, StorageError::JournalBytesOverflow { .. }),
        "{err:?}"
    );
    let sessions = storage.sessions().unwrap();
    assert_eq!(sessions[0].last_seq, 0);
    assert_eq!(sessions[0].journal_bytes, 0);
}

#[test]
fn seq_never_moves_backwards() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    let storage = open(&path).unwrap();

    let intent = launch_intent("seq-back");
    storage.record_launch_intent(intent.clone()).unwrap();

    storage
        .update_session_progress(&intent.session_id, 10, 500)
        .unwrap();
    let err = storage
        .update_session_progress(&intent.session_id, 9, 500)
        .unwrap_err();
    assert!(
        matches!(
            err,
            StorageError::SeqRegression {
                stored: 10,
                attempted: 9,
                ..
            }
        ),
        "{err:?}"
    );
    let sessions = storage.sessions().unwrap();
    assert_eq!(sessions[0].last_seq, 10);

    // journal_bytes may shrink (retention truncation) alongside a growing seq.
    storage
        .update_session_progress(&intent.session_id, 11, 200)
        .unwrap();
    let sessions = storage.sessions().unwrap();
    assert_eq!(sessions[0].last_seq, 11);
    assert_eq!(sessions[0].journal_bytes, 200);
}

#[test]
fn unknown_session_is_not_found() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("meta.db3");
    let storage = open(&path).unwrap();

    let missing = launch_intent("missing-session").session_id;
    assert!(matches!(
        storage.update_session_progress(&missing, 1, 1).unwrap_err(),
        StorageError::SessionNotFound { .. }
    ));
}

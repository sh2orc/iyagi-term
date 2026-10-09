//! I07 integration tests: MTJ1 journal codec, budgets, replay, and the
//! output flow controller with its ACK ledger (spec `02-runner.md` §4–5).

use std::fs;

use uuid::Uuid;

use term_contracts::ViewId;
use term_pty::flow::{
    AckCoalescer, AckOutcome, FlowController, FlowError, FlowTransition, GlobalOutputBudget,
    SentRecord,
};
use term_pty::journal::{
    encode_record, GlobalJournalBudget, JournalFlowError, JournalReader, JournalRecord,
    JournalRecordKind, JournalWriter, ScanStatus, DEFAULT_SESSION_LIMIT, HEADER_LEN, MAGIC,
    MAX_OUTPUT_PAYLOAD,
};

/// Deterministic output payload, distinct per (seed, length).
fn pattern(seed: u64, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| {
            (seed as u8)
                .wrapping_mul(29)
                .wrapping_add(i as u8)
                .wrapping_mul(37)
                .wrapping_add(7)
        })
        .collect()
}

/// Full output chunk (16 KiB).
fn chunk(index: u64) -> Vec<u8> {
    pattern(index, MAX_OUTPUT_PAYLOAD)
}

const RECORD_OUTPUT_BYTES: u64 = 4 + 13 + MAX_OUTPUT_PAYLOAD as u64; // 16_401
const RECORD_RESIZE_BYTES: u64 = 4 + 13 + 4; // 21

fn file_len(path: &std::path::Path) -> u64 {
    fs::metadata(path).unwrap().len()
}

// ---------------------------------------------------------------------------
// Journal codec
// ---------------------------------------------------------------------------

#[test]
fn journal_round_trip_is_byte_exact_and_ordered() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("round.mtj");
    let session = Uuid::new_v4();

    let mut writer = JournalWriter::open(&path, session).unwrap();
    assert_eq!(writer.append_resize(120, 40).unwrap(), 1);
    assert_eq!(writer.append_output(&pattern(1, 1_500)).unwrap(), 2);
    assert_eq!(writer.append_resize(200, 50).unwrap(), 3);
    assert_eq!(writer.append_output(&chunk(2)).unwrap(), 4);
    writer.finalize().unwrap();
    assert_eq!(writer.last_seq(), 4);

    // Header + first record are byte-exact against a hand-crafted frame.
    let bytes = fs::read(&path).unwrap();
    assert_eq!(&bytes[..4], &MAGIC[..]);
    assert_eq!(&bytes[4..HEADER_LEN], session.as_bytes());
    let mut expected = Vec::new();
    expected.extend_from_slice(&17u32.to_le_bytes());
    expected.extend_from_slice(&1u64.to_le_bytes());
    expected.push(2);
    expected.extend_from_slice(&120u16.to_le_bytes());
    expected.extend_from_slice(&40u16.to_le_bytes());
    expected.extend_from_slice(&crc32fast::hash(&expected[4..]).to_le_bytes());
    assert_eq!(&bytes[HEADER_LEN..HEADER_LEN + 21], &expected[..]);
    assert_eq!(
        encode_record(1, JournalRecordKind::Resize, &[120, 0, 40, 0]),
        expected
    );

    // Reader classification and exact replay (outputs and resizes interleaved).
    let reader = JournalReader::open(&path).unwrap();
    assert_eq!(reader.status(), ScanStatus::Ok);
    assert!(!reader.tail_truncated());
    assert_eq!(reader.session_uuid(), session);
    assert_eq!(reader.record_count(), 4);
    assert_eq!(reader.last_seq(), 4);
    assert_eq!(reader.journal_bytes(), bytes.len() as u64);

    let records = reader.replay(1, 4).unwrap();
    let expected_records = vec![
        JournalRecord::resize(1, 120, 40),
        JournalRecord::output(2, &pattern(1, 1_500)),
        JournalRecord::resize(3, 200, 50),
        JournalRecord::output(4, &chunk(2)),
    ];
    assert_eq!(records, expected_records);

    // Streaming variant equals the collected variant; sub-ranges work.
    let streamed: Vec<_> = reader.iterate(1, 4).unwrap().map(Result::unwrap).collect();
    assert_eq!(streamed, records);
    assert_eq!(reader.replay(2, 3).unwrap().len(), 2);
    assert_eq!(reader.replay(2, 3).unwrap()[0].seq, 2);
    // Out-of-range request yields an empty replay, not an error.
    assert!(reader.replay(99, 100).unwrap().is_empty());
}

#[test]
fn flood_16mib_replays_identically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flood.mtj");
    let mut writer = JournalWriter::open(&path, Uuid::new_v4()).unwrap();
    writer.append_resize(80, 24).unwrap();
    let mut resize_seq = 0;
    let mut next_seq = 2u64; // first output follows the initial resize
    for index in 0..1024u64 {
        let seq = writer.append_output(&chunk(index)).unwrap();
        assert_eq!(seq, next_seq);
        next_seq += 1;
        if index == 511 {
            resize_seq = writer.append_resize(132, 43).unwrap();
            assert_eq!(resize_seq, next_seq);
            next_seq += 1;
        }
    }
    let last_seq = writer.last_seq();
    assert_eq!(resize_seq, 514);
    assert_eq!(last_seq, 1026);
    let total =
        HEADER_LEN as u64 + RECORD_RESIZE_BYTES + 1024 * RECORD_OUTPUT_BYTES + RECORD_RESIZE_BYTES;
    assert_eq!(writer.journal_bytes(), total);
    writer.finalize().unwrap();
    assert_eq!(file_len(&path), total);

    let reader = JournalReader::open(&path).unwrap();
    assert_eq!(reader.status(), ScanStatus::Ok);
    assert_eq!(reader.record_count(), 1026);
    assert_eq!(reader.journal_bytes(), total);

    let records = reader.replay(1, last_seq).unwrap();
    assert_eq!(records.len(), 1026);
    let mut output_bytes = 0u64;
    for (position, record) in records.iter().enumerate() {
        assert_eq!(record.seq, position as u64 + 1); // contiguous from 1
        if let Some(bytes) = record.output_bytes() {
            output_bytes += bytes.len() as u64;
        }
    }
    assert_eq!(output_bytes, 16 * 1024 * 1024);
    // records[k].seq == k+1: seq 513 is output i=511, seq 514 the resize,
    // seq 515 output i=512.
    assert_eq!(records[512].output_bytes(), Some(&chunk(511)[..]));
    assert_eq!(records[513].resize_dims(), Some((132, 43)));
    assert_eq!(records[514].output_bytes(), Some(&chunk(512)[..]));
}

#[test]
fn flush_tick_writes_at_most_every_250ms() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flush.mtj");
    let mut writer = JournalWriter::open(&path, Uuid::new_v4()).unwrap();
    writer.append_resize(80, 24).unwrap();
    for index in 0..3 {
        writer.append_output(&chunk(index)).unwrap();
    }
    let buffered_total = HEADER_LEN as u64 + RECORD_RESIZE_BYTES + 3 * RECORD_OUTPUT_BYTES;
    // Nothing has reached the OS before a flush.
    assert_eq!(file_len(&path), 0);
    // First tick flushes and anchors the 250 ms cadence.
    assert!(writer.flush_tick(1_000).unwrap());
    assert_eq!(file_len(&path), buffered_total);
    // New data stays buffered inside the interval.
    writer.append_output(&pattern(9, 100)).unwrap();
    assert!(!writer.flush_tick(1_100).unwrap()); // +100 ms
    assert_eq!(file_len(&path), buffered_total);
    assert!(!writer.flush_tick(1_249).unwrap()); // +249 ms
    assert_eq!(file_len(&path), buffered_total);
    assert!(writer.flush_tick(1_250).unwrap()); // +250 ms
    assert_eq!(file_len(&path), buffered_total + 4 + 13 + 100);
    // Clean (nothing dirty) tick is a no-op.
    assert!(!writer.flush_tick(10_000).unwrap());
}

#[test]
fn dropping_the_writer_flushes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("drop.mtj");
    let session = Uuid::new_v4();
    {
        let mut writer = JournalWriter::open(&path, session).unwrap();
        writer.append_resize(80, 24).unwrap();
        writer.append_output(&chunk(0)).unwrap();
        writer.append_output(&chunk(1)).unwrap();
    }
    let total = HEADER_LEN as u64 + RECORD_RESIZE_BYTES + 2 * RECORD_OUTPUT_BYTES;
    assert_eq!(file_len(&path), total);
    let reader = JournalReader::open_with_session(&path, session).unwrap();
    assert_eq!(reader.status(), ScanStatus::Ok);
    assert_eq!(reader.record_count(), 3);
    assert_eq!(reader.replay(1, 3).unwrap().len(), 3);
}

#[test]
fn torn_tail_is_excluded_but_prefix_replays() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("torn.mtj");
    let mut writer = JournalWriter::open(&path, Uuid::new_v4()).unwrap();
    writer.append_resize(80, 24).unwrap();
    for index in 0..3 {
        writer.append_output(&chunk(index)).unwrap();
    }
    writer.finalize().unwrap();
    let total = file_len(&path);

    // Truncate inside the last record's body.
    let torn = dir.path().join("torn-truncated.mtj");
    let bytes = fs::read(&path).unwrap();
    let mut torn_bytes = bytes.clone();
    torn_bytes.truncate((total - 10) as usize);
    fs::write(&torn, &torn_bytes).unwrap();

    let reader = JournalReader::open(&torn).unwrap();
    assert_eq!(reader.status(), ScanStatus::TailTruncated);
    assert!(reader.tail_truncated());
    assert_eq!(reader.last_seq(), 3);
    assert_eq!(reader.record_count(), 3);
    // Replay clamps to the verified prefix (resize + outputs 0 and 1);
    // earlier records stay intact, the torn chunk(2) record is excluded.
    let records = reader.replay(1, 4).unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(records[2].seq, 3);
    assert_eq!(records[2].output_bytes(), Some(&chunk(1)[..]));

    // A partial record header (2 of the 4 body_len bytes) is also a torn tail.
    let fragment = dir.path().join("torn-fragment.mtj");
    fs::write(&fragment, &bytes[..HEADER_LEN + 2]).unwrap();
    let fragment_reader = JournalReader::open(&fragment).unwrap();
    assert_eq!(fragment_reader.status(), ScanStatus::TailTruncated);
    assert_eq!(fragment_reader.record_count(), 0);
}

#[test]
fn mid_file_crc_corruption_stops_replay_at_that_seq() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("crc.mtj");
    let mut writer = JournalWriter::open(&path, Uuid::new_v4()).unwrap();
    writer.append_resize(80, 24).unwrap();
    for index in 0..4 {
        writer.append_output(&chunk(index)).unwrap();
    }
    writer.finalize().unwrap();

    // Flip one payload byte inside record 3 (after header + resize + one output).
    let mut bytes = fs::read(&path).unwrap();
    let record3_payload =
        HEADER_LEN + RECORD_RESIZE_BYTES as usize + RECORD_OUTPUT_BYTES as usize + 4 + 8 + 1;
    bytes[record3_payload + 5] ^= 0xFF;
    fs::write(&path, &bytes).unwrap();

    let reader = JournalReader::open(&path).unwrap();
    assert_eq!(reader.status(), ScanStatus::Corrupt { at_seq: 3 });
    assert!(!reader.tail_truncated());
    assert_eq!(reader.last_seq(), 2);
    assert_eq!(reader.record_count(), 2);
    // The damaged remainder is never returned as data.
    assert!(matches!(
        reader.replay(1, 5),
        Err(JournalFlowError::Corrupt { at_seq: 3 })
    ));
    assert!(matches!(
        reader.replay(3, 5),
        Err(JournalFlowError::Corrupt { at_seq: 3 })
    ));
    // The intact prefix still replays.
    let records = reader.replay(1, 2).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[1].output_bytes(), Some(&chunk(0)[..]));
}

#[test]
fn seq_gap_is_corruption_at_the_missing_seq() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gap.mtj");
    let session = Uuid::new_v4();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(session.as_bytes());
    bytes.extend(encode_record(1, JournalRecordKind::Resize, &[80, 0, 24, 0]));
    // Valid CRC, but seq jumps from 1 to 3.
    bytes.extend(encode_record(3, JournalRecordKind::Output, b"gap"));
    fs::write(&path, &bytes).unwrap();

    let reader = JournalReader::open(&path).unwrap();
    assert_eq!(reader.status(), ScanStatus::Corrupt { at_seq: 2 });
    assert_eq!(reader.record_count(), 1);
    assert_eq!(reader.replay(1, 1).unwrap().len(), 1);
    assert!(matches!(
        reader.replay(1, 3),
        Err(JournalFlowError::Corrupt { at_seq: 2 })
    ));
}

#[test]
fn output_first_record_is_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("output-first.mtj");
    let session = Uuid::new_v4();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(session.as_bytes());
    bytes.extend(encode_record(1, JournalRecordKind::Output, b"early"));
    fs::write(&path, &bytes).unwrap();

    let reader = JournalReader::open(&path).unwrap();
    assert_eq!(reader.status(), ScanStatus::Corrupt { at_seq: 1 });
    assert_eq!(reader.record_count(), 0);
}

#[test]
fn header_failures_are_open_errors() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.mtj");
    let session = Uuid::new_v4();
    let mut writer = JournalWriter::open(&good, session).unwrap();
    writer.append_resize(80, 24).unwrap();
    writer.finalize().unwrap();

    // Wrong magic.
    let bad_magic = dir.path().join("bad-magic.mtj");
    let mut bytes = fs::read(&good).unwrap();
    bytes[0] = b'X';
    fs::write(&bad_magic, &bytes).unwrap();
    assert!(matches!(
        JournalReader::open(&bad_magic),
        Err(JournalFlowError::BadHeader { .. })
    ));

    // File shorter than the header.
    let short = dir.path().join("short.mtj");
    fs::write(&short, &bytes[..10]).unwrap();
    assert!(matches!(
        JournalReader::open(&short),
        Err(JournalFlowError::BadHeader { .. })
    ));

    // Header UUID mismatch.
    assert!(matches!(
        JournalReader::open_with_session(&good, Uuid::new_v4()),
        Err(JournalFlowError::HeaderUuidMismatch { .. })
    ));
    // Matching UUID opens fine.
    assert!(JournalReader::open_with_session(&good, session).is_ok());
}

// ---------------------------------------------------------------------------
// Journal budgets
// ---------------------------------------------------------------------------

#[test]
fn session_cap_stops_the_journal_at_the_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capped.mtj");
    let session_limit = 1024 * 1024u64;
    let mut writer = JournalWriter::with_budget(
        &path,
        Uuid::new_v4(),
        session_limit,
        GlobalJournalBudget::shared_default(),
    )
    .unwrap();
    writer.append_resize(80, 24).unwrap();
    let mut accepted = 0u64;
    loop {
        match writer.append_output(&chunk(accepted)) {
            Ok(_) => accepted += 1,
            Err(error) => {
                assert!(matches!(
                    error,
                    JournalFlowError::SessionCap { limit } if limit == session_limit
                ));
                break;
            }
        }
    }
    // 20-byte header + resize + 63 outputs fit; the 64th would exceed 1 MiB.
    assert_eq!(accepted, 63);
    let written = HEADER_LEN as u64 + RECORD_RESIZE_BYTES + 63 * RECORD_OUTPUT_BYTES;
    assert!(written <= session_limit);
    assert!(written + RECORD_OUTPUT_BYTES > session_limit);
    assert_eq!(writer.journal_bytes(), written);
    writer.finalize().unwrap();
    // The journal stops growing exactly at the last complete record.
    assert_eq!(file_len(&path), written);
    let reader = JournalReader::open(&path).unwrap();
    assert_eq!(reader.status(), ScanStatus::Ok);
    assert_eq!(reader.record_count(), 64);
}

#[test]
fn global_budget_is_shared_across_writers() {
    let dir = tempfile::tempdir().unwrap();
    let global = GlobalJournalBudget::shared(64 * 1024);
    let path_a = dir.path().join("a.mtj");
    let path_b = dir.path().join("b.mtj");
    let mut a = JournalWriter::with_budget(
        &path_a,
        Uuid::new_v4(),
        DEFAULT_SESSION_LIMIT,
        global.clone(),
    )
    .unwrap();
    a.append_resize(80, 24).unwrap();
    for index in 0..3 {
        a.append_output(&chunk(index)).unwrap();
    }
    let used_after_a = HEADER_LEN as u64 + RECORD_RESIZE_BYTES + 3 * RECORD_OUTPUT_BYTES;
    assert_eq!(global.lock().unwrap().used(), used_after_a);

    // The second writer's header + initial size also come out of the shared
    // budget.
    let mut b = JournalWriter::with_budget(
        &path_b,
        Uuid::new_v4(),
        DEFAULT_SESSION_LIMIT,
        global.clone(),
    )
    .unwrap();
    b.append_resize(100, 30).unwrap();
    assert_eq!(
        global.lock().unwrap().used(),
        used_after_a + HEADER_LEN as u64 + RECORD_RESIZE_BYTES
    );
    // B's next chunk does not fit the shared 64 KiB budget.
    assert!(matches!(
        b.append_output(&chunk(0)),
        Err(JournalFlowError::GlobalCap { limit }) if limit == 64 * 1024
    ));
    // The failed attempt reserved nothing extra.
    assert_eq!(
        global.lock().unwrap().used(),
        used_after_a + HEADER_LEN as u64 + RECORD_RESIZE_BYTES
    );
    // A smaller record still fits, and both files stay readable.
    a.append_output(&pattern(7, 100)).unwrap();
    drop(a);
    drop(b);
    assert_eq!(JournalReader::open(&path_a).unwrap().record_count(), 5);
    assert_eq!(JournalReader::open(&path_b).unwrap().record_count(), 1);
}

// ---------------------------------------------------------------------------
// Flow control
// ---------------------------------------------------------------------------

fn new_view() -> ViewId {
    ViewId::generate()
}

#[test]
fn slow_consumer_blocks_only_its_view_and_resumes_below_low_watermark() {
    let mut controller = FlowController::new();
    let view = new_view();
    controller.attach_view(view.clone(), "epoch-1");
    assert!(controller.can_send(&view));
    assert_eq!(controller.view_epoch(&view).as_deref(), Some("epoch-1"));

    // Fill up to exactly the 256 KiB high watermark; the transition fires once.
    let mut blocked = None;
    for seq in 1..=16u64 {
        let transition = controller
            .record_sent(
                &view,
                SentRecord {
                    seq,
                    raw_len: 16_384,
                },
            )
            .unwrap();
        if transition.is_some() {
            assert!(blocked.is_none(), "transition must fire once");
            blocked = transition;
        }
    }
    assert_eq!(
        blocked,
        Some(FlowTransition::Blocked { view: view.clone() })
    );
    assert!(!controller.can_send(&view));
    assert!(!controller.view_blocked(&new_view())); // unknown view: not blocked
    assert_eq!(controller.unacked_bytes(&view), Some(262_144));
    assert_eq!(
        controller.budget().lock().unwrap().raw_used(),
        262_144,
        "each unacked raw byte holds one raw reservation"
    );

    // Another view on the same session is unaffected.
    let other = new_view();
    controller.attach_view(other.clone(), "epoch-2");
    assert!(controller.can_send(&other));

    // ACK through seq 4: unacked drops to 192 KiB, still inside hysteresis.
    match controller.on_ack(&view, "epoch-1", 4).unwrap() {
        AckOutcome::Advanced {
            released_bytes,
            unblocked_views,
            ..
        } => {
            assert_eq!(released_bytes, 4 * 16_384);
            assert!(unblocked_views.is_empty());
        }
        other => panic!("expected advance, got {other:?}"),
    }
    assert!(!controller.can_send(&view));

    // ACK through seq 12: unacked reaches the 64 KiB low watermark -> resume.
    match controller.on_ack(&view, "epoch-1", 12).unwrap() {
        AckOutcome::Advanced {
            unblocked_views, ..
        } => {
            assert_eq!(unblocked_views, vec![view.clone()]);
        }
        other => panic!("expected advance, got {other:?}"),
    }
    assert!(controller.can_send(&view));

    // Duplicate/lower ACK is a no-op (no double release).
    assert_eq!(
        controller.on_ack(&view, "epoch-1", 12).unwrap(),
        AckOutcome::Ignored
    );
    assert_eq!(
        controller.on_ack(&view, "epoch-1", 5).unwrap(),
        AckOutcome::Ignored
    );
    assert_eq!(controller.budget().lock().unwrap().raw_used(), 4 * 16_384);

    // Future/unsent seq is a protocol error.
    assert!(matches!(
        controller.on_ack(&view, "epoch-1", 17),
        Err(FlowError::ProtocolError { .. })
    ));

    // ACK for an older epoch after rotation is ignored; rotation also
    // releases the old epoch's outstanding reservations.
    controller.attach_view(view.clone(), "epoch-9");
    assert_eq!(
        controller.on_ack(&view, "epoch-1", 16).unwrap(),
        AckOutcome::Ignored
    );
    assert_eq!(controller.budget().lock().unwrap().raw_used(), 0);
    assert!(controller.can_send(&view), "fresh epoch starts unblocked");
}

#[test]
fn global_raw_budget_eventually_blocks_every_view() {
    let budget = GlobalOutputBudget::shared_with_limits(128 * 1024, 24 * 1024 * 1024);
    let mut controller = FlowController::with_budget(budget.clone());
    let a = new_view();
    let b = new_view();
    controller.attach_view(a.clone(), "e-a");
    controller.attach_view(b.clone(), "e-b");

    for seq in 1..=4u64 {
        controller
            .record_sent(
                &a,
                SentRecord {
                    seq,
                    raw_len: 16_384,
                },
            )
            .unwrap();
        controller
            .record_sent(
                &b,
                SentRecord {
                    seq,
                    raw_len: 16_384,
                },
            )
            .unwrap();
    }
    assert_eq!(budget.lock().unwrap().raw_used(), 128 * 1024);
    assert!(!controller.can_send(&a), "raw budget full blocks view A");
    assert!(!controller.can_send(&b), "raw budget full blocks view B");
    // Views are below their own watermarks; the global budget is the blocker.
    assert!(!controller.view_blocked(&a));
    assert!(!controller.view_blocked(&b));
    // Sending anyway is rejected without side effects.
    assert!(matches!(
        controller.record_sent(
            &a,
            SentRecord {
                seq: 5,
                raw_len: 16_384
            }
        ),
        Err(FlowError::RawBudgetExhausted { .. })
    ));
    assert_eq!(budget.lock().unwrap().raw_used(), 128 * 1024);

    // Freeing one view's window unblocks the other.
    controller.on_ack(&b, "e-b", 4).unwrap();
    assert_eq!(budget.lock().unwrap().raw_used(), 64 * 1024);
    assert!(controller.can_send(&a));
}

#[test]
fn transport_budget_counts_each_copy_and_blocks_sending() {
    let chunk_transport = GlobalOutputBudget::base64_len(16_384);
    let budget = GlobalOutputBudget::shared_with_limits(24 * 1024 * 1024, 2 * chunk_transport);
    let mut controller = FlowController::with_budget(budget.clone());
    let view = new_view();
    controller.attach_view(view.clone(), "e1");

    controller
        .record_sent(
            &view,
            SentRecord {
                seq: 1,
                raw_len: 16_384,
            },
        )
        .unwrap();
    controller
        .record_sent(
            &view,
            SentRecord {
                seq: 2,
                raw_len: 16_384,
            },
        )
        .unwrap();
    assert_eq!(budget.lock().unwrap().transport_used(), 2 * chunk_transport);
    assert_eq!(budget.lock().unwrap().raw_used(), 32_768);
    // One more base64 copy would not fit: view below watermark, yet blocked.
    assert!(!controller.view_blocked(&view));
    assert!(!controller.can_send(&view));
    assert!(matches!(
        controller.record_sent(
            &view,
            SentRecord {
                seq: 3,
                raw_len: 16_384
            }
        ),
        Err(FlowError::TransportBudgetExhausted { .. })
    ));
    // ACK of one record frees exactly one transport copy.
    controller.on_ack(&view, "e1", 1).unwrap();
    assert_eq!(budget.lock().unwrap().transport_used(), chunk_transport);
    assert!(controller.can_send(&view));
}

#[test]
fn detached_views_release_their_reservations() {
    let mut controller = FlowController::new();
    let view = new_view();
    controller.attach_view(view.clone(), "e1");
    controller
        .record_sent(
            &view,
            SentRecord {
                seq: 1,
                raw_len: 1_000,
            },
        )
        .unwrap();
    assert_eq!(controller.budget().lock().unwrap().raw_used(), 1_000);
    assert!(controller.detach_view(&view));
    assert_eq!(controller.budget().lock().unwrap().raw_used(), 0);
    assert!(!controller.can_send(&view));
    assert!(!controller.detach_view(&view));
    // Unknown view ACKs are ignored, not errors.
    assert_eq!(
        controller.on_ack(&view, "e1", 1).unwrap(),
        AckOutcome::Ignored
    );
}

#[test]
fn ack_coalescing_fires_at_16ms_or_64kib() {
    let mut coalescer = AckCoalescer::new(0);
    assert!(!coalescer.coalesce_due(15, 100));
    assert!(coalescer.coalesce_due(16, 100));
    coalescer.mark_sent(100);
    assert!(!coalescer.coalesce_due(115, 200));
    assert!(coalescer.coalesce_due(116, 200));
    // 64 KiB is due immediately, regardless of the clock.
    assert!(coalescer.coalesce_due(0, 65_536));
    // Nothing pending is never due.
    assert!(!coalescer.coalesce_due(10_000, 0));
}

//! 저널 검색 스캔 헬퍼의 종단 시험(W2 `session.search`).
//!
//! 핵심 계약: 배치(256레코드) 경계를 지나도 프레이밍 오프셋이 정확해
//! 끝까지 스캔한다 — 길이 접두어(4바이트)를 빠뜨린 구 버전은 두 번째
//! 배치에서 레코드 중간에 떨어져 조용히 멈추던 결함이었다.

use term_contracts::session::SessionSearchParams;
use term_pty::journal::JournalWriter;

fn write_journal(path: &std::path::Path, lines: usize, needle_every: usize) {
    let mut writer = JournalWriter::open(path, uuid::Uuid::new_v4()).expect("open journal");
    writer.append_resize(80, 24).expect("initial size record");
    for i in 1..=lines {
        let payload = if i % needle_every == 0 {
            format!("line {i}: NEEDLE-HIT error text\n")
        } else {
            format!("line {i}: ordinary output\n")
        };
        writer.append_output(payload.as_bytes()).expect("append");
        if i % 64 == 0 {
            writer.flush_now().expect("flush");
        }
    }
    writer.flush_now().expect("final flush");
}

fn params(query: &str) -> SessionSearchParams {
    SessionSearchParams {
        query: query.to_string(),
        case_sensitive: false,
        session_id: None,
    }
}

#[test]
fn scans_past_batch_boundaries_to_the_end() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("big.mtj");
    // 600 records: BATCH_RECORDS(256)를 두 번 넘는다. 100·300·600번째에만
    // 일치 → 경계(256·512) 뒤에 있는 300·600을 못 찾으면 실패다.
    write_journal(&path, 600, 150);

    let outcome =
        iyagi_termd_lib::search_scan::scan_journal(&path, &params("needle-hit"), "needle-hit");
    assert!(!outcome.truncated, "예산 안에서 끝까지 스캔해야 한다");
    let seqs: Vec<u64> = outcome.matches.iter().map(|(seq, _)| *seq).collect();
    // 초기 resize 레코드가 seq 1을 쓰므로 출력 i의 seq는 i+1.
    assert_eq!(seqs, vec![151, 301, 451, 601], "배치 경계 뒤 일치도 찾는다");
    // 발췌 줄에 원문 문맥이 담긴다.
    assert!(outcome.matches[0].1.contains("NEEDLE-HIT"));
}

#[test]
fn case_insensitive_and_case_sensitive_paths_agree_on_indices() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("case.mtj");
    // 'İ'(U+0130)의 to_lowercase는 길이가 변한다 — 접힌 haystack 인덱스를
    // 원문에 적용하면 panic이 나야 했던 경로.
    let mut writer = JournalWriter::open(&path, uuid::Uuid::new_v4()).unwrap();
    writer.append_resize(80, 24).unwrap();
    writer
        .append_output("İstem kontrolü: NEEDLE here\n".as_bytes())
        .unwrap();
    writer.flush_now().unwrap();

    let outcome = iyagi_termd_lib::search_scan::scan_journal(&path, &params("needle"), "needle");
    assert_eq!(outcome.matches.len(), 1);
    // 'İ' 접힘으로 길이가 달라 발췌는 haystack에서 나온다 — panic 없이.
    assert!(outcome.matches[0].1.to_lowercase().contains("needle"));
}

#[test]
fn respects_match_cap_and_reports_truncation() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("many.mtj");
    // 매 레코드마다 일치 → 상한(50)에서 멈추고 truncated를 알린다.
    write_journal(&path, 80, 1);

    let outcome =
        iyagi_termd_lib::search_scan::scan_journal(&path, &params("needle-hit"), "needle-hit");
    assert!(outcome.matches.len() <= 50);
    assert!(outcome.truncated);
}

#[test]
fn empty_and_absent_journals_are_noop() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("none.mtj");
    let outcome = iyagi_termd_lib::search_scan::scan_journal(&path, &params("x"), "x");
    assert!(outcome.matches.is_empty());
    assert!(!outcome.truncated);
}

#[test]
fn diagnostic_offsets() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("diag.mtj");
    let mut writer = JournalWriter::open(&path, uuid::Uuid::new_v4()).unwrap();
    writer.append_resize(80, 24).unwrap();
    for _ in 0..4 {
        writer.append_output(b"payload-0123456789\n").unwrap();
    }
    writer.flush_now().unwrap();
    let file_len = std::fs::metadata(&path).unwrap().len();

    // 상대 오프셋 계약: scan_window*의 반환 오프셋은 seek 시작 기준.
    let first = term_pty::journal::JournalReader::scan_window(
        &path,
        term_pty::journal::HEADER_LEN as u64,
        1,
        u64::MAX,
        2,
    )
    .unwrap();
    let (second_record, second_relative) = first.last().unwrap();
    assert_eq!(second_record.seq, 2);
    let second_size = (4 + 13 + second_record.payload.len()) as u64;
    let resume_at = term_pty::journal::HEADER_LEN as u64 + second_relative + second_size;

    // 이어서 스캔: 시드된 연속성으로 seq 3부터 끝까지 읽힌다.
    let rest =
        term_pty::journal::JournalReader::scan_window_resuming(&path, resume_at, 3, u64::MAX, 10)
            .unwrap();
    let seqs: Vec<u64> = rest.iter().map(|(r, _)| r.seq).collect();
    assert_eq!(seqs, vec![3, 4, 5]);
    let (last_record, last_relative) = rest.last().unwrap();
    let end = resume_at + last_relative + (4 + 13 + last_record.payload.len()) as u64;
    assert_eq!(
        end, file_len,
        "전진 오프셋이 파일 끝과 정확히 일치해야 한다"
    );
}

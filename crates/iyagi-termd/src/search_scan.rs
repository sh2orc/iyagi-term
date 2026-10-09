//! 저널 검색 스캔 헬퍼(W2 `session.search`의 본체) — 시험이 직접 굴릴 수
//! 있게 dispatch에서 분리했다. 세그먼트 파일 하나를 핸들 하나로 머리부터
//! 레코드 단위로(CRC·seq 연속성 검증) 읽고, 디스크 읽기 자체를 남은 바이트
//! 예산에서 자른다(`JournalReader::stream_bounded`).

use std::path::Path;

use term_contracts::session::search_limits as lim;
use term_contracts::session::{SessionSearchMatch, SessionSearchParams};
use term_contracts::U64String;
use term_pty::journal::{JournalReader, JournalRecordKind};

/// 파일을 하나 열 때마다 예산에 더하는 고정 비용(한 페이지). 읽은 바이트가
/// 0인 파일(빈 파일 등)도 예산을 움직이게 해, 호출자의 전체 예산이 방문하는
/// 파일 수까지 묶게 한다.
const FILE_OPEN_COST: u64 = 4096;

pub struct ScanOutcome {
    pub matches: Vec<(u64, String)>,
    pub truncated: bool,
    /// 예산에 센 바이트: 디스크에서 실제로 읽은 바이트(헤더·버퍼 채움 포함)에
    /// 연 파일마다 `FILE_OPEN_COST`를 더한 값.
    pub scanned_bytes: u64,
}

/// 저널 하나(세그먼트 런 전체)를 순차 스캔해 일치(seq, 발췌 줄)를 모은다.
/// 바이트 예산으로 유계다. 파라미터의 query는 이미 검증·트림된 값이어야 한다.
pub fn scan_journal(path: &Path, params: &SessionSearchParams, needle: &str) -> ScanOutcome {
    let mut outcome = ScanOutcome {
        matches: Vec::new(),
        truncated: false,
        scanned_bytes: 0,
    };
    // 롤링 저널: 보존된 첫 세그먼트부터 활성 파일까지 차례로 본다. 잘린
    // 앞부분은 디스크에 없으므로 검색 대상도 아니다.
    //
    // 세그먼트 목록은 read_dir 열거로만 얻는다. JournalSet::open은 매칭을
    // 시작하기도 전에 보존된 모든 세그먼트(세션 상한 128 MiB까지)를 CRC까지
    // 완전 검증하는데, 그 패스는 아래 바이트 예산에 잡히지 않는 비용이라
    // 예산이 무의미해졌다. 대신 세그먼트마다 핸들 하나로 머리부터 읽으며
    // 레코드 단위로 CRC를 검증하고, 읽기 자체를 남은 예산에서 자른다.
    let Ok(closed) = term_pty::segments::list_closed_segments(path) else {
        return outcome;
    };
    for (_, segment) in &closed {
        if !scan_file(segment, params, needle, &mut outcome) {
            return outcome;
        }
    }
    scan_file(path, params, needle, &mut outcome);
    outcome
}

/// 파일(세그먼트) 하나를 남은 세션 예산 안에서 스캔한다. 다음 파일로 계속
/// 가도 되면 true, 상한(일치 수·바이트 예산)에 닿아 멈춰야 하면 false.
///
/// 예산은 파싱한 레코드 크기가 아니라 디스크에서 실제로 읽은 바이트로 센다.
/// 256레코드 배치마다 파일을 다시 열고 64 KiB 버퍼를 채우던 예전 방식은
/// 작은 레코드 저널에서 센 값의 약 10배를 실제로 읽었다. 이제 읽기(버퍼
/// 채움 포함)가 남은 예산에서 잘리므로 세션 상한을 넘겨 읽지 않는다.
fn scan_file(
    path: &Path,
    params: &SessionSearchParams,
    needle: &str,
    outcome: &mut ScanOutcome,
) -> bool {
    if outcome.scanned_bytes >= lim::PER_SESSION_SCAN_BYTES {
        outcome.truncated = true;
        return false;
    }
    let read_cap = lim::PER_SESSION_SCAN_BYTES - outcome.scanned_bytes;
    let Ok(mut stream) = JournalReader::stream_bounded(path, read_cap) else {
        return true; // 열 수 없는 파일(회전 중 잠깐 없음 등) — 넘어간다
    };
    let mut match_cap_hit = false;
    // 손상(헤더·CRC·seq 틈)을 만나면 이 파일은 거기까지 — 다음 세그먼트는 본다.
    for record in stream.by_ref().map_while(Result::ok) {
        if record.kind != JournalRecordKind::Output {
            continue;
        }
        let text = String::from_utf8_lossy(&record.payload);
        let haystack = if params.case_sensitive {
            text.clone().into_owned()
        } else {
            text.to_lowercase()
        };
        if let Some(at) = haystack.find(needle) {
            if outcome.matches.len() >= lim::MAX_MATCHES {
                match_cap_hit = true;
                break;
            }
            let display: &str = if haystack.len() == text.len() {
                &text
            } else {
                &haystack
            };
            outcome
                .matches
                .push((record.seq, context_line(display, at, needle.len())));
        }
    }
    outcome.scanned_bytes += stream.bytes_read() + FILE_OPEN_COST;
    if match_cap_hit || stream.cap_reached() {
        outcome.truncated = true;
        return false;
    }
    true
}

/// 일치 위치를 포함하는 줄 발췌(상한 240자, char 경계 방어 포함).
pub(crate) fn context_line(text: &str, at: usize, needle_len: usize) -> String {
    let at = {
        let mut i = at.min(text.len());
        while i > 0 && !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let needle_len = needle_len.min(text.len() - at);
    let start = text[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let end = {
        let stop = (at + needle_len).min(text.len());
        text[stop..]
            .find('\n')
            .map(|i| stop + i)
            .unwrap_or(text.len())
    };
    let line = &text[start..end];
    if line.len() <= lim::LINE_CONTEXT {
        line.to_string()
    } else {
        let center = at.saturating_sub(start);
        let from = center.saturating_sub(120);
        let mut end2 = (from + lim::LINE_CONTEXT).min(line.len());
        while !line.is_char_boundary(end2) {
            end2 -= 1;
        }
        let mut from2 = from;
        while !line.is_char_boundary(from2) {
            from2 += 1;
        }
        format!("…{}", &line[from2..end2])
    }
}

/// 검색 결과 조립(세션 id 부착 + 정렬). 시험/호출 양쪽에서 쓴다.
pub(crate) fn matches_for_session(
    session_id: &term_contracts::ids::SessionId,
    raw: Vec<(u64, String)>,
) -> Vec<SessionSearchMatch> {
    raw.into_iter()
        .map(|(seq, line)| SessionSearchMatch {
            session_id: session_id.clone(),
            seq: U64String::new(seq).unwrap_or_else(|_| U64String::new(0).expect("0 fits")),
            line,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use term_pty::journal::{GlobalJournalBudget, JournalOptions, JournalWriter};

    fn params() -> SessionSearchParams {
        SessionSearchParams {
            query: "needle".into(),
            case_sensitive: false,
            session_id: None,
        }
    }

    /// 회전 런(닫힌 세그먼트 + 활성 파일)을 만든다. 세그먼트 목표 4 KiB에
    /// 120바이트 출력 200건이면 여러 번 회전한다(세션 상한 64 KiB 아래라
    /// 머리 정리는 일어나지 않는다).
    fn rolling_run(base: &Path) {
        let mut writer = JournalWriter::open_with(
            base,
            uuid::Uuid::new_v4(),
            JournalOptions {
                session_limit: 64 * 1024,
                segment_cap: Some(256),
            },
            GlobalJournalBudget::shared_default(),
        )
        .unwrap();
        writer.append_resize(80, 24).unwrap(); // 필수 첫 크기 레코드
        writer
            .append_output(b"an old needle sits in the first segment")
            .unwrap();
        for _ in 0..200 {
            writer.append_output(&[b'f'; 120][..]).unwrap();
        }
        writer
            .append_output(b"the needle hides in the active segment")
            .unwrap();
        drop(writer); // Drop이 BufWriter를 flush한다
    }

    /// 닫힌 세그먼트와 활성 파일 모두에서 일치를 찾는다 — 세그먼트 열거가
    /// JournalSet::open의 전체 검증 패스 없이도 런 전체를 커버한다.
    #[test]
    fn scan_journal_walks_every_segment_of_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("s.mtj");
        rolling_run(&base);
        let closed = term_pty::segments::list_closed_segments(&base).unwrap();
        assert!(!closed.is_empty(), "fixture must produce closed segments");

        let outcome = scan_journal(&base, &params(), "needle");
        assert!(!outcome.truncated);
        assert_eq!(outcome.matches.len(), 2, "{:?}", outcome.matches);
        assert!(outcome.matches[0].0 < outcome.matches[1].0);
        // 예산은 디스크에서 실제로 읽은 바이트다: 파일마다 헤더까지 전부와
        // 열기 비용. 파싱한 레코드 크기만 세던 예전 값은 이보다 작았다.
        let files = term_pty::segments::journal_files(&base);
        let on_disk = term_pty::segments::journal_files_bytes(&base);
        assert_eq!(
            outcome.scanned_bytes,
            on_disk + FILE_OPEN_COST * files.len() as u64
        );
    }

    /// 세션 상한(8 MiB)을 넘는 저널은 상한에서 읽기를 멈추고 truncated를
    /// 알린다. 상한 너머의 일치는 읽지 않으며, 센 바이트는 상한과 열기 비용
    /// 한 번을 넘지 않는다 — 버퍼 채움까지 예산 안이다.
    #[test]
    fn scan_journal_stops_reading_at_the_per_session_budget() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("big.mtj");
        let mut writer = JournalWriter::open(&base, uuid::Uuid::new_v4()).unwrap();
        writer.append_resize(80, 24).unwrap();
        let filler = [b'f'; 16 * 1024];
        let records = lim::PER_SESSION_SCAN_BYTES / filler.len() as u64 + 8;
        for _ in 0..records {
            writer.append_output(&filler).unwrap();
        }
        writer.append_output(b"late needle").unwrap();
        drop(writer);

        let outcome = scan_journal(&base, &params(), "needle");
        assert!(outcome.truncated);
        assert!(outcome.matches.is_empty(), "{:?}", outcome.matches);
        let ceiling = lim::PER_SESSION_SCAN_BYTES + FILE_OPEN_COST;
        assert!(outcome.scanned_bytes <= ceiling);
    }

    /// 머리가 잘린(레코드 영역이 빈) 세그먼트는 건너뛰고 나머지 런은 계속
    /// 본다 — JournalSet::usable이 손상 머리를 제외하던 것과 같은 결과다.
    #[test]
    fn scan_journal_skips_an_empty_head_segment_and_continues() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("s.mtj");
        rolling_run(&base);
        let mut closed = term_pty::segments::list_closed_segments(&base).unwrap();
        let first = closed.remove(0).1;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&first)
            .unwrap();
        file.set_len(term_pty::journal::HEADER_LEN as u64).unwrap();
        drop(file);

        let outcome = scan_journal(&base, &params(), "needle");
        assert!(!outcome.truncated);
        // 첫 세그먼트의 일치는 사라지고 활성 파일의 일치만 남는다.
        assert_eq!(outcome.matches.len(), 1, "{:?}", outcome.matches);
        assert!(outcome.matches[0].1.contains("active segment"));
    }

    /// 저널이 아예 없어도 조용히 빈 결과다(패닉도 truncated도 아니다).
    #[test]
    fn scan_journal_tolerates_a_missing_run() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = scan_journal(&dir.path().join("none.mtj"), &params(), "needle");
        assert!(outcome.matches.is_empty());
        assert!(!outcome.truncated);
        assert_eq!(outcome.scanned_bytes, 0);
    }
}

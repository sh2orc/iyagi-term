//! 감지된 에이전트의 **현재 모델·effort** 관찰 — 순수 파서 모음.
//!
//! 파일 입출력(꼬리 읽기 제외)과 pane 귀속은 호출자(`agent_watch`) 몫이다.
//! 출력 화면을 긁지 않고, 에이전트가 스스로 남기는 기록만 읽는다.
//!
//! 즉시성 실측(Claude Code 2.1.270 · Codex 0.154, 2026-09-13 — 세션 전용
//! `--model sonnet --effort high`로 띄운 뒤 `/effort xhigh`, `/model opus[1m]`):
//!
//! | 출처 | 반영 시점 | 범위 |
//! |---|---|---|
//! | Claude 상태줄 stdin JSON(`iyagi-termd claude-usage`가 세션별 캐시로 기록) | 시작 직후 1회, `/effort` 뒤 77ms, `/model` 뒤 67ms | 세션(`session_id`) |
//! | Claude transcript 로컬 명령 항목(`Set model to …`, `Set effort level to …`) | 약 30ms | 세션 |
//! | Claude transcript assistant 항목(`message.model`, 최상위 `effort`) | 응답마다 | 세션 |
//! | Codex `config.toml`(`/model` 선택이 `model`·`model_reasoning_effort`를 즉시 교체) | 즉시 | **전역** — 호출자가 pane에 귀속 |
//! | Codex rollout `thread_settings_applied`·`turn_context` | 다음 턴 시작 | 세션 |
//! | 실행 인수(`--model`·`--effort` / `-m`·`-c model_reasoning_effort=`·`-p`) | 시작 시 | 프로세스 |
//!
//! Claude의 `settings.json`은 저장된 기본값일 뿐이고 effort가 모델별
//! (`modelSettings.<모델>.effortLevel`)로도 저장되므로 세션의 현재 값으로 쓰지 않는다.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// 표시 문자열 상한(문자). 모델 표시 이름·effort는 짧다 — 넘으면 버린다.
pub const LABEL_MAX_CHARS: usize = 64;

/// 로그 꼬리 읽기 상한. 모델 변경 기록은 끝부분에 있다 — 거대한 transcript/
/// rollout 전체를 읽지 않는다.
pub const TAIL_READ_MAX: u64 = 256 * 1024;

/// 관찰한 모델·effort 한 벌.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelObservation {
    /// 사람이 읽는 모델 이름 — 에이전트의 표시 이름("Opus 5 (1M context)")이
    /// 있으면 그것, 없으면 id("claude-opus-5", "gpt-5.6-sol").
    pub model: Option<String>,
    /// effort/추론 수준 원문("low" | "medium" | "high" | "xhigh" | "max" | "auto" …).
    /// 모델이 effort를 지원하지 않으면 없음.
    pub effort: Option<String>,
}

impl ModelObservation {
    pub fn is_empty(&self) -> bool {
        self.model.is_none() && self.effort.is_none()
    }
}

// ---------------------------------------------------------------------------
// 공용 정리 함수

/// ANSI CSI 시퀀스(`ESC [ … 최종 바이트`)와 OSC(`ESC ] … BEL`)를 걷어낸다.
pub(crate) fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() || n == '~' {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                for n in chars.by_ref() {
                    if n == '\u{7}' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// 표시 라벨: 앞뒤 공백·백틱 제거, 제어문자·과도한 길이는 거절.
fn clean_label(raw: &str) -> Option<String> {
    let stripped = strip_ansi(raw);
    let trimmed = stripped.trim().trim_matches('`').trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > LABEL_MAX_CHARS
        || trimmed.chars().any(char::is_control)
    {
        return None;
    }
    Some(trimmed.to_string())
}

/// effort 단어: 영숫자·`-`·`_`만, 소문자로. 문장이 붙어 있으면 첫 단어만 쓴다.
fn effort_word(raw: &str) -> Option<String> {
    let word: String = raw
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if word.is_empty() || word.len() > 16 {
        return None;
    }
    Some(word.to_ascii_lowercase())
}

fn str_field<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a str> {
    value?.get(key)?.as_str()
}

/// `YYYY-MM-DDTHH:MM:SS[.frac]Z`(UTC) → Unix ms. 다른 형식은 `None`.
pub fn iso8601_utc_ms(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |range: std::ops::Range<usize>| -> Option<i64> {
        let s = text.get(range)?;
        if !s.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    };
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut idx = 19;
    let mut millis = 0i64;
    if b.get(idx) == Some(&b'.') {
        idx += 1;
        let mut digits = 0;
        while idx < b.len() && b[idx].is_ascii_digit() {
            if digits < 3 {
                millis = millis * 10 + i64::from(b[idx] - b'0');
                digits += 1;
            }
            idx += 1;
        }
        if digits == 0 {
            return None;
        }
        while digits < 3 {
            millis *= 10;
            digits += 1;
        }
    }
    if b.get(idx) != Some(&b'Z') || idx + 1 != b.len() {
        return None;
    }
    // days-from-civil(Howard Hinnant).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hour * 3_600 + minute * 60 + second;
    u64::try_from(secs * 1_000 + millis).ok()
}

/// 파일 끝 `max` 바이트를 읽는다. 중간에서 시작했으면 잘린 첫 줄은 버린다.
pub fn read_tail(path: &Path, max: u64) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(max);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity(usize::try_from(len - start).unwrap_or(0));
    file.take(max).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    if start == 0 {
        return Some(text.into_owned());
    }
    Some(match text.find('\n') {
        Some(i) => text[i + 1..].to_string(),
        None => String::new(),
    })
}

// ---------------------------------------------------------------------------
// Claude Code — 상태줄 stdin JSON

/// 상태줄 JSON에서 세션 id와 모델·effort 스냅숏. 상태줄은 매번 **전체 상태**를
/// 싣는다 — `effort`가 없으면 그 모델은 effort를 쓰지 않는 것이다(없음으로 교체).
pub fn claude_statusline_model(payload: &Value) -> Option<(String, ModelObservation)> {
    let session_id = payload.get("session_id")?.as_str()?.to_string();
    let model = payload.get("model");
    let label = str_field(model, "display_name")
        .and_then(clean_label)
        .or_else(|| str_field(model, "id").and_then(clean_label));
    let effort = match payload.get("effort") {
        Some(Value::Object(object)) => object
            .get("level")
            .and_then(Value::as_str)
            .and_then(effort_word),
        Some(Value::String(text)) => effort_word(text),
        _ => None,
    };
    let observation = ModelObservation {
        model: label,
        effort,
    };
    (!observation.is_empty()).then_some((session_id, observation))
}

/// 세션별 상태줄 캐시 파일 이름(`<data_dir>/data/claude-model/<session>.json`).
pub fn statusline_cache_path(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir
        .join("data")
        .join("claude-model")
        .join(format!("{session_id}.json"))
}

/// 상태줄 캐시 본문 — 관찰 시각(Unix ms)과 스냅숏만 담는다(프롬프트·경로 없음).
pub fn statusline_cache_value(observation: &ModelObservation, observed_at_ms: u64) -> Value {
    serde_json::json!({
        "observedAtMs": observed_at_ms,
        "model": observation.model,
        "effort": observation.effort,
    })
}

/// 상태줄 캐시 파싱 — (관찰 시각 ms, 스냅숏).
pub fn parse_statusline_cache(text: &str) -> Option<(u64, ModelObservation)> {
    let value: Value = serde_json::from_str(text).ok()?;
    let at = value.get("observedAtMs")?.as_u64()?;
    let observation = ModelObservation {
        model: value
            .get("model")
            .and_then(Value::as_str)
            .and_then(clean_label),
        effort: value
            .get("effort")
            .and_then(Value::as_str)
            .and_then(effort_word),
    };
    Some((at, observation))
}

// ---------------------------------------------------------------------------
// Claude Code — transcript

/// transcript에서 읽은 모델 관련 사건 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeModelEventKind {
    /// `/model`(또는 모델 피커·Fast mode) 로컬 명령 결과 — 표시 이름.
    Model(String),
    /// `/effort` 로컬 명령 결과.
    Effort(String),
    /// assistant 응답 — 모델 id와 effort. `effort`가 `Some(None)`이면 명시적
    /// 없음(null), `None`이면 필드 자체가 없는 구버전(이전 값 유지).
    Assistant {
        model: Option<String>,
        effort: Option<Option<String>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeModelEvent {
    pub at_ms: u64,
    pub kind: ClaudeModelEventKind,
}

/// 로컬 명령 stdout → 모델/effort 사건. 문구는 Claude Code 2.1.270 기준:
/// "Set model to `X` and saved…", "Kept model as `X`", "Fast mode ON · model set to X",
/// "Set effort level to X (saved…|this session only)…", "Effort level set to auto".
fn parse_local_command_stdout(text: &str) -> Vec<ClaudeModelEventKind> {
    let text = strip_ansi(text);
    let mut out = Vec::new();
    for prefix in ["Set model to ", "Kept model as ", " \u{b7} model set to "] {
        if let Some(index) = text.find(prefix) {
            if let Some(label) = backticked_or_head(&text[index + prefix.len()..]) {
                out.push(ClaudeModelEventKind::Model(label));
                break;
            }
        }
    }
    if let Some(index) = text.find("Set effort level to ") {
        if let Some(level) = effort_word(&text[index + "Set effort level to ".len()..]) {
            out.push(ClaudeModelEventKind::Effort(level));
        }
    } else if text.contains("Effort level set to auto") || text.contains("Effort set to auto") {
        out.push(ClaudeModelEventKind::Effort("auto".to_string()));
    }
    out
}

/// 백틱으로 감싼 이름이면 그 안, 아니면 " and "·" ("·줄 끝 앞까지.
fn backticked_or_head(rest: &str) -> Option<String> {
    let rest = rest.trim_start();
    if let Some(inner) = rest.strip_prefix('`') {
        let end = inner.find('`')?;
        return clean_label(&inner[..end]);
    }
    let end = [" and ", " (", "\n", "<"]
        .iter()
        .filter_map(|marker| rest.find(marker))
        .min()
        .unwrap_or(rest.len());
    clean_label(&rest[..end])
}

/// 메시지 content(문자열 또는 블록 배열)의 텍스트.
fn content_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let joined: Vec<&str> = blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect();
            (!joined.is_empty()).then(|| joined.join("\n"))
        }
        _ => None,
    }
}

/// transcript 꼬리 → 시간순 모델 사건. 사이드체인(서브에이전트)·합성 응답은 뺀다.
pub fn claude_transcript_events(tail: &str) -> Vec<ClaudeModelEvent> {
    let mut events = Vec::new();
    for line in tail.lines() {
        let local = line.contains("local-command-stdout");
        if !local && !line.contains("\"assistant\"") {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(at_ms) = entry
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(iso8601_utc_ms)
        else {
            continue;
        };
        let message = entry.get("message");
        match entry.get("type").and_then(Value::as_str) {
            Some("user") if local => {
                let Some(text) = content_text(message.and_then(|m| m.get("content"))) else {
                    continue;
                };
                let Some(start) = text.find("<local-command-stdout>") else {
                    continue;
                };
                let body = &text[start + "<local-command-stdout>".len()..];
                let body = body.split("</local-command-stdout>").next().unwrap_or(body);
                for kind in parse_local_command_stdout(body) {
                    events.push(ClaudeModelEvent { at_ms, kind });
                }
            }
            Some("assistant") => {
                let model = str_field(message, "model")
                    .filter(|id| !id.starts_with('<'))
                    .and_then(clean_label);
                let effort = match entry.get("effort") {
                    None => None,
                    Some(Value::String(text)) => Some(effort_word(text)),
                    Some(_) => Some(None),
                };
                if model.is_some() || effort.is_some() {
                    events.push(ClaudeModelEvent {
                        at_ms,
                        kind: ClaudeModelEventKind::Assistant { model, effort },
                    });
                }
            }
            _ => {}
        }
    }
    events
}

/// 표시 이름이 모델 id와 같은 모델을 가리키는가 — "Opus 5 (1M context)"는
/// "claude-opus-5"와 같다(더 풍부한 표시 이름을 유지하려는 판정).
fn label_matches_model_id(label: &str, id: &str) -> bool {
    if label.eq_ignore_ascii_case(id) {
        return true;
    }
    let base = id.strip_prefix("claude-").unwrap_or(id);
    let base = base.split('[').next().unwrap_or(base);
    let mut parts: Vec<&str> = base.split('-').filter(|p| !p.is_empty()).collect();
    if parts
        .last()
        .is_some_and(|p| p.len() == 8 && p.bytes().all(|c| c.is_ascii_digit()))
    {
        parts.pop();
    }
    let Some((family, version)) = parts.split_first() else {
        return false;
    };
    let label_lc = label.to_ascii_lowercase();
    if !label_lc.contains(&family.to_ascii_lowercase()) {
        return false;
    }
    let version = version.join(".");
    if version.is_empty() {
        return true;
    }
    // 버전은 단어 경계로만 인정한다("5"가 "5.1"에 맞지 않게).
    let mut from = 0;
    while let Some(found) = label_lc[from..].find(&version) {
        let end = from + found + version.len();
        let rest = &label_lc.as_bytes()[end..];
        let continues = match rest {
            [c, ..] if c.is_ascii_digit() => true,
            [b'.', c, ..] if c.is_ascii_digit() => true,
            _ => false,
        };
        if !continues {
            return true;
        }
        from = end;
    }
    false
}

/// 상태줄 스냅숏(있으면)을 바탕으로 그 뒤의 transcript 사건을 적용한다.
/// 상태줄보다 오래된 사건은 무시한다 — 둘 다 같은 기계 시계다.
pub fn fold_claude(
    base: Option<(u64, ModelObservation)>,
    events: &[ClaudeModelEvent],
) -> Option<ModelObservation> {
    let (since, mut observation) = base.unwrap_or_default();
    for event in events.iter().filter(|e| e.at_ms > since) {
        match &event.kind {
            ClaudeModelEventKind::Model(label) => observation.model = Some(label.clone()),
            ClaudeModelEventKind::Effort(level) => observation.effort = Some(level.clone()),
            ClaudeModelEventKind::Assistant { model, effort } => {
                if let Some(id) = model {
                    let keep = observation
                        .model
                        .as_deref()
                        .is_some_and(|label| label_matches_model_id(label, id));
                    if !keep {
                        observation.model = Some(id.clone());
                    }
                }
                if let Some(effort) = effort {
                    observation.effort = effort.clone();
                }
            }
        }
    }
    (!observation.is_empty()).then_some(observation)
}

/// Claude Code의 프로젝트 디렉터리 이름 — 영숫자 외 문자를 `-`로.
/// (긴 경로·비BMP 문자는 규칙이 달라질 수 있어 호출자가 세션 id로 폴백 검색한다.)
pub fn claude_project_slug(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

// ---------------------------------------------------------------------------
// Codex

/// rollout 꼬리 → 가장 최근 세션 설정(`thread_settings_applied` 또는 `turn_context`)과
/// **그 기록의** 시각(Unix ms, 없으면 0). 파일 수정 시각은 토큰 집계 등으로 계속
/// 바뀌므로 비교에 쓰면 즉시 반영한 설정 변경을 옛 기록이 덮어쓴다.
pub fn codex_rollout_model(tail: &str) -> Option<(u64, ModelObservation)> {
    let mut latest = None;
    for line in tail.lines() {
        let settings_event = line.contains("thread_settings_applied");
        if !settings_event && !line.contains("\"turn_context\"") {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let payload = entry.get("payload");
        let source = match entry.get("type").and_then(Value::as_str) {
            Some("turn_context") => payload,
            Some("event_msg") if str_field(payload, "type") == Some("thread_settings_applied") => {
                payload.and_then(|p| p.get("thread_settings"))
            }
            _ => continue,
        };
        let Some(source) = source else {
            continue;
        };
        let collaboration = source
            .get("collaboration_mode")
            .and_then(|c| c.get("settings"));
        let model = source
            .get("model")
            .and_then(Value::as_str)
            .or_else(|| str_field(collaboration, "model"))
            .and_then(clean_label);
        let effort = ["effort", "reasoning_effort"]
            .iter()
            .find_map(|key| source.get(*key).and_then(Value::as_str))
            .or_else(|| str_field(collaboration, "reasoning_effort"))
            .and_then(effort_word);
        let observation = ModelObservation { model, effort };
        if !observation.is_empty() {
            let at = entry
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(iso8601_utc_ms)
                .unwrap_or(0);
            latest = Some((at, observation));
        }
    }
    latest
}

/// TOML 문자열 값(`"…"`·`'…'`)만 해석한다. 다른 형식은 `None`.
fn toml_string(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if let Some(inner) = raw.strip_prefix('\'') {
        return inner.find('\'').map(|end| inner[..end].to_string());
    }
    let inner = raw.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
    None
}

/// 따옴표 밖의 `#` 주석을 떼어낸다.
fn strip_toml_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' && q == '"' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '#' => return &line[..i],
                _ => {}
            },
        }
    }
    line
}

/// 테이블 헤더 `[profiles."a b"]` → 점으로 구분된 키 경로(따옴표 제거).
fn toml_table_path(header: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for c in header.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => current.push(c),
            None => match c {
                '"' | '\'' => quote = Some(c),
                '.' => parts.push(std::mem::take(&mut current).trim().to_string()),
                _ => current.push(c),
            },
        }
    }
    parts.push(current.trim().to_string());
    parts
}

/// `config.toml`의 활성 모델·추론 수준. 프로필(`profile = "x"` 또는 실행 인수
/// `-p x`)의 `[profiles.x]` 값이 최상위 값을 덮는다.
pub fn codex_config_model(text: &str, profile_override: Option<&str>) -> ModelObservation {
    let mut table: Vec<String> = Vec::new();
    let mut top = ModelObservation::default();
    let mut active_profile: Option<String> = None;
    let mut profiles: std::collections::HashMap<String, ModelObservation> =
        std::collections::HashMap::new();
    for raw_line in text.lines() {
        let line = strip_toml_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("[[") {
            table = vec!["<array>".to_string()];
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            table = toml_table_path(header);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"');
        let target = match table.as_slice() {
            [] => {
                if key == "profile" {
                    active_profile = toml_string(value);
                    continue;
                }
                &mut top
            }
            [first, name] if first == "profiles" => profiles.entry(name.clone()).or_default(),
            _ => continue,
        };
        match key {
            "model" => target.model = toml_string(value).as_deref().and_then(clean_label),
            "model_reasoning_effort" => {
                target.effort = toml_string(value).as_deref().and_then(effort_word)
            }
            _ => {}
        }
    }
    let profile = profile_override.map(str::to_string).or(active_profile);
    if let Some(selected) = profile.and_then(|name| profiles.get(&name)) {
        if selected.model.is_some() {
            top.model = selected.model.clone();
        }
        if selected.effort.is_some() {
            top.effort = selected.effort.clone();
        }
    }
    top
}

// ---------------------------------------------------------------------------
// 실행 인수

/// 실행 인수에서 읽은 모델·effort와(Codex) 프로필.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchModel {
    pub observation: ModelObservation,
    pub codex_profile: Option<String>,
}

/// `--flag value` 또는 `--flag=value`.
fn flag_value<'a>(args: &'a [String], index: usize, names: &[&str]) -> Option<(&'a str, usize)> {
    let arg = args[index].as_str();
    for name in names {
        if arg == *name {
            return args.get(index + 1).map(|v| (v.as_str(), 2));
        }
        if let Some(value) = arg
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('='))
        {
            return Some((value, 1));
        }
    }
    None
}

pub fn launch_model(agent: &str, argv: &[String]) -> LaunchModel {
    let mut out = LaunchModel::default();
    let mut i = 0;
    while i < argv.len() {
        if argv[i] == "--" {
            break;
        }
        let step = match agent {
            "claude" => {
                if let Some((value, step)) = flag_value(argv, i, &["--model"]) {
                    out.observation.model = clean_label(value);
                    step
                } else if let Some((value, step)) = flag_value(argv, i, &["--effort"]) {
                    out.observation.effort = effort_word(value);
                    step
                } else {
                    1
                }
            }
            "codex" => {
                if let Some((value, step)) = flag_value(argv, i, &["--model", "-m"]) {
                    out.observation.model = clean_label(value);
                    step
                } else if let Some((value, step)) = flag_value(argv, i, &["--profile", "-p"]) {
                    out.codex_profile = clean_label(value);
                    step
                } else if let Some((value, step)) = flag_value(argv, i, &["--config", "-c"]) {
                    if let Some((key, raw)) = value.split_once('=') {
                        let parsed = toml_string(raw).unwrap_or_else(|| raw.trim().to_string());
                        match key.trim() {
                            "model" => out.observation.model = clean_label(&parsed),
                            "model_reasoning_effort" => {
                                out.observation.effort = effort_word(&parsed)
                            }
                            _ => {}
                        }
                    }
                    step
                } else {
                    1
                }
            }
            _ => break,
        };
        i += step;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn iso_timestamps_convert_to_unix_ms() {
        assert_eq!(iso8601_utc_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            iso8601_utc_ms("2000-01-01T00:00:00.000Z"),
            Some(946_684_800_000)
        );
        assert_eq!(
            iso8601_utc_ms("2000-01-01T00:00:00.5Z"),
            Some(946_684_800_500)
        );
        let a = iso8601_utc_ms("2026-09-12T23:48:00.126Z").unwrap();
        let b = iso8601_utc_ms("2026-09-13T00:02:01.001Z").unwrap();
        assert_eq!(b - a, 840_875);
        assert_eq!(iso8601_utc_ms("2026-09-12 23:48:00Z"), None);
        assert_eq!(iso8601_utc_ms("2026-13-12T23:48:00Z"), None);
        assert_eq!(iso8601_utc_ms("2026-09-12T23:48:00+09:00"), None);
    }

    #[test]
    fn statusline_payload_yields_display_name_and_effort_level() {
        // 실측 키 집합(2.1.270): context_window, cost, cwd, effort, exceeds_200k_tokens, fast_mode,
        // model, output_style, rate_limits, session_id, thinking, transcript_path, version, workspace.
        let payload = json!({
            "session_id": "6d0d2c32-3575-4e13-a11d-c9dad4cb85e1",
            "model": { "id": "claude-opus-5[1m]", "display_name": "Opus 5 (1M context)" },
            "effort": { "level": "xhigh" },
            "fast_mode": false,
            "transcript_path": "/secret/path.jsonl",
        });
        let (session, observation) = claude_statusline_model(&payload).unwrap();
        assert_eq!(session, "6d0d2c32-3575-4e13-a11d-c9dad4cb85e1");
        assert_eq!(observation.model.as_deref(), Some("Opus 5 (1M context)"));
        assert_eq!(observation.effort.as_deref(), Some("xhigh"));

        // effort를 지원하지 않는 모델 — 필드가 없으면 없음.
        let no_effort = json!({ "session_id": "s", "model": { "id": "claude-haiku-4-5" } });
        let (_, observation) = claude_statusline_model(&no_effort).unwrap();
        assert_eq!(observation.model.as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(observation.effort, None);

        assert!(claude_statusline_model(&json!({ "model": { "id": "x" } })).is_none());
        assert!(claude_statusline_model(&json!({ "session_id": "s" })).is_none());
    }

    #[test]
    fn statusline_cache_round_trips_without_leaking_payload() {
        let observation = ModelObservation {
            model: Some("Sonnet 5".into()),
            effort: Some("high".into()),
        };
        let value = statusline_cache_value(&observation, 1_789_257_769_549);
        let text = value.to_string();
        assert!(!text.contains("transcript"));
        assert_eq!(
            parse_statusline_cache(&text),
            Some((1_789_257_769_549, observation))
        );
        let none = statusline_cache_value(
            &ModelObservation {
                model: Some("Haiku 4.5".into()),
                effort: None,
            },
            5,
        );
        assert_eq!(
            parse_statusline_cache(&none.to_string()).unwrap().1.effort,
            None
        );
        assert_eq!(
            statusline_cache_path(Path::new("/d"), "abc"),
            PathBuf::from("/d/data/claude-model/abc.json")
        );
    }

    /// 실측 transcript 줄(2.1.270, 실험 세션)을 그대로 쓴다.
    fn transcript_fixture() -> String {
        [
            r#"{"type":"assistant","timestamp":"2026-09-13T00:02:40.000Z","message":{"model":"claude-sonnet-5","content":[]},"effort":"high","perTurnEffort":null}"#,
            r#"{"type":"user","timestamp":"2026-09-13T00:02:49.504Z","message":{"role":"user","content":"<command-name>/effort</command-name>\n            <command-message>effort</command-message>\n            <command-args>xhigh</command-args>"}}"#,
            r#"{"type":"user","timestamp":"2026-09-13T00:02:49.504Z","message":{"role":"user","content":"<local-command-stdout>Set effort level to xhigh (saved as your default for new sessions): Deeper reasoning than high, just below maximum</local-command-stdout>"}}"#,
            r#"{"type":"user","timestamp":"2026-09-13T00:02:54.875Z","message":{"role":"user","content":"<local-command-stdout>Set model to `Opus 5 (1M context)` and saved as your default for new sessions</local-command-stdout>"}}"#,
            r#"{"type":"assistant","isSidechain":true,"timestamp":"2026-09-13T00:02:55.000Z","message":{"model":"claude-haiku-4-5"},"effort":null}"#,
        ]
        .join("\n")
    }

    #[test]
    fn transcript_local_commands_and_assistant_entries_become_events() {
        let events = claude_transcript_events(&transcript_fixture());
        let kinds: Vec<_> = events.iter().map(|e| e.kind.clone()).collect();
        assert_eq!(
            kinds,
            vec![
                ClaudeModelEventKind::Assistant {
                    model: Some("claude-sonnet-5".into()),
                    effort: Some(Some("high".into()))
                },
                ClaudeModelEventKind::Effort("xhigh".into()),
                ClaudeModelEventKind::Model("Opus 5 (1M context)".into()),
            ],
            "사이드체인(서브에이전트) 응답은 세션 모델이 아니다"
        );
        let folded = fold_claude(None, &events).unwrap();
        assert_eq!(folded.model.as_deref(), Some("Opus 5 (1M context)"));
        assert_eq!(folded.effort.as_deref(), Some("xhigh"));
    }

    #[test]
    fn local_command_variants_are_recognized() {
        let parse = parse_local_command_stdout;
        assert_eq!(
            parse("Kept model as `Sonnet 5`"),
            vec![ClaudeModelEventKind::Model("Sonnet 5".into())]
        );
        assert_eq!(
            parse("Fast mode ON \u{b7} model set to Opus 5"),
            vec![ClaudeModelEventKind::Model("Opus 5".into())]
        );
        assert_eq!(
            parse("\u{1b}[1mSet effort level to high\u{1b}[22m (this session only)"),
            vec![ClaudeModelEventKind::Effort("high".into())]
        );
        assert_eq!(
            parse("Set effort level to ultracode (this session only): xhigh + dynamic workflow orchestration"),
            vec![ClaudeModelEventKind::Effort("ultracode".into())]
        );
        assert_eq!(
            parse("Effort level set to auto"),
            vec![ClaudeModelEventKind::Effort("auto".into())]
        );
        assert_eq!(
            parse("Current model: Opus 5"),
            vec![],
            "변경이 아닌 조회는 사건이 아니다"
        );
    }

    #[test]
    fn assistant_model_id_keeps_the_richer_display_name_of_the_same_model() {
        let base = Some((
            1_000,
            ModelObservation {
                model: Some("Opus 5 (1M context)".into()),
                effort: Some("xhigh".into()),
            },
        ));
        let same = [ClaudeModelEvent {
            at_ms: 2_000,
            kind: ClaudeModelEventKind::Assistant {
                model: Some("claude-opus-5".into()),
                effort: Some(Some("xhigh".into())),
            },
        }];
        assert_eq!(
            fold_claude(base.clone(), &same).unwrap().model.as_deref(),
            Some("Opus 5 (1M context)")
        );

        let switched = [ClaudeModelEvent {
            at_ms: 2_000,
            kind: ClaudeModelEventKind::Assistant {
                model: Some("claude-sonnet-5".into()),
                effort: Some(None),
            },
        }];
        let folded = fold_claude(base.clone(), &switched).unwrap();
        assert_eq!(folded.model.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(folded.effort, None, "명시적 null은 effort 없음");

        let old_format = [ClaudeModelEvent {
            at_ms: 2_000,
            kind: ClaudeModelEventKind::Assistant {
                model: Some("claude-opus-5".into()),
                effort: None,
            },
        }];
        assert_eq!(
            fold_claude(base.clone(), &old_format)
                .unwrap()
                .effort
                .as_deref(),
            Some("xhigh")
        );

        // 상태줄 스냅숏보다 오래된 사건은 무시한다.
        let stale = [ClaudeModelEvent {
            at_ms: 500,
            kind: ClaudeModelEventKind::Effort("low".into()),
        }];
        assert_eq!(
            fold_claude(base, &stale).unwrap().effort.as_deref(),
            Some("xhigh")
        );

        assert!(label_matches_model_id("Fable 5.1", "claude-fable-5-1"));
        assert!(label_matches_model_id(
            "Haiku 4.5",
            "claude-haiku-4-5-20251001"
        ));
        assert!(
            !label_matches_model_id("Opus 5.1", "claude-opus-5"),
            "5는 5.1이 아니다"
        );
        assert!(!label_matches_model_id("Opus 5", "claude-sonnet-5"));
    }

    #[test]
    fn project_slug_matches_claude_code() {
        assert_eq!(
            claude_project_slug("/Users/sh2orc/project/iyagi"),
            "-Users-sh2orc-project-iyagi"
        );
        assert_eq!(
            claude_project_slug("/private/tmp/claude-501/x_y.z"),
            "-private-tmp-claude-501-x-y-z"
        );
    }

    #[test]
    fn codex_rollout_prefers_the_latest_settings_record() {
        // 실측 형식(0.154): turn_context.payload.{model,effort},
        // event_msg/thread_settings_applied.payload.thread_settings.{model,reasoning_effort}.
        let tail = [
            r#"{"timestamp":"2026-09-10T16:08:38.430Z","type":"turn_context","payload":{"model":"gpt-5.6-sol","collaboration_mode":{"mode":"default","settings":{"model":"gpt-5.6-sol","reasoning_effort":"high"}},"effort":"high"}}"#,
            r#"{"timestamp":"2026-09-10T16:09:00.000Z","type":"event_msg","payload":{"type":"token_count","info":{}}}"#,
            r#"{"timestamp":"2026-09-10T16:10:00.000Z","type":"event_msg","payload":{"type":"thread_settings_applied","thread_settings":{"model":"gpt-6-astra","reasoning_effort":"medium"}}}"#,
            r#"{"timestamp":"2026-09-10T16:10:00.001Z","type":"event_msg","payload":{"type":"task_started"}}"#,
        ]
        .join("\n");
        assert_eq!(
            codex_rollout_model(&tail),
            Some((
                iso8601_utc_ms("2026-09-10T16:10:00.000Z").unwrap(),
                ModelObservation {
                    model: Some("gpt-6-astra".into()),
                    effort: Some("medium".into())
                }
            )),
            "기록 시각은 그 설정 기록의 timestamp다(뒤따르는 task_started가 아니다)"
        );
        let collaboration_only = r#"{"type":"turn_context","payload":{"collaboration_mode":{"settings":{"model":"gpt-5.5","reasoning_effort":"low"}}}}"#;
        assert_eq!(
            codex_rollout_model(collaboration_only),
            Some((
                0,
                ModelObservation {
                    model: Some("gpt-5.5".into()),
                    effort: Some("low".into())
                }
            ))
        );
        assert_eq!(
            codex_rollout_model("{\"type\":\"session_meta\"}\nnot json"),
            None
        );
    }

    #[test]
    fn codex_config_reads_top_level_and_active_profile() {
        let text = r#"
model = "gpt-5.6-sol" # 기본
model_reasoning_effort = "high"
profile = "deep"

[tui.model_availability_nux]
"gpt-5.5" = 4

[profiles.deep]
model_reasoning_effort = 'xhigh'

[profiles."fast one"]
model = "gpt-5.5"
model_reasoning_effort = "low"

[mcp_servers.x]
model = "not-a-codex-model"
"#;
        assert_eq!(
            codex_config_model(text, None),
            ModelObservation {
                model: Some("gpt-5.6-sol".into()),
                effort: Some("xhigh".into())
            }
        );
        assert_eq!(
            codex_config_model(text, Some("fast one")),
            ModelObservation {
                model: Some("gpt-5.5".into()),
                effort: Some("low".into())
            }
        );
        assert_eq!(
            codex_config_model("model = \"a#b\"\n", None)
                .model
                .as_deref(),
            Some("a#b"),
            "따옴표 안의 #는 주석이 아니다"
        );
        assert_eq!(codex_config_model("", None), ModelObservation::default());
    }

    #[test]
    fn launch_arguments_pin_model_and_effort() {
        let claude = launch_model(
            "claude",
            &args(&[
                "node",
                "/x/cli.js",
                "--model",
                "sonnet",
                "--effort=high",
                "-p",
                "hi",
            ]),
        );
        assert_eq!(
            claude.observation,
            ModelObservation {
                model: Some("sonnet".into()),
                effort: Some("high".into())
            }
        );

        let codex = launch_model(
            "codex",
            &args(&[
                "codex",
                "-m",
                "gpt-5.5",
                "-c",
                "model_reasoning_effort=\"low\"",
                "--profile=deep",
                "resume",
            ]),
        );
        assert_eq!(
            codex.observation,
            ModelObservation {
                model: Some("gpt-5.5".into()),
                effort: Some("low".into())
            }
        );
        assert_eq!(codex.codex_profile.as_deref(), Some("deep"));

        let plain = launch_model("codex", &args(&["codex", "--", "-m", "ignored"]));
        assert_eq!(plain, LaunchModel::default());
        assert_eq!(
            launch_model("opencode", &args(&["opencode", "--model", "x"])),
            LaunchModel::default()
        );
    }

    #[test]
    fn tail_reader_drops_the_cut_first_line() {
        let dir = std::env::temp_dir().join(format!("agent-model-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        std::fs::write(&path, "first line\nsecond\nthird\n").unwrap();
        assert_eq!(
            read_tail(&path, 1024).as_deref(),
            Some("first line\nsecond\nthird\n")
        );
        assert_eq!(read_tail(&path, 10).as_deref(), Some("third\n"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn labels_reject_control_characters_and_oversized_text() {
        assert_eq!(clean_label(" `Opus 5` "), Some("Opus 5".into()));
        assert_eq!(clean_label("bad\u{7}title"), None);
        assert_eq!(clean_label(&"x".repeat(LABEL_MAX_CHARS + 1)), None);
        assert_eq!(effort_word("XHigh (saved)"), Some("xhigh".into()));
        assert_eq!(effort_word(""), None);
    }
}

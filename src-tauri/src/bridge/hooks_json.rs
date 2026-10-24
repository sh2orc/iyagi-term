//! CLI hooks settings.json 편집 어댑터(T13 추출 이후).
//!
//! 구현은 공용 crate `term-integrations`로 옮겼다(claude/codex hooks가
//! 공유하는 순수 JSON 편집기). 이 모듈은 기존 GUI import 경로를 유지하는
//! re-export다.

pub use term_integrations::hooks_json::*;

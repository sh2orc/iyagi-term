//! # term-integrations
//!
//! GUI(Tauri bridge)와 TUI가 공유하는 로컬 연동 서비스(IMPLEMENTATION_PLAN
//! §3.3, T13). 원본 `src-tauri/src/bridge`의 순수 서비스 모듈을 그대로
//! 옮겼다 — 이 파일들은 `tauri::`를 전혀 쓰지 않는 순수 Rust였다:
//!
//! * `system` — CLI 탐색/검증, 셸 탐지, 콘솔 숨김.
//! * `subscriptions` — Z.ai 키 저장/상태, 키 풀, 구독 사용량 조회.
//! * `claude_usage` — Claude Code 상태줄 사용량 캡처 스크립트 관리.
//! * `shell_profiles` — zsh `ccd`/`ccg` 프로필 설치(동의 기반).
//! * `hooks_json` — CLI hooks settings.json 편집(claude/codex 공용).
//!
//! 이 crate에 tauri/React/Ratatui 타입이 들어가지 않는다. bridge는 얇은
//! re-export로 기존 경로를 유지한다.

pub mod claude_usage;
pub mod daemon_name;
pub mod hooks_json;
pub mod shell_profiles;
pub mod subscriptions;
pub mod system;

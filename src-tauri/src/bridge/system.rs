//! 시스템 연동 어댑터(T13 추출 이후).
//!
//! 구현은 공용 crate `term-integrations`로 옮겼다(CLI 탐색/검증, 셸 탐지,
//! 콘솔 숨김 — tauri 의존이 없던 순수 서비스). 이 모듈은 기존 GUI import
//! 경로(`crate::bridge::system::*`)를 유지하는 re-export다.

pub use term_integrations::system::*;

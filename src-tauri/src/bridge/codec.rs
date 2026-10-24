//! Frame codec adapter(T02 추출 이후).
//!
//! 구현은 공용 crate `term-client`로 옮겼다. 이 모듈은 기존 GUI import
//! 경로(`crate::bridge::codec::{read_frame, write_frame}`)를 유지하는
//! re-export다. 바이트 수준 동작(크기·UTF-8·JSON·깊이 검증)은 그대로다.

pub use term_client::codec::{read_frame, read_frame_sized, write_frame};

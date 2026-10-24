//! Control/data connection adapter(T02 추출 이후).
//!
//! 구현은 공용 crate `term-client`로 옮겼다 — GUI와 TUI가 같은 codec/
//! connection 구현을 참조한다(IMPLEMENTATION_PLAN §3.3, T02 완료 기준).
//! 이 모듈은 기존 GUI import 경로를 그대로 유지하는 얇은 re-export와,
//! Tauri build script가 주입하는 빌드 id(`IYAGI_GIT_SHA`)로 만드는
//! `app_build_version()`만 남긴다.
//!
//! 동작 보존:
//! * hello 제한 2s, 일반 RPC timeout 5s(timeout은 `Busy`로 반환 — 주석이
//!   아닌 실제 코드 동작, §2.3),
//! * 알 수 없는 RPC error 코드 원문 보존(`RpcRaw`),
//! * 연결 종료 시 pending 정리.

#[allow(unused_imports)]
pub use term_client::connection::{
    BridgeError, Connection, HELLO_TIMEOUT, RPC_TIMEOUT, WRITER_QUEUE,
};
#[allow(unused_imports)]
pub use term_client::transport::{BoxedStream, OsTransport, Stream, Transport};
#[allow(unused_imports)]
pub use term_client::version::{daemon_is_outdated, restart_would_update};

/// This app build's version id, the same shape the daemon stamps into
/// `HelloResult.daemon_version`: `format!("{CARGO_PKG_VERSION} ({IYAGI_GIT_SHA})")`.
/// `IYAGI_GIT_SHA` includes the commit and shared daemon-source fingerprint,
/// so uncommitted dev rebuilds also change it. Compared to the connected daemon's
/// `daemon_version` to detect a stale, still-running daemon.
pub fn app_build_version() -> String {
    term_client::version::build_version(env!("CARGO_PKG_VERSION"), env!("IYAGI_GIT_SHA"))
}

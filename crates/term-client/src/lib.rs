//! # term-client
//!
//! GUI(Tauri bridge)와 TUI가 공유하는 iyagi-termd 클라이언트 계층.
//!
//! * `codec` — `term-contracts` 프레임codec의 async 래퍼(기존 bridge/codec.rs).
//! * `transport` — UDS/파이프 연결(주입 가능).
//! * `connection` — hello handshake, pending RPC, reader/writer 루프
//!   (기존 bridge/connection.rs). 이벤트 전달 정책은 GUI의 기존 동작
//!   (unbounded)과 TUI용 bounded 정책을 선택할 수 있게 나뉜다(T03).
//! * `version` — build id 비교(주입형: crate 버전을 데몬 build id로 오인하지
//!   않게 caller가 계산값을 넘긴다).
//! * `client`/`events` — control/data 쌍의 typed RPC·이벤트 해석(T03).
//! * `discovery`/`launcher` — 데이터 경로·runtime 파일·데몬 탐색/시작(T04).
//!
//! 이 crate에 tauri, React/JS Channel, Ratatui 타입이 들어가지 않는다
//! (IMPLEMENTATION_PLAN §3.3). `iyagi_termd_lib` 전체를 링크하지 않는다 —
//! 데몬의 네트워크·모델·DB 의존성이 클라이언트로 따라 들어오지 않게.

pub mod client;
pub mod codec;
pub mod connection;
pub mod discovery;
pub mod events;
pub mod launcher;
pub mod mission;
pub mod transport;
pub mod version;

pub use client::{Client, ClientBuilder, ClientEvent, ConnectError, LaunchOutcome};
pub use connection::{BridgeError, Connection, EventDelivery, EventPolicy};
pub use transport::{BoxedStream, OsTransport, Stream, Transport};
pub use version::{build_version, daemon_is_outdated, restart_would_update};

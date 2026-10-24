//! 연결 transport: 주입 가능한 연결 팩토리(원본 bridge/connection.rs에서
//! 분리, T02). UDS(Unix)/named pipe(Windows)/테스트 duplex 모두 같은
//! `Stream` 트레이트로 취급한다.

use std::future::Future;
use std::pin::Pin;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::connection::BridgeError;

/// Any full-duplex async byte stream (UDS, named pipe, test duplex).
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}
pub type BoxedStream = Box<dyn Stream>;

/// Connects a daemon endpoint string (socket path / pipe name).
pub trait Transport: Send + Sync + 'static {
    fn connect<'a>(
        &'a self,
        endpoint: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<BoxedStream, BridgeError>> + Send + 'a>>;
}

/// OS transport: named pipe on Windows, UDS on Unix.
pub struct OsTransport;

#[cfg(windows)]
impl Transport for OsTransport {
    fn connect<'a>(
        &'a self,
        endpoint: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<BoxedStream, BridgeError>> + Send + 'a>> {
        Box::pin(async move {
            use tokio::net::windows::named_pipe::ClientOptions;
            // Retry while another client holds the pipe instance (ERROR_PIPE_BUSY).
            const ERROR_PIPE_BUSY: i32 = 231;
            let name = normalize_pipe_name(endpoint);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            loop {
                match ClientOptions::new().open(&name) {
                    Ok(client) => return Ok(Box::new(client) as BoxedStream),
                    Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                        if tokio::time::Instant::now() >= deadline {
                            return Err(BridgeError::daemon_unavailable(
                                "daemon named pipe stayed busy",
                            ));
                        }
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                    Err(err) => return Err(BridgeError::Io(err)),
                }
            }
        })
    }
}

#[cfg(windows)]
fn normalize_pipe_name(endpoint: &str) -> String {
    let trimmed = endpoint.trim();
    if trimmed.starts_with(r"\\.\pipe\") || trimmed.starts_with(r"\\?\pipe\") {
        trimmed.to_string()
    } else {
        format!(r"\\.\pipe\{trimmed}")
    }
}

#[cfg(unix)]
impl Transport for OsTransport {
    fn connect<'a>(
        &'a self,
        endpoint: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<BoxedStream, BridgeError>> + Send + 'a>> {
        Box::pin(async move {
            let stream = tokio::net::UnixStream::connect(endpoint).await?;
            Ok(Box::new(stream) as BoxedStream)
        })
    }
}

//! Bounded, acknowledged stdin writes for supervised interactive children.
//! A timed-out/partial write closes the stream and is never retried.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

struct Frame {
    bytes: Vec<u8>,
    reply: std::sync::mpsc::SyncSender<Result<(), ()>>,
}
struct State {
    serial: Mutex<()>,
    sender: tokio::sync::mpsc::Sender<Frame>,
    shutdown: tokio::sync::watch::Sender<bool>,
    closed: Arc<AtomicBool>,
}

/// Clones share one ordered byte stream. `write_blocking` belongs on a
/// protocol/actor worker, never on the pipe pump's async executor.
#[derive(Clone)]
pub struct ExecInput {
    state: Arc<State>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InputError {
    #[error("stdin is closed")]
    Closed,
    #[error("stdin frame exceeds the byte limit")]
    Overcap,
    #[error("stdin write outcome is unknown; channel closed without retry")]
    Unconfirmed,
}

impl ExecInput {
    pub(super) fn start(
        mut pipe: tokio::process::ChildStdin,
        runtime: &tokio::runtime::Handle,
    ) -> Self {
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<Frame>(1);
        let (shutdown, mut closing) = tokio::sync::watch::channel(false);
        let closed = Arc::new(AtomicBool::new(false));
        let worker_closed = closed.clone();
        struct CloseFlag(Arc<AtomicBool>);
        impl Drop for CloseFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let completion = CloseFlag(worker_closed);
        runtime.spawn(async move {
            let _completion = completion;
            loop {
                let frame = tokio::select! {
                    biased;
                    _ = closing.changed() => break,
                    frame = receiver.recv() => match frame {Some(frame)=>frame,None=>break},
                };
                if *closing.borrow() {
                    let _ = frame.reply.send(Err(()));
                    break;
                }
                let result = tokio::select! {
                    biased;
                    _ = closing.changed() => Err(()),
                    result = tokio::time::timeout(WRITE_TIMEOUT, async {
                        pipe.write_all(&frame.bytes).await?;
                        pipe.flush().await
                    }) => match result { Ok(Ok(()))=>Ok(()), _=>Err(()) },
                };
                let failed = result.is_err();
                let _ = frame.reply.send(result);
                if failed {
                    break;
                }
            }
            // Dropping stdin wakes an app-server waiting for input. Queued
            // frames are abandoned, with no second write attempt.
            drop(pipe);
        });
        Self {
            state: Arc::new(State {
                serial: Mutex::new(()),
                sender,
                shutdown,
                closed,
            }),
        }
    }

    pub fn write_blocking(&self, bytes: &[u8]) -> Result<(), InputError> {
        if bytes.len() > super::MAX_LINE_BYTES {
            return Err(InputError::Overcap);
        }
        let _serial = self.state.serial.lock().unwrap_or_else(|p| p.into_inner());
        if self.state.closed.load(Ordering::Acquire) {
            return Err(InputError::Closed);
        }
        let (reply, ack) = std::sync::mpsc::sync_channel(1);
        if self
            .state
            .sender
            .try_send(Frame {
                bytes: bytes.to_vec(),
                reply,
            })
            .is_err()
        {
            self.close();
            return Err(InputError::Closed);
        }
        match ack.recv_timeout(WRITE_TIMEOUT + Duration::from_millis(100)) {
            Ok(Ok(())) => Ok(()),
            _ => {
                self.close();
                Err(InputError::Unconfirmed)
            }
        }
    }

    /// Nonblocking cancellation; an in-flight partial frame is abandoned.
    pub fn close(&self) {
        self.state.closed.store(true, Ordering::Release);
        let _ = self.state.shutdown.send(true);
    }
}

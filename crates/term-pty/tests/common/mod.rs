//! Shared helpers for term-pty integration tests: a robust term-fixture
//! binary locator and an in-process duplex stream implementing the gate
//! transport contract (idle reads surface `WouldBlock` at zero-progress
//! points only, so `read_exact`-based framing stays aligned).

#![allow(dead_code)]

use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Locate the term-fixture binary. `cargo test -p term-pty` does not build
/// sibling binaries, so we probe the usual target layouts; tests SKIP with
/// a clear message when it is absent (build it with
/// `cargo build -p term-fixture`).
pub fn locate_fixture() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "term-fixture.exe"
    } else {
        "term-fixture"
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(target) = std::env::var("CARGO_TARGET_DIR") {
        candidates.push(PathBuf::from(target).join("debug").join(name));
    }
    // term-pty manifest dir: <workspace>/crates/term-pty
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    candidates.push(manifest.join("../../target/debug").join(name));
    candidates.push(manifest.join("../../../target/debug").join(name));
    candidates.push(manifest.join("../target/debug").join(name));
    candidates.push(manifest.join("../../target/release").join(name));
    candidates.into_iter().find(|p| p.is_file())
}

// ---------------------------------------------------------------------------
// In-memory duplex

struct DuplexHalf {
    /// Bytes arriving from the peer, chunk-wise.
    rx: Receiver<Vec<u8>>,
    /// Our outbound channel toward the peer.
    tx: Sender<Vec<u8>>,
    pending: Vec<u8>,
}

pub struct DuplexEnd {
    half: Arc<Mutex<DuplexHalf>>,
}

/// Create a connected pair of duplex streams (A.write == B.read and back).
pub fn duplex_pair() -> (DuplexEnd, DuplexEnd) {
    let (a_to_b, b_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let (b_to_a, a_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let a = DuplexEnd {
        half: Arc::new(Mutex::new(DuplexHalf {
            rx: a_rx,
            tx: a_to_b,
            pending: Vec::new(),
        })),
    };
    let b = DuplexEnd {
        half: Arc::new(Mutex::new(DuplexHalf {
            rx: b_rx,
            tx: b_to_a,
            pending: Vec::new(),
        })),
    };
    (a, b)
}

impl Read for DuplexEnd {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut half = self.half.lock().expect("duplex lock");
        let mut copied = 0usize;
        loop {
            if !half.pending.is_empty() {
                let take = half.pending.len().min(buf.len() - copied);
                buf[copied..copied + take].copy_from_slice(&half.pending[..take]);
                half.pending.drain(..take);
                copied += take;
                if copied == buf.len() {
                    return Ok(copied);
                }
            }
            // Fill-whole-buffer semantics: once we have partial progress we
            // keep waiting, so a timeout error is only ever reported at a
            // zero-progress point (keeps read_exact retries aligned).
            match half.rx.recv_timeout(Duration::from_millis(2)) {
                Ok(chunk) => half.pending.extend_from_slice(&chunk),
                Err(RecvTimeoutError::Disconnected) => {
                    return if copied > 0 { Ok(copied) } else { Ok(0) };
                }
                Err(RecvTimeoutError::Timeout) => {
                    if copied > 0 {
                        continue;
                    }
                    return Err(std::io::Error::new(ErrorKind::WouldBlock, "duplex idle"));
                }
            }
        }
    }
}

impl Write for DuplexEnd {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.half
            .lock()
            .expect("duplex lock")
            .tx
            .send(buf.to_vec())
            .map(|_| buf.len())
            .map_err(|_| std::io::Error::new(ErrorKind::BrokenPipe, "peer gone"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

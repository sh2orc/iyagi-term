//! Launch gate between the daemon and `iyagi-termd --launch-helper`
//! (spec `02-runner.md` §3). Transport-agnostic: any duplex stream works.
//!
//! NORMATIVE sequence:
//! ```text
//! Helper  --HelperHello(nonce, identity)-->  Daemon   (verify nonce + peer identity)
//! Daemon  --GateTarget-->                     Helper   (target spec, no side effects yet)
//! Daemon  attach-hook()  -> OS group join BEFORE release (fail => Abort)
//! Daemon  --Release-->                        Helper   (single-use!)
//! Helper  runs the target (Unix exec / Windows spawn+wait)
//! Helper  --Started | StartFailed{code}-->    Daemon
//! Helper  --Exited{code}-->                   Daemon   (Windows helper waits; unused on Unix)
//! ```
//!
//! THE invariant: the target process is NEVER created before RELEASE. On any
//! timeout, EOF or Abort the helper exits without creating anything.
//!
//! Framing reuses `term_contracts::rpc::{encode_frame, decode_frame}`
//! (u32 LE length + JSON, 64 KiB cap) — no separate gate codec exists.
//!
//! Timeout model: every wait takes an absolute `Instant` deadline
//! (injectable; the spec default is `GATE_TIMEOUT_MS` = 5 s). Deadline
//! enforcement happens in a [`DeadlineReader`] adapter below the contracts'
//! `decode_frame`: transient read errors (`WouldBlock`/`TimedOut` on
//! timeout-capable transports) are retried until either real data, real EOF
//! or the deadline; the adapter records which one happened so a frame error
//! can be classified as `Timeout` vs `Eof`. This also makes mid-frame
//! transport timeouts SAFE: the adapter keeps blocking per read call instead
//! of erroring with partial progress, so `read_exact` framing never loses
//! alignment.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use term_contracts::gate::{
    DaemonToHelper, GateTarget, HelperHello, HelperToDaemon, GATE_TIMEOUT_MS,
};
use term_contracts::ids::ProcessIdentity;
use term_contracts::rpc::{decode_frame, encode_frame};

/// Spec default deadline helper: 5 s from now.
pub fn default_deadline() -> Instant {
    Instant::now() + default_timeout()
}

pub fn default_timeout() -> Duration {
    Duration::from_millis(GATE_TIMEOUT_MS)
}

/// Any duplex byte stream. Blanket-implemented for everything that is
/// `Read + Write`.
pub trait GateStream: Read + Write {}
impl<T: Read + Write> GateStream for T {}

/// Generate a fresh 256-bit one-time nonce (64 hex chars). The nonce is one
/// factor only; the private endpoint ACL and peer identity are verified
/// alongside it, never via the nonce alone (spec §3).
pub fn generate_nonce() -> String {
    let a = uuid::Uuid::new_v4().simple().to_string();
    let b = uuid::Uuid::new_v4().simple().to_string();
    format!("{a}{b}")
}

#[derive(Debug, thiserror::Error)]
pub enum GateError {
    #[error("gate stream I/O failed: {0}")]
    Io(String),
    #[error("gate deadline expired")]
    Timeout,
    #[error("gate stream closed (EOF)")]
    Eof,
    #[error("gate nonce mismatch")]
    NonceMismatch,
    #[error("helper identity mismatch (pid/start_token/boot_id)")]
    IdentityMismatch,
    #[error("gate protocol violation: {0}")]
    Protocol(String),
    #[error("attach hook failed before release; Abort sent: {0}")]
    AttachFailed(String),
    #[error("daemon aborted the launch before release")]
    Aborted,
    #[error("target failed to start; code {0}")]
    StartFailed(i32),
    #[error("message encoding failed: {0}")]
    Encode(String),
}

fn send_msg<S: GateStream, T: Serialize>(stream: &mut S, msg: &T) -> Result<(), GateError> {
    let value = serde_json::to_value(msg).map_err(|e| GateError::Encode(e.to_string()))?;
    let frame = encode_frame(&value).map_err(|e| GateError::Encode(e.to_string()))?;
    stream
        .write_all(&frame)
        .and_then(|_| stream.flush())
        .map_err(|e| GateError::Io(e.to_string()))
}

/// Deadline-aware reader below `decode_frame`. The contracts' decoder maps
/// EVERY `read_exact` error to `FrameError::Truncated`, so transient
/// timeouts must never reach it as errors: this adapter blocks per read
/// call (retrying `WouldBlock`/`TimedOut`/`Interrupted`) until real data,
/// real EOF, a hard I/O error, or the deadline — and remembers which one
/// occurred so `recv_msg` can classify the frame failure.
struct DeadlineReader<'a, S: GateStream> {
    inner: &'a mut S,
    deadline: Instant,
    hit_deadline: bool,
    saw_eof: bool,
    saw_error: Option<String>,
}

impl<'a, S: GateStream> DeadlineReader<'a, S> {
    fn new(inner: &'a mut S, deadline: Instant) -> Self {
        Self {
            inner,
            deadline,
            hit_deadline: false,
            saw_eof: false,
            saw_error: None,
        }
    }
}

impl<S: GateStream> Read for DeadlineReader<'_, S> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match self.inner.read(buf) {
                Ok(0) => {
                    self.saw_eof = true;
                    return Ok(0);
                }
                Ok(n) => return Ok(n),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    if Instant::now() >= self.deadline {
                        // Fake the EOF to unwind read_exact; `recv_msg`
                        // reclassifies via `hit_deadline`.
                        self.hit_deadline = true;
                        return Ok(0);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => {
                    self.saw_error = Some(e.to_string());
                    return Err(e);
                }
            }
        }
    }
}

/// Read one frame and decode it as `T`, enforcing `deadline`. The adapter's
/// outcome flags disambiguate `Timeout` / `Eof` / hard I/O failure (all of
/// which surface as `FrameError::Truncated` from the contracts' decoder).
fn recv_msg<S: GateStream, T: DeserializeOwned>(
    stream: &mut S,
    deadline: Instant,
) -> Result<T, GateError> {
    let mut reader = DeadlineReader::new(stream, deadline);
    let decoded = decode_frame(&mut reader);
    if reader.hit_deadline {
        return Err(GateError::Timeout);
    }
    if let Some(err) = reader.saw_error {
        return Err(GateError::Io(err));
    }
    if reader.saw_eof {
        return Err(GateError::Eof);
    }
    match decoded {
        Ok(value) => serde_json::from_value(value)
            .map_err(|e| GateError::Protocol(format!("unexpected message shape: {e}"))),
        Err(e) => Err(GateError::Protocol(e.to_string())),
    }
}

/// Result of `GateServer::await_started` (spec §3: RELEASE alone never
/// confirms RUNNING; the daemon waits for this success signal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOutcome {
    Started,
    StartFailed { code: i32 },
}

// ---------------------------------------------------------------------------
// Daemon side

/// Drives the helper through the gate from the daemon side.
pub struct GateServer<S: GateStream> {
    stream: S,
    target_sent: bool,
    released: bool,
}

impl<S: GateStream> GateServer<S> {
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            target_sent: false,
            released: false,
        }
    }

    /// Wait for `HelperHello` (default deadline 5 s) and verify BOTH the
    /// one-time nonce (string equality) and the reported identity
    /// (pid + start_token + boot_id, `ProcessIdentity::same_process`).
    pub fn wait_hello(
        &mut self,
        expected_nonce: &str,
        expected_identity: &ProcessIdentity,
        deadline: Instant,
    ) -> Result<HelperHello, GateError> {
        let hello: HelperHello = recv_msg(&mut self.stream, deadline)?;
        if !hello.nonce.eq(expected_nonce) {
            return Err(GateError::NonceMismatch);
        }
        if !expected_identity.same_process(&hello.identity) {
            return Err(GateError::IdentityMismatch);
        }
        Ok(hello)
    }

    /// Send the target spec. No side effects on the helper until RELEASE.
    pub fn send_target(&mut self, target: &GateTarget) -> Result<(), GateError> {
        send_msg(&mut self.stream, target)?;
        self.target_sent = true;
        Ok(())
    }

    /// Run the attach hook (daemon puts the helper into the OS resource
    /// group BEFORE any release), then send the single-use RELEASE.
    /// * hook failure -> Abort is sent, nothing was released;
    /// * a second RELEASE attempt is a protocol error;
    /// * releasing before `send_target` is a protocol error.
    pub fn release<F>(&mut self, attach: F) -> Result<(), GateError>
    where
        F: FnOnce() -> Result<(), String>,
    {
        if self.released {
            return Err(GateError::Protocol("RELEASE is single-use".into()));
        }
        if !self.target_sent {
            return Err(GateError::Protocol("RELEASE before target spec".into()));
        }
        if let Err(reason) = attach() {
            self.abort()?;
            return Err(GateError::AttachFailed(reason));
        }
        send_msg(&mut self.stream, &DaemonToHelper::Release)?;
        self.released = true;
        Ok(())
    }

    /// Explicit abort (cancellation, admission revoked, ...). Never creates
    /// a target; the helper exits on receipt.
    pub fn abort(&mut self) -> Result<(), GateError> {
        send_msg(&mut self.stream, &DaemonToHelper::Abort)
    }

    /// Await the start report. Windows helper: `Started` / `StartFailed`.
    /// Unix helper: `StartFailed` on exec failure; a clean `GateError::Eof`
    /// right after RELEASE is the SUCCESS signal (the exec closed the
    /// CLOEXEC stream — same semantics as the spec's close-on-exec error
    /// pipe). EOF before any report with an ambiguous cause leaves the
    /// outcome unknown; the spec forbids auto-relaunch precisely because
    /// that ambiguity is inherent.
    pub fn await_started(&mut self, deadline: Instant) -> Result<StartOutcome, GateError> {
        let msg: HelperToDaemon = recv_msg(&mut self.stream, deadline)?;
        match msg {
            HelperToDaemon::Started => Ok(StartOutcome::Started),
            HelperToDaemon::StartFailed { code } => Ok(StartOutcome::StartFailed { code }),
            HelperToDaemon::Exited { code } => Err(GateError::Protocol(format!(
                "Exited({code}) before any start report"
            ))),
        }
    }

    /// Windows helper reports the target's exit through the gate. On Unix
    /// the exec'd target owns the stream and this is unused.
    pub fn await_exited(&mut self, deadline: Instant) -> Result<i32, GateError> {
        let msg: HelperToDaemon = recv_msg(&mut self.stream, deadline)?;
        match msg {
            HelperToDaemon::Exited { code } => Ok(code),
            other => Err(GateError::Protocol(format!(
                "expected Exited, got {other:?}"
            ))),
        }
    }

    /// Whether RELEASE has already been sent.
    pub fn is_released(&self) -> bool {
        self.released
    }

    /// Recover the underlying stream.
    pub fn into_inner(self) -> S {
        self.stream
    }
}

// ---------------------------------------------------------------------------
// Helper side

/// Daemon->helper frames: the target spec plus the command enum arrive on
/// the same stream in a fixed order (target first, then Release/Abort).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DaemonFrame {
    Target(GateTarget),
    Cmd(DaemonToHelper),
}

/// Helper-side gate driver.
pub struct GateClient<S: GateStream> {
    stream: S,
}

impl<S: GateStream> GateClient<S> {
    pub fn new(stream: S) -> Self {
        Self { stream }
    }

    /// Send the hello (nonce + own identity) immediately after connecting.
    pub fn hello(&mut self, nonce: &str, identity: &ProcessIdentity) -> Result<(), GateError> {
        send_msg(
            &mut self.stream,
            &HelperHello {
                nonce: nonce.to_string(),
                identity: identity.clone(),
            },
        )
    }

    /// Wait for RELEASE (with the target spec that precedes it). Abort,
    /// timeout or EOF are errors: the helper exits WITHOUT creating the
    /// target in every one of those cases.
    pub fn await_release(&mut self, deadline: Instant) -> Result<GateTarget, GateError> {
        let mut target: Option<GateTarget> = None;
        loop {
            let frame: DaemonFrame = recv_msg(&mut self.stream, deadline)?;
            match frame {
                DaemonFrame::Target(t) => {
                    if target.replace(t).is_some() {
                        return Err(GateError::Protocol("duplicate target spec".into()));
                    }
                }
                DaemonFrame::Cmd(DaemonToHelper::Release) => {
                    return target.ok_or_else(|| {
                        GateError::Protocol("RELEASE without a preceding target spec".into())
                    });
                }
                DaemonFrame::Cmd(DaemonToHelper::Abort) => return Err(GateError::Aborted),
            }
        }
    }

    /// Run the released target. Unix: exec replaces the helper (keeps the
    /// controlling terminal; never returns on success). Windows: spawn the
    /// target as a child inside the inherited Job, wait for it, report
    /// `Exited{code}` through the gate, return the code.
    pub fn run_target(&mut self, target: GateTarget) -> Result<i32, GateError> {
        self.run_target_platform(target)
    }

    #[cfg(windows)]
    fn run_target_platform(&mut self, target: GateTarget) -> Result<i32, GateError> {
        let mut cmd = std::process::Command::new(&target.program);
        if !target.argv.is_empty() {
            cmd.args(&target.argv[1..]);
        }
        if !target.cwd.is_empty() {
            cmd.current_dir(&target.cwd);
        }
        if target.env_clear {
            cmd.env_clear();
        }
        // Inherited names the daemon wants gone (provider/auth vars) are
        // dropped BEFORE the overrides so an override of the same name wins.
        for name in &target.env_remove {
            cmd.env_remove(name);
        }
        cmd.envs(&target.env_overrides);
        // Inherit stdio: the helper is a ConPTY child, so the target lands on
        // the same pseudoconsole. No new console, no breakaway: the child is
        // automatically a member of the helper's Job.
        match cmd.spawn() {
            Ok(mut child) => {
                send_msg(&mut self.stream, &HelperToDaemon::Started)?;
                let status = child.wait().map_err(|e| GateError::Io(e.to_string()))?;
                let code = status.code().unwrap_or(1);
                send_msg(&mut self.stream, &HelperToDaemon::Exited { code })?;
                Ok(code)
            }
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(-1);
                send_msg(&mut self.stream, &HelperToDaemon::StartFailed { code })?;
                Err(GateError::StartFailed(code))
            }
        }
    }

    #[cfg(unix)]
    fn run_target_platform(&mut self, target: GateTarget) -> Result<i32, GateError> {
        use std::io::Write as _;
        use std::os::unix::net::UnixStream;
        use std::os::unix::process::CommandExt as _;

        // Close-on-exec error pipe: Rust creates both socketpair ends with
        // CLOEXEC, so a successful exec closes the write end (peer sees EOF)
        // while a failed exec leaves the helper alive to write the errno.
        let (mut err_write, _err_read) =
            UnixStream::pair().map_err(|e| GateError::Io(e.to_string()))?;

        let mut cmd = std::process::Command::new(&target.program);
        if let Some(argv0) = target.argv.first() {
            cmd.arg0(argv0);
        }
        if target.argv.len() > 1 {
            cmd.args(&target.argv[1..]);
        }
        if !target.cwd.is_empty() {
            cmd.current_dir(&target.cwd);
        }
        if target.env_clear {
            cmd.env_clear();
        }
        // Inherited names the daemon wants gone (provider/auth vars) are
        // dropped BEFORE the overrides so an override of the same name wins.
        for name in &target.env_remove {
            cmd.env_remove(name);
        }
        cmd.envs(&target.env_overrides);

        // NO Started message is sent before exec on Unix: success is
        // signalled by the gate stream CLOEXEC-closing at exec time, which
        // the daemon observes as EOF right after RELEASE (mirroring the
        // spec's close-on-exec error-pipe EOF semantics on the transport
        // available without fd passing). Failure is always observable as an
        // explicit StartFailed frame, so there is no Started-then-failed race.
        let exec_error = cmd.exec(); // returns only on failure
        let code = exec_error.raw_os_error().unwrap_or(2);
        let _ = err_write.write_all(&(code as u32).to_le_bytes());
        let _ = err_write.flush();
        send_msg(&mut self.stream, &HelperToDaemon::StartFailed { code })?;
        Err(GateError::StartFailed(code))
    }

    /// Recover the underlying stream.
    pub fn into_inner(self) -> S {
        self.stream
    }
}

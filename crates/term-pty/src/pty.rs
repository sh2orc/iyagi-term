//! PTY wrapper over portable-pty 0.9 (spec `02-runner.md` §2–3).
//!
//! Ownership rules enforced here:
//! * the master stays alive for the whole session (dropping it closes the
//!   pty), and it is never handed to UI objects;
//! * the writer is acquired exactly ONCE (`take_writer`) and kept for the
//!   session lifetime — attach/detach never drops it because dropping the
//!   portable-pty writer sends EOF to the child. Consumers clone the guard
//!   per write via [`PtyHandle::write_input`], never the writer itself;
//! * readers are obtained per consumer through `try_clone_reader`;
//! * `argv` is the FULL argv vector including `argv[0]` (matches
//!   `GateTarget`); the element after the program is forwarded as arguments.
//!
//! This crate is sync-thread based (one reader thread + one writer thread per
//! session); no blocking PTY I/O happens on any async runtime.

use std::collections::BTreeMap;
use std::io::{Read, Write};
#[cfg(unix)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use portable_pty::{native_pty_system, Child, CommandBuilder, ExitStatus, MasterPty, PtySize};
use term_contracts::session::limits::{TERMINAL_MAX_DIM, TERMINAL_MIN_DIM};

#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("pty open failed: {0}")]
    Open(String),
    #[error("spawning the first pty child failed: {0}")]
    Spawn(String),
    #[error("invalid terminal size {cols}x{rows}: each dimension must be {min}..={max}")]
    InvalidSize {
        cols: u16,
        rows: u16,
        min: u16,
        max: u16,
    },
    #[error("terminal resize failed: {0}")]
    Resize(String),
    #[error("polling child exit failed: {0}")]
    Wait(String),
    #[error("killing child failed: {0}")]
    Kill(String),
    #[error("obtaining pty reader failed: {0}")]
    Reader(String),
    #[error("pty I/O failed: {0}")]
    Io(String),
}

/// Locale variables that decide the child's charset, in POSIX precedence
/// order (LC_ALL > LC_CTYPE > LANG).
const LOCALE_VARS: [&str; 3] = ["LC_ALL", "LC_CTYPE", "LANG"];

/// Validate terminal dimensions (spec `02-runner.md` §6: 0x0 and values
/// outside the supported range are rejected before anything is applied).
pub fn validate_size(cols: u16, rows: u16) -> Result<(), PtyError> {
    let in_range = |v: u16| (TERMINAL_MIN_DIM..=TERMINAL_MAX_DIM).contains(&v);
    if in_range(cols) && in_range(rows) {
        Ok(())
    } else {
        Err(PtyError::InvalidSize {
            cols,
            rows,
            min: TERMINAL_MIN_DIM,
            max: TERMINAL_MAX_DIM,
        })
    }
}

/// `true` when a locale value selects a UTF-8 charset ("C.UTF-8",
/// "ko_KR.UTF-8", "utf-8", ...). Punctuation-insensitive so every spelling
/// the platform libraries accept is recognized.
fn value_selects_utf8(value: &str) -> bool {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect::<String>()
        .contains("UTF8")
}

/// Locale variables the child will see: launch overrides win over the
/// daemon's inherited environment, one value per variable.
fn effective_locale_vars(env_overrides: &BTreeMap<String, String>) -> [Option<String>; 3] {
    let mut vars = [None, None, None];
    for (slot, name) in vars.iter_mut().zip(LOCALE_VARS) {
        *slot = env_overrides
            .get(name)
            .cloned()
            .or_else(|| std::env::var(name).ok());
    }
    vars
}

/// The LC_CTYPE default to layer onto the child, if any:
/// - explicit `LC_ALL`/`LC_CTYPE` anywhere → `None` (a caller who pinned the
///   charset, UTF-8 or not, is respected — same policy as color overrides);
/// - `LANG` already UTF-8 → `None`;
/// - no locale variables at all (GUI launch) or a non-UTF-8 `LANG` such as
///   `C`/`ko_KR.EUC-KR` → the platform UTF-8 default (POSIX: LC_CTYPE beats
///   LANG, so this upgrades without fighting the inherited value).
pub(crate) fn utf8_ctype_default(
    vars: &[Option<String>; 3],
    platform_default: Option<&str>,
) -> Option<String> {
    let platform_default = platform_default?;
    let [lc_all, lc_ctype, lang] = vars;
    if lc_all.is_some() || lc_ctype.is_some() {
        return None;
    }
    match lang {
        Some(value) if value_selects_utf8(value) => None,
        _ => Some(platform_default.to_string()),
    }
}

/// Platform's UTF-8 LC_CTYPE default. Windows returns `None`: ConPTY
/// negotiates charsets internally (UTF-16 pipe) and locale variables are not
/// part of that contract.
fn platform_utf8_ctype() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        Some(macos_utf8_ctype())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Some("C.UTF-8".to_string())
    }
    #[cfg(windows)]
    {
        None
    }
}

/// macOS: the user's region preference (`AppleLocale` `ko_KR` →
/// `ko_KR.UTF-8`), validated against `locale -a` — an unknown value would
/// silently fall back to the C locale again. Cached: spawn latency pays for
/// the two helper processes once per daemon.
#[cfg(target_os = "macos")]
fn macos_utf8_ctype() -> String {
    static DEFAULT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DEFAULT
        .get_or_init(|| {
            let preference = std::process::Command::new("defaults")
                .args(["read", "-g", "AppleLocale"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .and_then(|locale| locale.split('@').next().map(str::to_string))
                .filter(|base| !base.is_empty());
            let mut candidates: Vec<String> = preference
                .map(|base| format!("{base}.UTF-8"))
                .into_iter()
                .collect();
            candidates.push("en_US.UTF-8".to_string());
            candidates.push("C.UTF-8".to_string());
            let available = std::process::Command::new("locale")
                .arg("-a")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .map(|line| line.trim().to_string())
                        .collect::<std::collections::HashSet<_>>()
                });
            let chosen = match available {
                Some(set) => candidates.into_iter().find(|c| set.contains(c)),
                None => candidates.first().cloned(),
            };
            // Unreachable in practice: macOS always lists en_US.UTF-8/C.UTF-8.
            chosen.unwrap_or_else(|| {
                tracing::debug!("locale -a listed no known UTF-8 locale; using C.UTF-8");
                "C.UTF-8".to_string()
            })
        })
        .clone()
}

/// Guarantee the child a UTF-8 charset. The frontend speaks UTF-8 end to end
/// (xterm rendering, base64 transport), so a PTY child in the C locale's
/// single-byte mode mangles multibyte input — Hangul commits byte-wise and
/// backspace/arrows act on bytes. GUI launches inherit no LANG/LC_* at all
/// (the same gap as TERM above), so default LC_CTYPE when nothing explicit
/// pins the charset. Message language (LANG) is deliberately left alone.
fn apply_utf8_ctype_default(cmd: &mut CommandBuilder, env_overrides: &BTreeMap<String, String>) {
    let vars = effective_locale_vars(env_overrides);
    if let Some(value) = utf8_ctype_default(&vars, platform_utf8_ctype().as_deref()) {
        cmd.env("LC_CTYPE", value);
    }
}

/// A session-owned PTY: master + once-acquired writer + first child handle.
pub struct PtyHandle {
    /// `None` after [`PtyHandle::close`]: the master is the pty's lifetime
    /// owner. On Windows taking+dropping it runs `ClosePseudoConsole`, which
    /// unblocks a reader blocked in `read()` — killing the child alone does
    /// NOT do that on ConPTY. On Unix the reader owns its own dup of the
    /// master fd, so closing here alone would not wake it; the
    /// [`PollingReader`] stop flag does (see [`PtyHandle::close`]).
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    /// Shared so the Windows ConPTY query responder (see [`PtyHandle::reader`])
    /// can reply to conhost while the writer thread owns input writes.
    /// `None` after [`PtyHandle::close`]: the writer holds its own dup of
    /// the master, and the pty only hangs up once every master fd is gone.
    writer: std::sync::Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    pid: Option<u32>,
    /// Unix: tells [`PollingReader`]s to return EOF at their next poll step,
    /// and the non-blocking input writer to give up its pending chunk.
    #[cfg(unix)]
    reader_stop: std::sync::Arc<AtomicBool>,
    /// Monotonic ms (+1, so 0 means "not blocked") since which the current
    /// input write has made no progress because the tty input queue is full
    /// — the foreground program is not reading (hung, busy, stopped). See
    /// [`PtyHandle::input_blocked_marker`].
    input_blocked_since: std::sync::Arc<AtomicU64>,
}

/// Monotonic millisecond clock shared by the input-stall marker.
fn monotonic_ms() -> u64 {
    // input.rs의 SystemClock과 같은 원천(프로세스 단 하나의 epoch)을 쓴다 —
    // 사본을 두면 스톨 마커와 actor의 시간 판정이 서로 다른 시계를 읽게 된다.
    use crate::input::Clock as _;
    crate::input::SystemClock.now_ms()
}

/// How long an input write has been stalled, from a marker returned by
/// [`PtyHandle::input_blocked_marker`]. `None` while input flows.
pub fn input_blocked_for(marker: &AtomicU64) -> Option<std::time::Duration> {
    match marker.load(Ordering::Acquire) {
        0 => None,
        since => Some(std::time::Duration::from_millis(
            monotonic_ms().saturating_sub(since - 1),
        )),
    }
}

/// Backoff bounds while the tty input queue is full (Unix non-blocking
/// writer). Only a stalled session pays these wake-ups.
#[cfg(unix)]
const INPUT_RETRY_MIN_MS: u64 = 5;
#[cfg(unix)]
const INPUT_RETRY_MAX_MS: u64 = 100;

/// Unix reader over a private dup of the master fd. portable-pty's
/// `try_clone_reader` also dups the fd, but a blocking `read()` on it only
/// returns when data arrives or EVERY slave fd is closed — a descendant that
/// kept the slave (`sleep 1000 &` from an exited shell) blocks the reader
/// thread, and with it the actor's `join`, for as long as it lives.
/// Polling with a bounded step lets [`PtyHandle::close`] end the thread
/// promptly; the fd is closed on drop so the pty can hang up.
#[cfg(unix)]
struct PollingReader {
    fd: std::os::unix::io::RawFd,
    stop: std::sync::Arc<AtomicBool>,
}

/// Poll step for [`PollingReader`]: bounds how long `close()` waits for the
/// reader thread to notice the stop flag.
#[cfg(unix)]
const READER_POLL_STEP_MS: libc::c_int = 250;

#[cfg(unix)]
impl Read for PollingReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.stop.load(Ordering::Acquire) {
                // Reported as EOF: the actor treats it exactly like a pty
                // hangup (the session is finalizing anyway).
                return Ok(0);
            }
            let mut pfd = libc::pollfd {
                fd: self.fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `pfd` is a valid, initialized pollfd for the duration
            // of the call; the fd is owned by this reader until drop.
            let ready = unsafe { libc::poll(&mut pfd, 1, READER_POLL_STEP_MS) };
            if ready < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            if ready == 0 {
                continue;
            }
            // Readable, hung up or errored: a read returns at once in all
            // three cases (data / EOF / EIO — the actor maps errors to
            // EOF). `spawn()` sets O_NONBLOCK on the master's open file
            // description, which this reader's dup shares — so a read can
            // still come back WouldBlock (spuriously, right after poll
            // said readable). Treat it like a poll timeout and go around
            // again; the writer side handles the flag symmetrically with
            // its own bounded WouldBlock retry.
            // SAFETY: `buf` is a valid writable slice of `buf.len()` bytes.
            let n = unsafe { libc::read(self.fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) {
                    continue;
                }
                return Err(err);
            }
            return Ok(n as usize);
        }
    }
}

#[cfg(unix)]
impl Drop for PollingReader {
    fn drop(&mut self) {
        // SAFETY: the fd was created by F_DUPFD_CLOEXEC in `PtyHandle::reader`
        // and is owned exclusively by this reader.
        unsafe {
            libc::close(self.fd);
        }
    }
}

impl PtyHandle {
    /// Open a pty at `cols`x`rows` and spawn `program` as its FIRST child
    /// (the launch-helper in the gate flow; a plain CLI for direct tests).
    ///
    /// Environment layering, in order: the daemon's inherited environment,
    /// the PTY baseline (TERM/COLORTERM/TERM_PROGRAM, UTF-8 LC_CTYPE
    /// default), then every name in `env_remove` is dropped, then
    /// `env_overrides` are applied. `env_remove` neutralizes inherited
    /// provider/auth variables (e.g. `ANTHROPIC_API_KEY` for a routed Claude
    /// launch) without the caller having to know the daemon's environment;
    /// an override of a removed name still wins because it is applied last.
    /// Names that are not present are ignored.
    pub fn spawn(
        cols: u16,
        rows: u16,
        program: &str,
        argv: &[String],
        env_overrides: &BTreeMap<String, String>,
        env_remove: &[String],
        cwd: Option<&str>,
    ) -> Result<Self, PtyError> {
        validate_size(cols, rows)?;
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::Open(e.to_string()))?;
        let mut cmd = CommandBuilder::new(program);
        for arg in argv.iter().skip(1) {
            cmd.arg(arg);
        }
        // Advertise our xterm renderer, independent of the daemon's parent
        // terminal (GUI launches may have no TERM, agents may pass TERM=dumb).
        // Color policy from an IDE/agent belongs to its captured output, not
        // this interactive PTY. Explicit launch overrides below still win.
        for key in ["NO_COLOR", "FORCE_COLOR", "CLICOLOR", "CLICOLOR_FORCE"] {
            cmd.env_remove(key);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "iyagi");
        apply_utf8_ctype_default(&mut cmd, env_overrides);
        // Caller-requested removals come AFTER the baseline and BEFORE the
        // overrides: the child must not see the daemon's inherited copy of
        // these names, but an explicit override for the same name is the
        // launch's decision and is re-added below.
        for key in env_remove {
            cmd.env_remove(key);
        }
        for (key, value) in env_overrides {
            cmd.env(key, value);
        }
        if let Some(dir) = cwd.filter(|d| !d.is_empty()) {
            cmd.cwd(dir);
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| PtyError::Spawn(e.to_string()))?;
        // The first child now owns the slave side; keeping our slave open
        // would prevent EOF semantics, so drop it immediately after spawn.
        drop(pair.slave);
        // Writer acquired ONCE, kept for the session lifetime.
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| PtyError::Io(e.to_string()))?;
        // Unix: the master's open file description goes non-blocking. A
        // blocking write into a full tty input queue (a raw-mode TUI that
        // stopped reading: hung, busy, SIGSTOPped) never returns — it held
        // the writer lock forever, so every later key queued behind it and
        // `close()` (plus portable-pty's EOF-on-drop write) wedged the
        // session finalize, leaving its processes alive. The flag is shared
        // by the writer and reader dups: `write_input` retries WouldBlock
        // with a bounded backoff, and `PollingReader` already polls first
        // and treats WouldBlock as "poll again". Failure to set it keeps
        // the old blocking behaviour.
        #[cfg(unix)]
        if let Some(fd) = pair.master.as_raw_fd() {
            // SAFETY: fcntl on a valid descriptor owned by the master.
            let ok = unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                flags >= 0 && libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) == 0
            };
            if !ok {
                tracing::warn!(
                    error = %std::io::Error::last_os_error(),
                    "pty master could not be made non-blocking; input writes may block"
                );
            }
        }
        let pid = child.process_id();
        Ok(Self {
            master: Mutex::new(Some(pair.master)),
            writer: std::sync::Arc::new(Mutex::new(Some(writer))),
            child: Mutex::new(child),
            pid,
            input_blocked_since: std::sync::Arc::new(AtomicU64::new(0)),
            #[cfg(unix)]
            reader_stop: std::sync::Arc::new(AtomicBool::new(false)),
        })
    }

    /// Close the master and the writer, and (Unix) stop every reader
    /// obtained from [`PtyHandle::reader`]. Readers return EOF within one
    /// poll step, so a session's reader thread can always be joined even
    /// while a descendant still holds the slave; once the reader's dup is
    /// gone too, no master fd remains and the pty hangs up. Windows:
    /// `ClosePseudoConsole` unblocks the reader. Further IO fails.
    /// Idempotent.
    pub fn close(&self) {
        #[cfg(unix)]
        self.reader_stop.store(true, Ordering::Release);
        let taken = self.master.lock().unwrap_or_else(|p| p.into_inner()).take();
        drop(taken);
        let writer = self.lock_writer().take();
        drop(writer);
    }

    pub fn is_closed(&self) -> bool {
        self.master
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_none()
    }

    /// A fresh readable end (output of the child side). Each consumer gets
    /// its own clone via `try_clone_reader`.
    ///
    /// On Windows the clone is wrapped in a [`ConPtyQueryResponder`]:
    /// conhost sends a cursor-position request (`ESC[6n`) when a ConPTY
    /// starts (and after resizes, due to the resize quirk) and delays
    /// passing the child's output through until it gets a response. A real
    /// terminal emulator answers; the daemon is a transport, so the reader
    /// path answers on its behalf. The query bytes are still delivered to
    /// the consumer unchanged — the journal stays byte-exact.
    ///
    /// On Unix the reader is a [`PollingReader`] over a private
    /// close-on-exec dup of the master fd, stoppable by [`PtyHandle::close`].
    pub fn reader(&self) -> Result<Box<dyn Read + Send>, PtyError> {
        let master = self.master.lock().unwrap_or_else(|p| p.into_inner());
        let master_ref = master
            .as_ref()
            .ok_or_else(|| PtyError::Reader("pty closed".into()))?;
        #[cfg(windows)]
        {
            let raw: Box<dyn Read + Send> = master_ref
                .try_clone_reader()
                .map_err(|e| PtyError::Reader(e.to_string()))?;
            drop(master);
            Ok(Box::new(ConPtyQueryResponder::new(
                raw,
                std::sync::Arc::clone(&self.writer),
            )))
        }
        #[cfg(unix)]
        {
            let fd = master_ref
                .as_raw_fd()
                .ok_or_else(|| PtyError::Reader("pty master has no fd".into()))?;
            // SAFETY: `fd` is a valid open descriptor owned by the master for
            // the duration of this call (the lock is held).
            let dup = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
            drop(master);
            if dup < 0 {
                return Err(PtyError::Reader(
                    std::io::Error::last_os_error().to_string(),
                ));
            }
            Ok(Box::new(PollingReader {
                fd: dup,
                stop: std::sync::Arc::clone(&self.reader_stop),
            }))
        }
    }

    /// Write raw input to the child (writer thread path). One outstanding
    /// write at a time is guaranteed by the single writer thread that owns
    /// all calls to this method.
    ///
    /// Unix: the master is non-blocking (see [`PtyHandle::spawn`]). While
    /// the tty input queue is full the write retries with a bounded backoff,
    /// holding the writer lock only for each attempt, so [`PtyHandle::close`]
    /// is never blocked behind it; the stall is published through
    /// [`PtyHandle::input_blocked_marker`] and `close()` aborts the chunk.
    pub fn write_input(&self, bytes: &[u8]) -> Result<usize, PtyError> {
        let result = self.write_input_inner(bytes);
        self.input_blocked_since.store(0, Ordering::Release);
        result
    }

    #[cfg(not(unix))]
    fn write_input_inner(&self, bytes: &[u8]) -> Result<usize, PtyError> {
        let mut writer = self.lock_writer();
        writer
            .as_mut()
            .ok_or_else(|| PtyError::Io("pty closed".into()))?
            .write_all(bytes)
            .map(|_| bytes.len())
            .map_err(|e| PtyError::Io(e.to_string()))
    }

    #[cfg(unix)]
    fn write_input_inner(&self, bytes: &[u8]) -> Result<usize, PtyError> {
        let mut written = 0usize;
        let mut backoff_ms = INPUT_RETRY_MIN_MS;
        while written < bytes.len() {
            if self.reader_stop.load(Ordering::Acquire) {
                return Err(PtyError::Io("pty closed".into()));
            }
            let attempt = {
                let mut writer = self.lock_writer();
                match writer.as_mut() {
                    Some(w) => w.write(&bytes[written..]),
                    None => return Err(PtyError::Io("pty closed".into())),
                }
            };
            match attempt {
                Ok(0) => return Err(PtyError::Io("pty accepted no bytes".into())),
                Ok(n) => {
                    written += n;
                    backoff_ms = INPUT_RETRY_MIN_MS;
                    self.input_blocked_since.store(0, Ordering::Release);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                    // EINTR 자체는 드물지만 시그널이 촘촘한 순간(SIGCHLD·타이머)
                    // 에는 잠금을 쥔 스핀이 된다 — 최소 한 박자 양보한다(진행이
                    // 아니므로 막힘 시계는 그대로).
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    let _ = self.input_blocked_since.compare_exchange(
                        0,
                        monotonic_ms() + 1,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                    std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                    backoff_ms = (backoff_ms * 2).min(INPUT_RETRY_MAX_MS);
                }
                Err(e) => return Err(PtyError::Io(e.to_string())),
            }
        }
        Ok(bytes.len())
    }

    /// Shared marker of the current input stall (read it with
    /// [`input_blocked_for`]). Non-zero only while a write is waiting on a
    /// full tty input queue.
    pub fn input_blocked_marker(&self) -> std::sync::Arc<AtomicU64> {
        std::sync::Arc::clone(&self.input_blocked_since)
    }

    /// Apply a new size. Validates `2..=1000` on each dimension first; the
    /// pty keeps its previous size when validation fails.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), PtyError> {
        validate_size(cols, rows)?;
        let master = self.master.lock().unwrap_or_else(|p| p.into_inner());
        match master.as_ref() {
            Some(m) => m
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| PtyError::Resize(e.to_string())),
            None => Err(PtyError::Resize("pty closed".into())),
        }
    }

    /// Current kernel-known size as `(cols, rows)`.
    pub fn size(&self) -> Result<(u16, u16), PtyError> {
        let master = self.master.lock().unwrap_or_else(|p| p.into_inner());
        match master.as_ref() {
            Some(m) => {
                let size = m.get_size().map_err(|e| PtyError::Resize(e.to_string()))?;
                Ok((size.cols, size.rows))
            }
            None => Err(PtyError::Resize("pty closed".into())),
        }
    }

    /// Non-blocking exit poll (`try_wait`): `None` while the child runs.
    pub fn poll_exit(&self) -> Result<Option<ExitStatus>, PtyError> {
        self.child
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .try_wait()
            .map_err(|e| PtyError::Wait(e.to_string()))
    }

    /// Terminate the first child (cancel path; group teardown of descendants
    /// is the platform layer's job, not the pty's).
    pub fn kill(&self) -> Result<(), PtyError> {
        self.child
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .kill()
            .map_err(|e| PtyError::Kill(e.to_string()))
    }

    /// Pid of the first child, if the platform reports one.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    fn lock_writer(&self) -> MutexGuard<'_, Option<Box<dyn Write + Send>>> {
        self.writer.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// conhost's cursor-position request. It carries no parameters.
#[cfg(windows)]
const DSR_QUERY: &[u8; 4] = b"\x1b[6n";
/// Minimal valid CPR reply (row 1, column 1). conhost only needs SOME
/// well-formed response before it releases the child's output.
#[cfg(windows)]
const DSR_REPLY: &[u8; 6] = b"\x1b[1;1R";

/// Windows-only transport fix: answers conhost's `ESC[6n` so the ConPTY
/// output path is not stalled at startup (see [`PtyHandle::reader`]).
/// Bytes are passed through unmodified.
#[cfg(windows)]
struct ConPtyQueryResponder {
    inner: Box<dyn Read + Send>,
    writer: std::sync::Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    /// Rolling tail of recently delivered bytes so a query split across
    /// chunk boundaries is still detected.
    tail: Vec<u8>,
}

#[cfg(windows)]
impl ConPtyQueryResponder {
    fn new(
        inner: Box<dyn Read + Send>,
        writer: std::sync::Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    ) -> Self {
        Self {
            inner,
            writer,
            tail: Vec::new(),
        }
    }

    fn maybe_reply(&mut self, chunk: &[u8]) {
        let mut window = self.tail.clone();
        window.extend_from_slice(chunk);
        if window.windows(DSR_QUERY.len()).any(|w| w == DSR_QUERY) {
            // Never block the reader on the writer: try_lock and skip if
            // the writer thread holds it mid-write (conhost re-asks).
            if let Ok(mut writer) = self.writer.try_lock() {
                if let Some(writer) = writer.as_mut() {
                    let _ = writer.write_all(DSR_REPLY);
                    let _ = writer.flush();
                }
            }
        }
        let keep = window.len().min(DSR_QUERY.len() - 1);
        self.tail.clear();
        self.tail.extend_from_slice(&window[window.len() - keep..]);
    }
}

#[cfg(windows)]
impl Read for ConPtyQueryResponder {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.maybe_reply(&buf[..n]);
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: Option<&str> = Some("ko_KR.UTF-8");

    fn vars(
        lc_all: Option<&str>,
        lc_ctype: Option<&str>,
        lang: Option<&str>,
    ) -> [Option<String>; 3] {
        [lc_all, lc_ctype, lang].map(|v| v.map(str::to_string))
    }

    #[test]
    fn ctype_defaults_when_no_locale_vars_exist() {
        // GUI launch: nothing in the daemon env or overrides.
        assert_eq!(
            utf8_ctype_default(&vars(None, None, None), DEFAULT),
            Some("ko_KR.UTF-8".to_string())
        );
    }

    #[test]
    fn ctype_skipped_when_lang_already_utf8() {
        for lang in ["C.UTF-8", "ko_KR.UTF-8", "c.utf-8", "UTF-8"] {
            assert_eq!(
                utf8_ctype_default(&vars(None, None, Some(lang)), DEFAULT),
                None
            );
        }
    }

    #[test]
    fn ctype_upgrades_non_utf8_lang() {
        // `LANG=C`/POSIX/EUC-KR children edit text byte-wise; LC_CTYPE
        // layers a UTF-8 charset on top without touching LANG.
        for lang in ["C", "POSIX", "ko_KR.EUC-KR"] {
            assert_eq!(
                utf8_ctype_default(&vars(None, None, Some(lang)), DEFAULT),
                Some("ko_KR.UTF-8".to_string())
            );
        }
    }

    #[test]
    fn explicit_ctype_or_lc_all_is_respected_verbatim() {
        // A pinned charset — even non-UTF-8 — is the caller's choice.
        assert_eq!(
            utf8_ctype_default(&vars(Some("C"), None, Some("C.UTF-8")), DEFAULT),
            None
        );
        assert_eq!(
            utf8_ctype_default(&vars(None, Some("en_US.ISO-8859-1"), None), DEFAULT),
            None
        );
        // An UTF-8 LC_CTYPE needs no default either.
        assert_eq!(
            utf8_ctype_default(&vars(None, Some("ko_KR.UTF-8"), Some("C")), DEFAULT),
            None
        );
    }

    #[test]
    fn overrides_win_over_inherited_env_per_variable() {
        let vars =
            effective_locale_vars(&BTreeMap::from([("LC_CTYPE".to_string(), "1".to_string())]));
        // LC_CTYPE comes from the override; the others mirror this test
        // process's environment, so only the override slot is asserted.
        assert_eq!(vars[1].as_deref(), Some("1"));
    }

    #[test]
    fn no_platform_default_means_no_layering() {
        // Windows: ConPTY negotiates charsets internally.
        assert_eq!(utf8_ctype_default(&vars(None, None, None), None), None);
    }
}

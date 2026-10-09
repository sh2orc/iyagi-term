//! Bounded, local CLI version lookup. No task, credentials, or model request.
use super::installation_identity::ExecutableIdentity;
use std::io;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;
use term_contracts::mission::rpc::InstallationStatus;
use term_contracts::mission::types::RuntimeKind;
use tokio::io::{AsyncRead, AsyncReadExt};

const OUTPUT_LIMIT: usize = 4096;
const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeFailure {
    NotFound,
    Failed,
    TimedOut,
    OutputLimit,
    UnrecognizedVersion,
    Changed,
}

impl ProbeFailure {
    /// Wire classification shared by `binding.probe` and `runtime.detect`.
    pub fn installation_status(self) -> InstallationStatus {
        match self {
            ProbeFailure::NotFound => InstallationStatus::NotFound,
            ProbeFailure::Failed | ProbeFailure::Changed => InstallationStatus::Failed,
            ProbeFailure::TimedOut => InstallationStatus::TimedOut,
            ProbeFailure::OutputLimit => InstallationStatus::OutputLimit,
            ProbeFailure::UnrecognizedVersion => InstallationStatus::UnrecognizedVersion,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    pub version: String,
    /// None is reserved for explicitly injected protocol-fixture evidence.
    pub executable: Option<ExecutableIdentity>,
}

pub fn observe(program: &str, runtime: RuntimeKind) -> Result<Installation, ProbeFailure> {
    observe_expected(program, runtime, None)
}

pub fn observe_expected(
    program: &str,
    runtime: RuntimeKind,
    expected: Option<&ExecutableIdentity>,
) -> Result<Installation, ProbeFailure> {
    let deadline = std::time::Instant::now() + TIMEOUT;
    let before = ExecutableIdentity::capture(program, deadline)?;
    if expected.is_some_and(|expected| expected != &before) {
        return Err(ProbeFailure::Changed);
    }
    let remaining = deadline
        .checked_duration_since(std::time::Instant::now())
        .ok_or(ProbeFailure::TimedOut)?;
    let version = inspect(program, runtime, remaining)?;
    let after = ExecutableIdentity::capture(program, deadline)?;
    if before != after {
        return Err(ProbeFailure::Changed);
    }
    Ok(Installation {
        version,
        executable: Some(after),
    })
}

pub fn version(program: &str, runtime: RuntimeKind) -> Result<String, ProbeFailure> {
    inspect(program, runtime, TIMEOUT)
}

/// Bounded stdout of a read-only local listing command — same footing as the
/// version probe: explicit argv (never a shell string), null stdin, killed
/// process group on timeout, and a caller-set byte cap per stream so a chatty
/// CLI cannot grow the daemon. No task, credentials, or model request.
pub fn capture_stdout(
    program: &str,
    args: &[&str],
    limit: usize,
    timeout: Duration,
) -> Result<String, ProbeFailure> {
    // Same scoped-thread reasoning as `inspect`: RPC callers can already be
    // inside a Tokio runtime, and this short-lived IO runtime always joins.
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("cli-listing-probe".into())
            .spawn_scoped(scope, || {
                let runtime_io = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| ProbeFailure::Failed)?;
                runtime_io
                    .block_on(capture(program, args, limit, timeout))
                    .map(|(stdout, _)| stdout)
            })
            .map_err(|_| ProbeFailure::Failed)?
            .join()
            .unwrap_or(Err(ProbeFailure::Failed))
    })
}

fn inspect(program: &str, runtime: RuntimeKind, timeout: Duration) -> Result<String, ProbeFailure> {
    // RPC callers can already be inside a Tokio runtime. A scoped thread owns
    // this short-lived IO runtime and always joins; no detached watchdog.
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("cli-version-probe".into())
            .spawn_scoped(scope, || {
                let runtime_io = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| ProbeFailure::Failed)?;
                let (stdout, stderr) =
                    runtime_io.block_on(capture(program, &["--version"], OUTPUT_LIMIT, timeout))?;
                let text = if stdout.trim().is_empty() {
                    &stderr
                } else {
                    &stdout
                };
                parse_version(runtime, text).ok_or(ProbeFailure::UnrecognizedVersion)
            })
            .map_err(|_| ProbeFailure::Failed)?
            .join()
            .unwrap_or(Err(ProbeFailure::Failed))
    })
}

pub fn parse_version(runtime: RuntimeKind, text: &str) -> Option<String> {
    let line = text.lines().next()?.trim();
    let candidate = match runtime {
        RuntimeKind::Codex => line.strip_prefix("codex-cli ")?,
        RuntimeKind::Claude => line.strip_suffix(" (Claude Code)")?,
        RuntimeKind::Opencode => line.strip_prefix("opencode ").unwrap_or(line),
        RuntimeKind::Fake => return None,
    };
    // Keep the exact version, including prerelease/build suffixes. Never
    // collapse a newer build into an older compatibility-evidence key.
    let base = candidate.split(['-', '+']).next()?;
    let parts: Vec<_> = base.split('.').collect();
    if candidate.len() > 128
        || parts.len() != 3
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
        || !candidate
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b))
    {
        return None;
    }
    Some(candidate.into())
}

async fn read_limited(
    mut stream: impl AsyncRead + Unpin,
    limit: usize,
) -> Result<String, ProbeFailure> {
    let mut bytes = Vec::new();
    (&mut stream)
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| ProbeFailure::Failed)?;
    if bytes.len() > limit {
        return Err(ProbeFailure::OutputLimit);
    }
    String::from_utf8(bytes).map_err(|_| ProbeFailure::UnrecognizedVersion)
}

struct OwnedChild {
    child: Child,
    reaped: bool,
    #[cfg(windows)]
    job: windows::Win32::Foundation::HANDLE,
}

impl OwnedChild {
    fn spawn(program: &str, args: &[&str]) -> io::Result<Self> {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = command.spawn()?;
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows::Win32::{
                Foundation::{CloseHandle, HANDLE},
                System::JobObjects::*,
            };
            let mut child = child;
            let job = match unsafe { CreateJobObjectW(None, None) } {
                Ok(job) => job,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(io::Error::other(error));
                }
            };
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let attached = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const _,
                    std::mem::size_of_val(&limits) as u32,
                )
                .and_then(|_| AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())))
            };
            if let Err(error) = attached {
                let _ = child.kill();
                let _ = child.wait();
                unsafe {
                    let _ = CloseHandle(job);
                }
                return Err(io::Error::other(error));
            }
            return Ok(Self {
                child,
                reaped: false,
                job,
            });
        }
        #[cfg(not(windows))]
        Ok(Self {
            child,
            reaped: false,
        })
    }

    fn exited(&mut self) -> io::Result<bool> {
        #[cfg(unix)]
        {
            // WNOWAIT keeps the root waitable: its PID/group cannot be reused
            // before cleanup, even when descendants still own the output pipes.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.child.id(),
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(unsafe { info.si_pid() } != 0)
        }
        #[cfg(not(unix))]
        {
            Ok(self.child.try_wait()?.is_some())
        }
    }

    fn finish(&mut self) -> io::Result<ExitStatus> {
        #[cfg(unix)]
        {
            let pid = i32::try_from(self.child.id()).map_err(io::Error::other)?;
            if pid > 1 && !self.reaped {
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
        }
        #[cfg(windows)]
        unsafe {
            let _ = windows::Win32::System::JobObjects::TerminateJobObject(self.job, 1);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = self.child.kill();
        }
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.finish();
        }
        #[cfg(windows)]
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.job);
        }
    }
}

async fn capture(
    program: &str,
    args: &[&str],
    limit: usize,
    timeout: Duration,
) -> Result<(String, String), ProbeFailure> {
    let mut owned = OwnedChild::spawn(program, args).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            ProbeFailure::NotFound
        } else {
            ProbeFailure::Failed
        }
    })?;
    let stdout = tokio::process::ChildStdout::from_std(
        owned.child.stdout.take().ok_or(ProbeFailure::Failed)?,
    )
    .map_err(|_| ProbeFailure::Failed)?;
    let stderr = tokio::process::ChildStderr::from_std(
        owned.child.stderr.take().ok_or(ProbeFailure::Failed)?,
    )
    .map_err(|_| ProbeFailure::Failed)?;
    let outcome = tokio::time::timeout(timeout, async {
        tokio::try_join!(
            read_limited(stdout, limit),
            read_limited(stderr, limit),
            async {
                loop {
                    if owned.exited().map_err(|_| ProbeFailure::Failed)? {
                        return Ok(());
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        )
    })
    .await;
    let status = owned.finish().map_err(|_| ProbeFailure::Failed)?;
    let (out, err, _) = outcome.map_err(|_| ProbeFailure::TimedOut)??;
    if !status.success() {
        return Err(ProbeFailure::Failed);
    }
    Ok((out, err))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions_keep_exact_runtime_and_build_identity() {
        assert_eq!(
            parse_version(RuntimeKind::Codex, "codex-cli 0.154.0-beta.1+abc\n"),
            Some("0.154.0-beta.1+abc".into())
        );
        assert_eq!(
            parse_version(RuntimeKind::Claude, "2.1.271 (Claude Code)"),
            Some("2.1.271".into())
        );
        assert_eq!(
            parse_version(RuntimeKind::Opencode, "1.18.30"),
            Some("1.18.30".into())
        );
        for text in [
            "0.154.0",
            "codex-cli bad",
            "codex-cli 1.2.3 extra",
            "codex-cli 1.2.3\u{1b}[m",
        ] {
            assert!(
                parse_version(RuntimeKind::Codex, text).is_none(),
                "{text:?}"
            );
        }
    }

    #[cfg(unix)]
    fn script(body: &str) -> (tempfile::TempDir, String) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("probe with spaces");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        (dir, path.to_string_lossy().into())
    }

    #[test]
    #[cfg(unix)]
    fn local_probe_bounds_time_output_and_rejects_failed_commands() {
        for (body, want) in [
            ("printf 'codex-cli 0.154.0\\n'", Ok("0.154.0".into())),
            ("printf 'codex-cli 0.154.0\\n' >&2", Ok("0.154.0".into())),
            (
                "printf 'codex-cli 0.154.0\\n'; exit 1",
                Err(ProbeFailure::Failed),
            ),
            ("sleep 30", Err(ProbeFailure::TimedOut)),
            (
                "sleep 30 & printf 'codex-cli 0.154.0\\n'",
                Err(ProbeFailure::TimedOut),
            ),
            (
                "while :; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; done",
                Err(ProbeFailure::OutputLimit),
            ),
            (
                "printf 'not a version'",
                Err(ProbeFailure::UnrecognizedVersion),
            ),
        ] {
            let (_dir, path) = script(body);
            let start = std::time::Instant::now();
            assert_eq!(
                inspect(&path, RuntimeKind::Codex, Duration::from_secs(2)),
                want,
                "{body}"
            );
            assert!(start.elapsed() < Duration::from_secs(4), "{body}");
        }
        assert_eq!(
            version("/nonexistent/iyagi-cli", RuntimeKind::Codex),
            Err(ProbeFailure::NotFound)
        );
    }

    #[test]
    #[cfg(unix)]
    fn replacing_a_same_version_entrypoint_is_rejected_before_version_execution() {
        let (_dir, path) = script("printf 'codex-cli 0.154.0\\n'");
        let first = observe(&path, RuntimeKind::Codex).unwrap();
        std::fs::write(
            &path,
            "#!/bin/sh\ntouch \"$0.executed\"; printf 'codex-cli 0.154.0\\n'\n",
        )
        .unwrap();
        assert_eq!(
            observe_expected(&path, RuntimeKind::Codex, first.executable.as_ref()),
            Err(ProbeFailure::Changed)
        );
        assert!(!std::path::Path::new(&format!("{path}.executed")).exists());
        // A version-string-only check would accept this replacement.
        assert_eq!(version(&path, RuntimeKind::Codex).unwrap(), first.version);
    }

    #[test]
    #[cfg(unix)]
    fn a_file_changed_during_its_version_command_never_produces_a_verified_observation() {
        let (_dir, path) = script(
            "printf '#!/bin/sh\\nprintf changed\\n' > \"$0\"; printf 'codex-cli 0.154.0\\n'",
        );
        assert_eq!(
            observe(&path, RuntimeKind::Codex),
            Err(ProbeFailure::Changed)
        );
    }

    #[test]
    #[cfg(unix)]
    fn timeout_terminates_the_owned_descendant_before_returning() {
        let (_dir, path) =
            script("sleep 30 & printf '%s' \"$!\" > \"$0.child\"; printf 'codex-cli 0.154.0\\n'");
        assert_eq!(
            inspect(&path, RuntimeKind::Codex, Duration::from_secs(2)),
            Err(ProbeFailure::TimedOut)
        );
        let pid = std::fs::read_to_string(format!("{path}.child"))
            .unwrap()
            .parse::<u32>()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while term_platform::process_identity(pid).is_some() && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            term_platform::process_identity(pid).is_none(),
            "version-probe descendant is still alive"
        );
    }
}

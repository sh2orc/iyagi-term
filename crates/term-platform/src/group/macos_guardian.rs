//! A small independent process preserves macOS observed-tree ownership across
//! daemon restarts. It provides observed_tree coverage, never kernel containment.
//! Every connection checks the kernel peer PID/UID and the guardian's birth
//! identity. Endpoint disappearance or guardian death cannot prove target exit.
use super::{
    macos_tree::{MacGroupInner, MacosTreePlatform},
    GroupHandle, GroupInner, ResourcePlatform, StopPhase,
};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use term_contracts::{
    ids::{ProcessIdentity, WorkloadId},
    metrics::WorkloadUsage,
    rpc::{decode_frame, encode_frame},
    workload::{GroupKind, GroupRecoveryIdentity},
};

const VERSION: u32 = 1;
const CALL_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct GuardianInner {
    identity: ProcessIdentity,
    endpoint: String,
    finished: Arc<AtomicBool>,
    root: Arc<Mutex<Option<ProcessIdentity>>>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Inspect,
    Attach { identity: ProcessIdentity },
    Members,
    Sample { now_ms: u64 },
    Stop { force: bool },
    Retire,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    version: u32,
    workload_id: WorkloadId,
    guardian: ProcessIdentity,
    root: Option<ProcessIdentity>,
    attached: bool,
    empty: bool,
    members: Vec<ProcessIdentity>,
    usage: Option<WorkloadUsage>,
    error: Option<String>,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn private_parent(endpoint: &Path) -> io::Result<()> {
    if !endpoint.is_absolute() || endpoint.as_os_str().as_encoded_bytes().len() >= 104 {
        return Err(invalid(
            "guardian endpoint must be an absolute short Unix socket path",
        ));
    }
    let parent = endpoint
        .parent()
        .ok_or_else(|| invalid("guardian endpoint has no parent"))?;
    let metadata = fs::symlink_metadata(parent)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "guardian directory must be private and owned by this user",
        ));
    }
    Ok(())
}

fn peer(stream: &UnixStream) -> io::Result<u32> {
    let (mut uid, mut gid) = (0, 0);
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if uid != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "guardian peer has a different uid",
        ));
    }
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of_val(&pid) as libc::socklen_t;
    // LOCAL_PEERPID is the kernel-recorded process that bound/connected the
    // socket; a matching JSON pid alone is not authentication.
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut pid as *mut libc::pid_t).cast(),
            &mut len,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if len as usize != std::mem::size_of_val(&pid) || pid <= 0 {
        return Err(invalid("guardian peer pid unavailable"));
    }
    Ok(pid as u32)
}

/// Full-identity check that the connected peer is the guardian's original
/// parent (the daemon that spawned this guardian): the kernel-reported peer
/// pid matches and the live identity still matches the recorded birth
/// identity, so a reused parent pid cannot impersonate the daemon.
fn peer_is_parent(peer_pid: u32, parent: &ProcessIdentity) -> bool {
    peer_pid == parent.pid
        && crate::identity::process_identity(peer_pid).is_some_and(|live| live.same_process(parent))
}

/// The original parent is provably gone (its boot-scoped identity matches no
/// live process). The guardian then exists only for restart recovery by a
/// successor daemon, which by construction is not the original parent, so
/// non-Attach requests accept a same-uid peer again for that window.
fn parent_gone(parent: &ProcessIdentity) -> bool {
    crate::identity::process_identity(parent.pid).is_none_or(|live| !live.same_process(parent))
}

fn configure(stream: &UnixStream) -> io::Result<()> {
    stream.set_read_timeout(Some(CALL_TIMEOUT))?;
    stream.set_write_timeout(Some(CALL_TIMEOUT))
}

impl GuardianInner {
    fn request(&self, workload: &WorkloadId, request: Request) -> io::Result<Reply> {
        private_parent(Path::new(&self.endpoint))?;
        if !crate::identity::boot_id_reliable()
            || self.identity.boot_id != crate::identity::boot_id()
            || crate::identity::process_identity(self.identity.pid)
                .is_none_or(|p| !p.same_process(&self.identity))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "guardian process identity changed or is unavailable",
            ));
        }
        let mut stream = UnixStream::connect(&self.endpoint)?;
        configure(&stream)?;
        if peer(&stream)? != self.identity.pid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "guardian socket belongs to another process",
            ));
        }
        use std::io::Write;
        stream.write_all(
            &encode_frame(
                &serde_json::to_value(request).map_err(|_| invalid("guardian request encoding"))?,
            )
            .map_err(|_| invalid("guardian request size"))?,
        )?;
        let value = decode_frame(&mut stream)
            .map_err(|_| invalid("guardian response unavailable or malformed"))?;
        let reply: Reply =
            serde_json::from_value(value).map_err(|_| invalid("invalid guardian response"))?;
        if reply.version != VERSION
            || &reply.workload_id != workload
            || reply.guardian != self.identity
        {
            return Err(invalid(
                "guardian response does not match this owned workload",
            ));
        }
        if let Some(error) = &reply.error {
            return Err(io::Error::other(error.clone()));
        }
        *self.root.lock().unwrap_or_else(|p| p.into_inner()) = reply.root.clone();
        if reply.attached && reply.empty {
            self.finished.store(true, Ordering::Release);
        }
        Ok(reply)
    }
}

/// The daemon obtains the guardian identity directly from its spawned child,
/// then persists it with this private endpoint before target RELEASE.
pub fn connect_group(
    workload: &WorkloadId,
    endpoint: &str,
    identity: &ProcessIdentity,
) -> io::Result<GroupHandle> {
    let inner = GuardianInner {
        identity: identity.clone(),
        endpoint: endpoint.into(),
        finished: Arc::new(AtomicBool::new(false)),
        root: Arc::new(Mutex::new(None)),
    };
    inner.request(workload, Request::Inspect)?;
    Ok(GroupHandle {
        workload_id: workload.clone(),
        kind: GroupKind::ObservedTree,
        reference: endpoint.into(),
        inner: GroupInner::Guardian(inner),
    })
}

pub(super) fn recovery_identity(group: &GroupHandle) -> Option<GroupRecoveryIdentity> {
    let GroupInner::Guardian(inner) = &group.inner else {
        return None;
    };
    Some(GroupRecoveryIdentity::MacosGuardian {
        guardian: inner.identity.clone(),
        endpoint: inner.endpoint.clone(),
    })
}
pub(super) fn attach(group: &GroupHandle, identity: &ProcessIdentity) -> io::Result<()> {
    if inner(group)?.finished.load(Ordering::Acquire) {
        return Err(invalid("guardian has finished"));
    }
    inner(group)?.request(
        &group.workload_id,
        Request::Attach {
            identity: identity.clone(),
        },
    )?;
    Ok(())
}
pub(super) fn verify_root(group: &GroupHandle, root: &ProcessIdentity) -> io::Result<()> {
    let state = inner(group)?;
    if state.finished.load(Ordering::Acquire) {
        return if state
            .root
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            == Some(root)
        {
            Ok(())
        } else {
            Err(invalid("guardian tracks a different launch helper"))
        };
    }
    if inner(group)?
        .request(&group.workload_id, Request::Inspect)?
        .root
        .as_ref()
        != Some(root)
    {
        return Err(invalid("guardian tracks a different launch helper"));
    }
    Ok(())
}
pub(super) fn members(group: &GroupHandle) -> io::Result<Vec<ProcessIdentity>> {
    if inner(group)?.finished.load(Ordering::Acquire) {
        return Ok(vec![]);
    }
    Ok(inner(group)?
        .request(&group.workload_id, Request::Members)?
        .members)
}
pub(super) fn sample(group: &GroupHandle, now_ms: u64) -> io::Result<WorkloadUsage> {
    inner(group)?
        .request(&group.workload_id, Request::Sample { now_ms })?
        .usage
        .ok_or_else(|| invalid("guardian omitted usage"))
}
pub(super) fn stop(group: &GroupHandle, phase: StopPhase) -> io::Result<()> {
    if inner(group)?.finished.load(Ordering::Acquire) {
        return Ok(());
    }
    inner(group)?.request(
        &group.workload_id,
        Request::Stop {
            force: phase == StopPhase::Force,
        },
    )?;
    Ok(())
}
pub(super) fn is_empty(group: &GroupHandle) -> io::Result<bool> {
    if inner(group)?.finished.load(Ordering::Acquire) {
        return Ok(true);
    }
    let reply = inner(group)?.request(&group.workload_id, Request::Inspect)?;
    if !reply.attached {
        return Err(invalid("guardian has no attached launch helper"));
    }
    Ok(reply.empty)
}
pub(super) fn retire(group: &GroupHandle) -> io::Result<()> {
    let state = inner(group)?;
    match state.request(&group.workload_id, Request::Retire) {
        Ok(_) => {
            state.finished.store(true, Ordering::Release);
            Ok(())
        }
        Err(error) => {
            // A previous authenticated empty observation is proof. A missing
            // guardian alone is not; this also makes a lost retirement ack safe.
            if state.finished.load(Ordering::Acquire)
                && crate::identity::process_identity_checked(state.identity.pid)?
                    .is_none_or(|p| !p.same_process(&state.identity))
            {
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}
fn inner(group: &GroupHandle) -> io::Result<&GuardianInner> {
    match &group.inner {
        GroupInner::Guardian(inner) => Ok(inner),
        _ => Err(invalid("not a guardian group")),
    }
}

#[derive(Clone, Default)]
struct Observation {
    empty: bool,
    members: Vec<ProcessIdentity>,
    error: Option<String>,
}
struct Observer {
    attached: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    state: Arc<Mutex<Observation>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Observer {
    fn new(platform: Arc<MacosTreePlatform>, group: GroupHandle) -> io::Result<Self> {
        let attached = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(Observation::default()));
        let (active, done, observed) = (attached.clone(), stop.clone(), state.clone());
        let thread = std::thread::Builder::new()
            .name("owned-tree-observer".into())
            .spawn(move || {
                while !done.load(Ordering::Acquire) {
                    if active.load(Ordering::Acquire)
                        && !observed.lock().unwrap_or_else(|p| p.into_inner()).empty
                    {
                        let result = platform.member_identities(&group);
                        let mut observation = observed.lock().unwrap_or_else(|p| p.into_inner());
                        match result {
                            Ok(members) => {
                                observation.empty = members.is_empty();
                                observation.members = members;
                                observation.error = None;
                            }
                            Err(error) => {
                                observation.empty = false;
                                observation.error = Some(error.to_string());
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            })?;
        Ok(Self {
            attached,
            stop,
            state,
            thread: Some(thread),
        })
    }
    fn snapshot(&self) -> Observation {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct SocketCleanup {
    path: std::path::PathBuf,
    device: u64,
    inode: u64,
}
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.dev() == self.device && m.ino() == self.inode)
        {
            let _ = fs::remove_file(&self.path);
            if let Some(parent) = self.path.parent() {
                let _ = fs::remove_dir(parent);
            }
        }
    }
}

/// Entrypoint for the independent `--exec-guardian` helper. No provider
/// credentials, stdout, protocol result or database are read here.
pub fn serve(endpoint: &str, workload: &WorkloadId) -> io::Result<()> {
    use std::io::Write;
    private_parent(Path::new(endpoint))?;
    let listener = UnixListener::bind(endpoint)?; // Never unlink/adopt an existing socket.
    let metadata = fs::symlink_metadata(endpoint)?;
    let _cleanup = SocketCleanup {
        path: endpoint.into(),
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    fs::set_permissions(endpoint, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let guardian = crate::identity::current_process_identity()
        .ok_or_else(|| invalid("guardian identity unavailable"))?;
    let platform = Arc::new(MacosTreePlatform::new());
    let group = GroupHandle {
        workload_id: workload.clone(),
        kind: GroupKind::ObservedTree,
        reference: format!("observed-tree:{workload}"),
        inner: GroupInner::Tree(MacGroupInner::default()),
    };
    let parent = crate::identity::process_identity(unsafe { libc::getppid() } as u32)
        .ok_or_else(|| invalid("guardian parent identity unavailable"))?;
    let observer = Observer::new(platform.clone(), group.clone())?;
    let mut root: Option<ProcessIdentity> = None;
    let started = Instant::now();
    loop {
        if root.is_none() && started.elapsed() > Duration::from_secs(30) {
            return Ok(());
        }
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(error) => return Err(error),
        };
        if configure(&stream).is_err() {
            continue;
        }
        let peer_pid = match peer(&stream) {
            Ok(pid) => pid,
            Err(_) => continue,
        };
        let request: Request = match decode_frame(&mut stream)
            .ok()
            .and_then(|v| serde_json::from_value(v).ok())
        {
            Some(v) => v,
            None => continue,
        };
        // The control plane serves only the owning daemon. Attach always
        // requires the live original parent; every other request also accepts
        // a successor daemon once the original parent is provably gone
        // (restart recovery). Any other same-uid peer — e.g. a compromised
        // agent CLI — is refused: no cross-workload Stop/Retire or member
        // disclosure through the daemon's own supervision channel.
        let caller_allowed = peer_is_parent(peer_pid, &parent)
            || (!matches!(request, Request::Attach { .. }) && parent_gone(&parent));
        let observation = observer.snapshot();
        let empty = observation.empty;
        let mut reply = Reply {
            version: VERSION,
            workload_id: workload.clone(),
            guardian: guardian.clone(),
            root: root.clone(),
            attached: root.is_some(),
            empty,
            members: vec![],
            usage: None,
            error: None,
        };
        let mut retiring = false;
        let mut denied = false;
        let result = (|| -> io::Result<()> {
            if !caller_allowed {
                denied = true;
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    if matches!(request, Request::Attach { .. }) {
                        "only the original daemon may attach a helper"
                    } else {
                        "guardian control plane serves only the owning daemon"
                    },
                ));
            }
            match request {
                Request::Attach { identity } => {
                    if root.as_ref().is_some_and(|r| r != &identity) || empty {
                        return Err(invalid("guardian ownership cannot be replaced"));
                    }
                    if root.is_none() {
                        platform.attach_waiting_helper(&group, &identity)?;
                        root = Some(identity);
                        observer.attached.store(true, Ordering::Release);
                    }
                    reply.attached = true;
                    reply.root = root.clone();
                }
                Request::Inspect => {
                    if let Some(error) = &observation.error {
                        return Err(io::Error::other(error.clone()));
                    }
                }
                Request::Members => {
                    if let Some(error) = &observation.error {
                        return Err(io::Error::other(error.clone()));
                    }
                    reply.members = observation.members;
                }
                Request::Sample { now_ms } => {
                    reply.usage = Some(platform.sample_group(&group, now_ms)?);
                }
                Request::Stop { force } => {
                    if root.is_some() && !empty {
                        platform.terminate_owned(
                            &group,
                            if force {
                                StopPhase::Force
                            } else {
                                StopPhase::Grace
                            },
                        )?;
                    }
                }
                Request::Retire => {
                    if root.is_some() && !empty {
                        return Err(invalid("guardian still owns live processes"));
                    }
                    retiring = true;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            reply.error = Some(error.to_string());
            if denied {
                // A refused caller learns nothing: the envelope's state
                // fields are blanked (the legitimate client returns on the
                // error before ever reading them).
                reply.root = None;
                reply.attached = false;
                reply.empty = false;
            }
        }
        let value =
            serde_json::to_value(&reply).map_err(|_| invalid("guardian response encoding"))?;
        let frame = match encode_frame(&value) {
            Ok(frame) => frame,
            Err(_) => {
                reply.members.clear();
                reply.error = Some("guardian member response exceeds frame limit".into());
                encode_frame(&serde_json::to_value(reply).unwrap())
                    .map_err(|_| invalid("guardian error frame"))?
            }
        };
        if stream.write_all(&frame).is_ok() && retiring {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Caller-gate primitives: the check is a full-identity match on the
    /// guardian's recorded parent — a pid alone never passes, a mismatched
    /// start token never passes, and only a provably gone parent opens the
    /// successor-recovery path.
    #[test]
    fn peer_and_parent_checks_follow_identity() {
        let me = crate::identity::current_process_identity().expect("self identity");
        assert!(peer_is_parent(std::process::id(), &me));
        assert!(!peer_is_parent(std::process::id() + 1, &me));
        let stale = ProcessIdentity {
            pid: me.pid,
            start_token: format!("{}-stale", me.start_token),
            boot_id: me.boot_id.clone(),
        };
        assert!(!peer_is_parent(std::process::id(), &stale));
        assert!(parent_gone(&stale));
        assert!(!parent_gone(&me));
    }
}

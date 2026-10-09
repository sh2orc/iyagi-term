//! Data-directory layout, runtime files (endpoint/token), and the singleton
//! daemon lock (spec `02-runner.md` §1).
//!
//! Layout under the data dir:
//!
//! ```text
//! <data>/config/                     user configuration (unused in R1)
//! <data>/data/iyagi.db                SQLite database
//! <data>/data/journals/<session>.mtj session journals
//! <data>/runtime/endpoint            IPC endpoint (UDS path / named pipe)
//! <data>/runtime/token               per-start control auth token
//! <data>/runtime/daemon.lock         singleton marker + process identity
//! <data>/runtime/socket-dir          recorded fallback socket dir (Unix)
//! <data>/runtime/gate-*.sock         per-launch gate endpoints (Unix)
//! ```
//!
//! Singleton lock — ADR: instead of an OS-exclusive byte-range lock (which
//! would need libc `flock` / Win32 `LockFileEx` bindings), the lock is a
//! marker file containing the daemon's full `ProcessIdentity` (pid +
//! start_token + boot_id). The body is staged in a private temp file and
//! published with `hard_link`, which fails atomically when the marker
//! already exists — so the marker never appears with an empty or partial
//! body that a concurrent starter could mistake for a stale one. On a data
//! dir whose filesystem has no hard links (FAT/exFAT, some FUSE/SMB mounts)
//! the marker is published by exclusive create + write instead: still
//! exclusive, but a racer can briefly see an empty body, so an unparseable
//! body gets a short settle delay before it is judged stale. A second
//! daemon that fails to publish re-reads it and probes the recorded
//! identity with the same `term_platform::identity` source: same identity ⇒
//! a live daemon owns the dir (refuse to start); different/absent identity
//! ⇒ the owner died (crash), so the marker is replaced. PID alone is never
//! trusted (spec §1.2: "PID 파일만으로 singleton을 판단하지 않는다").

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use base64::Engine;
use term_contracts::ids::ProcessIdentity;
use term_platform::identity;

#[derive(Debug, Clone)]
pub struct Paths {
    root: PathBuf,
    /// Directory holding the Unix sockets (main endpoint + per-launch
    /// gates). Normally `runtime/`; a private hashed dir when `runtime/`
    /// cannot fit `sun_path` (see [`resolve_socket_dir`]). Windows: unused.
    sock_dir: PathBuf,
}

/// `sun_path` capacity including the NUL: 108 on Linux, 104 on macOS/BSD.
#[cfg(unix)]
const SUN_PATH_MAX: usize = if cfg!(target_os = "macos") { 104 } else { 108 };
/// Longest socket file name this daemon creates (`gate-<8 hex>.sock`).
#[cfg(unix)]
const LONGEST_SOCK_NAME: &str = "gate-00000000.sock";
/// Records the fallback socket dir chosen for this data dir, so every later
/// `Paths::init` (daemon restart, CLI hook) resolves the same directory
/// instead of drawing a new random name. Lives inside the 0700 `runtime/`.
#[cfg(unix)]
const SOCKET_DIR_RECORD: &str = "socket-dir";
/// Name draws for a fresh fallback dir before giving up.
#[cfg(unix)]
const FALLBACK_DIR_ATTEMPTS: usize = 8;

/// True when every socket path this daemon creates under `dir` still fits
/// `sun_path`.
#[cfg(unix)]
fn fits_sun_path(dir: &Path) -> bool {
    dir.join(LONGEST_SOCK_NAME).as_os_str().len() < SUN_PATH_MAX
}

/// Where the Unix sockets live (02-runner §1: "OS 길이 제한을 넘으면 사용자
/// runtime 디렉터리 아래 해시 이름"). `runtime/` when every socket path fits
/// `sun_path`; otherwise a private dir under `$XDG_RUNTIME_DIR`, or `/tmp`
/// as the last resort — a deep or non-ASCII `--data-dir`
/// (`/home/홍길동/Nextcloud/dev/…`) would otherwise make `bind` fail.
///
/// The fallback leaf is `iyagi-<hash>-<random>`, never the fixed
/// `iyagi-<hash>` alone: that name is predictable, so another local
/// user could pre-create it in `/tmp`, let `create_dir_all` succeed, and
/// turn the follow-up chmod into a permanent EPERM at startup. Creation is
/// therefore always a fresh non-recursive mkdir of an unpredictable 0700
/// leaf, and a previously recorded dir is reused only after re-checking
/// that it is a real directory owned by this uid — a foreign or symlinked
/// path is never chmod'd, never filled with our sockets.
#[cfg(unix)]
fn resolve_socket_dir(
    runtime_dir: &Path,
    root_hash: &str,
    xdg_runtime: Option<&Path>,
) -> std::io::Result<PathBuf> {
    if fits_sun_path(runtime_dir) {
        return Ok(runtime_dir.to_path_buf());
    }
    if let Some(dir) = recorded_socket_dir(runtime_dir) {
        tracing::info!(dir = %dir.display(), "reusing recorded fallback socket dir");
        return Ok(dir);
    }
    let base = fallback_base(root_hash, xdg_runtime);
    // The base itself is not ours to tighten (`/tmp`, the user's runtime
    // dir); only the leaf below is.
    fs::create_dir_all(&base)?;
    for _ in 0..FALLBACK_DIR_ATTEMPTS {
        let candidate = base.join(format!("iyagi-{root_hash}-{:08x}", rand::random::<u32>()));
        // Non-recursive mkdir: success means this process created the leaf,
        // so it is ours by construction (no ownership re-check needed).
        match create_private_dir(&candidate) {
            Ok(()) => {
                atomic_write(
                    &runtime_dir.join(SOCKET_DIR_RECORD),
                    candidate.to_string_lossy().as_bytes(),
                )?;
                tracing::info!(
                    dir = %candidate.display(),
                    "runtime path too long for sun_path; sockets in private fallback dir"
                );
                return Ok(candidate);
            }
            // Lost a name draw (collision / someone watching the base):
            // redraw rather than touch the existing path.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other(
        "fallback socket dir name collided too many times",
    ))
}

/// Base dir for a fresh fallback: `$XDG_RUNTIME_DIR` when the (fixed-length)
/// unique leaf still fits `sun_path` there, else `/tmp`.
#[cfg(unix)]
fn fallback_base(root_hash: &str, xdg_runtime: Option<&Path>) -> PathBuf {
    // Same length as the drawn names (`iyagi-<16 hex>-<8 hex>`).
    let probe = format!("iyagi-{root_hash}-00000000");
    if let Some(xdg) = xdg_runtime {
        if fits_sun_path(&xdg.join(&probe)) {
            return xdg.to_path_buf();
        }
    }
    PathBuf::from("/tmp")
}

/// The dir recorded in `runtime/<SOCKET_DIR_RECORD>`, if it is still safe to
/// use: absolute, a real directory (`symlink_metadata` reports links as
/// non-directories, so a symlinked record is rejected), owned by the same
/// uid as `runtime/`, and short enough for `sun_path`. Anything else is
/// ignored — the caller draws a fresh private dir instead.
#[cfg(unix)]
fn recorded_socket_dir(runtime_dir: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let raw = fs::read_to_string(runtime_dir.join(SOCKET_DIR_RECORD)).ok()?;
    let path = PathBuf::from(raw.trim());
    if !path.is_absolute() {
        return None;
    }
    let meta = fs::symlink_metadata(&path).ok()?;
    if !meta.file_type().is_dir() {
        return None;
    }
    let runtime_uid = fs::metadata(runtime_dir).ok()?.uid();
    if meta.uid() != runtime_uid || !fits_sun_path(&path) {
        return None;
    }
    Some(path)
}

/// mkdir a leaf with 0700 at creation time (the mode is applied by the
/// mkdir itself, so the dir is never briefly group/world-traversable), then
/// affirm 0700 explicitly so a restrictive umask cannot leave it closed.
#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    fs::DirBuilder::new().mode(0o700).create(dir)?;
    let mut perms = fs::metadata(dir)?.permissions();
    perms.set_mode(0o700);
    fs::set_permissions(dir, perms)
}

/// 16 hex chars of sha256(canonical root): the data-dir identity used in
/// pipe and socket-dir names.
fn root_hash(root: &Path) -> String {
    use sha2::{Digest, Sha256};
    let canonical = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    hex_prefix(&hasher.finalize(), 16)
}

impl Paths {
    /// Create the directory tree (idempotent) and tighten permissions where
    /// the platform supports it (Unix: 0700 runtime, 0700 data/config).
    pub fn init(root: impl Into<PathBuf>) -> std::io::Result<Paths> {
        let root = root.into();
        for sub in [
            "config",
            "data",
            "data/journals",
            "runtime",
            // O1 mission assets (04 §1): workspaces and artifact bodies live
            // under daemon ownership, ids only — never user text.
            "data/missions",
            "data/missions/artifacts",
        ] {
            fs::create_dir_all(root.join(sub))?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for sub in ["config", "data", "runtime"] {
                let meta = fs::metadata(root.join(sub))?;
                let mut perms = meta.permissions();
                perms.set_mode(0o700);
                fs::set_permissions(root.join(sub), perms)?;
            }
        }
        let runtime_dir = root.join("runtime");
        #[cfg(unix)]
        let sock_dir = {
            let xdg = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
            resolve_socket_dir(&runtime_dir, &root_hash(&root), xdg.as_deref())?
        };
        #[cfg(not(unix))]
        let sock_dir = runtime_dir;
        Ok(Paths { root, sock_dir })
    }

    /// Directory holding the Unix sockets (see [`resolve_socket_dir`]).
    pub fn socket_dir(&self) -> &Path {
        &self.sock_dir
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn db(&self) -> PathBuf {
        self.root.join("data/iyagi.db")
    }
    /// O1 mission asset root (workspaces + artifacts).
    pub fn missions_dir(&self) -> PathBuf {
        self.root.join("data").join("missions")
    }

    pub fn journals_dir(&self) -> PathBuf {
        self.root.join("data/journals")
    }
    pub fn journal(&self, session_id: &str) -> PathBuf {
        // Session ids are UUID v4 (validated by contracts), so they are safe
        // path segments.
        self.root
            .join("data/journals")
            .join(format!("{session_id}.mtj"))
    }
    pub fn runtime_dir(&self) -> PathBuf {
        self.root.join("runtime")
    }
    pub fn endpoint_file(&self) -> PathBuf {
        self.root.join("runtime/endpoint")
    }
    pub fn token_file(&self) -> PathBuf {
        self.root.join("runtime/token")
    }
    pub fn lock_file(&self) -> PathBuf {
        self.root.join("runtime/daemon.lock")
    }

    /// Per-launch gate endpoint. Unix: a UDS path under runtime/; Windows: a
    /// named pipe (per-launch random suffix, no filesystem artifact).
    pub fn gate_endpoint(&self, token: &str) -> String {
        #[cfg(unix)]
        {
            self.sock_dir
                .join(format!("gate-{}.sock", &token[..8.min(token.len())]))
                .to_string_lossy()
                .into_owned()
        }
        #[cfg(windows)]
        {
            format!(r"\\.\pipe\iyagi-gate-{}", &token[..16.min(token.len())])
        }
    }

    /// Main IPC endpoint for this data dir. Unix: UDS in runtime/ (dir is
    /// 0700, satisfying the user-private ACL). Windows: a named pipe whose
    /// name embeds a hash of the data dir so each daemon instance (and each
    /// hermetic test data dir) owns a unique pipe.
    pub fn main_endpoint(&self) -> String {
        #[cfg(unix)]
        {
            self.sock_dir
                .join("iyagi.sock")
                .to_string_lossy()
                .into_owned()
        }
        #[cfg(windows)]
        {
            format!(r"\\.\pipe\iyagi-{}", root_hash(&self.root))
        }
    }

    pub fn write_endpoint(&self, endpoint: &str) -> std::io::Result<()> {
        atomic_write(&self.endpoint_file(), endpoint.as_bytes())
    }

    /// (Re)generate the per-start control token: 32 random bytes, base64.
    /// Regenerated on every daemon start so stale tokens never outlive their
    /// daemon. 0600 on Unix — already 0600 when the temp file is created
    /// (see [`atomic_write`]); the post-rename chmod is a cheap assertion.
    pub fn write_token(&self) -> std::io::Result<String> {
        let bytes: [u8; 32] = rand::random();
        let token = base64::engine::general_purpose::STANDARD.encode(bytes);
        atomic_write(&self.token_file(), token.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(self.token_file())?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(self.token_file(), perms)?;
        }
        Ok(token)
    }

    pub fn read_token(&self) -> String {
        fs::read_to_string(self.token_file())
            .unwrap_or_default()
            .trim()
            .to_string()
    }
}

fn hex_prefix(digest: &[u8], len: usize) -> String {
    digest
        .iter()
        .take(len)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Create `path` exclusively for writing (`AlreadyExists` when the name is
/// taken), with 0600 from the first inode on Unix.
fn create_private_new(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Create (or replace) `path` for writing with 0600 from the first inode
/// on Unix, so the contents are never briefly group/world-readable — the
/// callers stage secrets (control token, lock identity) through here. A
/// leftover file at `path` (a temp from a daemon that crashed before its
/// rename, possibly created 0644 by an older build) is unlinked, not
/// truncated: the create mode only applies to a new inode, so reusing the
/// old one would keep its permissions and any descriptor already open on it.
fn open_private_writer(path: &Path) -> std::io::Result<fs::File> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    create_private_new(path)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = open_private_writer(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

/// Result of the singleton-lock acquisition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SingletonOutcome {
    Acquired,
    HeldByLiveDaemon,
}

/// Claim/probe rounds before giving up (each round is a publish attempt, two
/// reads and an identity probe; the only racers are our own starters).
const LOCK_ATTEMPTS: usize = 8;

/// How long an unparseable lock body may settle before it is judged stale.
/// Only the exclusive-create fallback ever shows a live owner's lock with an
/// empty body (between its create and its write); with hard links this is a
/// one-off delay when reclaiming a legacy empty marker.
const LOCK_BODY_SETTLE: std::time::Duration = std::time::Duration::from_millis(50);

/// Acquire the singleton lock for this data dir (see module ADR).
///
/// The body is staged in a private temp file and published with `hard_link`,
/// which fails atomically with `AlreadyExists` when the lock is taken: the
/// lock therefore never exists with an empty or partial body, so a
/// concurrent starter always has a full identity to probe instead of
/// mistaking a just-created lock for a stale one and unlinking it. Both
/// names live in `runtime/`, so the link never crosses a filesystem.
///
/// A filesystem without hard links would otherwise make the daemon unable to
/// start at all, so there [`claim_lock`] falls back to exclusive create +
/// write (see [`publish_by_create`] for the reduced atomicity).
pub fn acquire_singleton_lock(paths: &Paths) -> std::io::Result<SingletonOutcome> {
    let lock = paths.lock_file();
    let identity = identity::current_process_identity().ok_or_else(|| {
        std::io::Error::other("cannot determine own process identity for the lock file")
    })?;
    let body = serde_json::to_string(&identity)
        .map_err(|e| std::io::Error::other(format!("lock serialization failed: {e}")))?;
    let tmp = lock.with_file_name(format!(
        "daemon.lock.{}.{:08x}.tmp",
        std::process::id(),
        rand::random::<u32>()
    ));
    {
        let mut f = open_private_writer(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    let outcome = claim_lock(&lock, &tmp, &body, publish_by_link);
    // On success the link already holds the inode; this only drops the
    // staging name (best effort — a leftover temp is inert).
    let _ = fs::remove_file(&tmp);
    outcome
}

/// One acquisition round: atomically claim the lock, else probe the recorded
/// owner and clear it only when it is provably stale. `link` publishes the
/// staged `tmp` as `lock` (`publish_by_link`; injected so tests can model a
/// filesystem without hard links, where `body` is published by create).
fn claim_lock(
    lock: &Path,
    tmp: &Path,
    body: &str,
    link: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<SingletonOutcome> {
    let mut use_link = true;
    for _ in 0..LOCK_ATTEMPTS {
        let published = if use_link {
            match link(tmp, lock) {
                Err(e) if link_unsupported(&e) => {
                    tracing::warn!(
                        error = %e,
                        "no hard links on the data dir filesystem; lock uses exclusive create"
                    );
                    use_link = false;
                    publish_by_create(lock, body)
                }
                other => other,
            }
        } else {
            publish_by_create(lock, body)
        };
        match published {
            Ok(()) => return Ok(SingletonOutcome::Acquired),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        // Owner alive? Probe the recorded identity (pid + start_token +
        // boot_id must all match — a recycled PID differs in start_token).
        let raw = fs::read_to_string(lock).unwrap_or_default();
        let stored: Option<ProcessIdentity> = serde_json::from_str(raw.trim()).ok();
        let parsed = stored.is_some();
        let live = stored.is_some_and(|stored| {
            identity::process_identity(stored.pid)
                .is_some_and(|current| current.same_process(&stored))
        });
        if live {
            return Ok(SingletonOutcome::HeldByLiveDaemon);
        }
        // An unparseable body may belong to a fallback publisher caught
        // between its create and its write: let it settle before the
        // compare below decides the marker is stale.
        if !parsed {
            std::thread::sleep(LOCK_BODY_SETTLE);
        }
        // Stale (dead owner, or an unparseable legacy body). Unlink only if
        // the file still holds exactly what we probed, so a lock that a
        // concurrent starter legitimately published in the meantime
        // survives; then retry the atomic claim.
        if fs::read_to_string(lock).unwrap_or_default() == raw {
            let _ = fs::remove_file(lock);
        }
    }
    Err(std::io::Error::other(
        "singleton lock contention did not resolve",
    ))
}

/// Atomic publish: the staged, complete body becomes the lock in one step,
/// or fails with `AlreadyExists` when the lock is taken.
fn publish_by_link(tmp: &Path, lock: &Path) -> std::io::Result<()> {
    fs::hard_link(tmp, lock)
}

/// Fallback publish for a filesystem without hard links: exclusive create,
/// then write the body. Exclusivity still holds (`AlreadyExists` when taken),
/// but atomicity is reduced — the marker is visible with an empty body until
/// the write lands, so a racing starter could judge it stale; `claim_lock`
/// narrows that window with `LOCK_BODY_SETTLE`. A marker left empty by a
/// failed write has no identity, so the next starter reclaims it.
fn publish_by_create(lock: &Path, body: &str) -> std::io::Result<()> {
    let mut f = create_private_new(lock)?;
    f.write_all(body.as_bytes())?;
    f.sync_all()
}

/// Whether a `hard_link` failure means this filesystem cannot publish by
/// link at all, as opposed to the lock being taken or a real I/O fault.
/// FAT/exFAT report EPERM on Linux and ENOTSUP on macOS, FUSE mounts without
/// a link op ENOSYS (`Unsupported`), Windows ERROR_ACCESS_DENIED,
/// ERROR_INVALID_FUNCTION or ERROR_NOT_SUPPORTED. A genuine permission
/// problem on `runtime/` still surfaces: the fallback create fails with it.
fn link_unsupported(error: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    let kind = error.kind();
    if kind == ErrorKind::Unsupported || kind == ErrorKind::PermissionDenied {
        return true;
    }
    #[cfg(unix)]
    let unsupported = [libc::ENOTSUP, libc::EOPNOTSUPP];
    // ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED.
    #[cfg(not(unix))]
    let unsupported = [1, 50];
    error
        .raw_os_error()
        .is_some_and(|code| unsupported.contains(&code))
}

/// Default data dir: platform local-data dir + `Iyagi`.
pub fn default_data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_DATA_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("Iyagi")
}

/// Overrides applied from `IYAGI_TEST_CONFIG` (a JSON file path); see
/// `config::DaemonConfig::load`.
pub fn config_file_used() -> Option<PathBuf> {
    std::env::var_os("IYAGI_TEST_CONFIG").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_creates_tree_and_unique_gate_endpoints() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::init(dir.path()).expect("init");
        assert!(paths.journals_dir().is_dir());
        assert!(paths.runtime_dir().is_dir());
        let a = paths.gate_endpoint("abcdef1234567890");
        let b = paths.gate_endpoint("zzzzzz1234567890");
        assert_ne!(a, b);
        let main = paths.main_endpoint();
        assert!(!main.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn fallback_base_prefers_xdg_when_the_unique_leaf_fits() {
        let hash = "0123456789abcdef";
        assert_eq!(
            fallback_base(hash, Some(Path::new("/run/user/1000"))),
            PathBuf::from("/run/user/1000")
        );
        // An absurdly long XDG_RUNTIME_DIR is skipped too.
        let long_xdg = PathBuf::from(format!("/run/{}", "x".repeat(120)));
        assert_eq!(fallback_base(hash, Some(&long_xdg)), PathBuf::from("/tmp"));
        assert_eq!(fallback_base(hash, None), PathBuf::from("/tmp"));
    }

    #[cfg(unix)]
    #[test]
    fn fallback_socket_dir_is_private_recorded_and_recreated_when_unusable() {
        let base = tempfile::tempdir().expect("tempdir");
        // A runtime dir too long for sun_path forces the fallback path.
        let runtime = base.path().join("d".repeat(60)).join("r".repeat(60));
        std::fs::create_dir_all(&runtime).expect("mkdir runtime");
        // The XDG candidate must itself fit sun_path once the unique leaf
        // and socket name are appended: a tempfile dir is ~60 bytes on
        // darwin and loses the fit probe, which would move the draw to /tmp
        // and break the starts_with assertions below. A short absolute base
        // keeps the XDG branch deterministic; it is removed at the end.
        let xdg = PathBuf::from("/tmp").join(format!("iyagi-t-{:08x}", rand::random::<u32>()));
        std::fs::create_dir_all(&xdg).expect("mkdir xdg base");
        let first = resolve_socket_dir(&runtime, "0123456789abcdef", Some(&xdg)).expect("resolve");
        assert!(first.starts_with(&xdg));
        assert!(fits_sun_path(&first));
        // The choice is recorded, so the next resolver (hook / daemon
        // restart) picks the same dir…
        assert_eq!(
            std::fs::read_to_string(runtime.join(SOCKET_DIR_RECORD)).unwrap(),
            first.to_string_lossy().into_owned()
        );
        assert_eq!(
            resolve_socket_dir(&runtime, "0123456789abcdef", Some(&xdg)).expect("resolve again"),
            first
        );
        // …and it stays 0700.
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&first).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        // A record pointing at a symlink is rejected: a fresh dir is drawn
        // instead of following the link.
        std::os::unix::fs::symlink(base.path(), base.path().join("loop")).expect("symlink");
        std::fs::write(
            runtime.join(SOCKET_DIR_RECORD),
            base.path().join("loop").to_string_lossy().as_bytes(),
        )
        .expect("write record");
        let second = resolve_socket_dir(&runtime, "0123456789abcdef", Some(&xdg))
            .expect("resolve after bad record");
        assert_ne!(second, first);
        assert!(second.starts_with(&xdg));
        // The drawn leaves live outside `base`; remove them with their base.
        let _ = std::fs::remove_dir_all(&xdg);
    }

    #[cfg(unix)]
    #[test]
    fn init_publishes_bindable_endpoints_for_a_deep_data_dir() {
        let base = tempfile::tempdir().expect("tempdir");
        let deep = base.path().join("d".repeat(60)).join("e".repeat(60));
        let paths = Paths::init(&deep).expect("init");
        for endpoint in [
            paths.main_endpoint(),
            paths.gate_endpoint("abcdef1234567890"),
        ] {
            assert!(
                endpoint.len() < SUN_PATH_MAX,
                "{endpoint} does not fit sun_path"
            );
            assert!(endpoint.starts_with(&paths.socket_dir().to_string_lossy().into_owned()));
        }
        assert_ne!(paths.socket_dir(), paths.runtime_dir());
        // A second init (hook / restart) resolves the recorded dir, not a
        // fresh one.
        let again = Paths::init(&deep).expect("init again");
        assert_eq!(again.socket_dir(), paths.socket_dir());
        // And it really binds.
        let listener = std::os::unix::net::UnixListener::bind(paths.main_endpoint()).expect("bind");
        drop(listener);
        let _ = std::fs::remove_dir_all(paths.socket_dir());
    }

    #[test]
    fn token_round_trips_through_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::init(dir.path()).expect("init");
        let token = paths.write_token().expect("write");
        assert_eq!(token.len(), 44, "b64 of 32 bytes");
        assert_eq!(paths.read_token(), token);
        let again = paths.write_token().expect("rewrite");
        assert_ne!(token, again, "regenerated per start");
        // 0600 — already true at temp-file creation, never briefly 0644.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(paths.token_file())
                .expect("stat token")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn singleton_lock_acquires_then_reports_live_owner_and_steals_after_death() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::init(dir.path()).expect("init");
        assert_eq!(
            acquire_singleton_lock(&paths).expect("first"),
            SingletonOutcome::Acquired
        );
        // Same live process: the identity probe matches.
        assert_eq!(
            acquire_singleton_lock(&paths).expect("second"),
            SingletonOutcome::HeldByLiveDaemon
        );
        // A stale marker (recycled pid / mismatching start token) is stolen.
        let stale = ProcessIdentity {
            pid: std::process::id(),
            start_token: "999999".into(),
            boot_id: identity::boot_id(),
        };
        std::fs::write(
            paths.lock_file(),
            serde_json::to_string(&stale).expect("serialize"),
        )
        .expect("write stale");
        assert_eq!(
            acquire_singleton_lock(&paths).expect("third"),
            SingletonOutcome::Acquired
        );
        // A legacy empty body (writer crashed before writing) is stale too.
        std::fs::write(paths.lock_file(), b"").expect("write empty");
        assert_eq!(
            acquire_singleton_lock(&paths).expect("fourth"),
            SingletonOutcome::Acquired
        );
        // The staged temp never lingers next to the published lock.
        let mut names: Vec<String> = std::fs::read_dir(paths.runtime_dir())
            .expect("read runtime")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["daemon.lock".to_owned()]);
    }

    #[test]
    fn singleton_lock_falls_back_to_exclusive_create_without_hard_links() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::init(dir.path()).expect("init");
        let lock = paths.lock_file();
        let own = identity::current_process_identity().expect("own identity");
        let body = serde_json::to_string(&own).expect("serialize");
        let tmp = paths.runtime_dir().join("daemon.lock.test.tmp");
        std::fs::write(&tmp, &body).expect("stage");
        // FAT/exFAT and some FUSE/SMB mounts refuse link(2) outright; the
        // daemon must still start there.
        let no_links = |_: &Path, _: &Path| -> std::io::Result<()> {
            Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
        };
        assert_eq!(
            claim_lock(&lock, &tmp, &body, no_links).expect("first"),
            SingletonOutcome::Acquired
        );
        assert_eq!(std::fs::read_to_string(&lock).expect("read lock"), body);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&lock)
                .expect("stat lock")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // The create is still exclusive: a live owner is detected, never
        // overwritten.
        assert_eq!(
            claim_lock(&lock, &tmp, &body, no_links).expect("second"),
            SingletonOutcome::HeldByLiveDaemon
        );
        // An empty marker (a fallback writer that died before its write) is
        // reclaimed once it has had time to settle.
        std::fs::write(&lock, b"").expect("write empty");
        assert_eq!(
            claim_lock(&lock, &tmp, &body, no_links).expect("third"),
            SingletonOutcome::Acquired
        );
        assert_eq!(std::fs::read_to_string(&lock).expect("reread"), body);
        // A real I/O fault is not mistaken for missing link support.
        std::fs::remove_file(&lock).expect("remove lock");
        let faulty = |_: &Path, _: &Path| -> std::io::Result<()> {
            Err(std::io::Error::other("injected link fault"))
        };
        assert!(claim_lock(&lock, &tmp, &body, faulty).is_err());
        assert!(!lock.exists(), "no fallback publish on a real fault");
    }

    #[cfg(unix)]
    #[test]
    fn private_writer_replaces_a_stale_world_readable_temp() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let tmp = dir.path().join("token.tmp");
        // A temp left by a crashed older daemon, created 0644.
        std::fs::write(&tmp, b"old").expect("write stale");
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let f = open_private_writer(&tmp).expect("open");
        // A fresh 0600 inode, not the stale one truncated in place.
        let meta = f.metadata().expect("stat");
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(meta.len(), 0);
    }
}

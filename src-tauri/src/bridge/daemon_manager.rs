//! iyagi-termd binary location and detached spawn lifecycle
//! (`02-runner.md` §1). The app creates the data dir, spawns the daemon
//! detached from the app's lifetime/console, then polls the daemon-written
//! runtime files (`<data>/runtime/endpoint` + `token`) until ready.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use super::connection::BridgeError;

/// Daemon ready budget (02 §1-4: ready ≤ 10s), also reused as the overall
/// connect budget in `state.rs`.
pub const READY_TIMEOUT: Duration = Duration::from_secs(10);
/// Poll cadence while waiting for the runtime files.
pub const READY_POLL: Duration = Duration::from_millis(100);

/// `DETACHED_PROCESS` — no console inherited (`02-runner.md` §1.3).
#[cfg(windows)]
pub const DETACHED_PROCESS_FLAG: u32 = 0x0000_0008;
/// `CREATE_NEW_PROCESS_GROUP` — separate Ctrl+C group from the app window.
#[cfg(windows)]
pub const CREATE_NEW_PROCESS_GROUP_FLAG: u32 = 0x0000_0200;

/// `CREATE_BREAKAWAY_FROM_JOB` — leave whatever job the *app* was started
/// in (Task Scheduler, IDE runners, some launchers use kill-on-close jobs)
/// so that job cannot take the daemon down with the window (02 §1). The
/// app itself creates no jobs, but its parent may have.
#[cfg(windows)]
pub const CREATE_BREAKAWAY_FROM_JOB_FLAG: u32 = 0x0100_0000;

/// Creation flags used for the Windows daemon: the daemon must never be tied
/// to the app's lifetime or console. Breakaway is tried on top of these in
/// `spawn_detached` and dropped on refusal — CreateProcess fails outright
/// with the flag when the parent sits in a job without BREAKAWAY_OK, so it
/// cannot be part of the unconditional set.
#[cfg(windows)]
pub fn windows_detached_flags() -> u32 {
    DETACHED_PROCESS_FLAG | CREATE_NEW_PROCESS_GROUP_FLAG
}

pub fn daemon_binary_name() -> &'static str {
    if cfg!(windows) {
        "iyagi-termd.exe"
    } else {
        "iyagi-termd"
    }
}

/// Candidate order: `IYAGI_TERMD_PATH` env → adjacent to the app exe →
/// `../target/debug` next to the exe → `target/debug` under the cwd
/// (dev fallback while the app exe still lives in the workspace target dir).
pub fn daemon_binary_candidates() -> Vec<PathBuf> {
    let name = daemon_binary_name();
    let mut candidates = Vec::new();
    if let Ok(env_path) = std::env::var("IYAGI_TERMD_PATH") {
        if !env_path.trim().is_empty() {
            candidates.push(PathBuf::from(env_path));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(name));
            candidates.push(dir.join("..").join("target").join("debug").join(name));
        }
    }
    candidates.push(Path::new("target").join("debug").join(name));
    candidates
}

pub fn locate_daemon_binary() -> Option<PathBuf> {
    daemon_binary_candidates().into_iter().find(|p| p.is_file())
}

/// `iyagi-termd --version` 조회 상한 — 느린 디스크에서도 연결을 오래 붙잡지 않는다.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// 디스크에 있는 데몬 바이너리가 보고하는 빌드 id(`iyagi-termd --version`
/// 출력의 이름 뒤 부분 — hello `daemon_version`과 같은 모양). 실행 중인
/// 데몬이 이 바이너리보다 오래됐는지(다시 빌드했지만 옛 프로세스가 아직
/// 도는 개발 함정) 판별하는 데 쓴다. 어떤 실패도 치명적이지 않다(`None`).
pub async fn binary_build_version(binary: &Path) -> Option<String> {
    let mut command = tokio::process::Command::new(binary);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    super::system::hide_console(&mut command);
    let output = tokio::time::timeout(VERSION_PROBE_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next()?.trim();
    let version = line.strip_prefix("iyagi-termd").unwrap_or(line).trim();
    (!version.is_empty()).then(|| version.to_string())
}

// ---------------------------------------------------------------- AppImage
//
// An AppImage runs the app (and its sidecar) from a transient squashfs mount
// (`/tmp/.mount_XXXX`) that disappears when the app exits. A daemon that must
// outlive the window (02 §1) cannot be started from there: its
// `/proc/self/exe` would read "(deleted)", the launch helper — resolved by
// the daemon through its own `current_exe()` — would vanish with the mount,
// and a hook command baked into `~/.claude/settings.json` would point at a
// path that changes every launch. So on Linux the daemon is copied once to a
// stable per-user location and spawned from there.
//
// The same instability exists in dev without any AppImage: a checkout path
// (`…/target/debug/iyagi-termd`) baked into hooks or the ccd/ccg script dies
// the moment the repo is moved or renamed. So every path we BAKE — hook
// commands and the shell profiles alike — points at the stable copy on all
// platforms, and launching the app refreshes the copy in place (the stamp
// makes an unchanged binary a no-op). Only the app's own SPAWN keeps using
// the located binary outside AppImages, so a dev rebuild is picked up
// without a copy hop.

/// AppImage runtime variables the launcher sets (AppRun / linuxdeploy).
fn appimage_runtime_active() -> bool {
    std::env::var_os("APPIMAGE").is_some() || std::env::var_os("APPDIR").is_some()
}

/// `/tmp/.mount_<name><rand>/…` — the AppImage runtime's mount point naming.
fn under_transient_mount(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str().to_string_lossy().starts_with(".mount_"))
}

fn needs_stable_copy(located: &Path) -> bool {
    cfg!(target_os = "linux") && (appimage_runtime_active() || under_transient_mount(located))
}

/// Stable per-user copy of the daemon: `<data_dir>/bin/iyagi-termd`.
pub fn stable_daemon_path(data_dir: &Path) -> PathBuf {
    data_dir.join("bin").join(daemon_binary_name())
}

/// Freshness key of a source binary: length + mtime + path, recorded next
/// to the copy so an unchanged source is never re-copied (and a changed one
/// — new app version — always is).
fn source_stamp(source: &Path, meta: &std::fs::Metadata) -> String {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}:{}:{}", meta.len(), mtime, source.display())
}

/// Copy `source` to the stable location unless the copy is already current;
/// dir 0700, file 0755, temp + rename so a crash never leaves a half-written
/// binary. Returns the stable path.
pub fn stable_daemon_copy(source: &Path, data_dir: &Path) -> io::Result<PathBuf> {
    let target = stable_daemon_path(data_dir);
    let stamp_path = target.with_extension("stamp");
    let meta = std::fs::metadata(source)?;
    let stamp = source_stamp(source, &meta);
    if target.is_file()
        && std::fs::read_to_string(&stamp_path)
            .map(|recorded| recorded == stamp)
            .unwrap_or(false)
    {
        return Ok(target);
    }
    let dir = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "stable path has no parent"))?;
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        daemon_binary_name(),
        std::process::id()
    ));
    std::fs::copy(source, &tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, &target)?;
    std::fs::write(&stamp_path, stamp)?;
    Ok(target)
}

/// The binary the daemon is actually spawned from: on Linux under an
/// AppImage (or any `.mount_*` path) the stable copy, else the located one.
/// A failed copy keeps the located binary — logged, never blocking the app.
pub fn effective_daemon_binary(located: PathBuf, data_dir: &Path) -> PathBuf {
    if !needs_stable_copy(&located) {
        return located;
    }
    match stable_daemon_copy(&located, data_dir) {
        Ok(stable) => stable,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "iyagi-termd: stable copy failed; spawning from the transient mount"
            );
            located
        }
    }
}

/// Binary path to bake into hook commands (`iyagi-termd hook`) and the
/// ccd/ccg shell script: the per-user stable copy, refreshed right here. A
/// baked absolute path must outlive the checkout it came from — a moved or
/// renamed repo would otherwise leave every baked path dead, and the app
/// updating would leave it stale. A failed copy falls back to the located
/// binary — logged, never blocking the install.
pub fn hook_daemon_binary() -> Option<PathBuf> {
    baked_daemon_binary(locate_daemon_binary(), default_data_dir().ok().as_deref())
}

/// Decision core of `hook_daemon_binary`, with the paths injected so tests
/// never depend on a real workspace build: make the stable copy and return
/// it; without a data dir, or when the copy fails, bake the located path.
fn baked_daemon_binary(located: Option<PathBuf>, data_dir: Option<&Path>) -> Option<PathBuf> {
    let located = located?;
    match data_dir.map(|dir| stable_daemon_copy(&located, dir)) {
        Some(Ok(stable)) => Some(stable),
        Some(Err(err)) => {
            tracing::warn!(
                error = %err,
                "iyagi-termd: stable copy for baked paths failed; baking the located path"
            );
            Some(located)
        }
        None => Some(located),
    }
}

/// Startup refresh of the stable copy. The baked paths (ccd/ccg, CLI hooks)
/// point at the copy, so a moved checkout heals just by launching the app;
/// the stamp check makes this a no-op when nothing changed.
pub fn refresh_stable_daemon_copy() {
    let _ = hook_daemon_binary();
}

/// AppImage runtime variables that must not reach the daemon — and through
/// it every PTY shell: GTK/GIO/GStreamer module paths pointing into the
/// squashfs make any GTK app started from a iyagi shell load loaders and
/// schemas from a mount that is gone once the app exits.
#[cfg(unix)]
const APPIMAGE_LEAKED_VARS: [&str; 9] = [
    "APPDIR",
    "APPIMAGE",
    "OWD",
    "ARGV0",
    "GDK_PIXBUF_MODULE_FILE",
    "GIO_MODULE_DIR",
    "GTK_PATH",
    "GTK_IM_MODULE_FILE",
    "GSETTINGS_SCHEMA_DIR",
];
#[cfg(unix)]
const DEFAULT_XDG_DATA_DIRS: &str = "/usr/local/share:/usr/share";

/// Which variables to drop (`None`) or rewrite (`Some`) before spawning the
/// daemon from inside an AppImage whose mount is `appdir`. Pure, for tests.
#[cfg(unix)]
pub fn appimage_env_overrides(
    appdir: &str,
    env: impl Iterator<Item = (String, String)>,
) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for (name, value) in env {
        if APPIMAGE_LEAKED_VARS.contains(&name.as_str())
            || name.starts_with("GST_PLUGIN_SYSTEM_PATH")
        {
            out.push((name, None));
        } else if name == "LD_LIBRARY_PATH" || name == "XDG_DATA_DIRS" {
            let all: Vec<&str> = value.split(':').filter(|p| !p.is_empty()).collect();
            let kept: Vec<&str> = all
                .iter()
                .copied()
                .filter(|p| !p.starts_with(appdir))
                .collect();
            if kept.len() == all.len() {
                continue;
            }
            let replacement = if kept.is_empty() {
                (name == "XDG_DATA_DIRS").then(|| DEFAULT_XDG_DATA_DIRS.to_string())
            } else {
                Some(kept.join(":"))
            };
            out.push((name, replacement));
        }
    }
    out
}

/// Default data dir: user local-data dir + `Iyagi` (02 §1). The `dirs`
/// crate is not a dependency, so the resolution is manual: Windows
/// `%LOCALAPPDATA%`, Unix `$XDG_DATA_HOME` else `~/.local/share`.
pub fn default_data_dir() -> Result<PathBuf, BridgeError> {
    #[cfg(windows)]
    {
        let base = std::env::var("LOCALAPPDATA")
            .map_err(|_| BridgeError::invalid_argument("LOCALAPPDATA is not set"))?;
        Ok(PathBuf::from(base).join("Iyagi"))
    }
    #[cfg(unix)]
    {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".local").join("share"))
            })
            .ok_or_else(|| {
                BridgeError::invalid_argument("neither XDG_DATA_HOME nor HOME is set")
            })?;
        Ok(base.join("Iyagi"))
    }
}

/// Resolve the effective data dir; explicit overrides must be absolute.
pub fn resolve_data_dir(override_dir: Option<&str>) -> Result<PathBuf, BridgeError> {
    match override_dir {
        Some(dir) => {
            let path = PathBuf::from(trim_trailing_separators(dir));
            if !path.is_absolute() {
                return Err(BridgeError::invalid_argument(
                    "data_dir override must be an absolute path",
                ));
            }
            Ok(path)
        }
        None => default_data_dir(),
    }
}

/// Strip trailing separators from an override, but never below a root:
/// `C:\` would otherwise become the drive-relative `C:` (rejected as
/// non-absolute) and `/` an empty path.
fn trim_trailing_separators(dir: &str) -> &str {
    let trimmed = dir.trim_end_matches(['/', '\\']);
    if trimmed.len() < dir.len() && (trimmed.is_empty() || trimmed.ends_with(':')) {
        &dir[..trimmed.len() + 1]
    } else {
        trimmed
    }
}

pub fn runtime_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("runtime")
}

/// Daemon-written endpoint string (socket path / pipe name).
pub fn endpoint_file(data_dir: &Path) -> PathBuf {
    runtime_dir(data_dir).join("endpoint")
}

/// Daemon-written auth token (0600 on the daemon side; never logged here).
pub fn token_file(data_dir: &Path) -> PathBuf {
    runtime_dir(data_dir).join("token")
}

/// Read a runtime file, trimmed; `None` when missing/empty.
pub fn read_runtime_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// How the daemon is launched. A trait so tests never spawn processes.
pub trait DaemonSpawner: Send + Sync {
    /// Spawn the daemon detached from the app lifetime/console. `Ok` does
    /// not mean the daemon kept running — the singleton lock (02 §1-2)
    /// makes a redundant instance exit immediately.
    fn spawn_detached(&self, binary: &Path, data_dir: &Path) -> io::Result<()>;
}

/// Production spawner using std::process with per-OS detachment flags.
pub struct OsDaemonSpawner;

impl DaemonSpawner for OsDaemonSpawner {
    fn spawn_detached(&self, binary: &Path, data_dir: &Path) -> io::Result<()> {
        let mut cmd = std::process::Command::new(binary);
        // Standard in/out/err detached from the app's terminal (02 §1-3);
        // the daemon owns its own log files under <data_dir>/logs.
        cmd.arg("--data-dir").arg(data_dir);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // First try to leave the parent's job (CREATE_BREAKAWAY_FROM_JOB);
            // a job that forbids breakaway refuses the spawn with
            // ERROR_ACCESS_DENIED, and then the plain detached flags are used.
            cmd.creation_flags(windows_detached_flags() | CREATE_BREAKAWAY_FROM_JOB_FLAG);
            match cmd.spawn() {
                Ok(child) => return reap_in_background(child),
                Err(err)
                    if err.kind() == io::ErrorKind::PermissionDenied
                        || err.raw_os_error() == Some(5) =>
                {
                    cmd.creation_flags(windows_detached_flags());
                }
                Err(err) => return Err(err),
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // process_group(0) = setpgid(0, 0): a fresh process group so
            // terminal signals aimed at the app never reach the daemon.
            // (std has no setsid; process_group is the stable subset.)
            cmd.process_group(0);
            // Inside an AppImage the launcher's GTK/GIO module paths point
            // into the transient mount — scrub them so shells spawned by the
            // daemon do not inherit a runtime that vanishes with the app.
            if let Some(appdir) = std::env::var("APPDIR").ok().filter(|v| !v.is_empty()) {
                let env = std::env::vars_os().filter_map(|(name, value)| {
                    Some((
                        name.into_string().ok()?,
                        value.to_string_lossy().into_owned(),
                    ))
                });
                for (name, value) in appimage_env_overrides(&appdir, env) {
                    match value {
                        Some(value) => {
                            cmd.env(&name, value);
                        }
                        None => {
                            cmd.env_remove(&name);
                        }
                    }
                }
            }
        }
        reap_in_background(cmd.spawn()?)
    }
}

/// Dropping the handle never kills the child on either OS. A parked reaper
/// thread waits on it so the daemon never lingers as a zombie after IT exits
/// (it used to, until the app itself exited).
fn reap_in_background(mut child: std::process::Child) -> io::Result<()> {
    let _ = std::thread::Builder::new()
        .name("iyagi-termd-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(())
}

/// Spawn the daemon unconditionally (singleton-safe: a redundant instance
/// exits immediately via its lock, 02 §1-2). The binary locator is injected
/// so tests never depend on a real workspace build.
pub fn spawn_daemon_with(
    locate: &dyn Fn() -> Option<PathBuf>,
    spawner: &dyn DaemonSpawner,
    data_dir: &Path,
) -> Result<(), BridgeError> {
    std::fs::create_dir_all(runtime_dir(data_dir))
        .map_err(|e| BridgeError::daemon_unavailable(format!("cannot create runtime dir: {e}")))?;
    let binary =
        locate().ok_or_else(|| BridgeError::daemon_unavailable("iyagi-termd binary not found"))?;
    // AppImage: spawn from the stable per-user copy, never from the mount.
    let binary = effective_daemon_binary(binary, data_dir);
    spawner
        .spawn_detached(&binary, data_dir)
        .map_err(|e| BridgeError::daemon_unavailable(format!("failed to spawn iyagi-termd: {e}")))
}

/// Poll until the daemon's runtime files exist or the deadline passes.
pub async fn wait_ready(data_dir: &Path, deadline: Instant) -> bool {
    loop {
        if endpoint_file(data_dir).is_file() && token_file(data_dir).is_file() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_detached_flags_combines_process_and_group_bits() {
        assert_eq!(windows_detached_flags(), 0x0000_0008 | 0x0000_0200);
        assert_eq!(windows_detached_flags(), 0x208);
        // Breakaway is layered on top for the first attempt only.
        assert_eq!(
            windows_detached_flags() | CREATE_BREAKAWAY_FROM_JOB_FLAG,
            0x0100_0208
        );
    }

    #[test]
    fn trailing_separator_trim_keeps_roots() {
        assert_eq!(trim_trailing_separators(r"C:\"), r"C:\");
        assert_eq!(trim_trailing_separators(r"C:\\"), r"C:\");
        assert_eq!(trim_trailing_separators("/"), "/");
        assert_eq!(trim_trailing_separators("D:/data/"), "D:/data");
        assert_eq!(trim_trailing_separators("/tmp/x//"), "/tmp/x");
        assert_eq!(trim_trailing_separators("/tmp/x"), "/tmp/x");
    }

    #[test]
    fn transient_mount_paths_are_recognized() {
        assert!(under_transient_mount(Path::new(
            "/tmp/.mount_IYAGIabc123/usr/bin/iyagi-termd"
        )));
        assert!(!under_transient_mount(Path::new("/usr/bin/iyagi-termd")));
        assert!(!under_transient_mount(Path::new(
            "/home/u/.local/share/Iyagi/bin/iyagi-termd"
        )));
    }

    #[test]
    fn stable_copy_is_made_once_and_refreshed_when_the_source_changes() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("mount").join("iyagi-termd");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"v1").unwrap();
        let data = dir.path().join("data");

        let stable = stable_daemon_copy(&source, &data).unwrap();
        assert_eq!(stable, stable_daemon_path(&data));
        assert_eq!(std::fs::read(&stable).unwrap(), b"v1");
        assert!(stable.with_extension("stamp").is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&stable).unwrap().permissions().mode() & 0o777,
                0o755
            );
            assert_eq!(
                std::fs::metadata(stable.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        // Unchanged source → the copy is left alone (same inode contents/mtime).
        let before = std::fs::metadata(&stable).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        stable_daemon_copy(&source, &data).unwrap();
        assert_eq!(
            std::fs::metadata(&stable).unwrap().modified().unwrap(),
            before
        );
        // New app version (different length) → refreshed atomically.
        std::fs::write(&source, b"v2-longer").unwrap();
        stable_daemon_copy(&source, &data).unwrap();
        assert_eq!(std::fs::read(&stable).unwrap(), b"v2-longer");
        assert!(!dir
            .path()
            .join("data/bin")
            .read_dir()
            .unwrap()
            .any(|e| { e.unwrap().file_name().to_string_lossy().ends_with(".tmp") }));
        // A missing source is an error, not a silent fallback.
        assert!(stable_daemon_copy(&dir.path().join("nope"), &data).is_err());
    }

    #[test]
    fn baked_binary_is_the_stable_copy_and_survives_copy_failures() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("repo").join("iyagi-termd");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"v1").unwrap();
        let data = dir.path().join("data");

        // The copy is made here and its path is what gets baked.
        let baked = baked_daemon_binary(Some(source.clone()), Some(&data)).unwrap();
        assert_eq!(baked, stable_daemon_path(&data));
        assert_eq!(std::fs::read(&baked).unwrap(), b"v1");

        // No data dir → bake the located path as-is.
        assert_eq!(
            baked_daemon_binary(Some(source.clone()), None).unwrap(),
            source
        );

        // Copy failure (source vanished after locate) → located fallback,
        // never `None`: an install must not block on the copy.
        let missing = dir.path().join("gone");
        assert_eq!(
            baked_daemon_binary(Some(missing.clone()), Some(&data)).unwrap(),
            missing
        );

        // Nothing located → nothing to bake.
        assert_eq!(baked_daemon_binary(None, Some(&data)), None);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn effective_binary_is_the_located_one_off_linux() {
        let dir = tempfile::tempdir().unwrap();
        let located = PathBuf::from("/tmp/.mount_IYAGIabc123/usr/bin/iyagi-termd");
        assert_eq!(
            effective_daemon_binary(located.clone(), dir.path()),
            located
        );
    }

    #[cfg(unix)]
    #[test]
    fn appimage_env_overrides_drop_runtime_vars_and_filter_mount_paths() {
        let appdir = "/tmp/.mount_IYAGIabc123";
        let env = vec![
            ("APPDIR".to_string(), appdir.to_string()),
            ("APPIMAGE".to_string(), "/home/u/IYAGI.AppImage".to_string()),
            (
                "GDK_PIXBUF_MODULE_FILE".to_string(),
                format!("{appdir}/usr/lib/loaders.cache"),
            ),
            (
                "GST_PLUGIN_SYSTEM_PATH_1_0".to_string(),
                format!("{appdir}/usr/lib/gst"),
            ),
            (
                "LD_LIBRARY_PATH".to_string(),
                format!("{appdir}/usr/lib:/opt/cuda/lib"),
            ),
            (
                "XDG_DATA_DIRS".to_string(),
                format!("{appdir}/usr/share:/usr/share"),
            ),
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("HOME".to_string(), "/home/u".to_string()),
        ];
        let overrides = appimage_env_overrides(appdir, env.into_iter());
        let get = |name: &str| {
            overrides
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get("APPDIR"), Some(None));
        assert_eq!(get("APPIMAGE"), Some(None));
        assert_eq!(get("GDK_PIXBUF_MODULE_FILE"), Some(None));
        assert_eq!(get("GST_PLUGIN_SYSTEM_PATH_1_0"), Some(None));
        assert_eq!(
            get("LD_LIBRARY_PATH"),
            Some(Some("/opt/cuda/lib".to_string()))
        );
        assert_eq!(get("XDG_DATA_DIRS"), Some(Some("/usr/share".to_string())));
        // Untouched variables are not even listed.
        assert_eq!(get("PATH"), None);
        assert_eq!(get("HOME"), None);
        // Only-mount values: LD_LIBRARY_PATH goes away, XDG_DATA_DIRS gets the system default.
        let only = vec![
            ("LD_LIBRARY_PATH".to_string(), format!("{appdir}/usr/lib")),
            ("XDG_DATA_DIRS".to_string(), format!("{appdir}/usr/share")),
        ];
        let overrides = appimage_env_overrides(appdir, only.into_iter());
        assert_eq!(overrides[0], ("LD_LIBRARY_PATH".to_string(), None));
        assert_eq!(
            overrides[1],
            (
                "XDG_DATA_DIRS".to_string(),
                Some(DEFAULT_XDG_DATA_DIRS.to_string())
            )
        );
    }

    #[test]
    fn detached_spawner_refuses_missing_binary() {
        let dir = tempfile::tempdir().unwrap();
        let err = OsDaemonSpawner
            .spawn_detached(Path::new("/definitely/not/iyagi-termd"), dir.path())
            .unwrap_err();
        assert!(err.kind() == io::ErrorKind::NotFound || err.raw_os_error().is_some());
    }

    #[test]
    fn data_dir_override_must_be_absolute() {
        assert!(resolve_data_dir(Some("relative/dir")).is_err());
        let dir = resolve_data_dir(Some(if cfg!(windows) {
            "D:/data/Iyagi"
        } else {
            "/tmp/Iyagi"
        }))
        .unwrap();
        assert!(dir.is_absolute());
        assert!(dir.ends_with("Iyagi"));
        // A drive root stays a root instead of collapsing to `C:`.
        let root = if cfg!(windows) { r"C:\" } else { "/" };
        assert_eq!(resolve_data_dir(Some(root)).unwrap(), Path::new(root));
    }

    #[test]
    fn runtime_files_live_under_runtime_dir() {
        let data = Path::new("/data");
        assert_eq!(endpoint_file(data), Path::new("/data/runtime/endpoint"));
        assert_eq!(token_file(data), Path::new("/data/runtime/token"));
    }

    struct RecordingSpawner {
        calls: std::sync::Mutex<Vec<PathBuf>>,
    }

    impl DaemonSpawner for RecordingSpawner {
        fn spawn_detached(&self, binary: &Path, data_dir: &Path) -> io::Result<()> {
            // Simulate the daemon becoming ready: write both runtime files.
            self.calls.lock().unwrap().push(binary.to_path_buf());
            std::fs::create_dir_all(runtime_dir(data_dir)).unwrap();
            std::fs::write(endpoint_file(data_dir), "fake-endpoint").unwrap();
            std::fs::write(token_file(data_dir), "fake-token").unwrap();
            Ok(())
        }
    }

    #[tokio::test]
    async fn spawn_writes_runtime_files_then_reports_ready() {
        let dir = tempfile::tempdir().unwrap();
        let spawner = RecordingSpawner {
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let locate = || Some(PathBuf::from("fake-iyagi-termd"));

        spawn_daemon_with(&locate, &spawner, dir.path()).unwrap();
        assert_eq!(spawner.calls.lock().unwrap().len(), 1);
        assert!(wait_ready(dir.path(), Instant::now() + READY_TIMEOUT).await);
        assert_eq!(
            read_runtime_file(&token_file(dir.path())).as_deref(),
            Some("fake-token")
        );
    }

    #[tokio::test]
    async fn wait_ready_times_out_when_files_never_appear() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(runtime_dir(dir.path())).unwrap();
        // Expired deadline: one file check, no sleep.
        let ready = wait_ready(dir.path(), Instant::now()).await;
        assert!(!ready);
    }

    #[test]
    fn spawn_daemon_reports_missing_binary_as_daemon_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        struct NoopSpawner;
        impl DaemonSpawner for NoopSpawner {
            fn spawn_detached(&self, _binary: &Path, _data_dir: &Path) -> io::Result<()> {
                Ok(())
            }
        }
        let err = spawn_daemon_with(&|| None, &NoopSpawner, dir.path())
            .unwrap_err()
            .into_rpc();
        assert_eq!(
            err.code,
            term_contracts::error::ErrorCode::DaemonUnavailable
        );
        assert!(!err.message.contains("token"));
    }
}

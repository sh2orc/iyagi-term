//! Git plumbing for mission workspaces (ticket O06, spec 04 §1): repository
//! identity, clean-base checks, detached worktrees, private snapshot commits,
//! and deterministic candidate application. Everything runs the `git` CLI
//! with explicit argv — never a shell string — and never moves user refs.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Canonical repository identity (04 §1): path + common dir + HEAD + object
/// format. The common dir is what deduplicates the same repository reached
/// through different paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryIdentity {
    pub canonical_path: PathBuf,
    pub common_dir: PathBuf,
    pub head_oid: String,
    pub object_format: String,
}

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("not a git repository: {0}")]
    NotARepository(String),
    #[error("dirty worktree — clean, stash, or commit before starting (samples: {0:?})")]
    DirtyWorktree(Vec<String>),
    /// The `git` executable could not be started (missing from PATH).
    #[error("git is not installed or not executable by the daemon: {0}")]
    GitUnavailable(String),
    /// A repository exists but HEAD resolves to no commit yet.
    #[error("repository has no commits yet: {0}")]
    NoCommits(String),
    /// HEAD moved while a working-tree base snapshot was being written, so
    /// the snapshot's parent would describe a base the user already left.
    #[error("repository HEAD moved while the base snapshot was taken")]
    BaseMoved,
    /// A merge or rebase is in progress: snapshotting now would fold conflict
    /// markers into the base as if they were ordinary work.
    #[error("the repository has unmerged paths; finish or abort the merge first")]
    UnmergedPaths,
    #[error("uncommitted changes are too large to use as a base: {0}")]
    SnapshotTooLarge(String),
    #[error("git failed ({args:?}): {stderr}")]
    Git { args: Vec<String>, stderr: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl GitError {
    /// Stable `details.reason_code` for errors a user can act on.
    pub fn reason_code(&self) -> Option<&'static str> {
        match self {
            GitError::NotARepository(_) => Some("not_a_repository"),
            GitError::DirtyWorktree(_) => Some("dirty_worktree"),
            GitError::GitUnavailable(_) => Some("git_unavailable"),
            GitError::NoCommits(_) => Some("no_commits"),
            GitError::BaseMoved => Some("base_changed"),
            GitError::UnmergedPaths => Some("unmerged_paths"),
            GitError::SnapshotTooLarge(_) => Some("snapshot_too_large"),
            GitError::Git { .. } | GitError::Io(_) => None,
        }
    }
}

pub type GitResult<T> = Result<T, GitError>;

/// Repository selection belongs to the supplied cwd, not the shell that
/// launched the daemon. Apply this to capture/apply subprocesses as well.
///
/// The config prefix mirrors workspace/verification.rs: repository-controlled
/// execution must never fire from daemon git calls. githooks(5) runs
/// post-checkout on `worktree add` (writer/integration worktrees keep their
/// checkout), core.fsmonitor runs a command during `status`, and
/// submodule.recurse drives clone/update from .gitmodules during checkout.
/// `hooks_path_config` names a path that cannot contain hooks, so every hook
/// lookup finds nothing. Callers must not pass their own `core.hooksPath`:
/// a later `-c` overrides this one.
fn git_command() -> Command {
    let mut command = Command::new("git");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    ] {
        command.env_remove(name);
    }
    command
        .arg("-c")
        .arg(hooks_path_config())
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-c")
        .arg("submodule.recurse=false");
    command
}

/// `core.hooksPath=<path>` under which every hook lookup fails.
///
/// Unix: `/dev/null` is a character device, so `/dev/null/<hook>` fails with
/// ENOTDIR.
#[cfg(not(windows))]
fn hooks_path_config() -> &'static std::ffi::OsStr {
    std::ffi::OsStr::new("core.hooksPath=/dev/null")
}

/// `core.hooksPath=<path>` under which every hook lookup fails.
///
/// Windows: Git for Windows maps only the exact name `/dev/null` to NUL, so
/// `/dev/null/<hook>` resolves to `\dev\null\<hook>` on the repository's
/// drive — a directory any local user may create under `C:\`. Point it at
/// the daemon's own executable instead: a regular file can never have
/// children, so `<exe>\<hook>` cannot exist on any filesystem, and only
/// someone who can already replace the daemon could change that path. No
/// directory is created and nothing depends on the data dir.
#[cfg(windows)]
fn hooks_path_config() -> &'static std::ffi::OsStr {
    static VALUE: std::sync::OnceLock<std::ffi::OsString> = std::sync::OnceLock::new();
    let value = VALUE.get_or_init(|| {
        let mut value = std::ffi::OsString::from("core.hooksPath=");
        match std::env::current_exe() {
            Ok(exe) => value.push(exe),
            // Not expected (GetModuleFileNameW). Fail closed with a name that
            // Win32 refuses (`<`, `>`), so no such directory can be opened.
            Err(_) => value.push("C:\\<iyagi-no-hooks>"),
        }
        value
    });
    value.as_os_str()
}

/// Test-visible runner (integration tests drive real repositories).
pub fn run_git_for_test(cwd: &Path, args: &[&str]) -> String {
    run_git(cwd, args).expect("git succeeds in tests")
}

pub(super) fn run_git(cwd: &Path, args: &[&str]) -> GitResult<String> {
    String::from_utf8(run_git_bytes(cwd, args)?)
        .map(|s| s.trim_end_matches(['\r', '\n']).to_string())
        .map_err(|_| GitError::Git {
            args: args.iter().map(|s| s.to_string()).collect(),
            stderr: "Git output is not UTF-8".into(),
        })
}

pub(super) fn run_git_bytes(cwd: &Path, args: &[&str]) -> GitResult<Vec<u8>> {
    // Deterministic diffs: ignore the user's line-ending config so captures
    // and integrations behave identically across machines.
    let mut command = git_command();
    command
        // Full OIDs must identify the actual stored objects even when the
        // repository has local refs/replace entries. Do not modify those refs.
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .arg("-c")
        .arg("core.autocrlf=false")
        .arg("-c")
        .arg("user.name=iyagi daemon")
        .arg("-c")
        .arg("user.email=daemon@iyagi.local")
        .current_dir(cwd)
        .args(args);
    let output = command.output().map_err(|error| spawn_error(cwd, error))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(GitError::Git {
            args: args.iter().map(|a| a.to_string()).collect(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

/// A missing executable is an environment problem, not a repository
/// property; callers must not report it as "not a repository".
fn spawn_error(cwd: &Path, error: std::io::Error) -> GitError {
    if error.kind() == std::io::ErrorKind::NotFound && cwd.is_dir() {
        GitError::GitUnavailable(error.to_string())
    } else {
        GitError::Io(error)
    }
}

/// Read-only query bounded by `timeout`: `None` when Git did not finish in
/// time (the child is killed and reaped). Housekeeping RPCs use this to stay
/// inside the bridge timeout. Never use it for commands that modify state.
fn run_git_bytes_within(
    cwd: &Path,
    args: &[&str],
    timeout: Duration,
) -> Option<GitResult<Vec<u8>>> {
    use std::io::Read;
    let deadline = Instant::now() + timeout;
    // Same repository view as `run_git_bytes` (e.g. `status_entries`).
    let mut child = match git_command()
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .arg("-c")
        .arg("core.autocrlf=false")
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return Some(Err(spawn_error(cwd, error))),
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        let error = std::io::Error::other("git stdout unavailable");
        return Some(Err(GitError::Io(error)));
    };
    // A reader thread keeps a large output from filling the pipe while the
    // deadline is enforced here.
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.read_to_end(&mut bytes).map(|_| bytes);
        let _ = sender.send(result);
    });
    let output = match receiver.recv_timeout(timeout) {
        Ok(output) => output,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Some(output.map_err(GitError::Io)),
            Ok(Some(status)) => {
                return Some(Err(GitError::Git {
                    args: args.iter().map(|a| a.to_string()).collect(),
                    stderr: format!("git exited with {status}"),
                }));
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Some(Err(GitError::Io(error)));
            }
        }
    }
}

/// Resolve and validate a repository. `expected_base_oid`, when given, must
/// equal the current HEAD — start refuses silently-moved bases (02 §4).
pub fn repository_identity(path: &Path) -> GitResult<RepositoryIdentity> {
    let canonical = path
        .canonicalize()
        .map_err(|_| GitError::NotARepository(path.display().to_string()))?;
    let top =
        run_git(&canonical, &["rev-parse", "--show-toplevel"]).map_err(|error| match error {
            GitError::GitUnavailable(_) => error,
            _ => GitError::NotARepository(path.display().to_string()),
        })?;
    let top = PathBuf::from(top);
    let canonical = top.canonicalize().unwrap_or(top);
    let common = run_git(&canonical, &["rev-parse", "--git-common-dir"])?;
    let common = canonical.join(common);
    // `rev-parse --verify` fails only when HEAD names no commit (unborn branch).
    let head_oid = run_git(&canonical, &["rev-parse", "--verify", "-q", "HEAD"]).map_err(
        |error| match error {
            GitError::Git { .. } => GitError::NoCommits(canonical.display().to_string()),
            other => other,
        },
    )?;
    let object_format = run_git(&canonical, &["rev-parse", "--show-object-format"])
        .unwrap_or_else(|_| "sha1".to_string());
    validate_oid(&head_oid)?;
    Ok(RepositoryIdentity {
        canonical_path: canonical,
        common_dir: common.canonicalize().unwrap_or(common),
        head_oid,
        object_format,
    })
}

pub fn validate_oid(oid: &str) -> GitResult<()> {
    let hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    if (oid.len() == 40 || oid.len() == 64) && oid.bytes().all(hex) {
        Ok(())
    } else {
        Err(GitError::Git {
            args: vec!["rev-parse".into()],
            stderr: format!("invalid object id {oid:?}"),
        })
    }
}

/// Require a full commit OID (not a branch, tag, tree, or revision expression)
/// and return its actual tree, with replacement objects disabled.
pub fn commit_tree_oid(repo: &Path, oid: &str) -> GitResult<String> {
    validate_oid(oid)?;
    if run_git(repo, &["cat-file", "-t", oid])? != "commit" {
        return Err(GitError::Git {
            args: vec!["cat-file".into()],
            stderr: "integration input is not a commit".into(),
        });
    }
    rev_parse(repo, &format!("{oid}^{{tree}}"))
}

/// `git status --porcelain` entries (empty = clean). Staged, unstaged, and
/// untracked all count (04 §1: no auto commit/stash).
pub fn status_entries(repo: &Path) -> GitResult<Vec<(char, String)>> {
    // NUL records preserve whitespace, Unicode, quotes, and newlines. Disable
    // rename folding so both old and new paths are subject to scope checks.
    let out = run_git_bytes(
        repo,
        &["status", "--porcelain=v1", "-z", "-uall", "--no-renames"],
    )?;
    let mut entries = Vec::new();
    for record in out.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        if record.len() < 4 || record[2] != b' ' {
            return Err(GitError::Git {
                args: vec!["status".into()],
                stderr: "malformed Git status record".into(),
            });
        }
        let code = if record[0] == b' ' {
            record[1]
        } else {
            record[0]
        } as char;
        let path = std::str::from_utf8(&record[3..]).map_err(|_| GitError::Git {
            args: vec!["status".into()],
            stderr: "non-UTF-8 paths are unsupported".into(),
        })?;
        entries.push((code, path.into()));
    }
    Ok(entries)
}

/// Whether `git status` (staged, unstaged and untracked, as
/// [`status_entries`]) is empty, answered within `timeout` (`None` = Git did
/// not finish in time).
pub fn is_clean_within(worktree: &Path, timeout: Duration) -> Option<GitResult<bool>> {
    run_git_bytes_within(
        worktree,
        &["status", "--porcelain=v1", "-z", "-uall", "--no-renames"],
        timeout,
    )
    .map(|result| result.map(|out| out.is_empty()))
}

/// W01: refuse to start on a dirty repository.
pub fn ensure_clean(repo: &Path) -> GitResult<()> {
    let entries = status_entries(repo)?;
    if entries.is_empty() {
        return Ok(());
    }
    let samples = entries
        .iter()
        .take(5)
        .map(|(_, path)| path.clone())
        .collect();
    Err(GitError::DirtyWorktree(samples))
}

/// Detached worktree at `base_oid` under the daemon-owned destination
/// (04 §1: never checks out or resets the user's branch).
pub fn add_detached_worktree(repo: &Path, base_oid: &str, destination: &Path) -> GitResult<()> {
    run_git(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            &destination.to_string_lossy(),
            base_oid,
        ],
    )?;
    Ok(())
}

pub fn remove_worktree(repo: &Path, worktree: &Path) -> GitResult<()> {
    let _ = run_git(
        repo,
        &["worktree", "remove", "--force", &worktree.to_string_lossy()],
    );
    Ok(())
}

/// Remove a worktree only if Git itself still finds it clean. Unlike
/// [`remove_worktree`] there is no `--force`: changes made after a caller's
/// status check make the removal fail instead of being deleted.
pub fn remove_clean_worktree(repo: &Path, worktree: &Path) -> GitResult<()> {
    run_git(repo, &["worktree", "remove", &worktree.to_string_lossy()])?;
    Ok(())
}

/// `worktree <path>` records of `git worktree list --porcelain`.
fn parse_worktree_list(out: &[u8], separator: u8) -> Vec<PathBuf> {
    out.split(|b| *b == separator)
        .filter_map(|field| std::str::from_utf8(field).ok()?.strip_prefix("worktree "))
        .map(|path| PathBuf::from(path.trim_end_matches('\r')))
        .collect()
}

/// Paths of every worktree registered in the repository's common directory,
/// answered within `timeout` (`None` = Git did not finish in time). NUL
/// records keep newlines and quotes in paths intact; Git older than 2.36 has
/// no `-z` here, so its line records are parsed instead (a path that cannot
/// be represented there simply does not match any workspace).
pub fn worktree_paths_within(repo: &Path, timeout: Duration) -> Option<GitResult<Vec<PathBuf>>> {
    let deadline = Instant::now() + timeout;
    match run_git_bytes_within(repo, &["worktree", "list", "--porcelain", "-z"], timeout)? {
        Ok(out) => Some(Ok(parse_worktree_list(&out, 0))),
        Err(GitError::Git { .. }) => {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            let out = run_git_bytes_within(repo, &["worktree", "list", "--porcelain"], remaining)?;
            Some(out.map(|out| parse_worktree_list(&out, b'\n')))
        }
        Err(error) => Some(Err(error)),
    }
}

/// Delete the daemon's private refs below `prefix` (for example
/// `refs/iyagi/missions/<id>/inputs`) in one `update-ref --stdin` transaction.
/// Anything outside the daemon namespace is refused; user branches and tags
/// are never listed or touched.
pub fn delete_private_refs(repo: &Path, prefix: &str) -> GitResult<u32> {
    let prefix = prefix.trim_end_matches('/');
    if !prefix.starts_with("refs/iyagi/missions/") || prefix.contains("..") {
        return Err(GitError::Git {
            args: vec!["update-ref".into()],
            stderr: "refusing to delete refs outside the daemon namespace".into(),
        });
    }
    let listed = run_git(repo, &["for-each-ref", "--format=%(refname)", prefix])?;
    let children = format!("{prefix}/");
    let names: Vec<&str> = listed
        .lines()
        .filter(|name| name.starts_with(&children))
        .collect();
    if names.is_empty() {
        return Ok(0);
    }
    let mut commands = String::new();
    for name in &names {
        commands.push_str("delete ");
        commands.push_str(name);
        commands.push('\n');
    }
    run_git_with_stdin(repo, &["update-ref", "--stdin"], commands.as_bytes())?;
    Ok(u32::try_from(names.len()).unwrap_or(u32::MAX))
}

fn run_git_with_stdin(cwd: &Path, args: &[&str], input: &[u8]) -> GitResult<()> {
    use std::io::Write;
    let mut child = git_command()
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| spawn_error(cwd, error))?;
    // Dropping the handle at the end of this statement closes Git's stdin.
    let written = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(input),
        None => Err(std::io::Error::other("git stdin unavailable")),
    };
    let output = child.wait_with_output()?;
    written?;
    if output.status.success() {
        Ok(())
    } else {
        Err(GitError::Git {
            args: args.iter().map(|a| a.to_string()).collect(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

/// Capture must never advance a branch an agent checked out in its worktree.
pub fn ensure_detached_head(worktree: &Path) -> GitResult<()> {
    let output = git_command()
        .current_dir(worktree)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()?;
    if output.status.code() == Some(1) {
        return Ok(());
    }
    Err(GitError::Git {
        args: vec!["symbolic-ref".into()],
        stderr: if output.status.success() {
            "capture requires a detached HEAD; workspace retained without committing".into()
        } else {
            String::from_utf8_lossy(&output.stderr).to_string()
        },
    })
}

/// Read the immutable Git blob, including a symlink's target text. Check
/// its object size before allocating; never follow a worktree symlink.
pub fn read_blob_bounded(
    worktree: &Path,
    commit: &str,
    path: &str,
    max_bytes: usize,
) -> GitResult<Vec<u8>> {
    validate_oid(commit)?;
    let object = format!("{commit}:{path}");
    let kind = run_git(worktree, &["cat-file", "-t", &object])?;
    let size: usize = run_git(worktree, &["cat-file", "-s", &object])?
        .parse()
        .map_err(|_| GitError::Git {
            args: vec!["cat-file".into()],
            stderr: "invalid Git object size".into(),
        })?;
    if kind != "blob" || size > max_bytes {
        return Err(GitError::Git {
            args: vec!["cat-file".into()],
            stderr: format!("unsupported object or file exceeds capture bound: {path:?}"),
        });
    }
    let bytes = run_git_bytes(worktree, &["cat-file", "blob", &object])?;
    if bytes.len() != size {
        return Err(GitError::Git {
            args: vec!["cat-file".into()],
            stderr: "Git object size mismatch".into(),
        });
    }
    Ok(bytes)
}

/// Commit every change in a detached worktree and point a private ref at it
/// (04 §3: keeps objects alive against GC; user branches never move).
pub fn commit_on_private_ref(
    worktree: &Path,
    ref_name: &str,
    message: &str,
) -> GitResult<(String, String)> {
    ensure_detached_head(worktree)?;
    let parent = rev_parse(worktree, "HEAD")?;
    run_git(worktree, &["add", "-A"])?;
    let tree_oid = run_git(worktree, &["write-tree"])?;
    let commit_oid = run_git(
        worktree,
        &["commit-tree", &tree_oid, "-p", &parent, "-m", message],
    )?;
    run_git(worktree, &["update-ref", ref_name, &commit_oid])?;
    // Plumbing avoids hooks and branch updates. Even if another actor
    // attaches HEAD between the check and this CAS, no user ref is moved.
    run_git(
        worktree,
        &["update-ref", "--no-deref", "HEAD", &commit_oid, &parent],
    )?;
    validate_oid(&commit_oid)?;
    validate_oid(&tree_oid)?;
    Ok((commit_oid, tree_oid))
}

/// Retain a commit the agent already made in its detached worktree.
pub fn retain_head_on_private_ref(worktree: &Path, ref_name: &str) -> GitResult<(String, String)> {
    ensure_detached_head(worktree)?;
    let commit = rev_parse(worktree, "HEAD")?;
    let tree = rev_parse(worktree, "HEAD^{tree}")?;
    run_git(worktree, &["update-ref", ref_name, &commit])?;
    Ok((commit, tree))
}

/// `base..commit` diff as bytes (used by deterministic integration).
pub fn diff_commit(worktree: &Path, base: &str, commit: &str) -> GitResult<Vec<u8>> {
    validate_oid(base)?;
    validate_oid(commit)?;
    let range = format!("{base}..{commit}");
    let mut command = git_command();
    command
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .arg("-c")
        .arg("core.autocrlf=false")
        .current_dir(worktree)
        .args([
            "diff",
            "--binary",
            "--no-color",
            "--output-indicator-new=+",
            "--output-indicator-old=-",
            "--output-indicator-context= ",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            &range,
            "--",
        ]);
    let output = command.output()?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(GitError::Git {
            args: vec!["diff".into(), range],
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

/// One path entry of a commit relative to a base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffEntry {
    pub kind: ChangeKind,
    pub path: String,
}

/// File-level changes of `commit` vs `base`.
pub fn diff_names(worktree: &Path, base: &str, commit: &str) -> GitResult<Vec<DiffEntry>> {
    let range = format!("{base}..{commit}");
    let out = run_git_bytes(
        worktree,
        &[
            "diff",
            "--name-status",
            "--no-color",
            "-z",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            &range,
            "--",
        ],
    )?;
    let fields: Vec<_> = out.split(|b| *b == 0).filter(|r| !r.is_empty()).collect();
    if fields.len() % 2 != 0 {
        return Err(GitError::Git {
            args: vec!["diff".into()],
            stderr: "malformed Git diff record".into(),
        });
    }
    let mut entries = Vec::new();
    for pair in fields.chunks_exact(2) {
        let kind = match pair[0][0] {
            b'A' => ChangeKind::Added,
            b'D' => ChangeKind::Deleted,
            _ => ChangeKind::Modified,
        };
        let path = std::str::from_utf8(pair[1]).map_err(|_| GitError::Git {
            args: vec!["diff".into()],
            stderr: "non-UTF-8 paths are unsupported".into(),
        })?;
        entries.push(DiffEntry {
            kind,
            path: path.into(),
        });
    }
    Ok(entries)
}

/// The private-ref namespace owned by the daemon.
pub fn candidate_ref(mission_id: &str, candidate_id: &str) -> String {
    format!("refs/iyagi/missions/{mission_id}/candidates/{candidate_id}")
}

/// Resolve a ref to an oid.
pub fn rev_parse(cwd: &Path, reference: &str) -> GitResult<String> {
    let oid = run_git(cwd, &["rev-parse", reference])?;
    validate_oid(&oid)?;
    Ok(oid)
}

/// Apply a binary patch with a three-way fallback. Returns false when the
/// patch conflicts (the caller records the partial state; nothing is rolled
/// back silently).
pub struct ApplyOutcome {
    pub ok: bool,
    pub conflict_paths: Vec<String>,
}

pub fn apply_three_way(cwd: &Path, patch: &[u8]) -> GitResult<ApplyOutcome> {
    use std::io::Write;
    let mut child = git_command()
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .arg("-c")
        .arg("core.autocrlf=false")
        .current_dir(cwd)
        .args(["apply", "--3way", "--index", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(patch)?;
    }
    drop(child.stdin.take());
    let output = child.wait_with_output()?;
    if output.status.success() {
        return Ok(ApplyOutcome {
            ok: true,
            conflict_paths: Vec::new(),
        });
    }
    // Index entries preserve literal paths (including quotes, tabs and
    // newlines); human-readable stderr may quote or split those names.
    let unmerged = run_git_bytes(cwd, &["ls-files", "--unmerged", "-z"])?;
    let mut indexed_paths = Vec::new();
    for entry in unmerged
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
    {
        let Some(tab) = entry.iter().position(|b| *b == b'\t') else {
            return Err(GitError::Git {
                args: vec!["ls-files".into()],
                stderr: "invalid unmerged index entry".into(),
            });
        };
        let path = std::str::from_utf8(&entry[tab + 1..]).map_err(|_| GitError::Git {
            args: vec!["ls-files".into()],
            stderr: "conflict path is not UTF-8".into(),
        })?;
        indexed_paths.push(path.to_owned());
    }
    if !indexed_paths.is_empty() {
        indexed_paths.sort();
        indexed_paths.dedup();
        return Ok(ApplyOutcome {
            ok: false,
            conflict_paths: indexed_paths,
        });
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout_text = String::from_utf8_lossy(&output.stdout);
    let mut paths = Vec::new();
    for line in stderr.lines().chain(stdout_text.lines()) {
        for marker in [
            "error: patch failed:",
            "CONFLICT (content): Merge conflict in",
            // git >= 2.4x --3way conflict wording
            "Applied patch to '",
        ] {
            if let Some(rest) = line.strip_prefix(marker) {
                let path = rest
                    .trim_end_matches("' with conflicts.")
                    .split(':')
                    .next()
                    .unwrap_or(rest)
                    .trim()
                    .trim_matches('\'');
                if !path.is_empty() && path != "." {
                    paths.push(path.to_string());
                }
            }
        }
    }
    paths.sort();
    paths.dedup();
    Ok(ApplyOutcome {
        ok: false,
        conflict_paths: paths,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo(dir: &Path) {
        run_git(dir, &["init", "-q"]).unwrap();
        run_git(dir, &["commit", "--allow-empty", "-m", "base", "-q"]).unwrap();
    }

    #[test]
    fn identity_and_clean_checks() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let identity = repository_identity(dir.path()).unwrap();
        assert_eq!(identity.object_format, "sha1");
        assert!(identity.head_oid.len() == 40);
        ensure_clean(dir.path()).unwrap();
        std::fs::write(dir.path().join("dirty.txt"), "x").unwrap();
        assert!(matches!(
            ensure_clean(dir.path()),
            Err(GitError::DirtyWorktree(_))
        ));
    }

    #[test]
    fn not_a_repository_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let error = repository_identity(dir.path()).unwrap_err();
        assert!(matches!(error, GitError::NotARepository(_)));
        assert_eq!(error.reason_code(), Some("not_a_repository"));
    }

    #[test]
    fn unborn_head_is_distinct_from_missing_repository_and_git() {
        let dir = tempfile::tempdir().unwrap();
        run_git(dir.path(), &["init", "-q"]).unwrap();
        let error = repository_identity(dir.path()).unwrap_err();
        assert!(matches!(error, GitError::NoCommits(_)), "{error:?}");
        assert_eq!(error.reason_code(), Some("no_commits"));
        std::fs::write(dir.path().join("dirty.txt"), "x").unwrap();
        assert_eq!(
            ensure_clean(dir.path()).unwrap_err().reason_code(),
            Some("dirty_worktree")
        );
        assert_eq!(
            GitError::GitUnavailable("fixture".into()).reason_code(),
            Some("git_unavailable")
        );
    }

    // Regression test for the git_command() config prefix: a planted
    // post-checkout hook (githooks(5) runs it on `worktree add`) and a
    // configured core.fsmonitor command must never execute.
    #[cfg(unix)]
    #[test]
    fn worktree_add_and_status_never_execute_repository_hooks() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        init_repo(&root);
        let commit = run_git(&root, &["rev-parse", "HEAD"]).unwrap();
        let marker = dir.path().join("executed");
        let executable = dir.path().join("unexpected-hook");
        std::fs::write(
            &executable,
            format!("#!/bin/sh\nprintf executed >> '{}'\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::copy(&executable, root.join(".git/hooks/post-checkout")).unwrap();
        run_git(
            &root,
            &["config", "core.fsmonitor", executable.to_str().unwrap()],
        )
        .unwrap();
        let worktree = dir.path().join("writer");
        add_detached_worktree(&root, &commit, &worktree).unwrap();
        assert!(
            !marker.exists(),
            "post-checkout must not run on worktree add"
        );
        status_entries(&root).unwrap();
        assert!(!marker.exists(), "core.fsmonitor must not run on status");
    }

    // Windows has no ENOTDIR trick for `/dev/null` (Git for Windows would
    // look under `\dev\null\` on the repository drive): the hooks path must
    // be an existing regular file, which can never contain a hook.
    #[cfg(windows)]
    #[test]
    fn windows_hooks_path_is_a_file_that_cannot_contain_hooks() {
        let config = hooks_path_config().to_string_lossy();
        let value = config.strip_prefix("core.hooksPath=").unwrap();
        let hooks = Path::new(value);
        assert!(hooks.is_absolute(), "{value}");
        assert!(hooks.is_file(), "{value}");
        assert!(!hooks.join("post-checkout").exists());
    }

    /// Commit one file so the fixture has a real tree to diff against.
    fn commit_file(dir: &Path, name: &str, body: &str, message: &str) -> String {
        std::fs::write(dir.join(name), body).unwrap();
        run_git(dir, &["add", name]).unwrap();
        run_git(dir, &["commit", "-m", message, "-q"]).unwrap();
        run_git(dir, &["rev-parse", "HEAD"]).unwrap()
    }

    fn staged_paths(dir: &Path) -> String {
        run_git(dir, &["diff", "--cached", "--name-only"]).unwrap()
    }

    #[test]
    fn snapshot_records_the_working_tree_and_leaves_the_user_checkout_alone() {
        let dir = tempfile::tempdir().unwrap();
        // The scratch index lives outside the working tree, as it does in
        // production; inside it, `add -A` would fold it into the snapshot.
        let repo = &dir.path().join("repo");
        std::fs::create_dir(repo).unwrap();
        init_repo(repo);
        commit_file(repo, ".gitignore", "ignored.txt\n", "ignore");
        let head = commit_file(repo, "tracked.txt", "committed\n", "tracked");
        std::fs::write(repo.join("tracked.txt"), "edited\n").unwrap();
        std::fs::write(repo.join("untracked.txt"), "new\n").unwrap();
        std::fs::write(repo.join("ignored.txt"), "secret\n").unwrap();
        let branch = run_git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        let status = status_entries(repo).unwrap();
        let staged = staged_paths(repo);

        let scratch = dir.path().join("scratch");
        let snapshot = snapshot_working_tree(repo, &scratch, "m-1", &head).unwrap();

        assert!(snapshot.is_snapshot());
        assert_eq!(snapshot.head_oid, head);
        assert_eq!(snapshot.entry_count, 2, "ignored files are not entries");
        assert_eq!(
            run_git(repo, &["rev-parse", &base_snapshot_ref("m-1")]).unwrap(),
            snapshot.commit_oid
        );
        assert_eq!(
            run_git(repo, &["rev-parse", &format!("{}^", snapshot.commit_oid)]).unwrap(),
            head,
            "the snapshot sits on the commit the user is on"
        );
        let read = |path: &str| {
            run_git(
                repo,
                &["cat-file", "-p", &format!("{}:{path}", snapshot.commit_oid)],
            )
        };
        assert_eq!(read("tracked.txt").unwrap(), "edited");
        assert_eq!(read("untracked.txt").unwrap(), "new");
        assert!(read("ignored.txt").is_err(), "ignored files stay out");

        // Nothing of the user's moved: same HEAD, same branch, same pending
        // changes, nothing staged, and the files still hold their own text.
        assert_eq!(run_git(repo, &["rev-parse", "HEAD"]).unwrap(), head);
        assert_eq!(
            run_git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(),
            branch
        );
        assert_eq!(status_entries(repo).unwrap(), status);
        assert_eq!(staged_paths(repo), staged);
        assert_eq!(
            std::fs::read_to_string(repo.join("tracked.txt")).unwrap(),
            "edited\n"
        );
    }

    #[test]
    fn a_clean_tree_records_no_snapshot_and_an_unchanged_one_repeats_its_commit() {
        let dir = tempfile::tempdir().unwrap();
        let repo = &dir.path().join("repo");
        std::fs::create_dir(repo).unwrap();
        init_repo(repo);
        let head = commit_file(repo, "tracked.txt", "committed\n", "tracked");
        let scratch = dir.path().join("scratch");

        let clean = snapshot_working_tree(repo, &scratch, "m-1", &head).unwrap();
        assert!(!clean.is_snapshot());
        assert_eq!(clean.commit_oid, head);
        assert_eq!(clean.entry_count, 0);
        assert!(
            run_git(
                repo,
                &["rev-parse", "--verify", "-q", &base_snapshot_ref("m-1")]
            )
            .is_err(),
            "a clean tree mints no private ref"
        );

        std::fs::write(repo.join("tracked.txt"), "edited\n").unwrap();
        let first = snapshot_working_tree(repo, &scratch, "m-1", &head).unwrap();
        let second = snapshot_working_tree(repo, &scratch, "m-1", &head).unwrap();
        assert_eq!(
            first.commit_oid, second.commit_oid,
            "re-recording an unchanged tree is idempotent"
        );
    }

    #[test]
    fn snapshot_refuses_a_repository_with_unmerged_paths() {
        let dir = tempfile::tempdir().unwrap();
        let repo = &dir.path().join("repo");
        std::fs::create_dir(repo).unwrap();
        init_repo(repo);
        commit_file(repo, "conflict.txt", "base\n", "base file");
        run_git(repo, &["checkout", "-b", "side", "-q"]).unwrap();
        commit_file(repo, "conflict.txt", "side\n", "side");
        run_git(repo, &["checkout", "-", "-q"]).unwrap();
        let head = commit_file(repo, "conflict.txt", "main\n", "main");
        assert!(run_git(repo, &["merge", "side", "--no-edit"]).is_err());

        let error = snapshot_working_tree(repo, &dir.path().join("scratch"), "m-1", &head)
            .expect_err("a conflicted tree cannot be a base");
        assert!(matches!(error, GitError::UnmergedPaths));
        assert_eq!(error.reason_code(), Some("unmerged_paths"));
        assert_eq!(
            run_git(repo, &["rev-parse", "HEAD"]).unwrap(),
            head,
            "a refused snapshot leaves HEAD where it was"
        );
    }

    #[test]
    fn snapshot_bounds_refuse_an_unreasonable_uncommitted_set() {
        let dir = tempfile::tempdir().unwrap();
        let entries: Vec<(char, String)> = (0..=MAX_BASE_SNAPSHOT_ENTRIES)
            .map(|index| ('?', format!("file-{index}.txt")))
            .collect();
        let error = check_snapshot_bounds(dir.path(), &entries).expect_err("over the entry bound");
        assert!(matches!(error, GitError::SnapshotTooLarge(_)));
        assert_eq!(error.reason_code(), Some("snapshot_too_large"));
        assert_eq!(check_snapshot_bounds(dir.path(), &entries[..1]).unwrap(), 1);
    }
}

/// Compose dependency patches through a private index. The user's index,
/// checkout, and branch are never selected. Deterministic commit metadata
/// makes retry after a pre-claim crash produce the same immutable input.
pub fn compose_task_input(
    repository: &Path,
    scratch_root: &Path,
    mission_id: &str,
    run_id: &str,
    base: &str,
    sources: &[(String, String)], // (source input base, captured commit)
) -> GitResult<String> {
    if sources.is_empty() {
        return Ok(base.into());
    }
    std::fs::create_dir_all(scratch_root)?;
    let scratch = tempfile::Builder::new()
        .prefix("input-")
        .tempdir_in(scratch_root)?;
    let index = scratch.path().join("index");
    // Build from git_command() like every other git call site: the inherited
    // GIT_DIR/GIT_WORK_TREE scrub and the neutralized repo config apply here
    // too. GIT_INDEX_FILE is set after the scrub, so the private index wins.
    let command = || {
        let mut cmd = git_command();
        cmd.current_dir(repository)
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_INDEX_FILE", &index)
            .args([
                "-c",
                "core.autocrlf=false",
                "-c",
                "user.name=iyagi daemon",
                "-c",
                "user.email=daemon@iyagi.local",
            ]);
        cmd
    };
    let run = |args: &[&str]| -> GitResult<String> {
        let output = command().args(args).output()?;
        if !output.status.success() {
            return Err(GitError::Git {
                args: args.iter().map(|s| s.to_string()).collect(),
                stderr: String::from_utf8_lossy(&output.stderr).into(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().into())
    };
    run(&["read-tree", base])?;
    for (source_base, commit) in sources {
        let patch = diff_commit(repository, source_base, commit)?;
        if patch.is_empty() {
            continue;
        }
        use std::io::Write;
        let mut child = command()
            .args(["apply", "--cached", "--3way", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let write = child.stdin.take().expect("piped stdin").write_all(&patch);
        let output = child.wait_with_output()?;
        write?;
        if !output.status.success() {
            return Err(GitError::Git {
                args: vec!["apply".into()],
                stderr: format!(
                    "dependency input conflict: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }
    }
    let tree = run(&["write-tree"])?;
    let output = command()
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00+00:00")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00+00:00")
        .args([
            "commit-tree",
            &tree,
            "-p",
            base,
            "-m",
            &format!("iyagi input {run_id}"),
        ])
        .output()?;
    if !output.status.success() {
        return Err(GitError::Git {
            args: vec!["commit-tree".into()],
            stderr: String::from_utf8_lossy(&output.stderr).into(),
        });
    }
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
    validate_oid(&commit)?;
    run(&[
        "update-ref",
        &format!("refs/iyagi/missions/{mission_id}/inputs/{run_id}"),
        &commit,
    ])?;
    Ok(commit)
}

// ---- working-tree base snapshot (04 §1) ---------------------------------

/// Bounds for the uncommitted set a base snapshot may fold in. A snapshot
/// hashes only what Git reports as changed, so these bound that set, not the
/// repository. They are far below the verification input bounds because this
/// work happens inside an interactive `mission.create` call, which the bridge
/// gives 5 s: writing and compressing the objects has to fit well inside it.
pub const MAX_BASE_SNAPSHOT_ENTRIES: usize = 2_000;
pub const MAX_BASE_SNAPSHOT_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_BASE_SNAPSHOT_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Fixed commit identity: the same working tree on the same HEAD always
/// produces the same snapshot OID, so re-snapshotting at start is idempotent
/// and an unchanged tree mints no second object.
const SNAPSHOT_DATE: &str = "2000-01-01T00:00:00+00:00";
const SNAPSHOT_MESSAGE: &str = "iyagi base snapshot";

/// Private ref keeping a mission's snapshot base reachable. It sits under
/// `inputs/` so mission housekeeping removes it with the other intermediate
/// inputs; candidates built on the base keep the commit reachable through
/// their own parent chain.
pub fn base_snapshot_ref(mission_id: &str) -> String {
    format!("refs/iyagi/missions/{mission_id}/inputs/base")
}

/// Outcome of [`snapshot_working_tree`]. `commit_oid == head_oid` when the
/// working tree held nothing to fold in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingTreeSnapshot {
    pub commit_oid: String,
    pub tree_oid: String,
    pub head_oid: String,
    /// Uncommitted entries folded into the snapshot (0 when clean).
    pub entry_count: u32,
}

impl WorkingTreeSnapshot {
    /// Whether anything was folded in on top of HEAD.
    pub fn is_snapshot(&self) -> bool {
        self.commit_oid != self.head_oid
    }
}

/// Record the user's working tree as a daemon-private commit on top of
/// `head_oid` (04 §1). The user's index, checkout, branch and HEAD are never
/// written: staging happens in a private index copy and the only ref touched
/// is `refs/iyagi/missions/<id>/inputs/base`. Ignored files stay out, exactly
/// as they do for capture.
pub fn snapshot_working_tree(
    repository: &Path,
    scratch_root: &Path,
    mission_id: &str,
    head_oid: &str,
) -> GitResult<WorkingTreeSnapshot> {
    validate_oid(head_oid)?;
    let head_tree = commit_tree_oid(repository, head_oid)?;
    let unchanged = WorkingTreeSnapshot {
        commit_oid: head_oid.to_string(),
        tree_oid: head_tree.clone(),
        head_oid: head_oid.to_string(),
        entry_count: 0,
    };
    let entries = status_entries(repository)?;
    if entries.is_empty() {
        return Ok(unchanged);
    }
    let entry_count = check_snapshot_bounds(repository, &entries)?;
    ensure_no_unmerged_paths(repository)?;

    std::fs::create_dir_all(scratch_root)?;
    let scratch = tempfile::Builder::new()
        .prefix("base-")
        .tempdir_in(scratch_root)?;
    let index = scratch.path().join("index");
    copy_repository_index(repository, &index)?;

    // Built from git_command() like every other call site: the inherited
    // GIT_DIR/GIT_WORK_TREE scrub and the neutralized repository config apply
    // here too. GIT_INDEX_FILE is set after the scrub, so the private index
    // wins and `add` cannot reach the user's own.
    let command = || {
        let mut cmd = git_command();
        cmd.current_dir(repository)
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_INDEX_FILE", &index)
            .args([
                "-c",
                "core.autocrlf=false",
                "-c",
                "user.name=iyagi daemon",
                "-c",
                "user.email=daemon@iyagi.local",
            ]);
        cmd
    };
    let run = |args: &[&str]| -> GitResult<String> {
        let output = command().args(args).output()?;
        if !output.status.success() {
            return Err(GitError::Git {
                args: args.iter().map(|s| s.to_string()).collect(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().into(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().into())
    };
    run(&["add", "-A"])?;
    let tree_oid = run(&["write-tree"])?;
    validate_oid(&tree_oid)?;
    // Status can report entries that leave the tree identical (mode-only or
    // stat-only noise). Nothing to record then.
    if tree_oid == head_tree {
        return Ok(unchanged);
    }
    let output = command()
        .env("GIT_AUTHOR_DATE", SNAPSHOT_DATE)
        .env("GIT_COMMITTER_DATE", SNAPSHOT_DATE)
        .args([
            "commit-tree",
            &tree_oid,
            "-p",
            head_oid,
            "-m",
            SNAPSHOT_MESSAGE,
        ])
        .output()?;
    if !output.status.success() {
        return Err(GitError::Git {
            args: vec!["commit-tree".into()],
            stderr: String::from_utf8_lossy(&output.stderr).trim().into(),
        });
    }
    let commit_oid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    validate_oid(&commit_oid)?;
    // The parent must still be where the caller found it. A commit or branch
    // switch during staging would otherwise be recorded as this base.
    if rev_parse(repository, "HEAD")? != head_oid {
        return Err(GitError::BaseMoved);
    }
    run(&["update-ref", &base_snapshot_ref(mission_id), &commit_oid])?;
    Ok(WorkingTreeSnapshot {
        commit_oid,
        tree_oid,
        head_oid: head_oid.to_string(),
        entry_count,
    })
}

/// Refuse a snapshot that would fold in an unreasonable amount of data
/// (a large untracked build output or dataset that is not ignored).
fn check_snapshot_bounds(repository: &Path, entries: &[(char, String)]) -> GitResult<u32> {
    let mib = |bytes: u64| bytes / (1024 * 1024);
    if entries.len() > MAX_BASE_SNAPSHOT_ENTRIES {
        return Err(GitError::SnapshotTooLarge(format!(
            "{} uncommitted entries exceed the {} entry bound",
            entries.len(),
            MAX_BASE_SNAPSHOT_ENTRIES
        )));
    }
    let mut total: u64 = 0;
    for (_, path) in entries {
        // Deleted paths have nothing to read; a symlink is sized as the link
        // itself so no link out of the repository is ever followed.
        let Ok(metadata) = std::fs::symlink_metadata(repository.join(path)) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let bytes = metadata.len();
        if bytes > MAX_BASE_SNAPSHOT_FILE_BYTES {
            return Err(GitError::SnapshotTooLarge(format!(
                "{path:?} exceeds the {} MiB per-file bound",
                mib(MAX_BASE_SNAPSHOT_FILE_BYTES)
            )));
        }
        total = total.saturating_add(bytes);
        if total > MAX_BASE_SNAPSHOT_TOTAL_BYTES {
            return Err(GitError::SnapshotTooLarge(format!(
                "uncommitted changes exceed the {} MiB bound",
                mib(MAX_BASE_SNAPSHOT_TOTAL_BYTES)
            )));
        }
    }
    Ok(u32::try_from(entries.len()).unwrap_or(u32::MAX))
}

/// A merge or rebase in progress must be finished by the user: `add -A`
/// would otherwise stage the conflicted files with their markers.
fn ensure_no_unmerged_paths(repository: &Path) -> GitResult<()> {
    // Reads the user's index. Nothing is written.
    if run_git_bytes(repository, &["ls-files", "--unmerged", "-z"])?.is_empty() {
        Ok(())
    } else {
        Err(GitError::UnmergedPaths)
    }
}

/// Copy the repository index so staging keeps its stat cache (only changed
/// files are rehashed) and its skip-worktree bits (a sparse checkout keeps
/// the files it does not have on disk instead of recording them as deleted).
fn copy_repository_index(repository: &Path, destination: &Path) -> GitResult<()> {
    let path = repository.join(run_git(repository, &["rev-parse", "--git-path", "index"])?);
    std::fs::copy(&path, destination).map_err(|error| GitError::Git {
        args: vec!["rev-parse".into()],
        stderr: format!(
            "cannot read the repository index at {}: {error}",
            path.display()
        ),
    })?;
    Ok(())
}

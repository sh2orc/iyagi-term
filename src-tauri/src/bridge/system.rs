//! System probe for execution profiles (`04-ui.md` §5): PATH/install-dir
//! scan for the three supported CLIs plus a hardened `--version` query.
//!
//! The version query never accepts user argv: `arg` is a fixed per-CLI
//! constant supplied by the bridge, validated against a strict whitelist,
//! and `program` must be an absolute native executable. Windows script
//! shims (`.cmd`/`.bat`/`.ps1`) are listed for profiles but never
//! version-queried (`02-runner.md` §3); npm's extensionless POSIX shim is
//! never started at all (CreateProcess cannot run it).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

use super::connection::BridgeError;

/// Verified version query limits (04 §5).
pub const VERSION_QUERY_TIMEOUT: Duration = Duration::from_secs(2);
pub const VERSION_OUTPUT_CAP: usize = 8 * 1024;
const GIT_QUERY_TIMEOUT: Duration = Duration::from_secs(1);
/// How much of the output is scanned for the version line.
const VERSION_SCAN_BYTES: usize = 256;
const VERSION_ARG_MAX: usize = 64;

/// `CREATE_NO_WINDOW`: the release app is a GUI-subsystem process, so a
/// console child spawned without this flag pops a visible console window.
#[cfg(windows)]
pub const CREATE_NO_WINDOW_FLAG: u32 = 0x0800_0000;

/// Hide the console of a helper child (git, version probes, `wsl -l`,
/// `codex app-server`). No-op off Windows.
pub fn hide_console(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        command.creation_flags(CREATE_NO_WINDOW_FLAG);
    }
    command
}

/// Same for synchronous std commands.
pub fn hide_console_std(command: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW_FLAG);
    }
    command
}

/// One discovered CLI executable. Field names are camelCase on the wire to
/// match `src/features/bridge/systemProbe.ts` exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliCandidate {
    /// Absolute executable path as discovered.
    pub program: String,
    /// `codex` | `claude` | `opencode` (only the three are scanned).
    pub kind: &'static str,
    /// Symlink-resolved canonical target (verbatim `\\?\` prefix stripped).
    pub resolved_target: Option<String>,
    /// `native` | `symlink` | `cmd-shim` | `script`, when determinable.
    pub install_form: Option<String>,
}

/// Current repository branch for one terminal working directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranch {
    pub name: String,
    pub detached: bool,
    /// Repository top-level *working* directory (the ancestor holding
    /// `.git`) — for a linked worktree this is the worktree folder
    /// itself, not the main repository it links to. Groups terminal tabs
    /// by project (`gitBranch.ts`'s `GitBranchInfo.top_level`).
    #[serde(rename = "top_level")]
    pub top_level: String,
}

/// Branch lookups are polled by every pane header (`TerminalPane.tsx`, 4 s
/// cadence). The old implementation forked `git` once or twice per poll per
/// pane — hundreds of thousands of process spawns a day for an app left
/// running. Now the common case is a couple of `stat`s plus one tiny read
/// of `.git/HEAD`; `git` itself is only consulted for layouts whose HEAD
/// cannot be parsed. A small TTL cache collapses panes that share a cwd.
const GIT_CACHE_TTL: Duration = Duration::from_secs(3);
/// Bounded so the cache can never become the next leak.
const GIT_CACHE_MAX_ENTRIES: usize = 64;
/// `.git/HEAD` is one line; anything bigger is not a HEAD we understand.
const GIT_HEAD_MAX_BYTES: u64 = 4096;
/// Ancestor walk bound (a path of 4096 bytes cannot have more components).
const GIT_WALK_MAX_DEPTH: usize = 256;

type GitCache = HashMap<PathBuf, (Instant, Option<GitBranch>)>;
static GIT_CACHE: LazyLock<Mutex<GitCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Resolve a terminal cwd to its current branch without invoking a shell.
/// Non-repositories and unavailable Git installations are normal `None`s.
pub async fn git_branch(cwd: &str) -> Result<Option<GitBranch>, BridgeError> {
    let path = Path::new(cwd);
    if cwd.is_empty() || cwd.len() > 4096 || !path.is_absolute() || !path.is_dir() {
        return Err(BridgeError::invalid_argument(
            "cwd must be an existing absolute directory",
        ));
    }
    if let Some(hit) = git_cache_get(path, Instant::now()) {
        return Ok(hit);
    }
    let branch = match resolve_git_dir(path) {
        // Not inside a repository: answer without spawning anything.
        None => None,
        Some((top_level, git_dir)) => match read_head_branch(&git_dir, &top_level) {
            Some(branch) => Some(branch),
            // Unusual layouts (unparseable HEAD, packed symbolic refs the
            // daemon cannot follow, ...) fall back to asking git itself.
            None => spawn_git_branch(path, &top_level).await,
        },
    };
    git_cache_put(path.to_path_buf(), branch.clone(), Instant::now());
    Ok(branch)
}

fn git_cache_get(path: &Path, now: Instant) -> Option<Option<GitBranch>> {
    let cache = GIT_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .get(path)
        .filter(|(at, _)| now.duration_since(*at) < GIT_CACHE_TTL)
        .map(|(_, branch)| branch.clone())
}

fn git_cache_put(path: PathBuf, branch: Option<GitBranch>, now: Instant) {
    let mut cache = GIT_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Expired entries first; if still over the cap, evict the oldest one so
    // the map is bounded by the number of distinct cwds polled at once.
    cache.retain(|_, (at, _)| now.duration_since(*at) < GIT_CACHE_TTL);
    if cache.len() >= GIT_CACHE_MAX_ENTRIES {
        if let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, (at, _))| *at)
            .map(|(key, _)| key.clone())
        {
            cache.remove(&oldest);
        }
    }
    cache.insert(path, (now, branch));
}

/// Walk up from `cwd` to the enclosing `.git` directory, returning both
/// the repository's top-level *working* directory (the ancestor holding
/// `.git`) and the resolved git directory that owns HEAD. A `.git` *file*
/// (worktrees, submodules) holds `gitdir: <path>` pointing at the real
/// directory that owns HEAD; for that case the top level is the worktree
/// folder itself, not the main repository the pointer leads to.
fn resolve_git_dir(cwd: &Path) -> Option<(PathBuf, PathBuf)> {
    for dir in cwd.ancestors().take(GIT_WALK_MAX_DEPTH) {
        let dot_git = dir.join(".git");
        let Ok(meta) = std::fs::metadata(&dot_git) else {
            continue;
        };
        if meta.is_dir() {
            return Some((dir.to_path_buf(), dot_git));
        }
        if meta.is_file() && meta.len() <= GIT_HEAD_MAX_BYTES {
            let content = std::fs::read_to_string(&dot_git).ok()?;
            let target = content.trim().strip_prefix("gitdir:")?.trim();
            if target.is_empty() {
                return None;
            }
            let resolved = dir.join(target);
            return resolved.is_dir().then_some((dir.to_path_buf(), resolved));
        }
        return None;
    }
    None
}

/// Parse `<git_dir>/HEAD`: `ref: refs/heads/<name>` is a branch, a bare
/// object id is a detached HEAD (shown abbreviated like `rev-parse --short`).
fn read_head_branch(git_dir: &Path, top_level: &Path) -> Option<GitBranch> {
    let head_path = git_dir.join("HEAD");
    let meta = std::fs::metadata(&head_path).ok()?;
    if !meta.is_file() || meta.len() > GIT_HEAD_MAX_BYTES {
        return None;
    }
    let content = std::fs::read_to_string(&head_path).ok()?;
    let parsed = parse_head(&content)?;
    Some(GitBranch {
        name: parsed.name,
        detached: parsed.detached,
        top_level: top_level.to_string_lossy().into_owned(),
    })
}

/// The parts `parse_head` can determine from `HEAD`'s content alone, before
/// `read_head_branch` attaches the repository's top-level directory.
#[derive(Debug, PartialEq, Eq)]
struct ParsedHead {
    name: String,
    detached: bool,
}

fn parse_head(content: &str) -> Option<ParsedHead> {
    let line = content.lines().next()?.trim();
    if let Some(reference) = line.strip_prefix("ref:") {
        let reference = reference.trim();
        let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        if name.is_empty() || name.chars().any(char::is_control) {
            return None;
        }
        return Some(ParsedHead {
            name: name.to_string(),
            detached: false,
        });
    }
    let is_object_id = matches!(line.len(), 40 | 64) && line.bytes().all(|b| b.is_ascii_hexdigit());
    is_object_id.then(|| ParsedHead {
        name: line[..7].to_ascii_lowercase(),
        detached: true,
    })
}

/// Fallback for layouts `read_head_branch` cannot interpret: ask git.
async fn spawn_git_branch(cwd: &Path, top_level: &Path) -> Option<GitBranch> {
    if let Some(head) = run_git(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await {
        return Some(GitBranch {
            name: head,
            detached: false,
            top_level: top_level.to_string_lossy().into_owned(),
        });
    }
    run_git(cwd, &["rev-parse", "--short", "HEAD"])
        .await
        .map(|commit| GitBranch {
            name: commit,
            detached: true,
            top_level: top_level.to_string_lossy().into_owned(),
        })
}

async fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    hide_console(&mut command);
    let output = timeout(GIT_QUERY_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() || output.stdout.len() > 1024 {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.lines().next()?.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return None;
    }
    Some(value.to_string())
}

const CLI_NAMES: [(&str, &str); 3] = [
    ("codex", "codex"),
    ("claude", "claude"),
    ("opencode", "opencode"),
];

/// Windows lookup order: a real `.exe`, then the `cmd.exe`-runnable shims,
/// and only last the extensionless file — npm writes `codex` (POSIX shell
/// script for Git Bash), `codex.cmd` and `codex.ps1` side by side, and the
/// bare name must never shadow the runnable forms.
#[cfg(windows)]
const EXE_SUFFIXES: [&str; 4] = [".exe", ".cmd", ".bat", ""];
#[cfg(not(windows))]
const EXE_SUFFIXES: [&str; 1] = [""];

/// Scan PATH plus the common install dirs for the supported CLIs.
/// Deterministic order: PATH order, then install dirs, then name order.
pub fn scan_clis() -> Vec<CliCandidate> {
    let mut dirs = path_dirs();
    dirs.extend(extra_install_dirs());
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for dir in dirs {
        for (name, kind) in CLI_NAMES {
            for suffix in EXE_SUFFIXES {
                let file = dir.join(format!("{name}{suffix}"));
                let Ok(meta) = std::fs::metadata(&file) else {
                    continue;
                };
                if !meta.is_file() {
                    continue;
                }
                let program = file.to_string_lossy().to_string();
                if !seen.insert(program.to_ascii_lowercase()) {
                    continue;
                }
                out.push(describe(&file, kind));
            }
        }
    }
    out
}

fn describe(path: &Path, kind: &'static str) -> CliCandidate {
    let install_form = install_form(path);
    // npm's `claude.cmd`/`codex.cmd` is a launcher for `node <cli.js>`; the
    // script is the target the UI can turn into an interpreter profile
    // (02 §3), so it — not the shim's own path — is the resolved target.
    let resolved_target = match install_form.as_deref() {
        Some("cmd-shim") => cmd_shim_script_target(path).or_else(|| resolve_target(path)),
        _ => resolve_target(path),
    };
    CliCandidate {
        program: path.to_string_lossy().to_string(),
        kind,
        resolved_target,
        install_form,
    }
}

/// Bytes of a `.cmd` shim worth scanning; real npm shims are < 1 KiB.
const CMD_SHIM_SCAN_BYTES: u64 = 16 * 1024;

/// Script an npm cmd-shim launches, when it exists on disk (canonical,
/// verbatim prefix stripped like every other resolved target).
fn cmd_shim_script_target(shim: &Path) -> Option<String> {
    use std::io::Read as _;
    let mut body = Vec::new();
    std::fs::File::open(shim)
        .ok()?
        .take(CMD_SHIM_SCAN_BYTES)
        .read_to_end(&mut body)
        .ok()?;
    let script = npm_cmd_shim_target(shim, &String::from_utf8_lossy(&body))?;
    if !script.is_file() {
        return None;
    }
    resolve_target(&script)
}

/// Parse an npm `cmd-shim` body. The shim ends its launch line with the
/// package script anchored at its own directory, e.g.
/// `"%_prog%"  "%dp0%\node_modules\@anthropic-ai\claude-code\cli.js" %*`
/// (older shims spell the anchor `%~dp0`). Returns that script path with
/// the anchor replaced by the shim's parent directory, always in Windows
/// form; `None` for a hand-written `.cmd` or a shim that does not run a
/// `.js`/`.mjs`/`.cjs` script.
fn npm_cmd_shim_target(shim_path: &Path, body: &str) -> Option<PathBuf> {
    const ANCHORS: [&str; 4] = ["%dp0%\\", "%~dp0\\", "%dp0%/", "%~dp0/"];
    const SCRIPT_EXTENSIONS: [&str; 3] = [".js", ".mjs", ".cjs"];
    // Split on either separator from the string form: the shim path is
    // Windows-form even when this runs (tests) on a Unix host, where
    // `Path::parent` would not see `\\` as a separator.
    let shim_text = shim_path.to_string_lossy();
    let split_at = shim_text.rfind(['\\', '/'])?;
    let dir = shim_text[..split_at].trim_end_matches(['\\', '/']);
    if dir.is_empty() {
        return None;
    }
    for line in body.lines() {
        let mut rest = line;
        while let Some(start) = rest.find('"') {
            let after = &rest[start + 1..];
            let Some(end) = after.find('"') else {
                break;
            };
            let quoted = &after[..end];
            rest = &after[end + 1..];
            let Some(relative) = ANCHORS
                .iter()
                .find_map(|anchor| quoted.strip_prefix(anchor))
            else {
                continue;
            };
            let lower = relative.to_ascii_lowercase();
            if relative.is_empty()
                || !SCRIPT_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
                || relative.contains('%')
            {
                continue;
            }
            let relative = relative.replace('/', "\\");
            return Some(PathBuf::from(format!("{dir}\\{relative}")));
        }
    }
    None
}

/// Follow symlinks (≤ 8 hops) and canonicalize; strips the Windows verbatim
/// prefix so the UI shows a user-readable path.
fn resolve_target(path: &Path) -> Option<String> {
    let mut current = path.to_path_buf();
    for _ in 0..8 {
        match std::fs::read_link(&current) {
            Ok(next) => {
                current = if next.is_absolute() {
                    next
                } else {
                    current.parent().map(|p| p.join(&next)).unwrap_or(next)
                };
            }
            Err(_) => break,
        }
    }
    let canonical = std::fs::canonicalize(&current).unwrap_or(current);
    Some(
        strip_verbatim_prefix(&canonical)
            .to_string_lossy()
            .to_string(),
    )
}

#[cfg(windows)]
fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(stripped) => PathBuf::from(stripped),
        None => PathBuf::from(text.as_ref()),
    }
}

#[cfg(not(windows))]
fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    path.to_path_buf()
}

fn install_form(path: &Path) -> Option<String> {
    let is_link = std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if is_link {
        return Some("symlink".into());
    }
    #[cfg(windows)]
    {
        Some(install_form_for_extension(path.extension().and_then(|e| e.to_str())).into())
    }
    #[cfg(not(windows))]
    {
        Some("native".into())
    }
}

/// Windows install form by extension. Only a real `.exe` is `native`; an
/// extensionless file is npm's POSIX-shell shim (`script`), which
/// CreateProcess cannot start — it used to be reported as `native` and was
/// then handed to `Command::new`, failing with ERROR_BAD_EXE_FORMAT.
#[cfg_attr(not(windows), allow(dead_code))]
fn install_form_for_extension(ext: Option<&str>) -> &'static str {
    match ext.map(str::to_ascii_lowercase).as_deref() {
        Some("exe") => "native",
        Some("cmd") | Some("bat") => "cmd-shim",
        _ => "script",
    }
}

/// Rank for spawning a candidate directly from the bridge: native
/// executables and symlinks (scan order decides between them, as before),
/// then a `.cmd`/`.bat` shim (std runs those through `cmd.exe`), never a
/// POSIX `script` shim.
fn spawn_rank(candidate: &CliCandidate) -> Option<u8> {
    match candidate.install_form.as_deref() {
        Some("native") | Some("symlink") | None => Some(0),
        Some("cmd-shim") => Some(1),
        _ => None,
    }
}

/// First candidate of `kind` (scan order) among the best spawnable rank.
pub fn spawnable_program(candidates: &[CliCandidate], kind: &str) -> Option<String> {
    let mut best: Option<(u8, &CliCandidate)> = None;
    for candidate in candidates.iter().filter(|c| c.kind == kind) {
        let Some(rank) = spawn_rank(candidate) else {
            continue;
        };
        if best.is_none_or(|(current, _)| rank < current) {
            best = Some((rank, candidate));
        }
    }
    best.map(|(_, candidate)| candidate.program.clone())
}

/// Locate a bare program name (e.g. `node`) on PATH plus the platform's
/// common install dirs, honouring the `EXE_SUFFIXES` lookup order. The UI
/// uses it to build the interpreter form `node <cli.js>` for npm shims
/// (02 §3); it never probes an arbitrary path (see `is_bare_program_name`).
pub fn locate_program(name: &str) -> Option<String> {
    if !is_bare_program_name(name) {
        return None;
    }
    let mut dirs = path_dirs();
    dirs.extend(extra_install_dirs());
    #[cfg(windows)]
    {
        // The Node installer registers PATH for new shells only; a GUI app
        // started from the Start menu may still see the pre-install PATH.
        for var in ["ProgramFiles", "ProgramW6432"] {
            if let Some(pf) = std::env::var_os(var) {
                dirs.push(PathBuf::from(pf).join("nodejs"));
            }
        }
    }
    locate_in_dirs(name, &dirs, |p| {
        std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
    })
}

fn locate_in_dirs(name: &str, dirs: &[PathBuf], is_file: impl Fn(&Path) -> bool) -> Option<String> {
    for dir in dirs {
        for suffix in EXE_SUFFIXES {
            let file = dir.join(format!("{name}{suffix}"));
            if is_file(&file) {
                return Some(file.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// A bare file name: no separators, no drive/UNC, no leading dot, bounded.
pub fn is_bare_program_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn path_dirs() -> Vec<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .filter(|p| !p.as_os_str().is_empty())
        .collect()
}

fn extra_install_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    #[cfg(windows)]
    {
        // npm global bin (default shims location) + user local bin.
        if let Some(appdata) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(&appdata).join("npm"));
        }
        if let Some(home) = std::env::var_os("USERPROFILE") {
            dirs.push(PathBuf::from(&home).join(".local").join("bin"));
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(home) = std::env::var_os("HOME") {
            dirs.extend(unix_user_install_dirs(&PathBuf::from(home)));
        }
        dirs.push(PathBuf::from("/usr/local/bin"));
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        // Ubuntu: snap-installed CLIs live outside a dock-launched app's PATH.
        dirs.push(PathBuf::from("/snap/bin"));
    }
    dirs
}

#[cfg(not(windows))]
fn unix_user_install_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![
        home.join(".local/bin"),
        home.join(".opencode/bin"),
        home.join(".volta/bin"),
        home.join(".asdf/shims"),
        home.join(".bun/bin"),
        home.join("Library/pnpm"),
        // Linux conventions: npm's user prefix and pnpm's XDG home.
        home.join(".npm-global/bin"),
        home.join(".local/share/pnpm"),
    ];
    dirs.extend(versioned_bin_dirs(&home.join(".nvm/versions/node"), "bin"));
    dirs.extend(versioned_bin_dirs(
        &home.join(".local/share/fnm/node-versions"),
        "installation/bin",
    ));
    dirs
}

#[cfg(not(windows))]
fn versioned_bin_dirs(root: &Path, suffix: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut versions: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    versions.sort_by_key(|path| std::cmp::Reverse(version_key(path)));
    versions
        .into_iter()
        .map(|version| version.join(suffix))
        .collect()
}

#[cfg(not(windows))]
fn version_key(path: &Path) -> Vec<u64> {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .trim_start_matches('v')
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

/// Version-argument whitelist: a fixed flag like `--version`. Rejects shell
/// metacharacters, whitespace, control bytes, empty and oversized values —
/// defense in depth on top of argv-based spawning (no shell involved).
pub fn is_safe_version_arg(arg: &str) -> bool {
    !arg.is_empty()
        && arg.len() <= VERSION_ARG_MAX
        && arg.bytes().all(|b| {
            matches!(b, b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'-' | b'_' | b'=' | b'.' | b':' | b'/')
        })
}

/// Validate the program path for the version query: absolute, traversal-free,
/// existing, and a native executable (script shims refused — 02 §3).
pub fn validate_program(program: &str) -> Result<PathBuf, BridgeError> {
    let invalid = |m: &str| BridgeError::invalid_argument(m);
    if program.trim().is_empty() {
        return Err(invalid("program is empty"));
    }
    let path = Path::new(program);
    if !path.is_absolute() {
        return Err(invalid("program must be an absolute path"));
    }
    if program.contains("..") {
        return Err(invalid("program must not contain traversal segments"));
    }
    #[cfg(windows)]
    if let Some(ext) = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
    {
        if matches!(
            ext.as_str(),
            "cmd" | "bat" | "ps1" | "lnk" | "com" | "scr" | "pif" | "js" | "vbs" | "py"
        ) {
            return Err(invalid("script shims cannot be version-queried"));
        }
    }
    if !path.is_file() {
        return Err(invalid("program does not exist"));
    }
    Ok(path.to_path_buf())
}

/// Run the fixed version query (2s timeout, 8 KiB stdout cap, stderr
/// discarded) and extract the first semver-ish token from the first line.
/// Returns `Ok(None)` when the program cannot be executed or reports no
/// recognizable version (04 §5: 검증 안 되면 null).
pub async fn query_version(program: &str, arg: &str) -> Result<Option<String>, BridgeError> {
    let program = validate_program(program)?;
    if !is_safe_version_arg(arg) {
        return Err(BridgeError::invalid_argument("version argument rejected"));
    }

    let mut command = Command::new(&program);
    command
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    hide_console(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        // Spawn failure (permissions, bitness, missing DLLs) is an unknown
        // version, not a bridge error.
        Err(_) => return Ok(None),
    };
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let read = async {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            match stdout.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.len() >= VERSION_OUTPUT_CAP {
                        let _ = child.start_kill();
                        break;
                    }
                }
            }
        }
        buf
    };
    let buf = match timeout(VERSION_QUERY_TIMEOUT, read).await {
        Ok(buf) => buf,
        Err(_) => {
            // Kill AND reap: a killed-but-unwaited child lingers as a zombie
            // until tokio's orphan reaper happens to run.
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Ok(None);
        }
    };
    let _ = child.wait().await;
    let text = String::from_utf8_lossy(&buf);
    Ok(first_line_version(&text))
}

/// Extract a `X.Y` / `X.Y.Z` token (optional leading `v`/`V`) from the first
/// line of `text`. Hand-rolled — no regex dependency.
pub fn first_line_version(text: &str) -> Option<String> {
    let first_line_end = text.find('\n').unwrap_or(text.len());
    let scan = &text[..first_line_end.min(text.len())];
    extract_version(scan)
}

pub fn extract_version(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let scan_len = bytes.len().min(VERSION_SCAN_BYTES);
    let mut i = 0;
    while i < scan_len {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        // Reject digit runs embedded in identifiers ("x86_64", "256color").
        // A lone v/V prefix is allowed ("v2.4.1") and folded into the match.
        if i > 0 {
            let prev = bytes[i - 1];
            let prev_is_v = prev == b'v' || prev == b'V';
            if (prev.is_ascii_alphanumeric() && !prev_is_v) || prev == b'.' {
                i += 1;
                continue;
            }
        }
        let mut cursor = i;
        let mut end = i;
        let mut groups = 0;
        while groups < 3 {
            let mut k = cursor;
            while k < scan_len && bytes[k].is_ascii_digit() {
                k += 1;
            }
            if k == cursor {
                break;
            }
            groups += 1;
            end = k;
            if k < scan_len && bytes[k] == b'.' && k + 1 < scan_len && bytes[k + 1].is_ascii_digit()
            {
                cursor = k + 1;
            } else {
                break;
            }
        }
        if groups >= 2 {
            let mut start = i;
            if i > 0 && (bytes[i - 1] == b'v' || bytes[i - 1] == b'V') {
                let before_v = i >= 2 && bytes[i - 2].is_ascii_alphanumeric();
                if !before_v {
                    start = i - 1;
                }
            }
            return Some(text[start..end].to_string());
        }
        i = end.max(i + 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_program_name_rejects_paths_and_dotfiles() {
        assert!(is_bare_program_name("node"));
        assert!(is_bare_program_name("node.exe"));
        assert!(is_bare_program_name("python3.12"));
        assert!(!is_bare_program_name(""));
        assert!(!is_bare_program_name(".hidden"));
        assert!(!is_bare_program_name("C:\\nodejs\\node.exe"));
        assert!(!is_bare_program_name("../node"));
        assert!(!is_bare_program_name("bin/node"));
        assert!(!is_bare_program_name("no de"));
        assert!(!is_bare_program_name(&"x".repeat(65)));
        assert!(locate_program("../node").is_none());
    }

    #[test]
    fn locate_in_dirs_walks_path_order_then_suffix_order() {
        let dirs = vec![PathBuf::from("first"), PathBuf::from("second")];
        let first_suffix = EXE_SUFFIXES[0];
        let last_suffix = EXE_SUFFIXES[EXE_SUFFIXES.len() - 1];
        let in_second = PathBuf::from("second").join(format!("node{first_suffix}"));
        let in_first = PathBuf::from("first").join(format!("node{last_suffix}"));
        // Only the second dir has it.
        let found = locate_in_dirs("node", &dirs, |p| p == in_second);
        assert_eq!(found.as_deref(), Some(in_second.to_string_lossy().as_ref()));
        // Nothing anywhere.
        assert!(locate_in_dirs("node", &dirs, |_| false).is_none());
        // PATH order wins over suffix order: `first/node<last>` beats
        // `second/node<first>` (on Windows: a bare `node` in an earlier dir
        // still loses to nothing — but never to a later dir's `.exe`).
        let found = locate_in_dirs("node", &dirs, |p| p == in_first || p == in_second);
        assert_eq!(found.as_deref(), Some(in_first.to_string_lossy().as_ref()));
    }

    #[cfg(not(windows))]
    #[test]
    fn user_install_dirs_cover_cli_managers_and_prefer_newer_node() {
        let home = tempfile::tempdir().unwrap();
        for version in ["v9.3.0", "v24.11.1", "v20.0.0"] {
            std::fs::create_dir_all(home.path().join(".nvm/versions/node").join(version)).unwrap();
        }
        let dirs = unix_user_install_dirs(home.path());
        assert!(dirs.contains(&home.path().join(".opencode/bin")));
        assert!(dirs.contains(&home.path().join(".volta/bin")));
        assert!(dirs.contains(&home.path().join(".npm-global/bin")));
        assert!(dirs.contains(&home.path().join(".local/share/pnpm")));
        let nvm: Vec<_> = dirs
            .iter()
            .filter(|path| path.to_string_lossy().contains(".nvm/versions/node"))
            .collect();
        assert!(nvm[0].to_string_lossy().contains("v24.11.1"));
        assert!(nvm[2].to_string_lossy().contains("v9.3.0"));
    }

    #[test]
    fn version_arg_whitelist_rejects_metacharacters_and_empties() {
        assert!(is_safe_version_arg("--version"));
        assert!(is_safe_version_arg("-V"));
        assert!(is_safe_version_arg("version"));
        assert!(is_safe_version_arg("--output=json.patch-2"));

        for bad in [
            "",
            "a b",
            "; rm -rf /",
            "a&b",
            "a|b",
            "`x`",
            "$(x)",
            "a>b",
            "a<b",
            "'quoted'",
            "\"quoted\"",
            "line\nbreak",
            "tab\tchar",
            "日本語",
            "x".repeat(VERSION_ARG_MAX + 1).as_str(),
        ] {
            assert!(!is_safe_version_arg(bad), "expected rejection: {bad:?}");
        }
    }

    #[test]
    fn extract_version_finds_semver_tokens() {
        assert_eq!(
            extract_version("codex-cli 0.20.0\nbuild info"),
            Some("0.20.0".into())
        );
        assert_eq!(extract_version("1.2.3"), Some("1.2.3".into()));
        assert_eq!(extract_version("v2.4.1"), Some("v2.4.1".into()));
        assert_eq!(
            extract_version("claude version 1.0.32 (stable)"),
            Some("1.0.32".into())
        );
        assert_eq!(extract_version("opencode 0.9 x64"), Some("0.9".into()));
        assert_eq!(
            extract_version("ver 10.0.19045.3208"),
            Some("10.0.19045".into())
        );
    }

    #[test]
    fn extract_version_rejects_non_versions() {
        assert_eq!(extract_version("xterm-256color"), None);
        assert_eq!(extract_version("x86_64-pc-windows-gnu"), None);
        assert_eq!(extract_version("no digits here"), None);
        assert_eq!(extract_version(""), None);
        assert_eq!(first_line_version("0.1.0\n0.2.0"), Some("0.1.0".into()));
    }

    #[test]
    fn program_validation_requires_absolute_native_executable() {
        assert!(validate_program("codex").is_err());
        assert!(validate_program("").is_err());
        assert!(validate_program("../codex").is_err());
        if cfg!(windows) {
            assert!(validate_program("C:/tools/codex.cmd").is_err());
            assert!(validate_program("C:/tools/codex.ps1").is_err());
        }
        // Existing, absolute, non-shim path passes shape checks (existence
        // is checked against the real filesystem: use the test binary itself).
        let exe = std::env::current_exe().unwrap();
        let validated = validate_program(&exe.to_string_lossy()).unwrap();
        assert!(validated.is_absolute());
    }

    #[tokio::test]
    async fn query_version_reports_none_for_garbage_program() {
        // Absolute + existing file that is not executable-ish: an exe on
        // Windows, a text file we create ourselves.
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("not-a-cli.txt");
        std::fs::write(&program, b"hello").unwrap();
        let version = query_version(&program.to_string_lossy(), "--version")
            .await
            .unwrap();
        assert_eq!(version, None);
    }

    #[test]
    fn cli_candidate_serializes_camel_case() {
        let candidate = CliCandidate {
            program: "C:/x/codex.exe".into(),
            kind: "codex",
            resolved_target: Some("C:/x/codex.exe".into()),
            install_form: Some("native".into()),
        };
        let json = serde_json::to_value(&candidate).unwrap();
        assert!(json["resolvedTarget"].is_string());
        assert!(json["installForm"].is_string());
        assert_eq!(json["kind"], "codex");
    }

    #[test]
    fn install_form_by_extension_never_calls_a_bare_npm_shim_native() {
        assert_eq!(install_form_for_extension(Some("exe")), "native");
        assert_eq!(install_form_for_extension(Some("EXE")), "native");
        assert_eq!(install_form_for_extension(Some("cmd")), "cmd-shim");
        assert_eq!(install_form_for_extension(Some("bat")), "cmd-shim");
        assert_eq!(install_form_for_extension(Some("ps1")), "script");
        // npm's `codex` next to `codex.cmd`: a POSIX shell script.
        assert_eq!(install_form_for_extension(None), "script");
    }

    const NPM_CLAUDE_SHIM: &str = concat!(
        "@ECHO off\r\n",
        "GOTO start\r\n",
        ":find_dp0\r\n",
        "SET dp0=%~dp0\r\n",
        "EXIT /b\r\n",
        ":start\r\n",
        "SETLOCAL\r\n",
        "CALL :find_dp0\r\n",
        "IF EXIST \"%dp0%\\node.exe\" (\r\n",
        "  SET \"_prog=%dp0%\\node.exe\"\r\n",
        ") ELSE (\r\n",
        "  SET \"_prog=node\"\r\n",
        "  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n",
        ")\r\n",
        "endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  ",
        "\"%dp0%\\node_modules\\@anthropic-ai\\claude-code\\cli.js\" %*\r\n",
    );

    #[test]
    fn npm_cmd_shim_target_resolves_the_launched_script_at_the_shim_dir() {
        let shim = Path::new(r"C:\Users\dev\AppData\Roaming\npm\claude.cmd");
        let target = npm_cmd_shim_target(shim, NPM_CLAUDE_SHIM).unwrap();
        assert_eq!(
            target.to_string_lossy(),
            r"C:\Users\dev\AppData\Roaming\npm\node_modules\@anthropic-ai\claude-code\cli.js"
        );
        // Older cmd-shim: `%~dp0` anchor, forward slashes, `.mjs`, no
        // `_prog` indirection — still the first anchored script wins.
        let legacy = "@IF EXIST \"%~dp0\\node.exe\" (\r\n  \"%~dp0\\node.exe\"  \"%~dp0/node_modules/codex/bin/codex.mjs\" %*\r\n) ELSE (\r\n  node  \"%~dp0/node_modules/codex/bin/codex.mjs\" %*\r\n)\r\n";
        let target = npm_cmd_shim_target(Path::new(r"D:\npm\codex.cmd"), legacy).unwrap();
        assert_eq!(
            target.to_string_lossy(),
            r"D:\npm\node_modules\codex\bin\codex.mjs"
        );
    }

    #[test]
    fn npm_cmd_shim_target_ignores_hand_written_and_non_script_shims() {
        let shim = Path::new(r"C:\npm\claude.cmd");
        // Hand-written launcher: absolute path, no `%dp0%` anchor.
        assert_eq!(
            npm_cmd_shim_target(shim, "@echo off\r\nnode \"C:\\tools\\cli.js\" %*\r\n"),
            None
        );
        // Shim that launches a native exe: the UI registers the exe instead.
        assert_eq!(
            npm_cmd_shim_target(shim, "@\"%dp0%\\bin\\claude.exe\" %*\r\n"),
            None
        );
        // Unexpanded variable inside the anchored path, empty body, no parent.
        assert_eq!(
            npm_cmd_shim_target(shim, "\"%dp0%\\%name%\\cli.js\"\r\n"),
            None
        );
        assert_eq!(npm_cmd_shim_target(shim, ""), None);
        assert_eq!(
            npm_cmd_shim_target(Path::new("claude.cmd"), NPM_CLAUDE_SHIM),
            None
        );
    }

    #[test]
    fn cmd_shim_script_target_requires_the_script_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("claude.cmd");
        std::fs::write(&shim, NPM_CLAUDE_SHIM).unwrap();
        // The parsed path is Windows-form; on this host it only resolves
        // when the file exists — it does not, so no target is reported.
        assert_eq!(cmd_shim_script_target(&shim), None);
    }

    fn candidate(program: &str, kind: &'static str, form: Option<&str>) -> CliCandidate {
        CliCandidate {
            program: program.into(),
            kind,
            resolved_target: None,
            install_form: form.map(str::to_string),
        }
    }

    #[test]
    fn spawnable_program_prefers_exe_then_cmd_shim_and_skips_posix_shims() {
        // npm layout on Windows, in the (old) scan order that listed the
        // bare shim first.
        let npm = [
            candidate(r"C:\npm\codex", "codex", Some("script")),
            candidate(r"C:\npm\codex.cmd", "codex", Some("cmd-shim")),
            candidate(r"C:\npm\claude.exe", "claude", Some("native")),
        ];
        assert_eq!(
            spawnable_program(&npm, "codex").as_deref(),
            Some(r"C:\npm\codex.cmd")
        );
        let with_exe = [
            candidate(r"C:\npm\codex.cmd", "codex", Some("cmd-shim")),
            candidate(r"C:\tools\codex.exe", "codex", Some("native")),
        ];
        assert_eq!(
            spawnable_program(&with_exe, "codex").as_deref(),
            Some(r"C:\tools\codex.exe")
        );
        // Only the POSIX shim: nothing the bridge can start.
        let only_shim = [candidate(r"C:\npm\codex", "codex", Some("script"))];
        assert_eq!(spawnable_program(&only_shim, "codex"), None);
        // Unix: symlink (nvm/volta) and native tie — scan order wins, as before.
        let unix = [
            candidate("/Users/me/.volta/bin/codex", "codex", Some("symlink")),
            candidate("/usr/local/bin/codex", "codex", Some("native")),
        ];
        assert_eq!(
            spawnable_program(&unix, "codex").as_deref(),
            Some("/Users/me/.volta/bin/codex")
        );
        assert_eq!(spawnable_program(&unix, "claude"), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_scan_prefers_exe_and_cmd_before_the_bare_name() {
        assert_eq!(EXE_SUFFIXES, [".exe", ".cmd", ".bat", ""]);
    }

    #[tokio::test]
    async fn git_branch_rejects_invalid_cwd_and_hides_non_repositories() {
        assert!(git_branch("relative/path").await.is_err());
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            git_branch(&dir.path().to_string_lossy()).await.unwrap(),
            None
        );
    }

    #[test]
    fn head_file_is_parsed_without_spawning_git() {
        let branch = parse_head("ref: refs/heads/feature/pane-header\n").unwrap();
        assert_eq!(branch.name, "feature/pane-header");
        assert!(!branch.detached);
        // Non-branch refs keep their full name rather than being hidden.
        assert_eq!(
            parse_head("ref: refs/remotes/origin/x").unwrap().name,
            "refs/remotes/origin/x"
        );
        let detached = parse_head("0123456789ABCDEF0123456789abcdef01234567\n").unwrap();
        assert_eq!(detached.name, "0123456");
        assert!(detached.detached);
        assert_eq!(parse_head(""), None);
        assert_eq!(parse_head("ref: "), None);
        assert_eq!(parse_head("not a head"), None);
    }

    #[tokio::test]
    async fn git_branch_reads_head_from_the_enclosing_repository() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        std::fs::write(repo.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let nested = repo.path().join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        let branch = git_branch(&nested.to_string_lossy())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(branch.name, "main");
        assert!(!branch.detached);
        assert_eq!(branch.top_level, repo.path().to_string_lossy());
    }

    #[tokio::test]
    async fn git_branch_follows_worktree_gitdir_pointer_files() {
        let main = tempfile::tempdir().unwrap();
        let wt_git_dir = main.path().join(".git").join("worktrees").join("wt1");
        std::fs::create_dir_all(&wt_git_dir).unwrap();
        std::fs::write(wt_git_dir.join("HEAD"), "ref: refs/heads/wt-branch\n").unwrap();
        let worktree = tempfile::tempdir().unwrap();
        std::fs::write(
            worktree.path().join(".git"),
            format!("gitdir: {}\n", wt_git_dir.display()),
        )
        .unwrap();
        let branch = git_branch(&worktree.path().to_string_lossy())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(branch.name, "wt-branch");
        assert_eq!(branch.top_level, worktree.path().to_string_lossy());
        assert_ne!(branch.top_level, main.path().to_string_lossy());
    }

    #[test]
    fn git_cache_is_bounded_and_expires() {
        let now = Instant::now();
        let base = tempfile::tempdir().unwrap();
        for i in 0..(GIT_CACHE_MAX_ENTRIES + 10) {
            git_cache_put(base.path().join(format!("cwd-{i}")), None, now);
        }
        let len = GIT_CACHE
            .lock()
            .unwrap()
            .keys()
            .filter(|key| key.starts_with(base.path()))
            .count();
        assert!(len <= GIT_CACHE_MAX_ENTRIES, "cache stays bounded ({len})");
        let path = base.path().join("cwd-hit");
        let branch = Some(GitBranch {
            name: "x".into(),
            detached: false,
            top_level: "/repo".into(),
        });
        git_cache_put(path.clone(), branch.clone(), now);
        assert_eq!(git_cache_get(&path, now), Some(branch));
        assert_eq!(git_cache_get(&path, now + GIT_CACHE_TTL), None, "expired");
    }

    #[test]
    fn git_branch_serializes_for_the_frontend() {
        let branch = GitBranch {
            name: "feature/pane-header".into(),
            detached: false,
            top_level: "/repo/project".into(),
        };
        let json = serde_json::to_value(branch).unwrap();
        assert_eq!(json["name"], "feature/pane-header");
        assert_eq!(json["detached"], false);
        assert_eq!(json["top_level"], "/repo/project");
        assert!(json.get("topLevel").is_none());
    }
}

// ---------------------------------------------------------------------------
// Shell profile discovery (셸 프로필, 04-ui §1): PowerShell/pwsh/CMD/WSL.

/// One detected shell for the profile picker. Wire names match
/// `src/features/terminal/shellProfiles.ts` `DetectedShell`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedShell {
    pub program: String,
    /// `powershell` | `pwsh` | `cmd` | `wsl` | `unix`.
    pub kind: &'static str,
    /// WSL distribution name (WSL entries only).
    pub distro: Option<String>,
    pub is_default: bool,
}

/// Detect available shells. Windows: well-known paths + `wsl -l -q`
/// (UTF-16LE output decoded explicitly). Unix: zsh/bash existence.
pub fn detect_shells() -> Vec<DetectedShell> {
    let mut out = Vec::new();
    if cfg!(windows) {
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let dirs = path_dirs();
        out.extend(windows_shell_candidates(&env, &dirs, &|p| p.is_file()));
        // WSL 배포판 목록: wsl.exe는 UTF-16LE로 출력한다. 동기 호출이지만
        // `list_wsl_distros_sync`가 2초 시한을 넘기면 죽이고 빈 목록을 준다.
        if let Some(wsl) = out.iter().find(|d| d.kind == "wsl").cloned() {
            for distro in list_wsl_distros_sync(&wsl.program) {
                out.push(DetectedShell {
                    program: wsl.program.clone(),
                    kind: "wsl",
                    distro: Some(distro),
                    is_default: false,
                });
            }
        }
    } else {
        #[cfg(unix)]
        {
            let default = account_login_shell().or_else(|| std::env::var("SHELL").ok());
            out.extend(unix_shell_candidates(
                default.as_deref(),
                &["/bin/zsh", "/bin/bash"],
                &|p| p.is_file(),
                &|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()),
            ));
        }
    }
    out
}

/// Merge the account login shell (or `$SHELL`) with the fixed candidates,
/// deduplicating by canonical path: on usrmerge'd Ubuntu the login shell is
/// `/usr/bin/zsh` while the fixed candidate `/bin/zsh` is the same file, and
/// string comparison used to yield two "zsh" profiles. The first spelling
/// wins so the wire keeps the account's own path.
#[cfg(unix)]
fn unix_shell_candidates(
    default: Option<&str>,
    fixed: &[&str],
    is_file: &dyn Fn(&Path) -> bool,
    canonical: &dyn Fn(&Path) -> PathBuf,
) -> Vec<DetectedShell> {
    let mut out: Vec<DetectedShell> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for program in default.into_iter().chain(fixed.iter().copied()) {
        let path = Path::new(program);
        if !path.is_absolute() || !is_file(path) {
            continue;
        }
        let key = canonical(path);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(DetectedShell {
            program: program.to_string(),
            kind: "unix",
            distro: None,
            is_default: default == Some(program),
        });
    }
    out
}

/// Read the account database, not the parent terminal's inherited SHELL.
#[cfg(unix)]
fn account_login_shell() -> Option<String> {
    let mut capacity = 4096;
    loop {
        let mut buffer = vec![0u8; capacity];
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        // SAFETY: all output pointers and the backing buffer are valid for
        // this call. Copy pw_shell before the backing buffer is dropped.
        let status = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE && capacity < 1024 * 1024 {
            capacity *= 2;
            continue;
        }
        if status != 0 || result.is_null() {
            return None;
        }
        let shell = unsafe { (*result).pw_shell };
        if shell.is_null() {
            return None;
        }
        let shell = unsafe { std::ffi::CStr::from_ptr(shell) }.to_str().ok()?;
        return (!shell.is_empty()).then(|| shell.to_owned());
    }
}

/// Well-known Windows shells resolved through the environment instead of
/// hard-coded `C:\…` paths: `%SystemRoot%` (Windows on another drive),
/// `%ComSpec%`, pwsh from `%ProgramFiles%`/`%ProgramW6432%`, the Store
/// alias dir `%LOCALAPPDATA%\Microsoft\WindowsApps`, then PATH. Exactly one
/// entry is marked default — Windows PowerShell, else pwsh, else cmd
/// (Windows Terminal 관례). Injected lookups keep the logic testable on any
/// host.
fn windows_shell_candidates(
    env: &dyn Fn(&str) -> Option<String>,
    path_dirs: &[PathBuf],
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<DetectedShell> {
    let system_root = env("SystemRoot")
        .or_else(|| env("windir"))
        .unwrap_or_else(|| r"C:\Windows".to_string());
    let system32 = Path::new(&system_root).join("System32");
    let on_path = |name: &str| {
        path_dirs
            .iter()
            .map(|dir| dir.join(name))
            .find(|p| exists(p))
    };
    let first_existing = |candidates: Vec<PathBuf>| candidates.into_iter().find(|p| exists(p));

    let mut out = Vec::new();
    let mut push = |kind: &'static str, program: Option<PathBuf>| {
        if let Some(program) = program {
            out.push(DetectedShell {
                program: program.to_string_lossy().into_owned(),
                kind,
                distro: None,
                is_default: false,
            });
        }
    };
    push(
        "powershell",
        first_existing(vec![system32
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe")])
        .or_else(|| on_path("powershell.exe")),
    );
    push(
        "cmd",
        env("ComSpec")
            .map(PathBuf::from)
            .filter(|p| exists(p))
            .or_else(|| first_existing(vec![system32.join("cmd.exe")])),
    );
    push(
        "wsl",
        first_existing(vec![system32.join("wsl.exe")]).or_else(|| on_path("wsl.exe")),
    );
    let mut pwsh = Vec::new();
    for base in ["ProgramFiles", "ProgramW6432"] {
        if let Some(dir) = env(base) {
            pwsh.push(
                Path::new(&dir)
                    .join("PowerShell")
                    .join("7")
                    .join("pwsh.exe"),
            );
            pwsh.push(
                Path::new(&dir)
                    .join("PowerShell")
                    .join("7-x86")
                    .join("pwsh.exe"),
            );
        }
    }
    if let Some(local) = env("LOCALAPPDATA") {
        pwsh.push(
            Path::new(&local)
                .join("Microsoft")
                .join("WindowsApps")
                .join("pwsh.exe"),
        );
    }
    push("pwsh", first_existing(pwsh).or_else(|| on_path("pwsh.exe")));

    if let Some(default) = ["powershell", "pwsh", "cmd"]
        .iter()
        .find_map(|kind| out.iter().position(|shell| shell.kind == *kind))
    {
        out[default].is_default = true;
    }
    out
}

/// Hard deadline for `wsl.exe -l -q`: a cold WSL service start or a hung
/// `wsl.exe` must not freeze the profile picker.
const WSL_LIST_TIMEOUT: Duration = Duration::from_secs(2);
const WSL_LIST_OUTPUT_CAP: u64 = 64 * 1024;

/// `wsl.exe -l -q` 실행·해석(UTF-16LE). 실패 또는 2초 시한 초과(kill) 시 빈 목록.
fn list_wsl_distros_sync(wsl: &str) -> Vec<String> {
    let mut command = std::process::Command::new(wsl);
    command
        .arg("-l")
        .arg("-q")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hide_console_std(&mut command);
    let Ok(mut child) = command.spawn() else {
        return Vec::new();
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Vec::new();
    };
    // stdout은 별도 스레드가 비운다 — 파이프가 가득 차도 아래 시한 루프가
    // 막히지 않는다(출력 상한 64 KiB).
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        let _ = (&mut stdout)
            .take(WSL_LIST_OUTPUT_CAP)
            .read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let deadline = Instant::now() + WSL_LIST_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    if !status.is_some_and(|status| status.success()) {
        return Vec::new();
    }
    match rx.recv_timeout(Duration::from_millis(500)) {
        Ok(bytes) => parse_wsl_list(&bytes),
        Err(_) => Vec::new(),
    }
}

/// Decode `wsl -l -q` bytes and keep the distro names (default marker and
/// NUL residue removed, ≤ 16 entries).
fn parse_wsl_list(bytes: &[u8]) -> Vec<String> {
    let raw = decode_wsl_output(bytes);
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        // 기본 배포판 표시자 제거 및 UTF-16 느낌표 잔여 정리
        .map(|line| line.replace(['\u{0}', '*'], "").trim().to_string())
        .filter(|line| !line.is_empty() && line.len() < 64)
        .take(16)
        .collect()
}

/// wsl.exe 출력은 UTF-16LE(리틀엔디안 BOM 포함) 또는 ANSI일 수 있다.
fn decode_wsl_output(bytes: &[u8]) -> String {
    // BOM FE FF(LE) 확인.
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    // UTF-16 정렬 추정: 홀수 위치 절반이 0이면 UTF-16LE로 간주.
    if bytes.len() >= 4 {
        let zeros = bytes.iter().skip(1).step_by(2).filter(|b| **b == 0).count();
        if zeros * 2 > bytes.len() / 2 {
            let units: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            return String::from_utf16_lossy(&units);
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod shell_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn unix_shells_dedupe_by_canonical_path_and_keep_the_login_spelling() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("zsh");
        std::fs::write(&real, b"#!/bin/sh\n").unwrap();
        let link = dir.path().join("zsh-link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let bash = dir.path().join("bash");
        std::fs::write(&bash, b"#!/bin/sh\n").unwrap();
        let link_s = link.to_string_lossy().into_owned();
        let real_s = real.to_string_lossy().into_owned();
        let bash_s = bash.to_string_lossy().into_owned();
        let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let shells = unix_shell_candidates(
            Some(&link_s),
            &[&real_s, &bash_s, "/definitely/missing/fish"],
            &|p| p.is_file(),
            &canonical,
        );
        let programs: Vec<&str> = shells.iter().map(|s| s.program.as_str()).collect();
        assert_eq!(
            programs,
            vec![link_s.as_str(), bash_s.as_str()],
            "{shells:?}"
        );
        assert!(shells[0].is_default && !shells[1].is_default);
        // No login shell → the fixed list only, none marked default.
        let shells = unix_shell_candidates(None, &[&real_s, &link_s], &|p| p.is_file(), &canonical);
        assert_eq!(shells.len(), 1);
        assert!(!shells[0].is_default);
    }

    #[test]
    fn wsl_decode_handles_utf16_with_bom() {
        let text = "Ubuntu\r\nDebian\r\n";
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let decoded = decode_wsl_output(&bytes);
        assert!(decoded.contains("Ubuntu"));
        assert!(decoded.contains("Debian"));
    }

    #[test]
    fn wsl_decode_falls_back_to_utf8() {
        assert_eq!(decode_wsl_output(b"Ubuntu\n"), "Ubuntu\n");
    }

    #[test]
    fn wsl_list_parsing_strips_markers_and_caps_entries() {
        let text = "Ubuntu\r\n* Debian\r\n\r\n";
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(parse_wsl_list(&bytes), vec!["Ubuntu", "Debian"]);
        let many: String = (0..40).map(|i| format!("d{i}\n")).collect();
        assert_eq!(parse_wsl_list(many.as_bytes()).len(), 16);
    }

    /// Path joins use the host separator, so compare in Windows form.
    fn win(path: &Path) -> String {
        path.to_string_lossy().replace('/', "\\")
    }

    #[test]
    fn windows_shell_candidates_resolve_env_and_mark_one_default() {
        let env = |name: &str| match name {
            "SystemRoot" => Some(r"D:\Win".to_string()),
            "ComSpec" => Some(r"D:\Win\System32\cmd.exe".to_string()),
            "ProgramFiles" => Some(r"D:\PF".to_string()),
            "LOCALAPPDATA" => Some(r"D:\Users\me\AppData\Local".to_string()),
            _ => None,
        };
        let existing = [
            r"D:\Win\System32\WindowsPowerShell\v1.0\powershell.exe",
            r"D:\Win\System32\cmd.exe",
            r"D:\Win\System32\wsl.exe",
            r"D:\Users\me\AppData\Local\Microsoft\WindowsApps\pwsh.exe",
        ];
        let exists = |p: &Path| existing.contains(&win(p).as_str());
        let shells = windows_shell_candidates(&env, &[], &exists);
        let kinds: Vec<_> = shells.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, ["powershell", "cmd", "wsl", "pwsh"]);
        assert!(shells[0].program.replace('/', "\\").starts_with(r"D:\Win\"));
        assert!(shells[3]
            .program
            .replace('/', "\\")
            .ends_with(r"WindowsApps\pwsh.exe"));
        assert_eq!(shells.iter().filter(|s| s.is_default).count(), 1);
        assert!(
            shells[0].is_default,
            "Windows PowerShell is the conventional default"
        );
    }

    #[test]
    fn windows_shell_candidates_fall_back_to_path_and_pwsh_default() {
        let env = |_: &str| None;
        let dirs = [PathBuf::from(r"D:\tools")];
        let existing = [r"D:\tools\pwsh.exe", r"C:\Windows\System32\cmd.exe"];
        let exists = |p: &Path| existing.contains(&win(p).as_str());
        let shells = windows_shell_candidates(&env, &dirs, &exists);
        let kinds: Vec<_> = shells.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, ["cmd", "pwsh"]);
        assert!(
            shells[1].is_default,
            "no Windows PowerShell → pwsh is default"
        );
        assert!(!shells[0].is_default);
    }

    #[test]
    fn shell_detection_returns_entries() {
        let shells = detect_shells();
        // 최소한 이 호스트(Windows)에서는 powershell/cmd가 있어야 한다.
        if cfg!(windows) {
            assert!(
                shells.iter().any(|s| s.kind == "powershell"),
                "Windows PowerShell 미탐지: {shells:?}"
            );
        } else {
            assert!(!shells.is_empty());
        }
    }
}

#[cfg(all(test, unix))]
mod login_shell_tests {
    #[test]
    fn account_shell_is_first_and_marked_in_the_wire_contract() {
        let shell = super::account_login_shell().expect("test account has a login shell");
        assert!(std::path::Path::new(&shell).is_absolute());
        let detected = super::detect_shells();
        assert_eq!(detected[0].program, shell);
        assert!(detected[0].is_default);
        assert_eq!(detected.iter().filter(|s| s.is_default).count(), 1);
        assert_eq!(
            serde_json::to_value(&detected[0]).unwrap()["isDefault"],
            true
        );
    }
}

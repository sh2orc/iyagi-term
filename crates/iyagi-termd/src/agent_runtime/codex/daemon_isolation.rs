//! Keep daemon-owned codex processes off the user's shared background daemon.
//!
//! Newer codex releases attach `codex` invocations to a shared app-server
//! daemon under `$CODEX_HOME` (`~/.codex/app-server-control/`). A mission
//! workload that bootstraps or joins that daemon ties the user's global codex
//! terminal infrastructure to the workload's own process tree: force-stopping
//! the workload can then wedge or destroy the shared daemon, and every codex
//! terminal on the machine dies with it. The global `--no-daemon` flag keeps
//! each spawned codex self-contained.
//!
//! Support is version-dependent (0.153.4 did not have the flag), so it is
//! probed once per program path — `codex --no-daemon --version` either parses
//! and prints the version or exits non-zero — and only passed when the
//! installed build accepts it. A probe failure degrades to today's argv, which
//! older builds already handle. Interactive user panes deliberately keep the
//! shared daemon and never pass through here.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use super::super::installation;

/// `--no-daemon --version` answers with one short line; the cap only guards a
/// misbehaving CLI.
const PROBE_LIMIT: usize = 4096;
/// The probe sits inside caller budgets (e.g. `runtime.detect`'s 2 s row
/// budget), so it stays tight — but a busy machine can push a healthy
/// `--version` past one second, and a timed-out probe is simply retried on
/// the next spawn (never cached as "unsupported").
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// One capability probe outcome. Only definitive answers are cached: a
/// definitive build that rejects the flag answers fast (clap usage error), so
/// a timeout means "could not tell", not "unsupported" — caching it would
/// permanently strip the isolation flag because of one slow moment.
enum ProbeVerdict {
    Supported,
    Unsupported,
    Inconclusive,
}

/// argv prefix that keeps one spawned codex off the shared background daemon.
/// Empty when the installed build does not accept the flag (or the probe
/// could not tell yet). The prefix must precede the subcommand
/// (`codex --no-daemon app-server ...`).
pub fn argv_prefix(program: &Path) -> Vec<String> {
    if supported(program) {
        vec!["--no-daemon".to_owned()]
    } else {
        Vec::new()
    }
}

/// Cached per program path: the probe is a process spawn, and every codex
/// launch (each mission run, each binding probe) consults it.
fn supported(program: &Path) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(hit) = guard.get(program) {
        return *hit;
    }
    let verdict = probe(program);
    if let ProbeVerdict::Supported = verdict {
        guard.insert(program.to_path_buf(), true);
    } else if let ProbeVerdict::Unsupported = verdict {
        guard.insert(program.to_path_buf(), false);
    }
    matches!(verdict, ProbeVerdict::Supported)
}

fn probe(program: &Path) -> ProbeVerdict {
    let Some(program) = program.to_str() else {
        return ProbeVerdict::Unsupported;
    };
    match installation::capture_stdout(
        program,
        &["--no-daemon", "--version"],
        PROBE_LIMIT,
        PROBE_TIMEOUT,
    ) {
        Ok(_) => ProbeVerdict::Supported,
        // NotFound/Failed: the build answered definitively (or does not
        // exist to answer at all).
        Err(installation::ProbeFailure::NotFound) | Err(installation::ProbeFailure::Failed) => {
            ProbeVerdict::Unsupported
        }
        // TimedOut/OutputLimit: this machine could not hear the answer.
        Err(_) => ProbeVerdict::Inconclusive,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    fn script(body: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codex");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "#!/bin/sh\n{body}").unwrap();
        drop(file);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        (dir, path)
    }

    #[test]
    #[cfg(unix)]
    fn a_build_that_accepts_the_flag_gets_the_prefix() {
        let (_dir, path) = script("printf 'codex-cli 0.159.0\\n'");
        assert_eq!(
            argv_prefix(&path),
            vec!["--no-daemon".to_owned()],
            "any exit-0 answer to --no-daemon --version means the flag parses"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_build_that_rejects_the_flag_gets_no_prefix() {
        let (_dir, path) = script(
            // clap rejects unknown arguments before dispatching the command.
            "if [ \"$1\" = '--no-daemon' ]; then exit 2; fi\nprintf 'codex-cli 0.154.0\\n'",
        );
        assert!(argv_prefix(&path).is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn a_missing_or_slow_build_gets_no_prefix() {
        assert!(argv_prefix(Path::new("/nonexistent/codex")).is_empty());
        let (_dir, path) = script("sleep 30");
        assert!(
            argv_prefix(&path).is_empty(),
            "probe stays inside its 1 s cap"
        );
    }

    #[test]
    #[cfg(unix)]
    fn the_probe_runs_once_per_program_path() {
        let (dir, path) =
            script("printf 'codex-cli 0.159.0\\n' >> \"$0.calls\"; printf 'codex-cli 0.159.0\\n'");
        assert_eq!(argv_prefix(&path), vec!["--no-daemon".to_owned()]);
        assert_eq!(argv_prefix(&path), vec!["--no-daemon".to_owned()]);
        let calls = std::fs::read_to_string(path.with_extension("calls")).unwrap();
        assert_eq!(
            calls.trim(),
            "codex-cli 0.159.0",
            "the second lookup was served from the cache"
        );
        // A second program path is probed independently.
        let other = dir.path().join("codex-other");
        std::fs::copy(&path, &other).unwrap();
        let _ = std::fs::remove_file(path.with_extension("calls"));
        assert_eq!(argv_prefix(&other), vec!["--no-daemon".to_owned()]);
        assert!(
            !path.with_extension("calls").exists(),
            "the first path was not probed again"
        );
    }
}

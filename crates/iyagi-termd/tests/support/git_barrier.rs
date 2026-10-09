//! Test barriers inside the daemon's own Git calls.
//!
//! Daemon Git commands disable repository-controlled execution
//! (`core.hooksPath=/dev/null`, workspace/git.rs), so a repository hook can no
//! longer hold an integration or capture mid-flight. Instead a `git` stand-in
//! goes first on PATH: an invocation that a registered barrier's `matches`
//! accepts runs the real Git and then waits in that barrier's `hold`; every
//! other invocation `exec`s the real Git unchanged. Daemons and native helpers
//! inherit this process's PATH, so [`install`] must run before they spawn.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

/// Single-quoted for `/bin/sh`.
pub fn quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}

/// Put the stand-in first on PATH (once per process) and return the
/// directory that holds registered barriers.
pub fn install() -> &'static Path {
    static REGISTRY: OnceLock<PathBuf> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::var_os("PATH").unwrap_or_default();
        let real = std::env::split_paths(&path)
            .map(|dir| dir.join("git"))
            .find(|candidate| candidate.is_file())
            .expect("git on PATH");
        let root = tempfile::Builder::new()
            .prefix("iyagi-git-barrier-")
            .tempdir()
            .unwrap()
            .keep();
        let registry = root.join("barriers");
        let bin = root.join("bin");
        std::fs::create_dir(&registry).unwrap();
        std::fs::create_dir(&bin).unwrap();
        let shim = bin.join("git");
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\ngit_subcommand() {{\n  while [ \"$#\" -gt 0 ]; do\n    case \"$1\" in\n      -c|-C) [ \"$#\" -ge 2 ] || return 1; shift 2 ;;\n      -*) shift ;;\n      *) printf '%s\\n' \"$1\"; return 0 ;;\n    esac\n  done\n  return 1\n}}\nfor entry in {registry}/*.sh; do\n  [ -f \"$entry\" ] || continue\n  . \"$entry\"\n  if matches \"$@\"; then\n    {real} \"$@\"\n    hold \"$?\"\n    exit \"$?\"\n  fi\ndone\nexec {real} \"$@\"\n",
                registry = quote(&registry),
                real = quote(&real),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let joined =
            std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&path)))
                .unwrap();
        std::env::set_var("PATH", joined);
        registry
    })
}

/// Register one barrier: a shell fragment defining `matches "$@"` (status 0
/// for an invocation to hold) and `hold <git status>` (returns the status
/// the invocation exits with). `matches` runs in the caller's cwd and may use
/// `git_subcommand "$@"` (the first word after global options). Written
/// atomically so a concurrent stand-in never sources half of it.
pub fn register(script: &str) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let registry = install();
    let name = format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let staged = registry.join(format!("{name}.tmp"));
    std::fs::write(&staged, script).unwrap();
    std::fs::rename(&staged, registry.join(format!("{name}.sh"))).unwrap();
}

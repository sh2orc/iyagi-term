//! Build-time version stamp. Emits `IYAGI_GIT_SHA` (commit plus source fingerprint)
//! so the daemon can report a build id that changes with uncommitted code
//! (unlike `CARGO_PKG_VERSION`, which is static between dev rebuilds). The
//! app compares its own stamp against the daemon's `HelloResult.daemon_version`
//! to detect a stale, still-running daemon.

#[path = "../../build-support/daemon_version.rs"]
mod daemon_version;

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    daemon_version::emit(&root);
}

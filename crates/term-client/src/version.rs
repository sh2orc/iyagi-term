//! build id 비교(원본 bridge/connection.rs에서 분리, T02).
//!
//! build id는 caller가 주입한다 — 공통 crate의 package version을 GUI/daemon
//! build version으로 오인하지 않게(IMPLEMENTATION_PLAN §2.3, T02 완료 기준).

/// build id 문자열 조립: `{pkg version} ({source id})`.
/// 데몬의 `HelloResult.daemon_version`과 같은 모양.
pub fn build_version(pkg_version: &str, source_id: &str) -> String {
    format!("{pkg_version} ({source_id})")
}

/// Pure version-compare: the daemon is "outdated" when its reported build id
/// differs from this app's. An OLD daemon predates `daemon_version` and reports
/// `""` (serde default) — always different, so always outdated. Equal ids
/// (app and daemon built from the same source) are never outdated.
pub fn daemon_is_outdated(app_version: &str, daemon_version: &str) -> bool {
    app_version != daemon_version
}

/// Whether restarting would bring up a different daemon — the "outdated"
/// flag the UI turns into a restart offer. A restart spawns the binary on
/// disk, so when that binary's build id is known it is the only meaningful
/// reference: equal to the running daemon means a restart changes nothing
/// (e.g. only the daemon was rebuilt and is already running, while the app
/// still carries the older id — offering a restart there kills every
/// terminal for no gain, forever). Unknown on-disk id → compare with the app.
pub fn restart_would_update(
    app_version: &str,
    on_disk: Option<&str>,
    daemon_version: &str,
) -> bool {
    match on_disk {
        Some(next) => daemon_is_outdated(next, daemon_version),
        None => daemon_is_outdated(app_version, daemon_version),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_is_outdated_only_when_build_ids_differ() {
        // Same build id → current.
        assert!(!daemon_is_outdated("0.1.0 (abc1234)", "0.1.0 (abc1234)"));
        // Different sha (rebuilt app, stale daemon) → outdated.
        assert!(daemon_is_outdated("0.1.0 (abc1234)", "0.1.0 (def5678)"));
        // OLD daemon predates the field and reports "" → outdated.
        assert!(daemon_is_outdated("0.1.0 (abc1234)", ""));
        // 주입형 조립기는 빈 id를 만들지 않는다(호출자 규약).
        assert!(!build_version("0.1.0", "abc1234").is_empty());
    }

    #[test]
    fn restart_is_offered_only_when_it_would_bring_up_a_different_daemon() {
        let app = "0.1.0 (app1111)";
        // Rebuilt daemon binary, old process still running → restart helps.
        assert!(restart_would_update(
            app,
            Some("0.1.0 (new2222)"),
            "0.1.0 (old3333)"
        ));
        // Only the daemon was rebuilt and it already runs: the app id differs,
        // but a restart would spawn the very same binary → nothing to offer.
        assert!(!restart_would_update(
            app,
            Some("0.1.0 (new2222)"),
            "0.1.0 (new2222)"
        ));
        // Updated app bundle (new daemon on disk) while the old one runs.
        assert!(restart_would_update(app, Some(app), "0.1.0 (old3333)"));
        // On-disk id unknown (no binary / no --version): fall back to the app.
        assert!(restart_would_update(app, None, "0.1.0 (old3333)"));
        assert!(!restart_would_update(app, None, app));
    }
}

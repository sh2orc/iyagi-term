//! `PtyHandle::spawn(.., env_remove, ..)` drops the named INHERITED variables
//! from the first child (a routed Claude launch neutralizes the daemon's own
//! ANTHROPIC_*/CLAUDE_CODE_* copies) while explicit overrides, applied after
//! the removals, still win. The PTY baseline (TERM etc.) is unaffected.
#![cfg(unix)]

use std::{collections::BTreeMap, io::Read, sync::Once, time::Duration};
use term_pty::pty::PtyHandle;

const INHERITED: &str = "IYAGI_PTY_ENV_REMOVE_INHERITED";
const KEPT: &str = "IYAGI_PTY_ENV_REMOVE_KEPT";
const OVERRIDDEN: &str = "IYAGI_PTY_ENV_REMOVE_OVERRIDDEN";
const ABSENT: &str = "IYAGI_PTY_ENV_REMOVE_NEVER_SET";

/// Seed the probe variables in THIS process exactly once, before any test
/// here spawns: portable-pty snapshots the parent environment per spawn and
/// `set_var` must never race those snapshots on sibling test threads. Every
/// test calls this first, so all spawns happen after the single seeding.
fn seed_parent_environment() {
    static SEED: Once = Once::new();
    SEED.call_once(|| {
        std::env::set_var(INHERITED, "from-parent");
        std::env::set_var(KEPT, "kept");
        std::env::set_var(OVERRIDDEN, "from-parent");
    });
}

fn probe_environment(overrides: BTreeMap<String, String>, env_remove: &[String]) -> String {
    seed_parent_environment();
    let script = format!(
        "printf 'INHERITED:[%s] KEPT:[%s] OVERRIDDEN:[%s] ABSENT:[%s] TERM:[%s]\\n' \
         \"${{{INHERITED}-unset}}\" \"${{{KEPT}-unset}}\" \"${{{OVERRIDDEN}-unset}}\" \
         \"${{{ABSENT}-unset}}\" \"$TERM\""
    );
    let argv = vec!["/bin/sh".into(), "-c".into(), script];
    let pty = PtyHandle::spawn(80, 24, "/bin/sh", &argv, &overrides, env_remove, None).unwrap();
    let mut reader = pty.reader().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = reader.read_to_end(&mut output);
        let _ = tx.send(output);
    });
    let result = rx.recv_timeout(Duration::from_secs(10));
    let _ = pty.kill();
    pty.close();
    let _ = pty.poll_exit();
    String::from_utf8_lossy(&result.expect("PTY timed out")).into_owned()
}

/// `NAME:[value]` field extractor for the printf markers above.
fn field(output: &str, name: &str) -> String {
    let marker = format!("{name}:[");
    let start = output
        .find(&marker)
        .unwrap_or_else(|| panic!("{name} missing: {output}"))
        + marker.len();
    let end = output[start..]
        .find(']')
        .unwrap_or_else(|| panic!("unterminated {name}: {output}"))
        + start;
    output[start..end].to_string()
}

#[test]
fn env_remove_drops_only_the_named_inherited_variables() {
    // A name that was never set is silently ignored.
    let output = probe_environment(
        BTreeMap::new(),
        &[INHERITED.to_string(), ABSENT.to_string()],
    );
    assert_eq!(field(&output, "INHERITED"), "unset", "{output}");
    assert_eq!(field(&output, "ABSENT"), "unset", "{output}");
    assert_eq!(field(&output, "KEPT"), "kept", "{output}");
    // The PTY baseline is untouched by removals of unrelated names.
    assert_eq!(field(&output, "TERM"), "xterm-256color", "{output}");
}

#[test]
fn env_overrides_apply_after_env_remove() {
    // Removal then override for the same name: the launch's explicit value
    // wins, not the inherited one and not "unset".
    let output = probe_environment(
        BTreeMap::from([(OVERRIDDEN.to_string(), "override".to_string())]),
        &[OVERRIDDEN.to_string()],
    );
    assert_eq!(field(&output, "OVERRIDDEN"), "override", "{output}");
    assert_eq!(field(&output, "INHERITED"), "from-parent", "{output}");
}

#[test]
fn empty_env_remove_leaves_the_inherited_environment_alone() {
    let output = probe_environment(BTreeMap::new(), &[]);
    assert_eq!(field(&output, "INHERITED"), "from-parent", "{output}");
    assert_eq!(field(&output, "KEPT"), "kept", "{output}");
    assert_eq!(field(&output, "OVERRIDDEN"), "from-parent", "{output}");
}

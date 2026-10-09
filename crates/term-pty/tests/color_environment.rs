//! Run with NO_COLOR/FORCE_COLOR set to exercise parent environment isolation.
#![cfg(unix)]

use std::{collections::BTreeMap, io::Read, time::Duration};
use term_pty::pty::PtyHandle;

fn color_environment(overrides: BTreeMap<String, String>) -> String {
    let script = "printf 'COLOR:%s:%s:%s:%s:TERM:%s\\n' \"${NO_COLOR-unset}\" \"${FORCE_COLOR-unset}\" \"${CLICOLOR-unset}\" \"${CLICOLOR_FORCE-unset}\" \"$TERM\"";
    let argv = vec!["/bin/sh".into(), "-c".into(), script.into()];
    let pty = PtyHandle::spawn(80, 24, "/bin/sh", &argv, &overrides, &[], None).unwrap();
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

#[test]
fn interactive_pty_does_not_inherit_parent_color_policy() {
    let output = color_environment(BTreeMap::new());
    assert!(
        output.contains("COLOR:unset:unset:unset:unset:TERM:xterm-256color"),
        "{output}"
    );
}

#[test]
fn explicit_launch_color_policy_is_preserved() {
    let output = color_environment(BTreeMap::from([("NO_COLOR".into(), "1".into())]));
    assert!(
        output.contains("COLOR:1:unset:unset:unset:TERM:xterm-256color"),
        "{output}"
    );
    let output = color_environment(BTreeMap::from([("FORCE_COLOR".into(), "3".into())]));
    assert!(
        output.contains("COLOR:unset:3:unset:unset:TERM:xterm-256color"),
        "{output}"
    );
}

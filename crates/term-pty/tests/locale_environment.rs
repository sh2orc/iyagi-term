//! PTY children must end up with a UTF-8 charset even when the daemon was
//! launched from a GUI context carrying no LANG/LC_*: a C-locale shell edits
//! multibyte input (Hangul) byte-wise, which reads as broken IME input.
#![cfg(unix)]

use std::{collections::BTreeMap, io::Read, time::Duration};
use term_pty::pty::PtyHandle;

fn locale_environment(overrides: BTreeMap<String, String>) -> String {
    let script = "printf 'LC_ALL:[%s] LC_CTYPE:[%s] LANG:[%s]\\n' \"${LC_ALL-unset}\" \"${LC_CTYPE-unset}\" \"${LANG-unset}\"";
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

fn selects_utf8(value: &str) -> bool {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect::<String>()
        .contains("UTF8")
}

#[test]
fn explicit_locale_overrides_pass_through_unchanged() {
    let output = locale_environment(BTreeMap::from([("LC_CTYPE".into(), "ko_KR.UTF-8".into())]));
    assert_eq!(field(&output, "LC_CTYPE"), "ko_KR.UTF-8");
}

#[test]
fn child_has_utf8_charset_without_overrides() {
    // A charset pinned by the caller (LC_ALL/LC_CTYPE in this process) is
    // respected by design; only the defaulting path is asserted here.
    if std::env::var_os("LC_ALL").is_some() || std::env::var_os("LC_CTYPE").is_some() {
        return;
    }
    let output = locale_environment(BTreeMap::new());
    let lc_ctype = field(&output, "LC_CTYPE");
    let effective = if lc_ctype != "unset" {
        lc_ctype.clone()
    } else {
        field(&output, "LANG")
    };
    assert!(
        selects_utf8(&effective),
        "effective charset is not UTF-8: LC_CTYPE={lc_ctype} output={output}"
    );
}

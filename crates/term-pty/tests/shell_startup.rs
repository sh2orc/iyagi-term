//! Exercise real shell startup without reading or modifying the user's dotfiles.
#![cfg(target_os = "macos")]

use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;
use term_pty::pty::PtyHandle;

fn run_shell(program: &str, args: &[&str], files: &[(&str, &str)], term: Option<&str>) -> String {
    let home = tempfile::tempdir().unwrap();
    for (name, contents) in files {
        std::fs::write(home.path().join(name), contents).unwrap();
    }
    let path = home.path().to_str().unwrap();
    let mut env = BTreeMap::from([
        ("HOME".into(), path.into()),
        ("ZDOTDIR".into(), path.into()),
        ("HISTFILE".into(), "/dev/null".into()),
    ]);
    if let Some(term) = term {
        env.insert("TERM".into(), term.into());
    }
    let argv = std::iter::once(program)
        .chain(args.iter().copied())
        .map(String::from)
        .collect::<Vec<_>>();
    let pty = PtyHandle::spawn(80, 24, program, &argv, &env, &[], Some(path)).unwrap();
    let mut reader = pty.reader().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut output = Vec::new();
        // macOS PTYs may report EIO at EOF; keep all bytes read before it.
        let _ = reader.read_to_end(&mut output);
        let _ = tx.send(output);
    });
    let output = rx.recv_timeout(Duration::from_secs(10));
    let _ = pty.kill();
    pty.close();
    let _ = pty.poll_exit();
    String::from_utf8_lossy(&output.expect("shell startup timed out")).into_owned()
}

#[test]
fn zsh_login_loads_profile_and_rc_with_terminal_environment() {
    let output = run_shell("/bin/zsh", &["-l", "-i", "-c", "print -r -- RESULT:${IYAGI_STARTUP}:${TERM}:${COLORTERM}; [[ -o login && -o interactive && $PWD -ef $HOME ]] && print MODE_OK"], &[
        (".zprofile", "export IYAGI_STARTUP=profile\n"),
        (".zshrc", "export IYAGI_STARTUP=${IYAGI_STARTUP}:rc\n"),
    ], None);
    assert!(
        output.contains("RESULT:profile:rc:xterm-256color:truecolor"),
        "{output}"
    );
    assert!(output.contains("MODE_OK"), "{output}");
}

#[test]
fn bash_login_reads_profile_and_its_rc_once() {
    let output = run_shell("/bin/bash", &["-l", "-i", "-c", "printf 'RESULT:%s:%s\n' \"$IYAGI_STARTUP\" \"$TERM\"; shopt -q login_shell && [[ $- == *i* && $PWD -ef $HOME ]] && echo MODE_OK"], &[
        (".bash_profile", "export IYAGI_STARTUP=profile\n. \"$HOME/.bashrc\"\n"),
        (".bashrc", "export IYAGI_STARTUP=${IYAGI_STARTUP}:rc\n"),
    ], None);
    assert!(
        output.contains("RESULT:profile:rc:xterm-256color"),
        "{output}"
    );
    assert!(output.contains("MODE_OK"), "{output}");
}

#[test]
fn custom_non_login_bash_reads_rc_and_honors_explicit_term() {
    let output = run_shell(
        "/bin/bash",
        &[
            "-i",
            "-c",
            "printf 'RESULT:%s:%s\n' \"$IYAGI_STARTUP\" \"$TERM\"",
        ],
        &[
            (".bash_profile", "export IYAGI_STARTUP=unexpected\n"),
            (".bashrc", "export IYAGI_STARTUP=rc\n"),
        ],
        Some("vt100"),
    );
    assert!(output.contains("RESULT:rc:vt100"), "{output}");
}

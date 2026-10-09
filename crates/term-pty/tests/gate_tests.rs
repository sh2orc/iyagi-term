//! Launch-gate integration tests (spec `02-runner.md` §3, ticket I05).
//!
//! In-process duplex pairs drive the full daemon<->helper sequence; the
//! full-sequence test runs the REAL term-fixture `gate-observer` as the
//! target (spawned via std::process, not a PTY, for determinism) and proves
//! the core invariant: the target is NEVER created before RELEASE.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::duplex_pair;
use term_contracts::gate::GateTarget;
use term_contracts::ids::ProcessIdentity;
use term_pty::gate::{
    default_deadline, generate_nonce, GateClient, GateError, GateServer, StartOutcome,
};

fn identity(pid: u32) -> ProcessIdentity {
    ProcessIdentity {
        pid,
        start_token: "12345".into(),
        boot_id: "test-boot".into(),
    }
}

fn fixture_gate_target(marker: &std::path::Path, program: &str) -> GateTarget {
    GateTarget {
        program: program.to_string(),
        argv: vec![
            program.to_string(),
            "gate-observer".into(),
            "--marker".into(),
            marker.to_string_lossy().into_owned(),
        ],
        env_overrides: BTreeMap::new(),
        env_clear: false,
        env_remove: Vec::new(),
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
    }
}

#[test]
fn full_sequence_target_created_only_after_release() {
    let Some(fixture) = common::locate_fixture() else {
        eprintln!("SKIP: term-fixture binary not found (cargo build -p term-fixture)");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("gate-marker");

    let nonce = generate_nonce();
    assert_eq!(nonce.len(), 64, "256-bit nonce as 64 hex chars");
    let id = identity(4242);

    let (server_end, client_end) = duplex_pair();
    let deadline = Instant::now() + Duration::from_secs(10);

    // Helper side (a real helper would be a separate process; the protocol
    // logic under test is identical over any GateStream).
    let helper_nonce = nonce.clone();
    let helper_id = id.clone();
    let helper = std::thread::spawn(move || {
        let mut client = GateClient::new(client_end);
        client
            .hello(&helper_nonce, &helper_id)
            .expect("hello sends");
        let target = client
            .await_release(Instant::now() + Duration::from_secs(5))
            .expect("release arrives");
        // B06 invariant implemented at the helper: on any await_release
        // error we would exit WITHOUT running the target. Only RELEASE
        // reaches this line.
        client.run_target(target)
    });

    let mut server = GateServer::new(server_end);
    let hello = server
        .wait_hello(&nonce, &id, deadline)
        .expect("hello verifies");
    assert_eq!(hello.identity, id);

    // Target spec sent while the helper WAITS — no side effects yet.
    let program = fixture.to_string_lossy().into_owned();
    server
        .send_target(&fixture_gate_target(&marker, &program))
        .expect("target spec sends");
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !marker.exists(),
        "INVARIANT VIOLATION: target created before RELEASE"
    );

    // Attach hook runs BEFORE release; then the single RELEASE.
    let mut attached = false;
    server
        .release(|| {
            attached = true;
            Ok(())
        })
        .expect("release");
    assert!(attached, "attach hook must run before release");
    assert!(server.is_released());

    // Started arrives only after the target really spawned.
    let started = server.await_started(deadline).expect("start report");
    assert_eq!(started, StartOutcome::Started);

    let code = server.await_exited(deadline).expect("exit report");
    assert_eq!(code, 0);
    let helper_result = helper
        .join()
        .expect("helper thread")
        .expect("run_target ok");
    assert_eq!(helper_result, 0);
    assert!(marker.exists(), "marker appears only after RELEASE");
}

#[test]
fn wrong_nonce_is_rejected() {
    let nonce = generate_nonce();
    let other = generate_nonce();
    let id = identity(7);
    let (server_end, client_end) = duplex_pair();

    let helper_id = id.clone();
    let helper = std::thread::spawn(move || {
        let mut client = GateClient::new(client_end);
        client
            .hello(&other, &helper_id)
            .expect("hello sends with wrong nonce");
        // Daemon will abort; helper must see Abort, never RELEASE.
        client.await_release(Instant::now() + Duration::from_secs(2))
    });

    let mut server = GateServer::new(server_end);
    let err = server
        .wait_hello(&nonce, &id, default_deadline())
        .expect_err("nonce mismatch must fail");
    assert!(matches!(err, GateError::NonceMismatch));
    server.abort().expect("abort sends");
    let helper_err = helper.join().expect("helper thread").expect_err("abort");
    assert!(matches!(helper_err, GateError::Aborted));
}

#[test]
fn identity_mismatch_is_rejected() {
    let nonce = generate_nonce();
    let expected = identity(100);
    let reported = ProcessIdentity {
        pid: 100,
        start_token: "99999".into(), // PID reuse: same pid, different start
        boot_id: "test-boot".into(),
    };
    let (server_end, client_end) = duplex_pair();
    let helper_nonce = nonce.clone();
    let helper = std::thread::spawn(move || {
        let mut client = GateClient::new(client_end);
        client.hello(&helper_nonce, &reported).expect("hello sends");
        client.await_release(Instant::now() + Duration::from_millis(300))
    });
    let mut server = GateServer::new(server_end);
    let err = server
        .wait_hello(&nonce, &expected, default_deadline())
        .expect_err("identity mismatch must fail");
    assert!(matches!(err, GateError::IdentityMismatch));
    let _ = server.abort();
    let _ = helper.join();
}

#[test]
fn second_release_is_a_protocol_error() {
    let nonce = generate_nonce();
    let id = identity(9);
    let (server_end, client_end) = duplex_pair();
    let helper_nonce = nonce.clone();
    let helper_id = id.clone();
    let helper = std::thread::spawn(move || {
        let mut client = GateClient::new(client_end);
        client.hello(&helper_nonce, &helper_id).unwrap();
        let _ = client.await_release(Instant::now() + Duration::from_secs(2));
        // Keep the stream open a moment so the server's second release
        // attempt is a protocol event, not an EOF.
        std::thread::sleep(Duration::from_millis(300));
    });
    let mut server = GateServer::new(server_end);
    server.wait_hello(&nonce, &id, default_deadline()).unwrap();
    server
        .send_target(&GateTarget {
            program: "irrelevant".into(),
            argv: vec!["irrelevant".into()],
            env_overrides: BTreeMap::new(),
            env_clear: false,
            env_remove: Vec::new(),
            cwd: ".".into(),
        })
        .unwrap();
    server.release(|| Ok(())).expect("first release");
    let err = server.release(|| Ok(())).expect_err("double release");
    assert!(matches!(err, GateError::Protocol(_)));
    let _ = helper.join();
}

#[test]
fn attach_hook_failure_sends_abort_and_never_releases() {
    let Some(fixture) = common::locate_fixture() else {
        eprintln!("SKIP: term-fixture binary not found (cargo build -p term-fixture)");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("no-marker");
    let nonce = generate_nonce();
    let id = identity(11);
    let (server_end, client_end) = duplex_pair();

    let program = fixture.to_string_lossy().into_owned();
    let helper_nonce = nonce.clone();
    let helper_id = id.clone();
    let helper = std::thread::spawn(move || {
        let mut client = GateClient::new(client_end);
        client.hello(&helper_nonce, &helper_id).unwrap();
        match client.await_release(Instant::now() + Duration::from_secs(2)) {
            Ok(t) => panic!("release must not arrive when attach failed: {t:?}"),
            Err(e) => e,
        }
    });

    let mut server = GateServer::new(server_end);
    server.wait_hello(&nonce, &id, default_deadline()).unwrap();
    server
        .send_target(&fixture_gate_target(&marker, &program))
        .unwrap();
    let err = server
        .release(|| Err("group attach failed".into()))
        .expect_err("attach failure surfaces");
    assert!(matches!(err, GateError::AttachFailed(_)));
    assert!(!server.is_released(), "no RELEASE was sent");
    let helper_err = helper.join().expect("helper");
    assert!(matches!(helper_err, GateError::Aborted));
    assert!(!marker.exists(), "no target was ever created");
}

#[test]
fn timeout_expiry_exits_without_creating_target() {
    let Some(_fixture) = common::locate_fixture() else {
        eprintln!("SKIP: term-fixture binary not found (cargo build -p term-fixture)");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("timeout-marker");
    let nonce = generate_nonce();
    let id = identity(12);
    let (server_end, client_end) = duplex_pair();

    // Daemon never sends target/RELEASE (gate stuck at verification).
    let _daemon_hold = server_end;
    let helper = std::thread::spawn(move || {
        let mut client = GateClient::new(client_end);
        client.hello(&nonce, &id).unwrap();
        // 5s spec timeout compressed for the test; the semantics (timeout =>
        // exit WITHOUT creating the target) are identical.
        let outcome = client.await_release(Instant::now() + Duration::from_millis(150));
        outcome.is_err()
    });

    let timed_out = helper.join().expect("helper thread");
    assert!(timed_out, "await_release must time out");
    assert!(
        !marker.exists(),
        "helper exited without creating the target"
    );
}

#[test]
fn start_failed_path_reports_spawn_error_code() {
    let nonce = generate_nonce();
    let id = identity(13);
    let (server_end, client_end) = duplex_pair();
    let helper_nonce = nonce.clone();
    let helper_id = id.clone();
    let helper = std::thread::spawn(move || {
        let mut client = GateClient::new(client_end);
        client.hello(&helper_nonce, &helper_id).unwrap();
        let target = client
            .await_release(Instant::now() + Duration::from_secs(2))
            .expect("release");
        client.run_target(target)
    });
    let mut server = GateServer::new(server_end);
    server.wait_hello(&nonce, &id, default_deadline()).unwrap();
    server
        .send_target(&GateTarget {
            program: "iyagi-definitely-not-a-program-xyz".into(),
            argv: vec!["iyagi-definitely-not-a-program-xyz".into()],
            env_overrides: BTreeMap::new(),
            env_clear: false,
            env_remove: Vec::new(),
            cwd: ".".into(),
        })
        .unwrap();
    server.release(|| Ok(())).expect("release");
    let outcome = server
        .await_started(Instant::now() + Duration::from_secs(5))
        .expect("start report arrives");
    match outcome {
        StartOutcome::StartFailed { code } => assert_ne!(code, 0),
        StartOutcome::Started => panic!("nonexistent program must not start"),
    }
    let err = helper.join().expect("helper").expect_err("spawn fails");
    assert!(matches!(err, GateError::StartFailed(_)));
}

#[test]
fn release_before_target_spec_is_a_protocol_error() {
    let (server_end, _client_end) = duplex_pair();
    let mut server = GateServer::new(server_end);
    let err = server
        .release(|| Ok(()))
        .expect_err("release without target");
    assert!(matches!(err, GateError::Protocol(_)));
}

// ---------------------------------------------------------------------------
// GateTarget.env_remove through the real target-exec path (Unix).
//
// `run_target` execs on Unix, so the helper role must live in a separate
// process: the parent test re-execs THIS test binary with the probe variables
// set on the child only (same pattern as iyagi-termd's macOS guardian tests),
// drives the daemon side over a Unix socket and reads the exec'd target's
// stdout. This is the daemon -> helper -> target chain a routed Claude launch
// relies on to neutralize inherited provider/auth variables.

#[cfg(unix)]
const GATE_ENV_PROBE_SOCKET: &str = "IYAGI_GATE_ENV_PROBE_SOCKET";
#[cfg(unix)]
const GATE_ENV_PROBE_NONCE: &str = "IYAGI_GATE_ENV_PROBE_NONCE";
#[cfg(unix)]
const GATE_ENV_PROBE_REMOVED: &str = "IYAGI_GATE_ENV_PROBE_REMOVED";
#[cfg(unix)]
const GATE_ENV_PROBE_KEPT: &str = "IYAGI_GATE_ENV_PROBE_KEPT";
#[cfg(unix)]
const GATE_ENV_PROBE_OVERRIDDEN: &str = "IYAGI_GATE_ENV_PROBE_OVERRIDDEN";

#[cfg(unix)]
fn probe_identity(pid: u32) -> ProcessIdentity {
    ProcessIdentity {
        pid,
        start_token: "env-probe".into(),
        boot_id: "env-probe".into(),
    }
}

/// `NAME:[value]` field extractor for the printf markers of the probe target.
#[cfg(unix)]
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

/// Helper ROLE of `gate_target_env_remove_drops_inherited_variables`, not a
/// test of its own: without the socket variable it is a no-op. With it, this
/// process plays `iyagi-termd --launch-helper` and execs the released target;
/// the assertions live in the parent test.
#[cfg(unix)]
#[test]
fn gate_env_remove_probe_helper_role() {
    let Ok(socket) = std::env::var(GATE_ENV_PROBE_SOCKET) else {
        return;
    };
    let nonce = std::env::var(GATE_ENV_PROBE_NONCE).expect("probe nonce");
    let stream = std::os::unix::net::UnixStream::connect(socket).expect("connect gate socket");
    // Bounded reads, exactly like the real helper, so deadlines are honored.
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .expect("read timeout");
    let mut client = GateClient::new(stream);
    client
        .hello(&nonce, &probe_identity(std::process::id()))
        .expect("hello");
    let target = client
        .await_release(Instant::now() + Duration::from_secs(5))
        .expect("release");
    // exec replaces this process on success; getting past this line means
    // the target failed to start.
    let err = client
        .run_target(target)
        .expect_err("exec never returns on success");
    eprintln!("gate env probe helper: target did not start: {err}");
    std::process::exit(9);
}

#[cfg(unix)]
#[test]
fn gate_target_env_remove_drops_inherited_variables() {
    use std::os::unix::net::UnixListener;
    use std::process::{Command, Stdio};

    // Short path on purpose: a macOS $TMPDIR path would push sun_path past
    // SUN_LEN.
    let socket = format!("/tmp/iyagi-pty-gate-env-{}.sock", std::process::id());
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).expect("bind probe socket");
    listener.set_nonblocking(true).expect("set nonblocking");
    let nonce = generate_nonce();

    // Re-exec this test binary as the helper. The probe variables are set on
    // the CHILD only, so this test never mutates its own environment; the
    // target inherits them through the helper exactly like a daemon launch.
    let mut child = Command::new(std::env::current_exe().expect("test exe"))
        .args([
            "--exact",
            "gate_env_remove_probe_helper_role",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(GATE_ENV_PROBE_SOCKET, &socket)
        .env(GATE_ENV_PROBE_NONCE, &nonce)
        .env(GATE_ENV_PROBE_REMOVED, "inherited")
        .env(GATE_ENV_PROBE_KEPT, "kept")
        .env(GATE_ENV_PROBE_OVERRIDDEN, "inherited")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn helper role");

    // Bounded accept: a helper that never connects must not hang the suite.
    let accept_deadline = Instant::now() + Duration::from_secs(10);
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= accept_deadline {
                    let _ = child.kill();
                    let _ = std::fs::remove_file(&socket);
                    panic!("helper role did not connect within the deadline");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = std::fs::remove_file(&socket);
                panic!("accept failed: {e}");
            }
        }
    };
    // BSD/macOS accepted sockets inherit the listener's non-blocking flag;
    // the gate wants blocking reads with a short timeout (see the helper).
    stream.set_nonblocking(false).expect("blocking reads");
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .expect("read timeout");

    let mut server = GateServer::new(stream);
    server
        .wait_hello(&nonce, &probe_identity(child.id()), default_deadline())
        .expect("hello");
    let script = format!(
        "printf 'REMOVED:[%s] KEPT:[%s] OVERRIDDEN:[%s]\\n' \
         \"${{{GATE_ENV_PROBE_REMOVED}-unset}}\" \"${{{GATE_ENV_PROBE_KEPT}-unset}}\" \
         \"${{{GATE_ENV_PROBE_OVERRIDDEN}-unset}}\""
    );
    server
        .send_target(&GateTarget {
            program: "/bin/sh".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), script],
            env_overrides: BTreeMap::from([(
                GATE_ENV_PROBE_OVERRIDDEN.to_string(),
                "override".to_string(),
            )]),
            env_clear: false,
            // A never-set name is ignored; a removed-then-overridden name
            // ends up with the override (removals run before overrides).
            env_remove: vec![
                GATE_ENV_PROBE_REMOVED.into(),
                GATE_ENV_PROBE_OVERRIDDEN.into(),
                "IYAGI_GATE_ENV_PROBE_NEVER_SET".into(),
            ],
            cwd: ".".into(),
        })
        .expect("target spec");
    server.release(|| Ok(())).expect("release");
    // Unix: the exec closes the CLOEXEC gate stream, so a clean EOF right
    // after RELEASE is the success signal (see `GateServer::await_started`).
    let started = server.await_started(Instant::now() + Duration::from_secs(5));
    assert!(
        matches!(started, Err(GateError::Eof)),
        "expected the exec EOF, got {started:?}"
    );

    let output = child.wait_with_output().expect("helper role output");
    let _ = std::fs::remove_file(&socket);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "target exit {:?}\nstdout: {stdout}\nstderr: {stderr}",
        output.status
    );
    // libtest chatter ("running 1 test", "test ... ") shares the pipe with
    // the target's line; the field extractor does not care where it sits.
    assert_eq!(field(&stdout, "REMOVED"), "unset", "{stdout}");
    assert_eq!(field(&stdout, "KEPT"), "kept", "{stdout}");
    assert_eq!(field(&stdout, "OVERRIDDEN"), "override", "{stdout}");
}

// DuplexEnd is Send; helpers move their end into threads.
fn _assert_send<T: Send>(_: &T) {}
#[test]
fn duplex_ends_are_send() {
    let (a, b) = duplex_pair();
    _assert_send(&a);
    _assert_send(&b);
    let shared = Arc::new(a);
    _assert_send(&shared);
}

/// A connected-but-silent peer whose reads only ever step out with
/// `TimedOut` (what a per-read timeout / the daemon's stepped pipe adapter
/// produce). `DeadlineReader` must turn that into `GateError::Timeout` at
/// the deadline instead of spinning forever.
struct SilentStream;

impl std::io::Read for SilentStream {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "step"))
    }
}

impl std::io::Write for SilentStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn stepped_timeouts_still_enforce_the_deadline() {
    let mut server = GateServer::new(SilentStream);
    let deadline = Instant::now() + Duration::from_millis(200);
    let err = server
        .wait_hello("nonce", &identity(1), deadline)
        .expect_err("silent peer must not produce a hello");
    assert!(matches!(err, GateError::Timeout), "{err:?}");
    let now = Instant::now();
    assert!(now >= deadline, "returned before the deadline");
    assert!(
        now < deadline + Duration::from_secs(2),
        "deadline overshoot"
    );
}

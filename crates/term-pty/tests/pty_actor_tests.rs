//! Real-PTY integration tests (Windows ConPTY on this host; the same tests
//! exercise Unix ptys on CI). The term-fixture binary is located from the
//! workspace target dir — build it first with `cargo build -p term-fixture`
//! (tests skip with a clear message when it is missing).

mod common;

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use term_contracts::ids::SessionId;
use term_pty::actor::{
    start, ActorError, ActorEvent, ActorEventKind, DescendantWatch, JournalSink, MemRecordKind,
    OutputSink, SessionActorConfig, SessionCleanup, SessionLifecycle, TeardownInfo,
};
use term_pty::pty::{PtyError, PtyHandle};

// ---------------------------------------------------------------------------
// helpers

fn fixture_argv(fixture: &std::path::Path, mode_args: &[&str]) -> Vec<String> {
    let mut argv = vec![fixture.to_string_lossy().into_owned()];
    argv.extend(mode_args.iter().map(|s| s.to_string()));
    argv
}

/// Spawn the fixture in a PTY at 80x24.
fn spawn_fixture(fixture: &std::path::Path, args: &[&str]) -> PtyHandle {
    PtyHandle::spawn(
        80,
        24,
        &fixture.to_string_lossy(),
        &fixture_argv(fixture, args),
        &BTreeMap::new(),
        &[],
        None,
    )
    .expect("pty spawn")
}

/// Read from the pty on a side thread until `needle` appears in the
/// accumulated bytes or the timeout expires. Returns the accumulation.
fn read_until_contains(pty: &PtyHandle, needle: &[u8], timeout: Duration) -> Option<Vec<u8>> {
    let mut reader = pty.reader().expect("pty reader");
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(Vec::new());
                    break;
                }
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    let deadline = Instant::now() + timeout;
    let mut acc = Vec::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => {
                if chunk.is_empty() {
                    break; // EOF
                }
                acc.extend_from_slice(&chunk);
                if acc.windows(needle.len()).any(|w| w == needle) {
                    return Some(acc);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if acc.windows(needle.len().max(1)).any(|w| w == needle) {
        Some(acc)
    } else {
        None
    }
}

// -- test sink / journal / cleanup / watch --------------------------------

struct CollectSink {
    events: Arc<Mutex<Vec<ActorEvent>>>,
    blocked: Arc<AtomicBool>,
}

impl OutputSink for CollectSink {
    fn can_send(&self) -> bool {
        !self.blocked.load(Ordering::Acquire)
    }
    fn emit(&mut self, event: ActorEvent) {
        self.events.lock().expect("sink lock").push(event);
    }
}

#[derive(Clone, Default)]
struct SharedJournal(Arc<Mutex<term_pty::actor::MemJournal>>);

impl JournalSink for SharedJournal {
    fn append_output(&mut self, data: &[u8]) -> std::io::Result<u64> {
        self.0.lock().expect("journal lock").append_output(data)
    }
    fn append_resize(&mut self, cols: u16, rows: u16) -> std::io::Result<u64> {
        self.0
            .lock()
            .expect("journal lock")
            .append_resize(cols, rows)
    }
}

#[derive(Default)]
struct FlagCleanup {
    called: AtomicBool,
    info: Mutex<Option<TeardownInfo>>,
}

impl SessionCleanup for FlagCleanup {
    fn teardown(&self, info: &TeardownInfo) {
        self.called.store(true, Ordering::Release);
        *self.info.lock().expect("cleanup lock") = Some(info.clone());
    }
}

#[derive(Default)]
struct StubWatch(AtomicUsize);

impl DescendantWatch for StubWatch {
    fn owned_alive(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }
}

fn collect_output(events: &[ActorEvent]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in events {
        if let ActorEventKind::Output(bytes) = &e.kind {
            out.extend_from_slice(bytes);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// raw PTY

#[test]
fn actor_delivers_fragmented_output_without_losing_records_between_idle_rounds() {
    let Some(fixture) = common::locate_fixture() else {
        eprintln!("SKIP: term-fixture binary not found (cargo build -p term-fixture)");
        return;
    };
    struct ChannelSink(std::sync::mpsc::Sender<ActorEvent>);
    impl OutputSink for ChannelSink {
        fn can_send(&self) -> bool {
            true
        }
        fn emit(&mut self, event: ActorEvent) {
            let _ = self.0.send(event);
        }
    }
    let pty = Arc::new(spawn_fixture(&fixture, &["echo"]));
    let (tx, rx) = std::sync::mpsc::channel();
    let mut config = SessionActorConfig::new(SessionId::generate(), 80, 24, pty);
    config.sink = Box::new(ChannelSink(tx));
    let (handle, join) = start(config);
    let started = Instant::now();
    let mut previous_seq = 0;
    // Each fresh chunk follows a drained reader channel. The old backoff
    // treated those successful drains as idle and slept 10ms per response.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for round in 0..96 {
            let text = format!("fragment-{round:03}-{}\n", "x".repeat(480));
            handle
                .write_input(&format!("fragment-{round}"), text.as_bytes())
                .expect("queue input");
            let needle = text.trim_end().as_bytes();
            let mut bytes = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let event = rx
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .expect("output arrives");
                assert_eq!(event.seq, previous_seq + 1);
                previous_seq = event.seq;
                if let ActorEventKind::Output(data) = event.kind {
                    bytes.extend_from_slice(&data);
                }
                if bytes.windows(needle.len()).any(|window| window == needle) {
                    break;
                }
            }
        }
    }));
    eprintln!("96 fragmented PTY replies: {:?}", started.elapsed());
    handle.cancel();
    join.join().expect("actor finalizes");
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}

#[test]
fn pty_echo_round_trip_resize_and_exit_polling() {
    let Some(fixture) = common::locate_fixture() else {
        eprintln!("SKIP: term-fixture binary not found (cargo build -p term-fixture)");
        return;
    };
    let pty = spawn_fixture(&fixture, &["echo"]);

    // Still running before we do anything.
    assert!(pty.poll_exit().expect("poll").is_none());

    // Raw echo round-trip.
    pty.write_input(b"raw-echo-probe\n").expect("write");
    let seen = read_until_contains(&pty, b"raw-echo-probe", Duration::from_secs(15));
    assert!(seen.is_some(), "pty must echo the probe back");

    // Resize validation: 2..=1000 enforced, pty keeps its size on failure.
    pty.resize(120, 40).expect("valid resize");
    assert_eq!(pty.size().expect("size"), (120, 40));
    assert!(matches!(
        pty.resize(0, 0),
        Err(PtyError::InvalidSize { .. })
    ));
    assert!(matches!(
        pty.resize(1001, 500),
        Err(PtyError::InvalidSize { .. })
    ));
    assert_eq!(
        pty.size().expect("size"),
        (120, 40),
        "size unchanged after rejects"
    );

    // Kill + poll exit.
    pty.kill().expect("kill");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if pty.poll_exit().expect("poll").is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let status = pty.poll_exit().expect("poll");
    assert!(status.is_some(), "child must be reaped after kill");
}

// ---------------------------------------------------------------------------
// actor

#[test]
fn actor_echo_session_outputs_in_order_and_resizes() {
    let Some(fixture) = common::locate_fixture() else {
        eprintln!("SKIP: term-fixture binary not found (cargo build -p term-fixture)");
        return;
    };
    let pty = Arc::new(spawn_fixture(&fixture, &["echo"]));
    let events = Arc::new(Mutex::new(Vec::new()));
    let blocked = Arc::new(AtomicBool::new(false));
    let journal = SharedJournal::default();

    let mut config = SessionActorConfig::new(SessionId::generate(), 80, 24, Arc::clone(&pty));
    config.sink = Box::new(CollectSink {
        events: Arc::clone(&events),
        blocked: Arc::clone(&blocked),
    });
    config.journal = Box::new(journal.clone());
    let (handle, join) = start(config);

    // Two writes; echo must come back in order.
    handle.write_input("i1", b"first-probe\n").expect("write 1");
    handle
        .write_input("i2", b"second-probe\n")
        .expect("write 2");

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let bytes = collect_output(&events.lock().expect("sink lock"));
        let ok = bytes
            .windows(b"first-probe".len())
            .any(|w| w == b"first-probe")
            && bytes
                .windows(b"second-probe".len())
                .any(|w| w == b"second-probe");
        if ok {
            let first = bytes
                .windows(b"first-probe".len())
                .position(|w| w == b"first-probe")
                .expect("checked");
            let second = bytes
                .windows(b"second-probe".len())
                .position(|w| w == b"second-probe")
                .expect("checked");
            assert!(first < second, "outputs preserve write order");
            break;
        }
        assert!(Instant::now() < deadline, "echo never arrived");
        std::thread::sleep(Duration::from_millis(50));
    }

    // Invalid resize rejected at the handle; valid one coalesces through.
    assert!(matches!(
        handle.resize(0, 24),
        Err(ActorError::InvalidDimensions { .. })
    ));
    handle.resize(100, 30).expect("resize");
    handle.resize(100, 31).expect("resize 2"); // last-latest wins
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let got = events.lock().expect("sink lock").iter().any(|e| {
            matches!(
                e.kind,
                ActorEventKind::Resize {
                    cols: 100,
                    rows: 31
                }
            )
        });
        if got {
            break;
        }
        assert!(Instant::now() < deadline, "resize event never emitted");
        std::thread::sleep(Duration::from_millis(20));
    }

    // Epoch rotation on owner change.
    let old_epoch = handle.epoch();
    let new_epoch = handle.set_owner("view-1").expect("set owner");
    assert_ne!(old_epoch, new_epoch);

    // Dedup: replaying an input id does not rewrite it.
    match handle.write_input("i1", b"first-probe\n") {
        Ok(term_pty::actor::InputReply::Replayed { .. }) => {}
        other => panic!("duplicate input_id must replay, got {other:?}"),
    }

    // Cancel and finalize.
    handle.cancel();
    let status = handle
        .wait_finalized(Duration::from_secs(10))
        .expect("finalized");
    assert!(status.cancelled);
    let final_status = join.join().expect("actor join");
    assert_eq!(final_status.lifecycle, SessionLifecycle::Finalized);
    assert!(final_status.cancelled);

    // Journal order: first record is the initial size; seq strictly grows;
    // output records exist; the applied resize is journaled.
    let records = journal.0.lock().expect("journal lock").records.clone();
    assert!(
        matches!(
            records.first().map(|r| &r.kind),
            Some(MemRecordKind::Resize { cols: 80, rows: 24 })
        ),
        "first journal record must be the initial size"
    );
    let mut last_seq = 0;
    for record in &records {
        assert!(record.seq > last_seq, "seq strictly increases");
        last_seq = record.seq;
    }
    assert!(
        records.iter().any(|r| matches!(
            r.kind,
            MemRecordKind::Resize {
                cols: 100,
                rows: 31
            }
        )),
        "applied resize recorded"
    );
    assert!(
        records
            .iter()
            .any(|r| matches!(&r.kind, MemRecordKind::Output(bytes) if bytes.windows(5).any(|w| w == b"first")))
    );
    // Events were emitted in journal order.
    let events = events.lock().expect("sink lock").clone();
    let mut last = 0;
    for e in &events {
        assert!(e.seq > last, "sink events follow journal seq order");
        last = e.seq;
    }
}

#[test]
fn actor_root_exit_drains_and_finalizes_with_code_zero() {
    let Some(fixture) = common::locate_fixture() else {
        eprintln!("SKIP: term-fixture binary not found (cargo build -p term-fixture)");
        return;
    };
    let pty = Arc::new(spawn_fixture(
        &fixture,
        &["exit", "--code", "0", "--delay-ms", "100"],
    ));
    let cleanup = Arc::new(FlagCleanup::default());
    let mut config = SessionActorConfig::new(SessionId::generate(), 80, 24, pty);
    config.cleanup = cleanup.clone();
    let (handle, join) = start(config);

    let status = handle
        .wait_finalized(Duration::from_secs(15))
        .expect("session finalizes after root exit");
    assert_eq!(status.exit_code, Some(0));
    assert!(!status.cancelled);
    assert!(!status.descendants_remaining);
    assert!(cleanup.called.load(Ordering::Acquire), "teardown hook ran");
    let info = cleanup.info.lock().expect("cleanup lock").clone();
    assert_eq!(
        info,
        Some(TeardownInfo {
            session_id: handle.session_id(),
            cancelled: false,
            exit_code: Some(0),
            descendants_remaining: false,
        })
    );
    assert_eq!(join.join().expect("join").exit_code, Some(0));
}

#[test]
fn actor_drain_timeout_forces_finalize_with_descendants_remaining() {
    let Some(fixture) = common::locate_fixture() else {
        eprintln!("SKIP: term-fixture binary not found (cargo build -p term-fixture)");
        return;
    };
    let pty = Arc::new(spawn_fixture(
        &fixture,
        &["exit", "--code", "0", "--delay-ms", "100"],
    ));
    // Descendant watch stuck at "one alive" -> drain window must expire.
    let watch = Arc::new(StubWatch::default());
    watch.0.store(1, Ordering::Release);
    let mut config = SessionActorConfig::new(SessionId::generate(), 80, 24, pty);
    config.descendants = watch.clone();
    config.drain_timeout = Duration::from_millis(300);
    config.exit_poll_interval = Duration::from_millis(50);
    let (handle, _join) = start(config);

    // Root exit + EOF -> Draining is observable while the window is open.
    let draining = handle.wait_status(Duration::from_secs(15), |s| {
        s.lifecycle == SessionLifecycle::Draining
    });
    assert!(draining.is_some(), "must pass through Draining");

    // Spec 02-runner §5: root exit with owned descendants alive keeps the
    // session alive (RUNNING/root_exited) — the drain timeout may NOT force
    // a finalize that would close the master and kill the descendants.
    std::thread::sleep(Duration::from_millis(1200));
    let alive_status = handle.status();
    assert!(
        matches!(
            alive_status.lifecycle,
            SessionLifecycle::Running | SessionLifecycle::Draining
        ),
        "actor must stay alive with descendants present: {alive_status:?}"
    );
    assert!(alive_status.root_exited, "root exit is surfaced mid-flight");

    // The moment the last descendant leaves, the session finalizes.
    watch.0.store(0, Ordering::Release);
    let status = handle
        .wait_finalized(Duration::from_secs(10))
        .expect("finalizes once the owned group empties");
    assert_eq!(status.exit_code, Some(0), "root code is not lost");
}

#[test]
fn actor_cancel_works_even_with_full_control_mailbox() {
    let Some(fixture) = common::locate_fixture() else {
        eprintln!("SKIP: term-fixture binary not found (cargo build -p term-fixture)");
        return;
    };
    // Long-running child so nothing finalizes on its own.
    let pty = Arc::new(spawn_fixture(
        &fixture,
        &["exit", "--code", "0", "--delay-ms", "60000"],
    ));
    let events = Arc::new(Mutex::new(Vec::new()));
    let blocked = Arc::new(AtomicBool::new(false));
    let mut config = SessionActorConfig::new(SessionId::generate(), 80, 24, pty);
    config.sink = Box::new(CollectSink {
        events: Arc::clone(&events),
        blocked: Arc::clone(&blocked),
    });
    let (handle, join) = start(config);

    // Flood the 128-slot control mailbox until the actor is provably
    // saturated (a Busy result is the proof).
    let mut saw_busy = false;
    for _ in 0..50_000 {
        match handle.resize(80, 24) {
            Err(ActorError::Busy) => {
                saw_busy = true;
                break;
            }
            Ok(()) => continue,
            Err(e) => panic!("unexpected error while flooding: {e}"),
        }
    }
    assert!(saw_busy, "mailbox must saturate under a resize flood");

    // Cancel travels over the independent flag, not the full mailbox.
    handle.cancel();
    let status = handle
        .wait_finalized(Duration::from_secs(10))
        .expect("cancel must finalize even with a full mailbox");
    assert!(status.cancelled);
    assert!(join.join().expect("join").cancelled);
}

#[test]
fn actor_cancel_during_starting_runs_cleanup_hook() {
    let Some(fixture) = common::locate_fixture() else {
        eprintln!("SKIP: term-fixture binary not found (cargo build -p term-fixture)");
        return;
    };
    let pty = Arc::new(spawn_fixture(
        &fixture,
        &["exit", "--code", "0", "--delay-ms", "60000"],
    ));
    let cleanup = Arc::new(FlagCleanup::default());
    let mut config = SessionActorConfig::new(SessionId::generate(), 80, 24, pty);
    config.cleanup = cleanup.clone();
    let (handle, _join) = start(config);
    handle.cancel(); // immediately: session is still STARTING
    let status = handle
        .wait_finalized(Duration::from_secs(10))
        .expect("cancel during starting finalizes");
    assert!(status.cancelled);
    let info = cleanup.info.lock().expect("cleanup lock").clone();
    assert!(
        info.as_ref()
            .is_some_and(|i| i.cancelled && i.exit_code.is_none()),
        "cleanup hook ran with cancel semantics: {info:?}"
    );
}

#[test]
fn actor_blocked_sink_lags_cursor_but_journal_keeps_moving() {
    let Some(fixture) = common::locate_fixture() else {
        eprintln!("SKIP: term-fixture binary not found (cargo build -p term-fixture)");
        return;
    };
    let pty = Arc::new(spawn_fixture(&fixture, &["echo"]));
    let events = Arc::new(Mutex::new(Vec::new()));
    let blocked = Arc::new(AtomicBool::new(true)); // sink starts blocked
    let journal = SharedJournal::default();

    let mut config = SessionActorConfig::new(SessionId::generate(), 80, 24, pty);
    config.sink = Box::new(CollectSink {
        events: Arc::clone(&events),
        blocked: Arc::clone(&blocked),
    });
    config.journal = Box::new(journal.clone());
    let (handle, _join) = start(config);

    handle
        .write_input("i1", b"blocked-sink-probe\n")
        .expect("write");

    // Journal receives the echo even while the sink is blocked.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let got = journal
            .0
            .lock()
            .expect("journal lock")
            .records
            .iter()
            .any(|r| matches!(&r.kind, MemRecordKind::Output(b) if b.windows(b"blocked-sink".len()).any(|w| w == b"blocked-sink")));
        if got {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "journal never recorded the output"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        collect_output(&events.lock().expect("sink lock")).is_empty(),
        "blocked sink received nothing"
    );

    // Unblock: the cursor catches up and nothing was dropped.
    blocked.store(false, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let bytes = collect_output(&events.lock().expect("sink lock"));
        if bytes.windows(18).any(|w| w == b"blocked-sink-probe") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "sink never caught up after unblock"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    handle.cancel();
    assert!(handle.wait_finalized(Duration::from_secs(10)).is_some());
}

/// Watch whose platform never answers (`try_owned_alive == None`).
struct UnknownWatch;

impl DescendantWatch for UnknownWatch {
    fn owned_alive(&self) -> usize {
        0
    }
    fn try_owned_alive(&self) -> Option<usize> {
        None
    }
}

/// An unknown owned-descendant count is not "empty": the actor keeps
/// draining past the timeout instead of closing a pty whose group it cannot
/// see (03-resources: unknown is reported, never zeroed). Cancel still ends
/// the session.
#[test]
fn actor_unknown_descendant_count_keeps_draining_until_cancel() {
    let Some(fixture) = common::locate_fixture() else {
        eprintln!("SKIP: term-fixture binary not found (cargo build -p term-fixture)");
        return;
    };
    let pty = Arc::new(spawn_fixture(
        &fixture,
        &["exit", "--code", "0", "--delay-ms", "100"],
    ));
    let mut config = SessionActorConfig::new(SessionId::generate(), 80, 24, pty);
    config.descendants = Arc::new(UnknownWatch);
    config.drain_timeout = Duration::from_millis(300);
    config.exit_poll_interval = Duration::from_millis(50);
    let (handle, _join) = start(config);

    let draining = handle.wait_status(Duration::from_secs(15), |s| {
        s.lifecycle == SessionLifecycle::Draining
    });
    assert!(draining.is_some(), "must pass through Draining");
    std::thread::sleep(Duration::from_millis(1200));
    let status = handle.status();
    assert!(
        matches!(
            status.lifecycle,
            SessionLifecycle::Running | SessionLifecycle::Draining
        ),
        "unknown group must not be treated as empty: {status:?}"
    );
    assert!(status.root_exited);

    handle.cancel();
    let status = handle
        .wait_finalized(Duration::from_secs(10))
        .expect("cancel finalizes a session with an unknown group");
    assert!(status.cancelled);
    assert_eq!(status.exit_code, Some(0));
}

/// Unix: a descendant that keeps the slave open after the root shell exits
/// used to block the reader thread (and the actor's `join`) for as long as
/// it lived — one leaked pty + three threads per such session. The polling
/// Cancel must end a session whose foreground program ignores SIGHUP
/// (`nohup`, `trap '' HUP`): the finalize HUP is followed by a bounded
/// grace and SIGKILL on the direct child, so a shell session never
/// outlives its cancel (02-runner §7 applied to the direct child).
#[cfg(unix)]
#[test]
fn actor_cancel_escalates_to_sigkill_when_the_child_ignores_hup() {
    // `exec` keeps the pid: the ignored-HUP disposition survives exec, so
    // the direct child itself is the HUP-immune process.
    let script = "trap '' HUP; echo READY; exec sleep 30";
    let argv = vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
    let pty = Arc::new(
        PtyHandle::spawn(80, 24, "/bin/sh", &argv, &BTreeMap::new(), &[], None).expect("spawn sh"),
    );
    let child_pid = pty.pid().expect("child pid") as i32;
    let journal = SharedJournal::default();
    let mut config = SessionActorConfig::new(SessionId::generate(), 80, 24, Arc::clone(&pty));
    config.journal = Box::new(journal.clone());
    config.exit_poll_interval = Duration::from_millis(50);
    let (handle, join) = start(config);

    let ready_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output: Vec<u8> = journal
            .0
            .lock()
            .expect("journal lock")
            .records
            .iter()
            .filter_map(|r| match &r.kind {
                MemRecordKind::Output(bytes) => Some(bytes.clone()),
                MemRecordKind::Resize { .. } => None,
            })
            .flatten()
            .collect();
        if output.windows(5).any(|w| w == b"READY") {
            break;
        }
        assert!(
            Instant::now() < ready_deadline,
            "sleep never announced READY"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    let started = Instant::now();
    handle.cancel();
    let status = handle
        .wait_finalized(Duration::from_secs(8))
        .expect("cancel finalizes despite the ignored HUP");
    assert!(status.cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "escalation must be prompt, took {:?}",
        started.elapsed()
    );
    join.join().expect("actor thread");
    // The direct child was reaped by the actor (SIGKILL after the grace):
    // probing it with signal 0 must fail with ESRCH.
    // SAFETY: signal 0 only probes existence.
    let probe = unsafe { libc::kill(child_pid, 0) };
    assert_ne!(probe, 0, "HUP-immune child must be gone after cancel");
}

/// reader lets `close()` end the thread; the descendant itself is never
/// signalled (unproven ownership).
#[cfg(unix)]
#[test]
fn actor_finalize_joins_reader_while_a_descendant_holds_the_slave() {
    // `trap '' HUP` is inherited by the background sleep, so neither the
    // session leader's exit nor our hangup ends it.
    let script = "trap '' HUP; sleep 30 & echo BG=$!; exit 0";
    let argv = vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
    let pty = Arc::new(
        PtyHandle::spawn(80, 24, "/bin/sh", &argv, &BTreeMap::new(), &[], None).expect("spawn sh"),
    );
    let journal = SharedJournal::default();
    let mut config = SessionActorConfig::new(SessionId::generate(), 80, 24, pty);
    config.journal = Box::new(journal.clone());
    config.drain_timeout = Duration::from_millis(300);
    config.exit_poll_interval = Duration::from_millis(50);
    let (handle, join) = start(config);

    let status = handle
        .wait_finalized(Duration::from_secs(10))
        .expect("root exit finalizes the session");
    assert_eq!(status.exit_code, Some(0));

    // The actor thread joins its reader before returning: with a blocking
    // master read this waited for the 30 s sleep.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(join.join().is_ok());
    });
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(3)),
        Ok(true),
        "actor thread must join promptly while a descendant holds the slave"
    );

    // Recover the background pid from the journal and prove we left it
    // alone (never kill unproven descendants); then clean it up ourselves.
    let output: Vec<u8> = journal
        .0
        .lock()
        .expect("journal lock")
        .records
        .iter()
        .filter_map(|r| match &r.kind {
            MemRecordKind::Output(bytes) => Some(bytes.clone()),
            MemRecordKind::Resize { .. } => None,
        })
        .flatten()
        .collect();
    let text = String::from_utf8_lossy(&output);
    let pid: i32 = text
        .split("BG=")
        .nth(1)
        .and_then(|rest| {
            rest.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .ok()
        })
        .unwrap_or_else(|| panic!("background pid not found in output {text:?}"));
    // SAFETY: signal 0 only probes existence.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    // SAFETY: cleanup of the test's own background process.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    assert!(
        alive,
        "the unowned background sleep must not be signalled by finalize"
    );
}

// -- stalled input: a program that stops reading must not wedge the session --

fn journal_text(journal: &SharedJournal) -> String {
    let output: Vec<u8> = journal
        .0
        .lock()
        .expect("journal lock")
        .records
        .iter()
        .filter_map(|r| match &r.kind {
            MemRecordKind::Output(bytes) => Some(bytes.clone()),
            MemRecordKind::Resize { .. } => None,
        })
        .flatten()
        .collect();
    String::from_utf8_lossy(&output).into_owned()
}

/// Raw-mode tty whose readers never read (a hung TUI): the tty input queue
/// fills after ~1 KiB and a blocking master write never returns. Before the
/// non-blocking writer, the writer thread held its lock inside `write()`
/// forever, so every later key was "queued" into the void and finalize's
/// `pty.close()` blocked behind it — the session never finalized while a
/// HUP-immune background process kept the slave open. Now the stall is
/// visible, new input is refused, and cancel finalizes promptly.
#[cfg(unix)]
#[test]
fn actor_stalled_input_is_reported_refused_and_does_not_wedge_finalize() {
    let script = "stty raw -echo; trap '' HUP; sleep 30 & echo BG=$!; echo READY; exec sleep 30";
    let argv = vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
    let pty = Arc::new(
        PtyHandle::spawn(80, 24, "/bin/sh", &argv, &BTreeMap::new(), &[], None).expect("spawn sh"),
    );
    let journal = SharedJournal::default();
    let mut config = SessionActorConfig::new(SessionId::generate(), 80, 24, Arc::clone(&pty));
    config.journal = Box::new(journal.clone());
    config.exit_poll_interval = Duration::from_millis(50);
    let (handle, join) = start(config);

    let ready_deadline = Instant::now() + Duration::from_secs(5);
    while !journal_text(&journal).contains("READY") {
        assert!(
            Instant::now() < ready_deadline,
            "script never announced READY"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let bg_pid: i32 = journal_text(&journal)
        .split("BG=")
        .nth(1)
        .and_then(|rest| {
            rest.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .ok()
        })
        .expect("background pid in output");

    // Far more than the tty input queue holds; nobody ever reads it.
    let chunk = vec![b'x'; 4096];
    for i in 0..4 {
        handle
            .write_input(&format!("fill-{i}"), &chunk)
            .expect("queue accepts the first chunks");
    }
    let stall_deadline = Instant::now() + Duration::from_secs(3);
    while handle.input_blocked_for().is_none() {
        assert!(
            Instant::now() < stall_deadline,
            "a full tty input queue must be reported as an input stall"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // Past the refusal threshold new input is refused, not queued.
    std::thread::sleep(term_pty::actor::INPUT_STALL_REJECT + Duration::from_millis(200));
    match handle.write_input_await("after-stall", b"y", Duration::from_millis(750)) {
        Err(ActorError::InputStalled { blocked_ms }) => assert!(blocked_ms >= 2_000),
        other => panic!("expected InputStalled, got {other:?}"),
    }

    let started = Instant::now();
    handle.cancel();
    let finalized = handle.wait_finalized(Duration::from_secs(8));
    // SAFETY: cleanup of the test's own HUP-immune background process.
    unsafe {
        libc::kill(bg_pid, libc::SIGKILL);
    }
    let status = finalized.expect("cancel must finalize despite the stalled writer");
    assert!(status.cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "finalize must not wait on the stalled write, took {:?}",
        started.elapsed()
    );
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(join.join().is_ok());
    });
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(3)),
        Ok(true),
        "actor thread (and its writer thread) must join promptly"
    );
}

// -- degraded journal: transient failure retries instead of stopping -------

/// Journal double whose output appends fail the first `fail_first` calls
/// with a bare (transient) I/O error, then succeed. Resize appends always
/// succeed so the mandatory initial size lands.
struct FlakyJournal {
    inner: term_pty::actor::MemJournal,
    fail_first: std::sync::atomic::AtomicUsize,
}

impl JournalSink for FlakyJournal {
    fn append_output(&mut self, data: &[u8]) -> std::io::Result<u64> {
        let left = self.fail_first.load(std::sync::atomic::Ordering::Acquire);
        if left > 0 {
            self.fail_first
                .store(left - 1, std::sync::atomic::Ordering::Release);
            return Err(std::io::Error::other("fixture: disk full"));
        }
        self.inner.append_output(data)
    }
    fn append_resize(&mut self, cols: u16, rows: u16) -> std::io::Result<u64> {
        self.inner.append_resize(cols, rows)
    }
}

#[test]
fn actor_transient_journal_failure_degrades_then_recovers_in_order() {
    let Some(fixture) = common::locate_fixture() else {
        eprintln!("SKIP: term-fixture binary not found (cargo build -p term-fixture)");
        return;
    };
    let pty = Arc::new(spawn_fixture(&fixture, &["echo"]));
    let events = Arc::new(Mutex::new(Vec::new()));
    let journal = Arc::new(Mutex::new(FlakyJournal {
        inner: term_pty::actor::MemJournal::new(),
        fail_first: std::sync::atomic::AtomicUsize::new(2),
    }));
    struct SharedFlaky(Arc<Mutex<FlakyJournal>>);
    impl JournalSink for SharedFlaky {
        fn append_output(&mut self, data: &[u8]) -> std::io::Result<u64> {
            self.0.lock().expect("journal lock").append_output(data)
        }
        fn append_resize(&mut self, cols: u16, rows: u16) -> std::io::Result<u64> {
            self.0
                .lock()
                .expect("journal lock")
                .append_resize(cols, rows)
        }
    }

    let mut config = SessionActorConfig::new(SessionId::generate(), 80, 24, Arc::clone(&pty));
    config.sink = Box::new(CollectSink {
        events: Arc::clone(&events),
        blocked: Arc::new(AtomicBool::new(false)),
    });
    config.journal = Box::new(SharedFlaky(Arc::clone(&journal)));
    let (handle, join) = start(config);

    handle.write_input("d1", b"deg-one\n").expect("write");
    // The error surfaced while degraded…
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if handle.status().journal_error.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        handle.status().journal_error.is_some(),
        "degraded journal must surface journal_error"
    );
    // …and recovery replays the stashed bytes to the sink and the journal.
    let deadline_output = Instant::now() + Duration::from_secs(20);
    let mut seen = Vec::new();
    while Instant::now() < deadline_output {
        seen = collect_output(&events.lock().expect("sink lock"));
        if seen.windows(b"deg-one".len()).any(|w| w == b"deg-one") {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        seen.windows(b"deg-one".len()).any(|w| w == b"deg-one"),
        "output must reappear after the journal recovers"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && handle.status().journal_error.is_some() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        handle.status().journal_error,
        None,
        "recovery must clear journal_error"
    );
    let records = journal.lock().expect("journal lock").inner.records.clone();
    assert!(
        records.iter().any(|r| matches!(
            &r.kind,
            term_pty::actor::MemRecordKind::Output(bytes) if bytes.windows(7).any(|w| w == b"deg-one")
        )),
        "recovered journal must contain the degraded output"
    );
    let seqs: Vec<u64> = records.iter().map(|r| r.seq).collect();
    let mut sorted = seqs.clone();
    sorted.sort_unstable();
    assert_eq!(seqs, sorted, "journal seq stays ordered across degradation");

    handle.cancel();
    join.join().expect("actor finalizes");
}

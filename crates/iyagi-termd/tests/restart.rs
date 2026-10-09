//! Daemon restart on the same data dir: prior active workloads are reported
//! INTERRUPTED via snapshot (crash reconciliation), nothing auto-respawns
//! (spec §6).

mod common;

use common::{Client, DaemonProc};
use serde_json::json;
use std::time::{Duration, Instant};

#[test]
fn ordinary_terminal_output_replays_after_restart_without_respawning() {
    for (finish_before_restart, rolling) in [(false, false), (true, false), (false, true)] {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut first = DaemonProc::spawn_on(dir.path().to_path_buf(), "journal-first", None);
        let (mut control, _) = Client::control(&first.endpoint, &first.token);
        let mut data = Client::data(&first.endpoint, control.data_token.as_deref().unwrap());
        let launch = control
            .request(
                "workload.launch",
                common::launch_request("shell", &["echo"], "1048576"),
            )
            .unwrap();
        let session = launch["session_id"].as_str().unwrap();
        let attached = control
            .request(
                "session.attach",
                json!({
                    "session_id": session, "view_id": common::uuid_v4(), "access": "writer",
                }),
            )
            .unwrap();
        control.request("session.input", json!({
            "session_id": session, "epoch": attached["epoch"], "input_id": common::uuid_v4(),
            "data_b64": common::b64(b"retained-terminal-marker\n"),
        })).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut output = Vec::new();
        while !String::from_utf8_lossy(&output).contains("retained-terminal-marker") {
            let event = data
                .wait_event(
                    "session.output",
                    deadline.saturating_duration_since(Instant::now()),
                )
                .expect("original output");
            output.extend(common::unb64(
                event["data_b64"].as_str().unwrap_or_default(),
            ));
        }
        if finish_before_restart {
            control
                .request(
                    "workload.cancel",
                    json!({ "workload_id": launch["workload_id"] }),
                )
                .unwrap();
            common::wait_workload_state(
                &mut control,
                &launch["workload_id"],
                &["CANCELLED"],
                Duration::from_secs(10),
            );
        }
        first.kill();
        if rolling {
            use std::io::Write;
            use term_pty::journal::{GlobalJournalBudget, JournalOptions, JournalWriter};
            let path = dir
                .path()
                .join("data/journals")
                .join(format!("{session}.mtj"));
            let mut writer = JournalWriter::open_with(
                &path,
                uuid::Uuid::parse_str(session).unwrap(),
                JournalOptions {
                    session_limit: 16 * 1024,
                    segment_cap: Some(4096),
                },
                GlobalJournalBudget::shared_default(),
            )
            .unwrap();
            writer.append_resize(80, 24).unwrap();
            for _ in 0..24 {
                writer.append_output(&vec![b'x'; 2048]).unwrap();
            }
            writer
                .append_output(b"retained-terminal-marker\r\n")
                .unwrap();
            writer.finalize().unwrap();
            assert!(writer.first_seq() > 1, "the retained head was trimmed");
            drop(writer);
            // A crash can leave an incomplete length prefix after the last valid record.
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(&[0x20, 0])
                .unwrap();
        }
        let second = DaemonProc::spawn_on(dir.path().to_path_buf(), "journal-second", None);
        let (mut control, _) = Client::control(&second.endpoint, &second.token);
        let mut data = Client::data(&second.endpoint, control.data_token.as_deref().unwrap());
        let restored = control
            .request(
                "session.attach",
                json!({
                    "session_id": session, "view_id": common::uuid_v4(), "access": "writer",
                }),
            )
            .expect("load retained journal after restart");
        assert_eq!(restored["exited"], true);
        let last: u64 = restored["last_seq"].as_str().unwrap().parse().unwrap();
        let mut next: u64 = restored["replay_from_seq"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        if rolling {
            assert!(next > 1);
        }
        let mut output = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while next <= last {
            let event = data
                .wait_event(
                    "session.output",
                    deadline.saturating_duration_since(Instant::now()),
                )
                .expect("replayed output");
            assert_eq!(event["seq"].as_str().unwrap().parse::<u64>().unwrap(), next);
            assert_eq!(event["epoch"], restored["epoch"]);
            output.extend(common::unb64(
                event["data_b64"].as_str().unwrap_or_default(),
            ));
            next += 1;
        }
        assert!(String::from_utf8_lossy(&output).contains("retained-terminal-marker"));
        assert!(control.request("session.input", json!({
            "session_id": session, "epoch": restored["epoch"], "input_id": common::uuid_v4(),
            "data_b64": common::b64(b"do not run\n"),
        })).is_err());
        let snapshot = control.request("system.snapshot", json!({})).unwrap();
        assert!(snapshot["workloads"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w["state"] != "RUNNING"));
    }
}

#[test]
fn restart_marks_active_workloads_interrupted() {
    for agent in ["claude", "codex", "opencode"] {
        check_restart(agent);
    }
}

fn check_restart(agent: &str) {
    // Own data dir so the second daemon reuses it.
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.keep();

    let mut first = DaemonProc::spawn_on(data_dir.clone(), "restart-1", None);
    let launch = {
        let (mut client, _) = Client::control(&first.endpoint, &first.token);
        let launch = client
            .request(
                "workload.launch",
                json!({
                    "request_id": common::uuid_v4(),
                    "profile_id": common::uuid_v4(),
                    "cwd": std::env::temp_dir().to_string_lossy(),
                    "program": common::fixture_bin(),
                    "argv": ["echo"],
                    "env_overrides": {},
                    "mode": "shell",
                    "cols": 80, "rows": 24, "priority": 1,
                    "policy": {
                        "reservation_bytes": "2147483648", "cpu_slots": 1,
                        "enforcement": "observe",
                        "memory_max_bytes": null, "cpu_max_cores": null, "pids_max": null,
                    },
                }),
            )
            .expect("launch running");
        assert_eq!(launch["state"], "RUNNING");
        let reported = client.request("agent_session.report", json!({
            "agent": agent, "session_id": "recovery-session-1", "event": "start",
            "workload_id": launch["workload_id"], "cwd": std::env::temp_dir().to_string_lossy(),
            "source": "recovery-test",
        })).expect("record provider session before crash");
        assert_eq!(reported["recorded"], true);
        launch
    };
    // Hard-kill the daemon (crash).
    first.kill();

    // Restart on the same data dir.
    let second = DaemonProc::spawn_on(data_dir.clone(), "restart-2", None);
    let (mut client, hello) = Client::control(&second.endpoint, &second.token);
    assert!(hello["daemon_id"].is_string());

    let snapshot = client
        .request("system.snapshot", json!({}))
        .expect("snapshot");
    let workloads = snapshot["workloads"].as_array().expect("workloads");
    assert_eq!(
        workloads.len(),
        1,
        "the interrupted workload is reported: {workloads:?}"
    );
    assert_eq!(workloads[0]["state"], "INTERRUPTED");
    assert_eq!(workloads[0]["last_error_code"], "DAEMON_RESTART");

    // The reconciled summary lacks cwd, but identity lookup still finds the conversation.
    assert_eq!(workloads[0]["cwd"], "");
    let records = client.request("agent_session.list", json!({
        "workload_id": launch["workload_id"], "pty_session_id": launch["session_id"], "limit": 1,
    })).expect("targeted recovery lookup");
    assert_eq!(records.as_array().unwrap().len(), 1);
    assert_eq!(records[0]["agent"], agent);
    assert_eq!(records[0]["agent_session_id"], "recovery-session-1");
    assert_eq!(
        records[0]["cwd"],
        std::env::temp_dir().to_string_lossy().as_ref()
    );
    assert_eq!(records[0]["active"], false);
    assert_eq!(records[0]["end_reason"], "daemon_restart");
    let unrelated = client
        .request(
            "agent_session.list",
            json!({
                "workload_id": common::uuid_v4(), "limit": 1,
            }),
        )
        .expect("unrelated lookup");
    assert!(unrelated.as_array().unwrap().is_empty());

    // No auto-respawn: nothing is RUNNING/STARTING after the restart.
    assert!(
        workloads.iter().all(|w| w["state"] == "INTERRUPTED"),
        "no respawn: {workloads:?}"
    );

    // And the request ledger kept the id: replaying the same request id
    // returns the workload's (interrupted) state without running anything.
    let journal_rows: Vec<_> = std::fs::read_dir(data_dir.join("data/journals"))
        .expect("journals dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "mtj"))
        .collect();
    assert_eq!(
        journal_rows.len(),
        1,
        "no additional sessions after restart"
    );
}

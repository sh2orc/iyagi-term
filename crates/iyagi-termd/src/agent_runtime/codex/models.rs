//! # Codex model catalog (`model/list`, ticket O08, docs/orchestration/03-adapters.md §3)
//!
//! The adapter's [`super::Engine::handshake`] already calls `model/list`, but
//! only to assert that the bound model is advertised — the catalog itself is
//! discarded. Binding setup needs the opposite: the full list of selectable
//! models plus the reasoning efforts each one advertises, so the UI stops
//! offering a single hard-coded constant.
//!
//! [`collect`] is pure over [`ProtocolPeer`]: the very same code path runs
//! against a recorded JSONL transcript (`fixtures/streams/model_list.jsonl`)
//! and against a live child, so the wire sequence is evidence-backed offline.
//! [`list`] adds only the process lifetime — spawn, bounded wait, guaranteed
//! reap.
//!
//! Protocol shape is pinned by the generated schemas from codex-cli 0.153.4
//! (`fixtures/v2/ModelListParams.json`, `fixtures/v2/ModelListResponse.json`):
//! `initialize` → `initialized` → `model/list { includeHidden: false }`,
//! following `result.nextCursor` until it is null. This session never starts
//! a thread, so no approval request can arrive; anything inbound that is not
//! the awaited response is ignored.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use serde_json::{json, Value};
use term_contracts::mission::rpc::ProbeModel;

use super::daemon_isolation;
use super::peer::{LivePeer, PeerEvent, ProtocolPeer};
use super::{is_response_to, APP_SERVER_ARGV};

/// A model catalog is untrusted app-server output that lands in a persisted
/// binding and in a picker label, so what this module keeps is bounded the
/// same way the adapter bounds approval traffic (03 §2 caps, like the raw
/// line cap): a page count so a server that always returns a cursor cannot
/// hold the probe open, an entry count so a flood cannot grow the stored
/// catalog, and per-string byte/character limits so no id or effort can
/// smuggle control characters into a label.
///
/// Overflow is never an error: extra pages and extra entries are dropped and
/// the models collected so far are returned, because a truncated picker is
/// still usable. An individual entry whose id or effort breaks a text cap is
/// dropped whole — a sanitized id would no longer identify the model.
const MAX_MODELS: usize = 128;
const MAX_PAGES: usize = 8;
const MAX_MODEL_ID_BYTES: usize = 128;
const MAX_EFFORTS_PER_MODEL: usize = 16;
const MAX_EFFORT_BYTES: usize = 32;

/// Collect the models an already-connected app-server advertises.
/// Pure over the transport so recorded transcripts can drive it.
pub fn collect(peer: &dyn ProtocolPeer) -> Result<Vec<ProbeModel>, String> {
    // Request ids are local to this session and start at 1 (the transcripts
    // record them, so they must stay a plain increasing counter).
    let mut next_id: u64 = 0;

    // (1) initialize — same clientInfo and empty capabilities the adapter's
    // handshake sends, so a server that gates on them behaves identically.
    next_id += 1;
    let response = request(
        peer,
        next_id,
        "initialize",
        json!({
            "clientInfo": {
                "name": "iyagi",
                "title": null,
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {},
        }),
    )?;
    if response.get("error").is_some() {
        return Err("initialize was rejected".into());
    }
    if !response.get("result").is_some_and(Value::is_object) {
        return Err("initialize response result is not an object".into());
    }

    // (2) initialized notification (ClientNotification.json).
    notify(peer, "initialized", json!({}))?;

    // (3) model/list, following the cursor while the caps allow.
    let mut models: Vec<ProbeModel> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut params = json!({ "includeHidden": false });
        if let Some(cursor) = cursor.as_deref() {
            params["cursor"] = Value::String(cursor.to_string());
        }
        next_id += 1;
        let response = request(peer, next_id, "model/list", params)?;
        if response.get("error").is_some() {
            return Err("model/list was rejected".into());
        }
        let Some(entries) = response["result"]["data"].as_array() else {
            return Err("model/list response lacks result.data".into());
        };
        for entry in entries {
            if models.len() >= MAX_MODELS {
                break;
            }
            let Some(model) = probe_model(entry) else {
                continue;
            };
            // Server order is the picker order; a repeated id keeps the
            // first entry the server chose to show.
            if seen.insert(model.id.clone()) {
                models.push(model);
            }
        }
        if models.len() >= MAX_MODELS {
            break;
        }
        // `nextCursor: null` (or absent) is the documented end of the list.
        cursor = response["result"]["nextCursor"]
            .as_str()
            .filter(|cursor| !cursor.is_empty())
            .map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(models)
}

/// Spawn `codex app-server`, collect its model list, and always reap the
/// child. Bounded by `timeout`; never blocks past it.
///
/// Failure reasons are short and fixed: this path feeds binding setup, whose
/// message is shown to the user, so the peer's stderr tail (which can carry
/// paths, tokens-adjacent text, or arbitrary provider output) never reaches
/// it.
pub fn list(program: &Path, cwd: &Path, timeout: Duration) -> Result<Vec<ProbeModel>, String> {
    let mut argv = daemon_isolation::argv_prefix(program);
    argv.extend(APP_SERVER_ARGV.iter().map(|arg| arg.to_string()));
    let peer =
        LivePeer::spawn(program, &argv, cwd).map_err(|_| "app-server could not be started")?;
    // `LivePeer::recv` blocks with no deadline, so the collection runs on its
    // own thread and the deadline is enforced here. The worker is deliberately
    // detached: on a timeout it is still parked in `recv`, and `close()` below
    // is what drops stdin, reaps the child, and unblocks it.
    let (report, results) = mpsc::channel();
    let worker = Arc::clone(&peer);
    let spawned = std::thread::Builder::new()
        .name("codex-model-list".into())
        .spawn(move || {
            let _ = report.send(collect(worker.as_ref()));
        });
    if spawned.is_err() {
        // The child is already running: reap it before giving up.
        peer.close();
        return Err("model/list worker could not be started".into());
    }
    let collected = match results.recv_timeout(timeout) {
        Ok(collected) => collected,
        Err(mpsc::RecvTimeoutError::Timeout) => Err("model/list timed out".into()),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("model/list ended without a result".into())
        }
    };
    // Always reap, on both the timeout and the normal path: nothing else
    // owns this child.
    peer.close();
    collected
}

/// Send a request and wait for the response carrying `id`. Inbound traffic
/// that is not that response is ignored — a catalog read starts no thread,
/// so nothing here needs an answer.
fn request(peer: &dyn ProtocolPeer, id: u64, method: &str, params: Value) -> Result<Value, String> {
    let message = json!({ "id": id, "method": method, "params": params });
    peer.send(&message)
        .map_err(|_| format!("{method} transport failed"))?;
    loop {
        match peer.recv() {
            PeerEvent::Message(value) => {
                if is_response_to(&value, id) {
                    return Ok(value);
                }
            }
            PeerEvent::Eof => {
                return Err(format!("app-server ended while awaiting {method}"));
            }
            PeerEvent::ConnectionLost => {
                return Err(format!(
                    "app-server connection lost while awaiting {method}"
                ));
            }
            PeerEvent::Overcap => {
                return Err(format!("{method} exceeded the 1 MiB raw line cap (03 §2)"));
            }
        }
    }
}

/// Send a notification (no id, no response).
fn notify(peer: &dyn ProtocolPeer, method: &str, params: Value) -> Result<(), String> {
    peer.send(&json!({ "method": method, "params": params }))
        .map_err(|_| format!("{method} transport failed"))
}

/// Map one `ModelListResponse.data` entry onto a [`ProbeModel`]. `None` when
/// the entry is hidden, unidentifiable, or breaks a text cap.
fn probe_model(entry: &Value) -> Option<ProbeModel> {
    if entry.get("hidden").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    // `model` first: the handshake's advertisement check accepts either
    // field, and `model` is the value `thread/start` actually takes.
    let id = text(entry.get("model"))
        .or_else(|| text(entry.get("id")))
        .filter(|id| admissible(id, MAX_MODEL_ID_BYTES))?;
    let mut efforts: Vec<String> = Vec::new();
    if let Some(options) = entry
        .get("supportedReasoningEfforts")
        .and_then(Value::as_array)
    {
        for option in options {
            if efforts.len() >= MAX_EFFORTS_PER_MODEL {
                break;
            }
            let Some(effort) =
                text(option.get("reasoningEffort")).filter(|e| admissible(e, MAX_EFFORT_BYTES))
            else {
                continue;
            };
            if !efforts.contains(&effort) {
                efforts.push(effort);
            }
        }
    }
    if efforts.is_empty() {
        // Nothing advertised: the catalog default is still a valid choice,
        // and an empty list would leave the picker with no effort at all.
        if let Some(default) =
            text(entry.get("defaultReasoningEffort")).filter(|e| admissible(e, MAX_EFFORT_BYTES))
        {
            efforts.push(default);
        }
    }
    Some(ProbeModel { id, efforts })
}

/// A non-empty JSON string, owned.
fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Untrusted catalog text is admitted only inside `max` bytes and without
/// control characters. Ids and efforts are identifiers rendered as labels,
/// so tabs and newlines are rejected too (stricter than the mission-wide
/// `has_no_control_chars`, which allows whitespace inside prose).
fn admissible(value: &str, max: usize) -> bool {
    value.len() <= max && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::codex::{RecordedPeer, TranscriptLine};
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/agent_runtime/codex/fixtures/streams")
            .join(name)
    }

    /// The gate/reply preamble every `collect` run sends before `model/list`.
    /// Gates match on method, so the expected sends need nothing else.
    fn preamble() -> Vec<TranscriptLine> {
        vec![
            TranscriptLine::C {
                msg: json!({"method": "initialize"}),
            },
            TranscriptLine::S {
                msg: json!({"id": 1, "result": {"userAgent": "codex/0.153.4 recorded fixture"}}),
            },
            TranscriptLine::C {
                msg: json!({"method": "initialized"}),
            },
        ]
    }

    /// One schema-complete `ModelListResponse.data` entry.
    fn entry(id: &str, efforts: &[&str]) -> Value {
        json!({
            "id": id,
            "model": id,
            "displayName": id,
            "description": "recorded fixture entry",
            "hidden": false,
            "isDefault": false,
            "defaultReasoningEffort": "medium",
            "supportedReasoningEfforts": efforts
                .iter()
                .map(|effort| json!({
                    "reasoningEffort": effort,
                    "description": "recorded fixture entry",
                }))
                .collect::<Vec<_>>(),
        })
    }

    fn ids(models: &[ProbeModel]) -> Vec<&str> {
        models.iter().map(|model| model.id.as_str()).collect()
    }

    #[test]
    fn collect_merges_pages_and_drops_hidden_entries() {
        let peer = RecordedPeer::from_file(&fixture("model_list.jsonl")).expect("fixture loads");
        let models = collect(peer.as_ref()).expect("catalog collected");
        assert_eq!(
            ids(&models),
            ["gpt-5.1-codex", "gpt-5.1-codex-mini", "gpt-5.1"],
            "both pages merge in server order, the hidden entry is dropped"
        );
        assert_eq!(models[0].efforts, ["low", "medium", "high"]);
        assert_eq!(
            models[1].efforts,
            ["medium"],
            "an entry with no advertised efforts falls back to its default"
        );
        assert_eq!(models[2].efforts, ["medium", "high"]);

        let sent = peer.sent_messages();
        assert_eq!(
            peer.sent_methods(),
            ["initialize", "initialized", "model/list", "model/list"]
        );
        assert_eq!(sent[0]["id"], json!(1));
        assert_eq!(sent[0]["params"]["clientInfo"]["name"], json!("iyagi"));
        assert_eq!(sent[2]["id"], json!(2));
        assert_eq!(sent[2]["params"], json!({"includeHidden": false}));
        assert_eq!(sent[3]["id"], json!(3));
        assert_eq!(
            sent[3]["params"],
            json!({"includeHidden": false, "cursor": "codex-model-page-2"}),
            "the second page repeats includeHidden and carries the cursor"
        );
        assert!(peer.mismatches().is_empty());
    }

    #[test]
    fn collect_falls_back_to_the_default_effort() {
        let mut lines = preamble();
        lines.push(TranscriptLine::C {
            msg: json!({"method": "model/list"}),
        });
        let without_efforts = entry("fixture-model-default", &[]);
        let mut without_anything = entry("fixture-model-bare", &[]);
        without_anything["defaultReasoningEffort"] = json!("");
        lines.push(TranscriptLine::S {
            msg: json!({
                "id": 2,
                "result": {
                    "data": [without_efforts, without_anything],
                    "nextCursor": null,
                },
            }),
        });
        let peer = RecordedPeer::new(lines);
        let models = collect(peer.as_ref()).expect("catalog collected");
        assert_eq!(models[0].efforts, ["medium"]);
        assert!(
            models[1].efforts.is_empty(),
            "no advertised and no default effort stays empty, not fabricated"
        );
    }

    #[test]
    fn collect_reports_a_rejected_model_list() {
        let mut lines = preamble();
        lines.push(TranscriptLine::C {
            msg: json!({"method": "model/list"}),
        });
        lines.push(TranscriptLine::S {
            msg: json!({"id": 2, "error": {"code": -32603, "message": "catalog unavailable"}}),
        });
        let peer = RecordedPeer::new(lines);
        assert_eq!(
            collect(peer.as_ref()),
            Err("model/list was rejected".to_string())
        );
    }

    #[test]
    fn collect_reports_eof_before_a_response() {
        let mut lines = preamble();
        lines.push(TranscriptLine::C {
            msg: json!({"method": "model/list"}),
        });
        lines.push(TranscriptLine::Eof);
        let peer = RecordedPeer::new(lines);
        assert_eq!(
            collect(peer.as_ref()),
            Err("app-server ended while awaiting model/list".to_string())
        );
    }

    #[test]
    fn collect_drops_entries_that_break_the_untrusted_text_caps() {
        let mut lines = preamble();
        lines.push(TranscriptLine::C {
            msg: json!({"method": "model/list"}),
        });
        let overlong_id = "g".repeat(MAX_MODEL_ID_BYTES + 1);
        let overlong_effort = "e".repeat(MAX_EFFORT_BYTES + 1);
        lines.push(TranscriptLine::S {
            msg: json!({
                "id": 2,
                "result": {
                    "data": [
                        entry(&overlong_id, &["medium"]),
                        entry("fixture-model\u{7}bell", &["medium"]),
                        entry("fixture-model-ok", &[overlong_effort.as_str(), "high"]),
                    ],
                    "nextCursor": null,
                },
            }),
        });
        let peer = RecordedPeer::new(lines);
        let models = collect(peer.as_ref()).expect("catalog collected");
        assert_eq!(
            ids(&models),
            ["fixture-model-ok"],
            "an over-long id and a control character drop the whole entry"
        );
        assert_eq!(
            models[0].efforts,
            ["high"],
            "an over-long effort drops only that effort"
        );
    }

    #[test]
    fn collect_stops_following_cursors_at_the_page_cap() {
        let mut lines = preamble();
        for page in 0..(MAX_PAGES + 3) {
            // Page N answers request id N+2 (1 is initialize).
            let id = page as u64 + 2;
            lines.push(TranscriptLine::C {
                msg: json!({"method": "model/list"}),
            });
            lines.push(TranscriptLine::S {
                msg: json!({
                    "id": id,
                    "result": {
                        "data": [entry(&format!("fixture-model-{page}"), &["medium"])],
                        "nextCursor": format!("codex-model-page-{}", page + 2),
                    },
                }),
            });
        }
        let peer = RecordedPeer::new(lines);
        let models = collect(peer.as_ref()).expect("catalog collected");
        assert_eq!(
            models.len(),
            MAX_PAGES,
            "an endless cursor stops at the cap"
        );
        assert_eq!(
            peer.sent_methods()
                .iter()
                .filter(|method| *method == "model/list")
                .count(),
            MAX_PAGES
        );
    }
}

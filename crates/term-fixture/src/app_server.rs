//! A live stdio protocol peer for daemon mission integration tests. The
//! fixture executable is never part of the product bundle.
use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn send(value: Value) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{value}").unwrap();
    out.flush().unwrap();
}
fn completed_report(text: &str) {
    let result = json!({"kind":"report","report_text":text,"knowledge":[]});
    send(
        json!({"method":"turn/completed","params":{"threadId":"fixture-thread","turn":{"id":"fixture-turn","status":"completed","items":[{"type":"agentMessage","id":"final","phase":"final_answer","text":json!({"result":result}).to_string()}]}}}),
    );
}
pub fn run(overrides: &[String]) -> i32 {
    let mut config = json!({});
    for entry in overrides {
        let Some((key, value)) = entry.split_once('=') else {
            return 8;
        };
        let Ok(value) = serde_json::from_str::<Value>(value) else {
            return 8;
        };
        let mut target = &mut config;
        let mut keys = key.split('.').peekable();
        while let Some(key) = keys.next() {
            if keys.peek().is_none() {
                target[key] = value;
                break;
            }
            if target.get(key).is_none() {
                target[key] = json!({});
            }
            target = &mut target[key];
        }
    }
    let mut account = if config
        .get("cli_auth_credentials_store")
        .is_some_and(|v| v == "ephemeral")
    {
        Value::Null
    } else {
        json!({"type":"chatgpt","email":null,"planType":"pro"})
    };
    let mut api_key = String::new();
    let mut waiting_for_mission_message = false;
    let mut drop_message_ack = false;
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else {
            return 1;
        };
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            return 2;
        };
        let method = request["method"].as_str().unwrap_or("");
        let id = request["id"].clone();
        if method.is_empty() && id == "fixture-approval" && request.get("result").is_some() {
            if request["result"]["decision"] != "accept" {
                return 7;
            }
            completed_report("approval received exactly once");
            continue;
        }
        let response = match method {
            "initialize" => {
                // Owned end-to-end fixture: fail before reading any task frame
                // on the first process only; later attempts run normally.
                if let Some(marker) = std::fs::read(".iyagi-transient-fixture.json")
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                    .and_then(|config| config["marker"].as_str().map(str::to_owned))
                {
                    match std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(marker)
                    {
                        Ok(_) => return 75,
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(_) => return 76,
                    }
                }
                json!({"userAgent":"iyagi-test-fixture","codexHome":std::env::var("CODEX_HOME").unwrap_or_default()})
            }
            "initialized" => continue,
            "config/read" => json!({"config":config,"origins":{}}),
            "mcpServerStatus/list" => json!({"data":[],"nextCursor":null}),
            "account/login/start" => {
                if config
                    .get("cli_auth_credentials_store")
                    .is_none_or(|v| v != "ephemeral")
                    || request["params"]["type"] != "apiKey"
                {
                    return 9;
                }
                api_key = request["params"]["apiKey"]
                    .as_str()
                    .unwrap_or_default()
                    .into();
                if api_key.is_empty() {
                    return 10;
                }
                if std::env::vars().any(|(_, v)| v.contains(&api_key))
                    || std::env::args().any(|v| v.contains(&api_key))
                {
                    return 11;
                }
                account = json!({"type":"apiKey"});
                json!({"type":"apiKey"})
            }
            "account/read" => json!({"account":account,"requiresOpenaiAuth":true}),
            "model/list" => {
                json!({"data":[{"id":"fixture-model","model":"fixture-model","displayName":"Fixture model"}],"nextCursor":null})
            }
            "thread/start" => {
                json!({"thread":{"id":"fixture-thread"},"model":"fixture-model","modelProvider":"openai"})
            }
            "turn/start" => {
                send(
                    json!({"id":id,"result":{"turn":{"id":"fixture-turn","status":"inProgress","items":[]}}}),
                );
                send(
                    json!({"method":"turn/started","params":{"threadId":"fixture-thread","turn":{"id":"fixture-turn","status":"inProgress","items":[]}}}),
                );
                let prompt = request["params"]["input"][0]["text"].as_str().unwrap_or("");
                match prompt {
                    "fixture-auth-echo" => {
                        completed_report(&format!("fixture credential echo: {api_key}"));
                        continue;
                    }
                    "fixture-report" => {
                        completed_report("supervised fixture result");
                        continue;
                    }
                    "fixture-hold" => {
                        send(
                            json!({"method":"item/agentMessage/delta","params":{"threadId":"fixture-thread","turnId":"fixture-turn","itemId":"hold","delta":"waiting for control"}}),
                        );
                        continue;
                    }
                    "fixture-approval" => {
                        send(
                            json!({"id":"fixture-approval","method":"item/commandExecution/requestApproval","params":{"threadId":"fixture-thread","turnId":"fixture-turn","itemId":"approval-tool","command":"fixture operation","cwd":std::env::current_dir().unwrap()}}),
                        );
                        continue;
                    }
                    "fixture-disconnect" => return 0,
                    "fixture-overcap" => {
                        let _ = std::io::stdout().write_all(&vec![b'x'; 1024 * 1024 + 1]);
                        return 0;
                    }
                    _ => {}
                }
                let Some(document) = prompt
                    .split("TASK_CONTEXT_JSON\n")
                    .nth(1)
                    .and_then(|p| p.split("\nEND_TASK_CONTEXT_JSON").next())
                else {
                    return 3;
                };
                let context: Value = serde_json::from_str(document).unwrap();
                if context["goal"] == "fixture-mission-message"
                    || context["goal"] == "fixture-mission-message-replacement"
                {
                    waiting_for_mission_message = true;
                    drop_message_ack = context["goal"] == "fixture-mission-message-replacement";
                    continue;
                }
                if context["goal"] == "fixture-mission-rate-limit" && context["task_kind"] == "plan"
                {
                    let reset = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        + 3;
                    send(
                        json!({"method":"account/rateLimits/updated","params":{"rateLimits":{
                            "rateLimitReachedType":"rate_limit_reached","primary":{"usedPercent":100,"resetsAt":reset},"secondary":null
                        }}}),
                    );
                }
                let first_plan = context["task_kind"] == "plan"
                    && context["tasks"].as_array().is_some_and(|tasks| {
                        tasks.iter().any(|task| {
                            task["id"] == context["task_id"] && task["attempt_count"] == 1
                        })
                    });
                if context["goal"] == "fixture-mission-invalid-first-plan" && first_plan {
                    // Valid provider JSON with the wrong task result kind.
                    // The host must retain failure and supply it for correction.
                    completed_report("Fixture returned a report instead of a plan.");
                    continue;
                }
                if context["goal"] == "fixture-mission-invalid-first-plan"
                    && context["task_kind"] == "plan"
                    && (!context["plan_repair"]["diagnostic"]
                        .as_str()
                        .is_some_and(|text| text.contains("kind=plan"))
                        || !context["plan_repair"]["rejected_answer"]
                            .as_str()
                            .is_some_and(|text| text.contains("Fixture returned a report")))
                {
                    return 9;
                }
                let required_repair = matches!(
                    context["goal"].as_str(),
                    Some("fixture-mission-required-repair" | "fixture-mission-required-exhausted")
                );
                if required_repair
                    && context["task_kind"] == "implement"
                    && context["task_contract"]["allowed_paths"][0] == "api.txt"
                {
                    let replacement = context["tasks"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|t| t["id"] == context["task_id"])
                        .unwrap()["replacement_of"]
                        .is_string();
                    if !replacement || context["goal"] == "fixture-mission-required-exhausted" {
                        send(
                            json!({"method":"turn/completed","params":{"threadId":"fixture-thread","turn":{"id":"fixture-turn","status":"failed","items":[],"error":{"codexErrorInfo":"internalServerError","message":"fixture required worker failure"}}}}),
                        );
                        continue;
                    }
                }
                let mut result = match context["task_kind"].as_str().unwrap() {
                    "plan" if required_repair && !context["failure_repair"].is_null() => {
                        let failures = context["failure_repair"]["failed_tasks"]
                            .as_array()
                            .unwrap();
                        if failures.is_empty()
                            || failures.iter().any(|f| {
                                !f["diagnostic"]
                                    .as_str()
                                    .is_some_and(|s| s.contains("Codex turn failed"))
                            })
                        {
                            return 10;
                        }
                        let tasks: Vec<_> = failures.iter().enumerate().map(|(i,f)| {
                            let old=&f["task"]; let contract=&old["contract"];
                            json!({"local_key":format!("replacement{i}"),"title":"Repair failed implementation","kind":old["kind"],"role":old["role"],"required":true,"parent_key":null,"depends_on_keys":[],"objective_text":"Correct the failed local implementation using its diagnostic.","requirement_ids":contract["requirement_ids"],"input_artifact_ids":[],"allowed_paths":contract["allowed_paths"],"expected_outputs":contract["expected_outputs"],"verification_ids":contract["verification_ids"],"specialty":null,"binding_id":old["binding_id"],"replacement_of":old["id"]})
                        }).collect();
                        let retired: Vec<_> =
                            failures.iter().map(|f| f["task"]["id"].clone()).collect();
                        json!({"kind":"plan","based_on_plan_revision":context["plan_revision"],"tasks":tasks,"retire_task_ids":retired,"rationale_text":"The retained local failure requires a replacement; preserve the independent completed task."})
                    }
                    "plan" => {
                        let mut tasks:Vec<_>=["api","ui"].into_iter().map(|key|json!({"local_key":key,"title":format!("Write {key}"),"kind":"implement","role":"builder","required":true,"parent_key":null,"depends_on_keys":[],"objective_text":format!("Create {key}.txt"),"requirement_ids":[context["requirements"][0]["id"]],"input_artifact_ids":[],"allowed_paths":[format!("{key}.txt")],"expected_outputs":["patch"],"verification_ids":[],"specialty":null,"binding_id":null,"replacement_of":null})).collect();
                        if context["goal"] == "fixture-mission-integration-conflict" {
                            for task in &mut tasks {
                                task["allowed_paths"]
                                    .as_array_mut()
                                    .unwrap()
                                    .push(json!("shared.txt"));
                            }
                        }
                        json!({"kind":"plan","based_on_plan_revision":context["plan_revision"],"tasks":tasks,"retire_task_ids":[],"rationale_text":"Two independent writer workspaces."})
                    }
                    "implement" => {
                        let file = context["task_contract"]["allowed_paths"][0]
                            .as_str()
                            .unwrap();
                        if !matches!(file, "api.txt" | "ui.txt") {
                            return 4;
                        }
                        std::fs::write(file, format!("{file} from a real fixture process\n"))
                            .unwrap();
                        if context["goal"] == "fixture-mission-integration-conflict" {
                            std::fs::write("shared.txt", format!("shared by {file}\n")).unwrap();
                        }
                        json!({"kind":"patch","report_text":"File written in the isolated workspace.","verification_claims":[]})
                    }
                    "integrate" if context["goal"] == "fixture-mission-integration-conflict" => {
                        let objective: Value =
                            serde_json::from_str(context["objective"].as_str().unwrap()).unwrap();
                        if context["role"] != "integrator"
                            || objective["conflict_run_id"].is_null()
                            || !std::fs::read_to_string("shared.txt")
                                .unwrap()
                                .contains("<<<<<<<")
                        {
                            return 11;
                        }
                        if let Ok(bytes) = std::fs::read(".iyagi-integration-recovery-fixture.json")
                        {
                            let config: Value = serde_json::from_slice(&bytes).unwrap();
                            let marker = config["marker"].as_str().unwrap();
                            if std::fs::OpenOptions::new()
                                .write(true)
                                .create_new(true)
                                .open(marker)
                                .is_ok()
                            {
                                std::fs::write("shared.txt", "unconfirmed integrator changes\n")
                                    .unwrap();
                                std::fs::write("unknown-only.txt", "quarantined fixture output\n")
                                    .unwrap();
                                while !std::path::Path::new(config["release"].as_str().unwrap())
                                    .exists()
                                {
                                    std::thread::sleep(std::time::Duration::from_millis(25));
                                }
                                return 0; // End without a provider result after daemon restart.
                            }
                        }
                        std::fs::write("shared.txt", "resolved by native integrator\n").unwrap();
                        json!({"kind":"patch","report_text":"Resolved shared file; host must validate and continue.","verification_claims":[]})
                    }
                    "review" => {
                        if !std::path::Path::new("api.txt").is_file()
                            || !std::path::Path::new("ui.txt").is_file()
                        {
                            return 5;
                        }
                        json!({"kind":"review","candidate_id":context["candidate"]["id"],"report_text":"Both files exist in the immutable candidate.","findings":[]})
                    }
                    _ => return 6,
                };
                if context["goal"] == "fixture-mission-policy-first-plan" && first_plan {
                    result["tasks"][0]["allowed_paths"] = json!(["../outside-workspace"]);
                }
                send(
                    json!({"method":"thread/tokenUsage/updated","params":{"tokenUsage":{"last":{"inputTokens":10,"outputTokens":20}}}}),
                );
                send(
                    json!({"method":"turn/completed","params":{"threadId":"fixture-thread","turn":{"id":"fixture-turn","status":"completed","items":[{"type":"agentMessage","id":"final","phase":"final_answer","text":json!({"result":result}).to_string()}]}}}),
                );
                continue;
            }
            "turn/steer" => {
                if request["params"]["expectedTurnId"] != "fixture-turn" {
                    return 8;
                }
                if waiting_for_mission_message && drop_message_ack {
                    drop_message_ack = false;
                    // The request was received, but its acknowledgement is
                    // deliberately lost. Keep the same provider turn alive.
                    continue;
                }
                send(json!({"id":id,"result":{"turnId":"fixture-turn"}}));
                if waiting_for_mission_message {
                    let result = json!({"kind":"blocked","code":"fixture_message_received","report_text":request["params"]["input"][0]["text"]});
                    send(
                        json!({"method":"turn/completed","params":{"threadId":"fixture-thread","turn":{"id":"fixture-turn","status":"completed","items":[{"type":"agentMessage","id":"final","phase":"final_answer","text":json!({"result":result}).to_string()}]}}}),
                    );
                    waiting_for_mission_message = false;
                    continue;
                }
                completed_report(
                    request["params"]["input"][0]["text"]
                        .as_str()
                        .unwrap_or("missing steer"),
                );
                continue;
            }
            "turn/interrupt" => {
                send(json!({"id":id,"result":{}}));
                send(
                    json!({"method":"turn/completed","params":{"threadId":"fixture-thread","turn":{"id":"fixture-turn","status":"interrupted","items":[]}}}),
                );
                continue;
            }
            _ => {
                send(
                    json!({"id":id,"error":{"code":-32601,"message":"unsupported fixture method"}}),
                );
                continue;
            }
        };
        send(json!({"id":id,"result":response}));
    }
    0
}

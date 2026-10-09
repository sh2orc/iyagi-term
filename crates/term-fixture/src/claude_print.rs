//! Deterministic stdout/stdin/exit fixture for the supervised print path.
use serde_json::json;
use std::io::{BufRead, Read, Write};

pub fn run(argv: &[String]) -> i32 {
    if !argv
        .windows(2)
        .any(|a| a == ["--output-format", "stream-json"])
    {
        return 2;
    }
    let model = argv
        .windows(2)
        .find(|a| a[0] == "--model")
        .map(|a| a[1].as_str())
        .unwrap_or("missing");
    let mut prompt = String::new();
    let scenario: serde_json::Value = std::fs::read(".iyagi-claude-fixture.json")
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(json!({}));
    if argv
        .windows(2)
        .any(|a| a == ["--input-format", "stream-json"])
    {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else {
                return 3;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                return 3;
            };
            if value["type"] == "control_request" {
                if value["request"]["subtype"] != "initialize" {
                    return 3;
                }
                std::fs::write(".iyagi-claude-init-received", b"initialized").unwrap();
                if scenario["mode"] == "exit_before_init_reply" {
                    return 75;
                }
                if scenario["mode"] == "hold_init" {
                    continue;
                }
                let mut account = if std::env::var_os("ANTHROPIC_AUTH_TOKEN").is_some() {
                    json!({"tokenSource":"ANTHROPIC_AUTH_TOKEN","apiProvider":"firstParty"})
                } else if std::env::var_os("ANTHROPIC_API_KEY").is_some() {
                    json!({"apiKeySource":"ANTHROPIC_API_KEY","tokenSource":"none","apiProvider":"firstParty"})
                } else if std::env::var_os("CLAUDE_CODE_OAUTH_TOKEN").is_some() {
                    json!({"tokenSource":"CLAUDE_CODE_OAUTH_TOKEN","apiProvider":"firstParty"})
                } else if scenario["managed"] == true {
                    json!({"subscriptionType":"max","apiProvider":"firstParty"})
                } else {
                    json!({"tokenSource":"none","apiProvider":"firstParty"})
                };
                if scenario["mode"] == "auth_mismatch" {
                    account["apiProvider"] = json!("gateway");
                }
                let permission = argv
                    .windows(2)
                    .find(|a| a[0] == "--permission-mode")
                    .map(|a| a[1].as_str())
                    .unwrap_or("default");
                let request_id = if scenario["mode"] == "wrong_id" {
                    json!("unowned")
                } else {
                    value["request_id"].clone()
                };
                println!(
                    "{}",
                    json!({"type":"control_response","response":{"subtype":"success","request_id":request_id,"response":{"account":account,"current_permission_mode":if scenario["mode"]=="permission_mismatch" {"bypassPermissions"} else {permission}}}})
                );
                let _ = std::io::stdout().flush();
            } else if value["type"] == "user" {
                prompt = value["message"]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .into();
                std::fs::write(".iyagi-claude-prompt-received", b"received").unwrap();
                let env_keys: Vec<_> = std::env::vars_os()
                    .map(|(k, _)| k.to_string_lossy().into_owned())
                    .collect();
                let observation = json!({"argv":argv,"env_keys":env_keys,"home":std::env::var("HOME").ok(),"config":std::env::var("CLAUDE_CONFIG_DIR").ok(),"base_url":std::env::var("ANTHROPIC_BASE_URL").ok()});
                std::fs::write(".iyagi-claude-observation.json", observation.to_string()).unwrap();
            } else {
                return 3;
            }
        }
        if prompt.is_empty() {
            return 0;
        }
    } else if std::io::stdin()
        .take(8 * 1024 * 1024 + 1)
        .read_to_string(&mut prompt)
        .is_err()
        || prompt.len() > 8 * 1024 * 1024
    {
        return 2;
    }
    println!(
        "{}",
        json!({"type":"system","subtype":"init","session_id":"fixture-print-owned","model":model,"tools":[],"mcp_servers":[]})
    );
    let _ = std::io::stdout().flush();
    if prompt == "hold" {
        std::thread::sleep(std::time::Duration::from_secs(30));
    }
    if scenario["mode"] == "hold_report" {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !std::path::Path::new(".iyagi-claude-release").exists() {
            if std::time::Instant::now() >= deadline {
                return 4;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    let report = if prompt == "auth-echo" {
        format!(
            "fixture credential echo: {}",
            std::env::var("ANTHROPIC_API_KEY")
                .or_else(|_| std::env::var("CLAUDE_CODE_OAUTH_TOKEN"))
                .or_else(|_| std::env::var("ANTHROPIC_AUTH_TOKEN"))
                .unwrap_or_default()
        )
    } else {
        "fixture print completed".into()
    };
    let mut result = json!({"type":"result","subtype":"success","is_error":false,"session_id":"fixture-print-owned","result":report,"usage":{"input_tokens":1,"output_tokens":2},"total_cost_usd":0});
    if argv.iter().any(|a| a == "--json-schema") && scenario["mode"] != "no_structured" {
        result["structured_output"] = json!({"result":{"kind":"report","report_text":report,"knowledge":[]}});
    }
    println!("{}", result);
    0
}

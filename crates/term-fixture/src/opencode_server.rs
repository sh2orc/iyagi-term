//! Minimal real HTTP/SSE server for the owned-process adapter tests. It
//! authenticates every request and emits OpenAPI-shaped events. No provider
//! or external network calls are made.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    event: Option<TcpStream>,
    saved: Value,
}

pub fn run(hostname: &str, port: u16) -> std::io::Result<()> {
    if hostname != "127.0.0.1" {
        return Err(std::io::Error::other("fixture requires loopback"));
    }
    let password = std::env::var("OPENCODE_SERVER_PASSWORD")
        .map_err(|_| std::io::Error::other("fixture requires authentication"))?;
    use base64::Engine;
    let auth = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("opencode:{password}"))
    );
    if let Some(delay) = std::env::var("IYAGI_FIXTURE_SERVER_START_DELAY_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        std::thread::sleep(std::time::Duration::from_millis(delay.min(30_000)));
    }
    let listener = TcpListener::bind((hostname, port))?;
    println!(
        "opencode server listening on http://{}",
        listener.local_addr()?
    );
    std::io::stdout().flush()?;
    let shared = Arc::new(Mutex::new(State::default()));
    for socket in listener.incoming() {
        let socket = socket?;
        let auth = auth.clone();
        let shared = shared.clone();
        std::thread::spawn(move || {
            let _ = handle(socket, &auth, shared);
        });
    }
    Ok(())
}

fn reply(socket: &mut TcpStream, status: &str, body: Value) -> std::io::Result<()> {
    let bytes = if status.starts_with("204") {
        vec![]
    } else {
        serde_json::to_vec(&body)?
    };
    write!(socket,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len())?;
    socket.write_all(&bytes)?;
    socket.flush()
}

fn handle(mut socket: TcpStream, auth: &str, shared: Arc<Mutex<State>>) -> std::io::Result<()> {
    socket.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let mut reader = BufReader::new(socket.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let method = line.split_whitespace().next().unwrap_or("").to_owned();
    let route = line.split_whitespace().nth(1).unwrap_or("").to_owned();
    let mut authenticated = false;
    let mut len = 0usize;
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        if line == "\r\n" || line == "\n" {
            break;
        }
        if line.len() > 8192 {
            return Err(std::io::Error::other("header too large"));
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("authorization") {
                authenticated = value.trim() == auth;
            }
            if key.eq_ignore_ascii_case("content-length") {
                len = value.trim().parse().unwrap_or(usize::MAX);
            }
        }
    }
    if !authenticated {
        return reply(
            &mut socket,
            "401 Unauthorized",
            json!({"error":"auth required"}),
        );
    }
    if len > 4 * 1024 * 1024 {
        return reply(&mut socket, "413 Content Too Large", Value::Null);
    }
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes)?;
    let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
    match (method.as_str(), route.as_str()) {
        ("GET", "/global/health") => reply(
            &mut socket,
            "200 OK",
            json!({"healthy":true,"version":"fixture"}),
        ),
        ("GET", "/config") => {
            let mut config: Value = serde_json::from_str(
                &std::env::var("OPENCODE_CONFIG_CONTENT").unwrap_or_else(|_| "{}".into()),
            )?;
            if let Some(providers) = config["provider"].as_object_mut() {
                for provider in providers.values_mut() {
                    if provider["options"]["apiKey"] == "{env:IYAGI_PROVIDER_API_KEY}" {
                        provider["options"]["apiKey"] =
                            json!(std::env::var("IYAGI_PROVIDER_API_KEY").unwrap_or_default());
                    }
                }
            }
            reply(&mut socket, "200 OK", config)
        }
        ("POST", "/session") => {
            if !body["model"]["id"].is_string()
                || !body["model"]["providerID"].is_string()
                || !body["permission"].is_array()
            {
                return reply(&mut socket, "400 Bad Request", Value::Null);
            }
            reply(&mut socket, "200 OK", json!({"id":"ses_fixture"}))
        }
        ("GET", "/event") => {
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n")?;
            send(&mut socket, "server.connected", json!({}))?;
            shared.lock().unwrap().event = Some(socket);
            Ok(())
        }
        ("POST", "/session/ses_fixture/prompt_async") => {
            if !body["messageID"].is_string()
                || !body["parts"].is_array()
                || body["format"]["type"] != "json_schema"
            {
                return reply(&mut socket, "400 Bad Request", Value::Null);
            }
            reply(&mut socket, "204 No Content", Value::Null)?;
            let mut info = json!({"id":"msg_fixture","sessionID":"ses_fixture","role":"assistant","parentID":body["messageID"],"providerID":body["model"]["providerID"],"modelID":body["model"]["modelID"],"time":{"created":1},"mode":"build","agent":"build","path":{"cwd":"/workspace","root":"/workspace"},"cost":0.00002,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}}});
            let mut state = shared.lock().unwrap();
            if let Some(event) = state.event.as_mut() {
                send(
                    event,
                    "message.updated",
                    json!({"sessionID":"ses_fixture","info":info}),
                )?;
                send(
                    event,
                    "message.part.updated",
                    json!({"sessionID":"ses_fixture","time":1,"part":{"id":"prt_fixture","sessionID":"ses_fixture","messageID":"msg_fixture","type":"text","text":"작업 중"}}),
                )?;
            }
            if body["parts"][0]["text"].as_str() == Some("hold") {
                return Ok(());
            }
            info["time"]["completed"] = json!(2);
            info["finish"] = json!("stop");
            info["structured"] =
                json!({"kind":"report","report_text":"HTTP fixture complete","knowledge":[]});
            if body["parts"][0]["text"].as_str() == Some("auth-echo") {
                let key = std::env::var("IYAGI_PROVIDER_API_KEY").unwrap_or_default();
                info["structured"]["report_text"] = json!(format!("credential echo: {key}"));
                if let Some(event) = state.event.as_mut() {
                    send(
                        event,
                        "message.part.updated",
                        json!({"sessionID":"ses_fixture","time":2,
                        "part":{"id":"prt_fixture","sessionID":"ses_fixture","messageID":"msg_fixture","type":"text","text":format!("credential echo: {key}")}}),
                    )?;
                }
            }
            info["structured"] = json!({"result":info["structured"].take()});
            state.saved = json!({"info":info,"parts":[]});
            if let Some(event) = state.event.as_mut() {
                send(
                    event,
                    "message.updated",
                    json!({"sessionID":"ses_fixture","info":info}),
                )?;
                send(
                    event,
                    "session.status",
                    json!({"sessionID":"ses_fixture","status":{"type":"idle"}}),
                )?;
            }
            Ok(())
        }
        ("GET", "/session/ses_fixture/message/msg_fixture") => {
            reply(&mut socket, "200 OK", shared.lock().unwrap().saved.clone())
        }
        ("POST", "/session/ses_fixture/abort") => reply(&mut socket, "200 OK", json!(true)),
        _ => reply(&mut socket, "404 Not Found", Value::Null),
    }
}

fn send(socket: &mut TcpStream, kind: &str, properties: Value) -> std::io::Result<()> {
    let event = json!({"id":"evt_fixture","type":kind,"properties":properties});
    write!(socket, "data: {event}\n\n")?;
    socket.flush()
}

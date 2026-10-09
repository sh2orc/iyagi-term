//! Bounded HTTP/SSE over an authenticated, run-owned loopback server.
//! The owner supplies stop/probe hooks; an HTTP acknowledgment never turns
//! into a fabricated process exit. No proxy, redirect, or POST retry.

use std::io::{BufRead, BufReader, Read};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;

use super::OpencodeTransport;
use crate::agent_runtime::RunProbe;

const MAX_BODY: usize = 4 * 1024 * 1024;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(15);

pub struct HttpTransport {
    client: Client,
    base: String,
    closed: AtomicBool,
    stop: Arc<dyn Fn() + Send + Sync>,
    probe: Arc<dyn Fn() -> RunProbe + Send + Sync>,
    redactor: Option<Arc<crate::connections::SecretRedactor>>,
}

impl HttpTransport {
    /// Check that the owned server rejects invalid credentials before any
    /// session is created. Merely sending an Authorization header does not
    /// prove that server-side authentication was enabled.
    pub fn verify_authentication(&self) -> Result<(), String> {
        let response = self
            .client
            .get(self.url("/session")?)
            .header(AUTHORIZATION, "Basic aW52YWxpZDppbnZhbGlk")
            .timeout(CONTROL_TIMEOUT)
            .send()
            .map_err(|_| "OpenCode server authentication could not be verified")?;
        if response.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Err("OpenCode server did not reject invalid credentials".into());
        }
        Ok(())
    }

    pub fn new(
        address: SocketAddr,
        password: &str,
        stop: Arc<dyn Fn() + Send + Sync>,
        probe: Arc<dyn Fn() -> RunProbe + Send + Sync>,
    ) -> Result<Self, String> {
        if address.ip() != Ipv4Addr::LOCALHOST || address.port() == 0 || password.is_empty() {
            return Err(
                "OpenCode transport requires an authenticated IPv4 loopback address".into(),
            );
        }
        use base64::Engine;
        let auth = base64::engine::general_purpose::STANDARD.encode(format!("opencode:{password}"));
        let mut value = HeaderValue::from_str(&format!("Basic {auth}"))
            .map_err(|_| "OpenCode authentication header is invalid")?;
        value.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, value);
        let client = Client::builder()
            .default_headers(headers)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(5))
            .timeout(None)
            .build()
            .map_err(|_| "OpenCode HTTP client could not be initialized")?;
        Ok(Self {
            client,
            base: format!("http://{address}"),
            closed: AtomicBool::new(false),
            stop,
            probe,
            redactor: None,
        })
    }

    pub fn with_redactor(mut self, redactor: Arc<crate::connections::SecretRedactor>) -> Self {
        self.redactor = Some(redactor);
        self
    }

    fn redact_response(&self, mut value: Value) -> Value {
        if let Some(redactor) = &self.redactor {
            redactor.redact_json(&mut value);
        }
        value
    }

    fn url(&self, route: &str) -> Result<String, String> {
        if self.closed.load(Ordering::Acquire) {
            return Err("OpenCode transport is closed".into());
        }
        if !route.starts_with('/')
            || route.starts_with("//")
            || route
                .bytes()
                .any(|b| !(b.is_ascii_alphanumeric() || b == b'/' || b == b'_'))
        {
            return Err("OpenCode route is invalid".into());
        }
        Ok(format!("{}{route}", self.base))
    }

    fn json_response(response: Response) -> Result<Value, String> {
        if !response.status().is_success() {
            return Err("OpenCode HTTP request was rejected".into());
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_BODY as u64)
        {
            return Err("OpenCode HTTP response exceeds its byte limit".into());
        }
        let no_content = response.status() == reqwest::StatusCode::NO_CONTENT;
        let mut bytes = Vec::new();
        response
            .take((MAX_BODY + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| "OpenCode HTTP response was incomplete")?;
        if bytes.len() > MAX_BODY {
            return Err("OpenCode HTTP response exceeds its byte limit".into());
        }
        if no_content && bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| "OpenCode HTTP response is not valid JSON".into())
    }
}

impl OpencodeTransport for HttpTransport {
    fn post(&self, route: &str, body: &Value) -> Result<Value, String> {
        // Serialization is bounded as well; request bodies carry context.
        let bytes = serde_json::to_vec(body).map_err(|_| "OpenCode request is not JSON")?;
        if bytes.len() > MAX_BODY {
            return Err("OpenCode request exceeds its byte limit".into());
        }
        let response = self
            .client
            .post(self.url(route)?)
            .header(CONTENT_TYPE, "application/json")
            .body(bytes)
            .timeout(CONTROL_TIMEOUT)
            .send()
            .map_err(|_| "OpenCode POST response was not confirmed")?;
        Self::json_response(response).map(|v| self.redact_response(v))
    }

    fn get(&self, route: &str) -> Result<Value, String> {
        let response = self
            .client
            .get(self.url(route)?)
            .timeout(CONTROL_TIMEOUT)
            .send()
            .map_err(|_| "OpenCode GET failed")?;
        Self::json_response(response).map(|v| self.redact_response(v))
    }

    fn event_stream(
        &self,
        route: &str,
        on_event: &mut dyn FnMut(Value) -> bool,
    ) -> Result<(), String> {
        let response = self
            .client
            .get(self.url(route)?)
            .header("Accept", "text/event-stream")
            .send()
            .map_err(|_| "OpenCode event subscription failed")?;
        if !response.status().is_success()
            || !response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| {
                    v.split(';')
                        .next()
                        .is_some_and(|s| s.trim() == "text/event-stream")
                })
        {
            return Err("OpenCode event subscription returned an invalid response".into());
        }
        parse_sse(BufReader::new(response), &self.closed, &mut |value| {
            on_event(self.redact_response(value))
        })
    }

    fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            (self.stop)();
        }
    }

    fn process_probe(&self) -> RunProbe {
        (self.probe)()
    }
}

impl Drop for HttpTransport {
    fn drop(&mut self) {
        self.close();
    }
}

fn parse_sse(
    mut reader: impl BufRead,
    closed: &AtomicBool,
    on_event: &mut dyn FnMut(Value) -> bool,
) -> Result<(), String> {
    let mut line = Vec::new();
    let mut data = Vec::new();
    loop {
        if closed.load(Ordering::Acquire) {
            return Ok(());
        }
        line.clear();
        // read_until itself is unbounded: Take enforces the limit before
        // allocating a hostile newline-free SSE frame.
        let count = (&mut reader)
            .take((MAX_BODY + 1) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|_| "OpenCode event stream read failed")?;
        if count == 0 {
            return Ok(());
        }
        if line.len() > MAX_BODY {
            return Err("OpenCode event line exceeds its byte limit".into());
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.is_empty() {
            if !data.is_empty() {
                data.pop(); // The separator after the last data field.
                let value = serde_json::from_slice(&data)
                    .map_err(|_| "OpenCode event is not valid JSON")?;
                data.clear();
                if !on_event(value) {
                    return Ok(());
                }
            }
        } else if let Some(value) = line.strip_prefix(b"data:") {
            let value = value.strip_prefix(b" ").unwrap_or(value);
            if data.len().saturating_add(value.len()).saturating_add(1) > MAX_BODY {
                return Err("OpenCode event exceeds its byte limit".into());
            }
            data.extend_from_slice(value);
            data.push(b'\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_supports_comments_crlf_multiple_data_fields_and_stops_on_callback() {
        let mut events = Vec::new();
        let input = b": heartbeat\r\nevent: ignored\r\ndata: {\r\ndata: \"type\":\"server.connected\"}\r\n\r\ndata: invalid\n\n";
        parse_sse(&input[..], &AtomicBool::new(false), &mut |event| {
            events.push(event);
            false
        })
        .unwrap();
        assert_eq!(events, vec![serde_json::json!({"type":"server.connected"})]);
    }

    #[test]
    fn sse_bounds_lines_and_accumulated_frames_and_rejects_invalid_json() {
        let closed = AtomicBool::new(false);
        let mut count = 0;
        let mut sink = |_| {
            count += 1;
            true
        };
        assert!(parse_sse(&vec![b'x'; MAX_BODY + 1][..], &closed, &mut sink).is_err());
        let mut frame = Vec::new();
        for _ in 0..5 {
            frame.extend_from_slice(b"data: ");
            frame.extend_from_slice(&vec![b'x'; 1024 * 1024]);
            frame.push(b'\n');
        }
        assert!(parse_sse(&frame[..], &closed, &mut sink).is_err());
        assert!(parse_sse(&b"data: invalid\n\n"[..], &closed, &mut sink).is_err());
        assert_eq!(count, 0);
    }
    fn response_server(response: String) -> (SocketAddr, std::thread::JoinHandle<String>) {
        use std::io::Write;
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut request = String::new();
            let mut line = String::new();
            let mut length = 0usize;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                request.push_str(&line);
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            request.push_str(&String::from_utf8(body).unwrap());
            socket.write_all(response.as_bytes()).unwrap();
            request
        });
        (address, handle)
    }
    fn transport(address: SocketAddr) -> HttpTransport {
        HttpTransport::new(
            address,
            "test-only-secret",
            Arc::new(|| {}),
            Arc::new(|| RunProbe::Unknown),
        )
        .unwrap()
    }
    #[test]
    fn http_authenticates_posts_and_accepts_an_empty_204_response() {
        let (address, server) = response_server(
            "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
        );
        let client = transport(address);
        assert_eq!(
            client
                .post(
                    "/session/ses_one/prompt_async",
                    &serde_json::json!({"parts":[]})
                )
                .unwrap(),
            Value::Null
        );
        let request = server.join().unwrap();
        use base64::Engine;
        assert!(request.to_ascii_lowercase().contains(&format!(
            "authorization: basic {}",
            base64::engine::general_purpose::STANDARD
                .encode("opencode:test-only-secret")
                .to_ascii_lowercase()
        )));
        assert!(request.ends_with("{\"parts\":[]}"));
    }
    #[test]
    fn redirects_cannot_forward_the_authenticated_request() {
        let target = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        target.set_nonblocking(true).unwrap();
        let (address,server)=response_server(format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{}/leak\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",target.local_addr().unwrap()));
        assert!(transport(address)
            .post("/session", &serde_json::json!({}))
            .is_err());
        server.join().unwrap();
        assert_eq!(
            target.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn a_server_that_ignores_credentials_cannot_pass_the_authentication_probe() {
        let (address, server) = response_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]".into(),
        );
        assert!(transport(address).verify_authentication().is_err());
        server.join().unwrap();
    }
    #[test]
    fn oversized_json_and_server_error_bodies_never_escape_in_diagnostics() {
        for response in [format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",MAX_BODY+1),
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 15\r\nConnection: close\r\n\r\nsecret-upstream".into()] {
            let (address,server)=response_server(response);let failure=transport(address).get("/global/health").unwrap_err();server.join().unwrap();
            assert!(!failure.contains("secret-upstream"));assert!(!failure.contains("test-only-secret"));
        }
    }
}
